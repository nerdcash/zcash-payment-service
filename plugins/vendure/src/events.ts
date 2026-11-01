import { ID, RequestContext, VendureEvent } from "@vendure/core"

/**
 * Published when the plugin holds a paid order because its Zcash funds dropped, and when it lifts
 * that hold. Subscribe to pause and resume your own fulfillment integrations.
 */
export class ZcashPaymentHoldEvent extends VendureEvent {
  constructor(
    public ctx: RequestContext,
    public orderId: ID,
    public orderCode: string,
    public held: boolean,
    public message: string
  ) {
    super()
  }
}

/** Published when a Zcash payment needs a merchant's attention, for example a payment after cancellation. */
export class ZcashPaymentReviewEvent extends VendureEvent {
  constructor(
    public ctx: RequestContext,
    public orderId: ID,
    public orderCode: string,
    public message: string
  ) {
    super()
  }
}
