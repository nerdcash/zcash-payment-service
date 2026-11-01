// Drives a real browser through a Vendure storefront checkout, onto the service's pay page, and
// back to the store after payments are simulated through the testnet-only dev endpoint. Then checks
// the webhook's edge cases (hold and recovery, fulfillment blocked while held, late payment to a
// cancelled order, bad signatures) against the same running store.
import { chromium } from 'playwright';
import assert from 'node:assert/strict';
import { createHmac } from 'node:crypto';

const storeUrl = process.env.E2E_STORE_URL ?? 'http://localhost:3000';
const serviceUrl = process.env.E2E_SERVICE_URL ?? 'http://localhost:8787';
const secret = 'e2e-webhook-secret';
const shots = process.env.E2E_SCREENSHOT_DIR;
let page;
const shot = async (name, p = page) => {
  if (shots) await p.screenshot({ path: `${shots}/${name}.png`, fullPage: true });
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

let token;
const adminRaw = async (query, variables = {}) => {
  const res = await fetch(`${storeUrl}/admin-api`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', authorization: `Bearer ${token}` },
    body: JSON.stringify({ query, variables }),
  });
  return res.json();
};
const admin = async (query, variables = {}) => {
  const res = await fetch(`${storeUrl}/admin-api`, {
    method: 'POST',
    headers: { 'content-type': 'application/json', ...(token ? { authorization: `Bearer ${token}` } : {}) },
    body: JSON.stringify({ query, variables }),
  });
  const body = await res.json();
  assert.ok(res.ok && !body.errors, `admin-api: ${res.status} ${JSON.stringify(body.errors ?? body)}`);
  token = res.headers.get('vendure-auth-token') ?? token;
  return body.data;
};
const login = await admin(
  'mutation { login(username: "superadmin", password: "e2e-admin-password") { ... on CurrentUser { id } ... on ErrorResult { message } } }'
);
assert.ok(login.login.id, 'admin login');

const ORDER = `id code state
  lines { id quantity }
  payments { id state transactionId metadata }
  fulfillments { id state }
  history(options: { take: 100 }) { items { type data } }`;
const getOrder = async (id) => (await admin(`query($id: ID!) { order(id: $id) { ${ORDER} } }`, { id })).order;
const orderByAddress = async (address) => {
  const { orders } = await admin(
    'query { orders(options: { take: 20, sort: { createdAt: DESC } }) { items { id payments { transactionId } } } }'
  );
  const o = orders.items.find((x) => x.payments.some((p) => p.transactionId === address));
  assert.ok(o, `order for ${address}`);
  return getOrder(o.id);
};
const notes = (order) => order.history.items.filter((h) => h.type === 'ORDER_NOTE').map((h) => h.data.note);
const settled = (order) => order.payments.filter((p) => p.state === 'Settled').length;
// Returns the Fulfillment, an error result, or { errors } when the mutation was refused outright.
const fulfill = (order) =>
  adminRaw(
    `mutation($input: FulfillOrderInput!) { addFulfillmentToOrder(input: $input) {
      __typename ... on Fulfillment { id state } ... on ErrorResult { errorCode message }
      ... on FulfillmentStateTransitionError { transitionError }
      ... on CreateFulfillmentError { fulfillmentHandlerError } } }`,
    {
      input: {
        lines: order.lines.map((l) => ({ orderLineId: l.id, quantity: l.quantity })),
        handler: {
          code: 'manual-fulfillment',
          arguments: [
            { name: 'method', value: 'Courier' },
            { name: 'trackingCode', value: 'E2E-1' },
          ],
        },
      },
    }
  ).then((b) => (b.errors ? { errors: b.errors } : b.data.addFulfillmentToOrder));

