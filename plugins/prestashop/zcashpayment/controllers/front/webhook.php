<?php
/**
 * POST endpoint for zcash-payment-service's signed webhooks:
 * index.php?fc=module&module=zcashpayment&controller=webhook
 */

if ( ! defined( '_PS_VERSION_' ) ) {
	exit;
}

class ZcashpaymentWebhookModuleFrontController extends ModuleFrontController {
	/** @var bool */
	public $ajax = true;

	// The service must reach the webhook even when the shop is in maintenance or geo-restricted.
	protected function displayMaintenancePage() {
	}

	protected function displayRestrictedCountryPage() {
	}

	public function postProcess() {
		if ( 'POST' !== ( $_SERVER['REQUEST_METHOD'] ?? '' ) ) {
			$this->respond( 405, array( 'error' => 'POST only' ) );
		}
		$raw       = (string) file_get_contents( 'php://input' );
		$signature = isset( $_SERVER['HTTP_X_SIGNATURE'] ) ? (string) $_SERVER['HTTP_X_SIGNATURE'] : null;
		try {
			[ $status, $body ] = ZcashPaymentWebhook::handle( $raw, $signature );
		} catch ( Throwable $e ) {
			PrestaShopLogger::addLog( 'Zcash: webhook failed: ' . $e->getMessage(), 3 );
			[ $status, $body ] = array( 500, array( 'error' => 'internal error, retry' ) );
		}
		$this->respond( $status, $body );
	}

	private function respond( int $status, array $body ): void {
		http_response_code( $status );
		header( 'Content-Type: application/json' );
		header( 'Cache-Control: no-store' );
		echo json_encode( $body );
		exit;
	}
}
