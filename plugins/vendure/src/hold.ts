import { FulfillmentHandler, FulfillmentProcess, ID, Injector, OrderProcess, Payment, RequestContext, TransactionalConnection } from "@vendure/core"
import { SHIPPING_ORDER_STATES } from "./constants"
import { readState } from "./lib/decide"
import { HANDLER_CODE } from "./lib/options"

let connection: TransactionalConnection | undefined

/** The hold message, if any Zcash payment on the order is held by the plugin. */
export async function heldMessage(ctx: RequestContext, orderId: ID): Promise<string | null> {
  if (!connection) {
    return null
  }
  const payments = await connection
    .getRepository(ctx, Payment)
    .find({ where: { order: { id: orderId }, method: HANDLER_CODE } })
  for (const p of payments) {
    if (readState(p.metadata).hold === "auto") {
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
 * stranded "Created" fulfillment holding the order's lines.
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
        const message = await heldMessage(ctx, order.id)
        if (message) {
          throw new Error(message)
        }
      }
      return create(ctx, orders, lines, args)
    }
  }
}
