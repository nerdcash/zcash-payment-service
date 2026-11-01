import type { MedusaRequest, MedusaResponse } from "@medusajs/framework/http"
import { storePage } from "../page"
import { storeConfig } from "../route"

export async function GET(req: MedusaRequest, res: MedusaResponse) {
  res.setHeader("content-type", "text/html; charset=utf-8")
  res.send(storePage(await storeConfig(req), `<h2>Thank you!</h2><div id=order>Loading…</div>`, "showOrder();"))
}
