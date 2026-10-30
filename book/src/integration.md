# Integration guide

Your application owns the checkout. The service owns Zcash addresses and the observation of what arrived at them.

Your application:

- Prices the order and creates the invoice.
- Calls `POST /payment-session` when a payer is ready to pay.
- Shows the address and a QR code.
- Receives the webhook, checks `X-Signature`, and applies it when its `state_version` is newer than the last version stored for that address.
- Decides when `total_received` covers the invoice.

The service stores the requested amount, memo, message, and label beside the address. It does not compare the amount it observes with the amount you requested. `total_received` is the cumulative eligible incoming total for that one address, in ZEC. It is not a wallet balance.

The router does not authenticate callers. Keep `POST /payment-session` on a network only your application can reach.

## Creating a payment request

`POST /payment-session` with `Content-Type: application/json`.

`payment_source` is required and must be the string `zcash`. `label`, `memo`, `message`, and `amount` are optional strings. `amount` is copied into the QR text as you sent it. The service does not parse it as a number.

```powershell
curl.exe -s -X POST http://127.0.0.1:8787/payment-session `
  -H "Content-Type: application/json" `
  -d '{"payment_source":"zcash","label":"Pro annual plan","memo":"Invoice INV-1001","message":"Thanks for your purchase.","amount":"0.12345678"}'
```

A successful call returns `200` with JSON:

```json
{
  "address": "<unified address>",
  "qr_text": "zcash:<unified address>?amount=0.12345678&memo=Invoice%20INV-1001&message=Thanks%20for%20your%20purchase."
}
```

Every successful call mints a new unified address from the canonical UIVK. Calling twice returns two different addresses. The address string starts with `u`.

Save `address` on your invoice. That string is what the webhook sends back, and it is the only handle you have. There is no payment-status route to poll.

`payment_source` set to anything else returns `400` and this body:

```json
{"error":"invalid request: payment_source must be 'zcash'"}
```

