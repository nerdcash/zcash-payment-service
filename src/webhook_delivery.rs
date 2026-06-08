use std::time::Instant;

use hmac::{Hmac, Mac};
use reqwest::StatusCode;
use sha2::Sha256;
use time::{format_description::well_known::Rfc3339, Duration as TimeDuration, OffsetDateTime};

use crate::{config::Config, db::AppDb, error::AppError};

type HmacSha256 = Hmac<Sha256>;

pub fn spawn_webhook_delivery_worker(config: Config) {
    if config.webhook_url.is_none() || config.webhook_secret.is_none() {
        tracing::warn!(
            webhook_url_configured = config.webhook_url.is_some(),
            webhook_secret_configured = config.webhook_secret.is_some(),
            "webhook delivery worker is disabled until ZCASH_WEBHOOK_URL and ZCASH_WEBHOOK_SECRET are configured"
        );
        return;
    }

    tokio::spawn(async move {
        if let Err(error) = delivery_loop(config).await {
            tracing::error!(error = %error, "webhook delivery worker exited with error");
        }
    });
}

async fn delivery_loop(config: Config) -> Result<(), AppError> {
    let app_db = AppDb::open(&config.app_db_path)?;
    let client = reqwest::Client::builder().build()?;

    loop {
        match deliver_ready_once(&app_db, &client, &config).await {
            Ok(true) => continue,
            Ok(false) => {
                tokio::time::sleep(std::time::Duration::from_secs(
                    config.webhook_poll_interval_seconds,
                ))
                .await;
            }
            Err(error) => {
                tracing::error!(error = %error, "webhook delivery iteration failed");
                tokio::time::sleep(std::time::Duration::from_secs(
                    config.webhook_poll_interval_seconds,
                ))
                .await;
            }
        }
    }
}

async fn deliver_ready_once(
    app_db: &AppDb,
    client: &reqwest::Client,
    config: &Config,
) -> Result<bool, AppError> {
    let now = now_rfc3339()?;
    let Some(delivery) = app_db.claim_ready_webhook_delivery(&now)? else {
        return Ok(false);
    };

    let webhook_url = config
        .webhook_url
        .as_deref()
        .ok_or_else(|| AppError::InvalidConfig("ZCASH_WEBHOOK_URL is not configured".into()))?;
    let webhook_secret = config
        .webhook_secret
        .as_deref()
        .ok_or_else(|| AppError::InvalidConfig("ZCASH_WEBHOOK_SECRET is not configured".into()))?;

    let signature = sign_body(webhook_secret, delivery.request_body_json.as_bytes())?;
    let started = Instant::now();
    let response = client
        .post(webhook_url)
        .header("Content-Type", "application/json")
        .header("X-Signature", signature)
        .body(delivery.request_body_json.clone())
        .send()
        .await;
    let duration_ms = i64::try_from(started.elapsed().as_millis()).unwrap_or(i64::MAX);
    let attempted_at = now_rfc3339()?;

    match response {
        Ok(response) => {
            let status = i64::from(response.status().as_u16());
            let retryable = response.status().is_server_error()
                || response.status() == StatusCode::TOO_MANY_REQUESTS;
            let success = response.status().is_success();
            let response_body = response
                .text()
                .await
                .unwrap_or_else(|error| format!("failed to read response body: {error}"));

            if success {
                app_db.record_webhook_delivery_success(
                    delivery.delivery_id,
                    &attempted_at,
                    status,
                    &response_body,
                    duration_ms,
                )?;
                tracing::info!(
                    delivery_id = delivery.delivery_id,
                    event_id = %delivery.event_id,
                    webhook_url,
                    http_status = status,
                    "webhook delivery succeeded"
                );
            } else if retryable {
                if should_permanently_fail(config, delivery.attempt_count) {
                    app_db.record_webhook_delivery_permanent_failure(
                        delivery.delivery_id,
                        &attempted_at,
                        Some(status),
                        Some(&response_body),
                        None,
                        duration_ms,
                    )?;
                    tracing::error!(
                        delivery_id = delivery.delivery_id,
                        event_id = %delivery.event_id,
                        webhook_url,
                        http_status = status,
                        attempt_count = delivery.attempt_count,
                        max_attempts = config.webhook_max_attempts,
                        "webhook delivery failed permanently after exhausting retries"
                    );
                } else {
                    let retry_delay_seconds =
                        next_retry_delay_seconds(config, delivery.attempt_count);
                    let next_attempt_at = retry_at(retry_delay_seconds)?;
                    app_db.record_webhook_delivery_retry(
                        delivery.delivery_id,
                        &attempted_at,
                        &next_attempt_at,
                        Some(status),
                        Some(&response_body),
                        None,
                        duration_ms,
                    )?;
                    tracing::warn!(
                        delivery_id = delivery.delivery_id,
                        event_id = %delivery.event_id,
                        webhook_url,
                        http_status = status,
                        next_attempt_at = %next_attempt_at,
                        retry_delay_seconds,
                        "webhook delivery failed and will be retried"
                    );
                }
            } else {
                app_db.record_webhook_delivery_permanent_failure(
                    delivery.delivery_id,
                    &attempted_at,
                    Some(status),
                    Some(&response_body),
                    None,
                    duration_ms,
                )?;
                tracing::error!(
                    delivery_id = delivery.delivery_id,
                    event_id = %delivery.event_id,
                    webhook_url,
                    http_status = status,
                    "webhook delivery failed permanently"
                );
            }
        }
        Err(error) => {
            if should_permanently_fail(config, delivery.attempt_count) {
                app_db.record_webhook_delivery_permanent_failure(
                    delivery.delivery_id,
                    &attempted_at,
                    None,
                    None,
                    Some(&error.to_string()),
                    duration_ms,
                )?;
                tracing::error!(
                    delivery_id = delivery.delivery_id,
                    event_id = %delivery.event_id,
                    webhook_url,
                    error = %error,
                    attempt_count = delivery.attempt_count,
                    max_attempts = config.webhook_max_attempts,
                    "webhook delivery hit a transport error and exhausted retries"
                );
            } else {
                let retry_delay_seconds = next_retry_delay_seconds(config, delivery.attempt_count);
                let next_attempt_at = retry_at(retry_delay_seconds)?;
                app_db.record_webhook_delivery_retry(
                    delivery.delivery_id,
                    &attempted_at,
                    &next_attempt_at,
                    None,
                    None,
                    Some(&error.to_string()),
                    duration_ms,
                )?;
                tracing::warn!(
                    delivery_id = delivery.delivery_id,
                    event_id = %delivery.event_id,
                    webhook_url,
                    error = %error,
                    next_attempt_at = %next_attempt_at,
                    retry_delay_seconds,
                    "webhook delivery hit a transport error and will be retried"
                );
            }
        }
    }

    Ok(true)
}

