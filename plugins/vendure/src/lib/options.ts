export interface ZcashPluginOptions {
  /** Private URL Vendure uses to reach zcash-payment-service, e.g. http://zcash-payment-service:8787. */
  serviceUrl: string
  /** The same value as ZCASH_WEBHOOK_SECRET on the service. */
  webhookSecret: string
  /** Public base URL of the pay page, if the service does not set PUBLIC_BASE_URL. */
  payBaseUrl?: string
  /** ZEC per one unit of the order currency. Required unless the channel prices in ZEC. */
  zecRate?: string | Record<string, string>
  /**
   * Where the pay page sends the customer back to. `{order_code}` is replaced with the order code.
   * A storefront may pass its own `return_url` in the addPaymentToOrder metadata instead.
   */
  returnUrl?: string
}

/** The payment method handler code. Use it when creating the payment method in the admin. */
export const HANDLER_CODE = "zcash"

export function validateOptions(o: Partial<ZcashPluginOptions> | undefined): asserts o is ZcashPluginOptions {
  if (!o || typeof o.serviceUrl !== "string" || !/^https?:\/\//i.test(o.serviceUrl)) {
    throw new Error("ZcashPaymentPlugin: `serviceUrl` must be an http(s) URL.")
  }
  if (typeof o.webhookSecret !== "string" || o.webhookSecret === "") {
    throw new Error("ZcashPaymentPlugin: `webhookSecret` is required.")
  }
}

/** The configured rate for a currency, as a decimal string. */
export function rateFor(o: ZcashPluginOptions, currency: string): string | undefined {
  const r = o.zecRate
  if (r === undefined || r === null) {
    return undefined
  }
  if (typeof r === "string" || typeof r === "number") {
    return String(r)
  }
  const v = r[currency.toLowerCase()] ?? r[currency.toUpperCase()]
  return v === undefined ? undefined : String(v)
}
