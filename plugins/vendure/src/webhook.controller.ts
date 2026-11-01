import { Controller, HttpCode, Post, Req, Res } from "@nestjs/common"
import type { Request, Response } from "express"
import { ZcashWebhookService } from "./webhook.service"

export const WEBHOOK_PATH = "zcash-payments/webhook"

/**
 * Receives signed payment observations from zcash-payment-service. Point the service's
 * ZCASH_WEBHOOK_URL here: https://<vendure server>/zcash-payments/webhook
 */
@Controller(WEBHOOK_PATH)
export class ZcashWebhookController {
  constructor(private readonly service: ZcashWebhookService) {}

  @Post()
  @HttpCode(200)
  async post(@Req() req: Request, @Res() res: Response) {
    const result = await this.service.handle(req.body as Buffer | undefined, req.headers["x-signature"])
    res.status(result.status).json(result.body)
  }
}
