/**
 * Pure helpers with no Medusa dependencies, so they can be unit tested on their own.
 */
import { createHmac, timingSafeEqual } from "node:crypto"

export const ZAT_PER_ZEC = 100_000_000n

/** Parses a ZEC decimal such as "0.12345678" into zatoshis. Null for anything else. */
export function zecToZat(value: string): bigint | null {
  const v = value.trim()
  const m = /^(\d*)(?:\.(\d{0,8}))?$/.exec(v)
  if (!m || v === "" || v === ".") {
    return null
  }
  const whole = m[1] === "" ? "0" : m[1]
  if (whole.replace(/^0+/, "").length > 10) {
    return null
  }
  return BigInt(whole) * ZAT_PER_ZEC + BigInt((m[2] ?? "").padEnd(8, "0"))
}

/** Formats zatoshis as a ZEC decimal with 8 places. */
export function zatToZec(zat: bigint): string {
  const sign = zat < 0n ? "-" : ""
  const abs = zat < 0n ? -zat : zat
  return `${sign}${abs / ZAT_PER_ZEC}.${(abs % ZAT_PER_ZEC).toString().padStart(8, "0")}`
}

/** Splits a non-negative decimal string into an integer and its power-of-ten scale. */
function parseDecimal(value: string): { int: bigint; scale: number } | null {
  const m = /^(\d+)(?:\.(\d+))?$/.exec(value.trim())
  if (!m) {
    return null
  }
  const frac = m[2] ?? ""
  return { int: BigInt(m[1] + frac), scale: frac.length }
}

/**
 * Converts an amount in the store currency to zatoshis. A ZEC amount is used as-is (at most 8
 * decimals). Otherwise `zecPerUnit` is how many ZEC one unit of the currency buys, and the result
 * rounds up to the next zatoshi so the merchant is never short. Exact decimal math, no floats.
 */
export function amountToZat(amount: string, currency: string, zecPerUnit: string | undefined): bigint | null {
  if (currency.toLowerCase() === "zec") {
    const zat = zecToZat(amount)
    return zat !== null && zat > 0n ? zat : null
  }
  const a = parseDecimal(amount)
  const r = zecPerUnit === undefined ? null : parseDecimal(zecPerUnit)
  if (!a || !r || r.int === 0n) {
    return null
  }
  const product = a.int * r.int * ZAT_PER_ZEC
  const divisor = 10n ** BigInt(a.scale + r.scale)
  const zat = (product + divisor - 1n) / divisor
  return zat > 0n ? zat : null
}

/** Normalizes a Medusa amount (number, string, or BigNumber) to a plain decimal string. */
export function amountToString(amount: unknown): string | null {
  let s: string
  if (typeof amount === "number") {
    if (!Number.isFinite(amount) || amount < 0) {
      return null
    }
    // Avoid exponent notation for small or large numbers.
    s = amount.toFixed(8).replace(/\.?0+$/, "")
  } else if (typeof amount === "string") {
    s = amount.trim()
  } else if (amount && typeof amount === "object") {
    const o = amount as { numeric?: unknown; toJSON?: () => unknown; valueOf: () => unknown }
    const inner = typeof o.toJSON === "function" ? o.toJSON() : o.numeric ?? o.valueOf()
    return inner === amount ? null : amountToString(inner)
  } else {
    return null
  }
  return /^\d+(\.\d+)?$/.test(s) ? s : null
}

/**
 * Joins the service's pay_url with an optional public base URL. An absolute pay_url is used as-is.
 * Null when no public URL results.
 */
export function resolvePayUrl(payUrl: string | undefined, base: string | undefined): string | null {
  if (!payUrl) {
    return null
  }
  if (/^https?:\/\//i.test(payUrl)) {
    return payUrl
  }
  const b = (base ?? "").trim().replace(/\/+$/, "")
  if (!b || !/^https?:\/\//i.test(b) || !payUrl.startsWith("/")) {
    return null
  }
  return b + payUrl
}

/** Checks the service's X-Signature header (hex HMAC-SHA256 of the raw body) in constant time. */
export function verifySignature(rawBody: Buffer | string, signature: unknown, secret: string): boolean {
  if (!secret || typeof signature !== "string") {
    return false
  }
  const sig = signature.trim().toLowerCase()
  if (!/^[0-9a-f]{64}$/.test(sig)) {
    return false
  }
  const expected = createHmac("sha256", secret).update(rawBody).digest()
  return timingSafeEqual(expected, Buffer.from(sig, "hex"))
}

export interface Observation {
  eventId: string
  address: string
  totalReceived: string
  totalZat: bigint
  stateVersion: number
}

/** Validates a webhook body and returns its fields, or null. */
export function parseObservation(raw: Buffer | string): Observation | null {
  let data: Record<string, unknown>
  try {
    data = JSON.parse(raw.toString())
  } catch {
    return null
  }
  if (!data || typeof data !== "object" || Array.isArray(data)) {
    return null
  }
  for (const key of ["event_id", "payment_source", "address", "total_received", "observed_at"]) {
    const v = data[key]
    if (typeof v !== "string" || v.trim() === "") {
      return null
    }
  }
  if (data.payment_source !== "zcash") {
    return null
  }
  const m = /:v(\d+)$/.exec(data.event_id as string)
  if (!m) {
    return null
  }
  const stateVersion = Number(m[1])
  const totalZat = zecToZat(data.total_received as string)
  if (totalZat === null || !Number.isSafeInteger(stateVersion)) {
    return null
  }
  return {
    eventId: data.event_id as string,
    address: data.address as string,
    totalReceived: data.total_received as string,
    totalZat,
    stateVersion,
  }
}
