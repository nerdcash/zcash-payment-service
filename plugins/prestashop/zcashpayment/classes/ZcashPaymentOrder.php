<?php
/**
 * The module's per-order record (address, amounts, and review flags), kept in its own table.
 */

if ( ! defined( '_PS_VERSION_' ) ) {
	exit;
}

/** A read or write of the module's order table failed. Callers stop rather than guess. */
final class ZcashPaymentDbException extends RuntimeException {
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
		'seen_zat'       => 0,
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
				`seen_zat` BIGINT NOT NULL DEFAULT 0,
				`state_version` BIGINT NOT NULL DEFAULT 0,
				`priced` VARCHAR(64) NOT NULL DEFAULT \'\',
				`needs_review` TINYINT(1) NOT NULL DEFAULT 0,
				`auto_hold` TINYINT(1) NOT NULL DEFAULT 0,
				`pre_hold_state` INT UNSIGNED NOT NULL DEFAULT 0,
				KEY `address` (`address`(191))
			) ENGINE=' . _MYSQL_ENGINE_ . ' DEFAULT CHARSET=utf8mb4'
		);
	}

	/** Adds the columns later versions introduced to a table created by an earlier one. */
	public static function upgrade(): bool {
		$db     = Db::getInstance();
		$table  = _DB_PREFIX_ . self::TABLE;
		$exists = $db->executeS( 'SHOW COLUMNS FROM `' . $table . "` LIKE 'seen_zat'" );
		if ( ! is_array( $exists ) ) {
			return false;
		}
		return array() !== $exists || $db->execute( 'ALTER TABLE `' . $table . '` ADD `seen_zat` BIGINT NOT NULL DEFAULT 0 AFTER `received_zat`' );
	}

	/**
	 * The order's record, read fresh from the database (never from a request cache), or the
	 * defaults when the order has none.
	 *
	 * @return array<string,mixed>
	 * @throws ZcashPaymentDbException if the lookup failed, so a failure never reads as "no record".
	 */
	public static function get( int $id_order ): array {
		$db = Db::getInstance();
		try {
			$row = $db->getRow( 'SELECT * FROM `' . _DB_PREFIX_ . self::TABLE . '` WHERE `id_order` = ' . (int) $id_order, false );
		} catch ( Throwable $e ) {
			throw new ZcashPaymentDbException( 'order ' . (int) $id_order . ': ' . $e->getMessage(), 0, $e );
		}
		if ( $db->getNumberError() ) {
			throw new ZcashPaymentDbException( 'order ' . (int) $id_order . ': ' . $db->getMsgError() );
		}
		$out = self::DEFAULTS;
		if ( is_array( $row ) ) {
			foreach ( self::DEFAULTS as $key => $default ) {
				$out[ $key ] = is_int( $default ) ? (int) $row[ $key ] : (string) ( $row[ $key ] ?? '' );
			}
		}
		return $out;
	}

	/**
	 * @param array<string,mixed> $fields
	 * @throws ZcashPaymentDbException if the write failed, so nothing goes on as if it was saved.
	 */
	public static function set( int $id_order, array $fields ): void {
		$data = array( 'id_order' => (int) $id_order );
		foreach ( $fields as $key => $value ) {
			if ( ! array_key_exists( $key, self::DEFAULTS ) ) {
				continue;
			}
			$data[ $key ] = is_int( self::DEFAULTS[ $key ] ) ? (int) $value : pSQL( (string) $value );
		}
		try {
			$ok = Db::getInstance()->insert( self::TABLE, $data, false, false, Db::ON_DUPLICATE_KEY );
		} catch ( Throwable $e ) {
			throw new ZcashPaymentDbException( 'order ' . (int) $id_order . ': ' . $e->getMessage(), 0, $e );
		}
		if ( ! $ok ) {
			throw new ZcashPaymentDbException( 'order ' . (int) $id_order . ': ' . Db::getInstance()->getMsgError() );
		}
	}

	/**
	 * The order an address was issued for: its id, null if none, or false if the lookup failed.
	 *
	 * @return int|false|null
	 */
	public static function find_by_address( string $address ) {
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
