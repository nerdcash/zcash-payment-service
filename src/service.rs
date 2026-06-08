use axum::{extract::State, Json};
use serde::{Deserialize, Serialize};

use crate::{
    config::Config,
    db::{AppDb, AtomicFreshAddressAllocation, OwnedAddressReceiver, OwnedIssuedAddress},
    error::AppError,
    wallet_db::{initialize_wallet_db, read_wallet_db_identity},
    zcash::WalletView,
    AppState,
};

pub struct PaymentService {
    app_db: AppDb,
    wallet_view: WalletView,
}

#[derive(Debug, Deserialize)]
pub struct PaymentSessionRequest {
    pub payment_source: String,
    pub label: Option<String>,
    pub memo: Option<String>,
    pub message: Option<String>,
    pub amount: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PaymentSessionResponse {
    pub address: String,
    pub qr_text: String,
}

#[derive(Debug, Serialize)]
pub struct HealthResponse {
    pub ok: bool,
    pub current_sync_state: String,
    pub detected_tip_height: Option<u64>,
    pub last_scanned_height: Option<u64>,
}

impl PaymentService {
    pub fn initialize(config: Config) -> Result<Self, AppError> {
        initialize_wallet_db(&config)?;
        let app_db = AppDb::open(&config.app_db_path)?;
        let wallet_identity = app_db.ensure_wallet_identity(&config)?;
        let service_metadata = app_db.ensure_service_metadata(&config)?;
        let wallet_view =
            WalletView::decode(&wallet_identity.network, &wallet_identity.canonical_uivk)?;

        if let Some(wallet_db_identity) = read_wallet_db_identity(&config.wallet_db_path)? {
            if wallet_db_identity.uivk != wallet_view.encoded_uivk() {
                return Err(AppError::StartupIntegrity(
                    "wallet DB UIVK does not match the decoded canonical wallet view".into(),
                ));
            }
        }

        tracing::info!(
            network = %wallet_identity.network,
            uivk_fingerprint = %wallet_identity.uivk_fingerprint,
            app_schema_version = service_metadata.app_schema_version,
            webhook_report_confirmations = service_metadata.webhook_report_confirmations,
            finality_confirmations = service_metadata.finality_confirmations,
            app_db_path = %config.app_db_path.display(),
            wallet_db_path = %config.wallet_db_path.display(),
            "service startup integrity checks passed"
        );

        Ok(Self {
            app_db,
            wallet_view,
        })
    }

    pub fn issue_payment_session(
        &self,
        request: PaymentSessionRequest,
    ) -> Result<PaymentSessionResponse, AppError> {
        if request.payment_source != "zcash" {
            return Err(AppError::InvalidRequest(
                "payment_source must be 'zcash'".into(),
            ));
        }

        tracing::info!(
            label = request.label.as_deref().unwrap_or_default(),
            amount = request.amount.as_deref().unwrap_or_default(),
            "received payment-session request"
        );

        let address = self.mint_address(
            request.label.as_deref(),
            request.memo.as_deref(),
            request.message.as_deref(),
            request.amount.as_deref(),
        )?;

        let qr_text = build_qr_text(
            &address,
            request.amount.as_deref(),
            request.memo.as_deref(),
            request.message.as_deref(),
        );

        tracing::info!(address, "issued payment session");

        Ok(PaymentSessionResponse { address, qr_text })
    }

