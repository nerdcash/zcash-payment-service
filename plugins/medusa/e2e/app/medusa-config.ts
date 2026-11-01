import { defineConfig } from "@medusajs/framework/utils"

const store = process.env.STORE_URL ?? "http://localhost:9000"

module.exports = defineConfig({
  projectConfig: {
    databaseUrl: process.env.DATABASE_URL,
    // In production mode Medusa asks for TLS unless the database is on localhost; the e2e Postgres has none.
    databaseDriverOptions: { connection: { ssl: false } },
    // The e2e store is served over plain http on localhost, so session cookies can't be Secure.
    cookieOptions: { secure: false, sameSite: "lax" },
    http: {
      storeCors: store,
      adminCors: store,
      authCors: store,
      jwtSecret: "e2e-jwt-secret",
      cookieSecret: "e2e-cookie-secret",
    },
  },
  admin: { backendUrl: store },
  plugins: [{ resolve: "@nerdcash/medusa-payment-zcash", options: {} }],
  modules: [
    {
      resolve: "@medusajs/medusa/payment",
      options: {
        providers: [
          {
            resolve: "@nerdcash/medusa-payment-zcash/providers/zcash",
            id: "zcash",
            options: {
              serviceUrl: process.env.ZCASH_SERVICE_URL ?? "http://zcash-payment-service:8787",
              webhookSecret: process.env.ZCASH_WEBHOOK_SECRET,
              // 1 USD buys 0.025 ZEC in this test store, so $20 is 0.5 ZEC.
              zecRate: { usd: "0.025" },
              returnUrl: `${store}/demo-store/order?session_id={session_id}`,
            },
          },
        ],
      },
    },
  ],
})
