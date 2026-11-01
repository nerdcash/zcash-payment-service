import {
  AbstractPaymentProvider,
  MedusaError,
  PaymentActions,
  PaymentSessionStatus,
} from "@medusajs/framework/utils"
import type {
  AuthorizePaymentInput,
  AuthorizePaymentOutput,
  CancelPaymentInput,
  CancelPaymentOutput,
  CapturePaymentInput,
  CapturePaymentOutput,
  DeletePaymentInput,
  DeletePaymentOutput,
  GetPaymentStatusInput,
  GetPaymentStatusOutput,
  InitiatePaymentInput,
  InitiatePaymentOutput,
  Logger,
  ProviderWebhookPayload,
  RefundPaymentInput,
  RefundPaymentOutput,
  RetrievePaymentInput,
  RetrievePaymentOutput,
  UpdatePaymentInput,
  UpdatePaymentOutput,
  WebhookActionResult,
} from "@medusajs/framework/types"
import { PROVIDER_IDENTIFIER, rateFor, validateOptions, type ZcashOptions } from "../../lib/options"
import { ServiceClient } from "../../lib/service-client"
import { amountToString, amountToZat, resolvePayUrl, zatToZec, zecToZat } from "../../lib/zcash"

/** What the provider stores in a payment session's `data`. */
export interface ZcashSessionData {
  address: string
  payment_id: string
  qr_text: string
  /** Public pay page URL, when there is one. Storefronts send the customer here. */
  pay_url: string | null
  amount_zat: string
  amount_zec: string
  /** "<amount> <currency>" the address was issued for. */
  priced: string
  session_id?: string
  [key: string]: unknown
}

type Deps = { logger: Logger }

/**
 * A Medusa payment provider backed by a self-hosted zcash-payment-service. Payments go straight to
 * the merchant's own wallet: no middleman, no fees.
 *
 * Authorization is deferred: a session is `pending_authorization` until the service sees the full
 * amount, so the cart can be completed into an order before the customer pays (as with a bank
 * transfer). The signed webhook at /hooks/zcash then captures the payment.
 */
export class ZcashPaymentProviderService extends AbstractPaymentProvider<ZcashOptions> {
  static identifier = PROVIDER_IDENTIFIER

  protected readonly options_: ZcashOptions
  protected readonly logger_: Logger
  protected readonly client_: ServiceClient

  static validateOptions(options: Record<string, unknown>): void {
    validateOptions(options as Partial<ZcashOptions>)
  }

  constructor(container: Deps, options: ZcashOptions) {
    super(container, options)
    this.options_ = options
    this.logger_ = container.logger
    this.client_ = new ServiceClient(options.serviceUrl)
  }

  private priced(amount: unknown, currency: string): string {
    return `${amountToString(amount)} ${currency.toLowerCase()}`
  }

  private async createSession(
    amount: unknown,
    currency: string,
    data: Record<string, unknown> | undefined
  ): Promise<ZcashSessionData> {
    const amountStr = amountToString(amount)
    const zat = amountStr === null ? null : amountToZat(amountStr, currency, rateFor(this.options_, currency))
    if (zat === null) {
      throw new MedusaError(
        MedusaError.Types.INVALID_DATA,
        `This amount cannot be converted to ZEC. Set the Zcash provider's zecRate for ${currency.toUpperCase()}.`
      )
    }
    const sessionId = typeof data?.session_id === "string" ? data.session_id : undefined
    const returnUrl =
      typeof data?.return_url === "string" && data.return_url
        ? data.return_url
        : this.options_.returnUrl?.replace("{session_id}", encodeURIComponent(sessionId ?? ""))
    let session
    try {
      session = await this.client_.createSession({
        payment_source: "zcash",
        label: sessionId ? `Medusa ${sessionId}` : "Medusa",
        memo: sessionId ? `Payment ${sessionId}` : undefined,
        amount: zatToZec(zat),
        return_url: returnUrl || undefined,
      })
    } catch (e) {
      this.logger_.error(`zcash-payment-service: ${(e as Error).message}`)
      throw new MedusaError(
        MedusaError.Types.UNEXPECTED_STATE,
        "Zcash payments are temporarily unavailable. Please try again."
      )
    }
    return {
      address: session.address,
      payment_id: session.payment_id,
      qr_text: session.qr_text,
      pay_url: resolvePayUrl(session.pay_url, this.options_.payBaseUrl),
      amount_zat: zat.toString(),
      amount_zec: zatToZec(zat),
      priced: this.priced(amount, currency),
      session_id: sessionId,
    }
  }

