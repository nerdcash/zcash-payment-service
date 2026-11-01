import { defineMiddlewares } from "@medusajs/framework/http"

export default defineMiddlewares({
  routes: [
    {
      // The HMAC signature covers the exact bytes the service sent.
      matcher: "/hooks/zcash",
      method: ["POST"],
      bodyParser: { preserveRawBody: true },
    },
  ],
})
