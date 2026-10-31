# @nerdcash/zcash-payment-client

A small Node.js client for [zcash-payment-service](https://github.com/nerdcash/zcash-payment-service).
It has no runtime dependencies beyond Node's built-in `node:crypto`.

- `createPaymentSession` asks the service for a fresh unified address and the `zcash:` URI to show as a QR code.
- `verifyWebhook` checks the HMAC-SHA256 `X-Signature` on a payment observation the service posts to your store, then validates and returns the payload.

Your store still owns prices, orders, and the decision of when an order counts as paid.

## Install

```sh
npm install @nerdcash/zcash-payment-client
```

Requires Node.js 20 or later. The package is ESM-only and ships TypeScript types.

## Usage

```ts
import express from "express";
import {
  createZcashPaymentClient,
  PaymentServiceError,
  WebhookPayloadError,
  WebhookSignatureError,
} from "@nerdcash/zcash-payment-client";

const payments = createZcashPaymentClient({
  baseUrl: process.env.ZCASH_PAYMENT_SERVICE_BASE_URL!, // e.g. http://zcash-payment-service:8787
  webhookSecret: process.env.ZCASH_WEBHOOK_SECRET!, // same secret the service signs with
});

// At checkout: get an address for this order.
const session = await payments.createPaymentSession({
  label: "Espresso cup",
  memo: `Order ${orderId}`,
  message: "Thank you for your order.",
  amount: "0.05000000", // ZEC, as a decimal string
});
// session.address -> show to the customer
// session.qr_text -> render as a QR code (see "Rendering the QR code" below)

// Webhook: verify against the *raw* request body.
const app = express();
app.post("/webhooks/zcash-payments", express.raw({ type: "application/json" }), (req, res) => {
  try {
    const observation = payments.verifyWebhook(req.body, req.get("x-signature"));
    // observation.address, observation.total_received, observation.event_id, ...
    // Deduplicate on event_id, then compare total_received with the amount due.
    res.json({ ok: true });
  } catch (error) {
    if (error instanceof WebhookSignatureError) return res.status(401).end();
    if (error instanceof WebhookPayloadError) return res.status(400).end();
    throw error;
  }
});
```

### Rendering the QR code

`qr_text` is a `zcash:` payment URI. Any QR library works. [`qrcode`](https://www.npmjs.com/package/qrcode)
runs both in Node and in the browser:

```ts
import QRCode from "qrcode";

// Server side: inline SVG for your checkout page.
const svg = await QRCode.toString(session.qr_text, { type: "svg", errorCorrectionLevel: "M" });

// Or in the browser: draw onto a <canvas>.
await QRCode.toCanvas(document.querySelector("canvas")!, session.qr_text);
```

Also show `address` as copyable text, and link the `qr_text` URI (`<a href="zcash:...">`) so a wallet on the same device can open it.

### API

`createZcashPaymentClient(options)`

| Option | Description |
| --- | --- |
| `baseUrl` | Base URL of the service. Only the origin is used. |
| `webhookSecret` | Shared webhook signing secret. Required. |
| `fetchImpl` | Optional `fetch` replacement (tests, proxies). |
| `timeoutMs` | Timeout for `createPaymentSession`. Default `10000`. |

Errors:

- `PaymentServiceError` (`status` is the HTTP status, or `0` when the service is unreachable).
- `WebhookSignatureError` when the signature is missing or wrong.
- `WebhookPayloadError` when a correctly signed body is not a valid observation.

## Development

```sh
npm ci
npm test
npm run build
```

## Releasing

Releases are published from GitHub Actions with npm trusted publishing (OIDC) and provenance.
Bump `version` in `package.json`, merge, then push a tag named `client-node-v<version>` (for example `client-node-v0.1.0`).
The [publish workflow](../../.github/workflows/publish-node-client.yml) checks that the tag matches the package version, runs the tests, and publishes.

## License

MIT
