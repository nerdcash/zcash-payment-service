// Drives a real browser through a Medusa storefront checkout, onto the service's pay page, and back
// to the store after payments are simulated through the testnet-only dev endpoint. Then checks the
// webhook's edge cases (hold and recovery, late payment to a canceled order, bad signatures)
// against the same running store.
import { chromium } from 'playwright';
import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';

const storeUrl = process.env.E2E_STORE_URL ?? 'http://localhost:9000';
const serviceUrl = process.env.E2E_SERVICE_URL ?? 'http://localhost:8787';
const secret = 'e2e-webhook-secret';
const shots = process.env.E2E_SCREENSHOT_DIR;
let page;
const shot = async (name, p = page) => {
  if (shots) await p.screenshot({ path: `${shots}/${name}.png`, fullPage: true });
};
// The admin scrolls inside a panel, so bring the plugin's widget into view first.
const adminShot = async (name, p) => {
  const widget = p.getByText('Zcash payment', { exact: true });
  if (await widget.count()) await widget.first().scrollIntoViewIfNeeded();
  await shot(name, p);
};
const sleep = (ms) => new Promise((r) => setTimeout(r, ms));
const until = async (what, fn, tries = 40) => {
  for (let i = 0; i < tries; i++) {
    const v = await fn();
    if (v) return v;
    await sleep(1500);
  }
  throw new Error(`timed out waiting for ${what}`);
};

const simulate = async (address, amount) => {
  const res = await fetch(`${serviceUrl}/dev/simulate-payment`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ address, amount }),
  });
  assert.equal(res.status, 200, `simulate-payment: ${await res.text()}`);
};

const login = await fetch(`${storeUrl}/auth/user/emailpass`, {
  method: 'POST',
  headers: { 'content-type': 'application/json' },
  body: JSON.stringify({ email: 'admin@example.com', password: 'e2e-admin-password' }),
});
assert.equal(login.status, 200, 'admin login');
const { token } = await login.json();
const admin = async (path, init = {}) => {
  const res = await fetch(`${storeUrl}${path}`, {
    ...init,
    headers: { 'content-type': 'application/json', authorization: `Bearer ${token}`, ...(init.headers ?? {}) },
  });
  const body = await res.json();
  assert.ok(res.ok, `${path}: ${res.status} ${JSON.stringify(body)}`);
  return body;
};
const orderFields =
  'id,status,payment_status,metadata,payment_collections.payments.id,payment_collections.payments.captured_at,' +
  'payment_collections.payments.captures.id,payment_collections.payment_sessions.id,payment_collections.payment_sessions.data';
const getOrder = async (id) => (await admin(`/admin/orders/${id}?fields=${orderFields}`)).order;
const sessionOf = (order) => order.payment_collections[0].payment_sessions[0];
const captures = (order) => order.payment_collections.flatMap((c) => c.payments).flatMap((p) => p.captures ?? []).length;

const webhook = async (body, signature) =>
  fetch(`${storeUrl}/hooks/zcash`, {
    method: 'POST',
    headers: {
      'content-type': 'application/json',
      'x-signature': signature ?? createHmac('sha256', secret).update(body).digest('hex'),
    },
    body,
  });
const observation = (address, version, total) =>
  JSON.stringify({
    event_id: `zcash:999:v${version}`,
    payment_source: 'zcash',
    address,
    total_received: total,
    observed_at: new Date().toISOString(),
  });

