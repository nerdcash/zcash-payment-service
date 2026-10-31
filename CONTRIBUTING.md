# Contributing

This guide is for human contributors and coding agents alike. It describes what lives in this
repository, how each part ships, and which changes must be mirrored elsewhere so the service,
its clients, and the docs stay in sync.

## Components

| Component | Location | Ships as | Released by |
| --- | --- | --- | --- |
| Payment service (Rust crate + binary) | `Cargo.toml`, `src/` | Rust crate, and the Docker image `ghcr.io/nerdcash/zcash-payment-service` | `.github/workflows/ci.yml`: every push to `main` that touches service code pushes `:main` and `:<commit sha>` tags |
| Node client | `clients/node/` | npm package `@nerdcash/zcash-payment-client` | `.github/workflows/publish-node-client.yml`: pushing a `client-node-v<version>` tag publishes to npm with trusted publishing and provenance |
| Guide (mdBook) | `book/` | GitHub Pages site | `.github/workflows/pages.yml`: every push to `main` |
| Reference docs | `README.md`, `docs/`, `design.md`, `src/spec.md`, `specs/` | Read in the repo | n/a |

Other workflows: `node-client.yml` tests the Node client on Node 20, 22, and 24 when `clients/node/**`
changes. `ghcr-cleanup.yml` prunes old container images.

`ci.yml` always runs on pull requests. On pushes to `main` it is skipped when only docs, Markdown,
or `clients/**` changed, so those merges don't publish a new image.

## Keeping client and server in sync

The service's wire contract is the source of truth. It is defined in `src/` and described in
`src/spec.md` and `book/src/api.md`. A change to any of the following **must** be reflected in every place listed:

| If you change… | Also update |
| --- | --- |
| `POST /payment-session` request or response fields | `clients/node/src/index.ts` (types and validation), `clients/node/test/`, `clients/node/README.md`, `book/src/api.md`, `src/spec.md` |
| Webhook payload fields, `payment_source` values, or field limits | `clients/node/src/index.ts` (`PaymentObservation`, `parseObservation`), client tests, `book/src/integration.md`, `book/src/api.md`, `src/spec.md` |
| Webhook signing (algorithm, header name, encoding, what bytes are signed) | `verifyWebhook` in the Node client and its tests, plus every doc above. This is a breaking change for every store, so call it out in the PR and bump the client's major version (or minor while it is 0.x) |
| Configuration or environment variables | `book/src/api.md` (Configuration), `book/src/deployment.md`, `docker-compose.yml` |
| Error responses (status codes, `{ "error": ... }` shape) | The client's `PaymentServiceError` mapping and its tests |

Prefer additive changes: new optional fields that old clients ignore. When a change is not
backward compatible, say so in the PR description and describe how deployed stores upgrade.

## Changing the Node client

- Any change under `clients/node/src/` needs a `version` bump in `clients/node/package.json`
  (semver), and the lockfile updated with `npm install`.
- Run `npm ci && npm run typecheck && npm test && npm run build` in `clients/node/`.
- The client must stay dependency-free at runtime (only Node built-ins).
- To release after merge: `git tag client-node-v<version> && git push origin client-node-v<version>`.
  The workflow fails if the tag does not match `package.json`.
- If the client starts relying on new service behavior, note the minimum service version in
  the client README.

## Changing the service

```sh
cargo fmt --all --check
cargo clippy --all-targets -- -D warnings
cargo test
cargo build --release
docker compose config   # validate the Compose file
```

Run the service locally with `cargo run` or `docker compose up --build`
(see [Getting started](https://nerdcash.github.io/zcash-payment-service/getting-started.html)).

The live testnet scanner-vector test is opt-in because it depends on an external lightwalletd server:

```sh
ZCASH_TESTNET_LIGHTWALLETD_URL=https://testnet.lightwalletd.com:9067 \
  cargo test scanner::tests::live_testnet_vectors_are_detected_with_expected_amounts -- --ignored --exact
```

In PowerShell, set `$env:ZCASH_TESTNET_LIGHTWALLETD_URL` first, then run the same `cargo test` command.

If the change touches the HTTP or webhook contract, follow the table above in the same PR.

## Changing the docs

- `book/` builds with mdBook. Check it locally with `mdbook build book` before pushing.
- Keep the guide and `src/spec.md` consistent. If they disagree, the code wins: fix the docs.
- The README stays a short pitch that links into the guide. Put details in `book/`.

## Pull requests

- Keep each PR focused. Do not rebase or force-push branches that others may have pulled; add commits instead.
- CI must pass. Review threads should be answered and resolved before merge.
