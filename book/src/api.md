# API reference

This page is the contract implemented in this repository. Request and response fields are the serde names on the Rust structs. Nothing here adds a route, a field, or a header that the code does not set.

The crate depends on axum 0.8.9. Where a failure comes from axum's `Json` extractor rather than this crate's `AppError`, the status below is the one axum's own tests assert for that version.

## HTTP

`build_router` registers two routes and no middleware:

| Method | Path | Handler |
| --- | --- | --- |
| `GET` | `/health` | `service::health` |
| `POST` | `/payment-session` | `service::payment_session` |

Handlers that return `AppError` respond with JSON:

```json
{"error":"<Display of the AppError>"}
```

| Variant | Status | Display prefix |
| --- | --- | --- |
| `InvalidConfig` | 400 | `invalid configuration: ` |
| `InvalidRequest` | 400 | `invalid request: ` |
| `StartupIntegrity` | 400 | `startup integrity check failed: ` |
| `Wallet` | 500 | `wallet error: ` |
| `Database` | 500 | `database error: ` |
| `Io` | 500 | `io error: ` |
| `Json` | 500 | `json error: ` |
| `Http` | 500 | `http error: ` |
| `TimeFormat` | 500 | `time formatting error: ` |
| `TimeParse` | 500 | `time parsing error: ` |

`GET /health` returns `Database` when the metadata query fails. `POST /payment-session` returns `InvalidRequest` for a bad `payment_source`, and `Wallet` or `Database` when minting an address fails. Identity failures are returned by `PaymentService::initialize` and `run()` before the socket listens. They use the same `AppError` display text, as a process error.

### `GET /health`

Returns `200` and `HealthResponse`:

| Field | JSON type | Source |
| --- | --- | --- |
| `ok` | boolean | Always `true` when the handler succeeds. |
| `current_sync_state` | string | `service_metadata.current_sync_state`. |
| `detected_tip_height` | number or `null` | `last_seen_tip_height` when it is greater than 0, otherwise `null`. |
| `last_scanned_height` | number or `null` | `last_scanned_height` when it is greater than 0, otherwise `null`. |

The row inserted at first startup has `current_sync_state` `"starting"` and both heights `0`, so both JSON heights are `null`.

The sync worker later writes one of these states:

| State | When |
| --- | --- |
| `catching_up` | A scan toward the sampled tip is in progress. |
| `synced` | The last scanned height is at or past the sampled tip. |
| `blocked_scanning_key_material` | `ZCASH_UIVK` is unset, so scanning keys cannot be built. |
| `blocked_lightwalletd_endpoint` | The UIVK is set and `LIGHTWALLETD_URL` is unset. |

`record_sync_status` stores whatever string it is given. Those four, plus the initial `starting`, are the ones the running service writes.

The unit test `health_response_reports_persisted_sync_heights` records state `catching_up` with tip `321` and scanned height `300`, and the handler returns:

```json
{
  "ok": true,
  "current_sync_state": "catching_up",
  "detected_tip_height": 321,
  "last_scanned_height": 300
}
```

### `POST /payment-session`

Body JSON, content type `application/json` (a subtype with a `+json` suffix is also accepted by axum). `PaymentSessionRequest`:

| Field | Rust type | Required | Role |
| --- | --- | --- | --- |
| `payment_source` | `String` | yes | Must equal `zcash`. |
| `label` | `Option<String>` | no | Stored on the issued address. |
| `memo` | `Option<String>` | no | Stored, and copied into `qr_text` when non-blank. |
| `message` | `Option<String>` | no | Stored, and copied into `qr_text` when non-blank. |
| `amount` | `Option<String>` | no | Stored as text, and copied into `qr_text` when non-blank. It is not parsed. |

Unknown JSON fields are ignored. A missing `payment_source`, or a value of the wrong JSON type, never reaches the handler.

| Failure | Status | Body |
| --- | --- | --- |
| Content-Type is not JSON | 415 | axum's plain-text rejection |
| JSON syntax is invalid | 400 | plain text starting with `Failed to parse the request body as JSON` |
| JSON does not match the struct | 422 | plain text starting with `Failed to deserialize the JSON body into the target type` |
| `payment_source` is not `zcash` | 400 | `{"error":"invalid request: payment_source must be 'zcash'"}` |
| Address derivation or the database fails | 500 | `{"error":"wallet error: ..."}` or `{"error":"database error: ..."}` |

Success is `200` and `PaymentSessionResponse`:

| Field | Meaning |
| --- | --- |
| `address` | Newly derived unified address. Each successful call returns a different one. The test `fresh_addresses_are_real_unified_addresses` expects the string to start with `u`. |
| `qr_text` | `zcash:<address>` plus an optional query. |

