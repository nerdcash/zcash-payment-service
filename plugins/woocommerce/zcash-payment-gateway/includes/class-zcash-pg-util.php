<?php
/**
 * Pure helpers with no WordPress dependencies, so they can be unit tested with plain PHP.
 */

defined( 'ABSPATH' ) || exit;

final class Zcash_PG_Util {
	const ZAT_PER_ZEC = 100000000;

	/**
	 * Parses a ZEC decimal such as "0.12345678" into zatoshis.
	 * Returns null for anything else, including more than 8 decimal places.
	 */
	public static function zec_to_zat( string $value ): ?int {
		$value = trim( $value );
		if ( ! preg_match( '/^(\d*)(?:\.(\d{0,8}))?$/', $value, $m ) || '' === $value || '.' === $value ) {
			return null;
		}
		$whole    = '' === $m[1] ? '0' : $m[1];
		$fraction = str_pad( $m[2] ?? '', 8, '0' );
		if ( strlen( ltrim( $whole, '0' ) ) > 10 ) {
			return null;
		}
		return (int) $whole * self::ZAT_PER_ZEC + (int) $fraction;
	}

	/** Formats zatoshis as a ZEC decimal with 8 places. */
	public static function zat_to_zec( int $zat ): string {
		$sign = $zat < 0 ? '-' : '';
		$abs  = abs( $zat );
		return sprintf( '%s%d.%08d', $sign, intdiv( $abs, self::ZAT_PER_ZEC ), $abs % self::ZAT_PER_ZEC );
	}

	/**
	 * Converts an order total in the store currency to zatoshis.
	 * For a ZEC store the rate is ignored. Otherwise $zec_per_unit is how many ZEC one unit of the
	 * store currency buys. Rounds up to the next zatoshi so the merchant is never short.
	 */
	public static function order_total_to_zat( string $total, string $currency, string $zec_per_unit ): ?int {
		if ( 'ZEC' === strtoupper( $currency ) ) {
			return self::zec_to_zat( $total );
		}
		if ( ! is_numeric( $total ) || ! is_numeric( $zec_per_unit ) || (float) $zec_per_unit <= 0 ) {
			return null;
		}
		$zat = (int) ceil( round( (float) $total * (float) $zec_per_unit * self::ZAT_PER_ZEC, 4 ) );
		return $zat > 0 ? $zat : null;
	}

	/** Joins the service's pay_url with an optional public base URL. Null when no public URL results. */
	public static function resolve_pay_url( string $pay_url, string $base ): ?string {
		if ( '' === $pay_url ) {
			return null;
		}
		if ( preg_match( '#^https?://#i', $pay_url ) ) {
			return $pay_url;
		}
		$base = rtrim( trim( $base ), '/' );
		if ( '' === $base || ! preg_match( '#^https?://#i', $base ) || '/' !== $pay_url[0] ) {
			return null;
		}
		return $base . $pay_url;
	}

	/** Checks the service's X-Signature header (hex HMAC-SHA256 of the raw body) in constant time. */
	public static function verify_signature( string $raw_body, ?string $signature, string $secret ): bool {
		if ( '' === $secret || null === $signature ) {
			return false;
		}
		$signature = strtolower( trim( $signature ) );
		if ( ! preg_match( '/^[0-9a-f]{64}$/', $signature ) ) {
			return false;
		}
		return hash_equals( hash_hmac( 'sha256', $raw_body, $secret ), $signature );
	}

	/**
	 * Validates a webhook body and returns its fields, or null.
	 *
	 * `total_zat` is everything received, mempool included. `settled_zat` is what has the
	 * service's `REQUIRED_CONFIRMATIONS`: the last `received_by_confirmations` element, or
	 * `total_zat` when the service doesn't send that array.
	 *
	 * @return array{event_id:string,address:string,total_received:string,total_zat:int,settled_zat:int,state_version:int}|null
	 */
	public static function parse_observation( string $raw_body ): ?array {
		$data = self::decode_json( $raw_body );
		if ( ! is_array( $data ) ) {
			return null;
		}
		foreach ( array( 'event_id', 'payment_source', 'address', 'total_received', 'observed_at' ) as $key ) {
			if ( ! isset( $data[ $key ] ) || ! is_string( $data[ $key ] ) || '' === trim( $data[ $key ] ) ) {
				return null;
			}
		}
		if ( 'zcash' !== $data['payment_source'] ) {
			return null;
		}
		if ( ! preg_match( '/:v(\d+)$/', $data['event_id'], $m ) ) {
			return null;
		}
		$total_zat = self::zec_to_zat( $data['total_received'] );
		if ( null === $total_zat ) {
			return null;
		}
		$settled_zat = self::settled_zat( $data, $total_zat );
		if ( null === $settled_zat ) {
			return null;
		}
		return array(
			'event_id'       => $data['event_id'],
			'address'        => $data['address'],
			'total_received' => $data['total_received'],
			'total_zat'      => $total_zat,
			'settled_zat'    => $settled_zat,
			'state_version'  => (int) $m[1],
		);
	}

	/**
	 * Decodes a JSON object from the service into an array, or null. Associative decoding turns
	 * a numeric-keyed object into a PHP list, so `received_by_confirmations` is checked against
	 * an object decode to make sure it really is a JSON array.
	 */
	public static function decode_json( string $raw ): ?array {
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
	 * The amount with the service's required confirmations: the last element of
	 * `received_by_confirmations` when present, else `$total_zat`. Null when the array is
	 * malformed. Works on webhook bodies and on the status route's JSON.
	 */
	public static function settled_zat( array $data, int $total_zat ): ?int {
		if ( ! array_key_exists( 'received_by_confirmations', $data ) ) {
			return $total_zat;
		}
		$depths = $data['received_by_confirmations'];
		if ( ! is_array( $depths ) || array() === $depths || count( $depths ) > 101 || ! array_is_list( $depths ) ) {
			return null;
		}
		$settled = null;
		foreach ( $depths as $depth ) {
			$zat = is_string( $depth ) ? self::zec_to_zat( $depth ) : null;
			if ( null === $zat ) {
				return null;
			}
			$settled = $zat;
		}
		return $settled;
	}

	/** Takes a MySQL named lock, waiting up to 10 seconds. */
	public static function lock( string $name ): bool {
		global $wpdb;
		return '1' === (string) $wpdb->get_var( $wpdb->prepare( 'SELECT GET_LOCK(%s, 10)', $name ) );
	}

	public static function unlock( string $name ): void {
		global $wpdb;
		$wpdb->query( $wpdb->prepare( 'SELECT RELEASE_LOCK(%s)', $name ) );
	}
}
