# Deployment

The same crate ships as a library and as a container. CI on `main` compiles the binary, builds a `linux/amd64` and `linux/arm64` image, and pushes it to GHCR. Pull requests build the image and do not push it.

## Rust crate

`Cargo.toml` names the package `zcash-payment-service` at version `0.1.0`. The repository does not publish that version to crates.io. Depend on it from Git:

```toml
[dependencies]
zcash-payment-service = { git = "https://github.com/nerdcash/zcash-payment-service.git" }
```

### Run the binary

`src/main.rs` is:

```rust,ignore
#[tokio::main]
async fn main() -> Result<(), zcash_payment_service::error::AppError> {
    zcash_payment_service::run().await
}
```

`cargo run` and `cargo build --release` produce that binary. The release path is `target/release/zcash-payment-service` (with `.exe` on Windows). The Docker image copies the same binary to `/usr/local/bin/zcash-payment-service`.

`run()` loads `Config` from the environment, starts logging, opens the databases, checks the wallet identity, recovers interrupted webhook rows, spawns the scan worker and the webhook worker, and serves the router on `listen_addr` until Ctrl-C.

### Embed the router

You can build the same router inside your own Tokio process. This uses the public functions and skips the private startup recovery that `run()` performs. Call `init_logging` yourself if you want the daily JSON log file.

```rust,ignore
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

`PaymentService::initialize` creates the wallet database schema, stores or checks the canonical UIVK, and decodes that UIVK into the view used to mint addresses. Both workers take `Config` by value and spawn a Tokio task. The webhook task is skipped when `ZCASH_WEBHOOK_URL` or `ZCASH_WEBHOOK_SECRET` is unset.

Keep `APP_DATA_DIR`, or the two explicit database paths, on durable disk. `APP_DB_PATH` and `WALLET_DB_PATH` must be different paths. The app database holds the UIVK, issued addresses, receipt totals, and webhook state. The wallet database is the separate `zcash_client_sqlite` file.

## Docker container

The `Dockerfile` is a multi-stage build.

- The builder is the pinned `rust:1.99-bookworm` image. It compiles `cargo build --release` for `x86_64-unknown-linux-gnu` or `aarch64-unknown-linux-gnu`, matching the target architecture.
- The runtime image is the pinned `debian:bookworm-slim` image with `ca-certificates` and `curl`.
- The image exposes port `8787`, sets `PORT=8787`, and starts `zcash-payment-service`.

`PORT=8787` makes the process bind `0.0.0.0:8787` inside the container. `EXPOSE` does not publish the port. Nothing in the image sets `APP_DATA_DIR`, so a container without a volume writes `app.db`, `wallet.db`, and logs under `/app/data/zcash-payment`, and that data disappears with the container.

### Compose in this repository

`docker compose up --build` uses `docker-compose.yml`:

- Image name `zcash-payment-service:${IMAGE_TAG:-local}`.
- Host port `127.0.0.1:8787` forwarded to container port `8787`.
- `APP_DATA_DIR=/data/zcash-payment` and a bind mount of `./data` onto that path.
- `ZCASH_NETWORK` defaults to `testnet` when the variable is empty.
- `ZCASH_UIVK`, `ZCASH_BIRTHDAY_HEIGHT`, `LIGHTWALLETD_URL`, `ZCASH_WEBHOOK_URL`, and `ZCASH_WEBHOOK_SECRET` are passed through from the shell or from a `.env` file.
- Restart policy `unless-stopped`.
- Health check: `curl -sf http://localhost:8787/health` every 30 seconds.

### Published image

The CI workflow pushes, on a `main` push or a manual run of that workflow on `main`:

- `ghcr.io/nerdcash/zcash-payment-service:<git sha>`
- `ghcr.io/nerdcash/zcash-payment-service:main`

The image name is `ghcr.io/` plus the lowercased repository owner plus `/zcash-payment-service`. The cleanup workflow keeps the `main` tag and the five newest SHA tags.

```powershell
docker pull ghcr.io/nerdcash/zcash-payment-service:main
```

Run the published image with the same settings the Compose file uses, and point `ZCASH_WEBHOOK_URL` at an endpoint your application serves:

```yaml
services:
  zcash-payment-service:
    image: ghcr.io/nerdcash/zcash-payment-service:main
    environment:
      PORT: "8787"
      APP_DATA_DIR: /data/zcash-payment
      ZCASH_NETWORK: mainnet
      ZCASH_UIVK: ${ZCASH_UIVK}
      ZCASH_BIRTHDAY_HEIGHT: ${ZCASH_BIRTHDAY_HEIGHT}
      LIGHTWALLETD_URL: ${LIGHTWALLETD_URL}
      ZCASH_WEBHOOK_URL: http://consumer-app:8080/webhooks/zcash-payments
      ZCASH_WEBHOOK_SECRET: ${ZCASH_WEBHOOK_SECRET}
    volumes:
      - ./data/zcash-payment:/data/zcash-payment
    healthcheck:
      test: ["CMD", "curl", "-sf", "http://localhost:8787/health"]
      interval: 30s
      timeout: 5s
      retries: 3
      start_period: 5s
```

`src/spec.md` describes the calling application as the owner of `ZCASH_PAYMENT_SERVICE_BASE_URL`. This binary does not read that variable. From another container on the same Compose network the base URL is `http://zcash-payment-service:8787`.

Give the container the UIVK, the birthday height, and a lightwalletd URL, and keep the volume. The process has no request authentication, so publish the port on an address your application can reach and the public internet cannot.

## Scan parallelism

Catch-up scanning trial-decrypts every shielded output in each compact block. The service spreads blocks across a dedicated thread pool and downloads several block ranges ahead of the scanner. Receipts are still recorded in height order.

`SCAN_PARALLELISM` sets the size of that pool. The default is the number of CPUs available to the process, which gives the fastest catch-up. The pool is separate from the Tokio runtime and from rayon's global pool, so the cap holds even when you embed the router in another application.

Set a lower value when the same host also serves web pages or other latency-sensitive traffic. A long catch-up, such as a first sync from an old birthday height, can otherwise keep every core busy for minutes and slow page responses. For example, on a 4-core host that also runs a storefront, `SCAN_PARALLELISM=2` leaves room for the web server. `SCAN_PARALLELISM=1` scans one block at a time.

Each catch-up progress log line reports `blocks_per_second`, `scan_seconds`, `download_wait_seconds`, and `scan_parallelism`. If `download_wait_seconds` dominates, more threads will not help; the lightwalletd server is the bottleneck.

### Mainnet spam filter

On mainnet, between heights 1,710,000 and 2,050,000, the scanner skips any transaction with more than 50 shielded outputs. That range covers the 2022 output-spam attack, and skipping those transactions greatly speeds a scan through it. The filter is built in and has no setting. A real payment in that window would need more than 50 outputs in one transaction to be missed. Testnet is never filtered.