Query construction in `build_qr_text`, covered by tests in `src/service.rs`:

```text
zcash:uaddr
zcash:uaddr?amount=0.12345678&memo=Invoice%20INV-1001&message=Thanks%20for%20your%20purchase.
```

Order is `amount`, then `memo`, then `message`. A missing or whitespace-only field is omitted. The value that is encoded is the original string, not a trimmed copy. Unreserved characters stay as themselves (`A-Z a-z 0-9 - _ . ~`). Every other byte becomes `%` plus two uppercase hex digits.

The address is derived from the canonical UIVK with Orchard required, Sapling required, and the transparent receiver omitted. The app database also stores an Ironwood receiver whose bytes equal the Orchard receiver.

## Webhook

This is a request the service sends, not a route it serves. `spawn_webhook_delivery_worker` starts the loop only when both `webhook_url` and `webhook_secret` are set.

- Method: `POST`
- URL: `ZCASH_WEBHOOK_URL`
- Header `Content-Type`: `application/json`
- Header `X-Signature`: lowercase hex HMAC-SHA256
- Body: the exact `request_body_json` stored when the delivery was queued

The HMAC key is `ZCASH_WEBHOOK_SECRET` as UTF-8 bytes. The message is the raw body. `sign_body` uses `hmac` with `sha2::Sha256` and encodes the digest with `hex::encode`.

The stored JSON is `serde_json::to_string` of these fields, in this order:

| Field | Value |
| --- | --- |
| `event_id` | `zcash:<address_id>:v<state_version>` |
| `payment_source` | `zcash` |
| `address` | `issued_addresses.unified_address` |
| `total_received` | Eligible zatoshis rendered as ZEC |
| `observed_at` | RFC 3339 UTC time when the row was queued |

`format_zat_as_zec` divides the absolute zatoshis by `100_000_000` and prints the remainder in 8 digits. `12345678` zatoshis become `0.12345678`. `100000000` become `1.00000000`. A negative `i64` gets a leading `-`.

`state_version` increments by one when `current_total_zat` changes. `current_total_zat` is the sum of `value_zat` over receipts for that address that are `active` and `eligible_for_webhook`. A delivery is inserted when `dirty_state` is `dirty` and the current total differs from `last_notified_total_zat`. `event_id` is unique, and the insert is `INSERT OR IGNORE`.

`claim_ready_webhook_delivery` takes the oldest row that is `queued`, or `retry_wait` with `next_attempt_at` already due, ordered by `observed_at` then `delivery_id`. A newer queued delivery can be sent while an older one is still in `retry_wait`. Receivers should keep the highest `state_version` they have applied for an address. A later delivery of a lower version is an older total, including after a reorg where the newer total is smaller.

Eligibility:

- Mined receipt: confirmation depth is `tip_height - mined_height + 1` when `tip_height >= mined_height`, else `0`. Eligible when the depth is `>= webhook_report_confirmations`.
- Mempool receipt: eligible when `webhook_report_confirmations == 0` and `first_observed_at` is strictly later than 24 hours before the check. The sync loop reconciles mempool receipts only in that zero-confirmation mode.

