export interface ZcashOptions {
  /** Private URL Medusa uses to reach zcash-payment-service, e.g. http://zcash-payment-service:8787. */
  serviceUrl: string
  /** The same value as ZCASH_WEBHOOK_SECRET on the service. */
  webhookSecret: string
  /** Public base URL of the pay page, if the service does not set PUBLIC_BASE_URL. */
  payBaseUrl?: string
  /** ZEC per one unit of the store currency. Required unless the store prices in ZEC. */
  zecRate?: string | Record<string, string>
  /**
   * Where the pay page sends the customer back to. `{session_id}` is replaced with the payment
   * session id. A storefront may pass its own `return_url` in the session data instead.
   */
  returnUrl?: string
}

export const PROVIDER_IDENTIFIER = "zcash"

export function validateOptions(o: Partial<ZcashOptions> | undefined): asserts o is ZcashOptions {
  if (!o || typeof o.serviceUrl !== "string" || !/^https?:\/\//i.test(o.serviceUrl)) {
    throw new Error("Zcash payment provider: `serviceUrl` must be an http(s) URL.")
  }
  if (typeof o.webhookSecret !== "string" || o.webhookSecret === "") {
    throw new Error("Zcash payment provider: `webhookSecret` is required.")
  }
}

/** The configured rate for a currency, as a decimal string. */
export function rateFor(o: ZcashOptions, currency: string): string | undefined {
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

/**
 * Finds this provider's options in the resolved Medusa config, so the webhook route shares one
 * configuration with the provider. Returns the provider's id too (pp_zcash_<id>).
 */
export function findProviderConfig(configModule: any): { options: ZcashOptions; providerId: string } | null {
  const modules = configModule?.modules
  const entries: any[] = Array.isArray(modules) ? modules : Object.values(modules ?? {})
  for (const m of entries) {
    const providers: any[] = m && typeof m === "object" ? m.options?.providers ?? [] : []
    for (const p of providers) {
      if (typeof p?.resolve === "string" && /medusa-payment-zcash|providers\/zcash/.test(p.resolve)) {
        return { options: p.options, providerId: `pp_${PROVIDER_IDENTIFIER}_${p.id ?? PROVIDER_IDENTIFIER}` }
      }
    }
  }
  return null
}
