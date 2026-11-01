# Security policy

## Reporting a vulnerability

Please report vulnerabilities privately through GitHub's
[private vulnerability reporting](https://github.com/nerdcash/zcash-payment-service/security/advisories/new).
Do not open a public issue, pull request, or discussion for a suspected vulnerability.

Include the affected component (service, Docker image, or Node client), the version or commit,
steps to reproduce, and the impact you expect. We'll acknowledge the report, keep you updated,
and credit you in the advisory unless you ask us not to.

## Supported versions

Security fixes go into the latest code only:

| Component | Supported |
| --- | --- |
| Service and Docker image | The latest `main` commit (`ghcr.io/nerdcash/zcash-payment-service:main`) |
| Node client (`@nerdcash/zcash-payment-client`) | The latest release on npm |

Older images and client versions don't receive backported fixes. Upgrade to get a fix.

## Scope and threat model

- **No spending keys.** The service is configured with a unified incoming viewing key (UIVK)
  only. A compromise of the service, its host, or its databases can reveal incoming payments
  to that key, but cannot spend funds.
- **Webhooks are signed, not encrypted.** Each webhook carries an `X-Signature` header: a hex
  HMAC-SHA256 of the raw body, keyed with `ZCASH_WEBHOOK_SECRET`. Receivers must verify it over
  the raw bytes with a constant-time comparison (the Node client's `verifyWebhook` does this)
  and should use an `https://` webhook URL. The signature has no timestamp, so receivers should
  treat deliveries idempotently: keep the highest `state_version` applied per address, as the
  [API reference](https://nerdcash.github.io/zcash-payment-service/api.html#webhook) describes.
- **The HTTP API has no authentication.** Expose it only to your application, never to the
  public internet. The pay page routes (`GET /pay/*` and `GET /payments/*`) are the exception:
  they are meant for payers, are keyed by an unguessable payment id, and can be published on
  their own through a reverse proxy.
- **Operators are responsible for their host.** That includes keeping the UIVK, the webhook
  secret, and the data directory private, choosing a trustworthy lightwalletd server, and
  keeping the image and host patched.

Issues in upstream dependencies (for example librustzcash or lightwalletd) should be reported
to those projects. Tell us too if they affect this service.
