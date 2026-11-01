# Comparison with other processors

Several open-source projects let a merchant accept Zcash without giving up custody. This page
compares zcash-payment-service with two of them, [CipherPay](https://cipherpay.app) and
[BTCPay Server](https://btcpayserver.org) with its Zcash plugin, so you can pick the one that
fits your store. Facts about the other projects come from their own sites and repositories,
linked in the table, as of October 2026. Check those sources for anything that may have changed.

| | zcash-payment-service | CipherPay | BTCPay Server + Zcash plugin |
| --- | --- | --- | --- |
| Hosting model | Self-hosted only: one container, or a Rust crate embedded in your process | Hosted service at cipherpay.app; the API is also [self-hostable](https://github.com/atmospherelabs-dev/cipherpay-api) | Self-hosted BTCPay Server instance ([plugin](https://github.com/btcpay-zcash/btcpayserver-zcash-plugin)) |
| Fees | None | [1% of confirmed payments](https://github.com/atmospherelabs-dev/cipherpay-api#readme), settled in ZEC (`BILLING_FEE_RATE`, configurable when self-hosting) | None charged by BTCPay Server |
| Custody | Non-custodial: payers pay the merchant's own wallet | Non-custodial | Non-custodial |
| Third parties in a payment | None beyond the lightwalletd server you choose | CipherPay runs the checkout and holds the viewing key when you use the hosted service; self-hosted deployments depend on [CipherScan](https://github.com/atmospherelabs-dev/cipherpay-api#readme) | None beyond your chosen node or lightwalletd server |
| Key required | Unified incoming viewing key (UIVK) | Unified full viewing key ([UFVK](https://github.com/atmospherelabs-dev/cipherpay-api#readme)), stored encrypted | Viewing key entered in the plugin settings, via [zcash-walletd](https://github.com/hhanh00/zcash-walletd) |
| Chain access | lightwalletd | Not verified | lightwalletd, with an optional Zebra full node ([install guide](https://github.com/btcpay-zcash/btcpayserver-zcash-plugin/blob/master/docs/installation.md)) |
| Zcash support status | Production | Production ([cipherpay.app](https://cipherpay.app)) | Plugin 1.0.2 published June 2025 ([forum](https://forum.zcashcommunity.com/t/upgrade-btcpayserver-zcash-plugin-for-2-1/50800)). The plugin shares one Zcash wallet across every store on the instance |
| Store plugins | [WooCommerce](woocommerce.md) and [Medusa](medusa.md) plugins, plus the HTTP API, signed webhooks, and a [Node.js client](node-client.md) | Shopify app and WooCommerce plugin, plus payment links and a POS | BTCPay Server's own store integrations (not verified for Zcash specifically) |
| License | MIT | MIT ([cipherpay-api](https://github.com/atmospherelabs-dev/cipherpay-api)) | MIT ([BTCPay Server](https://github.com/btcpayserver/btcpayserver), [plugin](https://github.com/btcpay-zcash/btcpayserver-zcash-plugin)) |

## When to choose this service

- **No middleman.** The payer sends ZEC straight to your wallet, and your own server notices
  it. No processor stands between you and your customer, and nobody else holds your viewing key.
- **No fees.** There is no account, usage meter, or percentage.
- **Small footprint.** You run one more container or link one more crate. You don't need a full
  node or a BTCPay Server instance.
- **Least privilege.** It needs only an incoming viewing key, so it can see payments but never
  spend them.

## When another option may fit better

- You sell on Shopify and want a ready-made app: CipherPay offers one.
- You don't want to run any server process: CipherPay's hosted service handles that.
- You already run BTCPay Server for other currencies and don't share the instance: the Zcash
  plugin adds ZEC there.
