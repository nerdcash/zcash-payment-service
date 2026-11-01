import { Inject, Injectable } from "@nestjs/common"
import {
  EventBus,
  HistoryService,
  isGraphQlErrorResult,
  Logger,
  Order,
  OrderService,
  Payment,
  RequestContext,
  RequestContextService,
  TransactionalConnection,
} from "@vendure/core"
import { HistoryEntryType } from "@vendure/common/lib/generated-types"
import { loggerCtx, PLUGIN_OPTIONS } from "./constants"
import { ZcashPaymentHoldEvent, ZcashPaymentReviewEvent } from "./events"
import { decide, readState, type ZcashState } from "./lib/decide"
import { type ZcashPluginOptions } from "./lib/options"
import { isZcashPayment, rowLock } from "./payments"
import { ServiceClient } from "./lib/service-client"
import { parseObservation, verifySignature, zatToZec, zecToZat, type Observation } from "./lib/zcash"

export interface WebhookResult {
  status: number
  body: Record<string, unknown>
}

class RetryError extends Error {
  constructor(message: string, readonly status = 503) {
    super(message)
  }
}

const CLOSED_PAYMENT_STATES = ["Cancelled", "Declined", "Error"]

@Injectable()
export class ZcashWebhookService {
  constructor(
    @Inject(PLUGIN_OPTIONS) private readonly options: ZcashPluginOptions,
    private readonly connection: TransactionalConnection,
    private readonly requestContextService: RequestContextService,
    private readonly orderService: OrderService,
    private readonly historyService: HistoryService,
    private readonly eventBus: EventBus
  ) {}

  async handle(raw: Buffer | undefined, signature: unknown): Promise<WebhookResult> {
    if (!Buffer.isBuffer(raw) || !verifySignature(raw, signature, this.options.webhookSecret)) {
      return { status: 401, body: { error: "invalid signature" } }
    }
    const observation = parseObservation(raw)
    if (!observation) {
      return { status: 400, body: { error: "invalid payload" } }
    }

    // Find the payment and its channel first, outside any transaction.
    const bootstrap = await this.requestContextService.create({ apiType: "admin" })
    const candidates = await this.connection.getRepository(bootstrap, Payment).find({
      where: { transactionId: observation.address },
      relations: ["order", "order.channels"],
    })
    const found = candidates.find(isZcashPayment)
    if (!found?.order) {
      // Not ours. Acknowledge so the service stops retrying, but leave a trace for reconciliation.
      Logger.warn(
        `Webhook for unknown address ${observation.address} (total ${observation.totalReceived} ZEC). Ignored.`,
        loggerCtx
      )
      return { status: 200, body: { ok: true, ignored: "unknown address" } }
    }
    const ctx = await this.requestContextService.create({
      apiType: "admin",
      // The token, not the joined entity: the context needs the channel's tax zones, which the
      // channel service loads (and caches) by token.
      channelOrToken: found.order.channels?.[0]?.token,
    })

    try {
      const body = await this.connection.withTransaction(ctx, (tctx) => this.apply(tctx, found.id, observation))
      return { status: 200, body }
    } catch (e) {
      const status = e instanceof RetryError ? e.status : 500
      Logger.warn(`Webhook for ${observation.address} not applied yet: ${(e as Error).message}`, loggerCtx)
      // Not recording the version makes the service retry.
      return { status, body: { error: (e as Error).message } }
    }
  }

