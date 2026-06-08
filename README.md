# zcash-payment-service

`zcash-payment-service` is a reusable Zcash payment allocator, scanner, and
webhook producer. You can consume it either as:

1. an HTTP service that your application talks to over `POST /payment-session`
   and receives payment observations from by webhook, or
2. a Rust crate that you embed inside your own process to reuse the router,
   wallet logic, persistence, and background workers.

The current implementation includes:

- persistent service-owned SQLite storage
- startup wallet-identity integrity checks around the configured UIVK
- real UIVK-derived unified address allocation with Orchard and Sapling receivers
- compact-block and mempool scanning primitives built on `librustzcash`
- a long-lived sync loop with overlap rescans for reorg safety
- an outbound webhook delivery worker with HMAC signing and bounded retries
- rolling file logs for operational diagnostics

The scanner is usable today, but still intentionally conservative. It rescans a
recent finalized window to catch short reorgs, advances receipt maturity as the
tip moves, and falls back to polling if the lightwalletd mempool stream is
unavailable.

## Quick start

1. Copy `.env.example` to `.env`.
2. Fill in `ZCASH_UIVK`, `ZCASH_BIRTHDAY_HEIGHT`, and `LIGHTWALLETD_URL`.
3. Optionally set `ZCASH_WEBHOOK_URL` and `ZCASH_WEBHOOK_SECRET` if you want the
   service to push observations back into your app.
4. Start the service.

From source:

```powershell
cargo run
```

With Docker Compose:

```powershell
docker compose up --build
```

## HTTP API summary

The service exposes:

- `GET /health`
- `POST /payment-session`

`GET /health` reports liveness and persisted sync progress:

```json
{
  "ok": true,
  "current_sync_state": "synced",
  "detected_tip_height": 123456,
  "last_scanned_height": 123450
}
```

`POST /payment-session` allocates a fresh unified address and returns QR-ready
text:

```json
{
  "payment_source": "zcash",
  "label": "Pro annual plan",
  "memo": "Invoice INV-1001",
  "message": "Thanks for your purchase.",
  "amount": "0.12345678"
}
```

```json
{
  "address": "u1exampleaddress",
  "qr_text": "zcash:u1exampleaddress?amount=0.12345678&memo=Invoice%20INV-1001&message=Thanks%20for%20your%20purchase."
}
```

## Webhook contract summary

When both `ZCASH_WEBHOOK_URL` and `ZCASH_WEBHOOK_SECRET` are configured, the
service queues outbound webhook deliveries whenever the cumulative
`total_received` for an issued address changes.

- Method: `POST`
- Content type: `application/json`
- Signature header: `X-Signature`
- Signature format: lowercase hex HMAC-SHA256 of the raw request body

Example payload:

```json
{
  "event_id": "zcash:42:v3",
  "payment_source": "zcash",
  "address": "u1exampleaddress",
  "total_received": "0.12345678",
  "observed_at": "2026-01-01T00:00:00Z"
}
```

Your application should treat `event_id` as the idempotency key and verify the
signature before mutating business state.

## Consuming the Docker image

The repository includes a publish workflow that pushes:

- `ghcr.io/nerdcash/zcash-payment-service:<git-sha>`
- `ghcr.io/nerdcash/zcash-payment-service:main`

Those images are intended to be public. A normal pull does not require
registry credentials:

```powershell
docker pull ghcr.io/nerdcash/zcash-payment-service:main
```

Typical Compose wiring from another app looks like:

