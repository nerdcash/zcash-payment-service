import { createHmac, timingSafeEqual } from "node:crypto";

/**
 * Client for zcash-payment-service (https://github.com/nerdcash/zcash-payment-service).
 *
 * POST /payment-session mints a fresh unified address and the QR text.
 * The service later POSTs a payment observation to the store. It signs the
 * raw body with HMAC-SHA256 and sends the hex digest in X-Signature.
 *
 * The store owns prices, orders, and when an order counts as paid.
 */

const PAYMENT_SOURCE = "zcash";

export type PaymentSessionRequest = {
  label?: string;
  memo?: string;
  message?: string;
  amount?: string;
  /** Absolute http(s) URL the hosted pay page sends the payer to once paid. */
  returnUrl?: string;
};

export type PaymentSession = {
  address: string;
  qr_text: string;
  /** Id of the hosted pay page. Absent when the service predates payment links. */
  payment_id?: string;
  /** Link to the hosted pay page. Absent when the service predates payment links. */
  pay_url?: string;
};

export type PaymentStatusValue = "awaiting_payment" | "partially_paid" | "paid" | "received";

export type PaymentStatus = {
  payment_id: string;
  address: string;
  qr_text: string;
  pay_url: string;
  label: string | null;
  message: string | null;
  amount: string | null;
  total_received: string;
  /**
   * Present when the service sets `REQUIRED_CONFIRMATIONS=N`: N+1 ZEC amounts, where index `i`
   * is the total received with at least `i` confirmations (index 0 includes the mempool).
   */
  received_by_confirmations?: string[];
  state_version: number;
  status: PaymentStatusValue;
  return_url: string | null;
  created_at: string;
};

const PAYMENT_STATUSES: readonly string[] = ["awaiting_payment", "partially_paid", "paid", "received"];

export type PaymentObservation = {
  event_id: string;
  payment_source: "zcash";
  address: string;
  total_received: string;
  observed_at: string;
  /** See {@link PaymentStatus.received_by_confirmations}. */
  received_by_confirmations?: string[];
};

const MAX_CONFIRMATION_DEPTHS = 101;

/** Reads an optional `received_by_confirmations`; `null` means present but malformed. */
function optionalAmounts(value: unknown): string[] | undefined | null {
  if (value === undefined) return undefined;
  if (
    !Array.isArray(value) || value.length === 0 || value.length > MAX_CONFIRMATION_DEPTHS ||
    !value.every((item) => typeof item === "string" && item.trim() !== "" && item.length <= 64)
  ) {
    return null;
  }
  return value as string[];
}

/**
 * The amount received with at least `confirmations` confirmations, or `total_received` when the
 * service does not report `received_by_confirmations`. Throws a `RangeError` for a depth that
 * isn't a non-negative integer or that the service doesn't track (past the end of the array),
 * rather than substituting a shallower total.
 */
export function receivedWithConfirmations(
  payment: { total_received: string; received_by_confirmations?: string[] },
  confirmations: number,
): string {
  if (!Number.isSafeInteger(confirmations) || confirmations < 0) {
    throw new RangeError(`confirmations must be a non-negative integer, got ${confirmations}`);
  }
  const depths = payment.received_by_confirmations;
  if (depths === undefined) return payment.total_received;
  if (confirmations >= depths.length) {
    throw new RangeError(
      `the service tracks up to ${depths.length - 1} confirmations (REQUIRED_CONFIRMATIONS), not ${confirmations}`,
    );
  }
  return depths[confirmations];
}

export class PaymentServiceError extends Error {
  readonly status: number;

  constructor(message: string, status: number) {
    super(message);
    this.name = "PaymentServiceError";
    this.status = status;
  }
}

export class WebhookSignatureError extends Error {
  constructor() {
    super("invalid webhook signature");
    this.name = "WebhookSignatureError";
  }
}

export class WebhookPayloadError extends Error {
  constructor(message: string) {
    super(message);
    this.name = "WebhookPayloadError";
  }
}

