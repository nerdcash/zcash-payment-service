// A deliberately tiny storefront for the e2e test. A real store would use the Next.js starter;
// this shows the few Store API calls a storefront needs to take Zcash.
export const STORE_SCRIPT = String.raw`
const api = (path, init = {}) => fetch(path, {
  ...init,
  headers: { "content-type": "application/json", "x-publishable-api-key": CONFIG.key, ...(init.headers || {}) },
}).then(async (r) => { const j = await r.json(); if (!r.ok) throw new Error(j.message || r.status); return j; });

async function checkout(event) {
  event.preventDefault();
  const f = new FormData(event.target);
  const status = document.getElementById("status");
  status.textContent = "Placing order…";
  try {
    const address = { first_name: f.get("first_name"), last_name: f.get("last_name"), address_1: f.get("address_1"),
      city: f.get("city"), postal_code: f.get("postal_code"), country_code: "us" };
    let { cart } = await api("/store/carts", { method: "POST", body: JSON.stringify({ region_id: CONFIG.region }) });
    await api("/store/carts/" + cart.id + "/line-items", { method: "POST", body: JSON.stringify({ variant_id: CONFIG.variant, quantity: 1 }) });
    await api("/store/carts/" + cart.id, { method: "POST", body: JSON.stringify({ email: f.get("email"), shipping_address: address, billing_address: address }) });
    const { shipping_options } = await api("/store/shipping-options?cart_id=" + cart.id);
    ({ cart } = await api("/store/carts/" + cart.id + "/shipping-methods", { method: "POST", body: JSON.stringify({ option_id: shipping_options[0].id }) }));
    const { payment_collection } = await api("/store/payment-collections", { method: "POST", body: JSON.stringify({ cart_id: cart.id }) });
    const { payment_collection: pc } = await api("/store/payment-collections/" + payment_collection.id + "/payment-sessions", {
      method: "POST", body: JSON.stringify({ provider_id: f.get("provider") }) });
    const session = pc.payment_sessions.find((s) => s.provider_id === f.get("provider"));
    // Complete the cart first: the session is pending_authorization, so this creates the order
    // and keeps the Zcash address tied to it for life. The webhook captures payment later.
    const done = await api("/store/carts/" + cart.id + "/complete", { method: "POST" });
    if (done.type !== "order") throw new Error(done.error?.message || "cart not completed");
    localStorage.setItem("order:" + session.id, done.order.id);
    location.href = session.data.pay_url || ("/demo-store/order?session_id=" + session.id);
  } catch (e) { status.textContent = "Error: " + e.message; }
}

async function showOrder() {
  const sessionId = new URLSearchParams(location.search).get("session_id");
  const orderId = localStorage.getItem("order:" + sessionId);
  const el = document.getElementById("order");
  if (!orderId) { el.textContent = "Order not found in this browser."; return; }
  for (;;) {
    const { order } = await api("/store/orders/" + orderId + "?fields=id,display_id,payment_status,status,total,email");
    el.innerHTML = "<p>Order <b id=order-id data-id=" + order.id + ">#" + order.display_id + "</b></p>"
      + "<p>Total: $" + order.total + "</p>"
      + "<p>Payment: <b id=payment-status>" + order.payment_status + "</b></p>";
    if (order.payment_status === "captured") return;
    await new Promise((r) => setTimeout(r, 1500));
  }
}
`

const css = `body{font-family:system-ui,sans-serif;max-width:640px;margin:2rem auto;padding:0 1rem}
label{display:block;margin:.4rem 0}input{margin-left:.5rem}button{padding:.6rem 1.4rem;font-size:1rem;margin-top:1rem}
.product{border:1px solid #ccc;border-radius:8px;padding:1rem;margin-bottom:1rem}`

export function storePage(config: { key: string; region: string; variant: string }, body: string, onload: string) {
  return `<!doctype html><html><head><meta charset=utf-8><title>Zcash demo store</title><style>${css}</style></head>
<body><h1>Zcash demo store</h1>${body}
<script>const CONFIG=${JSON.stringify(config)};${STORE_SCRIPT};${onload}</script></body></html>`
}
