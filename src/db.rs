use std::{
    fs,
    path::{Path, PathBuf},
    time::Duration,
};

use rusqlite::{
    params, Connection, Error as SqliteError, ErrorCode, OptionalExtension, TransactionBehavior,
};
use serde::{Deserialize, Serialize};
use serde_json::json;
use time::{format_description::well_known::Rfc3339, OffsetDateTime};

use crate::{config::Config, error::AppError, wallet_db::read_wallet_db_identity};

#[derive(Clone, Debug)]
pub struct AppDb {
    path: PathBuf,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletIdentity {
    pub canonical_uivk: String,
    pub uivk_fingerprint: String,
    pub network: String,
    pub birthday_height: u32,
    pub wallet_db_path: String,
    pub initialized_at: String,
    pub last_validated_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ServiceMetadata {
    pub app_schema_version: i64,
    pub wallet_schema_version_seen: i64,
    pub last_seen_tip_height: i64,
    pub last_scanned_height: i64,
    pub last_stable_tip_height: i64,
    pub current_sync_state: String,
    pub current_scan_epoch: i64,
    pub catch_up_entered_at: Option<String>,
    pub catch_up_exited_at: Option<String>,
    pub webhook_report_confirmations: i64,
    pub finality_confirmations: i64,
}

#[derive(Clone, Debug, Serialize, Deserialize)]
pub struct IssuedAddressRecord {
    pub address_id: i64,
    pub unified_address: String,
    pub address_source: String,
    pub created_at: String,
    pub request_label: Option<String>,
    pub request_memo: Option<String>,
    pub request_message: Option<String>,
    pub requested_amount: Option<String>,
}

#[derive(Clone, Debug)]
pub struct NewIssuedAddress<'a> {
    pub unified_address: &'a str,
    pub address_source: &'a str,
    pub diversifier_index_be: Option<&'a [u8]>,
    pub diversifier_bytes: Option<&'a [u8]>,
    pub request_label: Option<&'a str>,
    pub request_memo: Option<&'a str>,
    pub request_message: Option<&'a str>,
    pub requested_amount: Option<&'a str>,
}

#[derive(Clone, Debug)]
pub struct NewAddressReceiver<'a> {
    pub pool: &'a str,
    pub receiver_encoding: &'a str,
    pub receiver_fingerprint: &'a [u8],
}

#[derive(Clone, Debug)]
pub struct OwnedIssuedAddress {
    pub unified_address: String,
    pub address_source: String,
    pub diversifier_index_be: Option<Vec<u8>>,
    pub diversifier_bytes: Option<Vec<u8>>,
    pub request_label: Option<String>,
    pub request_memo: Option<String>,
    pub request_message: Option<String>,
    pub requested_amount: Option<String>,
}