fn sign_body(secret: &str, body: &[u8]) -> Result<String, AppError> {
    let mut mac = HmacSha256::new_from_slice(secret.as_bytes())
        .map_err(|error| AppError::Wallet(format!("failed to construct webhook HMAC: {error}")))?;
    mac.update(body);
    Ok(hex::encode(mac.finalize().into_bytes()))
}

fn now_rfc3339() -> Result<String, AppError> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}

fn retry_at(delay_seconds: u64) -> Result<String, AppError> {
    let retry_time = OffsetDateTime::now_utc()
        .checked_add(TimeDuration::seconds(
            i64::try_from(delay_seconds).unwrap_or(i64::MAX),
        ))
        .ok_or_else(|| AppError::Wallet("webhook retry delay overflowed timestamp".into()))?;
    Ok(retry_time.format(&Rfc3339)?)
}

fn next_retry_delay_seconds(config: &Config, current_attempt_count: i64) -> u64 {
    let attempt_index = u32::try_from(current_attempt_count.max(0)).unwrap_or(u32::MAX);
    let multiplier = if attempt_index >= 63 {
        u64::MAX
    } else {
        1u64 << attempt_index
    };
    config
        .webhook_retry_delay_seconds
        .saturating_mul(multiplier)
        .min(config.webhook_retry_max_delay_seconds)
}

fn should_permanently_fail(config: &Config, current_attempt_count: i64) -> bool {
    let next_attempt_number = u32::try_from(current_attempt_count.max(0))
        .unwrap_or(u32::MAX)
        .saturating_add(1);
    next_attempt_number >= config.webhook_max_attempts
}

#[cfg(test)]
mod tests {
    use std::sync::Arc;

    use axum::{
        body::Bytes,
        extract::State,
        http::{HeaderMap, StatusCode},
        response::IntoResponse,
        routing::post,
        Router,
    };
    use tokio::sync::Mutex;

    use super::deliver_ready_once;
    use crate::{
        config::Config,
        db::{AppDb, AttributedScannedReceipt, NewAddressReceiver, NewIssuedAddress, ReceiptKind},
    };

    #[derive(Clone, Default)]
    struct CaptureState {
        requests: Arc<Mutex<Vec<(String, String)>>>,
    }

