import { AdminUiPlugin } from "@vendure/admin-ui-plugin"
import { DefaultJobQueuePlugin, VendureConfig } from "@vendure/core"
import { ZcashPaymentPlugin } from "@nerdcash/vendure-payment-zcash"
import { DemoStorePlugin } from "./demo-store.plugin"

const store = process.env.STORE_URL ?? "http://localhost:3000"

export const config: VendureConfig = {
  apiOptions: { port: Number(process.env.PORT ?? 3000), adminApiPath: "admin-api", shopApiPath: "shop-api" },
  authOptions: {
    tokenMethod: ["cookie", "bearer"],
    superadminCredentials: { identifier: "superadmin", password: "e2e-admin-password" },
    cookieOptions: { secret: "e2e-cookie-secret" },
  },
  dbConnectionOptions: {
    type: "postgres",
    url: process.env.DATABASE_URL,
    // Fine for a throwaway test database; use migrations in a real store.
    synchronize: true,
  },
  paymentOptions: { paymentMethodHandlers: [] },
  plugins: [
    DefaultJobQueuePlugin,
    AdminUiPlugin.init({ route: "admin", port: 3002 }),
    ZcashPaymentPlugin.init({
      serviceUrl: process.env.ZCASH_SERVICE_URL ?? "http://zcash-payment-service:8787",
      webhookSecret: process.env.ZCASH_WEBHOOK_SECRET ?? "",
      // 1 USD buys 0.025 ZEC in this test store, so $20 is 0.5 ZEC.
      zecRate: { usd: "0.025" },
      returnUrl: `${store}/demo-store/order?code={order_code}`,
    }),
    DemoStorePlugin,
  ],
}