#[derive(Clone, Debug)]
pub struct OwnedAddressReceiver {
    pub pool: String,
    pub receiver_encoding: String,
    pub receiver_fingerprint: Vec<u8>,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub enum ReceiptKind {
    Mined,
    Mempool,
}

impl ReceiptKind {
    fn as_str(&self) -> &'static str {
        match self {
            Self::Mined => "mined",
            Self::Mempool => "mempool",
        }
    }
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct AttributedScannedReceipt {
    pub address_id: i64,
    pub txid_hex: String,
    pub pool: String,
    pub receipt_uid: String,
    pub value_zat: i64,
    pub mined_height: i64,
    pub confirmation_depth: i64,
    pub eligible_for_webhook: bool,
    pub receipt_kind: ReceiptKind,
    pub first_observed_at: String,
    pub last_observed_at: String,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct QueuedWebhookDelivery {
    pub delivery_id: i64,
    pub address_id: i64,
    pub event_id: String,
    pub total_received_zat: i64,
    pub request_body_json: String,
    pub attempt_count: i64,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct ReceiptRangeReconciliation {
    pub affected_address_ids: Vec<i64>,
    pub inserted_receipt_count: usize,
    pub revoked_receipt_count: usize,
    pub reactivated_receipt_count: usize,
}

#[derive(Clone, Debug)]
pub struct AtomicFreshAddressAllocation {
    pub issued_address: OwnedIssuedAddress,
    pub receivers: Vec<OwnedAddressReceiver>,
}

impl AppDb {
    pub fn open(path: impl AsRef<Path>) -> Result<Self, AppError> {
        let path = path.as_ref().to_path_buf();
        if let Some(parent) = path.parent() {
            fs::create_dir_all(parent)?;
        }

        let db = Self { path };
        db.initialize_schema()?;
        Ok(db)
    }

    pub fn ensure_wallet_identity(&self, config: &Config) -> Result<WalletIdentity, AppError> {
        let now = now_rfc3339()?;
        let existing = self.wallet_identity()?;
        let wallet_db_identity = read_wallet_db_identity(&config.wallet_db_path)?;

        match existing {
            Some(existing_identity) => {
                if let Some(wallet_db_identity) = wallet_db_identity.as_ref() {
                    if wallet_db_identity.uivk != existing_identity.canonical_uivk {
                        return Err(AppError::StartupIntegrity(
                            "wallet DB UIVK does not match stored wallet identity".into(),
                        ));
                    }

                    if wallet_db_identity.birthday_height != existing_identity.birthday_height {
                        return Err(AppError::StartupIntegrity(
                            "wallet DB birthday height does not match stored wallet identity"
                                .into(),
                        ));
                    }
                }

                if let Some(startup_uivk) = config.startup_uivk.as_deref() {
                    if startup_uivk != existing_identity.canonical_uivk {
                        return Err(AppError::StartupIntegrity(
                            "provided ZCASH_UIVK does not match stored wallet identity".into(),
                        ));
                    }
                }

                if existing_identity.network != config.network {
                    return Err(AppError::StartupIntegrity(
                        "configured ZCASH_NETWORK does not match stored wallet identity".into(),
                    ));
                }

                if let Some(birthday_height) = config.birthday_height {
                    if birthday_height != existing_identity.birthday_height {
                        return Err(AppError::StartupIntegrity(
                            "provided ZCASH_BIRTHDAY_HEIGHT does not match stored wallet identity"
                                .into(),
                        ));
                    }
                }

                self.touch_wallet_identity_validation(&now)?;
                Ok(self
                    .wallet_identity()?
                    .expect("wallet identity must still exist"))
            }
            None => {
                if let (Some(startup_uivk), Some(wallet_db_identity)) =
                    (config.startup_uivk.as_deref(), wallet_db_identity.as_ref())
                {
                    if startup_uivk != wallet_db_identity.uivk {
                        return Err(AppError::StartupIntegrity(
                            "provided ZCASH_UIVK does not match the existing wallet DB identity"
                                .into(),
                        ));
                    }
                }

                let startup_uivk = config
                    .startup_uivk
                    .as_deref()
                    .or(wallet_db_identity.as_ref().map(|identity| identity.uivk.as_str()))
                    .ok_or_else(|| {
                        AppError::StartupIntegrity(
                            "first startup requires ZCASH_UIVK unless the wallet DB already contains a canonical account"
                                .into(),
                        )
                    })?;
                let birthday_height = config
                    .birthday_height
                    .or(wallet_db_identity.as_ref().map(|identity| identity.birthday_height))
                    .ok_or_else(|| {
                        AppError::StartupIntegrity(
                            "first startup requires ZCASH_BIRTHDAY_HEIGHT unless the wallet DB already contains a canonical account"
                                .into(),
                        )
                    })?;

                let identity = WalletIdentity {
                    canonical_uivk: startup_uivk.to_string(),
                    uivk_fingerprint: fingerprint(startup_uivk),
                    network: config.network.clone(),
                    birthday_height,
                    wallet_db_path: config.wallet_db_path.display().to_string(),
                    initialized_at: now.clone(),
                    last_validated_at: now,
                };

                match self.insert_wallet_identity(&identity) {
                    Ok(()) => Ok(identity),
                    Err(AppError::Database(error)) if is_singleton_insert_conflict(&error) => {
                        let existing = self.wallet_identity()?.ok_or(AppError::Database(error))?;
                        if existing.canonical_uivk != identity.canonical_uivk
                            || existing.network != identity.network
                            || existing.birthday_height != identity.birthday_height
                            || existing.wallet_db_path != identity.wallet_db_path
                        {
                            return Err(AppError::StartupIntegrity(
                                "concurrent wallet identity initialization produced conflicting singleton data"
                                    .into(),
                            ));
                        }
                        Ok(existing)
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }

    pub fn ensure_service_metadata(&self, config: &Config) -> Result<ServiceMetadata, AppError> {
        let existing = self.service_metadata()?;
        match existing {
            Some(metadata) => {
                if metadata.webhook_report_confirmations
                    != i64::from(config.webhook_report_confirmations)
                    || metadata.finality_confirmations != i64::from(config.finality_confirmations)
                {
                    return Err(AppError::StartupIntegrity(
                        "stored service metadata confirmation settings do not match the configured constants"
                            .into(),
                    ));
                }
                Ok(metadata)
            }
            None => {
                let metadata = ServiceMetadata {
                    app_schema_version: 1,
                    wallet_schema_version_seen: 0,
                    last_seen_tip_height: 0,
                    last_scanned_height: 0,
                    last_stable_tip_height: 0,
                    current_sync_state: "starting".into(),
                    current_scan_epoch: 0,
                    catch_up_entered_at: None,
                    catch_up_exited_at: None,
                    webhook_report_confirmations: i64::from(config.webhook_report_confirmations),
                    finality_confirmations: i64::from(config.finality_confirmations),
                };
                match self.insert_service_metadata(&metadata) {
                    Ok(()) => Ok(metadata),
                    Err(AppError::Database(error)) if is_singleton_insert_conflict(&error) => {
                        let existing = self.service_metadata()?.ok_or(AppError::Database(error))?;
                        if existing.webhook_report_confirmations
                            != i64::from(config.webhook_report_confirmations)
                            || existing.finality_confirmations
                                != i64::from(config.finality_confirmations)
                        {
                            return Err(AppError::StartupIntegrity(
                                "concurrent service metadata initialization produced conflicting confirmation settings"
                                    .into(),
                            ));
                        }
                        Ok(existing)
                    }
                    Err(error) => Err(error),
                }
            }
        }
    }

    pub fn current_service_metadata(&self) -> Result<ServiceMetadata, AppError> {
        self.service_metadata()?
            .ok_or_else(|| AppError::InvalidConfig("service metadata is missing".into()))
    }

    pub fn record_sync_status(
        &self,
        new_state: &str,
        reason: &str,
        last_seen_tip_height: i64,
        last_scanned_height: i64,
    ) -> Result<ServiceMetadata, AppError> {
        let now = now_rfc3339()?;
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let current = tx
            .query_row(
                "SELECT app_schema_version, wallet_schema_version_seen, last_seen_tip_height, last_scanned_height, last_stable_tip_height, current_sync_state, current_scan_epoch, catch_up_entered_at, catch_up_exited_at, webhook_report_confirmations, finality_confirmations FROM service_metadata WHERE singleton_id = 1",
                [],
                |row| {
                    Ok(ServiceMetadata {
                        app_schema_version: row.get(0)?,
                        wallet_schema_version_seen: row.get(1)?,
                        last_seen_tip_height: row.get(2)?,
                        last_scanned_height: row.get(3)?,
                        last_stable_tip_height: row.get(4)?,
                        current_sync_state: row.get(5)?,
                        current_scan_epoch: row.get(6)?,
                        catch_up_entered_at: row.get(7)?,
                        catch_up_exited_at: row.get(8)?,
                        webhook_report_confirmations: row.get(9)?,
                        finality_confirmations: row.get(10)?,
                    })
                },
            )
            .optional()?
            .ok_or_else(|| AppError::InvalidConfig("service metadata is missing".into()))?;

        let state_changed = current.current_sync_state != new_state;
        let next_scan_epoch = if state_changed {
            current.current_scan_epoch + 1
        } else {
            current.current_scan_epoch
        };
        let catch_up_entered_at = if state_changed && new_state == "catching_up" {
            Some(now.clone())
        } else {
            current.catch_up_entered_at.clone()
        };
        let catch_up_exited_at = if state_changed
            && current.current_sync_state == "catching_up"
            && new_state != "catching_up"
        {
            Some(now.clone())
        } else {
            current.catch_up_exited_at.clone()
        };

        tx.execute(
            "UPDATE service_metadata SET last_seen_tip_height = ?1, last_scanned_height = ?2, last_stable_tip_height = ?3, current_sync_state = ?4, current_scan_epoch = ?5, catch_up_entered_at = ?6, catch_up_exited_at = ?7 WHERE singleton_id = 1",
            params![
                last_seen_tip_height,
                last_scanned_height,
                last_scanned_height,
                new_state,
                next_scan_epoch,
                catch_up_entered_at,
                catch_up_exited_at,
            ],
        )?;

        if state_changed {
            tx.execute(
                "INSERT INTO sync_state_transitions (from_state, to_state, reason, tip_height, scanned_height, scan_epoch, detected_at, reorg_depth_estimate) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, NULL)",
                params![
                    current.current_sync_state,
                    new_state,
                    reason,
                    last_seen_tip_height,
                    last_scanned_height,
                    next_scan_epoch,
                    now,
                ],
            )?;
        }

        let updated = ServiceMetadata {
            app_schema_version: current.app_schema_version,
            wallet_schema_version_seen: current.wallet_schema_version_seen,
            last_seen_tip_height,
            last_scanned_height,
            last_stable_tip_height: last_scanned_height,
            current_sync_state: new_state.to_string(),
            current_scan_epoch: next_scan_epoch,
            catch_up_entered_at,
            catch_up_exited_at,
            webhook_report_confirmations: current.webhook_report_confirmations,
            finality_confirmations: current.finality_confirmations,
        };

        tx.commit()?;
        Ok(updated)
    }

    pub fn find_issued_address(
        &self,
        address: &str,
    ) -> Result<Option<IssuedAddressRecord>, AppError> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT address_id, unified_address, address_source, created_at, request_label, request_memo, request_message, requested_amount FROM issued_addresses WHERE unified_address = ?1",
            params![address],
            |row| {
                Ok(IssuedAddressRecord {
                    address_id: row.get(0)?,
                    unified_address: row.get(1)?,
                    address_source: row.get(2)?,
                    created_at: row.get(3)?,
                    request_label: row.get(4)?,
                    request_memo: row.get(5)?,
                    request_message: row.get(6)?,
                    requested_amount: row.get(7)?,
                })
            },
        )
        .optional()
        .map_err(AppError::from)
    }

    pub fn insert_issued_address(
        &self,
        record: NewIssuedAddress<'_>,
    ) -> Result<IssuedAddressRecord, AppError> {
        let created_at = now_rfc3339()?;
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO issued_addresses (unified_address, address_source, diversifier_index_be, diversifier_bytes, created_at, request_label, request_memo, request_message, requested_amount) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                record.unified_address,
                record.address_source,
                record.diversifier_index_be,
                record.diversifier_bytes,
                created_at,
                record.request_label,
                record.request_memo,
                record.request_message,
                record.requested_amount,
            ],
        )?;

        let address_id = conn.last_insert_rowid();
        conn.execute(
            "INSERT INTO address_totals (address_id, dirty_state, last_changed_at) VALUES (?1, 'clean', ?2)",
            params![address_id, created_at],
        )?;

        self.find_issued_address(record.unified_address)?
            .ok_or_else(|| {
                AppError::InvalidConfig("failed to reload inserted issued address".into())
            })
    }

    pub fn insert_address_receivers(
        &self,
        address_id: i64,
        receivers: &[NewAddressReceiver<'_>],
    ) -> Result<(), AppError> {
        let created_at = now_rfc3339()?;
        let mut conn = self.connect()?;
        let tx = conn.transaction()?;
        {
            let mut statement = tx.prepare(
                "INSERT INTO address_receivers (address_id, pool, receiver_encoding, receiver_fingerprint, is_active, created_at) VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            )?;

            for receiver in receivers {
                statement.execute(params![
                    address_id,
                    receiver.pool,
                    receiver.receiver_encoding,
                    receiver.receiver_fingerprint,
                    created_at,
                ])?;
            }
        }
        tx.commit()?;
        Ok(())
    }

    pub fn allocate_fresh_address<F>(&self, mut prepare: F) -> Result<IssuedAddressRecord, AppError>
    where
        F: FnMut(Option<&[u8]>) -> Result<AtomicFreshAddressAllocation, AppError>,
    {
        let created_at = now_rfc3339()?;
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let last_diversifier_index_be = tx
            .query_row(
                "SELECT diversifier_index_be FROM issued_addresses WHERE diversifier_index_be IS NOT NULL ORDER BY address_id DESC LIMIT 1",
                [],
                |row| row.get::<_, Vec<u8>>(0),
            )
            .optional()?;

        let allocation = prepare(last_diversifier_index_be.as_deref())?;

        tx.execute(
            "INSERT INTO issued_addresses (unified_address, address_source, diversifier_index_be, diversifier_bytes, created_at, request_label, request_memo, request_message, requested_amount) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9)",
            params![
                allocation.issued_address.unified_address,
                allocation.issued_address.address_source,
                allocation.issued_address.diversifier_index_be,
                allocation.issued_address.diversifier_bytes,
                created_at,
                allocation.issued_address.request_label,
                allocation.issued_address.request_memo,
                allocation.issued_address.request_message,
                allocation.issued_address.requested_amount,
            ],
        )?;

        let address_id = tx.last_insert_rowid();
        tx.execute(
            "INSERT INTO address_totals (address_id, dirty_state, last_changed_at) VALUES (?1, 'clean', ?2)",
            params![address_id, created_at],
        )?;

        {
            let mut statement = tx.prepare(
                "INSERT INTO address_receivers (address_id, pool, receiver_encoding, receiver_fingerprint, is_active, created_at) VALUES (?1, ?2, ?3, ?4, 1, ?5)",
            )?;
            for receiver in &allocation.receivers {
                statement.execute(params![
                    address_id,
                    receiver.pool,
                    receiver.receiver_encoding,
                    receiver.receiver_fingerprint,
                    created_at,
                ])?;
            }
        }

        let issued_address = allocation.issued_address;
        let record = IssuedAddressRecord {
            address_id,
            unified_address: issued_address.unified_address,
            address_source: issued_address.address_source,
            created_at,
            request_label: issued_address.request_label,
            request_memo: issued_address.request_memo,
            request_message: issued_address.request_message,
            requested_amount: issued_address.requested_amount,
        };

        tx.commit()?;
        Ok(record)
    }

    pub fn latest_diversifier_index_be(&self) -> Result<Option<Vec<u8>>, AppError> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT diversifier_index_be FROM issued_addresses WHERE diversifier_index_be IS NOT NULL ORDER BY address_id DESC LIMIT 1",
            [],
            |row| row.get(0),
        )
        .optional()
        .map_err(AppError::from)
    }

    pub fn find_address_id_by_receiver_fingerprint(
        &self,
        pool: &str,
        receiver_fingerprint: &[u8],
    ) -> Result<Option<i64>, AppError> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT address_id FROM address_receivers WHERE pool = ?1 AND receiver_fingerprint = ?2 AND is_active = 1",
            params![pool, receiver_fingerprint],
            |row| row.get(0),
        )
        .optional()
        .map_err(AppError::from)
    }

    pub fn apply_attributed_receipts(
        &self,
        receipts: &[AttributedScannedReceipt],
        tip_height: i64,
        finality_confirmations: u32,
    ) -> Result<Vec<i64>, AppError> {
        if receipts.is_empty() {
            return Ok(Vec::new());
        }

        let now = now_rfc3339()?;
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut affected_address_ids = std::collections::BTreeSet::new();
        let mut dirty_since_by_address = std::collections::BTreeMap::new();

        for receipt in receipts {
            let inserted = tx.execute(
                "INSERT INTO address_receipts_recent (address_id, txid, pool, receipt_uid, value_zat, mined_height, confirmation_depth, eligible_for_webhook, receipt_kind, first_observed_at, observed_at, receipt_state, state_version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'active', 0) ON CONFLICT(receipt_uid) DO UPDATE SET address_id = excluded.address_id, txid = excluded.txid, pool = excluded.pool, value_zat = excluded.value_zat, mined_height = excluded.mined_height, confirmation_depth = excluded.confirmation_depth, eligible_for_webhook = excluded.eligible_for_webhook, receipt_kind = excluded.receipt_kind, first_observed_at = CASE WHEN address_receipts_recent.receipt_kind = 'mempool' AND excluded.receipt_kind = 'mined' THEN address_receipts_recent.first_observed_at ELSE excluded.first_observed_at END, observed_at = excluded.observed_at, receipt_state = 'active', state_version = CASE WHEN address_receipts_recent.receipt_state != 'active' OR address_receipts_recent.address_id != excluded.address_id OR address_receipts_recent.mined_height != excluded.mined_height OR address_receipts_recent.value_zat != excluded.value_zat OR address_receipts_recent.confirmation_depth != excluded.confirmation_depth OR address_receipts_recent.eligible_for_webhook != excluded.eligible_for_webhook OR address_receipts_recent.receipt_kind != excluded.receipt_kind THEN address_receipts_recent.state_version + 1 ELSE address_receipts_recent.state_version END",
                params![
                    receipt.address_id,
                    receipt.txid_hex,
                    receipt.pool,
                    receipt.receipt_uid,
                    receipt.value_zat,
                    receipt.mined_height,
                    receipt.confirmation_depth,
                    if receipt.eligible_for_webhook { 1 } else { 0 },
                    receipt.receipt_kind.as_str(),
                    receipt.first_observed_at,
                    receipt.last_observed_at,
                ],
            )?;

            if inserted > 0 {
                affected_address_ids.insert(receipt.address_id);
                dirty_since_by_address
                    .entry(receipt.address_id)
                    .and_modify(|height: &mut i64| *height = (*height).min(receipt.mined_height))
                    .or_insert(receipt.mined_height);
            }
        }

        recompute_address_totals(
            &tx,
            &affected_address_ids,
            tip_height,
            finality_confirmations,
            &now,
            "receipt_observed",
            &dirty_since_by_address,
        )?;

        tx.commit()?;
        Ok(affected_address_ids.into_iter().collect())
    }

    pub fn advance_receipt_maturity(
        &self,
        tip_height: i64,
        webhook_report_confirmations: u32,
        finality_confirmations: u32,
    ) -> Result<Vec<i64>, AppError> {
        let now = now_rfc3339()?;
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let eligibility_threshold = i64::from(webhook_report_confirmations);

        let mut matured_since_by_address = std::collections::BTreeMap::new();
        let mut statement = tx.prepare(
            "SELECT DISTINCT address_id, mined_height FROM address_receipts_recent WHERE receipt_kind = 'mined' AND receipt_state = 'active' AND eligible_for_webhook = 0 AND CASE WHEN ?1 >= mined_height THEN ?1 - mined_height + 1 ELSE 0 END >= ?2",
        )?;
        let rows = statement.query_map(params![tip_height, eligibility_threshold], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })?;
        for row in rows {
            let (address_id, mined_height) = row?;
            matured_since_by_address
                .entry(address_id)
                .and_modify(|height: &mut i64| *height = (*height).min(mined_height))
                .or_insert(mined_height);
        }
        drop(statement);

        let mut affected_address_ids = std::collections::BTreeSet::new();
        let mut statement = tx.prepare(
            "SELECT DISTINCT address_id FROM address_receipts_recent WHERE receipt_kind = 'mined' AND receipt_state = 'active' AND (confirmation_depth != CASE WHEN ?1 >= mined_height THEN ?1 - mined_height + 1 ELSE 0 END OR eligible_for_webhook != CASE WHEN CASE WHEN ?1 >= mined_height THEN ?1 - mined_height + 1 ELSE 0 END >= ?2 THEN 1 ELSE 0 END)",
        )?;
        let rows = statement.query_map(params![tip_height, eligibility_threshold], |row| {
            row.get::<_, i64>(0)
        })?;
        for row in rows {
            affected_address_ids.insert(row?);
        }
        drop(statement);

        if affected_address_ids.is_empty() {
            tx.commit()?;
            return Ok(Vec::new());
        }

        tx.execute(
            "UPDATE address_receipts_recent SET confirmation_depth = CASE WHEN ?1 >= mined_height THEN ?1 - mined_height + 1 ELSE 0 END, eligible_for_webhook = CASE WHEN CASE WHEN ?1 >= mined_height THEN ?1 - mined_height + 1 ELSE 0 END >= ?2 THEN 1 ELSE 0 END WHERE receipt_kind = 'mined' AND receipt_state = 'active' AND (confirmation_depth != CASE WHEN ?1 >= mined_height THEN ?1 - mined_height + 1 ELSE 0 END OR eligible_for_webhook != CASE WHEN CASE WHEN ?1 >= mined_height THEN ?1 - mined_height + 1 ELSE 0 END >= ?2 THEN 1 ELSE 0 END)",
            params![tip_height, eligibility_threshold],
        )?;

        recompute_address_totals(
            &tx,
            &affected_address_ids,
            tip_height,
            finality_confirmations,
            &now,
            "receipt_matured",
            &matured_since_by_address,
        )?;

        tx.commit()?;
        Ok(affected_address_ids.into_iter().collect())
    }

