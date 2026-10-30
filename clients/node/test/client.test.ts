import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { describe, it } from "node:test";
import {
  createZcashPaymentClient,
  PaymentServiceError,
  WebhookPayloadError,
  WebhookSignatureError,
} from "../src/index.js";

const SECRET = "test-webhook-secret";

type Call = { url: string; init: RequestInit | undefined };

function fakeFetch(status: number, body: string, calls: Call[] = []): typeof fetch {
  return (async (input: string | URL | Request, init?: RequestInit) => {
    calls.push({ url: String(input), init });
    return new Response(body, { status, headers: { "content-type": "application/json" } });
  }) as typeof fetch;
}

function sign(body: string): string {
  return createHmac("sha256", SECRET).update(body).digest("hex");
}

const observation = {
  event_id: "zcash:7:v1",
  payment_source: "zcash",
  address: "u1testaddress",
  total_received: "0.04000000",
  observed_at: "2026-10-30T00:00:00Z",
};

describe("createPaymentSession", () => {
  it("posts the service contract and returns the session", async () => {
    const calls: Call[] = [];
    const client = createZcashPaymentClient({
      baseUrl: "http://service.test/ignored?x=1#y",
      webhookSecret: SECRET,
      fetchImpl: fakeFetch(200, JSON.stringify({ address: "u1abc", qr_text: "zcash:u1abc" }), calls),
    });
    const session = await client.createPaymentSession({ label: "Cup", amount: "0.05000000" });
    assert.deepEqual(session, { address: "u1abc", qr_text: "zcash:u1abc" });
    assert.equal(calls.length, 1);
    assert.equal(calls[0]?.url, "http://service.test/payment-session");
    assert.equal(calls[0]?.init?.method, "POST");
    assert.deepEqual(JSON.parse(String(calls[0]?.init?.body)), {
      payment_source: "zcash",
      label: "Cup",
      memo: null,
      message: null,
      amount: "0.05000000",
    });
  });

  it("surfaces the service's error message and status", async () => {
    const client = createZcashPaymentClient({
      baseUrl: "http://service.test",
      webhookSecret: SECRET,
      fetchImpl: fakeFetch(503, JSON.stringify({ error: "wallet error: uivk unavailable" })),
    });
    await assert.rejects(client.createPaymentSession({}), (error: unknown) => {
      assert.ok(error instanceof PaymentServiceError);
      assert.equal(error.status, 503);
      assert.equal(error.message, "wallet error: uivk unavailable");
      return true;
    });
  });

  it("rejects an unexpected payload", async () => {
    const client = createZcashPaymentClient({
      baseUrl: "http://service.test",
      webhookSecret: SECRET,
      fetchImpl: fakeFetch(200, JSON.stringify({ address: "" })),
    });
    await assert.rejects(client.createPaymentSession({}), PaymentServiceError);
  });

  it("reports an unreachable service with status 0", async () => {
    const client = createZcashPaymentClient({
      baseUrl: "http://service.test",
      webhookSecret: SECRET,
      fetchImpl: (async () => {
        throw new TypeError("fetch failed");
      }) as typeof fetch,
    });
    await assert.rejects(client.createPaymentSession({}), (error: unknown) => {
      assert.ok(error instanceof PaymentServiceError);
      assert.equal(error.status, 0);
      return true;
    });
  });

  it("requires a webhook secret", () => {
    assert.throws(() => createZcashPaymentClient({ baseUrl: "http://service.test", webhookSecret: "" }));
  });
});

describe("verifyWebhook", () => {
  const client = createZcashPaymentClient({ baseUrl: "http://service.test", webhookSecret: SECRET });

  it("accepts a correctly signed observation", () => {
    const body = JSON.stringify(observation);
    assert.deepEqual(client.verifyWebhook(Buffer.from(body), sign(body)), observation);
    assert.deepEqual(client.verifyWebhook(new TextEncoder().encode(body), sign(body).toUpperCase()), observation);
  });

  it("rejects a missing, malformed, or wrong signature", () => {
    const body = JSON.stringify(observation);
    assert.throws(() => client.verifyWebhook(Buffer.from(body), undefined), WebhookSignatureError);
    assert.throws(() => client.verifyWebhook(Buffer.from(body), "nothex"), WebhookSignatureError);
    const tampered = JSON.stringify({ ...observation, total_received: "9.00000000" });
    assert.throws(() => client.verifyWebhook(Buffer.from(tampered), sign(body)), WebhookSignatureError);
  });

  it("rejects signed but invalid payloads", () => {
    for (const body of [
      "not json",
      "[]",
      JSON.stringify({ ...observation, payment_source: "btc" }),
      JSON.stringify({ ...observation, event_id: "" }),
    ]) {
      assert.throws(() => client.verifyWebhook(Buffer.from(body), sign(body)), WebhookPayloadError);
    }
  });
});
