<?php
/**
 * Reads the amount received at an address from the service's messages.
 *
 * Every credited total in this module comes through here, so a change to how the service
 * reports balances (for example a balance per confirmation depth) only needs this class.
 */

if ( ! defined( '_PS_VERSION_' ) ) {
	exit;
}

final class ZcashPaymentBalance {
	/**
	 * Zatoshis to credit from a decoded webhook body, or null when it carries no valid total.
	 *
	 * @param array<string,mixed> $data
	 */
	public static function from_webhook( array $data ): ?int {
		return self::total_received( $data );
	}

	/**
	 * Zatoshis received according to GET /payments/{id}, or null when unknown.
	 *
	 * @param array<string,mixed> $status
	 */
	public static function from_status( array $status ): ?int {
		return self::total_received( $status );
	}

	/** @param array<string,mixed> $data */
	private static function total_received( array $data ): ?int {
		$total = $data['total_received'] ?? null;
		return is_string( $total ) ? ZcashPaymentUtil::zec_to_zat( $total ) : null;
	}
}