const webhook = async (body, signature) =>
  fetch(`${storeUrl}/zcash-payments/webhook`, {
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

const checkout = async () => {
  await page.goto(`${storeUrl}/demo-store`);
  await page.getByRole('button', { name: 'Place order' }).click();
  await page.waitForURL(`${serviceUrl}/pay/**`);
  return (await fetch(`${serviceUrl}/payments/${new URL(page.url()).pathname.split('/').pop()}`)).json();
};

const browser = await chromium.launch();
page = await browser.newPage();
page.setDefaultTimeout(30_000);
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
  const address = status.address;

  let order = await orderByAddress(address);
  assert.equal(order.state, 'PaymentAuthorized');

  await simulate(address, '0.2');
  await page.locator('#status[data-status="partially_paid"]').waitFor();
  await page.locator('a.button[href*="amount=0.3"]').waitFor({ timeout: 20_000 });
  await shot('3-partially-paid');
  console.log('partial payment shown on pay page');

  await simulate(address, '0.3');
  await page.waitForURL((url) => url.href.startsWith(`${storeUrl}/demo-store/order`), { timeout: 60_000 });
  await page.locator('#order-state', { hasText: 'PaymentSettled' }).waitFor({ timeout: 60_000 });
  await shot('4-order-confirmed');
  order = await getOrder(order.id);
  console.log(`order ${order.code}`);
  assert.equal(order.state, 'PaymentSettled');
  assert.equal(settled(order), 1);

  // The admin, for screenshots of the order's payment and history.
  const adminPage = await browser.newPage({ viewport: { width: 1280, height: 1100 } });
  await adminPage.goto(`${storeUrl}/admin/login`);
  await adminPage.locator('input[type="text"], input[name="username"]').first().fill('superadmin');
  await adminPage.locator('input[type="password"]').first().fill('e2e-admin-password');
  await adminPage.getByRole('button', { name: /log in|sign in/i }).click();
  await adminPage.waitForURL((u) => !u.pathname.includes('login'), { timeout: 30_000 });
  const adminShot = async (name, text) => {
    await adminPage.goto(`${storeUrl}/admin/orders/${order.id}`);
    await adminPage.getByText(order.code).first().waitFor();
    if (text) await adminPage.getByText(text).first().scrollIntoViewIfNeeded();
    await sleep(1000);
    await shot(name, adminPage);
  };
  await adminShot('5-admin-order-paid');

  console.log('--- funds drop after payment: order is held and cannot be fulfilled');
  let res = await webhook(observation(address, 100, '0.10000000'));
  assert.equal(res.status, 200, await res.text());
  order = await getOrder(order.id);
  assert.ok(notes(order).some((n) => /ON HOLD.*dropped to 0\.10000000 of 0\.50000000/.test(n)), 'hold note');
  const blocked = await fulfill(order);
  assert.notEqual(blocked.__typename, 'Fulfillment', 'held order was fulfilled');
  assert.equal(blocked.__typename, 'CreateFulfillmentError', JSON.stringify(blocked));
  assert.match(JSON.stringify(blocked), /on hold/i);
  const afterBlock = await getOrder(order.id);
  assert.equal(afterBlock.fulfillments?.length ?? 0, 0, `fulfillment left behind: ${JSON.stringify([blocked, afterBlock.fulfillments])}`);
  await adminShot('6-admin-order-held', 'ON HOLD');

  console.log('--- a stale event is ignored');
  res = await webhook(observation(address, 99, '0.50000000'));
  assert.equal((await res.json()).ignored, 'stale');

  console.log('--- funds recover: hold released without settling or fulfilling again');
  res = await webhook(observation(address, 101, '0.50000000'));
  assert.equal(res.status, 200, await res.text());
  order = await getOrder(order.id);
  assert.ok(notes(order).some((n) => /Hold released/.test(n)), 'release note');
  assert.equal(settled(order), 1, 'recovery settled again');
  const fulfillment = await fulfill(order);
  assert.equal(fulfillment.__typename, 'Fulfillment', JSON.stringify(fulfillment));
  res = await webhook(observation(address, 102, '0.60000000'));
  assert.equal(res.status, 200);
  order = await getOrder(order.id);
  assert.equal(settled(order), 1);
  assert.equal(order.fulfillments.length, 1, 'fulfillment ran twice');
  await adminShot('7-admin-order-released', 'Hold released');
  await adminPage.close();

  console.log('--- an administrator cannot settle an unpaid Zcash payment');
  const late = await checkout();
  let lateOrder = await orderByAddress(late.address);
  const settle = await admin(
    'mutation($id: ID!) { settlePayment(id: $id) { __typename ... on ErrorResult { message } ... on SettlePaymentError { paymentErrorMessage } } }',
    { id: lateOrder.payments[0].id }
  );
  assert.equal(settle.settlePayment.__typename, 'SettlePaymentError', JSON.stringify(settle));
  lateOrder = await getOrder(lateOrder.id);
  assert.equal(lateOrder.payments[0].state, 'Authorized', 'a refused settlement changed the payment state');

  console.log('--- a late payment after cancellation is flagged for refund, not settled');
  const cancel = await admin(
    'mutation($id: ID!) { cancelOrder(input: { orderId: $id }) { __typename ... on Order { state } ... on ErrorResult { message } } }',
    { id: lateOrder.id }
  );
  assert.equal(cancel.cancelOrder.state, 'Cancelled', JSON.stringify(cancel));
  await simulate(late.address, '0.5');
  lateOrder = await until('late payment review', async () => {
    const o = await getOrder(lateOrder.id);
    return notes(o).some((n) => /after the order or payment was closed/.test(n)) ? o : null;
  });
  assert.equal(lateOrder.state, 'Cancelled');
  assert.equal(settled(lateOrder), 0);
  const settleFlagged = await admin(
    'mutation($id: ID!) { settlePayment(id: $id) { __typename ... on ErrorResult { message } ... on SettlePaymentError { paymentErrorMessage } } }',
    { id: lateOrder.payments[0].id }
  );
  assert.notEqual(settleFlagged.settlePayment.__typename, 'Payment', JSON.stringify(settleFlagged));
  assert.equal(settled(await getOrder(lateOrder.id)), 0);

  console.log('--- a payment settles only once it has REQUIRED_CONFIRMATIONS');
  const conf = await checkout();
  const confOrder = await orderByAddress(conf.address);
  const txid = 'ef'.repeat(32);
  const simulateDepth = async (confirmations) => {
    const r = await fetch(`${serviceUrl}/dev/simulate-payment`, {
      method: 'POST',
      headers: { 'content-type': 'application/json' },
      body: JSON.stringify({ address: conf.address, amount: '0.5', txid, confirmations }),
    });
    assert.equal(r.status, 200, `simulate-payment: ${await r.text()}`);
  };
  await simulateDepth(0);
  let pending = await until('pending-confirmation note', async () => {
    const o = await getOrder(confOrder.id);
    return notes(o).some((n) => /pending confirmation/.test(n)) ? o : null;
  });
  assert.equal(settled(pending), 0, 'settled from the mempool');
  const settleEarly = await admin(
    'mutation($id: ID!) { settlePayment(id: $id) { __typename ... on ErrorResult { message } ... on SettlePaymentError { paymentErrorMessage } } }',
    { id: pending.payments[0].id }
  );
  assert.equal(settleEarly.settlePayment.__typename, 'SettlePaymentError', JSON.stringify(settleEarly));
  await simulateDepth(1);
  // Give the webhook time to be delivered and applied.
  await new Promise((r) => setTimeout(r, 6000));
  pending = await getOrder(confOrder.id);
  assert.equal(settled(pending), 0, 'settled at 1 of 2 confirmations');
  await simulateDepth(2);
  await until('settlement at 2 confirmations', async () => {
    const o = await getOrder(confOrder.id);
    return settled(o) === 1 ? o : null;
  });

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
    await p.screenshot({ path: `${shots ?? '.'}/failure-${i}.png`, fullPage: true }).catch(() => {});
    console.error(`failed at ${p.url()}`);
  }
  throw e;
} finally {
  await browser.close();
}
