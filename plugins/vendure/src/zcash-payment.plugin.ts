import { OnApplicationBootstrap } from "@nestjs/common"
import { ConfigService, PluginCommonModule, Type, VendurePlugin } from "@vendure/core"
import { raw } from "express"
import { PLUGIN_OPTIONS, pluginOptions } from "./constants"
import { guardFulfillmentHandlers, zcashHoldFulfillmentProcess, zcashHoldOrderProcess } from "./hold"
import { validateOptions, type ZcashPluginOptions } from "./lib/options"
import { zcashPaymentHandler } from "./payment-handler"
import { WEBHOOK_PATH, ZcashWebhookController } from "./webhook.controller"
import { ZcashWebhookService } from "./webhook.service"

/**
 * Zcash payments through a self-hosted zcash-payment-service.
 *
 * ```ts
 * plugins: [ZcashPaymentPlugin.init({ serviceUrl, webhookSecret, zecRate: { usd: "0.025" } })]
 * ```
 * then create a payment method with the "zcash" handler in the admin.
 */
@VendurePlugin({
  imports: [PluginCommonModule],
  controllers: [ZcashWebhookController],
  providers: [ZcashWebhookService, { provide: PLUGIN_OPTIONS, useFactory: () => pluginOptions.current }],
  configuration: (config) => {
    config.paymentOptions.paymentMethodHandlers.push(zcashPaymentHandler)
    config.orderOptions.process = [...(config.orderOptions.process ?? []), zcashHoldOrderProcess]
    config.shippingOptions.process = [...(config.shippingOptions.process ?? []), zcashHoldFulfillmentProcess]
    // The webhook signature covers the exact bytes sent, so keep the raw body on that route.
    config.apiOptions.middleware.push({
      route: `/${WEBHOOK_PATH}`,
      handler: raw({ type: "*/*", limit: "64kb" }),
      beforeListen: true,
    })
    return config
  },
  compatibility: "^3.0.0",
})
export class ZcashPaymentPlugin implements OnApplicationBootstrap {
  constructor(private readonly config: ConfigService) {}

  onApplicationBootstrap() {
    // At bootstrap, so handlers added by plugins configured after this one are covered too.
    guardFulfillmentHandlers(this.config.shippingOptions.fulfillmentHandlers)
  }

  static init(options: ZcashPluginOptions): Type<ZcashPaymentPlugin> {
    validateOptions(options)
    pluginOptions.current = options
    return ZcashPaymentPlugin
  }
}
