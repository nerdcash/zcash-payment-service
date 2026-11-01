<?php
/**
 * The module's per-order record (address, amounts, and review flags), kept in its own table.
 */

if ( ! defined( '_PS_VERSION_' ) ) {
	exit;
}

final class ZcashPaymentOrder {
	const TABLE = 'zcash_payment_order';

	/** Columns and their defaults. */
	const DEFAULTS = array(
		'address'        => '',
		'payment_id'     => '',
		'qr_text'        => '',
		'pay_url'        => '',
		'amount_zat'     => 0,
		'received_zat'   => 0,
		'state_version'  => 0,
		'priced'         => '',
		'needs_review'   => 0,
		'auto_hold'      => 0,
		'pre_hold_state' => 0,
	);

	public static function install(): bool {
		return Db::getInstance()->execute(
			'CREATE TABLE IF NOT EXISTS `' . _DB_PREFIX_ . self::TABLE . '` (
				`id_order` INT UNSIGNED NOT NULL PRIMARY KEY,
				`address` VARCHAR(255) NOT NULL DEFAULT \'\',
				`payment_id` VARCHAR(128) NOT NULL DEFAULT \'\',
				`qr_text` TEXT NULL,
				`pay_url` TEXT NULL,
				`amount_zat` BIGINT NOT NULL DEFAULT 0,
				`received_zat` BIGINT NOT NULL DEFAULT 0,
				`state_version` BIGINT NOT NULL DEFAULT 0,
				`priced` VARCHAR(64) NOT NULL DEFAULT \'\',
				`needs_review` TINYINT(1) NOT NULL DEFAULT 0,
				`auto_hold` TINYINT(1) NOT NULL DEFAULT 0,
				`pre_hold_state` INT UNSIGNED NOT NULL DEFAULT 0,
				KEY `address` (`address`(191))
			) ENGINE=' . _MYSQL_ENGINE_ . ' DEFAULT CHARSET=utf8mb4'
		);
	}

	/**
	 * The order's record, read fresh from the database (never from a request cache), or the
	 * defaults when the order has none.
	 *
	 * @return array<string,mixed>
	 */
	public static function get( int $id_order ): array {
		$row = Db::getInstance()->getRow( 'SELECT * FROM `' . _DB_PREFIX_ . self::TABLE . '` WHERE `id_order` = ' . (int) $id_order, false );
		$out = self::DEFAULTS;
		if ( is_array( $row ) ) {
			foreach ( self::DEFAULTS as $key => $default ) {
				$out[ $key ] = is_int( $default ) ? (int) $row[ $key ] : (string) ( $row[ $key ] ?? '' );
			}
		}
		return $out;
	}

	/** @param array<string,mixed> $fields */
	public static function set( int $id_order, array $fields ): bool {
		$data = array( 'id_order' => (int) $id_order );
		foreach ( $fields as $key => $value ) {
			if ( ! array_key_exists( $key, self::DEFAULTS ) ) {
				continue;
			}
			$data[ $key ] = is_int( self::DEFAULTS[ $key ] ) ? (int) $value : pSQL( (string) $value );
		}
		return Db::getInstance()->insert( self::TABLE, $data, false, false, Db::ON_DUPLICATE_KEY );
	}

	/**
	 * The order an address was issued for: its id, null if none, or false if the lookup failed.
	 */
	public static function find_by_address( string $address ): int|false|null {
		$db = Db::getInstance();
		try {
			$row = $db->getRow( 'SELECT `id_order` FROM `' . _DB_PREFIX_ . self::TABLE . "` WHERE `address` = '" . pSQL( $address ) . "' ORDER BY `id_order` DESC", false );
		} catch ( Throwable $e ) {
			return false;
		}
		if ( $db->getNumberError() ) {
			return false;
		}
		return is_array( $row ) ? (int) $row['id_order'] : null;
	}
}
