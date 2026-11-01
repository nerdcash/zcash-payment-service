import type { MedusaRequest, MedusaResponse } from "@medusajs/framework/http"
import { ContainerRegistrationKeys, Modules } from "@medusajs/framework/utils"
import { processPaymentWorkflow } from "@medusajs/medusa/core-flows"
import { decide, readState, type ZcashState } from "../../../lib/decide"
import { findProviderConfig } from "../../../lib/options"
import { ServiceClient } from "../../../lib/service-client"
import { amountToString, parseObservation, settledZat, verifySignature, zatToZec, zecToZat } from "../../../lib/zcash"
import type { ZcashSessionData } from "../../../providers/zcash/service"

/** Event emitted when the plugin holds or releases a paid order, for merchants to act on. */
export const HOLD_EVENT = "zcash_payment.hold_changed"
/** Order metadata key that is set while an order must not be fulfilled. */
export const HOLD_METADATA_KEY = "zcash_payment_hold"
/** Order metadata key with the latest message that needs the merchant's attention. */
export const REVIEW_METADATA_KEY = "zcash_payment_review"

class RetryError extends Error {
  constructor(message: string, readonly status = 503) {
    super(message)
  }
}

/**
 * Receives signed payment observations from zcash-payment-service. Point the service's
 * ZCASH_WEBHOOK_URL here: https://<medusa backend>/hooks/zcash
 */
export async function POST(req: MedusaRequest, res: MedusaResponse) {
  const logger = req.scope.resolve(ContainerRegistrationKeys.LOGGER)
  const config = findProviderConfig(req.scope.resolve(ContainerRegistrationKeys.CONFIG_MODULE))
  if (!config) {
    logger.error("Zcash webhook received, but the Zcash payment provider is not configured.")
    return res.status(503).json({ error: "not configured" })
  }
  const raw = (req as unknown as { rawBody?: Buffer }).rawBody
  if (!raw || !verifySignature(raw, req.headers["x-signature"], config.options.webhookSecret)) {
    return res.status(401).json({ error: "invalid signature" })
  }
  const observation = parseObservation(raw)
  if (!observation) {
    return res.status(400).json({ error: "invalid payload" })
  }

  const payment = req.scope.resolve(Modules.PAYMENT)
  const sessions = (
    await payment.listPaymentSessions(
      { provider_id: config.providerId, data: { address: observation.address } } as any,
      { select: ["id"] }
    )
  ).filter(Boolean)
  if (sessions.length === 0) {
    // Not ours, or the session was deleted (for example a cart that changed before checkout).
    // Acknowledge so the service stops retrying, but leave a trace for reconciliation.
    logger.warn(
      `Zcash webhook for unknown address ${observation.address} (total ${observation.totalReceived} ZEC). Ignored.`
    )
    return res.status(200).json({ ok: true, ignored: "unknown address" })
  }

  const locking = req.scope.resolve(Modules.LOCKING)
  try {
    // Serialize deliveries for one session so the version check and the update are atomic.
    const result = await locking.execute(
      `zcash-payment:${sessions[0].id}`,
      () => apply(req, sessions[0].id, observation, config.options.serviceUrl),
      { timeout: 10 }
    )
    return res.status(200).json(result)
  } catch (e) {
    const status = e instanceof RetryError ? e.status : 500
    logger.warn(`Zcash webhook for ${observation.address} not applied yet: ${(e as Error).message}`)
    // Not recording the version makes the service retry.
    return res.status(status).json({ error: (e as Error).message })
  }
}

