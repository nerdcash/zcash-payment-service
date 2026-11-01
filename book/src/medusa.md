# Medusa plugin

`plugins/medusa` is a [Medusa](https://medusajs.com) v2 plugin (`@nerdcash/medusa-payment-zcash`) that takes Zcash through your own copy of this service. It adds a payment provider, a signed webhook route, and an order widget in the admin.

## How it works

1. The customer picks **Zcash** at checkout. Medusa creates a payment session, and the provider calls `POST /payment-session` with the cart total and a `return_url`. It stores the issued address and the [pay page](pay-page.md) link (`pay_url`) in the session data.
2. The storefront completes the cart. The session is **pending authorization**, so Medusa places the order with payment still outstanding, and the storefront sends the customer to `pay_url`.
3. The service sends a signed webhook to `/hooks/zcash` each time the address's total changes. The plugin checks the `X-Signature` HMAC, finds the session by its address, ignores stale events, and confirms the total with `GET /payments/{id}`. Once the total covers the order, it runs Medusa's `processPaymentWorkflow`, which authorizes and captures the payment. A partial payment is recorded but captures nothing.
4. The pay page sends the customer back to your storefront.

Edge cases the plugin handles:

- **Waiting for confirmations.** When the service sets [`REQUIRED_CONFIRMATIONS`](integration.md#waiting-for-confirmations), the session is authorized as soon as `total_received` (mempool included) covers it, so checkout can finish, but the plugin captures only once the last `received_by_confirmations` element covers it. Until then it logs *pending confirmation*. A reorg that undoes those confirmations after capture sets the hold described below. When the service doesn't set `REQUIRED_CONFIRMATIONS`, the array is absent and the plugin uses `total_received` as before.
- **Funds drop after capture** (an unconfirmed transaction expires, or a reorg): the plugin sets the order's `zcash_payment_hold` metadata, shows it in the admin widget, and emits `zcash_payment.hold_changed` so your fulfillment code can wait. When the funds return and the service confirms the full total, the hold is cleared. **Releasing a hold never captures again or re-runs order placement or fulfillment.**
- **Holds you set yourself are kept.** If `zcash_payment_hold` already has a value, the plugin leaves it alone, and it only clears the exact message it wrote.
- **A payment's amount is fixed once its address is issued.** If the cart total changes, start a new payment session; the old address is not reused or re-priced.
- **Payment for a canceled order** is never captured. The plugin records it in `zcash_payment_review` metadata, shown in the widget, for you to reconcile. If a cancellation lands while a capture is in flight, the order is flagged the same way so you can refund it.
- **Stale or replayed events** are ignored by their event version. Overlapping deliveries for one session are serialized with Medusa's locking module, and the version is stored only after the update succeeds, so a failed update is retried.
- **State is kept in session metadata**, which storefront clients can't write, and amounts are converted with exact decimal math.

## Setup

Requires Medusa 2.22 or later and Node.js 20+.

1. Run the service as in [Deployment](deployment.md), with `ZCASH_WEBHOOK_URL` pointing at your Medusa server's `/hooks/zcash` route and a random `ZCASH_WEBHOOK_SECRET`. Set `PUBLIC_BASE_URL` and `RETURN_URL_ALLOWED_ORIGINS` (your storefront's origin) as described in [Exposing the page](pay-page.md#exposing-the-page). See the [WooCommerce page](woocommerce.md#setup) for choosing `WEBHOOK_REPORT_CONFIRMATIONS`; the same trade-off applies.
2. Build and install the plugin. It isn't on npm yet:

   ```sh
   cd plugins/medusa && npm ci && npm run build && npm pack
   cd /path/to/your-medusa-app && npm install /path/to/nerdcash-medusa-payment-zcash-0.1.0.tgz
   ```

3. Register it in `medusa-config.ts`:

   ```ts
   plugins: [{ resolve: "@nerdcash/medusa-payment-zcash", options: {} }],
   modules: [
     {
       resolve: "@medusajs/medusa/payment",
       options: {
         providers: [
           {
             resolve: "@nerdcash/medusa-payment-zcash/providers/zcash",
             id: "zcash",
             options: {
               serviceUrl: "http://zcash-payment-service:8787", // private address
               webhookSecret: process.env.ZCASH_WEBHOOK_SECRET,
               zecRate: { usd: "0.025" },                        // ZEC per unit of store currency
               returnUrl: "https://shop.example/order/confirmed?session_id={session_id}",
             },
           },
         ],
       },
     },
   ],
   ```

   | Option | Meaning |
   | --- | --- |
   | `serviceUrl` | Where Medusa reaches the service. It does not need to be public. |
   | `webhookSecret` | The same value as `ZCASH_WEBHOOK_SECRET`. |
   | `zecRate` | A decimal string, or a map by currency code. Required unless the region prices in ZEC. The plugin does not fetch prices from any third party; keep it current yourself. |
   | `returnUrl` | Where the pay page returns the customer. `{session_id}` is replaced with the payment session id. A storefront may pass `return_url` in the session data instead. |
   | `payBaseUrl` | Only if the service does not set `PUBLIC_BASE_URL`. |

4. Run `npx medusa db:migrate`, then enable the provider `pp_zcash_zcash` on your regions in the admin (**Settings → Regions**).

### Storefront

After `initiatePaymentSession` with `provider_id: "pp_zcash_zcash"`, complete the cart as usual, then redirect the customer to the session's `data.pay_url`. The order's `payment_status` turns `captured` once the payment arrives. `plugins/medusa/e2e/app/src/api/demo-store` contains a minimal working storefront.

## Testing

Nothing below needs a wallet, an exchange account, or test coins.

**Unit tests** cover amount conversion, signature checks, webhook parsing, and the webhook decision rules (capture, partial, hold, release, review, stale):

```sh
cd plugins/medusa && npm ci && npm run typecheck && npm test && npm run build
```

**End-to-end test** (`plugins/medusa/e2e`) starts Postgres, a Medusa store with the plugin installed, and this service with Docker Compose, then drives a headless Chromium through checkout:

```sh
cd plugins/medusa/e2e
npm ci && npx playwright install --with-deps chromium   # once per machine
./run.sh                 # KEEP_STACK=1 leaves it running on http://localhost:9000
```

The service runs on testnet with `ZCASH_DEV_SIMULATED_PAYMENTS=true`, which turns on [`POST /dev/simulate-payment`](api.md#post-devsimulate-payment-testnet-test-hook). The test:

- places an order and checks the pay page shows the right amount;
- simulates a partial payment, then the rest, and checks the browser returns to the store and the order is captured exactly once;
- sends signed webhooks for a drop in funds (the order is held), a stale event (ignored), and the recovery (the hold is released with no second capture);
- pays a canceled order and checks it is flagged for review, not captured;
- simulates one payment at 0, 1, and 2 confirmations (the stack sets `REQUIRED_CONFIRMATIONS=2`) and checks it is captured only at the last;
- checks that the webhook rejects a bad signature with `401`, acknowledges an unknown address, and that the service refuses a foreign `return_url`.

CI runs both on every pull request (`.github/workflows/medusa-plugin.yml`) and keeps screenshots, including the admin widget, as a build artifact.

### What the tests do not cover

- **The chain.** Simulated payments skip lightwalletd and trial decryption, and their confirmations are simulated rather than mined. A real testnet payment is still worth doing by hand before a release.
