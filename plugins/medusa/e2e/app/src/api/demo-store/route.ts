import type { MedusaRequest, MedusaResponse } from "@medusajs/framework/http"
import { ContainerRegistrationKeys } from "@medusajs/framework/utils"
import { storePage } from "./page"

export async function storeConfig(req: MedusaRequest) {
  const query = req.scope.resolve(ContainerRegistrationKeys.QUERY)
  const { data: keys } = await query.graph({ entity: "api_key", fields: ["token"], filters: { type: "publishable" } })
  const { data: regions } = await query.graph({ entity: "region", fields: ["id"] })
  const { data: variants } = await query.graph({ entity: "product_variant", fields: ["id"] })
  return { key: keys[0].token, region: regions[0].id, variant: variants[0].id }
}

export async function GET(req: MedusaRequest, res: MedusaResponse) {
  const body = `<div class=product><h2>Zcash T-shirt</h2><p>$20.00 · free shipping</p></div>
<form id=checkout>
<label>Email<input name=email value="buyer@example.com" required></label>
<label>First name<input name=first_name value="Test" required></label>
<label>Last name<input name=last_name value="Buyer" required></label>
<label>Address<input name=address_1 value="1 Main St" required></label>
<label>City<input name=city value="Denver" required></label>
<label>ZIP<input name=postal_code value="80202" required></label>
<fieldset><legend>Payment</legend><label><input type=radio name=provider value=pp_zcash_zcash checked> Zcash (ZEC), no middleman, no fees</label></fieldset>
<button type=submit>Place order</button></form><p id=status></p>`
  res.setHeader("content-type", "text/html; charset=utf-8")
  res.send(storePage(await storeConfig(req), body, `document.getElementById("checkout").addEventListener("submit", checkout);`))
}
