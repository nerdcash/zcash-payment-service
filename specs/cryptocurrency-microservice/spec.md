# Cryptocurrency payment microservice contract

## Purpose

A host application may delegate blockchain-specific payment operations to a
separate cryptocurrency microservice.

This document defines the general contract and repository conventions that apply
to those services regardless of blockchain.

## Scope

This spec covers:

- ownership boundaries between a host application and a cryptocurrency
  microservice
- generic HTTP contract expectations
- error and idempotency expectations
- local orchestration and repository conventions
- documentation and change-management rules

This spec does not define any blockchain-specific wallet, address, memo, or
network rules. Those belong in the service-specific documentation for each
microservice.

## Ownership boundaries

### Host application owns

- subscription plans and status
- invoice creation and expiry
- quote calculation and fiat-to-crypto conversion
- entitlement changes after payment reconciliation
- public webhook signature verification when payment observations are posted to
  the host application
- realtime client notification after subscription changes

### Cryptocurrency microservice owns

- blockchain-specific payment destination allocation or selection
- blockchain-specific wallet URI / QR text construction
- any chain-specific observation, wallet, or address-management internals
- health endpoints needed for orchestration

### Cryptocurrency microservice does not own

- subscription status
- invoice persistence inside the host application unless a future contract says
  otherwise
- entitlement decisions
- public webhook signing rules for callbacks into the host application unless a
  future contract says otherwise

## Generic HTTP contract expectations

Each cryptocurrency microservice should expose at minimum:

- `GET /health`
- one or more payment-session allocation endpoints used by the host application

The exact request and response fields are blockchain-specific and should be
defined in the service-specific spec.

## Error semantics

- Microservices should prefer simple JSON error bodies.
- Non-success responses are treated by the host application as upstream service
  failures and logged accordingly.
- Blockchain-specific provider internals should not leak into the shared
  contract unless they are necessary for diagnostics or operator action.

## Idempotency and state expectations

- The host application remains responsible for invoice-level idempotency and
  duplicate webhook handling unless a service-specific contract says otherwise.
- Service-specific `POST` semantics may be stateless or stateful, but the
  expectation must be documented in that service's spec.

## Signing and auth boundaries

- Internal server-to-microservice requests are trusted by deployment topology by
  default unless a service-specific contract adds mutual auth or internal
  request signing.
- Public webhooks into the host application are owned and verified by the host
  application unless a service-specific contract says otherwise.

## Local orchestration

The local Docker Compose setup may run:

- a consumer application
- one or more cryptocurrency microservices

Recommended local wiring:

- each microservice has its own compose service and health check
- the host application addresses each service through Docker service discovery
- microservice base URLs are configured through explicit environment variables

## Documentation model

For each blockchain integration, keep two layers of documentation:

1. a generic host-app/service contract
2. a service-specific supplement next to that service's source

The shared contract defines the consistent host-app↔microservice model. The
service-specific supplement defines the exact blockchain-specific endpoints,
payloads, and behavior.

## Change policy

- This spec is the canonical shared contract for cryptocurrency microservices in
  this repository.
- Backward-incompatible changes to the shared host-app↔microservice model should
  update this document.
- Blockchain-specific changes should update the service-specific documentation
  for the affected microservice.
