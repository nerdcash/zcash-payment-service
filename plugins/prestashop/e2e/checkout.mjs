// Drives a real browser through the PrestaShop checkout as a guest, onto the service's pay page,
// and back to the store's order confirmation after payments are simulated through the
// testnet-only dev endpoint.
import { chromium } from 'playwright';
import assert from 'node:assert/strict';

const env = (name) => {
  const v = process.env[name];
  if (!v) throw new Error(`${name} is required`);
  return v;
};
// E2E_STOP_AT_PAY_PAGE=1 leaves an order unpaid at the pay page.
const shots = process.env.E2E_STOP_AT_PAY_PAGE ? undefined : process.env.E2E_SCREENSHOT_DIR;
const shot = async (name) => {
  if (shots) await page.screenshot({ path: `${shots}/${name}.png`, fullPage: true });
};
const productId = env('E2E_PRODUCT_ID');
const psUrl = env('E2E_PS_URL');
const serviceUrl = env('E2E_SERVICE_URL');

const simulate = async (address, amount) => {
  const res = await fetch(`${serviceUrl}/dev/simulate-payment`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ address, amount }),
  });
  assert.equal(res.status, 200, `simulate-payment: ${await res.text()}`);
};

const browser = await chromium.launch();
const page = await browser.newPage();
try {
  await page.goto(`${psUrl}/index.php?id_product=${productId}&controller=product`);
  const added = page.waitForResponse((r) => r.url().includes('controller=cart') || (r.url().includes('/cart') && r.request().method() === 'POST'));
  await page.locator('#add-to-cart-or-refresh [data-button-action="add-to-cart"]').click();
  await added;
  await page.goto(`${psUrl}/index.php?controller=order`);

  // Personal information, as a guest.
  const customer = page.locator('#customer-form');
  await customer.locator('input[name="firstname"]').fill('Zoe');
  await customer.locator('input[name="lastname"]').fill('Cash');
  await customer.locator('input[name="email"]').fill('buyer@example.com');
  for (const name of ['customer_privacy', 'psgdpr']) {
    const box = customer.locator(`input[name="${name}"]`);
    if (await box.count()) await box.check();
  }
  await customer.locator('button[name="continue"]').click();

  // Invoice address (the product is virtual, so there is no delivery step).
  await page.locator('input[name="address1"]').fill('1 Test Street');
  await page.locator('input[name="city"]').fill('London');
  await page.locator('input[name="postcode"]').fill('SW1A 1AA');
  await page.locator('button[name="confirm-addresses"]').click();

  await page.locator('input[data-module-name="zcashpayment"]').check();
  await page.getByText('0.50000000').filter({ visible: true }).first().waitFor();
  await shot('1-checkout');
  await page.getByRole('button', { name: 'Place Order' }).click();

  await page.waitForURL((url) => url.href.startsWith(serviceUrl) && url.pathname.startsWith('/pay/'), {
    timeout: 30_000,
  });
  const paymentId = new URL(page.url()).pathname.split('/').pop();
  console.log(`pay page: ${page.url()}`);

  await page.locator('svg').first().waitFor();
  await page.getByText('0.5', { exact: false }).first().waitFor();
  await shot('2-pay-page');
  const status = await (await fetch(`${serviceUrl}/payments/${paymentId}`)).json();
  assert.equal(status.status, 'awaiting_payment');
  assert.equal(Number(status.amount), 0.5);
  assert.ok(status.return_url && status.return_url.startsWith(psUrl), `return_url ${status.return_url}`);
  const orderId = new URL(status.return_url).searchParams.get('id_order');
  assert.ok(orderId, `no order id in ${status.return_url}`);

  if (process.env.E2E_STOP_AT_PAY_PAGE) {
    console.log(`ORDER_ID=${orderId}`);
    process.exit(0);
  }

  await simulate(status.address, '0.2');
  await page.locator('#status[data-status="partially_paid"]').waitFor();
  // The page reloads with a request for only the rest, so rescanning cannot overpay.
  await page.locator('a.button[href*="amount=0.3"]').waitFor({ timeout: 20000 });
  await shot('3-partially-paid');
  console.log('partial payment shown on pay page');

  await simulate(status.address, '0.3');
  await page.waitForURL((url) => url.href.startsWith(psUrl), { timeout: 60_000 });
  await page.waitForLoadState('networkidle');
  // The webhook can trail the pay page's redirect by a moment; reload until the order is paid.
  for (let i = 0; i < 15; i++) {
    if ((await page.locator('.zcash-payment-complete').count()) > 0) break;
    await page.waitForTimeout(2000);
    await page.reload();
  }
  await page.locator('.zcash-payment-complete').waitFor({ timeout: 10_000 });
  await shot('4-confirmation');
  console.log(`ORDER_ID=${orderId}`);
} catch (e) {
  await page.screenshot({ path: 'failure.png', fullPage: true }).catch(() => {});
  console.error(`failed at ${page.url()}`);
  throw e;
} finally {
  await browser.close();
}