export type ZcashPaymentClient = {
  createPaymentSession(request: PaymentSessionRequest): Promise<PaymentSession>;
  /**
   * Reads a payment link's status. For display only: mark orders paid from the signed webhook.
   * Requires a service with payment links.
   */
  getPayment(paymentId: string): Promise<PaymentStatus>;
  verifyWebhook(rawBody: Uint8Array, signature: string | undefined): PaymentObservation;
};

export type ZcashPaymentClientOptions = {
  /** Base URL of the zcash-payment-service, e.g. `http://localhost:8080`. */
  baseUrl: string;
  /** Shared secret the service uses to sign webhooks (HMAC-SHA256). */
  webhookSecret: string;
  /** Optional fetch implementation, for tests or custom transports. */
  fetchImpl?: typeof fetch;
  /** Request timeout in milliseconds for service calls. Defaults to 10000. */
  timeoutMs?: number;
};

export function createZcashPaymentClient(options: ZcashPaymentClientOptions): ZcashPaymentClient {
  if (!options.webhookSecret) {
    throw new Error("webhookSecret is required");
  }

  const baseUrl = options.baseUrl;
  const secret = options.webhookSecret;
  const fetchImpl = options.fetchImpl ?? fetch;

  return {
    async createPaymentSession(request) {
      let response: Response;
      try {
        response = await fetchImpl(serviceUrl(baseUrl, "/payment-session"), {
          method: "POST",
          headers: { "content-type": "application/json", accept: "application/json" },
          body: JSON.stringify({
            payment_source: PAYMENT_SOURCE,
            label: request.label ?? null,
            memo: request.memo ?? null,
            message: request.message ?? null,
            amount: request.amount ?? null,
            return_url: request.returnUrl ?? null,
          }),
          signal: AbortSignal.timeout(options.timeoutMs ?? 10_000),
        });
      } catch (error) {
        if (error instanceof PaymentServiceError) throw error;
        throw new PaymentServiceError("payment service is unreachable", 0);
      }

      const payload = await readBody(response);
      if (
        !isRecord(payload) ||
        typeof payload.address !== "string" ||
        typeof payload.qr_text !== "string" ||
        payload.address.length === 0 ||
        payload.qr_text.length === 0 ||
        payload.address.length > 1024 ||
        payload.qr_text.length > 4096
      ) {
        throw new PaymentServiceError("payment service returned an unexpected payload", response.status);
      }

      const session: PaymentSession = { address: payload.address, qr_text: payload.qr_text };
      if (typeof payload.payment_id === "string" && /^[0-9a-f]{32}$/.test(payload.payment_id)) {
        session.payment_id = payload.payment_id;
      }
      if (typeof payload.pay_url === "string" && payload.pay_url.length > 0 && payload.pay_url.length <= 2048) {
        session.pay_url = payload.pay_url;
      }
      return session;
    },

    async getPayment(paymentId) {
      if (!/^[0-9a-f]{32}$/.test(paymentId)) {
        throw new PaymentServiceError("payment id must be 32 lowercase hex characters", 0);
      }
      let response: Response;
      try {
        response = await fetchImpl(serviceUrl(baseUrl, `/payments/${paymentId}`), {
          method: "GET",
          headers: { accept: "application/json" },
          signal: AbortSignal.timeout(options.timeoutMs ?? 10_000),
        });
      } catch {
        throw new PaymentServiceError("payment service is unreachable", 0);
      }

      const payload = await readBody(response);
      const status = parsePaymentStatus(payload);
      if (!status) {
        throw new PaymentServiceError("payment service returned an unexpected payload", response.status);
      }
      return status;
    },

    verifyWebhook(rawBody, signature) {
      const expected = createHmac("sha256", secret).update(rawBody).digest();
      const provided = decodeSignature(signature);
      if (!provided || !timingSafeEqual(expected, provided)) {
        throw new WebhookSignatureError();
      }
      return parseObservation(rawBody);
    },
  };
}

function serviceUrl(baseUrl: string, pathname: string): string {
  const url = new URL(baseUrl);
  url.pathname = pathname;
  url.search = "";
  url.hash = "";
  return url.href;
}

