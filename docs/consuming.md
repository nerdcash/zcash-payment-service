# Consuming zcash-payment-service

This guide is for application developers who want to integrate
`zcash-payment-service` without reading the entire codebase first.

## Choose an integration mode

Use the service in one of two ways:

1. **Container or standalone binary** when you want a clean process boundary and
   simple operations.
2. **Rust crate** when you want to embed the router, persistence, and background
   workers directly inside your own Tokio application.

For most applications, the container mode is the better default.

## What your application owns

Your application should own:

- pricing, quote lifetime, and invoice creation
- the call to `POST /payment-session` when a checkout session starts
- the webhook endpoint that receives payment observations
- idempotency on `event_id`
- business-state transitions after a payment observation is accepted

The service owns:

- Zcash-specific address derivation from a canonical UIVK
- persistence of issued addresses, receiver fingerprints, and receipt totals
- compact-block and mempool scanning
- retrying outbound webhook deliveries

## Integrating over HTTP

### 1. Configure the service

At minimum, set:

- `ZCASH_UIVK`
- `ZCASH_BIRTHDAY_HEIGHT`
- `LIGHTWALLETD_URL`

Set these as well if you want automatic callbacks:

- `ZCASH_WEBHOOK_URL`
- `ZCASH_WEBHOOK_SECRET`

Copy `.env.example` to `.env` for local development.

### 2. Start the service

From source:

```powershell
cargo run
```

With Docker:

```powershell
docker compose up --build
```

### 3. Call `POST /payment-session`

Request:

```json
{
  "payment_source": "zcash",
  "label": "Pro annual plan",
  "memo": "Invoice INV-1001",
  "message": "Thanks for your purchase.",
  "amount": "0.12345678"
}
```

Response:

```json
{
  "address": "u1exampleaddress",
  "qr_text": "zcash:u1exampleaddress?amount=0.12345678&memo=Invoice%20INV-1001&message=Thanks%20for%20your%20purchase."
}
```

Important response behavior:

- every successful call mints a fresh address
- `qr_text` is ready to turn into a QR code
- `payment_source` must be `zcash`

### 4. Accept outbound webhooks

When webhook delivery is enabled, the service sends JSON like:

```json
{
  "event_id": "zcash:42:v3",
  "payment_source": "zcash",
  "address": "u1exampleaddress",
  "total_received": "0.12345678",
  "observed_at": "2026-01-01T00:00:00Z"
}
```

Treat `event_id` as the deduplication key.

`total_received` is the cumulative eligible amount observed for that address, not
the wallet balance.

### 5. Verify `X-Signature`

The service signs the raw request body with HMAC-SHA256 using
`ZCASH_WEBHOOK_SECRET` and sends the lowercase hex digest in `X-Signature`.

Your webhook handler should:

1. read the raw request body
2. recompute the HMAC using the shared secret
3. compare it to `X-Signature` in constant time
4. reject the request if the signature does not match
5. apply idempotency using `event_id`

## Example Compose wiring

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
    depends_on:
      zcash-payment-service:
        condition: service_healthy
```

## Embedding the Rust crate

Add the dependency:

```toml
[dependencies]
zcash-payment-service = { git = "https://github.com/nerdcash/zcash-payment-service.git" }
```

Minimal embedding example:

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

When embedding, you are responsible for:

- providing Tokio runtime ownership
- deciding how your application should handle shutdown
- surfacing health and metrics at your preferred boundaries
- configuring environment variables or building your own config loader around
  `Config`

## Operational notes

- Persist `APP_DATA_DIR` or the explicit DB paths across restarts.
- `APP_DB_PATH` and `WALLET_DB_PATH` must point to different files.
- If `ZCASH_WEBHOOK_URL` or `ZCASH_WEBHOOK_SECRET` is missing, the service still
  runs but leaves outbound webhook delivery disabled.
- `WEBHOOK_REPORT_CONFIRMATIONS=0` enables provisional mempool reporting. If you
  prefer to notify your app only after mined confirmations, raise this value.
- Use `GET /health` for orchestration checks and for observing whether the
  scanner is caught up.