async function apply(
  req: MedusaRequest,
  sessionId: string,
  observation: NonNullable<ReturnType<typeof parseObservation>>,
  serviceUrl: string
): Promise<Record<string, unknown>> {
  const logger = req.scope.resolve(ContainerRegistrationKeys.LOGGER)
  const query = req.scope.resolve(ContainerRegistrationKeys.QUERY)
  const paymentModule = req.scope.resolve(Modules.PAYMENT)
  const orderModule = req.scope.resolve(Modules.ORDER)
  const eventBus = req.scope.resolve(Modules.EVENT_BUS)

  // Re-read inside the lock so we see any delivery that finished while we waited.
  const load = async () => {
    const {
      data: [session],
    } = await query.graph({
      entity: "payment_session",
      fields: [
        "id",
        "amount",
        "currency_code",
        "data",
        "metadata",
        "status",
        "payment_collection_id",
        "payment.id",
        "payment.captured_at",
        "payment.canceled_at",
        "payment.refunds.id",
      ],
      filters: { id: sessionId },
    })
    if (!session) {
      return null
    }
    const {
      data: [collection],
    } = await query.graph({
      entity: "payment_collection",
      fields: ["id", "order.id", "order.status", "order.metadata"],
      filters: { id: session.payment_collection_id },
    })
    return { session: session as any, order: (collection as any)?.order ?? null }
  }

  let loaded = await load()
  const data = loaded?.session.data as ZcashSessionData | undefined
  if (!loaded || !data || data.address !== observation.address) {
    return { ok: true, ignored: "unknown address" }
  }
  const state = readState(loaded.session.metadata)
  const dueZat = BigInt(data.amount_zat)
  const flags = (l: NonNullable<typeof loaded>) => {
    const p = l.session.payment
    return {
      paid: !!p?.captured_at,
      closed:
        l.order?.status === "canceled" ||
        !!p?.canceled_at ||
        (p?.refunds?.length ?? 0) > 0 ||
        l.session.status === "canceled",
      repriced: data.priced !== `${amountToString(l.session.amount)} ${String(l.session.currency_code).toLowerCase()}`,
    }
  }
  const decision = decide({
    state,
    // Capture and holds follow the confirmed amount; index 0 (everything seen) only informs.
    totalZat: observation.settledZat,
    seenZat: observation.totalZat,
    stateVersion: observation.stateVersion,
    dueZat,
    ...flags(loaded),
  })
  const zec = zatToZec(observation.settledZat)

  const setOrderMetadata = async (orderId: string | undefined, patch: Record<string, unknown>) => {
    if (!orderId) {
      return
    }
    // Medusa merges metadata keys; null removes one.
    await orderModule.updateOrders(orderId, { metadata: patch })
  }

  const next: ZcashState = {
    ...state,
    state_version: observation.stateVersion,
    received_zat: observation.settledZat.toString(),
  }
  const orderId: string | undefined = loaded.order?.id

  switch (decision.type) {
    case "stale":
      return { ok: true, ignored: "stale" }
    case "review":
      logger.warn(`Zcash payment ${sessionId}: ${decision.message}`)
      next.needs_review = true
      await setOrderMetadata(orderId, { [REVIEW_METADATA_KEY]: decision.message })
      if (decision.hold) {
        await hold(decision.message)
      }
      break
    case "hold":
      logger.warn(`Zcash payment ${sessionId}: ${decision.message}`)
      await hold(decision.message)
      break
    case "release": {
      // A newer drop may already have happened at the service: release only if it still sees the
      // full amount, and keep the hold if it can't be asked.
      const liveZat = await liveTotal()
      if (liveZat < dueZat) {
        logger.info(
          `Zcash payment ${sessionId}: an event reported ${zec} ZEC, but the service now sees ${zatToZec(liveZat)} ZEC. Hold kept.`
        )
        return { ok: true, ignored: "superseded" }
      }
      // Never capture or complete again: that would re-run payment and fulfillment side effects.
      // Clear the hold only if it is still the one the plugin wrote; a merchant may have replaced it.
      if (state.hold_message !== undefined && loaded.order?.metadata?.[HOLD_METADATA_KEY] === state.hold_message) {
        await setOrderMetadata(orderId, { [HOLD_METADATA_KEY]: null })
        await eventBus.emit({ name: HOLD_EVENT, data: { order_id: orderId, session_id: sessionId, held: false, message: decision.message } })
      }
      delete next.hold
      delete next.hold_message
      logger.info(`Zcash payment ${sessionId}: ${decision.message}`)
      break
    }
    case "note":
    case "partial":
    case "pending":
      logger.info(`Zcash payment ${sessionId}: ${decision.message}`)
      break
    case "capture": {
      // Deliveries can be retried after a newer state was skipped (for example a receipt that was
      // later dropped). Confirm the service still sees the funds first.
      const liveZat = await liveTotal()
      if (liveZat < dueZat) {
        logger.info(
          `Zcash payment ${sessionId}: an event reported ${zec} ZEC, but the service now sees ${zatToZec(liveZat)} ZEC. Not captured.`
        )
        return { ok: true, ignored: "superseded" }
      }
      // The status request took time: re-check so a cancellation made meanwhile wins.
      loaded = await load()
      const now = loaded ? flags(loaded) : null
      if (!loaded || !now || now.paid || now.closed || now.repriced || readState(loaded.session.metadata).needs_review) {
        throw new RetryError("payment changed, retry")
      }
      // Authorizes the session, completes the cart if it isn't an order yet, and captures.
      await processPaymentWorkflow(req.scope).run({
        input: {
          action: "captured",
          data: { session_id: sessionId, amount: loaded.session.amount },
        } as any,
      })
      loaded = await load()
      if (!loaded?.session.payment?.captured_at) {
        throw new RetryError("could not capture payment", 500)
      }
      logger.info(`Zcash payment ${sessionId}: received ${zec} ZEC. Payment captured.`)
      // Order cancellation runs outside this lock, so it can land between the check above and the
      // capture. Medusa has no hook to block it, so detect it and flag the order for a refund.
      if (loaded.order?.status === "canceled") {
        const message = `Received ${zec} ZEC and captured it while the order was being canceled. Refund or reconcile by hand.`
        logger.warn(`Zcash payment ${sessionId}: ${message}`)
        next.needs_review = true
        await setOrderMetadata(orderId, { [REVIEW_METADATA_KEY]: message })
      }
      break
    }
  }

  // Record the version last, after the transition above succeeded.
  await paymentModule.updatePaymentSession({
    id: sessionId,
    data: loaded!.session.data,
    amount: loaded!.session.amount,
    currency_code: loaded!.session.currency_code,
    metadata: { ...(loaded!.session.metadata ?? {}), zcash: next },
  })
  return { ok: true }

  async function liveTotal(): Promise<bigint> {
    const live = await new ServiceClient(serviceUrl).status(data!.payment_id, data!.address)
    const liveTotal = live ? zecToZat(String(live.total_received ?? "")) : null
    const liveZat = live && liveTotal !== null ? settledZat(live, liveTotal) : null
    if (liveZat === null) {
      throw new RetryError("cannot confirm with the service, retry")
    }
    return liveZat
  }

  async function hold(message: string) {
    if (state.hold === "auto") {
      return
    }
    if (typeof loaded!.order?.metadata?.[HOLD_METADATA_KEY] === "string") {
      // The merchant already holds this order. Leave their hold alone and don't claim it.
      return
    }
    next.hold = "auto"
    next.hold_message = message
    await setOrderMetadata(orderId, { [HOLD_METADATA_KEY]: message })
    await eventBus.emit({ name: HOLD_EVENT, data: { order_id: orderId, session_id: sessionId, held: true, message } })
  }
}
