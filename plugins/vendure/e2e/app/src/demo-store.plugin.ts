import { Controller, Get, Header } from "@nestjs/common"
import { PluginCommonModule, VendurePlugin } from "@vendure/core"

// A deliberately tiny storefront for the e2e test. It shows the few Shop API calls a storefront
// needs to take Zcash: add the payment, then send the customer to its pay_url (the Shop API shows
// only the metadata's `public` part).
const SCRIPT = String.raw`
async function gql(query, variables) {
  const r = await fetch("/shop-api", { method: "POST", credentials: "include",
    headers: { "content-type": "application/json" }, body: JSON.stringify({ query, variables }) });
  const j = await r.json();
  if (j.errors) throw new Error(j.errors[0].message);
  return j.data;
}
const must = (result) => { if (result && result.errorCode) throw new Error(result.message); return result; };

async function checkout(event) {
  event.preventDefault();
  const f = new FormData(event.target);
  const status = document.getElementById("status");
  status.textContent = "Placing order…";
  try {
    const { product } = await gql("query { product(slug: \"zcash-t-shirt\") { variants { id } } }");
    must((await gql("mutation($id: ID!) { addItemToOrder(productVariantId: $id, quantity: 1) { ... on Order { id } ... on ErrorResult { errorCode message } } }", { id: product.variants[0].id })).addItemToOrder);
    must((await gql("mutation($c: CreateCustomerInput!) { setCustomerForOrder(input: $c) { ... on Order { id } ... on ErrorResult { errorCode message } } }",
      { c: { emailAddress: f.get("email"), firstName: f.get("first_name"), lastName: f.get("last_name") } })).setCustomerForOrder);
    must((await gql("mutation($a: CreateAddressInput!) { setOrderShippingAddress(input: $a) { ... on Order { id } ... on ErrorResult { errorCode message } } }",
      { a: { fullName: f.get("first_name") + " " + f.get("last_name"), streetLine1: f.get("address_1"), city: f.get("city"), postalCode: f.get("postal_code"), countryCode: "US" } })).setOrderShippingAddress);
    const { eligibleShippingMethods } = await gql("query { eligibleShippingMethods { id } }");
    must((await gql("mutation($id: [ID!]!) { setOrderShippingMethod(shippingMethodId: $id) { ... on Order { id } ... on ErrorResult { errorCode message } } }", { id: [eligibleShippingMethods[0].id] })).setOrderShippingMethod);
    must((await gql("mutation { transitionOrderToState(state: \"ArrangingPayment\") { ... on Order { id } ... on OrderStateTransitionError { errorCode message transitionError } } }")).transitionOrderToState);
    // The Zcash payment is Authorized at once, so this places the order. The webhook settles it.
    const order = must((await gql("mutation { addPaymentToOrder(input: { method: \"zcash-testnet\", metadata: {} }) { ... on Order { code payments { method metadata } } ... on ErrorResult { errorCode message } } }")).addPaymentToOrder);
    const payment = order.payments.find((p) => p.method === "zcash-testnet");
    location.href = (payment.metadata.public ?? payment.metadata).pay_url || ("/demo-store/order?code=" + order.code);
  } catch (e) { status.textContent = "Error: " + e.message; }
}

async function showOrder() {
  const code = new URLSearchParams(location.search).get("code");
  const el = document.getElementById("order");
  for (;;) {
    const { orderByCode: o } = await gql("query($c: String!) { orderByCode(code: $c) { id code state totalWithTax payments { state } } }", { c: code });
    if (!o) { el.textContent = "Order not found."; return; }
    el.innerHTML = "<p>Order <b id=order-code data-id=" + o.id + ">" + o.code + "</b></p>"
      + "<p>Total: $" + (o.totalWithTax / 100).toFixed(2) + "</p>"
      + "<p>Status: <b id=order-state>" + o.state + "</b></p>";
    if (o.state === "PaymentSettled") return;
    await new Promise((r) => setTimeout(r, 1500));
  }
}
`

const page = (body: string, onload = "") => `<!doctype html><html><head><meta charset=utf-8><title>Zcash demo store</title>
<style>body{font-family:system-ui,sans-serif;max-width:640px;margin:2rem auto;padding:0 1rem}
label{display:block;margin:.4rem 0}input{margin-left:.5rem}button{padding:.6rem 1.4rem;font-size:1rem;margin-top:1rem}
.product{border:1px solid #ccc;border-radius:8px;padding:1rem;margin-bottom:1rem}</style></head>
<body><h1>Zcash demo store</h1>${body}<script>${SCRIPT};${onload}</script></body></html>`

@Controller("demo-store")
export class DemoStoreController {
  @Get()
  @Header("content-type", "text/html; charset=utf-8")
  index() {
    return page(`<div class=product><h2>Zcash T-shirt</h2><p>$20.00</p></div>
<form onsubmit="checkout(event)">
<label>Email<input name=email value="buyer@example.com"></label>
<label>First name<input name=first_name value="Test"></label>
<label>Last name<input name=last_name value="Buyer"></label>
<label>Street<input name=address_1 value="1 Main St"></label>
<label>City<input name=city value="Denver"></label>
<label>Postal code<input name=postal_code value="80202"></label>
<p>Payment: <b>Zcash</b></p>
<button type=submit>Place order</button></form><p id=status></p>`)
  }

  @Get("order")
  @Header("content-type", "text/html; charset=utf-8")
  order() {
    return page(`<h2>Thank you!</h2><div id=order>Loading…</div>`, "showOrder()")
  }
}

@VendurePlugin({ imports: [PluginCommonModule], controllers: [DemoStoreController] })
export class DemoStorePlugin {}
