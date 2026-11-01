import assert from "node:assert/strict";
import { createHmac } from "node:crypto";
import { describe, it } from "node:test";
import {
  createZcashPaymentClient,
  PaymentServiceError,
  receivedWithConfirmations,
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
      return_url: null,
    });
  });

  it("sends returnUrl and returns the pay page fields", async () => {
    const calls: Call[] = [];
    const id = "0123456789abcdef0123456789abcdef";
    const client = createZcashPaymentClient({
      baseUrl: "http://service.test",
      webhookSecret: SECRET,
      fetchImpl: fakeFetch(
        200,
        JSON.stringify({ address: "u1abc", qr_text: "zcash:u1abc", payment_id: id, pay_url: `/pay/${id}` }),
        calls,
      ),
    });
    const session = await client.createPaymentSession({ amount: "1", returnUrl: "https://shop.test/done" });
    assert.deepEqual(session, { address: "u1abc", qr_text: "zcash:u1abc", payment_id: id, pay_url: `/pay/${id}` });
    assert.equal(JSON.parse(String(calls[0]?.init?.body)).return_url, "https://shop.test/done");
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

  it("reports a failed response body read as a service error", async () => {
    const client = createZcashPaymentClient({
      baseUrl: "http://service.test",
      webhookSecret: SECRET,
      fetchImpl: (async () => {
        const body = new ReadableStream({
          start(controller) {
            controller.error(new TypeError("terminated"));
          },
        });
        return new Response(body, { status: 200 });
      }) as typeof fetch,
    });
    await assert.rejects(client.createPaymentSession({}), (error: unknown) => {
      assert.ok(error instanceof PaymentServiceError);
      assert.equal(error.status, 200);
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

  it("passes received_by_confirmations through and rejects a malformed one", () => {
    const withDepths = { ...observation, received_by_confirmations: ["0.04000000", "0.04000000", "0.00000000"] };
    const body = JSON.stringify(withDepths);
    assert.deepEqual(client.verifyWebhook(Buffer.from(body), sign(body)), withDepths);
    for (const bad of [[], "0.1", [1], [""], Array(102).fill("0.00000000")]) {
      const badBody = JSON.stringify({ ...observation, received_by_confirmations: bad });
      assert.throws(() => client.verifyWebhook(Buffer.from(badBody), sign(badBody)), WebhookPayloadError);
    }
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

describe("getPayment", () => {
  const id = "0123456789abcdef0123456789abcdef";
  const status = {
    payment_id: id,
    address: "u1abc",
    qr_text: "zcash:u1abc?amount=0.5",
    pay_url: `/pay/${id}`,
    label: "Order 1",
    message: null,
    amount: "0.5",
    total_received: "0.20000000",
    state_version: 1,
    status: "partially_paid",
    return_url: null,
    created_at: "2026-10-31T00:00:00Z",
  };

  it("reads the status route", async () => {
    const calls: Call[] = [];
    const client = createZcashPaymentClient({
      baseUrl: "http://service.test/x",
      webhookSecret: SECRET,
      fetchImpl: fakeFetch(200, JSON.stringify(status), calls),
    });
    assert.deepEqual(await client.getPayment(id), status);
    assert.equal(calls[0]?.url, `http://service.test/payments/${id}`);
    assert.equal(calls[0]?.init?.method, "GET");
  });

  it("reads received_by_confirmations from the status route", async () => {
    const withDepths = { ...status, received_by_confirmations: ["0.20000000", "0.00000000"] };
    const client = createZcashPaymentClient({
      baseUrl: "http://service.test",
      webhookSecret: SECRET,
      fetchImpl: fakeFetch(200, JSON.stringify(withDepths)),
    });
    assert.deepEqual(await client.getPayment(id), withDepths);
  });

  it("rejects malformed ids without calling the service", async () => {
    const calls: Call[] = [];
    const client = createZcashPaymentClient({
      baseUrl: "http://service.test",
      webhookSecret: SECRET,
      fetchImpl: fakeFetch(200, "{}", calls),
    });
    await assert.rejects(client.getPayment("../health"), PaymentServiceError);
    assert.equal(calls.length, 0);
  });

  it("maps 404 and unexpected payloads to PaymentServiceError", async () => {
    const missing = createZcashPaymentClient({
      baseUrl: "http://service.test",
      webhookSecret: SECRET,
      fetchImpl: fakeFetch(404, JSON.stringify({ error: "not found: unknown payment" })),
    });
    await assert.rejects(missing.getPayment(id), (error: unknown) => {
      assert.ok(error instanceof PaymentServiceError);
      assert.equal(error.status, 404);
      assert.equal(error.message, "not found: unknown payment");
      return true;
    });
    const odd = createZcashPaymentClient({
      baseUrl: "http://service.test",
      webhookSecret: SECRET,
      fetchImpl: fakeFetch(200, JSON.stringify({ ...status, status: "maybe" })),
    });
    await assert.rejects(odd.getPayment(id), PaymentServiceError);
  });
});

describe("receivedWithConfirmations", () => {
  it("indexes the array and falls back to total_received", () => {
    const depths = { total_received: "0.3", received_by_confirmations: ["0.3", "0.2", "0.1"] };
    assert.equal(receivedWithConfirmations(depths, 0), "0.3");
    assert.equal(receivedWithConfirmations(depths, 2), "0.1");
    assert.throws(() => receivedWithConfirmations(depths, 9), RangeError);
    assert.throws(() => receivedWithConfirmations(depths, 3), RangeError);
    assert.throws(() => receivedWithConfirmations(depths, -1), RangeError);
    assert.throws(() => receivedWithConfirmations(depths, 1.5), RangeError);
    assert.equal(receivedWithConfirmations({ total_received: "0.3" }, 2), "0.3");
  });
});
