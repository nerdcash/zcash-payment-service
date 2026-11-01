# Vendure plugin

`plugins/vendure` is a [Vendure](https://vendure.io) 3 plugin (`@nerdcash/vendure-payment-zcash`) that takes Zcash through your own copy of this service. It adds a payment method handler, a signed webhook route, and guards that keep a held order from being fulfilled.

## How it works

1. The storefront calls `addPaymentToOrder` with method `zcash`. The handler calls `POST /payment-session` with the order total and a `return_url`, and stores the issued address as the payment's `transactionId`. The payment is **Authorized**, so Vendure places the order (`PaymentAuthorized`) with payment still outstanding.
2. The storefront sends the customer to the [pay page](pay-page.md). Its link is in the payment's metadata as `pay_url`; the Shop API shows only the metadata's `public` part, which also holds the address, the ZEC amount, and the QR text.
3. The service sends a signed webhook to `/zcash-payments/webhook` each time the address's total changes. The plugin checks the `X-Signature` HMAC, finds the payment by its address, ignores stale events, and confirms the total with `GET /payments/{id}`. Once the total covers the payment, it settles it, and the order moves to `PaymentSettled`. A partial payment is recorded in the order history but settles nothing.
4. The pay page sends the customer back to your storefront.

Settling a Zcash payment by hand in the admin runs the same live check, so it is refused until the service reports enough received.

Edge cases the plugin handles:

- **Funds drop after settlement** (an unconfirmed transaction expires, or a reorg): the plugin puts the order on hold. It adds a private history note and publishes a `ZcashPaymentHoldEvent`. While held, fulfillments can't be created, shipped, or delivered, and the order can't move to a shipped or delivered state. When the funds return and the service confirms the full total, the hold is released. **Releasing a hold never settles again or re-runs order placement or fulfillment.**
- **A payment's amount is fixed once its address is issued.** If the order changes, add a new payment; the old address is not reused or re-priced.
- **Payment for a cancelled order** is never settled. The plugin adds a history note and publishes a `ZcashPaymentReviewEvent` so you can refund it. If a cancellation lands while a settlement is in flight, the order is flagged the same way.
- **Stale or replayed events** are ignored by their event version. Overlapping deliveries for one payment are serialized with a row lock, and the version is stored only after the update succeeds, so a failed update is retried.
- **State is kept in the payment's metadata**, which storefronts can't write, and amounts are converted with exact integer math.

## Setup

Requires Vendure 3 and Node.js 20+.

1. Run the service as in [Deployment](deployment.md), with `ZCASH_WEBHOOK_URL` pointing at your Vendure server's `/zcash-payments/webhook` route and a random `ZCASH_WEBHOOK_SECRET`. Set `PUBLIC_BASE_URL` and `RETURN_URL_ALLOWED_ORIGINS` (your storefront's origin) as described in [Exposing the page](pay-page.md#exposing-the-page). See the [WooCommerce page](woocommerce.md#setup) for choosing `WEBHOOK_REPORT_CONFIRMATIONS`; the same trade-off applies.
2. Build and install the plugin. It isn't on npm yet:

   ```sh
   cd plugins/vendure && npm ci && npm run build && npm pack
   cd /path/to/your-vendure-app && npm install /path/to/nerdcash-vendure-payment-zcash-0.1.0.tgz
   ```

3. Add it to `vendure-config.ts`:

   ```ts
   import { ZcashPaymentPlugin } from "@nerdcash/vendure-payment-zcash"

   plugins: [
     ZcashPaymentPlugin.init({
       serviceUrl: "http://zcash-payment-service:8787", // private address
       webhookSecret: process.env.ZCASH_WEBHOOK_SECRET!,
       zecRate: { usd: "0.025" },                        // ZEC per unit of store currency
       returnUrl: "https://shop.example/order/{order_code}",
     }),
   ],
   ```

   | Option | Meaning |
   | --- | --- |
   | `serviceUrl` | Where Vendure reaches the service. It does not need to be public. |
   | `webhookSecret` | The same value as `ZCASH_WEBHOOK_SECRET`. |
   | `zecRate` | A decimal string, or a map by currency code. Required unless the channel prices in ZEC. The plugin does not fetch prices from any third party; keep it current yourself. |
   | `returnUrl` | Where the pay page returns the customer. `{order_code}` is replaced with the order code. A storefront may pass `return_url` in the `addPaymentToOrder` metadata instead. |
   | `payBaseUrl` | Only if the service does not set `PUBLIC_BASE_URL`. |

4. In the admin, create a payment method with the **Zcash** handler (**Settings → Payment methods**).

### Storefront

Move the order to `ArrangingPayment`, call `addPaymentToOrder(input: { method: "<your payment method code>", metadata: {} })`, then redirect the customer to the payment's `metadata.public.pay_url`. The order turns `PaymentSettled` once the payment arrives. `plugins/vendure/e2e/app/src/demo-store.plugin.ts` contains a minimal working storefront.

## Testing

Nothing below needs a wallet, an exchange account, or test coins.

**Unit tests** cover amount conversion, signature checks, webhook parsing, and the webhook decision rules (settle, partial, hold, release, review, stale):

```sh
cd plugins/vendure && npm ci && npm run typecheck && npm test && npm run build
```

**End-to-end test** (`plugins/vendure/e2e`) starts Postgres, a Vendure server with the plugin and the admin UI installed, and this service with Docker Compose, then drives a headless Chromium through checkout:

```sh
cd plugins/vendure/e2e
npm ci && npx playwright install --with-deps chromium   # once per machine
./run.sh                 # KEEP_STACK=1 leaves it running on http://localhost:3000
```

The service runs on testnet with `ZCASH_DEV_SIMULATED_PAYMENTS=true`, which turns on [`POST /dev/simulate-payment`](api.md#post-devsimulate-payment-testnet-test-hook). The test:

- places an order and checks the pay page shows the right amount;
- simulates a partial payment, then the rest, and checks the browser returns to the store and the order is settled exactly once;
- sends signed webhooks for a drop in funds (the order is held and a fulfillment is refused), a stale event (ignored), and the recovery (the hold is released with no second settlement, and a fulfillment then succeeds once);
- checks that an administrator can't settle an unpaid Zcash payment;
- pays a cancelled order and checks it is flagged for review, not settled;
- checks that the webhook rejects a bad signature with `401`, acknowledges an unknown address, and that the service refuses a foreign `return_url`.

CI runs both on every pull request (`.github/workflows/vendure-plugin.yml`) and keeps screenshots, including the admin order page, as a build artifact.

### What the tests do not cover

- **The chain.** Simulated payments skip lightwalletd, trial decryption, and confirmations. A real testnet payment is still worth doing by hand before a release.
