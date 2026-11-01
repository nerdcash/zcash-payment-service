<?php
/**
 * Pure helpers with no PrestaShop dependencies (apart from the lock), so they can be unit
 * tested with plain PHP.
 */

if ( ! defined( '_PS_VERSION_' ) ) {
	exit;
}

final class ZcashPaymentUtil {
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

	/** Writes digits times 10^exp as a plain decimal, without rounding. */
	private static function expand_exponent( string $whole, string $fraction, int $exp ): ?string {
		$digits = ltrim( $whole . $fraction, '0' );
		if ( '' === $digits ) {
			return '0';
		}
		$point = strlen( $whole ) + $exp; // Position of the decimal point within $whole . $fraction.
		$all   = $whole . $fraction;
		if ( $point <= 0 ) {
			return '0.' . str_repeat( '0', -$point ) . $all;
		}
		if ( $point > 30 ) {
			return null;
		}
		$all = str_pad( $all, $point, '0' );
		$int = substr( $all, 0, $point );
		$frac = substr( $all, $point );
		return ( '' === $int ? '0' : $int ) . ( '' === $frac ? '' : '.' . $frac );
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
			// Totals can carry zero padding or come from floats; drop zero padding
			// past the 8th place, but refuse a real sub-zatoshi amount.
			$total = trim( $total );
			if ( preg_match( '/^\d*\.\d{8}0+$/', $total ) ) {
				$total = rtrim( $total, '0' );
			} elseif ( preg_match( '/^(\d*)(?:\.(\d*))?e([+-]?\d{1,3})$/i', $total, $m ) ) {
				$total = self::expand_exponent( $m[1], $m[2] ?? '', (int) $m[3] );
				if ( null === $total ) {
					return null;
				}
				if ( preg_match( '/^\d*\.\d{8}0+$/', $total ) ) {
					$total = rtrim( $total, '0' );
				}
			}
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
	 * @return array{event_id:string,address:string,total_received:string,total_zat:int,state_version:int}|null
	 */
	public static function parse_observation( string $raw_body ): ?array {
		$data = json_decode( $raw_body, true );
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
		$total_zat = ZcashPaymentBalance::from_webhook( $data );
		if ( null === $total_zat ) {
			return null;
		}
		return array(
			'event_id'       => $data['event_id'],
			'address'        => $data['address'],
			'total_received' => $data['total_received'],
			'total_zat'      => $total_zat,
			'state_version'  => (int) $m[1],
		);
	}

	/** Takes a MySQL named lock, waiting up to 10 seconds. */
	public static function lock( string $name ): bool {
		return '1' === (string) Db::getInstance()->getValue( "SELECT GET_LOCK('" . pSQL( $name ) . "', 10)", false );
	}

	public static function unlock( string $name ): void {
		Db::getInstance()->getValue( "SELECT RELEASE_LOCK('" . pSQL( $name ) . "')", false );
	}
}
