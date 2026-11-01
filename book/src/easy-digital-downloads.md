# Easy Digital Downloads plugin

`plugins/easy-digital-downloads/zcash-payment-edd` is an [Easy Digital Downloads](https://easydigitaldownloads.com) (EDD) payment gateway that takes Zcash through your own copy of this service. It works with EDD's block checkout and the classic `[download_checkout]` shortcode.

## How it works

1. The customer picks **Zcash** at checkout and places the order.
2. The plugin records the order as **Pending** and calls `POST /payment-session` with the order total, the order number as the label, and EDD's receipt page as `return_url`. It stores the issued address and payment ID on the order.
3. The customer is sent to the service's [pay page](pay-page.md), which shows the amount, a QR code, and an *Open in wallet* link, and updates live.
4. The service sends a signed webhook each time the address's total changes. The plugin checks the `X-Signature` HMAC, ignores stale events, and marks the order **Completed** (which releases the downloads) once the total covers it. A partial payment adds an order note instead.
5. The pay page sends the customer back to the receipt page.

Edge cases the plugin handles:

- **Waiting for confirmations.** When the service sets [`REQUIRED_CONFIRMATIONS`](integration.md#waiting-for-confirmations), the plugin completes the order only once the last `received_by_confirmations` element covers it. A payment seen in the mempool, or with fewer confirmations, adds a *pending confirmation* order note and leaves the order **Pending**; a reorg that undoes those confirmations on a completed order puts it **On hold**. When the service doesn't set `REQUIRED_CONFIRMATIONS`, the array is absent and the plugin uses `total_received` as before.
- **An order keeps one Zcash address for life.** If the order's total changes after its address was issued, a payment to the old address is noted for you to reconcile rather than completing the order.
- **Funds drop after payment** (an unconfirmed transaction expires, or a reorg): the order goes **On hold** with a note. The plugin restores it only if it put the order on hold itself.
- **Payment for a refunded, revoked, failed, or abandoned order** is never completed automatically. Refunding or revoking an order with a Zcash address flags it for review, and later payments to it are left for you until you delete the order's `_zcash_needs_review` meta.
- **After a partial payment**, the pay page asks only for the remaining balance. The remainder is based on receipts the service has reported (see `WEBHOOK_REPORT_CONFIRMATIONS`).
- **Overlapping webhook deliveries** for one order are serialized with a database lock, and the event version is recorded only after the order update succeeds, so a failed update is retried.

If the service has no public pay page (no `PUBLIC_BASE_URL` and no *Pay page base URL* setting), the customer goes straight to the receipt page, which shows the address and a wallet link.

## Setup

Requires WordPress 6.4+, PHP 8.1+, and Easy Digital Downloads 3.1+ (the plugin stays inactive on older EDD).

1. Run the service as in [Deployment](deployment.md), with `ZCASH_WEBHOOK_URL` set to your store's webhook route and a random `ZCASH_WEBHOOK_SECRET`:

   ```text
   ZCASH_WEBHOOK_URL=https://shop.example.com/?rest_route=/zcash-payment-edd/v1/webhook
   ```

   The route differs from the WooCommerce plugin's, so both plugins can run on one site. Choose `WEBHOOK_REPORT_CONFIRMATIONS` before the service's first start, as explained in the [WooCommerce setup](woocommerce.md#setup); the default `0` completes orders from unconfirmed transactions, which suits instant digital delivery but means a dropped transaction leaves files already delivered.
2. Expose the pay page publicly as described in [Exposing the page](pay-page.md#exposing-the-page) and set `PUBLIC_BASE_URL`. Set `RETURN_URL_ALLOWED_ORIGINS` to your store's origin.
3. Copy `plugins/easy-digital-downloads/zcash-payment-edd` into `wp-content/plugins/` (or zip the folder and upload it) and activate it.
4. Under **Downloads → Settings → Payments**, enable **Zcash (self-hosted)**, then on its **Zcash** section set:
   - **Service URL**: where WordPress reaches the service, for example `http://zcash-payment-service:8787`. This does not need to be public.
   - **Webhook secret**: the same value as `ZCASH_WEBHOOK_SECRET`.
   - **Pay page base URL**: only if the service does not set `PUBLIC_BASE_URL`.
   - **ZEC per unit of store currency**: only if the store does not price in ZEC.

The gateway hides itself at checkout until the service URL and secret are set.

### Store currency

The plugin adds ZEC to EDD's currency list and shows ZEC amounts with 8 decimal places. If the store prices in ZEC, order totals are sent as-is. If it prices in another currency, you must enter a fixed rate (ZEC per one unit of store currency) and keep it current yourself; the gateway hides itself when no rate is set. The plugin does not fetch prices from any third party.

## Testing

Everything below runs without an EDD, WordPress.com, wallet, or exchange account, and without real or test coins.

**Unit tests** cover amount conversion, signature checks, webhook parsing, and pay URL handling:

```sh
php plugins/easy-digital-downloads/tests/unit.php
```

**End-to-end test** (`plugins/easy-digital-downloads/e2e`) starts MySQL, WordPress, and this service with Docker Compose, installs EDD from wordpress.org, configures the gateway, and drives a headless Chromium through the block checkout:

```sh
cd plugins/easy-digital-downloads/e2e
npm ci && npx playwright install --with-deps chromium   # once per machine
./run.sh                 # KEEP_STACK=1 leaves it running on http://localhost:8080
```

The service runs on testnet with `ZCASH_DEV_SIMULATED_PAYMENTS=true`, which turns on [`POST /dev/simulate-payment`](api.md#post-devsimulate-payment-testnet-test-hook). The test:

- buys a download and checks that the browser lands on the pay page with the right amount and a QR code;
- simulates a partial payment and checks the pay page shows it, then simulates the rest;
- checks that the browser returns to the receipt page and that the order is **Completed** with the full amount recorded;
- pays a revoked order and checks that the payment is held for review and the order stays revoked;
- simulates one payment at 0, 1, and 2 confirmations (the stack sets `REQUIRED_CONFIRMATIONS=2`) and checks the order stays **Pending** with a *pending confirmation* note until the last;
- checks that the webhook route rejects a bad signature with `401` and acknowledges a signed event for an unknown address.

CI runs both on every pull request through `.github/workflows/checks.yml`, which calls `.github/workflows/edd-plugin.yml`, and keeps screenshots of each step as a build artifact.

### What the tests do not cover

- **The chain.** Simulated payments skip lightwalletd and trial decryption, and their confirmations are simulated rather than mined.
- **Wallet apps.** Nobody scans the QR code or opens the `zcash:` link in a real wallet.
- **Other setups.** Only one EDD version, the default theme, and the block checkout are exercised.
- **Non-ZEC pricing.** The manual exchange rate path has unit tests but no end-to-end run.
- **Email.** The test stack has no mail server.