    pub fn reconcile_attributed_receipts_in_range(
        &self,
        receipts: &[AttributedScannedReceipt],
        start_height: i64,
        end_height: i64,
        tip_height: i64,
        finality_confirmations: u32,
    ) -> Result<ReceiptRangeReconciliation, AppError> {
        let now = now_rfc3339()?;
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let mut scanned_by_uid = std::collections::BTreeMap::new();
        for receipt in receipts {
            scanned_by_uid.insert(receipt.receipt_uid.clone(), receipt.clone());
        }

        let mut existing_by_uid = std::collections::BTreeMap::new();
        let mut statement = tx.prepare(
            "SELECT receipt_uid, address_id, mined_height, receipt_state, value_zat, confirmation_depth, eligible_for_webhook, receipt_kind, first_observed_at FROM address_receipts_recent WHERE mined_height BETWEEN ?1 AND ?2 OR (receipt_kind = 'mempool' AND txid IN (SELECT txid FROM address_receipts_recent WHERE mined_height BETWEEN ?1 AND ?2))",
        )?;
        let rows = statement.query_map(params![start_height, end_height], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, String>(3)?,
                row.get::<_, i64>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, String>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;
        for row in rows {
            let (
                receipt_uid,
                address_id,
                mined_height,
                receipt_state,
                value_zat,
                confirmation_depth,
                eligible_for_webhook,
                receipt_kind,
                first_observed_at,
            ) = row?;
            existing_by_uid.insert(
                receipt_uid,
                (
                    address_id,
                    mined_height,
                    receipt_state,
                    value_zat,
                    confirmation_depth,
                    eligible_for_webhook,
                    receipt_kind,
                    first_observed_at,
                ),
            );
        }
        drop(statement);

        let mut affected_address_ids = std::collections::BTreeSet::new();
        let mut dirty_since_by_address = std::collections::BTreeMap::new();
        let mut revoked_receipt_count = 0usize;
        let mut reactivated_receipt_count = 0usize;
        let mut inserted_receipt_count = 0usize;

        for (receipt_uid, (address_id, mined_height, receipt_state, _, _, _, receipt_kind, _)) in
            &existing_by_uid
        {
            if receipt_kind == "mined"
                && receipt_state == "active"
                && !scanned_by_uid.contains_key(receipt_uid)
            {
                tx.execute(
                    "UPDATE address_receipts_recent SET receipt_state = 'revoked', state_version = state_version + 1, observed_at = ?2 WHERE receipt_uid = ?1",
                    params![receipt_uid, now],
                )?;
                affected_address_ids.insert(*address_id);
                dirty_since_by_address
                    .entry(*address_id)
                    .and_modify(|height: &mut i64| *height = (*height).min(*mined_height))
                    .or_insert(*mined_height);
                revoked_receipt_count += 1;
            }
        }

        for receipt in scanned_by_uid.values() {
            let prior = existing_by_uid.get(&receipt.receipt_uid);
            let was_insert = prior.is_none();
            let was_revoked = prior
                .map(|(_, _, state, _, _, _, _, _)| state != "active")
                .unwrap_or(false);
            let was_changed = prior
                .map(
                    |(
                        address_id,
                        mined_height,
                        _,
                        value_zat,
                        confirmation_depth,
                        eligible,
                        receipt_kind,
                        _,
                    )| {
                        *address_id != receipt.address_id
                            || *mined_height != receipt.mined_height
                            || *value_zat != receipt.value_zat
                            || *confirmation_depth != receipt.confirmation_depth
                            || *eligible != if receipt.eligible_for_webhook { 1 } else { 0 }
                            || *receipt_kind != receipt.receipt_kind.as_str()
                    },
                )
                .unwrap_or(false);

            tx.execute(
                "INSERT INTO address_receipts_recent (address_id, txid, pool, receipt_uid, value_zat, mined_height, confirmation_depth, eligible_for_webhook, receipt_kind, first_observed_at, observed_at, receipt_state, state_version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, 'active', 0) ON CONFLICT(receipt_uid) DO UPDATE SET address_id = excluded.address_id, txid = excluded.txid, pool = excluded.pool, value_zat = excluded.value_zat, mined_height = excluded.mined_height, confirmation_depth = excluded.confirmation_depth, eligible_for_webhook = excluded.eligible_for_webhook, receipt_kind = excluded.receipt_kind, first_observed_at = CASE WHEN address_receipts_recent.receipt_kind = 'mempool' AND excluded.receipt_kind = 'mined' THEN address_receipts_recent.first_observed_at ELSE excluded.first_observed_at END, observed_at = excluded.observed_at, receipt_state = 'active', state_version = CASE WHEN address_receipts_recent.receipt_state != 'active' OR address_receipts_recent.address_id != excluded.address_id OR address_receipts_recent.mined_height != excluded.mined_height OR address_receipts_recent.value_zat != excluded.value_zat OR address_receipts_recent.confirmation_depth != excluded.confirmation_depth OR address_receipts_recent.eligible_for_webhook != excluded.eligible_for_webhook OR address_receipts_recent.receipt_kind != excluded.receipt_kind THEN address_receipts_recent.state_version + 1 ELSE address_receipts_recent.state_version END",
                params![
                    receipt.address_id,
                    receipt.txid_hex,
                    receipt.pool,
                    receipt.receipt_uid,
                    receipt.value_zat,
                    receipt.mined_height,
                    receipt.confirmation_depth,
                    if receipt.eligible_for_webhook { 1 } else { 0 },
                    receipt.receipt_kind.as_str(),
                    receipt.first_observed_at,
                    receipt.last_observed_at,
                ],
            )?;

            if was_insert {
                inserted_receipt_count += 1;
                affected_address_ids.insert(receipt.address_id);
                dirty_since_by_address
                    .entry(receipt.address_id)
                    .and_modify(|height: &mut i64| *height = (*height).min(receipt.mined_height))
                    .or_insert(receipt.mined_height);
            } else if was_revoked || was_changed {
                if was_revoked {
                    reactivated_receipt_count += 1;
                }
                affected_address_ids.insert(receipt.address_id);
                dirty_since_by_address
                    .entry(receipt.address_id)
                    .and_modify(|height: &mut i64| *height = (*height).min(receipt.mined_height))
                    .or_insert(receipt.mined_height);
                if let Some((old_address_id, old_height, _, _, _, _, _, _)) = prior {
                    affected_address_ids.insert(*old_address_id);
                    dirty_since_by_address
                        .entry(*old_address_id)
                        .and_modify(|height: &mut i64| *height = (*height).min(*old_height))
                        .or_insert(*old_height);
                }
            }
        }

        recompute_address_totals(
            &tx,
            &affected_address_ids,
            tip_height,
            finality_confirmations,
            &now,
            "reorg_reconcile",
            &dirty_since_by_address,
        )?;

        tx.commit()?;
        Ok(ReceiptRangeReconciliation {
            affected_address_ids: affected_address_ids.into_iter().collect(),
            inserted_receipt_count,
            revoked_receipt_count,
            reactivated_receipt_count,
        })
    }

    pub fn revoke_mined_receipts_above_tip(
        &self,
        tip_height: i64,
        finality_confirmations: u32,
    ) -> Result<ReceiptRangeReconciliation, AppError> {
        let now = now_rfc3339()?;
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;

        let mut statement = tx.prepare(
            "SELECT receipt_uid, address_id, mined_height FROM address_receipts_recent WHERE receipt_kind = 'mined' AND receipt_state = 'active' AND mined_height > ?1 ORDER BY mined_height ASC, receipt_uid ASC",
        )?;
        let rows = statement.query_map(params![tip_height], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, i64>(1)?,
                row.get::<_, i64>(2)?,
            ))
        })?;

        let mut affected_address_ids = std::collections::BTreeSet::new();
        let mut dirty_since_by_address = std::collections::BTreeMap::new();
        let mut revoked_receipt_count = 0usize;

        for row in rows {
            let (receipt_uid, address_id, mined_height) = row?;
            tx.execute(
                "UPDATE address_receipts_recent SET receipt_state = 'revoked', state_version = state_version + 1, observed_at = ?2 WHERE receipt_uid = ?1",
                params![receipt_uid, now],
            )?;
            affected_address_ids.insert(address_id);
            dirty_since_by_address
                .entry(address_id)
                .and_modify(|height: &mut i64| *height = (*height).min(mined_height))
                .or_insert(mined_height);
            revoked_receipt_count += 1;
        }
        drop(statement);

        if affected_address_ids.is_empty() {
            tx.commit()?;
            return Ok(ReceiptRangeReconciliation {
                affected_address_ids: Vec::new(),
                inserted_receipt_count: 0,
                revoked_receipt_count: 0,
                reactivated_receipt_count: 0,
            });
        }

        recompute_address_totals(
            &tx,
            &affected_address_ids,
            tip_height,
            finality_confirmations,
            &now,
            "reorg_reconcile",
            &dirty_since_by_address,
        )?;

        tx.commit()?;
        Ok(ReceiptRangeReconciliation {
            affected_address_ids: affected_address_ids.into_iter().collect(),
            inserted_receipt_count: 0,
            revoked_receipt_count,
            reactivated_receipt_count: 0,
        })
    }

    pub fn active_mempool_txids(&self) -> Result<std::collections::BTreeSet<String>, AppError> {
        let conn = self.connect()?;
        let mut statement = conn.prepare(
            "SELECT DISTINCT txid FROM address_receipts_recent WHERE receipt_kind = 'mempool' AND receipt_state = 'active' ORDER BY txid ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, String>(0))?;
        let mut txids = std::collections::BTreeSet::new();
        for row in rows {
            txids.insert(row?);
        }
        Ok(txids)
    }

    pub fn reconcile_mempool_snapshot(
        &self,
        current_mempool_txids: &std::collections::BTreeSet<String>,
        receipts: &[AttributedScannedReceipt],
        now: &str,
        tip_height: i64,
        finality_confirmations: u32,
        webhook_report_confirmations: u32,
    ) -> Result<ReceiptRangeReconciliation, AppError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut existing_by_uid = std::collections::BTreeMap::new();
        let mut statement = tx.prepare(
            "SELECT receipt_uid, txid, address_id, mined_height, receipt_state, value_zat, confirmation_depth, eligible_for_webhook, first_observed_at FROM address_receipts_recent WHERE receipt_kind = 'mempool'",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, i64>(2)?,
                row.get::<_, i64>(3)?,
                row.get::<_, String>(4)?,
                row.get::<_, i64>(5)?,
                row.get::<_, i64>(6)?,
                row.get::<_, i64>(7)?,
                row.get::<_, String>(8)?,
            ))
        })?;
        for row in rows {
            let (
                receipt_uid,
                txid,
                address_id,
                mined_height,
                receipt_state,
                value_zat,
                confirmation_depth,
                eligible_for_webhook,
                first_observed_at,
            ) = row?;
            existing_by_uid.insert(
                receipt_uid,
                (
                    txid,
                    address_id,
                    mined_height,
                    receipt_state,
                    value_zat,
                    confirmation_depth,
                    eligible_for_webhook,
                    first_observed_at,
                ),
            );
        }
        drop(statement);

        let fresh_cutoff = mempool_fresh_cutoff(now)?;
        let mut affected_address_ids = std::collections::BTreeSet::new();
        let mut dirty_since_by_address = std::collections::BTreeMap::new();
        let mut revoked_receipt_count = 0usize;
        let mut reactivated_receipt_count = 0usize;
        let mut inserted_receipt_count = 0usize;

        for (receipt_uid, (txid, address_id, mined_height, receipt_state, _, _, _, _)) in
            &existing_by_uid
        {
            if receipt_state == "active" && !current_mempool_txids.contains(txid) {
                tx.execute(
                    "UPDATE address_receipts_recent SET receipt_state = 'revoked', state_version = state_version + 1, observed_at = ?2 WHERE receipt_uid = ?1",
                    params![receipt_uid, now],
                )?;
                affected_address_ids.insert(*address_id);
                dirty_since_by_address
                    .entry(*address_id)
                    .and_modify(|height: &mut i64| *height = (*height).min(*mined_height))
                    .or_insert(*mined_height);
                revoked_receipt_count += 1;
            }
        }

        for receipt in receipts {
            let prior = existing_by_uid.get(&receipt.receipt_uid);
            let effective_first_observed_at = prior
                .map(|(_, _, _, _, _, _, _, first_observed_at)| first_observed_at.clone())
                .unwrap_or_else(|| receipt.first_observed_at.clone());
            let eligible_for_webhook =
                webhook_report_confirmations == 0 && effective_first_observed_at > fresh_cutoff;
            let was_insert = prior.is_none();
            let was_revoked = prior
                .map(|(_, _, _, state, _, _, _, _)| state != "active")
                .unwrap_or(false);
            let was_changed = prior
                .map(
                    |(
                        _,
                        address_id,
                        mined_height,
                        _,
                        value_zat,
                        confirmation_depth,
                        eligible,
                        first_observed_at,
                    )| {
                        *address_id != receipt.address_id
                            || *mined_height != receipt.mined_height
                            || *value_zat != receipt.value_zat
                            || *confirmation_depth != receipt.confirmation_depth
                            || *eligible != if eligible_for_webhook { 1 } else { 0 }
                            || *first_observed_at != effective_first_observed_at
                    },
                )
                .unwrap_or(false);

            tx.execute(
                "INSERT INTO address_receipts_recent (address_id, txid, pool, receipt_uid, value_zat, mined_height, confirmation_depth, eligible_for_webhook, receipt_kind, first_observed_at, observed_at, receipt_state, state_version) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, 'mempool', ?9, ?10, 'active', 0) ON CONFLICT(receipt_uid) DO UPDATE SET address_id = excluded.address_id, txid = excluded.txid, pool = excluded.pool, value_zat = excluded.value_zat, mined_height = excluded.mined_height, confirmation_depth = excluded.confirmation_depth, eligible_for_webhook = excluded.eligible_for_webhook, receipt_kind = 'mempool', first_observed_at = excluded.first_observed_at, observed_at = excluded.observed_at, receipt_state = 'active', state_version = CASE WHEN address_receipts_recent.receipt_state != 'active' OR address_receipts_recent.address_id != excluded.address_id OR address_receipts_recent.value_zat != excluded.value_zat OR address_receipts_recent.eligible_for_webhook != excluded.eligible_for_webhook OR address_receipts_recent.first_observed_at != excluded.first_observed_at THEN address_receipts_recent.state_version + 1 ELSE address_receipts_recent.state_version END",
                params![
                    receipt.address_id,
                    receipt.txid_hex,
                    receipt.pool,
                    receipt.receipt_uid,
                    receipt.value_zat,
                    0,
                    0,
                    if eligible_for_webhook { 1 } else { 0 },
                    effective_first_observed_at,
                    now,
                ],
            )?;

            if was_insert {
                inserted_receipt_count += 1;
                affected_address_ids.insert(receipt.address_id);
                dirty_since_by_address
                    .entry(receipt.address_id)
                    .or_insert(0);
            } else if was_revoked || was_changed {
                if was_revoked {
                    reactivated_receipt_count += 1;
                }
                affected_address_ids.insert(receipt.address_id);
                dirty_since_by_address
                    .entry(receipt.address_id)
                    .or_insert(0);
            }
        }

        let mut statement = tx.prepare(
            "SELECT DISTINCT address_id FROM address_receipts_recent WHERE receipt_kind = 'mempool' AND receipt_state = 'active' AND eligible_for_webhook != CASE WHEN ?1 = 0 AND first_observed_at > ?2 THEN 1 ELSE 0 END",
        )?;
        let rows = statement.query_map(
            params![i64::from(webhook_report_confirmations), fresh_cutoff],
            |row| row.get::<_, i64>(0),
        )?;
        for row in rows {
            let address_id = row?;
            affected_address_ids.insert(address_id);
            dirty_since_by_address.entry(address_id).or_insert(0);
        }
        drop(statement);

        tx.execute(
            "UPDATE address_receipts_recent SET confirmation_depth = 0, eligible_for_webhook = CASE WHEN ?1 = 0 AND first_observed_at > ?2 THEN 1 ELSE 0 END, observed_at = CASE WHEN eligible_for_webhook != CASE WHEN ?1 = 0 AND first_observed_at > ?2 THEN 1 ELSE 0 END THEN ?3 ELSE observed_at END WHERE receipt_kind = 'mempool' AND receipt_state = 'active' AND eligible_for_webhook != CASE WHEN ?1 = 0 AND first_observed_at > ?2 THEN 1 ELSE 0 END",
            params![i64::from(webhook_report_confirmations), fresh_cutoff, now],
        )?;

        recompute_address_totals(
            &tx,
            &affected_address_ids,
            tip_height,
            finality_confirmations,
            now,
            "mempool_reconcile",
            &dirty_since_by_address,
        )?;

        tx.commit()?;
        Ok(ReceiptRangeReconciliation {
            affected_address_ids: affected_address_ids.into_iter().collect(),
            inserted_receipt_count,
            revoked_receipt_count,
            reactivated_receipt_count,
        })
    }

    pub fn queue_webhook_deliveries_for_dirty_addresses(
        &self,
        address_ids: &[i64],
    ) -> Result<Vec<i64>, AppError> {
        if address_ids.is_empty() {
            return Ok(Vec::new());
        }

        let now = now_rfc3339()?;
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut queued_address_ids = std::collections::BTreeSet::new();

        for address_id in address_ids
            .iter()
            .copied()
            .collect::<std::collections::BTreeSet<_>>()
        {
            let Some((address, current_total_zat, last_notified_total_zat, state_version, dirty_state)) = tx
                .query_row(
                    "SELECT issued_addresses.unified_address, address_totals.current_total_zat, address_totals.last_notified_total_zat, address_totals.state_version, address_totals.dirty_state FROM address_totals INNER JOIN issued_addresses ON issued_addresses.address_id = address_totals.address_id WHERE address_totals.address_id = ?1",
                    params![address_id],
                    |row| {
                        Ok((
                            row.get::<_, String>(0)?,
                            row.get::<_, i64>(1)?,
                            row.get::<_, i64>(2)?,
                            row.get::<_, i64>(3)?,
                            row.get::<_, String>(4)?,
                        ))
                    },
                )
                .optional()? else {
                    continue;
                };

            if dirty_state != "dirty" || current_total_zat == last_notified_total_zat {
                continue;
            }

            let event_id = format!("zcash:{address_id}:v{state_version}");
            let request_body_json = serde_json::to_string(&json!({
                "event_id": event_id,
                "payment_source": "zcash",
                "address": address,
                "total_received": format_zat_as_zec(current_total_zat),
                "observed_at": now,
            }))?;

            tx.execute(
                "INSERT OR IGNORE INTO webhook_deliveries (address_id, event_id, total_received_zat, observed_at, request_body_json, delivery_state, attempt_count, next_attempt_at, last_http_status, last_response_body, last_attempt_at) VALUES (?1, ?2, ?3, ?4, ?5, 'queued', 0, NULL, NULL, NULL, NULL)",
                params![address_id, event_id, current_total_zat, now, request_body_json],
            )?;

            tx.execute(
                "UPDATE address_totals SET dirty_state = 'queued_for_webhook', dirty_reason = 'webhook_queued', last_changed_at = ?2 WHERE address_id = ?1",
                params![address_id, now],
            )?;
            queued_address_ids.insert(address_id);
        }

        tx.commit()?;
        Ok(queued_address_ids.into_iter().collect())
    }

    pub fn queue_all_dirty_webhook_deliveries(&self) -> Result<Vec<i64>, AppError> {
        let conn = self.connect()?;
        let mut statement = conn.prepare(
            "SELECT address_id FROM address_totals WHERE dirty_state = 'dirty' ORDER BY address_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, i64>(0))?;
        let mut address_ids = Vec::new();
        for row in rows {
            address_ids.push(row?);
        }
        drop(statement);

        self.queue_webhook_deliveries_for_dirty_addresses(&address_ids)
    }

    pub fn recover_interrupted_webhook_deliveries(&self) -> Result<Vec<i64>, AppError> {
        let now = now_rfc3339()?;
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let mut delivery_ids = Vec::new();

        let mut statement = tx.prepare(
            "SELECT delivery_id FROM webhook_deliveries WHERE delivery_state = 'sending' ORDER BY delivery_id ASC",
        )?;
        let rows = statement.query_map([], |row| row.get::<_, i64>(0))?;
        for row in rows {
            delivery_ids.push(row?);
        }
        drop(statement);

        if delivery_ids.is_empty() {
            tx.commit()?;
            return Ok(Vec::new());
        }

        for delivery_id in &delivery_ids {
            tx.execute(
                "UPDATE webhook_deliveries SET delivery_state = 'retry_wait', next_attempt_at = ?2, last_response_body = COALESCE(last_response_body, 'delivery interrupted before completion'), last_attempt_at = COALESCE(last_attempt_at, ?2) WHERE delivery_id = ?1",
                params![delivery_id, now],
            )?;
        }

        tx.commit()?;
        Ok(delivery_ids)
    }

    pub fn claim_ready_webhook_delivery(
        &self,
        now: &str,
    ) -> Result<Option<QueuedWebhookDelivery>, AppError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let delivery = tx
            .query_row(
                "SELECT delivery_id, address_id, event_id, total_received_zat, request_body_json, attempt_count FROM webhook_deliveries WHERE delivery_state = 'queued' OR (delivery_state = 'retry_wait' AND next_attempt_at IS NOT NULL AND next_attempt_at <= ?1) ORDER BY observed_at ASC, delivery_id ASC LIMIT 1",
                params![now],
                |row| {
                    Ok(QueuedWebhookDelivery {
                        delivery_id: row.get(0)?,
                        address_id: row.get(1)?,
                        event_id: row.get(2)?,
                        total_received_zat: row.get(3)?,
                        request_body_json: row.get(4)?,
                        attempt_count: row.get(5)?,
                    })
                },
            )
            .optional()?;

        if let Some(delivery) = delivery {
            tx.execute(
                "UPDATE webhook_deliveries SET delivery_state = 'sending', next_attempt_at = NULL WHERE delivery_id = ?1",
                params![delivery.delivery_id],
            )?;
            tx.commit()?;
            Ok(Some(delivery))
        } else {
            tx.commit()?;
            Ok(None)
        }
    }

    pub fn record_webhook_delivery_success(
        &self,
        delivery_id: i64,
        attempted_at: &str,
        http_status: i64,
        response_body: &str,
        duration_ms: i64,
    ) -> Result<(), AppError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let (address_id, event_id, total_received_zat): (i64, String, i64) = tx.query_row(
            "SELECT address_id, event_id, total_received_zat FROM webhook_deliveries WHERE delivery_id = ?1",
            params![delivery_id],
            |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
        )?;

        tx.execute(
            "UPDATE webhook_deliveries SET delivery_state = 'succeeded', attempt_count = attempt_count + 1, next_attempt_at = NULL, last_http_status = ?2, last_response_body = ?3, last_attempt_at = ?4 WHERE delivery_id = ?1",
            params![delivery_id, http_status, response_body, attempted_at],
        )?;
        tx.execute(
            "INSERT INTO webhook_delivery_attempts (delivery_id, attempted_at, http_status, response_body, transport_error, duration_ms) VALUES (?1, ?2, ?3, ?4, NULL, ?5)",
            params![delivery_id, attempted_at, http_status, response_body, duration_ms],
        )?;
        tx.execute(
            "UPDATE address_totals SET last_notified_total_zat = ?2, last_notified_event_id = ?3, last_notified_at = ?4, dirty_state = CASE WHEN current_total_zat = ?2 THEN 'clean' ELSE 'dirty' END, dirty_reason = CASE WHEN current_total_zat = ?2 THEN NULL ELSE dirty_reason END, dirty_since_height = CASE WHEN current_total_zat = ?2 THEN NULL ELSE dirty_since_height END WHERE address_id = ?1",
            params![address_id, total_received_zat, event_id, attempted_at],
        )?;

        tx.commit()?;
        Ok(())
    }

    #[allow(clippy::too_many_arguments)]
    pub fn record_webhook_delivery_retry(
        &self,
        delivery_id: i64,
        attempted_at: &str,
        next_attempt_at: &str,
        http_status: Option<i64>,
        response_body: Option<&str>,
        transport_error: Option<&str>,
        duration_ms: i64,
    ) -> Result<(), AppError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE webhook_deliveries SET delivery_state = 'retry_wait', attempt_count = attempt_count + 1, next_attempt_at = ?2, last_http_status = ?3, last_response_body = ?4, last_attempt_at = ?5 WHERE delivery_id = ?1",
            params![delivery_id, next_attempt_at, http_status, response_body, attempted_at],
        )?;
        tx.execute(
            "INSERT INTO webhook_delivery_attempts (delivery_id, attempted_at, http_status, response_body, transport_error, duration_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![delivery_id, attempted_at, http_status, response_body, transport_error, duration_ms],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_webhook_delivery_permanent_failure(
        &self,
        delivery_id: i64,
        attempted_at: &str,
        http_status: Option<i64>,
        response_body: Option<&str>,
        transport_error: Option<&str>,
        duration_ms: i64,
    ) -> Result<(), AppError> {
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        tx.execute(
            "UPDATE webhook_deliveries SET delivery_state = 'permanent_failure', attempt_count = attempt_count + 1, next_attempt_at = NULL, last_http_status = ?2, last_response_body = ?3, last_attempt_at = ?4 WHERE delivery_id = ?1",
            params![delivery_id, http_status, response_body, attempted_at],
        )?;
        tx.execute(
            "INSERT INTO webhook_delivery_attempts (delivery_id, attempted_at, http_status, response_body, transport_error, duration_ms) VALUES (?1, ?2, ?3, ?4, ?5, ?6)",
            params![delivery_id, attempted_at, http_status, response_body, transport_error, duration_ms],
        )?;
        tx.commit()?;
        Ok(())
    }

    pub fn record_reorg_event(
        &self,
        previous_tip_height: i64,
        new_tip_height: i64,
        rewind_height: i64,
        affected_address_count: usize,
        notes: &str,
    ) -> Result<(), AppError> {
        let detected_at = now_rfc3339()?;
        let mut conn = self.connect()?;
        let tx = conn.transaction_with_behavior(TransactionBehavior::Immediate)?;
        let scan_epoch: i64 = tx.query_row(
            "SELECT current_scan_epoch FROM service_metadata WHERE singleton_id = 1",
            [],
            |row| row.get(0),
        )?;
        tx.execute(
            "INSERT INTO reorg_events (scan_epoch, detected_at, previous_tip_height, new_tip_height, rewind_height, affected_address_count, notes) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![scan_epoch, detected_at, previous_tip_height, new_tip_height, rewind_height, i64::try_from(affected_address_count).unwrap_or(i64::MAX), notes],
        )?;
        tx.commit()?;
        Ok(())
    }

    fn connect(&self) -> Result<Connection, AppError> {
        let conn = Connection::open(&self.path)?;
        conn.busy_timeout(Duration::from_secs(5))?;
        conn.pragma_update(None, "foreign_keys", "ON")?;
        Ok(conn)
    }

    fn initialize_schema(&self) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute_batch(
            "
            CREATE TABLE IF NOT EXISTS wallet_identity (
                singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
                canonical_uivk TEXT NOT NULL,
                uivk_fingerprint TEXT NOT NULL,
                network TEXT NOT NULL,
                birthday_height INTEGER NOT NULL,
                wallet_db_path TEXT NOT NULL,
                wallet_account_uuid BLOB,
                initialized_at TEXT NOT NULL,
                last_validated_at TEXT NOT NULL,
                startup_uivk_required INTEGER NOT NULL DEFAULT 1
            );

            CREATE TABLE IF NOT EXISTS service_metadata (
                singleton_id INTEGER PRIMARY KEY CHECK (singleton_id = 1),
                app_schema_version INTEGER NOT NULL,
                wallet_schema_version_seen INTEGER NOT NULL,
                last_seen_tip_height INTEGER NOT NULL,
                last_scanned_height INTEGER NOT NULL,
                last_stable_tip_height INTEGER NOT NULL,
                current_sync_state TEXT NOT NULL,
                current_scan_epoch INTEGER NOT NULL,
                catch_up_entered_at TEXT,
                catch_up_exited_at TEXT,
                webhook_report_confirmations INTEGER NOT NULL,
                finality_confirmations INTEGER NOT NULL
            );

            CREATE TABLE IF NOT EXISTS issued_addresses (
                address_id INTEGER PRIMARY KEY,
                unified_address TEXT NOT NULL UNIQUE,
                address_source TEXT NOT NULL CHECK (address_source IN ('fresh')),
                diversifier_index_be BLOB,
                diversifier_bytes BLOB,
                created_at TEXT NOT NULL,
                request_label TEXT,
                request_memo TEXT,
                request_message TEXT,
                requested_amount TEXT,
                retired_at TEXT
            );
            CREATE INDEX IF NOT EXISTS issued_addresses_created_at_idx ON issued_addresses (created_at);

            CREATE TABLE IF NOT EXISTS address_receivers (
                receiver_id INTEGER PRIMARY KEY,
                address_id INTEGER NOT NULL REFERENCES issued_addresses(address_id) ON DELETE CASCADE,
                pool TEXT NOT NULL CHECK (pool IN ('orchard', 'sapling')),
                receiver_encoding TEXT NOT NULL,
                receiver_fingerprint BLOB NOT NULL,
                is_active INTEGER NOT NULL DEFAULT 1,
                created_at TEXT NOT NULL,
                UNIQUE(address_id, pool),
                UNIQUE(pool, receiver_encoding),
                UNIQUE(pool, receiver_fingerprint)
            );

            CREATE TABLE IF NOT EXISTS address_totals (
                address_id INTEGER PRIMARY KEY REFERENCES issued_addresses(address_id) ON DELETE CASCADE,
                finalized_through_height INTEGER NOT NULL DEFAULT 0,
                finalized_total_zat INTEGER NOT NULL DEFAULT 0,
                recent_total_zat INTEGER NOT NULL DEFAULT 0,
                current_total_zat INTEGER NOT NULL DEFAULT 0,
                last_computed_tip_height INTEGER NOT NULL DEFAULT 0,
                dirty_state TEXT NOT NULL CHECK (dirty_state IN ('clean', 'dirty', 'queued_for_webhook')),
                dirty_reason TEXT,
                dirty_since_height INTEGER,
                state_version INTEGER NOT NULL DEFAULT 0,
                last_changed_at TEXT NOT NULL,
                last_notified_total_zat INTEGER NOT NULL DEFAULT 0,
                last_notified_event_id TEXT,
                last_notified_at TEXT
            );
            CREATE INDEX IF NOT EXISTS address_totals_dirty_state_idx ON address_totals (dirty_state);

            CREATE TABLE IF NOT EXISTS address_receipts_recent (
                receipt_id INTEGER PRIMARY KEY,
                address_id INTEGER NOT NULL REFERENCES issued_addresses(address_id) ON DELETE CASCADE,
                txid TEXT NOT NULL,
                pool TEXT NOT NULL CHECK (pool IN ('orchard', 'sapling')),
                receipt_uid TEXT NOT NULL UNIQUE,
                value_zat INTEGER NOT NULL,
                mined_height INTEGER NOT NULL,
                confirmation_depth INTEGER NOT NULL,
                eligible_for_webhook INTEGER NOT NULL,
                receipt_kind TEXT NOT NULL CHECK (receipt_kind IN ('mined', 'mempool')) DEFAULT 'mined',
                first_observed_at TEXT NOT NULL,
                observed_at TEXT NOT NULL,
                receipt_state TEXT NOT NULL CHECK (receipt_state IN ('active', 'revoked')),
                state_version INTEGER NOT NULL
            );
            CREATE INDEX IF NOT EXISTS address_receipts_recent_address_height_idx ON address_receipts_recent (address_id, mined_height);
            CREATE INDEX IF NOT EXISTS address_receipts_recent_address_eligibility_idx ON address_receipts_recent (address_id, eligible_for_webhook, receipt_state);
            CREATE INDEX IF NOT EXISTS address_receipts_recent_mempool_txid_idx ON address_receipts_recent (receipt_kind, receipt_state, txid);

            CREATE TABLE IF NOT EXISTS address_finality_checkpoints (
                checkpoint_id INTEGER PRIMARY KEY,
                address_id INTEGER NOT NULL REFERENCES issued_addresses(address_id) ON DELETE CASCADE,
                finalized_height INTEGER NOT NULL,
                finalized_total_zat INTEGER NOT NULL,
                receipt_count INTEGER NOT NULL,
                created_at TEXT NOT NULL,
                receipt_set_hash BLOB,
                UNIQUE(address_id, finalized_height)
            );

            CREATE TABLE IF NOT EXISTS sync_state_transitions (
                transition_id INTEGER PRIMARY KEY,
                from_state TEXT NOT NULL,
                to_state TEXT NOT NULL,
                reason TEXT NOT NULL,
                tip_height INTEGER NOT NULL,
                scanned_height INTEGER NOT NULL,
                scan_epoch INTEGER NOT NULL,
                detected_at TEXT NOT NULL,
                reorg_depth_estimate INTEGER
            );
            CREATE INDEX IF NOT EXISTS sync_state_transitions_epoch_detected_idx ON sync_state_transitions (scan_epoch, detected_at);

            CREATE TABLE IF NOT EXISTS reorg_events (
                reorg_id INTEGER PRIMARY KEY,
                scan_epoch INTEGER NOT NULL,
                detected_at TEXT NOT NULL,
                previous_tip_height INTEGER NOT NULL,
                new_tip_height INTEGER NOT NULL,
                rewind_height INTEGER NOT NULL,
                affected_address_count INTEGER NOT NULL,
                notes TEXT
            );
            CREATE INDEX IF NOT EXISTS reorg_events_detected_idx ON reorg_events (detected_at);

            CREATE TABLE IF NOT EXISTS webhook_deliveries (
                delivery_id INTEGER PRIMARY KEY,
                address_id INTEGER NOT NULL REFERENCES issued_addresses(address_id) ON DELETE CASCADE,
                event_id TEXT NOT NULL UNIQUE,
                total_received_zat INTEGER NOT NULL,
                observed_at TEXT NOT NULL,
                request_body_json TEXT NOT NULL,
                delivery_state TEXT NOT NULL CHECK (delivery_state IN ('queued', 'sending', 'retry_wait', 'succeeded', 'permanent_failure')),
                attempt_count INTEGER NOT NULL DEFAULT 0,
                next_attempt_at TEXT,
                last_http_status INTEGER,
                last_response_body TEXT,
                last_attempt_at TEXT
            );
            CREATE INDEX IF NOT EXISTS webhook_deliveries_state_next_idx ON webhook_deliveries (delivery_state, next_attempt_at);
            CREATE INDEX IF NOT EXISTS webhook_deliveries_address_observed_idx ON webhook_deliveries (address_id, observed_at);

            CREATE TABLE IF NOT EXISTS webhook_delivery_attempts (
                attempt_id INTEGER PRIMARY KEY,
                delivery_id INTEGER NOT NULL REFERENCES webhook_deliveries(delivery_id) ON DELETE CASCADE,
                attempted_at TEXT NOT NULL,
                http_status INTEGER,
                response_body TEXT,
                transport_error TEXT,
                duration_ms INTEGER
            );
            ",
        )?;

        ensure_address_receipts_recent_mempool_columns(&conn)?;

        Ok(())
    }

    fn wallet_identity(&self) -> Result<Option<WalletIdentity>, AppError> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT canonical_uivk, uivk_fingerprint, network, birthday_height, wallet_db_path, initialized_at, last_validated_at FROM wallet_identity WHERE singleton_id = 1",
            [],
            |row| {
                Ok(WalletIdentity {
                    canonical_uivk: row.get(0)?,
                    uivk_fingerprint: row.get(1)?,
                    network: row.get(2)?,
                    birthday_height: row.get(3)?,
                    wallet_db_path: row.get(4)?,
                    initialized_at: row.get(5)?,
                    last_validated_at: row.get(6)?,
                })
            },
        )
        .optional()
        .map_err(AppError::from)
    }

    fn insert_wallet_identity(&self, identity: &WalletIdentity) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO wallet_identity (singleton_id, canonical_uivk, uivk_fingerprint, network, birthday_height, wallet_db_path, initialized_at, last_validated_at) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7)",
            params![
                identity.canonical_uivk,
                identity.uivk_fingerprint,
                identity.network,
                identity.birthday_height,
                identity.wallet_db_path,
                identity.initialized_at,
                identity.last_validated_at,
            ],
        )?;
        Ok(())
    }

    fn touch_wallet_identity_validation(&self, validated_at: &str) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute(
            "UPDATE wallet_identity SET last_validated_at = ?1 WHERE singleton_id = 1",
            params![validated_at],
        )?;
        Ok(())
    }

    fn service_metadata(&self) -> Result<Option<ServiceMetadata>, AppError> {
        let conn = self.connect()?;
        conn.query_row(
            "SELECT app_schema_version, wallet_schema_version_seen, last_seen_tip_height, last_scanned_height, last_stable_tip_height, current_sync_state, current_scan_epoch, catch_up_entered_at, catch_up_exited_at, webhook_report_confirmations, finality_confirmations FROM service_metadata WHERE singleton_id = 1",
            [],
            |row| {
                Ok(ServiceMetadata {
                    app_schema_version: row.get(0)?,
                    wallet_schema_version_seen: row.get(1)?,
                    last_seen_tip_height: row.get(2)?,
                    last_scanned_height: row.get(3)?,
                    last_stable_tip_height: row.get(4)?,
                    current_sync_state: row.get(5)?,
                    current_scan_epoch: row.get(6)?,
                    catch_up_entered_at: row.get(7)?,
                    catch_up_exited_at: row.get(8)?,
                    webhook_report_confirmations: row.get(9)?,
                    finality_confirmations: row.get(10)?,
                })
            },
        )
        .optional()
        .map_err(AppError::from)
    }

    fn insert_service_metadata(&self, metadata: &ServiceMetadata) -> Result<(), AppError> {
        let conn = self.connect()?;
        conn.execute(
            "INSERT INTO service_metadata (singleton_id, app_schema_version, wallet_schema_version_seen, last_seen_tip_height, last_scanned_height, last_stable_tip_height, current_sync_state, current_scan_epoch, catch_up_entered_at, catch_up_exited_at, webhook_report_confirmations, finality_confirmations) VALUES (1, ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11)",
            params![
                metadata.app_schema_version,
                metadata.wallet_schema_version_seen,
                metadata.last_seen_tip_height,
                metadata.last_scanned_height,
                metadata.last_stable_tip_height,
                metadata.current_sync_state,
                metadata.current_scan_epoch,
                metadata.catch_up_entered_at,
                metadata.catch_up_exited_at,
                metadata.webhook_report_confirmations,
                metadata.finality_confirmations,
            ],
        )?;
        Ok(())
    }
}

