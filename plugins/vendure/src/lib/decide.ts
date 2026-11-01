/**
 * The webhook's decision rules, kept free of Medusa so every branch can be unit tested.
 * The webhook service applies the result.
 */
import { zatToZec } from "./zcash"

/** What the plugin remembers per payment session (stored in the payment's metadata). */
export interface ZcashState {
  /** Highest state_version applied. */
  state_version: number
  /** Confirmed total at that version, in zatoshis. */
  received_zat: string
  /** "auto" while the plugin holds a paid order because its funds dropped. */
  hold?: "auto"
  /** Everything seen at that version, mempool included, in zatoshis. */
  seen_zat?: string
  /** Set once funds must be reconciled by hand. Later receipts stay in review too. */
  needs_review?: boolean
}

export interface DecideInput {
  state: ZcashState
  /** The confirmed amount: what has the service's REQUIRED_CONFIRMATIONS. */
  totalZat: bigint
  /** Everything received, mempool included. Defaults to totalZat. */
  seenZat?: bigint
  stateVersion: number
  dueZat: bigint
  /** The payment was settled (the order is paid). */
  paid: boolean
  /** The order was canceled, or the payment refunded or canceled. */
  closed: boolean
}

export type Decision =
  | { type: "stale" }
  | { type: "review"; message: string; hold: boolean }
  | { type: "hold"; message: string }
  | { type: "release"; message: string }
  | { type: "note"; message: string }
  | { type: "capture" }
  | { type: "partial"; message: string }
  | { type: "pending"; message: string }

export function decide(i: DecideInput): Decision {
  // Deliveries can arrive out of order. Apply only a newer state_version.
  if (i.stateVersion <= i.state.state_version) {
    return { type: "stale" }
  }
  const zec = zatToZec(i.totalZat)
  const due = zatToZec(i.dueZat)
  const short = i.dueZat > 0n && i.totalZat < i.dueZat

  let review: string | null = null
  if (i.state.needs_review) {
    review = `Zcash total for this payment is now ${zec} ZEC. A payment for it is under manual review, so this was not applied automatically.`
  } else if (i.closed) {
    review = `Received ${zec} ZEC after the order or payment was closed. Not applied automatically; review stock and refund or reconcile by hand.`
  }
  if (review !== null) {
    // Review doesn't make an underfunded paid order safe to fulfill: hold it too.
    return { type: "review", message: review, hold: i.paid && short }
  }

  if (i.paid) {
    if (short) {
      return {
        type: "hold",
        message: `Zcash funds for this paid order dropped to ${zec} of ${due} ZEC (a transaction was dropped or reorganized out). Check before fulfilling.`,
      }
    }
    if (i.state.hold === "auto") {
      // Recovery never captures or completes again: that would re-run payment and fulfillment
      // side effects. Only the plugin's own hold is lifted.
      return { type: "release", message: `Zcash funds recovered to ${zec} ZEC. Hold released.` }
    }
    return { type: "note", message: `Zcash total for this order is now ${zec} ZEC.` }
  }

  if (i.dueZat > 0n && i.totalZat >= i.dueZat) {
    return { type: "capture" }
  }
  const seen = i.seenZat ?? i.totalZat
  if (i.dueZat > 0n && seen >= i.dueZat) {
    return {
      type: "pending",
      message: `Payment of ${zatToZec(seen)} ZEC received, pending confirmation. It is settled once confirmed.`,
    }
  }
  return { type: "partial", message: `Received ${zatToZec(seen)} of ${due} ZEC so far.` }
}

/** Reads the plugin's state from session metadata, tolerating anything malformed. */
export function readState(metadata: unknown): ZcashState {
  const raw = (metadata && typeof metadata === "object" ? (metadata as Record<string, unknown>).zcash : null) as
    | Record<string, unknown>
    | null
    | undefined
  const version = Number(raw?.state_version)
  const received = typeof raw?.received_zat === "string" && /^\d+$/.test(raw.received_zat) ? raw.received_zat : "0"
  const state: ZcashState = {
    state_version: Number.isSafeInteger(version) && version >= 0 ? version : 0,
    received_zat: received,
  }
  if (raw?.hold === "auto") {
    state.hold = "auto"
  }
  if (typeof raw?.seen_zat === "string" && /^\d+$/.test(raw.seen_zat)) {
    state.seen_zat = raw.seen_zat
  }
  if (raw?.needs_review === true) {
    state.needs_review = true
  }
  return state
}
