# PrestaShop module

`plugins/prestashop/zcashpayment` is a [PrestaShop](https://www.prestashop-project.org) payment module that takes Zcash through your own copy of this service. It is tested on PrestaShop 9 with the default Hummingbird theme and needs PrestaShop 8.1 or later.

## How it works

1. The customer picks **Pay with Zcash (ZEC)** at checkout and places the order.
2. The module creates the order in its own **Awaiting Zcash payment** state and calls `POST /payment-session` with the order total, the order reference as the label, and PrestaShop's order confirmation page as `return_url`. It stores the issued address and payment ID in its own `zcash_payment_order` table.
3. The customer is sent to the service's [pay page](pay-page.md), which shows the amount, a QR code, and an *Open in wallet* link, and updates live.
4. The service sends a signed webhook each time the address's total changes. The module checks the `X-Signature` HMAC, ignores stale events, and moves the order to **Payment accepted** once the total covers it. That records an order payment for the full amount. A partial payment adds a private order message instead.
5. The pay page sends the customer back to the order confirmation page.

Edge cases the module handles:

- **An order keeps one Zcash address for life.** If the order's total changes after its address was issued, a payment to the old address is noted for you to reconcile rather than completing the order.
- **Funds drop after payment** (an unconfirmed transaction expires, or a reorg): the order goes to **Zcash funds on hold** with a message. The module restores the earlier state only if it put the order on hold itself.
- **Payment for a cancelled, refunded, or failed order** is never applied automatically. Moving an order with a Zcash address to one of those states, or issuing a credit slip for it, flags it for review. Later payments to it are left for you until you clear `needs_review` for the order in `zcash_payment_order`.
- **After a partial payment**, the pay page asks only for the remaining balance. The remainder is based on receipts the service has reported (see `WEBHOOK_REPORT_CONFIRMATIONS`).
- **Overlapping webhook deliveries** for one order are serialized with a MySQL named lock, and the event version is recorded only after the order update succeeds, so a failed update is retried.

If the service has no public pay page (no `PUBLIC_BASE_URL` and no *Pay page base URL* setting), the customer goes straight to the order confirmation page, which shows the address and a wallet link.

## Setup

1. Run the service as in [Deployment](deployment.md), with `ZCASH_WEBHOOK_URL` set to the module's webhook and a random `ZCASH_WEBHOOK_SECRET`:

   ```text
   ZCASH_WEBHOOK_URL=https://shop.example.com/index.php?fc=module&module=zcashpayment&controller=webhook
   ```

   Use your shop's own domain: PrestaShop redirects requests for any other host name, and a redirected webhook arrives as a `GET` that the module rejects. Choose `WEBHOOK_REPORT_CONFIRMATIONS` before the service's first start, as explained in the [WooCommerce setup](woocommerce.md#setup); the default `0` accepts orders from unconfirmed transactions.
2. Expose the pay page publicly as described in [Exposing the page](pay-page.md#exposing-the-page) and set `PUBLIC_BASE_URL`. Set `RETURN_URL_ALLOWED_ORIGINS` to your shop's origin.
3. Zip the `plugins/prestashop/zcashpayment` folder (the zip must contain the `zcashpayment` folder) and upload it under **Modules → Module Manager → Upload a module**, or copy the folder into `modules/` and install it there.
4. Open the module's **Configure** page and set:
   - **Service URL**: where PrestaShop reaches the service, for example `http://zcash-payment-service:8787`. This does not need to be public.
   - **Webhook secret**: the same value as `ZCASH_WEBHOOK_SECRET`.
   - **Pay page base URL**: only if the service does not set `PUBLIC_BASE_URL`.
   - **ZEC per unit of store currency**: only if the shop does not price in ZEC.

   The page also shows the webhook URL to give the service.

The payment option is hidden at checkout until the service URL and secret are set.

### Store currency

Installing the module adds a disabled **ZEC** currency with 8 decimal places. To price in ZEC, enable it under **International → Localization → Currencies**, and check that the module is allowed for it under **Payment → Preferences**. Order totals in ZEC are sent as-is. PrestaShop stores amounts with 6 decimal places, so prices finer than 0.000001 ZEC are not possible.

If the shop prices in another currency, you must enter a fixed rate (ZEC per one unit of store currency) and keep it current yourself; the option is hidden for carts in other currencies when no rate is set. The module does not fetch prices from any third party.

## Testing

Everything below runs without a PrestaShop, wallet, or exchange account, and without real or test coins.

**Unit tests** cover amount conversion, signature checks, webhook parsing, and pay URL handling:

```sh
php plugins/prestashop/tests/unit.php
```

**End-to-end test** (`plugins/prestashop/e2e`) starts MySQL, PrestaShop, and this service with Docker Compose, installs and configures the module, and drives a headless Chromium through a guest checkout:

```sh
cd plugins/prestashop/e2e
npm ci && npx playwright install --with-deps chromium   # once per machine
./run.sh                 # KEEP_STACK=1 leaves it running on http://localhost:8080
```

The service runs on testnet with `ZCASH_DEV_SIMULATED_PAYMENTS=true`, which turns on [`POST /dev/simulate-payment`](api.md#post-devsimulate-payment-testnet-test-hook). The test:

- buys a virtual product and checks that the browser lands on the pay page with the right amount and a QR code;
- simulates a partial payment and checks the pay page shows it, then simulates the rest;
- checks that the browser returns to the confirmation page and that the order is **Payment accepted** with the full amount recorded;
- pays a cancelled order and checks that the payment is held for review and the order stays cancelled;
- checks that the webhook rejects a bad signature with `401` and acknowledges a signed event for an unknown address.

CI runs both on every pull request through `.github/workflows/checks.yml`, which calls `.github/workflows/prestashop-plugin.yml`, and keeps screenshots of each step as a build artifact.

### What the tests do not cover

- **The chain.** Simulated payments skip lightwalletd, trial decryption, and confirmations.
- **Wallet apps.** Nobody scans the QR code or opens the `zcash:` link in a real wallet.
- **Other setups.** Only one PrestaShop version, the default theme, and guest checkout are exercised.
- **Non-ZEC pricing.** The manual exchange rate path has unit tests but no end-to-end run.
- **Email.** The test stack has no mail server.