fn is_singleton_insert_conflict(error: &SqliteError) -> bool {
    matches!(
        error,
        SqliteError::SqliteFailure(inner, _)
            if inner.code == ErrorCode::ConstraintViolation
    )
}

fn ensure_address_receipts_recent_mempool_columns(conn: &Connection) -> Result<(), AppError> {
    if !table_column_exists(conn, "address_receipts_recent", "receipt_kind")? {
        conn.execute(
            "ALTER TABLE address_receipts_recent ADD COLUMN receipt_kind TEXT NOT NULL DEFAULT 'mined'",
            [],
        )?;
    }

    if !table_column_exists(conn, "address_receipts_recent", "first_observed_at")? {
        conn.execute(
            "ALTER TABLE address_receipts_recent ADD COLUMN first_observed_at TEXT NOT NULL DEFAULT ''",
            [],
        )?;
        conn.execute(
            "UPDATE address_receipts_recent SET first_observed_at = observed_at WHERE first_observed_at = ''",
            [],
        )?;
    }

    conn.execute(
        "CREATE INDEX IF NOT EXISTS address_receipts_recent_mempool_txid_idx ON address_receipts_recent (receipt_kind, receipt_state, txid)",
        [],
    )?;

    Ok(())
}

fn table_column_exists(
    conn: &Connection,
    table_name: &str,
    column_name: &str,
) -> Result<bool, AppError> {
    let mut statement = conn.prepare(&format!("PRAGMA table_info({table_name})"))?;
    let rows = statement.query_map([], |row| row.get::<_, String>(1))?;
    for row in rows {
        if row? == column_name {
            return Ok(true);
        }
    }

    Ok(false)
}