  async initiatePayment(input: InitiatePaymentInput): Promise<InitiatePaymentOutput> {
    const data = await this.createSession(input.amount, input.currency_code, input.data)
    return { id: data.payment_id, data, status: PaymentSessionStatus.PENDING }
  }

  /**
   * A session keeps one address for life, so a payment to an address the customer already saw
   * always reaches it. Repricing mints a new address only while nothing has arrived.
   */
  async updatePayment(input: UpdatePaymentInput): Promise<UpdatePaymentOutput> {
    const data = input.data as ZcashSessionData | undefined
    if (data?.address && data.priced === this.priced(input.amount, input.currency_code)) {
      return { data }
    }
    if (data?.address) {
      const live = await this.client_.status(data.payment_id, data.address)
      const received = live ? zecToZat(String(live.total_received ?? "")) : null
      if (received === null || received > 0n) {
        throw new MedusaError(
          MedusaError.Types.NOT_ALLOWED,
          "This payment changed after Zcash funds arrived. Please contact the store."
        )
      }
    }
    return { data: await this.createSession(input.amount, input.currency_code, input.data) }
  }

  /** Maps the service's live total to a session status. */
  private async liveStatus(data: ZcashSessionData): Promise<PaymentSessionStatus> {
    const live = await this.client_.status(data.payment_id, data.address)
    if (!live) {
      // Unknown right now; keep authorization deferred rather than fail checkout.
      return PaymentSessionStatus.PENDING_AUTHORIZATION
    }
    const received = zecToZat(String(live.total_received ?? ""))
    const due = BigInt(data.amount_zat)
    return received !== null && due > 0n && received >= due
      ? PaymentSessionStatus.AUTHORIZED
      : PaymentSessionStatus.PENDING_AUTHORIZATION
  }

  async authorizePayment(input: AuthorizePaymentInput): Promise<AuthorizePaymentOutput> {
    const data = input.data as ZcashSessionData
    return { status: await this.liveStatus(data), data }
  }

  async getPaymentStatus(input: GetPaymentStatusInput): Promise<GetPaymentStatusOutput> {
    const data = input.data as ZcashSessionData
    return { status: await this.liveStatus(data), data }
  }

  /** Funds are already in the merchant's wallet; capture only records it. */
  async capturePayment(input: CapturePaymentInput): Promise<CapturePaymentOutput> {
    return { data: input.data }
  }

  /** Nothing to release on-chain. The address stays watched; late funds are flagged for review. */
  async cancelPayment(input: CancelPaymentInput): Promise<CancelPaymentOutput> {
    return { data: input.data }
  }

  async deletePayment(input: DeletePaymentInput): Promise<DeletePaymentOutput> {
    return { data: input.data }
  }

  /**
   * Shielded refunds are sent by the merchant from their own wallet. Medusa records the refund;
   * the webhook route then keeps any later receipts for this payment in manual review.
   */
  async refundPayment(input: RefundPaymentInput): Promise<RefundPaymentOutput> {
    this.logger_.warn(
      `Zcash refund of ${amountToString(input.amount)} recorded for ${(input.data as ZcashSessionData)?.address}. Send it from your wallet by hand.`
    )
    return { data: input.data }
  }

  async retrievePayment(input: RetrievePaymentInput): Promise<RetrievePaymentOutput> {
    return { data: input.data }
  }

  /**
   * Medusa's generic /hooks/payment route has no way to find a session by address, so the service's
   * webhook goes to this plugin's own /hooks/zcash route instead. Anything sent here is ignored.
   */
  async getWebhookActionAndData(_payload: ProviderWebhookPayload["payload"]): Promise<WebhookActionResult> {
    return { action: PaymentActions.NOT_SUPPORTED }
  }
}

export default ZcashPaymentProviderService
