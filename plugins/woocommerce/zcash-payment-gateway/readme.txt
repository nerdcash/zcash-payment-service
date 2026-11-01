=== Zcash Payment Gateway (self-hosted) ===
Tags: zcash, zec, crypto, payments, woocommerce
Requires at least: 6.4
Requires PHP: 8.1
Stable tag: 0.1.0
License: MIT

Accept shielded Zcash in WooCommerce through your own zcash-payment-service. Payments go straight to your wallet. No middleman, no fees.

== Description ==

The plugin asks your self-hosted zcash-payment-service for a fresh address for each order, sends
the customer to the service's pay page (QR code and live status), and marks the order paid when
the service's signed webhook reports enough ZEC.

Setup: https://nerdcash.github.io/zcash-payment-service/woocommerce.html
