import type { ZcashPluginOptions } from "./lib/options"

export const PLUGIN_OPTIONS = Symbol("ZCASH_PAYMENT_PLUGIN_OPTIONS")
export const loggerCtx = "ZcashPaymentPlugin"

/** Set by ZcashPaymentPlugin.init(); the payment handler and processes are plain objects and read it here. */
export const pluginOptions: { current?: ZcashPluginOptions } = {}

/** Order states that ship or deliver goods. A held order may not enter them. */
export const SHIPPING_ORDER_STATES = ["PartiallyShipped", "Shipped", "PartiallyDelivered", "Delivered"]