```yaml
services:
  zcash-payment-service:
    image: ghcr.io/nerdcash/zcash-payment-service:main
    environment:
      ZCASH_NETWORK: mainnet
      ZCASH_UIVK: ${ZCASH_UIVK}
      ZCASH_BIRTHDAY_HEIGHT: ${ZCASH_BIRTHDAY_HEIGHT}
      LIGHTWALLETD_URL: ${LIGHTWALLETD_URL}
      ZCASH_WEBHOOK_URL: http://consumer-app:8080/webhooks/zcash-payments
      ZCASH_WEBHOOK_SECRET: ${ZCASH_WEBHOOK_SECRET}
    volumes:
      - ./data/zcash-payment:/data/zcash-payment

  consumer-app:
    image: ghcr.io/example/consumer-app:main
    environment:
      ZCASH_PAYMENT_SERVICE_BASE_URL: http://zcash-payment-service:8787
```

## Consuming the crate

The crate is currently intended to be consumed directly from Git:

```toml
[dependencies]
zcash-payment-service = { git = "https://github.com/nerdcash/zcash-payment-service.git" }
```

You can embed the service in your own Tokio process:

```rust
use std::sync::Arc;

use zcash_payment_service::{
    build_router,
    config::Config,
    service::PaymentService,
    wallet_runtime::spawn_wallet_sync_worker,
    webhook_delivery::spawn_webhook_delivery_worker,
    AppState,
};

#[tokio::main]
async fn main() -> Result<(), zcash_payment_service::error::AppError> {
    let config = Config::from_env()?;
    let service = Arc::new(PaymentService::initialize(config.clone())?);

    spawn_wallet_sync_worker(config.clone());
    spawn_webhook_delivery_worker(config.clone());

    let app = build_router(AppState { service });
    let listener = tokio::net::TcpListener::bind(&config.listen_addr).await?;
    axum::serve(listener, app).await?;

    Ok(())
}
```

## Key configuration

Important environment variables for the current implementation:

- `ZCASH_UIVK` — required on first startup unless the wallet DB already contains
  the canonical wallet identity
- `ZCASH_BIRTHDAY_HEIGHT` — required on first startup and validated on later
  restarts if supplied
- `ZCASH_NETWORK` — `testnet` or `mainnet`
- `LIGHTWALLETD_URL` — required for live chain scanning
- `ZCASH_WEBHOOK_URL` — optional absolute callback URL for outbound payment
  observations
- `ZCASH_WEBHOOK_SECRET` — optional HMAC secret; required when
  `ZCASH_WEBHOOK_URL` is set
- `APP_DB_PATH` / `WALLET_DB_PATH` — service-owned SQLite files that must point
  at different paths
- `APP_DATA_DIR` — base directory for default DB and log locations
- `CATCH_UP_BATCH_SIZE` — max compact-block batch size per catch-up chunk
- `SYNC_POLL_INTERVAL_SECONDS` — fallback idle polling interval
- `WEBHOOK_POLL_INTERVAL_SECONDS` — idle polling interval for outbound webhook work
- `WEBHOOK_RETRY_DELAY_SECONDS` / `WEBHOOK_RETRY_MAX_DELAY_SECONDS` —
  exponential backoff bounds for delivery retries
- `WEBHOOK_MAX_ATTEMPTS` — retry ceiling before a delivery becomes permanent
- `WEBHOOK_REPORT_CONFIRMATIONS` — defaults to `0`; at `0`, provisional mempool
  receipts count for up to 24 hours

## Documentation map

- `docs/consuming.md` — practical guide for integrating another app with the
  service as a container or crate
- `design.md` — service design decisions and invariants
- `src/spec.md` — Zcash-specific HTTP and webhook contract details
- `specs/cryptocurrency-microservice/spec.md` — generic host-app/service
  responsibilities

## Development commands

```powershell
cargo fmt --all --check
cargo test
cargo build --release
cargo clippy --all-targets -- -D warnings
docker compose config
```

Run the live testnet scanner-vector test manually:

```powershell
$env:ZCASH_TESTNET_LIGHTWALLETD_URL = "https://testnet.lightwalletd.com:9067"
cargo test scanner::tests::live_testnet_vectors_are_detected_with_expected_amounts -- --ignored --exact
```

That test is opt-in because it depends on external lightwalletd availability.