fn now_rfc3339() -> Result<String, AppError> {
    Ok(OffsetDateTime::now_utc().format(&Rfc3339)?)
}

fn query_sum(tx: &rusqlite::Transaction<'_>, sql: &str, address_id: i64) -> Result<i64, AppError> {
    tx.query_row(sql, params![address_id], |row| row.get(0))
        .map_err(AppError::from)
}

fn query_sum_with_cutoff(
    tx: &rusqlite::Transaction<'_>,
    address_id: i64,
    finalized_cutoff_height: i64,
) -> Result<i64, AppError> {
    tx.query_row(
        "SELECT COALESCE(SUM(value_zat), 0) FROM address_receipts_recent WHERE address_id = ?1 AND receipt_kind = 'mined' AND receipt_state = 'active' AND eligible_for_webhook = 1 AND mined_height <= ?2",
        params![address_id, finalized_cutoff_height],
        |row| row.get(0),
    )
    .map_err(AppError::from)
}

fn mempool_fresh_cutoff(now: &str) -> Result<String, AppError> {
    let cutoff = OffsetDateTime::parse(now, &Rfc3339)? - time::Duration::hours(24);
    Ok(cutoff.format(&Rfc3339)?)
}

fn format_zat_as_zec(value_zat: i64) -> String {
    let sign = if value_zat < 0 { "-" } else { "" };
    let absolute = value_zat.unsigned_abs();
    let whole = absolute / 100_000_000;
    let fractional = absolute % 100_000_000;
    format!("{sign}{whole}.{fractional:08}")
}