async function readBody(response: Response): Promise<unknown> {
  let text: string;
  try {
    text = await response.text();
  } catch {
    throw new PaymentServiceError("payment service response could not be read", response.status);
  }
  if (!response.ok) {
    throw new PaymentServiceError(errorDetail(text, response.statusText), response.status);
  }
  try {
    return JSON.parse(text) as unknown;
  } catch {
    throw new PaymentServiceError("payment service returned invalid JSON", response.status);
  }
}

function errorDetail(text: string, fallback: string): string {
  try {
    const parsed = JSON.parse(text) as { error?: unknown };
    if (typeof parsed.error === "string" && parsed.error.trim()) {
      return parsed.error.trim().slice(0, 300);
    }
  } catch {
    // The service can also answer with plain text.
  }
  const trimmed = text.trim();
  return (trimmed || fallback || "payment service request failed").slice(0, 300);
}

function decodeSignature(signature: string | undefined): Buffer | null {
  if (!signature || !/^[0-9a-fA-F]{64}$/.test(signature.trim())) return null;
  return Buffer.from(signature.trim(), "hex");
}

function parseObservation(rawBody: Uint8Array): PaymentObservation {
  let value: unknown;
  try {
    value = JSON.parse(new TextDecoder().decode(rawBody)) as unknown;
  } catch {
    throw new WebhookPayloadError("webhook body is not JSON");
  }
  if (!isRecord(value)) {
    throw new WebhookPayloadError("webhook body must be an object");
  }

  const eventId = requiredString(value, "event_id", 200);
  const paymentSource = requiredString(value, "payment_source", 32);
  const address = requiredString(value, "address", 1024);
  const totalReceived = requiredString(value, "total_received", 64);
  const observedAt = requiredString(value, "observed_at", 64);
  if (paymentSource !== PAYMENT_SOURCE) {
    throw new WebhookPayloadError("payment_source must be 'zcash'");
  }
  const byConfirmations = optionalAmounts(value.received_by_confirmations);
  if (byConfirmations === null) {
    throw new WebhookPayloadError("received_by_confirmations must be an array of amount strings");
  }

  return {
    event_id: eventId,
    payment_source: PAYMENT_SOURCE,
    address,
    total_received: totalReceived,
    observed_at: observedAt,
    ...(byConfirmations ? { received_by_confirmations: byConfirmations } : {}),
  };
}

function parsePaymentStatus(value: unknown): PaymentStatus | null {
  if (!isRecord(value)) return null;
  const str = (key: string) => (typeof value[key] === "string" ? (value[key] as string) : null);
  const nullableStr = (key: string): string | null | undefined =>
    value[key] === null ? null : typeof value[key] === "string" ? (value[key] as string) : undefined;
  const paymentId = str("payment_id");
  const address = str("address");
  const qrText = str("qr_text");
  const payUrl = str("pay_url");
  const totalReceived = str("total_received");
  const createdAt = str("created_at");
  const status = str("status");
  const label = nullableStr("label");
  const message = nullableStr("message");
  const amount = nullableStr("amount");
  const returnUrl = nullableStr("return_url");
  const stateVersion = value.state_version;
  const byConfirmations = optionalAmounts(value.received_by_confirmations);
  if (
    byConfirmations === null ||
    !paymentId || !address || !qrText || !payUrl || !totalReceived || !createdAt ||
    !status || !PAYMENT_STATUSES.includes(status) ||
    label === undefined || message === undefined || amount === undefined || returnUrl === undefined ||
    typeof stateVersion !== "number" || !Number.isInteger(stateVersion)
  ) {
    return null;
  }
  return {
    payment_id: paymentId,
    address,
    qr_text: qrText,
    pay_url: payUrl,
    label,
    message,
    amount,
    total_received: totalReceived,
    ...(byConfirmations ? { received_by_confirmations: byConfirmations } : {}),
    state_version: stateVersion,
    status: status as PaymentStatusValue,
    return_url: returnUrl,
    created_at: createdAt,
  };
}

function requiredString(body: Record<string, unknown>, key: string, max: number): string {
  const value = body[key];
  if (typeof value !== "string" || value.trim() === "" || value.length > max) {
    throw new WebhookPayloadError(`${key} must be a non-empty string`);
  }
  return value;
}

function isRecord(value: unknown): value is Record<string, unknown> {
  return typeof value === "object" && value !== null && !Array.isArray(value);
}
