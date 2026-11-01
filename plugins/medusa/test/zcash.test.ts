import assert from "node:assert/strict"
import { createHmac } from "node:crypto"
import { test } from "node:test"
import { findProviderConfig, rateFor, validateOptions } from "../src/lib/options"
import {
  amountToString,
  amountToZat,
  parseObservation,
  resolvePayUrl,
  verifySignature,
  zatToZec,
  zecToZat,
} from "../src/lib/zcash"

test("zecToZat parses up to 8 decimals", () => {
  assert.equal(zecToZat("0.5"), 50_000_000n)
  assert.equal(zecToZat("1"), 100_000_000n)
  assert.equal(zecToZat(".00000001"), 1n)
  assert.equal(zecToZat("0.123456789"), null)
  assert.equal(zecToZat("-1"), null)
  assert.equal(zecToZat("1e3"), null)
  assert.equal(zecToZat(""), null)
  assert.equal(zecToZat("."), null)
  assert.equal(zecToZat("12345678901"), null)
})

test("zatToZec formats 8 places", () => {
  assert.equal(zatToZec(50_000_000n), "0.50000000")
  assert.equal(zatToZec(1n), "0.00000001")
  assert.equal(zatToZec(-150_000_000n), "-1.50000000")
})

test("amountToZat converts exactly and rounds up", () => {
  assert.equal(amountToZat("0.5", "zec", undefined), 50_000_000n)
  assert.equal(amountToZat("0", "ZEC", undefined), null)
  assert.equal(amountToZat("20", "usd", "0.025"), 50_000_000n)
  // 0.1 * 0.3 = 0.03 exactly; floats would give 0.030000000000000002.
  assert.equal(amountToZat("0.1", "usd", "0.3"), 3_000_000n)
  // 1/3 of a zatoshi rounds up.
  assert.equal(amountToZat("1", "usd", "0.000000003"), 1n)
  assert.equal(amountToZat("10", "usd", undefined), null)
  assert.equal(amountToZat("10", "usd", "0"), null)
  assert.equal(amountToZat("10", "usd", "abc"), null)
})

test("amountToString handles Medusa amount shapes", () => {
  assert.equal(amountToString(20), "20")
  assert.equal(amountToString(0.1), "0.1")
  assert.equal(amountToString(1e-7), "0.0000001")
  assert.equal(amountToString("12.50"), "12.50")
  assert.equal(amountToString({ numeric: 3, toJSON: () => 3 }), "3")
  assert.equal(amountToString(0.123456781), "0.123456781")
  assert.equal(amountToString(1.5e21), "1500000000000000000000")
  assert.equal(amountToString(1.25e-9), "0.00000000125")
  assert.equal(amountToZat(amountToString(0.123456781)!, "usd", "1"), 12345679n)
  assert.equal(amountToString(-1), null)
  assert.equal(amountToString(Number.NaN), null)
  assert.equal(amountToString(null), null)
})

test("resolvePayUrl", () => {
  assert.equal(resolvePayUrl("https://pay.example/pay/ab", ""), "https://pay.example/pay/ab")
  assert.equal(resolvePayUrl("/pay/ab", "https://pay.example/"), "https://pay.example/pay/ab")
  assert.equal(resolvePayUrl("/pay/ab", ""), null)
  assert.equal(resolvePayUrl("/pay/ab", "javascript:alert(1)"), null)
  assert.equal(resolvePayUrl("", "https://pay.example"), null)
  assert.equal(resolvePayUrl(undefined, "https://pay.example"), null)
})

test("verifySignature", () => {
  const body = '{"a":1}'
  const sig = createHmac("sha256", "s3cret").update(body).digest("hex")
  assert.ok(verifySignature(body, sig, "s3cret"))
  assert.ok(verifySignature(Buffer.from(body), sig.toUpperCase(), "s3cret"))
  assert.ok(!verifySignature(body + " ", sig, "s3cret"))
  assert.ok(!verifySignature(body, sig, "other"))
  assert.ok(!verifySignature(body, sig, ""))
  assert.ok(!verifySignature(body, undefined, "s3cret"))
  assert.ok(!verifySignature(body, ["x"], "s3cret"))
  assert.ok(!verifySignature(body, "00", "s3cret"))
})

test("parseObservation", () => {
  const ok = {
    event_id: "zcash:7:v3",
    payment_source: "zcash",
    address: "utest1abc",
    total_received: "0.25000000",
    observed_at: "2026-01-01T00:00:00Z",
  }
  assert.deepEqual(parseObservation(JSON.stringify(ok)), {
    eventId: "zcash:7:v3",
    address: "utest1abc",
    totalReceived: "0.25000000",
    totalZat: 25_000_000n,
    stateVersion: 3,
  })
  assert.equal(parseObservation("not json"), null)
  assert.equal(parseObservation("[]"), null)
  assert.equal(parseObservation(JSON.stringify({ ...ok, payment_source: "btc" })), null)
  assert.equal(parseObservation(JSON.stringify({ ...ok, event_id: "zcash:7" })), null)
  assert.equal(parseObservation(JSON.stringify({ ...ok, total_received: "-1" })), null)
  assert.equal(parseObservation(JSON.stringify({ ...ok, address: "" })), null)
})

test("options", () => {
  assert.throws(() => validateOptions(undefined))
  assert.throws(() => validateOptions({ serviceUrl: "ftp://x", webhookSecret: "s" }))
  assert.throws(() => validateOptions({ serviceUrl: "http://x", webhookSecret: "" }))
  validateOptions({ serviceUrl: "http://x", webhookSecret: "s" })
  const o = { serviceUrl: "http://x", webhookSecret: "s", zecRate: { usd: "0.02" } }
  assert.equal(rateFor(o, "USD"), "0.02")
  assert.equal(rateFor(o, "eur"), undefined)
  assert.equal(rateFor({ ...o, zecRate: "0.03" }, "eur"), "0.03")
  const config = {
    modules: {
      payment: {
        resolve: "@medusajs/medusa/payment",
        options: { providers: [{ resolve: "@nerdcash/medusa-payment-zcash/providers/zcash", id: "zec", options: o }] },
      },
    },
  }
  assert.deepEqual(findProviderConfig(config), { options: o, providerId: "pp_zcash_zec" })
  assert.equal(findProviderConfig({ modules: [] }), null)
})