fn recompute_address_totals(
    tx: &rusqlite::Transaction<'_>,
    address_ids: &std::collections::BTreeSet<i64>,
    tip_height: i64,
    finality_confirmations: u32,
    now: &str,
    dirty_reason_label: &str,
    dirty_since_by_address: &std::collections::BTreeMap<i64, i64>,
) -> Result<(), AppError> {
    let finalized_cutoff_height = if tip_height >= i64::from(finality_confirmations) {
        tip_height - i64::from(finality_confirmations) + 1
    } else {
        0
    };

    for address_id in address_ids {
        let previous: (i64, i64) = tx.query_row(
            "SELECT current_total_zat, state_version FROM address_totals WHERE address_id = ?1",
            params![address_id],
            |row| Ok((row.get(0)?, row.get(1)?)),
        )?;

        let active_total_zat = query_sum(
            tx,
            "SELECT COALESCE(SUM(value_zat), 0) FROM address_receipts_recent WHERE address_id = ?1 AND receipt_state = 'active'",
            *address_id,
        )?;
        let eligible_total_zat = query_sum(
            tx,
            "SELECT COALESCE(SUM(value_zat), 0) FROM address_receipts_recent WHERE address_id = ?1 AND receipt_state = 'active' AND eligible_for_webhook = 1",
            *address_id,
        )?;
        let finalized_total_zat = query_sum_with_cutoff(tx, *address_id, finalized_cutoff_height)?;
        let current_total_zat = eligible_total_zat;
        let recent_total_zat = current_total_zat - finalized_total_zat;
        let finalized_through_height: i64 = tx.query_row(
            "SELECT COALESCE(MAX(mined_height), 0) FROM address_receipts_recent WHERE address_id = ?1 AND receipt_kind = 'mined' AND receipt_state = 'active' AND eligible_for_webhook = 1 AND mined_height <= ?2",
            params![address_id, finalized_cutoff_height],
            |row| row.get(0),
        )?;
        let changed = previous.0 != current_total_zat;
        let next_state_version = if changed { previous.1 + 1 } else { previous.1 };
        let dirty_state = if changed { "dirty" } else { "clean" };
        let dirty_reason = if changed {
            Some(dirty_reason_label)
        } else {
            None
        };
        let dirty_since_height = if changed {
            dirty_since_by_address.get(address_id).copied()
        } else {
            None
        };

        tx.execute(
            "UPDATE address_totals SET finalized_through_height = ?2, finalized_total_zat = ?3, recent_total_zat = ?4, current_total_zat = ?5, last_computed_tip_height = ?6, dirty_state = ?7, dirty_reason = ?8, dirty_since_height = ?9, state_version = ?10, last_changed_at = ?11 WHERE address_id = ?1",
            params![
                address_id,
                finalized_through_height,
                finalized_total_zat,
                recent_total_zat,
                current_total_zat,
                tip_height,
                dirty_state,
                dirty_reason,
                dirty_since_height,
                next_state_version,
                now,
            ],
        )?;

        tracing::info!(
            address_id = *address_id,
            active_total_zat,
            current_total_zat,
            eligible_total_zat,
            finalized_total_zat,
            tip_height,
            dirty_reason = dirty_reason.unwrap_or("none"),
            "recomputed address totals"
        );
    }

    Ok(())
}

