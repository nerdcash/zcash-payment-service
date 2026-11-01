import assert from "node:assert/strict"
import { test } from "node:test"
import { decide, readState, type DecideInput } from "../src/lib/decide"

const base: DecideInput = {
  state: { state_version: 0, received_zat: "0" },
  totalZat: 0n,
  stateVersion: 1,
  dueZat: 50_000_000n,
  paid: false,
  closed: false,
  repriced: false,
}
const d = (o: Partial<DecideInput>) => decide({ ...base, ...o })

test("stale and duplicate versions are ignored", () => {
  assert.equal(d({ state: { state_version: 3, received_zat: "0" }, stateVersion: 3 }).type, "stale")
  assert.equal(d({ state: { state_version: 3, received_zat: "0" }, stateVersion: 2, totalZat: 99n }).type, "stale")
})

test("partial, then full payment captures", () => {
  assert.equal(d({ totalZat: 20_000_000n }).type, "partial")
  assert.equal(d({ totalZat: 50_000_000n, stateVersion: 2 }).type, "capture")
  assert.equal(d({ totalZat: 60_000_000n }).type, "capture")
})

test("a drop after payment holds the order", () => {
  const r = d({ paid: true, totalZat: 10_000_000n, stateVersion: 3 })
  assert.equal(r.type, "hold")
})

test("recovery releases only the plugin's hold and never captures again", () => {
  const held = { state_version: 3, received_zat: "10000000", hold: "auto" as const }
  assert.equal(d({ paid: true, state: held, stateVersion: 4, totalZat: 50_000_000n }).type, "release")
  // Still short: stays held.
  assert.equal(d({ paid: true, state: held, stateVersion: 4, totalZat: 20_000_000n }).type, "hold")
  // A paid order that was never held just gets a note.
  assert.equal(d({ paid: true, stateVersion: 4, totalZat: 70_000_000n }).type, "note")
  for (const total of [0n, 50_000_000n, 90_000_000n]) {
    for (const hold of [undefined, "auto" as const]) {
      const r = d({ paid: true, state: { state_version: 1, received_zat: "0", hold }, stateVersion: 2, totalZat: total })
      assert.notEqual(r.type, "capture")
    }
  }
})

test("closed orders are never revived", () => {
  const r = d({ closed: true, totalZat: 50_000_000n })
  assert.equal(r.type, "review")
  assert.equal(r.type === "review" && r.hold, false)
})

test("repriced sessions go to review", () => {
  assert.equal(d({ repriced: true, totalZat: 50_000_000n }).type, "review")
})

test("review sticks, and holds an underfunded paid order", () => {
  const state = { state_version: 1, received_zat: "0", needs_review: true }
  assert.equal(d({ state, stateVersion: 2, totalZat: 50_000_000n }).type, "review")
  const r = d({ state, stateVersion: 2, paid: true, totalZat: 1n })
  assert.ok(r.type === "review" && r.hold)
  const refunded = d({ paid: true, closed: true, totalZat: 1n })
  assert.ok(refunded.type === "review" && refunded.hold)
})

test("readState tolerates junk", () => {
  assert.deepEqual(readState(undefined), { state_version: 0, received_zat: "0" })
  assert.deepEqual(readState({ zcash: { state_version: "x", received_zat: 5, hold: "manual" } }), {
    state_version: 0,
    received_zat: "0",
  })
  assert.deepEqual(readState({ zcash: { state_version: 4, received_zat: "12", hold: "auto", needs_review: true } }), {
    state_version: 4,
    received_zat: "12",
    hold: "auto",
    needs_review: true,
  })
})

test("a payment seen but not yet confirmed waits, then captures at the required depth", () => {
  const pending = d({ seenZat: 50_000_000n, totalZat: 0n })
  assert.equal(pending.type, "pending")
  assert.match((pending as { message: string }).message, /pending confirmation/)
  assert.equal(d({ seenZat: 50_000_000n, totalZat: 20_000_000n }).type, "pending")
  assert.equal(d({ seenZat: 50_000_000n, totalZat: 50_000_000n }).type, "capture")
  // A reorg that undoes the confirmations on a captured payment holds the order.
  assert.equal(d({ paid: true, seenZat: 50_000_000n, totalZat: 0n }).type, "hold")
})
