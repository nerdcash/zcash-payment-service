<?php
/**
 * Reads the amount received at an address from the service's messages.
 *
 * Every credited total in this module comes through here. The service reports two amounts:
 *
 * - "seen": `total_received`, everything received at the address, mempool included (index 0
 *   of `received_by_confirmations` when that is present).
 * - "settled": what has the service's `REQUIRED_CONFIRMATIONS`: the last element of
 *   `received_by_confirmations`, or `total_received` when the service doesn't send the array
 *   (older services, or `REQUIRED_CONFIRMATIONS` unset).
 *
 * An order is marked paid only when the settled amount covers it. A seen amount that covers it
 * is reported as received and pending confirmation.
 */

if ( ! defined( '_PS_VERSION_' ) ) {
	exit;
}

final class ZcashPaymentBalance {
	/** The service sends at most REQUIRED_CONFIRMATIONS + 1 elements, and caps that at 100. */
	const MAX_DEPTHS = 101;

	/**
	 * Decodes a JSON object from the service into an array, or null. Associative decoding turns
	 * a numeric-keyed object into a PHP list, so `received_by_confirmations` is checked against
	 * an object decode to make sure it really is a JSON array.
	 *
	 * @return array<string,mixed>|null
	 */
	public static function decode( string $raw ): ?array {
		$data   = json_decode( $raw, true );
		$object = json_decode( $raw );
		if ( ! is_array( $data ) || ! $object instanceof stdClass ) {
			return null;
		}
		if ( property_exists( $object, 'received_by_confirmations' ) && ! is_array( $object->received_by_confirmations ) ) {
			return null;
		}
		return $data;
	}

	/**
	 * Zatoshis to credit (settled) from a decoded webhook body, or null when it carries no valid total.
	 *
	 * @param array<string,mixed> $data
	 */
	public static function from_webhook( array $data ): ?int {
		return self::settled( $data );
	}

	/**
	 * Settled zatoshis according to GET /payments/{id}, or null when unknown.
	 *
	 * @param array<string,mixed> $status
	 */
	public static function from_status( array $status ): ?int {
		return self::settled( $status );
	}

	/**
	 * Zatoshis seen at the address, mempool included, or null when there is no valid total.
	 *
	 * @param array<string,mixed> $data
	 */
	public static function seen( array $data ): ?int {
		$total = $data['total_received'] ?? null;
		return is_string( $total ) ? ZcashPaymentUtil::zec_to_zat( $total ) : null;
	}

	/**
	 * Zatoshis with the required confirmations, or null when the total or the array is malformed.
	 *
	 * @param array<string,mixed> $data
	 */
	public static function settled( array $data ): ?int {
		$seen = self::seen( $data );
		if ( null === $seen ) {
			return null;
		}
		if ( ! array_key_exists( 'received_by_confirmations', $data ) ) {
			return $seen;
		}
		$depths = $data['received_by_confirmations'];
		if ( ! is_array( $depths ) || array() === $depths || count( $depths ) > self::MAX_DEPTHS || array_keys( $depths ) !== range( 0, count( $depths ) - 1 ) ) {
			return null;
		}
		$settled = null;
		foreach ( $depths as $depth ) {
			$zat = is_string( $depth ) ? ZcashPaymentUtil::zec_to_zat( $depth ) : null;
			if ( null === $zat ) {
				return null;
			}
			$settled = $zat;
		}
		return $settled;
	}
}
