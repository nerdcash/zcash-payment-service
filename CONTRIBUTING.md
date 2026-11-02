# Contributing

This guide is for human contributors and coding agents alike. It describes what lives in this
repository, how each part ships, and which changes must be mirrored elsewhere so the service,
its clients, and the docs stay in sync.

## Components

| Component | Location | Ships as | Released by |
| --- | --- | --- | --- |
| Payment service (Rust crate + binary) | `Cargo.toml`, `src/` | Rust crate, and the Docker image `ghcr.io/nerdcash/zcash-payment-service` | `.github/workflows/ci.yml`: every push to `main` that touches service code pushes `:main` and `:<commit sha>` tags. `.github/workflows/release.yml`: pushing a `v<version>` tag that matches `Cargo.toml` pushes `:v<version>` and `:<version>`, and for a stable version also `:<major>.<minor>` (if it is the newest patch in that series) and `:latest` (if it is the newest stable release) |
| Node client | `clients/node/` | npm package `@nerdcash/zcash-payment-client` | `.github/workflows/publish-node-client.yml`: pushing a `client-node-v<version>` tag publishes to npm with trusted publishing and provenance |
| Medusa plugin | `plugins/medusa/` | npm package `@nerdcash/medusa-payment-zcash` | `.github/workflows/publish-medusa-plugin.yml`: pushing a `medusa-v<version>` tag publishes to npm with trusted publishing and provenance |
| Vendure plugin | `plugins/vendure/` | npm package `@nerdcash/vendure-payment-zcash` | `.github/workflows/publish-vendure-plugin.yml`: pushing a `vendure-v<version>` tag publishes to npm with trusted publishing and provenance |
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

## Publishing the npm plugins

The Medusa and Vendure plugins publish the same way as the Node client. They call the service's
HTTP API directly and don't depend on `@nerdcash/zcash-payment-client`, so they can be released
in any order relative to it.

To release a new version after merge:

1. Bump `version` in `plugins/<medusa|vendure>/package.json` and run `npm install` there to update
   the lockfile, in a PR.
2. After it merges, tag `main` and push the tag:
   `git tag medusa-v<version> && git push origin medusa-v<version>` (or `vendure-v<version>`).
   The workflow fails if the tag does not match `package.json`. It runs the typecheck and tests,
   builds through `prepack`, and publishes with provenance.

Check the tarball before a release with `npm pack --dry-run` in the plugin directory. It should
contain only the build output (`.medusa/server` or `dist`), `README.md`, `LICENSE`, and
`package.json`.

### First release of a new package

npm trusted publishing can be configured only for a package that already exists, so the first
version of each package is published by hand from a clean checkout of `main`:

```sh
npm login
cd plugins/medusa   # or plugins/vendure
npm ci
npm test
npm publish --access public
```

Then on npmjs.com, open the package's Settings, add a trusted publisher of type GitHub Actions,
and enter organization or user `nerdcash`, repository `zcash-payment-service`, workflow filename
`publish-medusa-plugin.yml` (or `publish-vendure-plugin.yml`), and environment `npm`. While there,
you can set Publishing access to require two-factor authentication and disallow tokens.
Then tag the hand-published version (`medusa-v0.1.0`, `vendure-v0.1.0`) so the tags record what
was released; the workflow sees the version already on npm and skips publishing. From the next
version on, pushing the tag publishes.

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


### Releasing the service

1. Bump `version` in `Cargo.toml` (and `Cargo.lock` with `cargo update -p zcash-payment-service`) in a PR, and merge it.
2. Tag the merge commit: `git tag v<version> && git push origin v<version>`.
3. `release.yml` checks the tag against `Cargo.toml`, runs the tests, and pushes the versioned images.
   A pre-release tag such as `v0.2.0-rc.1` publishes only its own tags and never moves `:latest`.

## Changing the docs

- `book/` builds with mdBook. Check it locally with `mdbook build book` before pushing.
- Keep the guide and `src/spec.md` consistent. If they disagree, the code wins: fix the docs.
- The README stays a short pitch that links into the guide. Put details in `book/`.

## Pull requests

- Keep each PR focused. Do not rebase or force-push branches that others may have pulled; add commits instead.
- CI must pass. Review threads should be answered and resolved before merge.
