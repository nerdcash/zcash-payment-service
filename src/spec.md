# Zcash payment microservice supplement

## Purpose

This document describes the Zcash-specific behavior for the
`zcash-payment-service`.

It supplements the shared cryptocurrency microservice contract documented in:

- `specs/cryptocurrency-microservice/spec.md`

## Current implementation status

This repository now contains the first implementation slice of the real service
at its root.

It includes persistent service-owned storage, startup wallet-identity checks,
rotating logs, real UIVK-derived unified address allocation, a background
sync task, compact-block incoming-payment scanning primitives, outbound webhook
delivery, and a production-oriented application skeleton.

The blockchain scan loop is still pre-production, but it now includes continuous
operation with mempool-stream block wakeups, overlap rescans for recent-height
reorg correction, receipt maturity advancement, and bounded webhook retry
behavior.

The current startup task constructs scanner keys from the configured UIVK,
performs bounded catch-up scanning through lightwalletd when configured, stores
observed receipt totals in the app DB, advances webhook eligibility for stored
receipts as confirmations mature, rescans a recent overlap window for reorg
correction, and queues webhook deliveries for dirty addresses.

The steady-state loop now uses `GetMempoolStream` as the primary signal that a
new block has been mined. It still falls back to configurable polling when the
mempool stream cannot be opened or terminates unexpectedly.

Deterministic runtime coverage now exercises cold-start catch-up, deep rewinds,
and repeated rewind/recovery flap sequences without depending on a live
lightwalletd instance.

When `WEBHOOK_REPORT_CONFIRMATIONS` is `0`, the service also maintains a
reconciled view of current mempool receipts for issued addresses. Those receipts
are provisional: they count immediately, are revoked if their transaction leaves
the mempool, and age out after 24 hours until the transaction is confirmed.

## Configuration

### Consumer application

The application consuming this service typically owns settings such as:

| Variable | Description |
| --- | --- |
| `ZCASH_PAYMENT_SERVICE_BASE_URL` | Base URL for the payment microservice |
| `ZCASH_WEBHOOK_SECRET` | HMAC secret shared with the microservice for outbound webhook signing |
| `PAYMENT_QUOTE_TTL_SECONDS` | Optional quote lifetime owned by the consuming app |
| `PLAN_PRICE_*` | Any application-specific pricing or plan metadata |

### Zcash payment microservice

The current implementation uses:

| Variable | Default | Description |
| --- | --- | --- |
| `LISTEN_ADDR` | `0.0.0.0:8787` | Bind address |
| `PORT` | `8787` | Alternate way to set the listen port |
| `ZCASH_NETWORK` | `testnet` | Active Zcash network: `testnet` or `mainnet` |
| `ZCASH_UIVK` | none | Unified Incoming Viewing Key used as the canonical wallet identity and to construct scanner keys |
| `ZCASH_BIRTHDAY_HEIGHT` | none | Birthday height required on first startup unless the wallet DB already has a canonical account |
| `APP_DATA_DIR` | `data/zcash-payment` | Base directory for default app DB, wallet DB, and logs |
| `APP_DB_PATH` | `APP_DATA_DIR/app.db` | Service-owned SQLite database |
| `WALLET_DB_PATH` | `APP_DATA_DIR/wallet.db` | Reserved path for the future `zcash_client_sqlite` database |
| `LOG_DIR` | `APP_DATA_DIR/logs` | Rolling log directory |
| `CATCH_UP_THRESHOLD_BLOCKS` | `1` | Threshold for entering catch-up mode once scanning is implemented |
| `LIGHTWALLETD_URL` | none | Required to scan compact blocks from lightwalletd; otherwise startup records a blocked sync state |
| `CATCH_UP_BATCH_SIZE` | `100` | Max compact-block range size per pipelined catch-up batch |
| `SYNC_POLL_INTERVAL_SECONDS` | `5` | Fallback idle poll interval for the long-lived sync loop when mempool-stream wakeups are unavailable |
| `ZCASH_WEBHOOK_URL` | none | Absolute callback URL used by the outbound delivery worker |
| `ZCASH_WEBHOOK_SECRET` | none | HMAC secret used to sign webhook bodies in the `X-Signature` header |
| `WEBHOOK_POLL_INTERVAL_SECONDS` | `2` | Idle poll interval for the outbound webhook worker |
| `WEBHOOK_RETRY_DELAY_SECONDS` | `30` | Base retry delay after transport errors, `429`, and `5xx` delivery responses |
| `WEBHOOK_RETRY_MAX_DELAY_SECONDS` | `300` | Max retry delay cap for exponential webhook backoff |
| `WEBHOOK_MAX_ATTEMPTS` | `8` | Total delivery attempts before a retryable webhook failure becomes permanent |
| `WEBHOOK_REPORT_CONFIRMATIONS` | `0` | Minimum confirmations before a receipt affects `total_received`; `0` also enables provisional mempool receipt reporting for up to 24 hours |
| `FINALITY_CONFIRMATIONS` | `100` | Finality boundary used for checkpointing and aggregation optimization |

