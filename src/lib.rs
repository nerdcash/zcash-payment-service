pub mod config;
pub mod constants;
pub mod db;
pub mod error;
pub mod logging;
pub mod receipt_ingest;
pub mod scanner;
pub mod service;
pub mod wallet_db;
pub mod wallet_runtime;
pub mod webhook_delivery;
pub mod zcash;

use std::sync::Arc;

use axum::{
    routing::{get, post},
    Router,
};
use tracing_appender::non_blocking::WorkerGuard;

use crate::{
    config::Config,
    db::AppDb,
    error::AppError,
    logging::init_logging,
    service::{health, payment_session, PaymentService},
    wallet_runtime::spawn_wallet_sync_worker,
    webhook_delivery::spawn_webhook_delivery_worker,
};

#[derive(Clone)]
pub struct AppState {
    pub service: Arc<PaymentService>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
struct StartupWebhookRecovery {
    recovered_delivery_ids: Vec<i64>,
    queued_dirty_address_ids: Vec<i64>,
}

impl StartupWebhookRecovery {
    fn is_empty(&self) -> bool {
        self.recovered_delivery_ids.is_empty() && self.queued_dirty_address_ids.is_empty()
    }
}

fn recover_startup_webhook_state(app_db: &AppDb) -> Result<StartupWebhookRecovery, AppError> {
    Ok(StartupWebhookRecovery {
        recovered_delivery_ids: app_db.recover_interrupted_webhook_deliveries()?,
        queued_dirty_address_ids: app_db.queue_all_dirty_webhook_deliveries()?,
    })
}

pub fn build_router(state: AppState) -> Router {
    Router::new()
        .route("/health", get(health))
        .route("/payment-session", post(payment_session))
        .with_state(state)
}

pub async fn run() -> Result<(), AppError> {
    let config = Config::from_env()?;
    let _guard = init_logging(&config)?;
    run_with_config(config, _guard).await
}

async fn run_with_config(config: Config, _guard: WorkerGuard) -> Result<(), AppError> {
    let service = Arc::new(PaymentService::initialize(config.clone())?);
    let app_db = AppDb::open(&config.app_db_path)?;
    let recovery = recover_startup_webhook_state(&app_db)?;
    if !recovery.is_empty() {
        tracing::info!(
            recovered_delivery_count = recovery.recovered_delivery_ids.len(),
            recovered_dirty_address_count = recovery.queued_dirty_address_ids.len(),
            "recovered interrupted webhook delivery state on startup"
        );
    }
    spawn_wallet_sync_worker(config.clone());
    spawn_webhook_delivery_worker(config.clone());
    let state = AppState { service };
    let app = build_router(state);

    let listener = tokio::net::TcpListener::bind(&config.listen_addr).await?;
    tracing::info!(
        listen_addr = %config.listen_addr,
        network = %config.network,
        log_dir = %config.log_dir.display(),
        app_db_path = %config.app_db_path.display(),
        wallet_db_path = %config.wallet_db_path.display(),
        "zcash payment service listening"
    );
    axum::serve(listener, app)
        .with_graceful_shutdown(shutdown_signal())
        .await?;
    Ok(())
}

async fn shutdown_signal() {
    if tokio::signal::ctrl_c().await.is_ok() {
        tracing::info!("shutdown signal received; stopping zcash payment service");
    }
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::recover_startup_webhook_state;
    use crate::db::{
        AppDb, AttributedScannedReceipt, NewAddressReceiver, NewIssuedAddress, ReceiptKind,
    };

    fn mined_receipt(
        address_id: i64,
        txid_hex: &str,
        receipt_uid: &str,
        value_zat: i64,
    ) -> AttributedScannedReceipt {
        AttributedScannedReceipt {
            address_id,
            txid_hex: txid_hex.into(),
            pool: "sapling".into(),
            receipt_uid: receipt_uid.into(),
            value_zat,
            mined_height: 600,
            confirmation_depth: 10,
            eligible_for_webhook: true,
            receipt_kind: ReceiptKind::Mined,
            first_observed_at: "2026-01-01T00:00:00Z".into(),
            last_observed_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn insert_test_address(db: &AppDb, unified_address: &str, receiver_encoding: &str) -> i64 {
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address,
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding,
                receiver_fingerprint: receiver_encoding.as_bytes(),
            }],
        )
        .unwrap();
        record.address_id
    }

    #[test]
    fn startup_webhook_recovery_restores_interrupted_and_dirty_work_idempotently() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();

        let interrupted_address_id =
            insert_test_address(&db, "uaddr1restart-send", "sapling-restart-send");
        db.apply_attributed_receipts(
            &[mined_receipt(
                interrupted_address_id,
                "tx-restart-send",
                "tx-restart-send:sapling:0",
                11,
            )],
            610,
            100,
        )
        .unwrap();
        db.queue_webhook_deliveries_for_dirty_addresses(&[interrupted_address_id])
            .unwrap();
        let claimed = db
            .claim_ready_webhook_delivery("2026-01-02T00:00:00Z")
            .unwrap()
            .unwrap();

        let dirty_address_id =
            insert_test_address(&db, "uaddr1restart-dirty", "sapling-restart-dirty");
        db.apply_attributed_receipts(
            &[mined_receipt(
                dirty_address_id,
                "tx-restart-dirty",
                "tx-restart-dirty:sapling:0",
                17,
            )],
            610,
            100,
        )
        .unwrap();

        let recovered = recover_startup_webhook_state(&db).unwrap();
        assert_eq!(recovered.recovered_delivery_ids, vec![claimed.delivery_id]);
        assert_eq!(recovered.queued_dirty_address_ids, vec![dirty_address_id]);

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let interrupted_state: String = conn
            .query_row(
                "SELECT delivery_state FROM webhook_deliveries WHERE delivery_id = ?1",
                rusqlite::params![claimed.delivery_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(interrupted_state, "retry_wait");

        let dirty_delivery_count: i64 = conn
            .query_row(
                "SELECT COUNT(*) FROM webhook_deliveries WHERE address_id = ?1",
                rusqlite::params![dirty_address_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(dirty_delivery_count, 1);

        let second_recovery = recover_startup_webhook_state(&db).unwrap();
        assert!(second_recovery.recovered_delivery_ids.is_empty());
        assert!(second_recovery.queued_dirty_address_ids.is_empty());
    }
}
