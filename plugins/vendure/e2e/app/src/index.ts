import { bootstrap, bootstrapWorker } from "@vendure/core"
import { populate } from "@vendure/core/cli"
import path from "node:path"
import { config } from "./vendure-config"

async function main() {
  if (process.argv[2] === "populate") {
    // Countries, tax, shipping, the Zcash payment method, and one product.
    const app = await populate(
      () => bootstrap(config),
      require(path.join(__dirname, "../initial-data.json")),
      path.join(__dirname, "../products.csv")
    )
    await app.close()
    return
  }
  const app = await bootstrap(config)
  const worker = await bootstrapWorker(config)
  await worker.startJobQueue()
  void app
}

main().catch((e) => {
  console.error(e)
  process.exit(1)
})
