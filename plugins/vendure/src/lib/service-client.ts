/** Minimal client for the private API of zcash-payment-service. */

export interface ServiceSession {
  address: string
  qr_text: string
  payment_id: string
  pay_url?: string
}

export interface ServiceStatus {
  address: string
  status?: string
  total_received?: string
  /** Index i: total with at least i confirmations. Present when the service sets REQUIRED_CONFIRMATIONS. */
  received_by_confirmations?: string[]
  qr_text?: string
  [key: string]: unknown
}

export class ServiceClient {
  constructor(private readonly baseUrl: string, private readonly fetchImpl: typeof fetch = fetch) {}

  private url(path: string): string {
    return this.baseUrl.replace(/\/+$/, "") + path
  }

  async createSession(body: Record<string, unknown>): Promise<ServiceSession> {
    const res = await this.fetchImpl(this.url("/payment-session"), {
      method: "POST",
      headers: { "content-type": "application/json", accept: "application/json" },
      body: JSON.stringify(body),
      signal: AbortSignal.timeout(15_000),
    })
    const text = await res.text()
    let json: any = null
    try {
      json = JSON.parse(text)
    } catch {
      // handled below
    }
    if (
      res.status !== 200 ||
      !json ||
      typeof json.address !== "string" ||
      !json.address ||
      typeof json.qr_text !== "string" ||
      typeof json.payment_id !== "string" ||
      !/^[0-9a-f]+$/i.test(json.payment_id)
    ) {
      throw new Error(`zcash-payment-service returned HTTP ${res.status}: ${text.slice(0, 300)}`)
    }
    return json as ServiceSession
  }

  /** The service's current view of a payment, or null if it can't be read or isn't for `address`. */
  async status(paymentId: string, address: string): Promise<ServiceStatus | null> {
    if (!/^[0-9a-f]+$/i.test(paymentId)) {
      return null
    }
    try {
      const res = await this.fetchImpl(this.url(`/payments/${paymentId}`), { signal: AbortSignal.timeout(5_000) })
      if (res.status !== 200) {
        return null
      }
      const body = (await res.json()) as ServiceStatus
      // Only trust the service's view for this session's own address.
      return body && body.address === address ? body : null
    } catch {
      return null
    }
  }
}