const browser = await chromium.launch();
page = await browser.newPage();
page.setDefaultTimeout(30_000);
let orderId;
try {
  console.log('--- browser checkout');
  await page.goto(`${storeUrl}/demo-store`);
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
  assert.ok(status.return_url?.startsWith(`${storeUrl}/demo-store/order`), `return_url ${status.return_url}`);

  await simulate(status.address, '0.2');
  await page.locator('#status[data-status="partially_paid"]').waitFor();
  await page.locator('a.button[href*="amount=0.3"]').waitFor({ timeout: 20_000 });
  await shot('3-partially-paid');
  console.log('partial payment shown on pay page');

  await simulate(status.address, '0.3');
  await page.waitForURL((url) => url.href.startsWith(`${storeUrl}/demo-store/order`), { timeout: 60_000 });
  await page.locator('#payment-status', { hasText: 'captured' }).waitFor({ timeout: 60_000 });
  await shot('4-order-confirmed');
  orderId = await page.locator('#order-id').getAttribute('data-id');
  console.log(`order ${orderId}`);

  let order = await getOrder(orderId);
  assert.equal(order.payment_status, 'captured');
  assert.equal(captures(order), 1);
  const address = sessionOf(order).data.address;
  assert.equal(address, status.address);

  // The admin, with the plugin's widget, for screenshots.
  const adminPage = await browser.newPage({ viewport: { width: 1280, height: 1000 } });
  await adminPage.goto(`${storeUrl}/app/login`);
  await adminPage.locator('input[name="email"]').waitFor();
  await adminPage.locator('input[name="email"]').fill('admin@example.com');
  await adminPage.locator('input[name="password"]').fill('e2e-admin-password');
  await adminPage.getByRole('button', { name: /continue|sign in|log in/i }).click();
  await adminPage.waitForURL((u) => !u.pathname.includes('login'), { timeout: 30_000 });
  await adminPage.goto(`${storeUrl}/app/orders/${orderId}`);
  await adminPage.getByText('Zcash T-shirt').first().waitFor();
  await adminShot('5-admin-order-paid', adminPage);

  console.log('--- funds drop after payment: order is held');
  let res = await webhook(observation(address, 100, '0.10000000'));
  assert.equal(res.status, 200, await res.text());
  order = await getOrder(orderId);
  assert.match(order.metadata?.zcash_payment_hold ?? '', /dropped to 0\.10000000 of 0\.50000000/);
  await adminPage.reload();
  await adminPage.getByText('On hold: do not fulfill').waitFor();
  await adminShot('6-admin-order-held', adminPage);

  console.log('--- a stale event is ignored');
  res = await webhook(observation(address, 99, '0.50000000'));
  assert.equal((await res.json()).ignored, 'stale');
  assert.ok((await getOrder(orderId)).metadata?.zcash_payment_hold, 'stale event released the hold');

  console.log('--- funds recover: hold released without capturing again');
  res = await webhook(observation(address, 101, '0.50000000'));
  assert.equal(res.status, 200, await res.text());
  order = await getOrder(orderId);
  assert.ok(!order.metadata?.zcash_payment_hold, 'hold not released');
  assert.equal(order.payment_status, 'captured');
  assert.equal(captures(order), 1, 'recovery captured again');
  await adminPage.reload();
  await adminPage.getByText('Zcash T-shirt').first().waitFor();
  await adminShot('7-admin-order-released', adminPage);
  await adminPage.close();

  console.log('--- a late payment never revives a canceled order');
  await page.goto(`${storeUrl}/demo-store`);
  await page.getByRole('button', { name: 'Place order' }).click();
  await page.waitForURL(`${serviceUrl}/pay/**`);
  const late = await (await fetch(`${serviceUrl}/payments/${new URL(page.url()).pathname.split('/').pop()}`)).json();
  const { orders } = await admin(`/admin/orders?fields=id,created_at&order=-created_at&limit=1`);
  const lateId = orders[0].id;
  assert.equal(sessionOf(await getOrder(lateId)).data.address, late.address);
  await admin(`/admin/orders/${lateId}/cancel`, { method: 'POST' });
  await simulate(late.address, '0.5');
  const lateOrder = await until('late payment review', async () => {
    const o = await getOrder(lateId);
    return o.metadata?.zcash_payment_review ? o : null;
  });
  assert.equal(lateOrder.status, 'canceled');
  assert.notEqual(lateOrder.payment_status, 'captured');
  assert.equal(captures(lateOrder), 0);

  console.log('--- webhook rejects a bad signature');
  const body = observation(address, 200, '9');
  assert.equal((await webhook(body, '00')).status, 401);
  assert.equal((await webhook(body, createHmac('sha256', 'wrong').update(body).digest('hex'))).status, 401);

  console.log('--- signed webhook for an unknown address is acknowledged and ignored');
  res = await webhook(observation('utest1unknown', 1, '1'));
  assert.equal(res.status, 200);
  assert.equal((await res.json()).ignored, 'unknown address');

  console.log('--- the service refuses a return_url outside RETURN_URL_ALLOWED_ORIGINS');
  res = await fetch(`${serviceUrl}/payment-session`, {
    method: 'POST',
    headers: { 'content-type': 'application/json' },
    body: JSON.stringify({ payment_source: 'zcash', amount: '0.1', return_url: 'https://evil.example/' }),
  });
  assert.equal(res.status, 400);

  console.log('e2e passed');
} catch (e) {
  for (const [i, p] of browser.contexts().flatMap((c) => c.pages()).entries()) {
    await p.screenshot({ path: `${shots ?? "."}/failure-${i}.png`, fullPage: true }).catch(() => {});
    console.error(`failed at ${p.url()}`);
  }
  throw e;
} finally {
  await browser.close();
}