  private async apply(ctx: RequestContext, paymentId: Payment["id"], observation: Observation) {
    const payments = this.connection.getRepository(ctx, Payment)
    // Lock the payment row so overlapping deliveries apply one at a time.
    const locked = await payments.findOne({ where: { id: paymentId }, lock: rowLock(this.connection) })
    if (!locked) {
      return { ok: true, ignored: "unknown address" }
    }
    const load = async () => {
      const payment = await payments.findOneOrFail({ where: { id: paymentId }, relations: ["refunds"] })
      const order = await this.connection
        .getRepository(ctx, Order)
        .findOneOrFail({ where: { payments: { id: paymentId } } })
      return { payment, order }
    }
    let { payment, order } = await load()
    const meta = (payment.metadata ?? {}) as Record<string, unknown>
    const state = readState(meta)
    const dueZat = BigInt(String(meta.amount_zat ?? "0"))
    const flags = (p: Payment, o: Order) => ({
      paid: p.state === "Settled",
      closed:
        o.state === "Cancelled" ||
        CLOSED_PAYMENT_STATES.includes(p.state) ||
        (p.refunds ?? []).some((r) => r.state !== "Failed"),
    })
    const decision = decide({
      state,
      totalZat: observation.totalZat,
      stateVersion: observation.stateVersion,
      dueZat,
      ...flags(payment, order),
    })
    const zec = zatToZec(observation.totalZat)
    const next: ZcashState = {
      ...state,
      state_version: observation.stateVersion,
      received_zat: observation.totalZat.toString(),
    }
    const note = (text: string) =>
      this.historyService.createHistoryEntryForOrder(
        { ctx, orderId: order.id, type: HistoryEntryType.ORDER_NOTE, data: { note: text } },
        false
      )
    const review = async (message: string) => {
      Logger.warn(`Order ${order.code}: ${message}`, loggerCtx)
      next.needs_review = true
      await note(`Zcash: ${message}`)
      await this.eventBus.publish(new ZcashPaymentReviewEvent(ctx, order.id, order.code, message))
    }
    const hold = async (message: string) => {
      if (state.hold === "auto") {
        return
      }
      next.hold = "auto"
      await note(`Zcash: ON HOLD, do not fulfill. ${message}`)
      await this.eventBus.publish(new ZcashPaymentHoldEvent(ctx, order.id, order.code, true, message))
    }
    const liveTotal = async () => {
      const live = await new ServiceClient(this.options.serviceUrl).status(
        String(meta.payment_id ?? ""),
        String(meta.address ?? "")
      )
      const zat = live ? zecToZat(String(live.total_received ?? "")) : null
      if (zat === null) {
        throw new RetryError("cannot confirm with the service, retry")
      }
      return zat
    }

    switch (decision.type) {
      case "stale":
        return { ok: true, ignored: "stale" }
      case "review":
        await review(decision.message)
        if (decision.hold) {
          await hold(decision.message)
        }
        break
      case "hold":
        Logger.warn(`Order ${order.code}: ${decision.message}`, loggerCtx)
        await hold(decision.message)
        break
      case "release": {
        // A newer drop may already have happened at the service: release only if it still sees
        // the full amount, and keep the hold if it can't be asked.
        const live = await liveTotal()
        if (live < dueZat) {
          Logger.info(
            `Order ${order.code}: an event reported ${zec} ZEC, but the service now sees ${zatToZec(live)} ZEC. Hold kept.`,
            loggerCtx
          )
          return { ok: true, ignored: "superseded" }
        }
        // Never settle again: the order already moved past payment, and its fulfillment must not rerun.
        delete next.hold
        await note(`Zcash: ${decision.message}`)
        await this.eventBus.publish(new ZcashPaymentHoldEvent(ctx, order.id, order.code, false, decision.message))
        Logger.info(`Order ${order.code}: ${decision.message}`, loggerCtx)
        break
      }
      case "note":
      case "partial":
        Logger.info(`Order ${order.code}: ${decision.message}`, loggerCtx)
        break
      case "capture": {
        // A retried delivery may describe funds that were dropped since. Confirm first.
        const live = await liveTotal()
        if (live < dueZat) {
          Logger.info(
            `Order ${order.code}: an event reported ${zec} ZEC, but the service now sees ${zatToZec(live)} ZEC. Not settled.`,
            loggerCtx
          )
          return { ok: true, ignored: "superseded" }
        }
        const result = await this.orderService.settlePayment(ctx, paymentId)
        if (isGraphQlErrorResult(result)) {
          throw new RetryError(`could not settle payment: ${result.message}`, 500)
        }
        ;({ payment, order } = await load())
        if (payment.state !== "Settled") {
          throw new RetryError("could not settle payment", 500)
        }
        Logger.info(`Order ${order.code}: received ${zec} ZEC. Payment settled.`, loggerCtx)
        // Cancellation can race the settlement. Flag it for a refund rather than block it.
        if (order.state === "Cancelled") {
          await review(`Received ${zec} ZEC and settled it while the order was being cancelled. Refund or reconcile by hand.`)
        }
        break
      }
    }

    // Record the version last, after the transition above succeeded.
    await payments.update({ id: paymentId }, { metadata: { ...(payment.metadata ?? {}), zcash: next } as Record<string, unknown> } as any)
    return { ok: true }
  }
}
