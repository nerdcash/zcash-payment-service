# Node.js client

If your store runs on Node.js, you don't need to hand-roll the HTTP call or the signature check.
[`@nerdcash/zcash-payment-client`](https://www.npmjs.com/package/@nerdcash/zcash-payment-client)
wraps both. Its source is in [`clients/node`](https://github.com/nerdcash/zcash-payment-service/tree/main/clients/node).

- It needs Node.js 20 or later.
- It's ESM-only and ships TypeScript types.
- Its only dependency is Node's built-in `node:crypto`.

## Install

```sh
npm install @nerdcash/zcash-payment-client
```

## Create the client

```ts
import { createZcashPaymentClient } from "@nerdcash/zcash-payment-client";

const payments = createZcashPaymentClient({
  baseUrl: process.env.ZCASH_PAYMENT_SERVICE_BASE_URL!, // e.g. http://zcash-payment-service:8787
  webhookSecret: process.env.ZCASH_WEBHOOK_SECRET!, // the same secret the service signs with
  // timeoutMs: 10_000,  // optional, for POST /payment-session
  // fetchImpl: fetch,   // optional, for tests or proxies
});
```

## Create a payment session

`createPaymentSession` calls [`POST /payment-session`](api.md#post-payment-session) and returns `{ address, qr_text }`.

```ts
const session = await payments.createPaymentSession({
  label: "Espresso cup",
  memo: `Order ${orderId}`,
  message: "Thank you for your order.",
  amount: "0.05000000", // ZEC as a decimal string
});
```

It throws `PaymentServiceError` when the service answers with an error or can't be reached. `error.status` holds the HTTP status, or `0` when the service was unreachable.

## Show the address and QR code

Show `session.address` as text. Render `session.qr_text` (a `zcash:` URI, see
[Showing the address and the QR code](integration.md#showing-the-address-and-the-qr-code)) as a QR image.
Any QR library works. [`qrcode`](https://www.npmjs.com/package/qrcode) runs on the server and in the browser:

```ts
import QRCode from "qrcode";

const svg = await QRCode.toString(session.qr_text, { type: "svg" });
```

## Verify a webhook

`verifyWebhook(rawBody, signature)` does three things:

1. It checks `X-Signature` against the raw body in constant time.
2. It validates the payload.
3. It returns the observation: `event_id`, `payment_source`, `address`, `total_received`, and `observed_at`.

Pass it the **raw** body bytes, not re-serialized JSON. In Express, give this route its own
`express.raw()` parser and register it **before** any app-wide `express.json()` (or exclude the route
from global body parsers). If a JSON parser runs first, `req.body` is an object and every delivery
fails verification.

```ts
import express from "express";
import { WebhookPayloadError, WebhookSignatureError } from "@nerdcash/zcash-payment-client";

app.post("/webhooks/zcash-payments", express.raw({ type: "application/json" }), async (req, res) => {
  let observation;
  try {
    observation = payments.verifyWebhook(req.body, req.get("x-signature"));
  } catch (error) {
    if (error instanceof WebhookSignatureError) return res.status(401).end();
    if (error instanceof WebhookPayloadError) return res.status(400).end();
    throw error;
  }

  // event_id is zcash:<address_id>:v<state_version>.
  const stateVersion = Number(observation.event_id.split(":v").pop());

  // In ONE database transaction:
  //   1. read the last state_version stored for observation.address (lock the row);
  //   2. if stateVersion <= that value, change nothing;
  //   3. otherwise store stateVersion, event_id, and total_received for the address,
  //      and update the invoice.
  // Doing the compare and the writes atomically keeps a retried or overlapping
  // older delivery from overwriting newer state.
  await applyObservationAtomically( // your own persistence code
    observation.address, stateVersion, observation);

  res.status(200).end();
});
```

The client checks the signature and the payload's shape. Your application still has to do the steps in
[A minimal handler](integration.md#a-minimal-handler): deduplicate events, compare `state_version`, and decide when an invoice is paid.

## Other languages

The service's wire contract is plain HTTP and JSON. For other stacks, follow the [Integration guide](integration.md) and the [API reference](api.md).
