# Payment links and the pay page

Instead of drawing the address and QR code in your own checkout, you can send the payer to a
pay page that the service serves itself. It runs in the same container as the rest of the
service, on your own host. No other party sees the payment.

1. Your application calls `POST /payment-session` as usual, optionally with a `return_url`.
2. The response includes `payment_id` and `pay_url`. Redirect the payer to `pay_url`, or send it
   to them as a payment link.
3. The page shows the amount, a QR code of the ZIP-321 `zcash:` URI, an **Open in wallet**
   button, and the address. It polls `GET /payments/{payment_id}` every few seconds, and reloads when the payment request changes (after a partial payment it asks only for the rest).
4. When the received total covers the amount, the page says so and, if you gave a
   `return_url`, sends the payer back there. Without a `return_url` the page keeps polling, so it
   also shows a receipt that is later dropped (for example an unconfirmed transaction that never
   mines).
5. If the URI is too long for a QR code (a very long `message`), the page shows a note in place of
   the code; the wallet button and address still work.

Your application still learns about the payment from the signed [webhook](integration.md#learning-that-a-payment-arrived).
The redirect is only for the payer's convenience. Never mark an order paid because the payer
arrived at your `return_url`: anyone can open that URL.

## Creating a link

```sh
curl -s -X POST http://127.0.0.1:8787/payment-session \
  -H "Content-Type: application/json" \
  -d '{"payment_source":"zcash","label":"Order 1001","amount":"0.25","return_url":"https://shop.example/orders/1001"}'
```

```json
{
  "address": "u1...",
  "qr_text": "zcash:u1...?amount=0.25",
  "payment_id": "3f9c0d6e1b2a4c5d8e7f6a5b4c3d2e1f",
  "pay_url": "https://pay.shop.example/pay/3f9c0d6e1b2a4c5d8e7f6a5b4c3d2e1f"
}
```

`payment_id` is 128 random bits in hex, so a link can't be guessed from another one. `pay_url`
is absolute when `PUBLIC_BASE_URL` is set and a path such as `/pay/<payment_id>` otherwise.

`return_url` must be an absolute `http` or `https` URL without user info (`user@host`). Anything
else, such as `javascript:` or `data:`, is rejected with `400`.

To limit where the pay page can send payers, set `RETURN_URL_ALLOWED_ORIGINS` to a
comma-separated list of origins, such as `https://shop.example,https://www.shop.example`. A
`return_url` on any other origin is rejected with `400`. When it is unset, any `http(s)` URL is
accepted. That is safe only while `POST /payment-session` stays private (see below), since only
your application can then choose a `return_url`. Setting it is recommended as a second line of
defense: if the session endpoint were ever exposed by mistake, nobody could use your pay page to
send payers to their own site.

Every session gets a payment id, whether or not you use the page. The existing `address` and
`qr_text` fields are unchanged.

## Status

`GET /payments/{payment_id}` returns JSON. The pay page uses it, and your application can too.

| Field | Meaning |
| --- | --- |
| `payment_id` | The id from the session. |
| `address`, `qr_text`, `pay_url` | As returned when the session was created. |
| `label`, `message`, `amount` | The values the session was created with, or `null`. |
| `total_received` | The address's eligible total in ZEC, the same number the webhook carries. |
| `state_version` | Increments each time `total_received` changes. |
| `status` | `awaiting_payment`, `partially_paid`, `paid`, or `received`. |
| `return_url` | The session's `return_url`, or `null`. |
| `created_at` | RFC 3339 UTC time the link was created. |

`status` compares `total_received` with `amount`:

| Status | When |
| --- | --- |
| `awaiting_payment` | Nothing eligible has arrived. |
| `partially_paid` | Something arrived, but less than `amount`. |
| `paid` | `total_received` is at least `amount`. |
| `received` | Something arrived, and `amount` was missing or isn't a plain ZEC decimal (up to 8 decimal places), so it can't be compared. |

Eligibility follows `WEBHOOK_REPORT_CONFIRMATIONS`, exactly as for webhooks. With the default
`0`, the page turns to paid as soon as the transaction reaches the mempool. Set a higher value if
you'd rather wait for confirmations.

An unknown or malformed id returns `404` with `{"error":"not found: unknown payment"}`.

## Exposing the page

The pay page and the status route are meant to be reached by payers. `POST /payment-session`
is not: it has no authentication, and anyone who can reach it can mint addresses. Put a reverse
proxy in front of the service and forward only these paths to the public:

- `GET /pay/*`
- `GET /payments/*`

Keep everything else on your private network. Then set `PUBLIC_BASE_URL` to the public origin
(and path prefix, if any) the proxy serves, such as `https://pay.shop.example`. The page fetches
status with a relative URL, so it also works under a path prefix.

A Caddy example that publishes only the pay page:

```text
pay.shop.example {
    @public path /pay/* /payments/*
    handle @public {
        reverse_proxy zcash-payment-service:8787
    }
    respond 404
}
```

The page loads no scripts, styles, fonts, or images from anywhere else. It is sent with a
strict `Content-Security-Policy`, `Referrer-Policy: no-referrer`, and `Cache-Control: no-store`.

## What it doesn't do

- It doesn't expire links or lock an exchange rate. Price the order in ZEC when you create the
  session, and decide in your application how long you honor that price.
- It doesn't refund overpayments or chase underpayments. `partially_paid` tells the payer to
  send the rest. Your application decides what to do with any difference.
