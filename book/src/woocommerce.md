# WooCommerce plugin

`plugins/woocommerce/zcash-payment-gateway` is a WooCommerce payment gateway that takes Zcash through your own copy of this service. It works with both the block checkout (the default for new stores) and the classic shortcode checkout, and it supports High-Performance Order Storage.

## How it works

1. The customer picks **Zcash** at checkout and places the order.
2. The plugin calls `POST /payment-session` with the order total, the order number as the label, and the order-received page as `return_url`. It stores the issued address and payment ID on the order and leaves the order **Pending payment**.
3. The customer is sent to the service's [pay page](pay-page.md), which shows the amount, a QR code, and an *Open in wallet* link, and updates live.
4. The service sends a signed webhook each time the address's total changes. The plugin checks the `X-Signature` HMAC, ignores stale events, and calls WooCommerce's `payment_complete()` once the total covers the order. A partial payment adds an order note instead.
5. The pay page sends the customer back to the order-received page.

Edge cases the plugin handles:

- **Paying again later** reuses the order's existing address, so a payment to the address the customer already saw still counts. If the order total changed since, it gets a new address, unless money already arrived; then checkout stops and an order note asks you to reconcile by hand.
- **Funds drop after payment** (an unconfirmed transaction expires, or a reorg): the order goes **On hold** with a note.
- **Payment for a cancelled, failed, or refunded order** is never completed automatically, since its stock may be gone. The plugin adds a note for you to review.
- **An order keeps one Zcash address for life.** If you change an order's total after its address was issued, checkout stops and a payment arriving at the old address is noted for you to reconcile rather than completing the order. The same applies if the customer switches the order to another payment method. Once a payment is set aside for review, later payments to that order are also left for you until you delete the order's `_zcash_needs_review` meta.
- **After a partial payment**, the pay page's QR code and wallet link ask only for the remaining balance. The remainder is based on receipts the service has reported (see `WEBHOOK_REPORT_CONFIRMATIONS`), so a just-mined payment can take a block or two to count.
- **Holds you set yourself are kept.** The plugin only completes an on-hold order automatically if it put the order on hold itself (after funds dropped).
- **Refunded or cancelled orders with Zcash funds are flagged for review**, and a closed order can't be paid again through checkout until you reconcile it.
- **Overlapping webhook deliveries** for one order are serialized with a database lock, and the event version is recorded only after the order update succeeds, so a failed update is retried.

If the service has no public pay page (no `PUBLIC_BASE_URL` and no *Pay page base URL* setting), the customer goes straight to the order-received page, which shows the address and a wallet link.

## Setup

1. Run the service as in [Deployment](deployment.md), with `ZCASH_WEBHOOK_URL` set to your store's webhook route and a random `ZCASH_WEBHOOK_SECRET`:

   ```text
   ZCASH_WEBHOOK_URL=https://shop.example.com/?rest_route=/zcash-payment/v1/webhook
   ```

   Also choose `WEBHOOK_REPORT_CONFIRMATIONS` before the service's first start; it is fixed once the database exists. The default `0` reports unconfirmed (mempool) payments, so the plugin marks an order paid before the transaction is mined. That is fine for low-value or digital goods where speed matters. If a dropped transaction would cost you, set `1` or more so orders are paid only from mined transactions. Either way, if funds for a paid order later disappear, the plugin puts the order **On hold** and adds a note.

   The `?rest_route=` form works with any permalink setting. `https://shop.example.com/wp-json/zcash-payment/v1/webhook` also works when pretty permalinks are on.
2. Expose the pay page publicly as described in [Exposing the page](pay-page.md#exposing-the-page) and set `PUBLIC_BASE_URL`. Set `RETURN_URL_ALLOWED_ORIGINS` to your store's origin (for example `https://shop.example`) so the pay page only ever returns payers to your store.
3. Copy `plugins/woocommerce/zcash-payment-gateway` into `wp-content/plugins/` (or zip the folder and upload it under **Plugins → Add New**) and activate it.
4. Under **WooCommerce → Settings → Payments → Zcash**, set:
   - **Service URL**: where WordPress reaches the service, for example `http://zcash-payment-service:8787`. This is a private address; it does not need to be public.
   - **Webhook secret**: the same value as `ZCASH_WEBHOOK_SECRET`.
   - **Pay page base URL**: only if the service does not set `PUBLIC_BASE_URL`.
   - **ZEC per unit of store currency**: only if the store does not price in ZEC. See below.

### Store currency

The plugin adds ZEC to WooCommerce's currency list. If the store prices in ZEC, order totals are sent as-is. Before entering ZEC prices, set **WooCommerce → Settings → General → Number of decimals** to 8; with the default of 2, a price such as 0.001 ZEC rounds to 0. If it prices in another currency, you must enter a fixed rate (ZEC per one unit of store currency) and keep it current yourself; the gateway hides itself when no rate is set. The plugin does not fetch prices from any third party.

## Testing

Everything below runs without a WooCommerce.com, WordPress.com, wallet, or exchange account, and without real or test coins.

**Unit tests** cover amount conversion, signature checks, webhook parsing, and pay URL handling:

```sh
php plugins/woocommerce/tests/unit.php
```

**End-to-end test** (`plugins/woocommerce/e2e`) starts MySQL, WordPress, and this service with Docker Compose, installs WooCommerce from wordpress.org, configures the gateway, and drives a headless Chromium through the real block checkout:

```sh
cd plugins/woocommerce/e2e
npx playwright install --with-deps chromium   # once per machine, after npm ci
./run.sh                 # KEEP_STACK=1 leaves it running on http://localhost:8080
```

The service runs on testnet with `ZCASH_DEV_SIMULATED_PAYMENTS=true`, which turns on [`POST /dev/simulate-payment`](api.md#post-devsimulate-payment-testnet-test-hook). The test:

- places an order and checks that the browser lands on the pay page with the right amount and a QR code;
- simulates a partial payment and checks the pay page shows it, then simulates the rest;
- checks that the browser returns to the order-received page and that the order is **Processing** with the full amount recorded;
- checks that the webhook route rejects a bad signature with `401` and acknowledges a signed event for an unknown address.

CI runs both on every pull request (`.github/workflows/woocommerce-plugin.yml`) and keeps screenshots of each step as a build artifact.

### What the tests do not cover

- **The chain.** Simulated payments skip lightwalletd, trial decryption, and confirmations. The service's own test suite covers scanning; a real testnet payment is still worth doing by hand before a release.
- **Wallet apps.** Nobody scans the QR code or opens the `zcash:` link in a real wallet.
- **Other setups.** Only one WooCommerce version, the default theme, and the block checkout are exercised. The classic checkout, other themes, and other plugins that change checkout are not.
- **Non-ZEC pricing.** The manual exchange rate path has unit tests but no end-to-end run.
- **Email.** The test stack has no mail server.