    fn mint_address(
        &self,
        label: Option<&str>,
        memo: Option<&str>,
        message: Option<&str>,
        amount: Option<&str>,
    ) -> Result<String, AppError> {
        let record = self.app_db.allocate_fresh_address(|last_index| {
            let derived = self.wallet_view.derive_address_after(last_index)?;

            Ok(AtomicFreshAddressAllocation {
                issued_address: OwnedIssuedAddress {
                    unified_address: derived.encoded.clone(),
                    address_source: "fresh".into(),
                    diversifier_index_be: Some(derived.diversifier_index_be.clone()),
                    diversifier_bytes: Some(derived.diversifier_index_be.clone()),
                    request_label: label.map(ToOwned::to_owned),
                    request_memo: memo.map(ToOwned::to_owned),
                    request_message: message.map(ToOwned::to_owned),
                    requested_amount: amount.map(ToOwned::to_owned),
                },
                receivers: vec![
                    OwnedAddressReceiver {
                        pool: "orchard".into(),
                        receiver_encoding: hex_string(&derived.orchard_receiver),
                        receiver_fingerprint: receiver_fingerprint(&derived.orchard_receiver),
                    },
                    OwnedAddressReceiver {
                        pool: "sapling".into(),
                        receiver_encoding: hex_string(&derived.sapling_receiver),
                        receiver_fingerprint: receiver_fingerprint(&derived.sapling_receiver),
                    },
                ],
            })
        })?;

        tracing::info!(address = %record.unified_address, "minted fresh unified address from canonical UIVK");
        Ok(record.unified_address)
    }

    pub fn health_response(&self) -> Result<HealthResponse, AppError> {
        let metadata = self.app_db.current_service_metadata()?;

        Ok(HealthResponse {
            ok: true,
            current_sync_state: metadata.current_sync_state,
            detected_tip_height: nonzero_height(metadata.last_seen_tip_height),
            last_scanned_height: nonzero_height(metadata.last_scanned_height),
        })
    }
}

pub async fn health(State(state): State<AppState>) -> Result<Json<HealthResponse>, AppError> {
    Ok(Json(state.service.health_response()?))
}

pub async fn payment_session(
    State(state): State<AppState>,
    Json(request): Json<PaymentSessionRequest>,
) -> Result<Json<PaymentSessionResponse>, AppError> {
    Ok(Json(state.service.issue_payment_session(request)?))
}

pub fn build_qr_text(
    address: &str,
    amount: Option<&str>,
    memo: Option<&str>,
    message: Option<&str>,
) -> String {
    let mut query = Vec::new();
    if let Some(amount) = amount.filter(|value| !value.trim().is_empty()) {
        query.push(format!("amount={}", encode_uri_component(amount)));
    }
    if let Some(memo) = memo.filter(|value| !value.trim().is_empty()) {
        query.push(format!("memo={}", encode_uri_component(memo)));
    }
    if let Some(message) = message.filter(|value| !value.trim().is_empty()) {
        query.push(format!("message={}", encode_uri_component(message)));
    }

    if query.is_empty() {
        format!("zcash:{address}")
    } else {
        format!("zcash:{address}?{}", query.join("&"))
    }
}

fn encode_uri_component(value: &str) -> String {
    let mut encoded = String::new();
    for byte in value.bytes() {
        match byte {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                encoded.push(byte as char)
            }
            _ => encoded.push_str(&format!("%{byte:02X}")),
        }
    }
    encoded
}

fn hex_string(bytes: &[u8]) -> String {
    let mut output = String::with_capacity(bytes.len() * 2);
    for byte in bytes {
        output.push_str(&format!("{byte:02x}"));
    }
    output
}

fn receiver_fingerprint(bytes: &[u8]) -> Vec<u8> {
    let mut state: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        state ^= u64::from(*byte);
        state = state.wrapping_mul(0x100000001b3);
    }
    state.to_be_bytes().to_vec()
}