    async fn capture_webhook(
        State(state): State<CaptureState>,
        headers: HeaderMap,
        body: Bytes,
    ) -> impl IntoResponse {
        let signature = headers
            .get("X-Signature")
            .and_then(|value| value.to_str().ok())
            .unwrap_or_default()
            .to_string();
        state
            .requests
            .lock()
            .await
            .push((signature, String::from_utf8_lossy(&body).into_owned()));
        (StatusCode::OK, "ok")
    }

    fn test_config(temp: &std::path::Path, webhook_url: String) -> Config {
        Config {
            listen_addr: "127.0.0.1:0".into(),
            network: "testnet".into(),
            startup_uivk: Some("uivk-one".into()),
            lightwalletd_url: None,
            birthday_height: Some(123),
            wallet_db_path: temp.join("wallet.db"),
            app_db_path: temp.join("app.db"),
            log_dir: temp.join("logs"),
            catch_up_threshold_blocks: 1,
            catch_up_batch_size: 100,
            sync_poll_interval_seconds: 5,
            webhook_url: Some(webhook_url),
            webhook_secret: Some("zcash-secret".into()),
            webhook_poll_interval_seconds: 1,
            webhook_retry_delay_seconds: 5,
            webhook_retry_max_delay_seconds: 20,
            webhook_max_attempts: 3,
            webhook_report_confirmations: 1,
            finality_confirmations: 100,
        }
    }

    #[test]
    fn retry_delay_grows_exponentially_until_cap() {
        let temp = tempfile::tempdir().unwrap();
        let config = test_config(temp.path(), "http://127.0.0.1".into());

        assert_eq!(super::next_retry_delay_seconds(&config, 0), 5);
        assert_eq!(super::next_retry_delay_seconds(&config, 1), 10);
        assert_eq!(super::next_retry_delay_seconds(&config, 2), 20);
        assert_eq!(super::next_retry_delay_seconds(&config, 3), 20);
    }

    #[test]
    fn retry_budget_is_exhausted_on_configured_attempt_limit() {
        let temp = tempfile::tempdir().unwrap();
        let config = test_config(temp.path(), "http://127.0.0.1".into());

        assert!(!super::should_permanently_fail(&config, 0));
        assert!(!super::should_permanently_fail(&config, 1));
        assert!(super::should_permanently_fail(&config, 2));
    }

    #[tokio::test]
    async fn deliver_ready_once_posts_signed_json_and_marks_success() {
        let temp = tempfile::tempdir().unwrap();
        let capture_state = CaptureState::default();
        let app = Router::new()
            .route("/", post(capture_webhook))
            .with_state(capture_state.clone());
        let listener = tokio::net::TcpListener::bind("127.0.0.1:0").await.unwrap();
        let address = listener.local_addr().unwrap();
        let server = tokio::spawn(async move {
            axum::serve(listener, app).await.unwrap();
        });

        let config = test_config(temp.path(), format!("http://{address}/"));
        let app_db = AppDb::open(&config.app_db_path).unwrap();
        let record = app_db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1deliver",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        app_db
            .insert_address_receivers(
                record.address_id,
                &[NewAddressReceiver {
                    pool: "sapling",
                    receiver_encoding: "sapling-d",
                    receiver_fingerprint: &[4, 4, 4, 4, 4, 4, 4, 4],
                }],
            )
            .unwrap();
        app_db
            .apply_attributed_receipts(
                &[AttributedScannedReceipt {
                    address_id: record.address_id,
                    txid_hex: "tx-deliver".into(),
                    pool: "sapling".into(),
                    receipt_uid: "tx-deliver:sapling:0".into(),
                    value_zat: 100_000_000,
                    mined_height: 500,
                    confirmation_depth: 10,
                    eligible_for_webhook: true,
                    receipt_kind: ReceiptKind::Mined,
                    first_observed_at: "2026-01-01T00:00:00Z".into(),
                    last_observed_at: "2026-01-01T00:00:00Z".into(),
                }],
                509,
                100,
            )
            .unwrap();
        app_db
            .queue_webhook_deliveries_for_dirty_addresses(&[record.address_id])
            .unwrap();

        let client = reqwest::Client::builder().build().unwrap();
        let delivered = deliver_ready_once(&app_db, &client, &config).await.unwrap();

        assert!(delivered);

        let requests = capture_state.requests.lock().await;
        assert_eq!(requests.len(), 1);
        assert!(requests[0].1.contains("\"payment_source\":\"zcash\""));
        assert!(!requests[0].0.is_empty());

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let state: (String, i64) = conn
            .query_row(
                "SELECT delivery_state, attempt_count FROM webhook_deliveries LIMIT 1",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state.0, "succeeded");
        assert_eq!(state.1, 1);

        server.abort();
    }
}
