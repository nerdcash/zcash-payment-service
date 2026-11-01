// Seeds the e2e store: one USD region that offers Zcash, free shipping, and one $20 product.
import type { ExecArgs } from "@medusajs/framework/types"
import { ContainerRegistrationKeys, Modules } from "@medusajs/framework/utils"
import {
  createApiKeysWorkflow,
  createProductsWorkflow,
  createRegionsWorkflow,
  createSalesChannelsWorkflow,
  createShippingOptionsWorkflow,
  createShippingProfilesWorkflow,
  createStockLocationsWorkflow,
  linkSalesChannelsToApiKeyWorkflow,
  linkSalesChannelsToStockLocationWorkflow,
  updateStoresWorkflow,
} from "@medusajs/medusa/core-flows"

export default async function seed({ container }: ExecArgs) {
  const link = container.resolve(ContainerRegistrationKeys.LINK)
  const query = container.resolve(ContainerRegistrationKeys.QUERY)
  const fulfillment = container.resolve(Modules.FULFILLMENT)
  const storeModule = container.resolve(Modules.STORE)

  const { data: existing } = await query.graph({ entity: "region", fields: ["id"] })
  if (existing.length) {
    return
  }

  const [store] = await storeModule.listStores()
  // Medusa creates a default sales channel and publishable key on first migration; reuse them.
  const { data: channels } = await query.graph({ entity: "sales_channel", fields: ["id"] })
  const channel =
    channels[0] ??
    (await createSalesChannelsWorkflow(container).run({ input: { salesChannelsData: [{ name: "Web" }] } })).result[0]
  await updateStoresWorkflow(container).run({
    input: {
      selector: { id: store.id },
      update: {
        supported_currencies: [{ currency_code: "usd", is_default: true }],
        default_sales_channel_id: channel.id,
      },
    },
  })
  const { result: [region] } = await createRegionsWorkflow(container).run({
    input: {
      regions: [{ name: "US", currency_code: "usd", countries: ["us"], payment_providers: ["pp_zcash_zcash"] }],
    },
  })
  const { result: [location] } = await createStockLocationsWorkflow(container).run({
    input: { locations: [{ name: "Warehouse", address: { address_1: "1 Main St", city: "Denver", country_code: "us" } }] },
  })
  await link.create({
    [Modules.STOCK_LOCATION]: { stock_location_id: location.id },
    [Modules.FULFILLMENT]: { fulfillment_provider_id: "manual_manual" },
  })
  const { result: [profile] } = await createShippingProfilesWorkflow(container).run({
    input: { data: [{ name: "Default", type: "default" }] },
  })
  const set = await fulfillment.createFulfillmentSets({
    name: "Warehouse delivery",
    type: "shipping",
    service_zones: [{ name: "US", geo_zones: [{ country_code: "us", type: "country" }] }],
  })
  await link.create({
    [Modules.STOCK_LOCATION]: { stock_location_id: location.id },
    [Modules.FULFILLMENT]: { fulfillment_set_id: set.id },
  })
  await createShippingOptionsWorkflow(container).run({
    input: [
      {
        name: "Free shipping",
        price_type: "flat",
        provider_id: "manual_manual",
        service_zone_id: set.service_zones[0].id,
        shipping_profile_id: profile.id,
        type: { label: "Free", description: "Free shipping", code: "free" },
        prices: [{ currency_code: "usd", amount: 0 }, { region_id: region.id, amount: 0 }],
        rules: [
          { attribute: "enabled_in_store", value: "true", operator: "eq" },
          { attribute: "is_return", value: "false", operator: "eq" },
        ],
      },
    ],
  })
  await linkSalesChannelsToStockLocationWorkflow(container).run({ input: { id: location.id, add: [channel.id] } })
  const { data: keys } = await query.graph({
    entity: "api_key",
    fields: ["id", "sales_channels.id"],
    filters: { type: "publishable" },
  })
  const key =
    keys[0] ??
    (
      await createApiKeysWorkflow(container).run({
        input: { api_keys: [{ title: "Web", type: "publishable", created_by: "" }] },
      })
    ).result[0]
  if (!(keys[0]?.sales_channels ?? []).some((c: any) => c?.id === channel.id)) {
    await linkSalesChannelsToApiKeyWorkflow(container).run({ input: { id: key.id, add: [channel.id] } })
  }
  await createProductsWorkflow(container).run({
    input: {
      products: [
        {
          title: "Zcash T-shirt",
          handle: "zcash-t-shirt",
          status: "published",
          shipping_profile_id: profile.id,
          options: [{ title: "Size", values: ["M"] }],
          variants: [{ title: "M", sku: "ZEC-TEE-M", manage_inventory: false, options: { Size: "M" }, prices: [{ currency_code: "usd", amount: 20 }] }],
          sales_channels: [{ id: channel.id }],
        },
      ],
    },
  })
}
