# Getting started

You need three values before the service can scan:

| Variable | Role |
| --- | --- |
| `ZCASH_UIVK` | Unified incoming viewing key for the network you select. It must include Orchard and Sapling receivers. |
| `ZCASH_BIRTHDAY_HEIGHT` | Block height to treat as the wallet birthday. A `u32`. |
| `LIGHTWALLETD_URL` | Lightwalletd endpoint the service calls for compact blocks, tree state, and mempool data. |

`ZCASH_NETWORK` defaults to `testnet`. Set it to `mainnet` when the UIVK is a mainnet key. The two must match: the service decodes the UIVK with the network you name.

Create a `.env` file next to `docker-compose.yml` if you use Compose. Compose substitutes `${ZCASH_UIVK}` and the other variables in that file from the shell or from `.env`. Export the same variables in the shell when you use `cargo run`.

```powershell
$env:ZCASH_NETWORK = "testnet"
$env:ZCASH_UIVK = "<your unified incoming viewing key>"
$env:ZCASH_BIRTHDAY_HEIGHT = "<birthday height>"
$env:LIGHTWALLETD_URL = "<lightwalletd url>"
```

```bash
export ZCASH_NETWORK=testnet
export ZCASH_UIVK='<your unified incoming viewing key>'
export ZCASH_BIRTHDAY_HEIGHT='<birthday height>'
export LIGHTWALLETD_URL='<lightwalletd url>'
```

On the first start, the service refuses to come up when `ZCASH_UIVK` or `ZCASH_BIRTHDAY_HEIGHT` is missing, unless the wallet database already contains a canonical account it can read. After the app database has stored an identity, a later start can omit those two variables and still pass the identity check. The scan loop is separate: each iteration builds scanning keys from the `ZCASH_UIVK` environment variable. Leave it unset and `GET /health` reports `current_sync_state` as `blocked_scanning_key_material`. Leave `LIGHTWALLETD_URL` unset while the UIVK is set, and the state is `blocked_lightwalletd_endpoint`.

Optional webhook settings are covered in the [integration guide](integration.md#learning-that-a-payment-arrived). The full variable list is in the [API reference](api.md#configuration).

## Run from source

CI builds with stable Rust. From the repository root:

```powershell
cargo run
```

The binary listens on `0.0.0.0:8787` unless you set `LISTEN_ADDR` or `PORT`. `LISTEN_ADDR` wins when both are set. `PORT` alone becomes `0.0.0.0:<port>`.

Data and logs default to `data/zcash-payment/` relative to the working directory (`app.db`, `wallet.db`, and `logs/`). That directory is gitignored.

## Run with Docker Compose

From the repository root:

```powershell
docker compose up --build
```

The Compose file builds the image, publishes `127.0.0.1:8787` to container port `8787`, sets `APP_DATA_DIR` to `/data/zcash-payment`, and mounts `./data` there. The health check calls `GET /health` with `curl` inside the container.

## Check that it is up

```powershell
curl.exe -s http://127.0.0.1:8787/health
```

Before the sync worker records a status, the metadata row inserted at startup looks like this:

```json
{
  "ok": true,
  "current_sync_state": "starting",
  "detected_tip_height": null,
  "last_scanned_height": null
}
```

`ok` means the HTTP handler could read that row. Heights are JSON `null` while the stored integers are still `0`. Read `current_sync_state` to see whether scanning is running. The sync worker then records one of `catching_up`, `synced`, `blocked_scanning_key_material`, or `blocked_lightwalletd_endpoint`. The two blocked states also leave both heights at `0`.

`cargo run` and the container process load configuration only from the environment. A missing or mismatched UIVK fails startup with `startup integrity check failed: ...` and the process exits before it listens. The [API reference](api.md#startup-rules) lists those checks.

Logs go to standard output and to a daily JSON file, `zcash-payment.log`, under `LOG_DIR`. The default filter is `info`. Set `RUST_LOG` to change it.

The binary stops when it receives Ctrl-C.
