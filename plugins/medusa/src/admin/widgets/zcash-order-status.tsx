import { defineWidgetConfig } from "@medusajs/admin-sdk"
import { Container, Heading, Text } from "@medusajs/ui"
import type { AdminOrder, DetailWidgetProps } from "@medusajs/framework/types"

/** Shows Zcash holds and review notes on the order page, so nobody fulfills a held order. */
const ZcashOrderStatus = ({ data: order }: DetailWidgetProps<AdminOrder>) => {
  const hold = order.metadata?.zcash_payment_hold
  const review = order.metadata?.zcash_payment_review
  if (typeof hold !== "string" && typeof review !== "string") {
    return null
  }
  return (
    <Container className="divide-y p-0">
      <div className="px-6 py-4">
        <Heading level="h2">Zcash payment</Heading>
      </div>
      {typeof hold === "string" && (
        <div className="px-6 py-4">
          <Text weight="plus" className="text-ui-fg-error">
            On hold: do not fulfill
          </Text>
          <Text size="small">{hold}</Text>
        </div>
      )}
      {typeof review === "string" && (
        <div className="px-6 py-4">
          <Text weight="plus">Needs review</Text>
          <Text size="small">{review}</Text>
        </div>
      )}
    </Container>
  )
}

export const config = defineWidgetConfig({
  zone: "order.details.side.before",
})

export default ZcashOrderStatus