fn fingerprint(value: &str) -> String {
    let mut a: u64 = 0xcbf29ce484222325;
    for byte in value.as_bytes() {
        a ^= u64::from(*byte);
        a = a.wrapping_mul(0x100000001b3);
    }
    format!("{a:016x}")
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::{
        AppDb, AttributedScannedReceipt, NewAddressReceiver, NewIssuedAddress, ReceiptKind,
    };
    use crate::config::Config;

    #[allow(clippy::too_many_arguments)]
    fn mined_receipt(
        address_id: i64,
        txid_hex: &str,
        pool: &str,
        receipt_uid: &str,
        value_zat: i64,
        mined_height: i64,
        confirmation_depth: i64,
        eligible_for_webhook: bool,
    ) -> AttributedScannedReceipt {
        AttributedScannedReceipt {
            address_id,
            txid_hex: txid_hex.into(),
            pool: pool.into(),
            receipt_uid: receipt_uid.into(),
            value_zat,
            mined_height,
            confirmation_depth,
            eligible_for_webhook,
            receipt_kind: ReceiptKind::Mined,
            first_observed_at: "2026-01-01T00:00:00Z".into(),
            last_observed_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn mempool_receipt(
        address_id: i64,
        txid_hex: &str,
        pool: &str,
        receipt_uid: &str,
        value_zat: i64,
        observed_at: &str,
    ) -> AttributedScannedReceipt {
        AttributedScannedReceipt {
            address_id,
            txid_hex: txid_hex.into(),
            pool: pool.into(),
            receipt_uid: receipt_uid.into(),
            value_zat,
            mined_height: 0,
            confirmation_depth: 0,
            eligible_for_webhook: true,
            receipt_kind: ReceiptKind::Mempool,
            first_observed_at: observed_at.into(),
            last_observed_at: observed_at.into(),
        }
    }

    fn create_wallet_db(temp: &std::path::Path, uivk: &str, birthday_height: u32) {
        let conn = rusqlite::Connection::open(temp.join("wallet.db")).unwrap();
        conn.execute_batch(
            "
            CREATE TABLE accounts (
                uivk TEXT NOT NULL,
                birthday_height INTEGER NOT NULL
            );
            ",
        )
        .unwrap();
        conn.execute(
            "INSERT INTO accounts (uivk, birthday_height) VALUES (?1, ?2)",
            rusqlite::params![uivk, birthday_height],
        )
        .unwrap();
    }

    fn test_config(temp: &std::path::Path, startup_uivk: Option<&str>) -> Config {
        Config {
            listen_addr: "127.0.0.1:0".into(),
            network: "testnet".into(),
            startup_uivk: startup_uivk.map(ToOwned::to_owned),
            lightwalletd_url: None,
            birthday_height: Some(123),
            wallet_db_path: temp.join("wallet.db"),
            app_db_path: temp.join("app.db"),
            log_dir: temp.join("logs"),
            catch_up_threshold_blocks: 1,
            catch_up_batch_size: 100,
            sync_poll_interval_seconds: 5,
            webhook_url: None,
            webhook_secret: None,
            webhook_poll_interval_seconds: 2,
            webhook_retry_delay_seconds: 30,
            webhook_retry_max_delay_seconds: 300,
            webhook_max_attempts: 8,
            webhook_report_confirmations: 1,
            finality_confirmations: 100,
        }
    }

    #[test]
    fn first_startup_requires_uivk() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let config = test_config(temp.path(), None);

        let error = db.ensure_wallet_identity(&config).unwrap_err();
        assert!(error
            .to_string()
            .contains("first startup requires ZCASH_UIVK"));
    }

    #[test]
    fn startup_rejects_mismatched_uivk() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let first = test_config(temp.path(), Some("uivk-one"));
        db.ensure_wallet_identity(&first).unwrap();

        let second = test_config(temp.path(), Some("uivk-two"));
        let error = db.ensure_wallet_identity(&second).unwrap_err();
        assert!(error
            .to_string()
            .contains("does not match stored wallet identity"));
    }

    #[test]
    fn startup_rejects_mismatch_with_existing_wallet_db_identity() {
        let temp = tempdir().unwrap();
        create_wallet_db(temp.path(), "wallet-uivk", 777);
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let config = test_config(temp.path(), Some("different-uivk"));

        let error = db.ensure_wallet_identity(&config).unwrap_err();
        assert!(error
            .to_string()
            .contains("does not match the existing wallet DB identity"));
    }

    #[test]
    fn startup_can_seed_from_existing_wallet_db_identity() {
        let temp = tempdir().unwrap();
        create_wallet_db(temp.path(), "wallet-uivk", 777);
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let mut config = test_config(temp.path(), None);
        config.birthday_height = None;

        let identity = db.ensure_wallet_identity(&config).unwrap();
        assert_eq!(identity.canonical_uivk, "wallet-uivk");
        assert_eq!(identity.birthday_height, 777);
    }

    #[test]
    fn restart_without_uivk_succeeds_after_identity_is_stored() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let first = test_config(temp.path(), Some("uivk-one"));
        db.ensure_wallet_identity(&first).unwrap();

        let second = test_config(temp.path(), None);
        let identity = db.ensure_wallet_identity(&second).unwrap();
        assert_eq!(identity.canonical_uivk, "uivk-one");
    }

    #[test]
    fn persists_diversifier_and_receivers() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1test",
                address_source: "fresh",
                diversifier_index_be: Some(&[1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]),
                diversifier_bytes: Some(&[1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11]),
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();

        db.insert_address_receivers(
            record.address_id,
            &[
                NewAddressReceiver {
                    pool: "orchard",
                    receiver_encoding: "orchard-1",
                    receiver_fingerprint: &[9, 9, 9],
                },
                NewAddressReceiver {
                    pool: "sapling",
                    receiver_encoding: "sapling-1",
                    receiver_fingerprint: &[8, 8, 8],
                },
            ],
        )
        .unwrap();

        assert_eq!(
            db.latest_diversifier_index_be().unwrap().unwrap(),
            vec![1, 0, 0, 0, 0, 0, 0, 0, 0, 0, 0]
        );
    }

    #[test]
    fn records_sync_state_transitions_when_state_changes() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        db.ensure_service_metadata(&test_config(temp.path(), Some("uivk-one")))
            .unwrap();

        let metadata = db
            .record_sync_status("scan_keys_ready", "scanner keys are ready", 0, 0)
            .unwrap();

        assert_eq!(metadata.current_sync_state, "scan_keys_ready");
        assert_eq!(metadata.current_scan_epoch, 1);
    }

    #[test]
    fn applies_attributed_receipts_and_updates_address_totals() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1test",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[
                NewAddressReceiver {
                    pool: "sapling",
                    receiver_encoding: "sapling-1",
                    receiver_fingerprint: &[1, 2, 3, 4, 5, 6, 7, 8],
                },
                NewAddressReceiver {
                    pool: "orchard",
                    receiver_encoding: "orchard-1",
                    receiver_fingerprint: &[8, 7, 6, 5, 4, 3, 2, 1],
                },
            ],
        )
        .unwrap();

        let affected = db
            .apply_attributed_receipts(
                &[
                    mined_receipt(
                        record.address_id,
                        "tx-one",
                        "sapling",
                        "tx-one:sapling:0",
                        12,
                        100,
                        5,
                        true,
                    ),
                    mined_receipt(
                        record.address_id,
                        "tx-one",
                        "orchard",
                        "tx-one:orchard:1",
                        34,
                        100,
                        5,
                        true,
                    ),
                ],
                104,
                100,
            )
            .unwrap();

        assert_eq!(affected, vec![record.address_id]);

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let totals: (i64, i64, i64, String) = conn
            .query_row(
                "SELECT current_total_zat, recent_total_zat, finalized_total_zat, dirty_state FROM address_totals WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?, row.get(3)?)),
            )
            .unwrap();

        assert_eq!(totals.0, 46);
        assert_eq!(totals.1, 46);
        assert_eq!(totals.2, 0);
        assert_eq!(totals.3, "dirty");
    }

    #[test]
    fn queueing_dirty_addresses_creates_webhook_delivery_and_marks_queue_state() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1queue",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-q",
                receiver_fingerprint: &[1, 1, 1, 1, 1, 1, 1, 1],
            }],
        )
        .unwrap();
        db.apply_attributed_receipts(
            &[mined_receipt(
                record.address_id,
                "tx-queue",
                "sapling",
                "tx-queue:sapling:0",
                100_000_000,
                200,
                5,
                true,
            )],
            204,
            100,
        )
        .unwrap();

        let queued = db
            .queue_webhook_deliveries_for_dirty_addresses(&[record.address_id])
            .unwrap();

        assert_eq!(queued, vec![record.address_id]);

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let delivery: (String, i64, String) = conn
            .query_row(
                "SELECT event_id, total_received_zat, delivery_state FROM webhook_deliveries WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let dirty_state: String = conn
            .query_row(
                "SELECT dirty_state FROM address_totals WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(delivery.0, format!("zcash:{}:v1", record.address_id));
        assert_eq!(delivery.1, 100_000_000);
        assert_eq!(delivery.2, "queued");
        assert_eq!(dirty_state, "queued_for_webhook");
    }

    #[test]
    fn claiming_and_succeeding_webhook_delivery_updates_notification_state() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1success",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-s",
                receiver_fingerprint: &[3, 3, 3, 3, 3, 3, 3, 3],
            }],
        )
        .unwrap();
        db.apply_attributed_receipts(
            &[mined_receipt(
                record.address_id,
                "tx-success",
                "sapling",
                "tx-success:sapling:0",
                25,
                400,
                10,
                true,
            )],
            409,
            100,
        )
        .unwrap();
        db.queue_webhook_deliveries_for_dirty_addresses(&[record.address_id])
            .unwrap();

        let delivery = db
            .claim_ready_webhook_delivery("2026-01-01T00:00:00Z")
            .unwrap()
            .unwrap();
        db.record_webhook_delivery_success(
            delivery.delivery_id,
            "2026-01-01T00:00:01Z",
            200,
            "{\"ok\":true}",
            15,
        )
        .unwrap();

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let delivery_state: String = conn
            .query_row(
                "SELECT delivery_state FROM webhook_deliveries WHERE delivery_id = ?1",
                rusqlite::params![delivery.delivery_id],
                |row| row.get(0),
            )
            .unwrap();
        let totals: (i64, Option<String>, String) = conn
            .query_row(
                "SELECT last_notified_total_zat, last_notified_event_id, dirty_state FROM address_totals WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        assert_eq!(delivery_state, "succeeded");
        assert_eq!(totals.0, 25);
        assert_eq!(totals.1, Some(format!("zcash:{}:v1", record.address_id)));
        assert_eq!(totals.2, "clean");
    }

    #[test]
    fn queue_all_dirty_webhook_deliveries_recovers_unqueued_dirty_addresses() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1recover-dirty",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-rd",
                receiver_fingerprint: &[6, 6, 6, 6, 6, 6, 6, 6],
            }],
        )
        .unwrap();
        db.apply_attributed_receipts(
            &[mined_receipt(
                record.address_id,
                "tx-recover-dirty",
                "sapling",
                "tx-recover-dirty:sapling:0",
                10,
                600,
                5,
                true,
            )],
            604,
            100,
        )
        .unwrap();

        let queued = db.queue_all_dirty_webhook_deliveries().unwrap();

        assert_eq!(queued, vec![record.address_id]);
    }

    #[test]
    fn interrupted_sending_delivery_is_requeued_for_retry() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1recover-send",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-rs",
                receiver_fingerprint: &[7, 7, 7, 7, 7, 7, 7, 7],
            }],
        )
        .unwrap();
        db.apply_attributed_receipts(
            &[mined_receipt(
                record.address_id,
                "tx-recover-send",
                "sapling",
                "tx-recover-send:sapling:0",
                11,
                700,
                5,
                true,
            )],
            704,
            100,
        )
        .unwrap();
        db.queue_webhook_deliveries_for_dirty_addresses(&[record.address_id])
            .unwrap();

        let claimed = db
            .claim_ready_webhook_delivery("2026-01-01T00:00:00Z")
            .unwrap()
            .unwrap();

        let recovered = db.recover_interrupted_webhook_deliveries().unwrap();

        assert_eq!(recovered, vec![claimed.delivery_id]);

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let state: (String, Option<String>) = conn
            .query_row(
                "SELECT delivery_state, next_attempt_at FROM webhook_deliveries WHERE delivery_id = ?1",
                rusqlite::params![claimed.delivery_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(state.0, "retry_wait");
        assert!(state.1.is_some());
    }

    #[test]
    fn ineligible_receipts_do_not_advance_reported_total() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1eligible",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-e",
                receiver_fingerprint: &[2, 2, 2, 2, 2, 2, 2, 2],
            }],
        )
        .unwrap();
        db.apply_attributed_receipts(
            &[mined_receipt(
                record.address_id,
                "tx-pending",
                "sapling",
                "tx-pending:sapling:0",
                75,
                300,
                0,
                false,
            )],
            299,
            100,
        )
        .unwrap();

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let totals: (i64, i64, i64) = conn
            .query_row(
                "SELECT current_total_zat, recent_total_zat, finalized_total_zat FROM address_totals WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        assert_eq!(totals, (0, 0, 0));
    }

    #[test]
    fn maturity_pass_promotes_receipts_and_advances_reported_total() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1mature",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-m",
                receiver_fingerprint: &[5, 5, 5, 5, 5, 5, 5, 5],
            }],
        )
        .unwrap();
        db.apply_attributed_receipts(
            &[mined_receipt(
                record.address_id,
                "tx-mature",
                "sapling",
                "tx-mature:sapling:0",
                123,
                500,
                1,
                false,
            )],
            500,
            100,
        )
        .unwrap();

        let affected = db.advance_receipt_maturity(501, 2, 100).unwrap();

        assert_eq!(affected, vec![record.address_id]);

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let receipt: (i64, i64) = conn
            .query_row(
                "SELECT confirmation_depth, eligible_for_webhook FROM address_receipts_recent WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let totals: (i64, String, i64) = conn
            .query_row(
                "SELECT current_total_zat, dirty_reason, state_version FROM address_totals WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();

        assert_eq!(receipt, (2, 1));
        assert_eq!(totals.0, 123);
        assert_eq!(totals.1, "receipt_matured");
        assert_eq!(totals.2, 1);
    }

    #[test]
    fn mempool_receipts_age_out_and_stop_counting_after_24_hours() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1mempool-age",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-ma",
                receiver_fingerprint: &[9, 1, 9, 1, 9, 1, 9, 1],
            }],
        )
        .unwrap();

        let current_txids = std::collections::BTreeSet::from(["tx-mempool-age".to_string()]);
        db.reconcile_mempool_snapshot(
            &current_txids,
            &[mempool_receipt(
                record.address_id,
                "tx-mempool-age",
                "sapling",
                "tx-mempool-age:sapling:0",
                42,
                "2026-01-01T00:00:00Z",
            )],
            "2026-01-01T00:00:00Z",
            1000,
            100,
            0,
        )
        .unwrap();

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let fresh_total: i64 = conn
            .query_row(
                "SELECT current_total_zat FROM address_totals WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(fresh_total, 42);

        db.reconcile_mempool_snapshot(&current_txids, &[], "2026-01-02T01:00:00Z", 1000, 100, 0)
            .unwrap();

        let stale: (i64, i64, String) = conn
            .query_row(
                "SELECT current_total_zat, recent_total_zat, dirty_reason FROM address_totals WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        assert_eq!(stale.0, 0);
        assert_eq!(stale.1, 0);
        assert_eq!(stale.2, "mempool_reconcile");
    }

    #[test]
    fn confirmed_receipt_replaces_provisional_mempool_receipt() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1mempool-confirm",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-mc",
                receiver_fingerprint: &[3, 1, 4, 1, 5, 9, 2, 6],
            }],
        )
        .unwrap();

        db.reconcile_mempool_snapshot(
            &std::collections::BTreeSet::from(["tx-mempool-confirm".to_string()]),
            &[mempool_receipt(
                record.address_id,
                "tx-mempool-confirm",
                "sapling",
                "tx-mempool-confirm:sapling:0",
                75,
                "2026-01-01T00:00:00Z",
            )],
            "2026-01-01T00:00:00Z",
            1000,
            100,
            0,
        )
        .unwrap();

        db.reconcile_attributed_receipts_in_range(
            &[mined_receipt(
                record.address_id,
                "tx-mempool-confirm",
                "sapling",
                "tx-mempool-confirm:sapling:0",
                75,
                1001,
                1,
                true,
            )],
            1001,
            1001,
            1001,
            100,
        )
        .unwrap();

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let receipt: (String, i64, i64) = conn
            .query_row(
                "SELECT receipt_kind, mined_height, value_zat FROM address_receipts_recent WHERE receipt_uid = 'tx-mempool-confirm:sapling:0'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let current_total: i64 = conn
            .query_row(
                "SELECT current_total_zat FROM address_totals WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(receipt.0, "mined");
        assert_eq!(receipt.1, 1001);
        assert_eq!(receipt.2, 75);
        assert_eq!(current_total, 75);
    }

    #[test]
    fn range_reconciliation_revokes_missing_mined_receipts_and_reduces_totals() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1reorg-revoke",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-rr",
                receiver_fingerprint: &[4, 4, 4, 4, 4, 4, 4, 4],
            }],
        )
        .unwrap();

        db.apply_attributed_receipts(
            &[
                mined_receipt(
                    record.address_id,
                    "tx-reorg-stays",
                    "sapling",
                    "tx-reorg-stays:sapling:0",
                    25,
                    1000,
                    3,
                    true,
                ),
                mined_receipt(
                    record.address_id,
                    "tx-reorg-revoked",
                    "sapling",
                    "tx-reorg-revoked:sapling:0",
                    75,
                    1001,
                    2,
                    true,
                ),
            ],
            1002,
            100,
        )
        .unwrap();

        let reconciliation = db
            .reconcile_attributed_receipts_in_range(
                &[mined_receipt(
                    record.address_id,
                    "tx-reorg-stays",
                    "sapling",
                    "tx-reorg-stays:sapling:0",
                    25,
                    1000,
                    3,
                    true,
                )],
                1000,
                1001,
                1002,
                100,
            )
            .unwrap();

        assert_eq!(reconciliation.affected_address_ids, vec![record.address_id]);
        assert_eq!(reconciliation.inserted_receipt_count, 0);
        assert_eq!(reconciliation.revoked_receipt_count, 1);
        assert_eq!(reconciliation.reactivated_receipt_count, 0);

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let revoked_state: String = conn
            .query_row(
                "SELECT receipt_state FROM address_receipts_recent WHERE receipt_uid = 'tx-reorg-revoked:sapling:0'",
                [],
                |row| row.get(0),
            )
            .unwrap();
        let totals: (i64, String) = conn
            .query_row(
                "SELECT current_total_zat, dirty_reason FROM address_totals WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();

        assert_eq!(revoked_state, "revoked");
        assert_eq!(totals.0, 25);
        assert_eq!(totals.1, "reorg_reconcile");
    }

    #[test]
    fn range_reconciliation_reactivates_revoked_mined_receipts_when_they_return() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1reorg-reactivate",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-ra",
                receiver_fingerprint: &[5, 5, 5, 5, 5, 5, 5, 5],
            }],
        )
        .unwrap();

        db.apply_attributed_receipts(
            &[mined_receipt(
                record.address_id,
                "tx-reorg-reactivate",
                "sapling",
                "tx-reorg-reactivate:sapling:0",
                90,
                1001,
                2,
                true,
            )],
            1002,
            100,
        )
        .unwrap();

        db.reconcile_attributed_receipts_in_range(&[], 1001, 1001, 1002, 100)
            .unwrap();

        let reconciliation = db
            .reconcile_attributed_receipts_in_range(
                &[mined_receipt(
                    record.address_id,
                    "tx-reorg-reactivate",
                    "sapling",
                    "tx-reorg-reactivate:sapling:0",
                    90,
                    1001,
                    2,
                    true,
                )],
                1001,
                1001,
                1002,
                100,
            )
            .unwrap();

        assert_eq!(reconciliation.affected_address_ids, vec![record.address_id]);
        assert_eq!(reconciliation.inserted_receipt_count, 0);
        assert_eq!(reconciliation.revoked_receipt_count, 0);
        assert_eq!(reconciliation.reactivated_receipt_count, 1);

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let receipt: (String, i64) = conn
            .query_row(
                "SELECT receipt_state, state_version FROM address_receipts_recent WHERE receipt_uid = 'tx-reorg-reactivate:sapling:0'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let total: i64 = conn
            .query_row(
                "SELECT current_total_zat FROM address_totals WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(receipt.0, "active");
        assert!(receipt.1 >= 2);
        assert_eq!(total, 90);
    }

    #[test]
    fn range_reconciliation_moves_changed_receipts_between_addresses_and_recomputes_both_totals() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let first = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1reorg-move-one",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        let second = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1reorg-move-two",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            first.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-rm1",
                receiver_fingerprint: &[6, 6, 6, 6, 6, 6, 6, 6],
            }],
        )
        .unwrap();
        db.insert_address_receivers(
            second.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-rm2",
                receiver_fingerprint: &[7, 7, 7, 7, 7, 7, 7, 7],
            }],
        )
        .unwrap();

        db.apply_attributed_receipts(
            &[mined_receipt(
                first.address_id,
                "tx-reorg-move",
                "sapling",
                "tx-reorg-move:sapling:0",
                40,
                1001,
                2,
                true,
            )],
            1002,
            100,
        )
        .unwrap();

        let reconciliation = db
            .reconcile_attributed_receipts_in_range(
                &[mined_receipt(
                    second.address_id,
                    "tx-reorg-move",
                    "sapling",
                    "tx-reorg-move:sapling:0",
                    55,
                    1001,
                    2,
                    true,
                )],
                1001,
                1001,
                1002,
                100,
            )
            .unwrap();

        assert_eq!(
            reconciliation.affected_address_ids,
            vec![first.address_id, second.address_id]
        );
        assert_eq!(reconciliation.inserted_receipt_count, 0);
        assert_eq!(reconciliation.revoked_receipt_count, 0);
        assert_eq!(reconciliation.reactivated_receipt_count, 0);

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let receipt: (i64, i64, String) = conn
            .query_row(
                "SELECT address_id, value_zat, receipt_state FROM address_receipts_recent WHERE receipt_uid = 'tx-reorg-move:sapling:0'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let first_total: i64 = conn
            .query_row(
                "SELECT current_total_zat FROM address_totals WHERE address_id = ?1",
                rusqlite::params![first.address_id],
                |row| row.get(0),
            )
            .unwrap();
        let second_total: i64 = conn
            .query_row(
                "SELECT current_total_zat FROM address_totals WHERE address_id = ?1",
                rusqlite::params![second.address_id],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(receipt.0, second.address_id);
        assert_eq!(receipt.1, 55);
        assert_eq!(receipt.2, "active");
        assert_eq!(first_total, 0);
        assert_eq!(second_total, 55);
    }

    #[test]
    fn successive_overlap_reconciliations_preserve_correct_totals_across_revoke_and_restore() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1multi-range",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-multi",
                receiver_fingerprint: &[8, 8, 8, 8, 8, 8, 8, 8],
            }],
        )
        .unwrap();

        db.apply_attributed_receipts(
            &[
                mined_receipt(
                    record.address_id,
                    "tx-stable",
                    "sapling",
                    "tx-stable:sapling:0",
                    20,
                    1000,
                    3,
                    true,
                ),
                mined_receipt(
                    record.address_id,
                    "tx-flaps",
                    "sapling",
                    "tx-flaps:sapling:0",
                    30,
                    1001,
                    2,
                    true,
                ),
            ],
            1002,
            100,
        )
        .unwrap();

        let first_reconciliation = db
            .reconcile_attributed_receipts_in_range(
                &[mined_receipt(
                    record.address_id,
                    "tx-stable",
                    "sapling",
                    "tx-stable:sapling:0",
                    20,
                    1000,
                    3,
                    true,
                )],
                1000,
                1001,
                1002,
                100,
            )
            .unwrap();

        assert_eq!(
            first_reconciliation.affected_address_ids,
            vec![record.address_id]
        );
        assert_eq!(first_reconciliation.revoked_receipt_count, 1);
        assert_eq!(first_reconciliation.reactivated_receipt_count, 0);

        let second_reconciliation = db
            .reconcile_attributed_receipts_in_range(
                &[
                    mined_receipt(
                        record.address_id,
                        "tx-stable",
                        "sapling",
                        "tx-stable:sapling:0",
                        20,
                        1000,
                        4,
                        true,
                    ),
                    mined_receipt(
                        record.address_id,
                        "tx-flaps",
                        "sapling",
                        "tx-flaps:sapling:0",
                        30,
                        1001,
                        3,
                        true,
                    ),
                ],
                1000,
                1001,
                1003,
                100,
            )
            .unwrap();

        assert_eq!(
            second_reconciliation.affected_address_ids,
            vec![record.address_id]
        );
        assert_eq!(second_reconciliation.inserted_receipt_count, 0);
        assert_eq!(second_reconciliation.revoked_receipt_count, 0);
        assert_eq!(second_reconciliation.reactivated_receipt_count, 1);

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let flap_receipt: (String, i64, i64) = conn
            .query_row(
                "SELECT receipt_state, confirmation_depth, state_version FROM address_receipts_recent WHERE receipt_uid = 'tx-flaps:sapling:0'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)),
            )
            .unwrap();
        let total: i64 = conn
            .query_row(
                "SELECT current_total_zat FROM address_totals WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(flap_receipt.0, "active");
        assert_eq!(flap_receipt.1, 3);
        assert!(flap_receipt.2 >= 2);
        assert_eq!(total, 50);
    }

    #[test]
    fn shifted_overlap_reconciliations_preserve_receipts_outside_the_current_window() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1shifted-range",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-shifted",
                receiver_fingerprint: &[9, 9, 9, 9, 9, 9, 9, 9],
            }],
        )
        .unwrap();

        db.apply_attributed_receipts(
            &[
                mined_receipt(
                    record.address_id,
                    "tx-older-stable",
                    "sapling",
                    "tx-older-stable:sapling:0",
                    20,
                    1000,
                    4,
                    true,
                ),
                mined_receipt(
                    record.address_id,
                    "tx-middle-flap",
                    "sapling",
                    "tx-middle-flap:sapling:0",
                    30,
                    1001,
                    3,
                    true,
                ),
                mined_receipt(
                    record.address_id,
                    "tx-newer-stable",
                    "sapling",
                    "tx-newer-stable:sapling:0",
                    40,
                    1002,
                    2,
                    true,
                ),
            ],
            1003,
            100,
        )
        .unwrap();

        let first_reconciliation = db
            .reconcile_attributed_receipts_in_range(
                &[mined_receipt(
                    record.address_id,
                    "tx-older-stable",
                    "sapling",
                    "tx-older-stable:sapling:0",
                    20,
                    1000,
                    4,
                    true,
                )],
                1000,
                1001,
                1003,
                100,
            )
            .unwrap();

        assert_eq!(
            first_reconciliation.affected_address_ids,
            vec![record.address_id]
        );
        assert_eq!(first_reconciliation.revoked_receipt_count, 1);

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let total_after_first: i64 = conn
            .query_row(
                "SELECT current_total_zat FROM address_totals WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(total_after_first, 60);

        let second_reconciliation = db
            .reconcile_attributed_receipts_in_range(
                &[
                    mined_receipt(
                        record.address_id,
                        "tx-middle-flap",
                        "sapling",
                        "tx-middle-flap:sapling:0",
                        30,
                        1001,
                        4,
                        true,
                    ),
                    mined_receipt(
                        record.address_id,
                        "tx-newer-stable",
                        "sapling",
                        "tx-newer-stable:sapling:0",
                        40,
                        1002,
                        3,
                        true,
                    ),
                ],
                1001,
                1002,
                1004,
                100,
            )
            .unwrap();

        assert_eq!(
            second_reconciliation.affected_address_ids,
            vec![record.address_id]
        );
        assert_eq!(second_reconciliation.inserted_receipt_count, 0);
        assert_eq!(second_reconciliation.revoked_receipt_count, 0);
        assert_eq!(second_reconciliation.reactivated_receipt_count, 1);

        let middle_receipt: (String, i64) = conn
            .query_row(
                "SELECT receipt_state, confirmation_depth FROM address_receipts_recent WHERE receipt_uid = 'tx-middle-flap:sapling:0'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        let total_after_second: i64 = conn
            .query_row(
                "SELECT current_total_zat FROM address_totals WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| row.get(0),
            )
            .unwrap();

        assert_eq!(middle_receipt.0, "active");
        assert_eq!(middle_receipt.1, 4);
        assert_eq!(total_after_second, 90);
    }

    #[test]
    fn deep_rewind_cleanup_revokes_mined_receipts_above_the_new_tip() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1reorg-deep-rewind",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-deep-rewind",
                receiver_fingerprint: &[4, 4, 4, 4, 4, 4, 4, 4],
            }],
        )
        .unwrap();
        db.apply_attributed_receipts(
            &[
                mined_receipt(
                    record.address_id,
                    "tx-reorg-before-tip",
                    "sapling",
                    "tx-reorg-before-tip:sapling:0",
                    5,
                    895,
                    10,
                    true,
                ),
                mined_receipt(
                    record.address_id,
                    "tx-reorg-above-tip",
                    "sapling",
                    "tx-reorg-above-tip:sapling:0",
                    7,
                    905,
                    10,
                    true,
                ),
            ],
            910,
            100,
        )
        .unwrap();

        let reconciliation = db.revoke_mined_receipts_above_tip(900, 100).unwrap();

        assert_eq!(reconciliation.revoked_receipt_count, 1);
        assert_eq!(reconciliation.reactivated_receipt_count, 0);
        assert_eq!(reconciliation.inserted_receipt_count, 0);
        assert_eq!(reconciliation.affected_address_ids, vec![record.address_id]);

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let states: Vec<(String, String)> = {
            let mut statement = conn
                .prepare(
                    "SELECT receipt_uid, receipt_state FROM address_receipts_recent ORDER BY mined_height ASC",
                )
                .unwrap();
            let rows = statement
                .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
                .unwrap();
            rows.map(|row| row.unwrap()).collect()
        };
        assert_eq!(
            states,
            vec![
                ("tx-reorg-before-tip:sapling:0".into(), "active".into()),
                ("tx-reorg-above-tip:sapling:0".into(), "revoked".into()),
            ]
        );

        let total: i64 = conn
            .query_row(
                "SELECT current_total_zat FROM address_totals WHERE address_id = ?1",
                rusqlite::params![record.address_id],
                |row| row.get(0),
            )
            .unwrap();
        assert_eq!(total, 5);
    }

    #[test]
    fn deep_rewind_cleanup_allows_above_tip_receipts_to_reactivate_when_the_chain_recovers() {
        let temp = tempdir().unwrap();
        let db = AppDb::open(temp.path().join("app.db")).unwrap();
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address: "uaddr1reorg-deep-recover",
                address_source: "fresh",
                diversifier_index_be: None,
                diversifier_bytes: None,
                request_label: None,
                request_memo: None,
                request_message: None,
                requested_amount: None,
            })
            .unwrap();
        db.insert_address_receivers(
            record.address_id,
            &[NewAddressReceiver {
                pool: "sapling",
                receiver_encoding: "sapling-deep-recover",
                receiver_fingerprint: &[5, 5, 5, 5, 5, 5, 5, 5],
            }],
        )
        .unwrap();
        db.apply_attributed_receipts(
            &[mined_receipt(
                record.address_id,
                "tx-reorg-returns",
                "sapling",
                "tx-reorg-returns:sapling:0",
                9,
                905,
                10,
                true,
            )],
            910,
            100,
        )
        .unwrap();

        db.revoke_mined_receipts_above_tip(900, 100).unwrap();

        let reconciliation = db
            .reconcile_attributed_receipts_in_range(
                &[mined_receipt(
                    record.address_id,
                    "tx-reorg-returns",
                    "sapling",
                    "tx-reorg-returns:sapling:0",
                    9,
                    905,
                    1,
                    true,
                )],
                901,
                905,
                905,
                100,
            )
            .unwrap();

        assert_eq!(reconciliation.revoked_receipt_count, 0);
        assert_eq!(reconciliation.reactivated_receipt_count, 1);

        let conn = rusqlite::Connection::open(temp.path().join("app.db")).unwrap();
        let state: (String, i64) = conn
            .query_row(
                "SELECT receipt_state, state_version FROM address_receipts_recent WHERE receipt_uid = 'tx-reorg-returns:sapling:0'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(state.0, "active");
        assert_eq!(state.1, 2);
    }
}
