import { ConfigService, Injector, LanguageCode, Logger, PaymentMethodHandler } from "@vendure/core"
import { loggerCtx, pluginOptions } from "./constants"
import { HANDLER_CODE, rateFor } from "./lib/options"
import { ServiceClient } from "./lib/service-client"
import { amountToZat, minorUnitsToString, resolvePayUrl, zatToZec, zecToZat } from "./lib/zcash"

/** What the handler stores in a Zcash payment's metadata. `public` is visible to the Shop API. */
export interface ZcashPaymentMetadata {
  address: string
  payment_id: string
  amount_zat: string
  public: {
    address: string
    amount_zec: string
    qr_text: string
    /** Public pay page URL, when there is one. Storefronts send the customer here. */
    pay_url: string | null
  }
  zcash?: unknown
  [key: string]: unknown
}

let precision = 2

/**
 * Takes Zcash through a self-hosted zcash-payment-service. Payments go straight to the merchant's
 * own wallet.
 *
 * Adding the payment issues a fresh address and returns it as **Authorized**, so the order is placed
 * and the customer goes to the pay page (like a bank transfer). The signed webhook settles the
 * payment once the service sees the full amount.
 */
export const zcashPaymentHandler = new PaymentMethodHandler({
  code: HANDLER_CODE,
  description: [{ languageCode: LanguageCode.en, value: "Zcash (self-hosted)" }],
  args: {},

  init(injector: Injector) {
    const strategy = injector.get(ConfigService).entityOptions.moneyStrategy as { precision?: number }
    precision = typeof strategy?.precision === "number" ? strategy.precision : 2
  },

  async createPayment(ctx, order, amount, _args, metadata) {
    const options = pluginOptions.current
    if (!options) {
      return { amount, state: "Error", errorMessage: "ZcashPaymentPlugin is not initialized." }
    }
    const currency = String(order.currencyCode)
    const decimal = minorUnitsToString(amount, precision)
    const zat = decimal === null ? null : amountToZat(decimal, currency, rateFor(options, currency))
    if (zat === null) {
      return {
        amount,
        state: "Declined",
        errorMessage: `This amount cannot be converted to ZEC. Set the Zcash plugin's zecRate for ${currency}.`,
      }
    }
    const returnUrl =
      typeof metadata?.return_url === "string" && metadata.return_url
        ? metadata.return_url
        : options.returnUrl?.replace("{order_code}", encodeURIComponent(order.code))
    let session
    try {
      session = await new ServiceClient(options.serviceUrl).createSession({
        payment_source: "zcash",
        label: `Order ${order.code}`,
        memo: `Order ${order.code}`,
        amount: zatToZec(zat),
        return_url: returnUrl || undefined,
      })
    } catch (e) {
      Logger.error(`zcash-payment-service: ${(e as Error).message}`, loggerCtx)
      return { amount, state: "Declined", errorMessage: "Zcash payments are temporarily unavailable. Please try again." }
    }
    const data: ZcashPaymentMetadata = {
      address: session.address,
      payment_id: session.payment_id,
      amount_zat: zat.toString(),
      public: {
        address: session.address,
        amount_zec: zatToZec(zat),
        qr_text: session.qr_text,
        pay_url: resolvePayUrl(session.pay_url, options.payBaseUrl),
      },
    }
    // The address identifies the payment to the webhook.
    return { amount, state: "Authorized", transactionId: session.address, metadata: data }
  },

  /**
   * Settles only when the service sees the full amount, whether the webhook or an administrator
   * asks, so nobody can mark an unpaid order paid by mistake.
   */
  async settlePayment(_ctx, _order, payment) {
    const options = pluginOptions.current
    const data = payment.metadata as Partial<ZcashPaymentMetadata>
    if (!options || typeof data?.payment_id !== "string" || typeof data.address !== "string") {
      return { success: false, errorMessage: "Not a Zcash payment this plugin issued." }
    }
    const live = await new ServiceClient(options.serviceUrl).status(data.payment_id, data.address)
    const received = live ? zecToZat(String(live.total_received ?? "")) : null
    if (received === null) {
      return { success: false, errorMessage: "Cannot reach zcash-payment-service to confirm the payment." }
    }
    const due = BigInt(String(data.amount_zat ?? "0"))
    if (due <= 0n || received < due) {
      return {
        success: false,
        errorMessage: `Received ${zatToZec(received)} of ${zatToZec(due)} ZEC so far.`,
      }
    }
    return { success: true }
  },

  /**
   * Cancelling can't stop someone from paying the address later. The webhook flags such payments
   * on the order for a refund instead of settling them.
   */
  cancelPayment() {
    return { success: true }
  },
})