Delivery outcomes are in the [integration guide](integration.md#retries). On startup, `run()` moves rows left in `sending` back to `retry_wait` with `next_attempt_at` set to the current time, then queues addresses whose `dirty_state` is still `dirty`.

## Configuration

`Config::from_env` reads the process environment. Strings are trimmed. An empty string is treated as unset for `ZCASH_UIVK`, `LIGHTWALLETD_URL`, `ZCASH_WEBHOOK_URL`, and `ZCASH_WEBHOOK_SECRET`.

| Variable | Default | Accepted values |
| --- | --- | --- |
| `LISTEN_ADDR` | `0.0.0.0:8787` when `PORT` is also unset | Used as the bind address when set. Wins over `PORT`. |
| `PORT` | `8787` | Used only when `LISTEN_ADDR` is unset. Bind address becomes `0.0.0.0:<port>`. |
| `ZCASH_NETWORK` | `testnet` | `testnet` or `mainnet`. |
| `ZCASH_UIVK` | unset | UIVK text. See [startup rules](#startup-rules). |
| `ZCASH_BIRTHDAY_HEIGHT` | unset | Must parse as `u32`. |
| `LIGHTWALLETD_URL` | unset | Passed to the lightwalletd client. Required for scanning. |
| `APP_DATA_DIR` | `data/zcash-payment` | Base directory for the default paths below. |
| `APP_DB_PATH` | `<APP_DATA_DIR>/app.db` | Must differ from `WALLET_DB_PATH`. |
| `WALLET_DB_PATH` | `<APP_DATA_DIR>/wallet.db` | `zcash_client_sqlite` database. |
| `LOG_DIR` | `<APP_DATA_DIR>/logs` | Daily JSON file `zcash-payment.log`. |
| `CATCH_UP_THRESHOLD_BLOCKS` | `1` | Parses as `u32`, including `0`. |
| `CATCH_UP_BATCH_SIZE` | `100` | `u32`, must be greater than 0. |
| `SYNC_POLL_INTERVAL_SECONDS` | `5` | `u64`, must be greater than 0. Fallback delay when the mempool stream is unavailable. |
| `ZCASH_WEBHOOK_URL` | unset | Absolute `http` or `https` URL with a host. |
| `ZCASH_WEBHOOK_SECRET` | unset | HMAC key. Delivery runs only when this and the URL are both set. |
| `WEBHOOK_POLL_INTERVAL_SECONDS` | `2` | `u64`, greater than 0. |
| `WEBHOOK_RETRY_DELAY_SECONDS` | `30` | `u64`, greater than 0. Base of the exponential backoff. |
| `WEBHOOK_RETRY_MAX_DELAY_SECONDS` | `300` | `u64`, greater than 0. Cap on the backoff. |
| `WEBHOOK_MAX_ATTEMPTS` | `8` | `u32`, greater than 0. |
| `WEBHOOK_REPORT_CONFIRMATIONS` | `0` | `u32`. `0` includes provisional mempool receipts. |
| `FINALITY_CONFIRMATIONS` | `100` | `u32`. Stored and used for checkpointing. Does not gate `total_received`. |
| `RUST_LOG` | `info` | `tracing-subscriber` `EnvFilter` directive. |

A value that fails its parse or range check returns `InvalidConfig` and the process does not listen. The error strings include:

- `ZCASH_NETWORK must be 'testnet' or 'mainnet'`
- `ZCASH_BIRTHDAY_HEIGHT must be a positive integer` (the parse failed; a numeric `u32`, including `0`, is accepted)
- `CATCH_UP_THRESHOLD_BLOCKS must be a non-negative integer`
- `CATCH_UP_BATCH_SIZE must be a positive integer` or `CATCH_UP_BATCH_SIZE must be greater than zero`
- the same pair of messages for `SYNC_POLL_INTERVAL_SECONDS`, `WEBHOOK_POLL_INTERVAL_SECONDS`, `WEBHOOK_RETRY_DELAY_SECONDS`, `WEBHOOK_RETRY_MAX_DELAY_SECONDS`, and `WEBHOOK_MAX_ATTEMPTS`
- `WEBHOOK_REPORT_CONFIRMATIONS must be a non-negative integer`
- `FINALITY_CONFIRMATIONS must be a non-negative integer`
- `APP_DB_PATH and WALLET_DB_PATH must point to different files`
- `ZCASH_WEBHOOK_URL must be a valid absolute http(s) URL; got ...`
- `ZCASH_WEBHOOK_URL must be an absolute http(s) URL; got ...`
- `ZCASH_WEBHOOK_URL must use http or https; got ...`
- `ZCASH_WEBHOOK_URL must include a host; got ...`

Setting the URL without the secret, or the secret without the URL, is allowed. The webhook worker stays off.

Named defaults live in `src/constants.rs`:

| Constant | Value |
| --- | --- |
| `APP_SCHEMA_VERSION` | `2` |
| `WEBHOOK_REPORT_CONFIRMATIONS` | `0` |
| `FINALITY_CONFIRMATIONS` | `100` |
| `DEFAULT_CATCH_UP_THRESHOLD_BLOCKS` | `1` |
| `DEFAULT_CATCH_UP_BATCH_SIZE` | `100` |
| `DEFAULT_SYNC_POLL_INTERVAL_SECONDS` | `5` |
| `DEFAULT_WEBHOOK_POLL_INTERVAL_SECONDS` | `2` |
| `DEFAULT_WEBHOOK_RETRY_DELAY_SECONDS` | `30` |
| `DEFAULT_WEBHOOK_RETRY_MAX_DELAY_SECONDS` | `300` |
| `DEFAULT_WEBHOOK_MAX_ATTEMPTS` | `8` |

`WEBHOOK_REPORT_CONFIRMATIONS` and `FINALITY_CONFIRMATIONS` in that table are the defaults. The environment variables of the same names override them for a new database. Once `service_metadata` exists, a later start whose values differ from the stored row fails. See the next section.

## Startup rules

`PaymentService::initialize` and each sync iteration call `ensure_wallet_identity`.

On a database with no wallet identity yet:

- The UIVK is `ZCASH_UIVK`, or, if that is unset, the UIVK read from the wallet database `accounts` table. If neither exists, startup fails with `first startup requires ZCASH_UIVK unless the wallet DB already contains a canonical account`.
- The birthday is `ZCASH_BIRTHDAY_HEIGHT`, or the wallet database birthday. If neither exists, startup fails with `first startup requires ZCASH_BIRTHDAY_HEIGHT unless the wallet DB already contains a canonical account`.
- When both the environment and the wallet database supply a UIVK, they must be equal (`provided ZCASH_UIVK does not match the existing wallet DB identity`).
- The UIVK is stored as `canonical_uivk`.

On a later start:

- A provided `ZCASH_UIVK` must equal `canonical_uivk`.
- `ZCASH_NETWORK` must equal the stored network.
- A provided `ZCASH_BIRTHDAY_HEIGHT` must equal the stored birthday. Omitting it is allowed.
- A UIVK and birthday read from the wallet database must match the stored identity.

`WalletView::decode` then requires the canonical UIVK to decode for that network and to contain Orchard and Sapling (`configured UIVK must contain Orchard and Sapling receivers`). If the wallet database UIVK does not match the decoded canonical form, startup fails with `wallet DB UIVK does not match the decoded canonical wallet view`.

`ensure_service_metadata` inserts the confirmation settings on first start. A later start fails with `stored service metadata confirmation settings do not match the configured constants` when `WEBHOOK_REPORT_CONFIRMATIONS` or `FINALITY_CONFIRMATIONS` differs from that row.

The scan loop builds keys from the environment variable `ZCASH_UIVK` on every iteration, not from the stored identity alone. Lightwalletd calls in `src/scanner.rs` request blocks by height, the latest block, tree state, the mempool stream, mempool transaction ids, and raw transactions by id. Those requests do not send the UIVK. Decryption uses scanning keys built locally from that UIVK.

The service keeps one shared connection to lightwalletd. It sends HTTP/2 and TCP keepalives on it, bounds connect and unary requests with timeouts, and polls the chain tip every 15 seconds while it waits on the mempool stream. A connection that drops silently is replaced within about half a minute instead of stalling block and mempool scanning until the operating system gives up on it.

## Crate entry points

`src/lib.rs` exports these modules: `config`, `constants`, `db`, `error`, `logging`, `receipt_ingest`, `scanner`, `service`, `wallet_db`, `wallet_runtime`, `webhook_delivery`, `zcash`.

The binary and an embedded host call the items below. Signatures match the source.

```rust,ignore
pub struct AppState {
    pub service: Arc<PaymentService>,
}

pub fn build_router(state: AppState) -> Router;
pub async fn run() -> Result<(), AppError>;
```

```rust,ignore
impl Config {
    pub fn from_env() -> Result<Self, AppError>;
}

impl PaymentService {
    pub fn initialize(config: Config) -> Result<Self, AppError>;
    pub fn issue_payment_session(
        &self,
        request: PaymentSessionRequest,
    ) -> Result<PaymentSessionResponse, AppError>;
    pub fn health_response(&self) -> Result<HealthResponse, AppError>;
}

pub fn build_qr_text(
    address: &str,
    amount: Option<&str>,
    memo: Option<&str>,
    message: Option<&str>,
) -> String;

pub fn spawn_wallet_sync_worker(config: Config);
pub fn spawn_webhook_delivery_worker(config: Config);
pub fn init_logging(config: &Config) -> Result<WorkerGuard, AppError>;
```

`run` is the path `main` uses. It calls `Config::from_env`, `init_logging`, `PaymentService::initialize`, the private webhook recovery, both `spawn_*` workers, `build_router`, and `axum::serve` with shutdown on Ctrl-C.

`PaymentService`'s fields are private. `Config`'s fields are public and match the environment table: `listen_addr`, `network`, `startup_uivk`, `lightwalletd_url`, `birthday_height`, `wallet_db_path`, `app_db_path`, `log_dir`, `catch_up_threshold_blocks`, `catch_up_batch_size`, `sync_poll_interval_seconds`, `webhook_url`, `webhook_secret`, `webhook_poll_interval_seconds`, `webhook_retry_delay_seconds`, `webhook_retry_max_delay_seconds`, `webhook_max_attempts`, `webhook_report_confirmations`, `finality_confirmations`.

`db`, `scanner`, `receipt_ingest`, `wallet_db`, and `zcash` are public as well. They are the storage and scan implementation behind the two HTTP routes. Their types and function signatures are the `pub` items in those files. This guide does not add behavior on top of them.
