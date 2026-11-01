use axum::{
    extract::{Path, State},
    Json,
};
use rand::{rand_core::UnwrapErr, rngs::SysRng, Rng};
use serde::{Deserialize, Serialize};

use crate::db::{AttributedScannedReceipt, ReceiptKind};
use crate::{
    config::{validate_return_url, Config},
    db::{
        format_zat_as_zec, AppDb, AtomicFreshAddressAllocation, OwnedAddressReceiver,
        OwnedIssuedAddress,
    },
    error::AppError,
    wallet_db::{initialize_wallet_db, read_wallet_db_identity},
    zcash::WalletView,
    AppState,
};

pub struct PaymentService {
    app_db: AppDb,
    wallet_view: WalletView,
    public_base_url: Option<String>,
    dev_simulated_payments: bool,
    return_url_allowed_origins: Vec<String>,
}

#[derive(Debug, Deserialize)]
pub struct PaymentSessionRequest {
    pub payment_source: String,
    pub label: Option<String>,
    pub memo: Option<String>,
    pub message: Option<String>,
    pub amount: Option<String>,
    /// Where the hosted pay page sends the payer once the payment is seen.
    pub return_url: Option<String>,
}

#[derive(Debug, Serialize)]
pub struct PaymentSessionResponse {
    pub address: String,
    pub qr_text: String,
    /// Unguessable id for the hosted pay page and the status route.
    pub payment_id: String,
    /// Link to the hosted pay page. Absolute when `PUBLIC_BASE_URL` is set, otherwise a path.
    pub pay_url: String,
}

/// Where a payment link stands, judged against the requested amount.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum PaymentStatus {
    /// Nothing eligible has arrived yet.
    AwaitingPayment,
    /// Something arrived, but less than the requested amount.
    PartiallyPaid,
    /// The eligible total covers the requested amount.
    Paid,
    /// Something arrived, and the request had no amount the service can compare with.
    Received,
}

#[derive(Debug, Serialize)]
pub struct PaymentStatusResponse {
    pub payment_id: String,
    pub address: String,
    pub qr_text: String,
    pub pay_url: String,
    pub label: Option<String>,
    pub message: Option<String>,
    pub amount: Option<String>,
    pub total_received: String,
    pub state_version: i64,
    pub status: PaymentStatus,
    pub return_url: Option<String>,
    pub created_at: String,
}

#[derive(Debug, Deserialize)]
pub struct SimulatePaymentRequest {
    pub address: String,
    pub amount: String,
}

#[derive(Debug, Serialize)]
pub struct SimulatePaymentResponse {
    /// Made-up transaction id. It does not exist on any chain.
    pub txid: String,
    pub total_received: String,
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

        if config.lightwalletd_url.is_some() && app_db.has_simulated_data()? {
            return Err(AppError::StartupIntegrity(
                "this database holds simulated payments (ZCASH_DEV_SIMULATED_PAYMENTS); use a fresh database for real scanning".into(),
            ));
        }

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
            public_base_url: config.public_base_url.clone(),
            dev_simulated_payments: config.dev_simulated_payments
                && wallet_identity.network == "testnet",
            return_url_allowed_origins: config.return_url_allowed_origins.clone(),
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

        let return_url = request
            .return_url
            .as_deref()
            .map(str::trim)
            .filter(|value| !value.is_empty());
        if let Some(return_url) = return_url {
            validate_return_url(return_url, &self.return_url_allowed_origins)
                .map_err(AppError::InvalidRequest)?;
        }

        tracing::info!(
            label = request.label.as_deref().unwrap_or_default(),
            amount = request.amount.as_deref().unwrap_or_default(),
            "received payment-session request"
        );

        let (address_id, address) = self.mint_address(
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

        let payment_id = new_payment_id();
        self.app_db
            .insert_payment_link(&payment_id, address_id, return_url)?;
        let pay_url = self.pay_url(&payment_id);

        tracing::info!(address, payment_id, "issued payment session");

        Ok(PaymentSessionResponse {
            address,
            qr_text,
            payment_id,
            pay_url,
        })
    }

