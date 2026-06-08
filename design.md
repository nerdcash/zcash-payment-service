# zcash-payment-service design

This document records the service design decisions that shape the implementation.
It travels with the microservice in its own repository so it can be reused by
multiple host applications.

## Purpose

The service allocates Zcash payment destinations and will eventually scan the
chain for incoming payments, coalesce address totals, and report those totals to
an upstream billing system by webhook.

The service is designed so it can be used by any Zcash-backed application that
needs address allocation, payment observation, and webhook-style reconciliation.

## Core decisions

### Wallet model

- The service is designed around exactly one Unified Incoming Viewing Key
  (UIVK).
- The service is read-only. It observes incoming payments and reports cumulative
  totals. It does not spend funds.
- The canonical UIVK is stored in the service-owned app database.
- The startup input is a UIVK supplied directly by the operator.
- The UIVK may include a transparent component.
- That transparent capability is currently tolerated but ignored by this
  service.
- Outside the scanner-key construction boundary, the service should continue to
  operate only on the canonical UIVK.
- On subsequent boots, the supplied UIVK must match the persisted canonical
  UIVK exactly or startup fails.

### Database separation

- The service uses two SQLite files.
- The app database is owned by this service and stores operational state such as
  wallet identity, issued addresses, totals, reorg records, and webhook
  delivery state.
- The future wallet database owned by `zcash_client_sqlite` is kept separate so
  this service does not depend on or mutate librustzcash-managed schema beyond
  supported APIs.

### Build isolation

- The service is isolated into its own nested Cargo workspace inside the repo.
- This allows the microservice to carry SQLite and Zcash crate versions that may
  intentionally diverge from the dependency stack used by any host application.
- That isolation also keeps the crate reusable as a standalone repository.

### Address model

- Each issued payment destination is a unified address.
- Phase 1 requires an Orchard receiver and a Sapling receiver, with no
  transparent receiver.
- The current implementation derives a fresh unified address directly from the
  canonical UIVK for every payment session.

### Reporting model

- The upstream webhook reports `total_received`, which means the cumulative
  total of eligible incoming payments observed for one issued address.
- The reported value is not wallet balance.
- The service suppresses webhook delivery while it is materially behind the tip.
- Once it catches up, it sends at most one authoritative webhook per dirty
  address for the resulting stable chain view.

### Confirmation thresholds

Two thresholds are intentionally distinct:

- `WEBHOOK_REPORT_CONFIRMATIONS`: the minimum confirmations required before a
  receipt can affect `total_received`. The current value is `0`.
- `FINALITY_CONFIRMATIONS`: the depth at which receipts are treated as finalized
  for checkpointing and aggregation optimization. The current value is `100`.

The reporting threshold is a named code constant in phase 1. A future phase may
make it configurable. The value `0` enables provisional mempool receipt
reporting, including eviction reconciliation and 24-hour aging until a receipt
is confirmed in a mined block.

### Reorg handling

- Reorgs are treated as authoritative changes to the cumulative total for one or
  more issued addresses.
- When a reorg is noticed, the service returns to catch-up mode, recomputes
  address totals against the new stable chain view, and sends corrected totals
  only after it is caught up again.

### Logging

- The service keeps a rolling file log.
- Important events to log include startup integrity checks, entering and exiting
  catch-up mode, every address request, every discovered payment, every noticed
  reorg, and every webhook attempt with HTTP result details.
- The log is for operators. It is not the source of truth; the app database is.

## Current implementation status

Implemented in this slice:

- persistent app database schema
- startup wallet-identity integrity checks
- named confirmation constants persisted for diagnostics
- rolling file logging
- UIVK startup input used directly as the canonical wallet identity and scanner keys
- real UIVK parsing and fresh unified-address derivation
- compact-block incoming-payment scanning and amount aggregation primitives
- receiver-level receipt attribution and app-DB receipt persistence
- payment-session endpoint backed by persistent issued-address storage
- long-lived sync with mempool-stream wakeups, persisted scan progress, backpressured download/scan pipelining, and recent-overlap reorg reconciliation
- webhook queue population for dirty addresses during catch-up batches
- receipt-maturity advancement as the chain tip moves
- durable webhook worker that signs and sends queued observations with exponential backoff and a max-attempt ceiling
- provisional mempool receipt tracking with eviction reconciliation and 24-hour aging when the reporting threshold is zero
- design baseline and operational documentation

Not yet implemented in this slice:

- deeper deterministic mempool integration tests with a mock lightwalletd surface; the current implementation relies on live network coverage for end-to-end transport behavior because upstream only generates the lightwalletd client, not a server test harness

## Reuse boundary

The intended reuse boundary is:

- keep the HTTP interface generic and billing-system-agnostic where possible
- keep UIVK-driven address allocation and scanning logic self-contained
- keep the app database schema and design document with the service
- adapt only the webhook contract and environment wiring for each host system
