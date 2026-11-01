// Drives a real browser through the WooCommerce block checkout, onto the service's pay page, and
// back to the store after payments are simulated through the testnet-only dev endpoint.
import { chromium } from 'playwright';
import assert from 'node:assert/strict';

const env = (name) => {
  const v = process.env[name];
  if (!v) throw new Error(`${name} is required`);
  return v;
};
const shots = process.env.E2E_SCREENSHOT_DIR;
const shot = async (name) => {
  if (shots) await page.screenshot({ path: `${shots}/${name}.png`, fullPage: true });
};
const productId = env('E2E_PRODUCT_ID');
const checkoutUrl = env('E2E_CHECKOUT_URL');
const wpUrl = env('E2E_WP_URL');
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
page.setDefaultTimeout(30_000);
try {
  await page.goto(`${wpUrl}/?add-to-cart=${productId}`);
  await page.goto(checkoutUrl);

  await page.getByLabel('Email address').fill('buyer@example.com');
  await page.getByLabel('First name').fill('Test');
  await page.getByLabel('Last name').fill('Buyer');
  await page.getByLabel('Address', { exact: true }).fill('1 Main St');
  await page.getByLabel('City').fill('Denver');
  const state = page.getByLabel('State', { exact: true });
  if (await state.count()) await state.selectOption({ label: 'Colorado' });
  await page.getByLabel('ZIP Code').fill('80202');

  const method = page.getByLabel('Zcash', { exact: true });
  if (await method.count()) await method.check();

  await shot('1-checkout');
  await page.getByRole('button', { name: 'Place order' }).click();
  await page.waitForURL(`${serviceUrl}/pay/**`);
  const paymentId = new URL(page.url()).pathname.split('/').pop();
  console.log(`pay page: ${page.url()}`);

  await page.locator('svg').first().waitFor();
  await page.getByText('0.5', { exact: false }).first().waitFor();

  await shot('2-pay-page');
  const status = await (await fetch(`${serviceUrl}/payments/${paymentId}`)).json();
  assert.equal(status.status, 'awaiting_payment');
  assert.equal(Number(status.amount), 0.5);
  assert.ok(status.return_url && status.return_url.startsWith(wpUrl), `return_url ${status.return_url}`);

  await simulate(status.address, '0.2');
  await page.locator('#status[data-status="partially_paid"]').waitFor();
  // The page reloads with a request for only the rest, so rescanning cannot overpay.
  await page.locator('a.button[href*="amount=0.3"]').waitFor({ timeout: 20000 });
  await shot('3-partially-paid');
  console.log('partial payment shown on pay page');

  await simulate(status.address, '0.3');
  await page.waitForURL((url) => url.href.startsWith(wpUrl) && url.href.includes('order-received'), {
    timeout: 60_000,
  });
  await page.waitForLoadState('networkidle');
  await shot('4-order-received');
  const url = new URL(page.url());
  const orderId =
    url.searchParams.get('order-received') ?? url.pathname.match(/order-received\/(\d+)/)?.[1];
  assert.ok(orderId, `no order id in ${page.url()}`);
  console.log(`ORDER_ID=${orderId}`);
} catch (e) {
  await page.screenshot({ path: 'failure.png', fullPage: true }).catch(() => {});
  console.error(`failed at ${page.url()}`);
  throw e;
} finally {
  await browser.close();
}
