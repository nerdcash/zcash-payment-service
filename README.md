# zcash-payment-service

Accept Zcash in your app with a **free, self-hosted** service that only ever holds a **viewing key**.

- **Free:** no fees, no payment-processor account, no usage meter. Payers' ZEC goes straight to your wallet.
- **Self-hosted:** run it as a small Docker container or embed the Rust crate in your own process.
- **Viewing-key only:** give it a unified incoming viewing key (UIVK). It hands out a fresh shielded address per checkout and sends your app a signed webhook when funds arrive. It can see payments, but it can never spend them.

[![Watch the demo](https://img.youtube.com/vi/OIlkqLVTn0w/maxresdefault.jpg)](https://www.youtube.com/watch?v=OIlkqLVTn0w)

## Quick start

```sh
docker pull ghcr.io/nerdcash/zcash-payment-service:main
```

Set `ZCASH_UIVK`, `ZCASH_BIRTHDAY_HEIGHT`, and `LIGHTWALLETD_URL` (plus `ZCASH_WEBHOOK_URL` and `ZCASH_WEBHOOK_SECRET` for payment webhooks), then run it. Or use `docker compose up --build` or `cargo run` from a clone.
See [Getting started](https://nerdcash.github.io/zcash-payment-service/getting-started.html) for the full walkthrough.

## Documentation

The guide at **[nerdcash.github.io/zcash-payment-service](https://nerdcash.github.io/zcash-payment-service/)** covers:

- [Overview](https://nerdcash.github.io/zcash-payment-service/overview.html): what the service does and doesn't do
- [Integration guide](https://nerdcash.github.io/zcash-payment-service/integration.html): creating payment sessions, showing the QR code, handling webhooks
- [Node.js client](https://nerdcash.github.io/zcash-payment-service/node-client.html): using the npm package
- [API reference](https://nerdcash.github.io/zcash-payment-service/api.html): HTTP routes, the webhook contract, configuration, crate entry points
- [Deployment](https://nerdcash.github.io/zcash-payment-service/deployment.html): the Docker image, Compose, and embedding the crate

## Node.js client

[`@nerdcash/zcash-payment-client`](https://www.npmjs.com/package/@nerdcash/zcash-payment-client) creates payment sessions and verifies webhook signatures for you:

```sh
npm install @nerdcash/zcash-payment-client
```

## Contributing

See [CONTRIBUTING.md](CONTRIBUTING.md) for the repo layout, development commands, and how to keep the service, client, and docs in sync.

## License

The Node.js client in [`clients/node`](clients/node) is [MIT licensed](clients/node/LICENSE).
