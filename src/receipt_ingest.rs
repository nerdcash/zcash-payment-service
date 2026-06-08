use crate::{
    db::{AppDb, AttributedScannedReceipt, ReceiptKind},
    error::AppError,
    scanner::IncomingReceipt,
};

pub fn attribute_scanned_receipts(
    app_db: &AppDb,
    receipts: &[IncomingReceipt],
    tip_height: u32,
    webhook_report_confirmations: u32,
    observed_at: &str,
) -> Result<Vec<AttributedScannedReceipt>, AppError> {
    let mut attributed = Vec::new();

    for receipt in receipts {
        let Some(address_id) = app_db.find_address_id_by_receiver_fingerprint(
            &receipt.pool,
            &receipt.receiver_fingerprint,
        )?
        else {
            continue;
        };

        let (receipt_kind, confirmation_depth, eligible_for_webhook) = if receipt.is_mempool {
            (ReceiptKind::Mempool, 0, webhook_report_confirmations == 0)
        } else {
            let confirmation_depth = if tip_height >= receipt.mined_height {
                tip_height - receipt.mined_height + 1
            } else {
                0
            };
            (
                ReceiptKind::Mined,
                confirmation_depth,
                confirmation_depth >= webhook_report_confirmations,
            )
        };

        attributed.push(AttributedScannedReceipt {
            address_id,
            txid_hex: receipt.txid_hex.clone(),
            pool: receipt.pool.clone(),
            receipt_uid: format!(
                "{}:{}:{}",
                receipt.txid_hex, receipt.pool, receipt.output_index
            ),
            value_zat: i64::try_from(receipt.value_zat)
                .map_err(|_| AppError::Wallet("receipt value overflowed i64".into()))?,
            mined_height: i64::from(receipt.mined_height),
            confirmation_depth: i64::from(confirmation_depth),
            eligible_for_webhook,
            receipt_kind,
            first_observed_at: observed_at.to_string(),
            last_observed_at: observed_at.to_string(),
        });
    }

    Ok(attributed)
}

pub fn persist_scanned_receipts(
    app_db: &AppDb,
    receipts: &[IncomingReceipt],
    tip_height: u32,
    webhook_report_confirmations: u32,
    finality_confirmations: u32,
) -> Result<Vec<i64>, AppError> {
    let observed_at =
        time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?;
    let attributed = attribute_scanned_receipts(
        app_db,
        receipts,
        tip_height,
        webhook_report_confirmations,
        &observed_at,
    )?;

    app_db.apply_attributed_receipts(&attributed, i64::from(tip_height), finality_confirmations)
}

#[cfg(test)]
mod tests {
    use tempfile::tempdir;

    use super::persist_scanned_receipts;
    use crate::{
        config::Config,
        db::{AppDb, NewAddressReceiver, NewIssuedAddress},
        scanner::IncomingReceipt,
    };

    fn test_config(temp: &std::path::Path) -> Config {
        Config {
            listen_addr: "127.0.0.1:0".into(),
            network: "testnet".into(),
            startup_uivk: Some("uivk-one".into()),
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
            webhook_report_confirmations: 2,
            finality_confirmations: 100,
        }
    }

    #[test]
    fn persists_only_receipts_for_known_address_receivers() {
        let temp = tempdir().unwrap();
        let app_db = AppDb::open(temp.path().join("app.db")).unwrap();
        app_db
            .ensure_service_metadata(&test_config(temp.path()))
            .unwrap();

        let record = app_db
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
        app_db
            .insert_address_receivers(
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

        let affected = persist_scanned_receipts(
            &app_db,
            &[
                IncomingReceipt {
                    txid_hex: "tx-known".into(),
                    mined_height: 100,
                    is_mempool: false,
                    pool: "sapling".into(),
                    output_index: 0,
                    value_zat: 50,
                    receiver_fingerprint: vec![1, 2, 3, 4, 5, 6, 7, 8],
                },
                IncomingReceipt {
                    txid_hex: "tx-unknown".into(),
                    mined_height: 100,
                    is_mempool: false,
                    pool: "sapling".into(),
                    output_index: 1,
                    value_zat: 75,
                    receiver_fingerprint: vec![9, 9, 9, 9, 9, 9, 9, 9],
                },
            ],
            101,
            2,
            100,
        )
        .unwrap();

        assert_eq!(affected, vec![record.address_id]);
    }
}