    /// Records a made-up receipt for an issued address and queues its webhook, as if a
    /// payment had been mined. Only available when `ZCASH_DEV_SIMULATED_PAYMENTS` is on,
    /// which the configuration allows on testnet only. Returns the address's new total.
    pub fn simulate_payment(
        &self,
        request: SimulatePaymentRequest,
    ) -> Result<SimulatePaymentResponse, AppError> {
        if !self.dev_simulated_payments {
            return Err(AppError::NotFound("simulated payments are disabled".into()));
        }
        let value_zat = parse_zec_amount(&request.amount)
            .filter(|value| *value > 0)
            .ok_or_else(|| {
                AppError::InvalidRequest("amount must be a positive ZEC decimal".into())
            })?;
        let record = self
            .app_db
            .find_issued_address(&request.address)?
            .ok_or_else(|| AppError::NotFound("unknown address".into()))?;
        // Mark first, so the database is flagged even if recording the receipt fails partway.
        self.app_db.mark_simulated_data()?;
        let txid_hex = hex_string(&{
            let mut bytes = [0u8; 32];
            UnwrapErr(SysRng).fill_bytes(&mut bytes);
            bytes
        });
        let now = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)?;
        let affected = self.app_db.apply_attributed_receipts(
            &[AttributedScannedReceipt {
                address_id: record.address_id,
                receipt_uid: format!("{txid_hex}:simulated:0"),
                txid_hex: txid_hex.clone(),
                pool: "orchard".into(),
                value_zat,
                mined_height: 1,
                confirmation_depth: 1_000,
                eligible_for_webhook: true,
                receipt_kind: ReceiptKind::Mined,
                first_observed_at: now.clone(),
                last_observed_at: now,
            }],
            1_000,
            u32::MAX,
        )?;
        self.app_db
            .queue_webhook_deliveries_for_dirty_addresses(&affected)?;
        let total_zat = self
            .app_db
            .address_total_zat(record.address_id)?
            .unwrap_or_default();
        tracing::warn!(
            address = %request.address,
            value_zat,
            "recorded a SIMULATED payment (ZCASH_DEV_SIMULATED_PAYMENTS is on)"
        );
        Ok(SimulatePaymentResponse {
            txid: txid_hex,
            total_received: format_zat_as_zec(total_zat),
        })
    }

    fn pay_url(&self, payment_id: &str) -> String {
        format!(
            "{}/pay/{payment_id}",
            self.public_base_url.as_deref().unwrap_or_default()
        )
    }

    /// Current state of a payment link, or `NotFound` for an unknown id.
    pub fn payment_status(&self, payment_id: &str) -> Result<PaymentStatusResponse, AppError> {
        let not_found = || AppError::NotFound("unknown payment".into());
        if !is_payment_id(payment_id) {
            return Err(not_found());
        }
        let record = self
            .app_db
            .find_payment_link(payment_id)?
            .ok_or_else(not_found)?;
        let amount = record
            .requested_amount
            .clone()
            .filter(|value| !value.trim().is_empty());
        let status = payment_status_for(amount.as_deref(), record.current_total_zat);
        // After a partial payment, request only what is still owed so rescanning does not overpay.
        let request_amount = remaining_amount(amount.as_deref(), record.current_total_zat);
        Ok(PaymentStatusResponse {
            qr_text: build_qr_text(
                &record.unified_address,
                request_amount.as_deref(),
                record.request_memo.as_deref(),
                record.request_message.as_deref(),
            ),
            pay_url: self.pay_url(&record.payment_id),
            payment_id: record.payment_id,
            address: record.unified_address,
            label: record.request_label,
            message: record.request_message,
            amount,
            total_received: format_zat_as_zec(record.current_total_zat),
            state_version: record.state_version,
            status,
            return_url: record.return_url,
            created_at: record.created_at,
        })
    }

    fn mint_address(
        &self,
        label: Option<&str>,
        memo: Option<&str>,
        message: Option<&str>,
        amount: Option<&str>,
    ) -> Result<(i64, String), AppError> {
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
                        pool: "ironwood".into(),
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
        Ok((record.address_id, record.unified_address))
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

pub async fn simulate_payment(
    State(state): State<AppState>,
    Json(request): Json<SimulatePaymentRequest>,
) -> Result<Json<SimulatePaymentResponse>, AppError> {
    Ok(Json(state.service.simulate_payment(request)?))
}

pub async fn payment_status(
    State(state): State<AppState>,
    Path(payment_id): Path<String>,
) -> Result<Json<PaymentStatusResponse>, AppError> {
    Ok(Json(state.service.payment_status(&payment_id)?))
}

/// Parses a ZEC decimal such as `0.12345678` into zatoshis.
/// Returns `None` for anything else, including more than 8 decimal places.
pub fn parse_zec_amount(value: &str) -> Option<i64> {
    let value = value.trim();
    let (whole, fraction) = value.split_once('.').unwrap_or((value, ""));
    if whole.is_empty() && fraction.is_empty() {
        return None;
    }
    if fraction.len() > 8
        || !whole.bytes().all(|byte| byte.is_ascii_digit())
        || !fraction.bytes().all(|byte| byte.is_ascii_digit())
    {
        return None;
    }
    let whole: i64 = if whole.is_empty() {
        0
    } else {
        whole.parse().ok()?
    };
    let fraction: i64 = if fraction.is_empty() {
        0
    } else {
        format!("{fraction:0<8}").parse().ok()?
    };
    whole.checked_mul(100_000_000)?.checked_add(fraction)
}

pub fn payment_status_for(amount: Option<&str>, total_zat: i64) -> PaymentStatus {
    match amount.and_then(parse_zec_amount) {
        Some(requested) if total_zat >= requested => PaymentStatus::Paid,
        _ if total_zat <= 0 => PaymentStatus::AwaitingPayment,
        Some(_) => PaymentStatus::PartiallyPaid,
        None => PaymentStatus::Received,
    }
}

/// 128 random bits as 32 lowercase hex characters.
fn new_payment_id() -> String {
    let mut bytes = [0u8; 16];
    UnwrapErr(SysRng).fill_bytes(&mut bytes);
    hex_string(&bytes)
}

pub fn is_payment_id(value: &str) -> bool {
    value.len() == 32
        && value
            .bytes()
            .all(|byte| byte.is_ascii_digit() || (b'a'..=b'f').contains(&byte))
}

/// The amount still owed, as ZEC text: the requested amount when nothing (or everything) has been
/// received, otherwise the requested amount minus what was received.
pub fn remaining_amount(requested: Option<&str>, received_zat: i64) -> Option<String> {
    let requested_text = requested?;
    match parse_zec_amount(requested_text) {
        Some(requested_zat) if received_zat > 0 && received_zat < requested_zat => {
            Some(format_zat_as_zec(requested_zat - received_zat))
        }
        _ => Some(requested_text.to_string()),
    }
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
        // ZIP-321 carries the memo as unpadded base64url of its bytes.
        query.push(format!("memo={}", base64url_unpadded(memo.as_bytes())));
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

fn base64url_unpadded(bytes: &[u8]) -> String {
    const ALPHABET: &[u8; 64] = b"ABCDEFGHIJKLMNOPQRSTUVWXYZabcdefghijklmnopqrstuvwxyz0123456789-_";
    let mut out = String::with_capacity(bytes.len().div_ceil(3) * 4);
    for chunk in bytes.chunks(3) {
        let n = (u32::from(chunk[0]) << 16)
            | (u32::from(*chunk.get(1).unwrap_or(&0)) << 8)
            | u32::from(*chunk.get(2).unwrap_or(&0));
        for index in 0..=chunk.len() {
            out.push(ALPHABET[((n >> (18 - 6 * index)) & 63) as usize] as char);
        }
    }
    out
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

    use super::{
        build_qr_text, is_payment_id, parse_zec_amount, payment_status_for, HealthResponse,
        PaymentService, PaymentSessionRequest, PaymentStatus,
    };
    use crate::{
        config::Config,
        db::{AttributedScannedReceipt, ReceiptKind},
        error::AppError,
    };

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
            scan_parallelism: 2,
            sync_poll_interval_seconds: 5,
            webhook_url: None,
            webhook_secret: None,
            webhook_poll_interval_seconds: 2,
            webhook_retry_delay_seconds: 30,
            webhook_retry_max_delay_seconds: 300,
            webhook_max_attempts: 8,
            webhook_report_confirmations: 1,
            finality_confirmations: 100,
            public_base_url: None,
            dev_simulated_payments: false,
            return_url_allowed_origins: Vec::new(),
        }
    }

    #[test]
    fn remaining_amount_subtracts_partial_payments() {
        assert_eq!(
            super::remaining_amount(Some("0.5"), 0).as_deref(),
            Some("0.5")
        );
        assert_eq!(
            super::remaining_amount(Some("0.5"), 20_000_000).as_deref(),
            Some("0.30000000")
        );
        assert_eq!(
            super::remaining_amount(Some("0.5"), 50_000_000).as_deref(),
            Some("0.5")
        );
        assert_eq!(super::remaining_amount(None, 1), None);
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
            "zcash:uaddr?amount=0.12345678&memo=SW52b2ljZSBJTlYtMTAwMQ&message=Thanks%20for%20your%20purchase."
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
                return_url: None,
            })
            .unwrap();

        let second = service
            .issue_payment_session(PaymentSessionRequest {
                payment_source: "zcash".into(),
                label: None,
                memo: None,
                message: None,
                amount: None,
                return_url: None,
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
                return_url: None,
            })
            .unwrap();

        assert!(first.address.starts_with('u'));
        assert!(first
            .qr_text
            .starts_with(&format!("zcash:{}", first.address)));
    }

    #[test]
    fn minted_addresses_register_ironwood_receiver_alongside_orchard() {
        let temp = tempdir().unwrap();
        let service = PaymentService::initialize(config(temp.path())).unwrap();
        let session = service
            .issue_payment_session(PaymentSessionRequest {
                payment_source: "zcash".into(),
                label: None,
                memo: None,
                message: None,
                amount: None,
                return_url: None,
            })
            .unwrap();

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let mut statement = conn
            .prepare(
                "SELECT pool FROM address_receivers
                 JOIN issued_addresses USING (address_id)
                 WHERE unified_address = ?1
                 ORDER BY pool",
            )
            .unwrap();
        let pools = statement
            .query_map(rusqlite::params![session.address], |row| {
                row.get::<_, String>(0)
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();

        assert_eq!(pools, vec!["ironwood", "orchard", "sapling"]);

        let fingerprints: Vec<(String, Vec<u8>)> = conn
            .prepare(
                "SELECT pool, receiver_fingerprint FROM address_receivers
                 JOIN issued_addresses USING (address_id)
                 WHERE unified_address = ?1 AND pool IN ('orchard', 'ironwood')
                 ORDER BY pool",
            )
            .unwrap()
            .query_map(rusqlite::params![session.address], |row| {
                Ok((row.get(0)?, row.get(1)?))
            })
            .unwrap()
            .collect::<Result<Vec<_>, _>>()
            .unwrap();
        assert_eq!(fingerprints[0].1, fingerprints[1].1);
    }

    #[test]
    fn concurrent_fresh_address_requests_all_succeed_with_unique_addresses() {
        let temp = tempdir().unwrap();
        let service = Arc::new(PaymentService::initialize(config(temp.path())).unwrap());
        let barrier = Arc::new(Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let service = Arc::clone(&service);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    service
                        .issue_payment_session(PaymentSessionRequest {
                            payment_source: "zcash".into(),
                            label: None,
                            memo: None,
                            message: None,
                            amount: None,
                            return_url: None,
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

    fn session_request(amount: Option<&str>, return_url: Option<&str>) -> PaymentSessionRequest {
        PaymentSessionRequest {
            payment_source: "zcash".into(),
            label: Some("Order 42".into()),
            memo: None,
            message: Some("Thanks <3".into()),
            amount: amount.map(Into::into),
            return_url: return_url.map(Into::into),
        }
    }

    fn credit(service: &PaymentService, address: &str, uid: &str, value_zat: i64) {
        let address_id = service
            .app_db
            .find_issued_address(address)
            .unwrap()
            .unwrap()
            .address_id;
        service
            .app_db
            .apply_attributed_receipts(
                &[AttributedScannedReceipt {
                    address_id,
                    txid_hex: format!("tx-{uid}"),
                    pool: "orchard".into(),
                    receipt_uid: uid.into(),
                    value_zat,
                    mined_height: 100,
                    confirmation_depth: 5,
                    eligible_for_webhook: true,
                    receipt_kind: ReceiptKind::Mined,
                    first_observed_at: "2026-01-01T00:00:00Z".into(),
                    last_observed_at: "2026-01-01T00:00:00Z".into(),
                }],
                104,
                100,
            )
            .unwrap();
    }

    #[test]
    fn parses_zec_amounts() {
        assert_eq!(parse_zec_amount("0.12345678"), Some(12_345_678));
        assert_eq!(parse_zec_amount("1"), Some(100_000_000));
        assert_eq!(parse_zec_amount("1.5"), Some(150_000_000));
        assert_eq!(parse_zec_amount(".5"), Some(50_000_000));
        assert_eq!(parse_zec_amount(" 2.00000001 "), Some(200_000_001));
        assert_eq!(parse_zec_amount("0.123456789"), None);
        assert_eq!(parse_zec_amount("-1"), None);
        assert_eq!(parse_zec_amount("1e3"), None);
        assert_eq!(parse_zec_amount("."), None);
        assert_eq!(parse_zec_amount(""), None);
        assert_eq!(parse_zec_amount("99999999999999999999"), None);
    }

    #[test]
    fn status_compares_total_with_requested_amount() {
        use PaymentStatus::*;
        assert_eq!(payment_status_for(Some("0.5"), 0), AwaitingPayment);
        assert_eq!(payment_status_for(Some("0.5"), 10), PartiallyPaid);
        assert_eq!(payment_status_for(Some("0.5"), 50_000_000), Paid);
        assert_eq!(payment_status_for(Some("0.5"), 60_000_000), Paid);
        assert_eq!(payment_status_for(None, 0), AwaitingPayment);
        assert_eq!(payment_status_for(None, 1), Received);
        assert_eq!(payment_status_for(Some("about 1 ZEC"), 1), Received);
    }

    #[test]
    fn sessions_get_unguessable_payment_ids_and_pay_urls() {
        let temp = tempdir().unwrap();
        let service = PaymentService::initialize(config(temp.path())).unwrap();
        let first = service
            .issue_payment_session(session_request(Some("0.5"), None))
            .unwrap();
        let second = service
            .issue_payment_session(session_request(Some("0.5"), None))
            .unwrap();
        assert!(is_payment_id(&first.payment_id));
        assert_ne!(first.payment_id, second.payment_id);
        assert_eq!(first.pay_url, format!("/pay/{}", first.payment_id));

        let mut with_base = config(temp.path());
        with_base.public_base_url = Some("https://pay.example.com/zec".into());
        let service = PaymentService::initialize(with_base).unwrap();
        let third = service
            .issue_payment_session(session_request(None, None))
            .unwrap();
        assert_eq!(
            third.pay_url,
            format!("https://pay.example.com/zec/pay/{}", third.payment_id)
        );
    }

    #[test]
    fn enforces_return_url_allowed_origins() {
        let temp = tempdir().unwrap();
        let mut restricted = config(temp.path());
        restricted.return_url_allowed_origins = vec!["https://shop.example".into()];
        let service = PaymentService::initialize(restricted).unwrap();
        service
            .issue_payment_session(session_request(None, Some("https://shop.example/o/1")))
            .unwrap();
        let error = service
            .issue_payment_session(session_request(None, Some("https://evil.example/")))
            .unwrap_err();
        assert!(
            matches!(error, AppError::InvalidRequest(ref text) if text.starts_with("return_url"))
        );
    }

    #[test]
    fn rejects_return_urls_that_are_not_absolute_http() {
        let temp = tempdir().unwrap();
        let service = PaymentService::initialize(config(temp.path())).unwrap();
        for bad in [
            "/thanks",
            "javascript:alert(1)",
            "data:text/html,x",
            "ftp://example.com/x",
            "https://shop.example@evil.example/",
        ] {
            let error = service
                .issue_payment_session(session_request(None, Some(bad)))
                .unwrap_err();
            assert!(
                matches!(error, AppError::InvalidRequest(ref text) if text.starts_with("return_url")),
                "{bad}: {error}"
            );
        }
        service
            .issue_payment_session(session_request(None, Some("  ")))
            .unwrap();
    }

    #[test]
    fn payment_status_tracks_received_total() {
        let temp = tempdir().unwrap();
        let service = PaymentService::initialize(config(temp.path())).unwrap();
        let session = service
            .issue_payment_session(session_request(
                Some("0.5"),
                Some("https://shop.example/thanks?order=42"),
            ))
            .unwrap();

        let status = service.payment_status(&session.payment_id).unwrap();
        assert_eq!(status.status, PaymentStatus::AwaitingPayment);
        assert_eq!(status.address, session.address);
        assert_eq!(status.qr_text, session.qr_text);
        assert_eq!(status.amount.as_deref(), Some("0.5"));
        assert_eq!(status.total_received, "0.00000000");
        assert_eq!(
            status.return_url.as_deref(),
            Some("https://shop.example/thanks?order=42")
        );

        credit(&service, &session.address, "a", 20_000_000);
        let status = service.payment_status(&session.payment_id).unwrap();
        assert_eq!(status.status, PaymentStatus::PartiallyPaid);
        assert_eq!(status.total_received, "0.20000000");
        assert_eq!(status.state_version, 1);

        credit(&service, &session.address, "b", 30_000_000);
        let status = service.payment_status(&session.payment_id).unwrap();
        assert_eq!(status.status, PaymentStatus::Paid);
        assert_eq!(status.total_received, "0.50000000");
    }

    #[test]
    fn unknown_or_malformed_payment_ids_are_not_found() {
        let temp = tempdir().unwrap();
        let service = PaymentService::initialize(config(temp.path())).unwrap();
        for id in ["0123456789abcdef0123456789abcdef", "nope", "../../etc"] {
            assert!(matches!(
                service.payment_status(id).unwrap_err(),
                AppError::NotFound(_)
            ));
        }
    }

    #[tokio::test]
    async fn router_serves_status_json_and_pay_page() {
        let temp = tempdir().unwrap();
        let service = Arc::new(PaymentService::initialize(config(temp.path())).unwrap());
        let session = service
            .issue_payment_session(session_request(
                Some("0.5"),
                Some("https://shop.example/done"),
            ))
            .unwrap();
        let app = crate::build_router(crate::AppState {
            service: service.clone(),
        });
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let base = format!("http://{}", listener.local_addr().unwrap());
        tokio::spawn(async move { axum::serve(listener, app).await.unwrap() });
        let http = reqwest::Client::new();

        let status: serde_json::Value = http
            .get(format!("{base}/payments/{}", session.payment_id))
            .send()
            .await
            .unwrap()
            .json()
            .await
            .unwrap();
        assert_eq!(status["status"], "awaiting_payment");
        assert_eq!(status["address"], session.address.as_str());

        let page = http
            .get(format!("{base}{}", session.pay_url))
            .send()
            .await
            .unwrap();
        assert_eq!(page.status(), 200);
        let csp = page.headers()["content-security-policy"].to_str().unwrap();
        assert!(csp.contains("connect-src 'self'"));
        let html = page.text().await.unwrap();
        assert!(html.contains("<svg"));
        assert!(html.contains("Thanks &lt;3"));
        assert!(!html.contains("Thanks <3"));
        assert!(html.contains("https://shop.example/done"));
        assert!(html.contains(&session.address));

        let missing = http
            .get(format!("{base}/pay/0123456789abcdef0123456789abcdef"))
            .send()
            .await
            .unwrap();
        assert_eq!(missing.status(), 404);
    }

    const TESTNET_UIVK: &str = "uivktest1ayc2s599yatrj5kgp2fr4sn0xa2enkykd858d5p7rfwq8u3u65zf3dgzdar2jqzm6nzck5laqdhhfuj849hv8t3gzkgqmecvq49420024aucsy73rxsa9222n8uczqmgus06nx80mjf5kl3tq4qnc3tjj3muhdxhmnavdfqaa8vjk7aqq03ll5sfntag00f4av72q89xwfkw3g26ez3mge8k0xt3kzs5hgzj5sz9fkqdgu4mphd";

    fn simulate(service: &PaymentService, address: &str, amount: &str) -> Result<String, AppError> {
        service
            .simulate_payment(super::SimulatePaymentRequest {
                address: address.into(),
                amount: amount.into(),
            })
            .map(|response| response.total_received)
    }

    #[test]
    fn simulated_payments_are_off_by_default() {
        let temp = tempdir().unwrap();
        let service = PaymentService::initialize(config(temp.path())).unwrap();
        let session = service
            .issue_payment_session(session_request(Some("1"), None))
            .unwrap();
        assert!(matches!(
            simulate(&service, &session.address, "1").unwrap_err(),
            AppError::NotFound(_)
        ));
    }

    #[test]
    fn simulated_payments_are_ignored_on_mainnet_even_if_enabled() {
        let temp = tempdir().unwrap();
        let mut config = config(temp.path());
        config.dev_simulated_payments = true;
        let service = PaymentService::initialize(config).unwrap();
        let session = service
            .issue_payment_session(session_request(Some("1"), None))
            .unwrap();
        assert!(matches!(
            simulate(&service, &session.address, "1").unwrap_err(),
            AppError::NotFound(_)
        ));
    }

    #[test]
    fn simulated_payments_update_totals_and_queue_webhooks_on_testnet() {
        let temp = tempdir().unwrap();
        let mut config = config(temp.path());
        config.network = "testnet".into();
        config.startup_uivk = Some(TESTNET_UIVK.into());
        config.dev_simulated_payments = true;
        let service = PaymentService::initialize(config.clone()).unwrap();
        let session = service
            .issue_payment_session(session_request(Some("0.5"), None))
            .unwrap();

        assert_eq!(
            simulate(&service, &session.address, "0.2").unwrap(),
            "0.20000000"
        );
        assert_eq!(
            service.payment_status(&session.payment_id).unwrap().status,
            PaymentStatus::PartiallyPaid
        );
        assert_eq!(
            simulate(&service, &session.address, "0.3").unwrap(),
            "0.50000000"
        );
        assert_eq!(
            service.payment_status(&session.payment_id).unwrap().status,
            PaymentStatus::Paid
        );

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let queued: i64 = conn
            .query_row("SELECT COUNT(*) FROM webhook_deliveries", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert!(queued >= 1);

        assert!(matches!(
            simulate(&service, &session.address, "0").unwrap_err(),
            AppError::InvalidRequest(_)
        ));
        assert!(matches!(
            simulate(&service, "u1unknown", "1").unwrap_err(),
            AppError::NotFound(_)
        ));

        // The same database can't be reused for real scanning.
        drop(service);
        config.dev_simulated_payments = false;
        config.lightwalletd_url = Some("https://lwd.example:443".into());
        assert!(matches!(
            PaymentService::initialize(config),
            Err(AppError::StartupIntegrity(_))
        ));
    }
}
