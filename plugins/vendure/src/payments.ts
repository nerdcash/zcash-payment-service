import { Payment, TransactionalConnection } from "@vendure/core"
import { HANDLER_CODE } from "./lib/options"

/**
 * Whether this plugin issued the payment. `Payment.method` holds the merchant's payment method
 * code, which can be anything, so ownership comes from the metadata the handler wrote. Storefronts
 * can't write a payment's metadata.
 */
export function isZcashPayment(p: Payment): boolean {
  const m = p.metadata as Record<string, unknown> | undefined
  return m?.handler === HANDLER_CODE && typeof m.payment_id === "string" && m.address === p.transactionId
}

const NO_ROW_LOCKS = ["sqlite", "better-sqlite3", "sqljs", "capacitor", "cordova", "react-native", "expo"]

/**
 * A row lock for databases that have one. SQLite has no row locks, but it runs one write
 * transaction at a time, which serializes the same updates.
 */
export function rowLock(connection: TransactionalConnection): { mode: "pessimistic_write" } | undefined {
  return NO_ROW_LOCKS.includes(String(connection.rawConnection.options.type))
    ? undefined
    : { mode: "pessimistic_write" }
}
