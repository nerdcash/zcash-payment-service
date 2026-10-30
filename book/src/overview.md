# Overview

`zcash-payment-service` hands your application a fresh Zcash address for each checkout and posts the amount it later observes at that address.

You run it on your own host. Use it as a small HTTP service, or embed the Rust crate in a process you already have. The service adds no fee: the `total_received` value it reports is the sum of the eligible incoming note values, converted from zatoshis, with nothing withheld. There is no account to open with a payment processor, and the code has no usage meter.

The only wallet key the server accepts is a unified incoming viewing key, the UIVK in `ZCASH_UIVK`. From that key it derives each payment address and builds the Sapling, Orchard, and Ironwood scanning keys it uses locally. You keep the spending key in the wallet that can spend. The service stores the UIVK in its own SQLite database on the host you run.

Two ways to run the same code:

- **Rust crate.** Depend on `zcash-payment-service` from this Git repository and call [`run`](api.md#crate-entry-points), or mount the router yourself.
- **Docker container.** Build the image in this repository, or pull `ghcr.io/nerdcash/zcash-payment-service` after CI publishes it from `main`.

## What a payment looks like

1. Your application asks `POST /payment-session` for an address when a checkout starts.
2. You show the returned `address` and a QR code of `qr_text` to the payer.
3. The service downloads compact blocks and mempool transactions from the lightwalletd URL you configure, and decrypts them on your host with keys derived from the UIVK.
4. When `ZCASH_WEBHOOK_URL` and `ZCASH_WEBHOOK_SECRET` are both set, a change in the eligible total for that address becomes a signed webhook to your application.

The HTTP API is two routes, `GET /health` and `POST /payment-session`. Payment totals leave the process as webhooks. Your application owns prices, invoices, and the decision that a payment is enough.

## What the service keeps track of

The current code does all of the following:

- Stores its own state in SQLite, separate from the wallet database.
- Checks on startup that the configured UIVK, network, and birthday height match the identity it already stored.
- Derives a new unified address from the canonical UIVK for every payment session. Each address has Orchard and Sapling receivers. An Ironwood receiver is recorded too, using the same receiver bytes as Orchard, so Ironwood notes can be attributed after NU6.3.
- Scans compact blocks, and the mempool when `WEBHOOK_REPORT_CONFIRMATIONS` is `0`.
- Rescans a recent window so a short reorg can correct a total, and advances receipt maturity as the chain tip moves.
- Signs outbound webhooks with HMAC-SHA256 and retries them with a capped exponential backoff.
- Writes a daily JSON log file plus a compact log on standard output.

A UIVK that also carries a transparent component still decodes. Issued addresses omit a transparent receiver, and the scanner builds Sapling, Orchard, and Ironwood keys only.

The router has no authentication middleware. Put the service on a private network, the way `docker-compose.yml` publishes it on `127.0.0.1`.

Continue with [Getting started](getting-started.md) to run it, or [Integration guide](integration.md) to wire a checkout.
