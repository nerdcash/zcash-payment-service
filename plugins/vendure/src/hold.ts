import { FulfillmentHandler, FulfillmentProcess, ID, Injector, OrderProcess, Payment, RequestContext, TransactionalConnection } from "@vendure/core"
import { SHIPPING_ORDER_STATES } from "./constants"
import { readState } from "./lib/decide"
import { isZcashPayment, rowLock } from "./payments"

let connection: TransactionalConnection | undefined

/**
 * The hold message, if any Zcash payment on the order is held by the plugin. With `lock`, the
 * order's payment rows stay locked until the caller's transaction ends, so the webhook can't place
 * a hold in the meantime.
 */
export async function heldMessage(ctx: RequestContext, orderId: ID, lock = false): Promise<string | null> {
  if (!connection) {
    return null
  }
  const query = connection
    .getRepository(ctx, Payment)
    .createQueryBuilder("payment")
    .where("payment.orderId = :orderId", { orderId })
  const mode = lock ? rowLock(connection) : undefined
  if (mode) {
    query.setLock(mode.mode)
  }
  for (const p of await query.getMany()) {
    if (isZcashPayment(p) && readState(p.metadata).hold === "auto") {
      return "This order's Zcash funds dropped after payment and it is on hold. Do not fulfill it until the hold is released."
    }
  }
  return null
}

/** Stops a held order from being shipped or delivered. */
export const zcashHoldOrderProcess: OrderProcess<string> = {
  init(injector: Injector) {
    connection = injector.get(TransactionalConnection)
  },
  async onTransitionStart(_from, to, data) {
    if (SHIPPING_ORDER_STATES.includes(to)) {
      return (await heldMessage(data.ctx, data.order.id)) ?? undefined
    }
  },
}

/** Stops fulfillments from being created, shipped, or delivered for a held order. */
export const zcashHoldFulfillmentProcess: FulfillmentProcess<string> = {
  init(injector: Injector) {
    connection = injector.get(TransactionalConnection)
  },
  async onTransitionStart(_from, to, data) {
    if (to === "Pending" || to === "Shipped" || to === "Delivered") {
      for (const order of data.orders) {
        const message = await heldMessage(data.ctx, order.id)
        if (message) {
          return message
        }
      }
    }
  },
}

const guarded = new WeakSet<FulfillmentHandler>()

/**
 * Makes every fulfillment handler refuse a held order before it creates anything. Vendure saves a
 * new fulfillment before its first state transition, so the process guard alone would leave a
 * stranded "Created" fulfillment holding the order's lines. The check locks the order's payment
 * rows for the rest of the fulfillment mutation, so a hold can't land between it and the save.
 */
export function guardFulfillmentHandlers(handlers: FulfillmentHandler[]) {
  for (const handler of handlers) {
    if (guarded.has(handler)) {
      continue
    }
    guarded.add(handler)
    const create = handler.createFulfillment.bind(handler)
    handler.createFulfillment = async (ctx, orders, lines, args) => {
      for (const order of orders) {
        const message = await heldMessage(ctx, order.id, true)
        if (message) {
          throw new Error(message)
        }
      }
      return create(ctx, orders, lines, args)
    }
  }
}