fn nonzero_height(value: i64) -> Option<u64> {
    u64::try_from(value).ok().filter(|height| *height > 0)
}

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::{Arc, Barrier},
        thread,
    };

    use tempfile::tempdir;

    use super::{build_qr_text, HealthResponse, PaymentService, PaymentSessionRequest};
    use crate::config::Config;

    const MAINNET_UIVK_NO_TRANSPARENT: &str = "uivk1020vq9j5zeqxh303sxa0zv2hn9wm9fev8x0p8yqxdwyzde9r4c90fcglc63usj0ycl2scy8zxuhtser0qrq356xfy8x3vyuxu7f6gas75svl9v9m3ctuazsu0ar8e8crtx7x6zgh4kw8xm3q4rlkpm9er2wefxhhf9pn547gpuz9vw27gsdp6c03nwlrxgzhr2g6xek0x8l5avrx9ue9lf032tr7kmhqf3nfdxg7ldfgx6yf09g";
    fn config(temp: &Path) -> Config {
        Config {
            listen_addr: "127.0.0.1:0".into(),
            network: "mainnet".into(),
            startup_uivk: Some(MAINNET_UIVK_NO_TRANSPARENT.into()),
            lightwalletd_url: None,
            birthday_height: Some(123),
            wallet_db_path: temp.join("wallet.db"),
            app_db_path: temp.join("app.db"),
            log_dir: temp.join("logs"),
            catch_up_threshold_blocks: 1,
            catch_up_batch_size: 100,
            sync_poll_interval_seconds: 5,
            webhook_url: None,
            webhook_secret: None,
            webhook_poll_interval_seconds: 2,
            webhook_retry_delay_seconds: 30,
            webhook_retry_max_delay_seconds: 300,
            webhook_max_attempts: 8,
            webhook_report_confirmations: 1,
            finality_confirmations: 100,
        }
    }

    #[test]
    fn qr_text_omits_query_when_optional_fields_are_missing() {
        assert_eq!(build_qr_text("uaddr", None, None, None), "zcash:uaddr");
    }

    #[test]
    fn qr_text_encodes_optional_fields() {
        assert_eq!(
            build_qr_text(
                "uaddr",
                Some("0.12345678"),
                Some("Invoice INV-1001"),
                Some("Thanks for your purchase."),
            ),
            "zcash:uaddr?amount=0.12345678&memo=Invoice%20INV-1001&message=Thanks%20for%20your%20purchase."
        );
    }

    #[test]
    fn payment_sessions_always_mint_fresh_addresses() {
        let temp = tempdir().unwrap();
        let service = PaymentService::initialize(config(temp.path())).unwrap();
        let first = service
            .issue_payment_session(PaymentSessionRequest {
                payment_source: "zcash".into(),
                label: None,
                memo: None,
                message: None,
                amount: None,
            })
            .unwrap();

        let second = service
            .issue_payment_session(PaymentSessionRequest {
                payment_source: "zcash".into(),
                label: None,
                memo: None,
                message: None,
                amount: None,
            })
            .unwrap();

        assert_ne!(first.address, second.address);
    }

    #[test]
    fn fresh_addresses_are_real_unified_addresses() {
        let temp = tempdir().unwrap();
        let service = PaymentService::initialize(config(temp.path())).unwrap();
        let first = service
            .issue_payment_session(PaymentSessionRequest {
                payment_source: "zcash".into(),
                label: None,
                memo: None,
                message: None,
                amount: None,
            })
            .unwrap();

        assert!(first.address.starts_with('u'));
        assert!(first
            .qr_text
            .starts_with(&format!("zcash:{}", first.address)));
    }

    #[test]
    fn concurrent_fresh_address_requests_all_succeed_with_unique_addresses() {
        let temp = tempdir().unwrap();
        let config = Arc::new(config(temp.path()));
        let barrier = Arc::new(Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let config = Arc::clone(&config);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    let service = PaymentService::initialize((*config).clone()).unwrap();
                    barrier.wait();
                    service
                        .issue_payment_session(PaymentSessionRequest {
                            payment_source: "zcash".into(),
                            label: None,
                            memo: None,
                            message: None,
                            amount: None,
                        })
                        .unwrap()
                        .address
                })
            })
            .collect();

        let mut addresses: Vec<String> = handles
            .into_iter()
            .map(|handle| handle.join().unwrap())
            .collect();
        addresses.sort();
        addresses.dedup();

        assert_eq!(addresses.len(), 8);
    }

    #[test]
    fn health_response_reports_persisted_sync_heights() {
        let temp = tempdir().unwrap();
        let service = PaymentService::initialize(config(temp.path())).unwrap();
        service
            .app_db
            .record_sync_status("catching_up", "testing", 321, 300)
            .unwrap();

        let response: HealthResponse = service.health_response().unwrap();

        assert!(response.ok);
        assert_eq!(response.current_sync_state, "catching_up");
        assert_eq!(response.detected_tip_height, Some(321));
        assert_eq!(response.last_scanned_height, Some(300));
    }
}
