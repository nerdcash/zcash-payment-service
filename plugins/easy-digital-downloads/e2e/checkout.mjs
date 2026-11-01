// Drives a real browser through the Easy Digital Downloads checkout, onto the service's pay page,
// and back to the store's receipt after payments are simulated through the testnet-only dev
// endpoint.
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
const downloadId = env('E2E_DOWNLOAD_ID');
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
try {
  await page.goto(`${wpUrl}/?p=${downloadId}&post_type=download`);
  // EDD adds to the cart over AJAX.
  const added = page.waitForResponse((r) => r.url().includes('admin-ajax.php') && (r.request().postData() ?? '').includes('action=edd_add_to_cart') && r.ok());
  await page.locator('.edd-add-to-cart:visible').first().click();
  await added;
  await page.goto(checkoutUrl);
  await page.locator('#edd-email').fill('buyer@example.com');
  await page.locator('#edd-first').fill('Zoe');
  await page.locator('#edd-last').fill('Cash').catch(() => {});
  await page.getByText('0.50000000', { exact: false }).first().waitFor();
  await shot('1-checkout');

  await page.locator('#edd-purchase-button').click();
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
  assert.ok(status.return_url && status.return_url.startsWith(wpUrl), `return_url ${status.return_url}`);
  const orderId = new URL(status.return_url).searchParams.get('id');
  assert.ok(orderId, `no order id in ${status.return_url}`);

  await simulate(status.address, '0.2');
  await page.locator('#status[data-status="partially_paid"]').waitFor();
  // The page reloads with a request for only the rest, so rescanning cannot overpay.
  await page.locator('a.button[href*="amount=0.3"]').waitFor({ timeout: 20000 });
  await shot('3-partially-paid');
  console.log('partial payment shown on pay page');

  await simulate(status.address, '0.3');
  await page.waitForURL((url) => url.href.startsWith(wpUrl), { timeout: 60_000 });
  await page.waitForLoadState('networkidle');
  // The webhook can trail the pay page's redirect by a moment; reload until the receipt is paid.
  for (let i = 0; i < 15; i++) {
    if ((await page.locator('.zcash-payment-details').count()) === 0) break;
    await page.waitForTimeout(2000);
    await page.reload();
  }
  await page.getByText('Completed', { exact: false }).first().waitFor({ timeout: 10_000 });
  await shot('4-receipt');
  console.log(`ORDER_ID=${orderId}`);
} catch (e) {
  await page.screenshot({ path: 'failure.png', fullPage: true }).catch(() => {});
  console.error(`failed at ${page.url()}`);
  throw e;
} finally {
  await browser.close();
}
