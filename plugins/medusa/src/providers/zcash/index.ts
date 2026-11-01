import { ModuleProvider, Modules } from "@medusajs/framework/utils"
import { ZcashPaymentProviderService } from "./service"

export default ModuleProvider(Modules.PAYMENT, {
  services: [ZcashPaymentProviderService],
})