### Startup integrity rules

- On first startup, the service requires `ZCASH_UIVK` unless the wallet DB
  already contains a canonical account.
- The service stores the canonical UIVK in its own app DB.
- The UIVK may include a transparent component; that is accepted, but the
  current implementation still issues shielded-only payment-session addresses
  and does not scan for transparent funds.
- If the wallet DB also contains a UIVK, it must match the app DB copy.
- If `ZCASH_UIVK` is provided on a later startup, it must match the persisted
  canonical UIVK or startup fails.
- `APP_DB_PATH` and `WALLET_DB_PATH` must be different files.

## HTTP API

All routes below are relative to `ZCASH_PAYMENT_SERVICE_BASE_URL`.

### `GET /health`

Health endpoint for Docker and operational checks.

#### `200 OK`

```json
{
  "ok": true,
  "current_sync_state": "synced",
  "detected_tip_height": 123456,
  "last_scanned_height": 123450
}
```

#### Response rules

- `ok` indicates the HTTP service is alive.
- `current_sync_state` reflects the latest persisted scanner state.
- `detected_tip_height` is the last chain tip height observed from lightwalletd, or `null` if no tip has been recorded yet.
- `last_scanned_height` is the last block height the service has fully scanned and checkpointed, or `null` if scanning has not advanced yet.

### `POST /payment-session`

Allocates a Zcash payment destination and returns QR-friendly text.

#### Request

```json
{
  "payment_source": "zcash",
  "label": "Pro annual plan",
  "memo": "Invoice INV-1001",
  "message": "Thanks for your purchase.",
  "amount": "0.12345678"
}
```

#### Request rules

- `payment_source` must currently be `zcash`.
- `label` is optional and informational.
- `memo` is optional and informational.
- `message` is optional and informational.
- `amount` is optional but expected in most checkout flows.
- The service always issues a fresh address for each payment session.

#### `200 OK`

```json
{
  "address": "zs1recipientaddress",
  "qr_text": "zcash:zs1recipientaddress?amount=0.12345678"
}
```

#### Response rules

- `address` must be the address the user should pay.
- `qr_text` must be a valid Zcash URI suitable for QR encoding.
- `qr_text` may include URL-encoded `amount`, `memo`, and `message` query
  params.
- Fresh address allocation derives a new unified address from the canonical
  UIVK and persists its diversifier metadata and shielded receiver fingerprints.

#### `400 Bad Request`

Returned if the request is structurally valid JSON but semantically invalid for
the service, for example an unsupported `payment_source`.

Example:

```json
{
  "error": "payment_source must be 'zcash'"
}
```

#### `500 Internal Server Error`

Returned if the microservice cannot allocate a payment session.

## Consumer webhook contract

The Zcash microservice contract described here is only the app-to-service
request path used for address allocation.

The consuming application separately exposes a public or internal webhook
endpoint, for example:

- `POST /webhooks/zcash-payments`

That endpoint is owned by the consuming application, not by the Zcash payment
microservice.

Webhook responsibilities remain with the consuming application:

- accept payment observations
- verify `X-Signature`
- apply idempotency using `event_id`
- reconcile invoices and subscriptions

## Zcash-specific state expectations

- The real service design now assumes a persistent app DB that tracks wallet
  identity, issued addresses, address totals, and webhook delivery state.
- The current implementation also records sync-state transitions in the app DB
  so the service can make scanner-key readiness explicit.
- The sync loop now records reorg events when an overlap rescan revokes or
  reactivates receipts after a tip change.
- The service crate is intentionally isolated so it can keep an independent
  SQLite/Zcash dependency graph without forcing that stack onto the consuming
  application.
- The contract does not currently require idempotent `POST /payment-session`
  semantics from the microservice.

## Reporting semantics

- `total_received` in upstream webhooks means the cumulative total of eligible
  incoming payments observed for one issued address.
- `total_received` is not wallet balance.
- The current named reporting threshold is 0 confirmations.
- At a threshold of 0, mempool receipts are provisional and may later be reversed by eviction or by aging past 24 hours without confirmation.
- The current named finality threshold is 100 confirmations.

## Local orchestration

The local Docker Compose setup should run:

- a consumer application
- `zcash-payment-service`

Recommended local wiring:

- the consumer application points `ZCASH_PAYMENT_SERVICE_BASE_URL` to
  `http://zcash-payment-service:8787`
- `zcash-payment-service` exposes `GET /health`
- the consumer application depends on the Zcash service becoming healthy

## Change policy

- This document is the canonical Zcash-specific supplement for the current
  payment-session integration.
- Backward-incompatible changes to Zcash request or response fields should be
  treated as versioned contract changes and reflected in both this document and
  the example service.
