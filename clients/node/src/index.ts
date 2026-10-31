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
};

export type PaymentSession = {
  address: string;
  qr_text: string;
};

export type PaymentObservation = {
  event_id: string;
  payment_source: "zcash";
  address: string;
  total_received: string;
  observed_at: string;
};

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
  verifyWebhook(rawBody: Uint8Array, signature: string | undefined): PaymentObservation;
};

export type ZcashPaymentClientOptions = {
  /** Base URL of the zcash-payment-service, e.g. `http://localhost:8080`. */
  baseUrl: string;
  /** Shared secret the service uses to sign webhooks (HMAC-SHA256). */
  webhookSecret: string;
  /** Optional fetch implementation, for tests or custom transports. */
  fetchImpl?: typeof fetch;
  /** Request timeout in milliseconds for createPaymentSession. Defaults to 10000. */
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

      return { address: payload.address, qr_text: payload.qr_text };
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

  return {
    event_id: eventId,
    payment_source: PAYMENT_SOURCE,
    address,
    total_received: totalReceived,
    observed_at: observedAt,
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