Other request failures are described in the [API reference](api.md#http).

## Showing the address and the QR code

Show `address` as the pay-to string. Render `qr_text` as a QR image in your application. The service returns the text. It does not return a PNG.

`build_qr_text` builds that string:

- The prefix is `zcash:` plus the address.
- A non-blank `amount`, then `memo`, then `message`, are appended as a query string, in that order.
- A field that is missing or only whitespace is left out.
- Bytes other than letters, digits, and `- _ . ~` are percent-encoded with uppercase hex. A space becomes `%20`.

The unit test in `src/service.rs` locks the encoding:

```text
zcash:uaddr
zcash:uaddr?amount=0.12345678&memo=Invoice%20INV-1001&message=Thanks%20for%20your%20purchase.
```

The live `address` is the unified address from the response, in the same pattern. Wallets that understand a Zcash URI read the amount, memo, and message from those query parameters.

## Learning that a payment arrived

Set both of these on the service or it will scan without posting anything:

| Variable | Role |
| --- | --- |
| `ZCASH_WEBHOOK_URL` | Absolute `http` or `https` URL of your endpoint, including the host. |
| `ZCASH_WEBHOOK_SECRET` | Shared secret used as the HMAC key. |

If either one is empty, `spawn_webhook_delivery_worker` logs a warning and returns without starting the worker. Startup still succeeds.

When both are set, the worker `POST`s to your URL whenever an issued address is dirty and its eligible total differs from the last total that was successfully delivered. Catch-up scanning queues those deliveries as each batch updates a total, so a webhook can arrive before `current_sync_state` is `synced`. A later change, including a reorg or an expired mempool note, queues another delivery with a new `event_id`.

The body is compact JSON produced by `serde_json::to_string`. The signed bytes are that string, with the fields in this order. The numbers in this example only illustrate the shape. `42` and `v3` stand in for the address id and `state_version`. The timestamp is the whole-second RFC 3339 value used in the crate tests. A live `observed_at` is `OffsetDateTime::now_utc()` formatted as RFC 3339, so it can include a fractional second.

```json
{"event_id":"zcash:42:v3","payment_source":"zcash","address":"<unified address>","total_received":"0.12345678","observed_at":"2026-01-01T00:00:00Z"}
```

| Field | Meaning |
| --- | --- |
| `event_id` | `zcash:<address_id>:v<state_version>`. `address_id` is the integer id of the issued address. `state_version` increments by one when the eligible total changes. The column is unique, and the insert is `INSERT OR IGNORE`, so the same id is stored once. |
| `payment_source` | Always `zcash`. |
| `address` | The unified address from `POST /payment-session`. |
| `total_received` | Eligible total in ZEC. Zatoshis divided by `100_000_000`, always with 8 fractional digits (`1.00000000`, `0.00000001`). |
| `observed_at` | RFC 3339 UTC timestamp taken when the delivery row was queued. |

### What counts toward the total

`WEBHOOK_REPORT_CONFIRMATIONS` defaults to `0`.

- A mined note's confirmation depth is `tip - mined_height + 1` when the tip is at or above the mined height, and `0` otherwise. The note is eligible when that depth is at least the configured threshold. At the default of `0`, a mined note is eligible as soon as it is recorded.
- At threshold `0`, a mempool note is eligible while its `first_observed_at` is later than 24 hours before the check. After that it stops counting. If the transaction leaves the mempool, the service keeps the provisional receipt until it has scanned the next block. When that block contains the transaction, the receipt is promoted in place. When the scanned tip does not contain it, the receipt is revoked.
- `FINALITY_CONFIRMATIONS` defaults to `100`. It is the depth used for checkpointing. It does not gate `total_received`.

`total_received` can go up or down. A reorg or an expired mempool note lowers it and still gets a higher `state_version`.

Apply a webhook only when its `state_version` is greater than the last version you stored for that address. `claim_ready_webhook_delivery` claims the oldest row that is `queued`, or `retry_wait` whose `next_attempt_at` has arrived. A newer `queued` delivery can succeed while an older one is still waiting to retry, and that older `event_id` can arrive later. Return 2xx for the stale event and leave the invoice unchanged, so the service stops retrying it. The same `event_id` is also safe to acknowledge again.

### Checking the signature

The worker sets:

- `Content-Type: application/json`
- `X-Signature`: lowercase hex HMAC-SHA256 of the raw body

The HMAC key is the UTF-8 bytes of `ZCASH_WEBHOOK_SECRET`. The message is the raw body bytes, before you parse JSON. Compare the hex digest to `X-Signature` in constant time.

This is the same construction as `sign_body` in `src/webhook_delivery.rs`:

```rust,ignore
fn sign_body(secret: &str, body: &[u8]) -> String {
    let mut mac = hmac::Hmac::<sha2::Sha256>::new_from_slice(secret.as_bytes()).unwrap();
    mac.update(body);
    hex::encode(mac.finalize().into_bytes())
}
```

### Retries

The worker polls for a queued delivery every `WEBHOOK_POLL_INTERVAL_SECONDS` (default `2`) when none is ready.

| Response | What the worker does |
| --- | --- |
| HTTP 2xx | Marks the delivery `succeeded` and records the notified total. |
| HTTP 429 or 5xx, or a transport error | Schedules `retry_wait`, unless this attempt reaches `WEBHOOK_MAX_ATTEMPTS`. |
| Any other HTTP status | Marks `permanent_failure` on that attempt. |

`attempt_count` starts at `0`. The attempt being made is `attempt_count + 1`. When that number is greater than or equal to `WEBHOOK_MAX_ATTEMPTS` (default `8`), a retryable failure becomes permanent.

The delay after a retryable failure is `WEBHOOK_RETRY_DELAY_SECONDS` (default `30`) multiplied by `2` raised to `attempt_count`, then capped at `WEBHOOK_RETRY_MAX_DELAY_SECONDS` (default `300`). With the defaults, the first failure waits 30 seconds, then 60, 120, and 240. The next waits are capped at 300 seconds.

`run()` also repairs a restart. Deliveries left in `sending` move back to `retry_wait` with `next_attempt_at` set to now, and addresses still marked `dirty` are queued again. That recovery function is private. The binary calls it. An embedder who builds the router by hand and does not call `run()` skips it. See [Deployment](deployment.md#rust-crate).

### A minimal handler

1. Read the raw body.
2. Recompute the HMAC and compare it to `X-Signature`.
3. Parse the JSON.
4. Read `state_version` from `event_id` (`zcash:<address_id>:v<state_version>`).
5. If this `event_id` is already applied, or its `state_version` is less than or equal to the last version stored for `address`, return 2xx and leave the invoice unchanged.
6. Otherwise store `total_received` as the cumulative total for that address and update the invoice. Compare the version, because a newer total can be smaller.
7. Return a 2xx status. Any other success-range status counts. `4xx` other than `429` will not be retried.
