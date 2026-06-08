use crate::{
    config::Config,
    db::{AppDb, ReceiptRangeReconciliation, ServiceMetadata, WalletIdentity},
    error::AppError,
    receipt_ingest::attribute_scanned_receipts,
    scanner::{
        fetch_compact_blocks_for_height_range, fetch_current_mempool_txids,
        fetch_raw_transactions_by_txids, latest_block_height, scan_compact_blocks_for_wallet,
        scan_incoming_mempool_receipts_from_raw_transactions, wait_for_block_mempool_stream,
    },
    wallet_db::initialize_wallet_db,
    zcash::scanning_keys_from_uivk,
};

use std::{future::Future, sync::Arc, time::Instant};
use tokio::{
    sync::mpsc,
    task::{self, JoinError},
};

const STATE_SYNCED: &str = "synced";
const STATE_CATCHING_UP: &str = "catching_up";
const STATE_BLOCKED_SCANNING_KEY_MATERIAL: &str = "blocked_scanning_key_material";
const STATE_BLOCKED_LIGHTWALLETD_ENDPOINT: &str = "blocked_lightwalletd_endpoint";
const DOWNLOAD_PIPELINE_DEPTH: usize = 2;
const MIN_CATCH_UP_PROGRESS_LOG_INTERVAL_BLOCKS: u32 = 1_000;

#[derive(Debug, Clone, PartialEq, Eq)]
enum SyncWakeStrategy {
    PollFallback,
    WaitForNextBlock {
        lightwalletd_url: String,
        expected_tip_height: u32,
    },
}

pub fn spawn_wallet_sync_worker(config: Config) {
    tokio::spawn(async move {
        if let Err(error) = sync_loop(&config).await {
            tracing::error!(error = %error, "wallet sync worker exited with error");
        }
    });
}

async fn sync_loop(config: &Config) -> Result<(), AppError> {
    loop {
        run_single_sync_loop_iteration_with(
            || sync_once(config),
            |wake_strategy| async move { wait_for_next_sync_trigger(config, &wake_strategy).await },
            || sleep_for_poll_interval(config),
        )
        .await;
    }
}

async fn run_single_sync_loop_iteration_with<
    SyncOnce,
    SyncOnceFuture,
    WaitForNextSyncTrigger,
    WaitForNextSyncTriggerFuture,
    SleepForPollInterval,
    SleepForPollIntervalFuture,
>(
    sync_once_fn: SyncOnce,
    wait_for_next_sync_trigger_fn: WaitForNextSyncTrigger,
    sleep_for_poll_interval_fn: SleepForPollInterval,
) where
    SyncOnce: FnOnce() -> SyncOnceFuture,
    SyncOnceFuture: Future<Output = Result<SyncWakeStrategy, AppError>>,
    WaitForNextSyncTrigger: FnOnce(SyncWakeStrategy) -> WaitForNextSyncTriggerFuture,
    WaitForNextSyncTriggerFuture: Future<Output = Result<(), AppError>>,
    SleepForPollInterval: FnOnce() -> SleepForPollIntervalFuture,
    SleepForPollIntervalFuture: Future<Output = ()>,
{
    let wake_strategy = match sync_once_fn().await {
        Ok(strategy) => strategy,
        Err(error) => {
            tracing::error!(error = %error, "wallet sync iteration failed");
            SyncWakeStrategy::PollFallback
        }
    };

    if let Err(error) = wait_for_next_sync_trigger_fn(wake_strategy).await {
        tracing::warn!(error = %error, "wallet sync wakeup wait failed; falling back to polling delay");
        sleep_for_poll_interval_fn().await;
    }
}

async fn sync_once(config: &Config) -> Result<SyncWakeStrategy, AppError> {
    initialize_wallet_db(config)?;
    let app_db = AppDb::open(&config.app_db_path)?;
    let wallet_identity = app_db.ensure_wallet_identity(config)?;
    let service_metadata = app_db.ensure_service_metadata(config)?;
    let (state, reason) = if let Some(startup_uivk) = config.startup_uivk.as_deref() {
        let scanning_keys = scanning_keys_from_uivk(&config.network, startup_uivk)?;
        tracing::info!(
            sapling_key_count = scanning_keys.sapling().len(),
            orchard_key_count = scanning_keys.orchard().len(),
            "constructed scanning keys from startup UIVK"
        );
        if let Some(lightwalletd_url) = config.lightwalletd_url.as_deref() {
            return sync_with_lightwalletd(
                config,
                &app_db,
                &wallet_identity,
                &service_metadata,
                startup_uivk,
                lightwalletd_url,
            )
            .await;
        }

        (
            STATE_BLOCKED_LIGHTWALLETD_ENDPOINT,
            "scanner keys are ready, but LIGHTWALLETD_URL is missing so no chain scan can run",
        )
    } else {
        (
            STATE_BLOCKED_SCANNING_KEY_MATERIAL,
            "startup scanning key material is missing; provide ZCASH_UIVK so the service can construct scanner keys",
        )
    };

    let metadata = app_db.record_sync_status(state, reason, 0, 0)?;
    tracing::info!(
        sync_state = %metadata.current_sync_state,
        scan_epoch = metadata.current_scan_epoch,
        reason,
        "wallet sync status updated"
    );
    Ok(SyncWakeStrategy::PollFallback)
}

async fn sync_with_lightwalletd(
    config: &Config,
    app_db: &AppDb,
    wallet_identity: &WalletIdentity,
    service_metadata: &ServiceMetadata,
    startup_uivk: &str,
    lightwalletd_url: &str,
) -> Result<SyncWakeStrategy, AppError> {
    sync_with_lightwalletd_with(
        config,
        app_db,
        wallet_identity,
        service_metadata,
        lightwalletd_url,
        |lightwalletd_url| async move { latest_block_height(&lightwalletd_url).await },
        |tip_height, overlap_batch| async move {
            reconcile_overlap_batch(
                config,
                app_db,
                startup_uivk,
                lightwalletd_url,
                tip_height,
                overlap_batch,
            )
            .await
        },
        |tip_height, first_batch, final_height, batch_size| async move {
            scan_batches_with_pipeline(
                config,
                app_db,
                startup_uivk,
                lightwalletd_url,
                tip_height,
                first_batch,
                final_height,
                batch_size,
            )
            .await
        },
        |tip_height| async move {
            reconcile_current_mempool(config, app_db, startup_uivk, lightwalletd_url, tip_height)
                .await
        },
    )
    .await
}

#[allow(clippy::too_many_arguments)]
async fn sync_with_lightwalletd_with<
    LatestBlockHeight,
    LatestBlockHeightFuture,
    ReconcileOverlapBatchFn,
    ReconcileOverlapBatchFuture,
    ScanBatchesWithPipelineFn,
    ScanBatchesWithPipelineFuture,
    ReconcileCurrentMempoolFn,
    ReconcileCurrentMempoolFuture,
>(
    config: &Config,
    app_db: &AppDb,
    wallet_identity: &WalletIdentity,
    service_metadata: &ServiceMetadata,
    lightwalletd_url: &str,
    latest_block_height_fn: LatestBlockHeight,
    reconcile_overlap_batch_fn: ReconcileOverlapBatchFn,
    scan_batches_with_pipeline_fn: ScanBatchesWithPipelineFn,
    reconcile_current_mempool_fn: ReconcileCurrentMempoolFn,
) -> Result<SyncWakeStrategy, AppError>
where
    LatestBlockHeight: FnOnce(String) -> LatestBlockHeightFuture,
    LatestBlockHeightFuture: Future<Output = Result<u32, AppError>>,
    ReconcileOverlapBatchFn: FnOnce(u32, ScanBatch) -> ReconcileOverlapBatchFuture,
    ReconcileOverlapBatchFuture: Future<Output = Result<ReceiptRangeReconciliation, AppError>>,
    ScanBatchesWithPipelineFn: FnOnce(u32, ScanBatch, u32, u32) -> ScanBatchesWithPipelineFuture,
    ScanBatchesWithPipelineFuture: Future<Output = Result<(Vec<i64>, i64), AppError>>,
    ReconcileCurrentMempoolFn: FnOnce(u32) -> ReconcileCurrentMempoolFuture,
    ReconcileCurrentMempoolFuture: Future<Output = Result<Vec<i64>, AppError>>,
{
    let tip_height = latest_block_height_fn(lightwalletd_url.to_string()).await?;
    let previous_tip_height = service_metadata.last_seen_tip_height;
    let decision = build_sync_scan_decision(
        wallet_identity.birthday_height,
        service_metadata.last_scanned_height,
        tip_height,
        config.catch_up_threshold_blocks,
        config.catch_up_batch_size,
        config.finality_confirmations,
    );

    let mut affected_address_ids = Vec::new();
    let mut reorg_affected_address_ids = std::collections::BTreeSet::new();
    let mut reorg_inserted_receipt_count = 0usize;
    let mut reorg_revoked_receipt_count = 0usize;
    let mut reorg_reactivated_receipt_count = 0usize;
    let mut reorg_rewind_height = None;

    if service_metadata.last_scanned_height > i64::from(tip_height) {
        let cleanup = app_db.revoke_mined_receipts_above_tip(
            i64::from(tip_height),
            config.finality_confirmations,
        )?;
        reorg_affected_address_ids.extend(cleanup.affected_address_ids);
        reorg_inserted_receipt_count += cleanup.inserted_receipt_count;
        reorg_revoked_receipt_count += cleanup.revoked_receipt_count;
        reorg_reactivated_receipt_count += cleanup.reactivated_receipt_count;
        reorg_rewind_height = Some(tip_height.saturating_add(1));
    }

    if let Some(overlap_batch) = decision.overlap_batch {
        let reorg_outcome = reconcile_overlap_batch_fn(tip_height, overlap_batch).await?;
        reorg_affected_address_ids.extend(reorg_outcome.affected_address_ids.iter().copied());
        reorg_inserted_receipt_count += reorg_outcome.inserted_receipt_count;
        reorg_revoked_receipt_count += reorg_outcome.revoked_receipt_count;
        reorg_reactivated_receipt_count += reorg_outcome.reactivated_receipt_count;
        reorg_rewind_height = Some(
            reorg_rewind_height
                .map(|height: u32| height.min(overlap_batch.start_height))
                .unwrap_or(overlap_batch.start_height),
        );
        affected_address_ids.extend(reorg_outcome.affected_address_ids);
    }

    if previous_tip_height > i64::from(tip_height)
        || reorg_revoked_receipt_count > 0
        || reorg_reactivated_receipt_count > 0
    {
        let reorg_affected_address_ids = reorg_affected_address_ids.into_iter().collect::<Vec<_>>();
        let queued =
            app_db.queue_webhook_deliveries_for_dirty_addresses(&reorg_affected_address_ids)?;
        let rewind_height = reorg_rewind_height.unwrap_or(tip_height);
        let notes = format!(
            "reconciled reorg from height {} through tip {} with {} revoked, {} reactivated, {} inserted receipts; queued {} webhook deliveries",
            rewind_height,
            tip_height,
            reorg_revoked_receipt_count,
            reorg_reactivated_receipt_count,
            reorg_inserted_receipt_count,
            queued.len(),
        );
        app_db.record_reorg_event(
            previous_tip_height,
            i64::from(tip_height),
            i64::from(rewind_height),
            reorg_affected_address_ids.len(),
            &notes,
        )?;
    }

    if decision.plan.should_mark_catching_up {
        let first_batch = decision
            .plan
            .first_batch
            .expect("catch-up plan must have a first batch when marking catch-up");
        let final_height = decision
            .plan
            .final_height
            .expect("catch-up plan must have a final height when marking catch-up");
        let remaining_blocks = final_height
            .saturating_sub(decision.forward_start_height)
            .saturating_add(1);
        let reason = format!(
            "chain tip is at {tip_height}; starting pipelined catch-up with compact blocks {}..={} and continuing through {}",
            first_batch.start_height,
            first_batch.end_height,
            final_height
        );
        app_db.record_sync_status(
            STATE_CATCHING_UP,
            &reason,
            i64::from(tip_height),
            service_metadata.last_scanned_height,
        )?;
        tracing::info!(
            last_scanned_height = service_metadata.last_scanned_height,
            next_scan_height = decision.forward_start_height,
            tip_height,
            remaining_blocks,
            first_batch_start = first_batch.start_height,
            first_batch_end = first_batch.end_height,
            final_height,
            batch_size = decision.plan.batch_size,
            "starting wallet catch-up scan"
        );
    }

    let (forward_affected_address_ids, new_last_scanned_height) = if let Some(first_batch) =
        decision.plan.first_batch
    {
        scan_batches_with_pipeline_fn(
            tip_height,
            first_batch,
            decision
                .plan
                .final_height
                .expect("planned scan batches must end at a final height"),
            decision.plan.batch_size,
        )
        .await?
    } else {
        (
            Vec::new(),
            i64::from(
                tip_height
                    .min(u32::try_from(service_metadata.last_scanned_height.max(0)).unwrap_or(0)),
            ),
        )
    };
    affected_address_ids.extend(forward_affected_address_ids);

    let matured_address_ids = app_db.advance_receipt_maturity(
        i64::from(tip_height),
        config.webhook_report_confirmations,
        config.finality_confirmations,
    )?;
    let matured_queued =
        app_db.queue_webhook_deliveries_for_dirty_addresses(&matured_address_ids)?;
    affected_address_ids.extend(matured_address_ids);

    if config.webhook_report_confirmations == 0 {
        let mempool_affected_address_ids = reconcile_current_mempool_fn(tip_height).await?;
        affected_address_ids.extend(mempool_affected_address_ids);
    }

    affected_address_ids.sort_unstable();
    affected_address_ids.dedup();

    let reason = if affected_address_ids.is_empty() {
        format!(
            "wallet scan is caught up through block {new_last_scanned_height} at tip {tip_height}; no new issued-address receipts were observed"
        )
    } else {
        format!(
            "wallet scan is caught up through block {new_last_scanned_height} at tip {tip_height}; updated {} issued addresses from observed or matured receipts and queued {} maturity-driven webhook deliveries",
            affected_address_ids.len(),
            matured_queued.len(),
        )
    };
    let reached_tip = new_last_scanned_height >= i64::from(tip_height);
    let metadata = app_db.record_sync_status(
        if reached_tip {
            STATE_SYNCED
        } else {
            STATE_CATCHING_UP
        },
        &reason,
        i64::from(tip_height),
        new_last_scanned_height,
    )?;
    tracing::info!(
        sync_state = %metadata.current_sync_state,
        scan_epoch = metadata.current_scan_epoch,
        tip_height,
        last_scanned_height = metadata.last_scanned_height,
        affected_address_count = affected_address_ids.len(),
        "wallet sync status updated after bounded lightwalletd scan"
    );
    if decision.plan.first_batch.is_some() && metadata.last_scanned_height >= i64::from(tip_height)
    {
        tracing::info!(
            tip_height,
            last_scanned_height = metadata.last_scanned_height,
            affected_address_count = affected_address_ids.len(),
            "wallet catch-up scan reached the chain tip"
        );
    }
    Ok(SyncWakeStrategy::WaitForNextBlock {
        lightwalletd_url: lightwalletd_url.to_string(),
        expected_tip_height: tip_height,
    })
}

async fn wait_for_next_sync_trigger(
    config: &Config,
    wake_strategy: &SyncWakeStrategy,
) -> Result<(), AppError> {
    wait_for_next_sync_trigger_with(
        config,
        wake_strategy,
        |sync_poll_interval_seconds| async move {
            tokio::time::sleep(std::time::Duration::from_secs(sync_poll_interval_seconds)).await;
            Ok(())
        },
        |lightwalletd_url, expected_tip_height| async move {
            tracing::debug!(
                expected_tip_height,
                "waiting on lightwalletd mempool stream for the next mined block"
            );
            wait_for_block_mempool_stream(&lightwalletd_url, expected_tip_height).await
        },
        |lightwalletd_url, expected_tip_height| async move {
            let startup_uivk = config.startup_uivk.as_deref().ok_or_else(|| {
                AppError::InvalidConfig(
                    "ZCASH_UIVK is required for mempool receipt reconciliation".into(),
                )
            })?;
            let app_db = AppDb::open(&config.app_db_path)?;
            wait_for_next_block_with_periodic_mempool_reconciliation(
                config.sync_poll_interval_seconds,
                || wait_for_block_mempool_stream(&lightwalletd_url, expected_tip_height),
                || async {
                    reconcile_current_mempool(
                        config,
                        &app_db,
                        startup_uivk,
                        &lightwalletd_url,
                        expected_tip_height,
                    )
                    .await?;
                    Ok(())
                },
            )
            .await
        },
    )
    .await
}

async fn wait_for_next_sync_trigger_with<
    SleepForPollInterval,
    SleepForPollIntervalFuture,
    WaitForBlock,
    WaitForBlockFuture,
    WaitWithPeriodicReconciliation,
    WaitWithPeriodicReconciliationFuture,
>(
    config: &Config,
    wake_strategy: &SyncWakeStrategy,
    sleep_for_poll_interval: SleepForPollInterval,
    wait_for_block: WaitForBlock,
    wait_with_periodic_reconciliation: WaitWithPeriodicReconciliation,
) -> Result<(), AppError>
where
    SleepForPollInterval: FnOnce(u64) -> SleepForPollIntervalFuture,
    SleepForPollIntervalFuture: Future<Output = Result<(), AppError>>,
    WaitForBlock: FnOnce(String, u32) -> WaitForBlockFuture,
    WaitForBlockFuture: Future<Output = Result<(), AppError>>,
    WaitWithPeriodicReconciliation: FnOnce(String, u32) -> WaitWithPeriodicReconciliationFuture,
    WaitWithPeriodicReconciliationFuture: Future<Output = Result<(), AppError>>,
{
    match wake_strategy {
        SyncWakeStrategy::PollFallback => {
            sleep_for_poll_interval(config.sync_poll_interval_seconds).await
        }
        SyncWakeStrategy::WaitForNextBlock {
            lightwalletd_url,
            expected_tip_height,
        } => {
            if config.webhook_report_confirmations != 0 {
                return wait_for_block(lightwalletd_url.clone(), *expected_tip_height).await;
            }

            if config.startup_uivk.is_none() {
                return Err(AppError::InvalidConfig(
                    "ZCASH_UIVK is required for mempool receipt reconciliation".into(),
                ));
            }

            wait_with_periodic_reconciliation(lightwalletd_url.clone(), *expected_tip_height).await
        }
    }
}

async fn wait_for_next_block_with_periodic_mempool_reconciliation<
    WaitForBlock,
    WaitForBlockFuture,
    Reconcile,
    ReconcileFuture,
>(
    sync_poll_interval_seconds: u64,
    wait_for_block: WaitForBlock,
    mut reconcile: Reconcile,
) -> Result<(), AppError>
where
    WaitForBlock: FnOnce() -> WaitForBlockFuture,
    WaitForBlockFuture: Future<Output = Result<(), AppError>>,
    Reconcile: FnMut() -> ReconcileFuture,
    ReconcileFuture: Future<Output = Result<(), AppError>>,
{
    let wait_for_block = wait_for_block();
    tokio::pin!(wait_for_block);
    let mut interval =
        tokio::time::interval(std::time::Duration::from_secs(sync_poll_interval_seconds));
    interval.tick().await;

    loop {
        tokio::select! {
            result = &mut wait_for_block => return result,
            _ = interval.tick() => {
                reconcile().await?;
            }
        }
    }
}

async fn sleep_for_poll_interval(config: &Config) {
    tokio::time::sleep(std::time::Duration::from_secs(
        config.sync_poll_interval_seconds,
    ))
    .await;
}

/// Converts a raw 32-byte txid in wire byte order to display hex (byte-reversed),
/// matching the format used by block explorers and `TxId::to_string()`.
fn wire_txid_to_display_hex(raw: &[u8]) -> String {
    hex::encode(raw.iter().rev().copied().collect::<Vec<u8>>())
}

async fn reconcile_current_mempool(
    config: &Config,
    app_db: &AppDb,
    startup_uivk: &str,
    lightwalletd_url: &str,
    tip_height: u32,
) -> Result<Vec<i64>, AppError> {
    let mempool_txids = fetch_current_mempool_txids(lightwalletd_url).await?;
    let current_mempool_txids = mempool_txids
        .iter()
        .map(|raw| wire_txid_to_display_hex(raw))
        .collect::<std::collections::BTreeSet<_>>();
    let known_active_txids = app_db.active_mempool_txids()?;
    let txids_to_fetch = mempool_txids
        .into_iter()
        .filter(|txid| !known_active_txids.contains(&wire_txid_to_display_hex(txid)))
        .collect::<Vec<_>>();
    let raw_transactions =
        fetch_raw_transactions_by_txids(lightwalletd_url, &txids_to_fetch).await?;
    let receipts = scan_incoming_mempool_receipts_from_raw_transactions(
        &config.network,
        startup_uivk,
        tip_height,
        raw_transactions,
    )?;
    let observed_at =
        time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?;
    let attributed = attribute_scanned_receipts(
        app_db,
        &receipts,
        tip_height,
        config.webhook_report_confirmations,
        &observed_at,
    )?;
    let reconciliation = app_db.reconcile_mempool_snapshot(
        &current_mempool_txids,
        &attributed,
        &observed_at,
        i64::from(tip_height),
        config.finality_confirmations,
        config.webhook_report_confirmations,
    )?;
    let queued = app_db
        .queue_webhook_deliveries_for_dirty_addresses(&reconciliation.affected_address_ids)?;
    if !reconciliation.affected_address_ids.is_empty() {
        tracing::info!(
            affected_address_count = reconciliation.affected_address_ids.len(),
            inserted_receipt_count = reconciliation.inserted_receipt_count,
            revoked_receipt_count = reconciliation.revoked_receipt_count,
            reactivated_receipt_count = reconciliation.reactivated_receipt_count,
            queued_webhook_count = queued.len(),
            "reconciled mempool receipts for issued addresses"
        );
    }
    Ok(reconciliation.affected_address_ids)
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScanRangePlan {
    first_batch: Option<ScanBatch>,
    final_height: Option<u32>,
    batch_size: u32,
    should_mark_catching_up: bool,
    is_caught_up: bool,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct ScanBatch {
    start_height: u32,
    end_height: u32,
}

#[derive(Debug)]
struct DownloadedBatch {
    batch: ScanBatch,
    blocks: Vec<zcash_client_backend::proto::compact_formats::CompactBlock>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct SyncScanDecision {
    overlap_batch: Option<ScanBatch>,
    forward_start_height: u32,
    plan: ScanRangePlan,
}

fn plan_scan_range(
    next_height: u32,
    tip_height: u32,
    catch_up_threshold_blocks: u32,
    catch_up_batch_size: u32,
) -> ScanRangePlan {
    if next_height > tip_height {
        return ScanRangePlan {
            first_batch: None,
            final_height: None,
            batch_size: catch_up_batch_size,
            should_mark_catching_up: false,
            is_caught_up: true,
        };
    }

    let lag_blocks = tip_height - next_height + 1;
    let first_batch = next_scan_batch(next_height, tip_height, catch_up_batch_size)
        .expect("there must be a first batch when next height is at or below the tip");
    ScanRangePlan {
        first_batch: Some(first_batch),
        final_height: Some(tip_height),
        batch_size: catch_up_batch_size,
        should_mark_catching_up: lag_blocks >= catch_up_threshold_blocks,
        is_caught_up: false,
    }
}

fn build_sync_scan_decision(
    birthday_height: u32,
    last_scanned_height: i64,
    tip_height: u32,
    catch_up_threshold_blocks: u32,
    catch_up_batch_size: u32,
    finality_confirmations: u32,
) -> SyncScanDecision {
    let overlap_batch = reorg_overlap_batch(
        birthday_height,
        last_scanned_height,
        tip_height,
        finality_confirmations,
    );
    let forward_start_height =
        forward_scan_start_height(birthday_height, last_scanned_height, tip_height);
    let plan = plan_scan_range(
        forward_start_height,
        tip_height,
        catch_up_threshold_blocks,
        catch_up_batch_size,
    );

    SyncScanDecision {
        overlap_batch,
        forward_start_height,
        plan,
    }
}

fn forward_scan_start_height(
    birthday_height: u32,
    last_scanned_height: i64,
    tip_height: u32,
) -> u32 {
    if last_scanned_height <= 0 {
        birthday_height
    } else {
        u32::try_from(last_scanned_height)
            .ok()
            .map(|height| height.min(tip_height).saturating_add(1))
            .unwrap_or(tip_height.saturating_add(1))
            .max(birthday_height)
    }
}

fn reorg_overlap_batch(
    birthday_height: u32,
    last_scanned_height: i64,
    tip_height: u32,
    finality_confirmations: u32,
) -> Option<ScanBatch> {
    if last_scanned_height <= 0 {
        return None;
    }

    let overlap_end = u32::try_from(last_scanned_height).ok()?.min(tip_height);
    if overlap_end < birthday_height {
        return None;
    }

    let window = finality_confirmations.saturating_sub(1);
    Some(ScanBatch {
        start_height: overlap_end.saturating_sub(window).max(birthday_height),
        end_height: overlap_end,
    })
}

async fn reconcile_overlap_batch(
    config: &Config,
    app_db: &AppDb,
    startup_uivk: &str,
    lightwalletd_url: &str,
    tip_height: u32,
    overlap_batch: ScanBatch,
) -> Result<ReceiptRangeReconciliation, AppError> {
    let blocks = fetch_compact_blocks_for_height_range(
        lightwalletd_url,
        overlap_batch.start_height,
        overlap_batch.end_height,
    )
    .await?;
    let network = config.network.clone();
    let scanning_keys = scanning_keys_from_uivk(&config.network, startup_uivk)?;
    let scanned_batch = task::spawn_blocking(move || {
        scan_compact_blocks_for_wallet(&network, &scanning_keys, blocks)
    })
    .await
    .map_err(join_error_to_wallet_error)??;
    let observed_at =
        time::OffsetDateTime::now_utc().format(&time::format_description::well_known::Rfc3339)?;
    let attributed = attribute_scanned_receipts(
        app_db,
        &scanned_batch.receipts,
        tip_height,
        config.webhook_report_confirmations,
        &observed_at,
    )?;

    app_db.reconcile_attributed_receipts_in_range(
        &attributed,
        i64::from(overlap_batch.start_height),
        i64::from(overlap_batch.end_height),
        i64::from(tip_height),
        config.finality_confirmations,
    )
}

fn next_scan_batch(
    next_height: u32,
    final_height: u32,
    catch_up_batch_size: u32,
) -> Option<ScanBatch> {
    if next_height > final_height {
        return None;
    }

    let end_height = next_height
        .saturating_add(catch_up_batch_size.saturating_sub(1))
        .min(final_height);
    Some(ScanBatch {
        start_height: next_height,
        end_height,
    })
}

#[allow(clippy::too_many_arguments)]
async fn scan_batches_with_pipeline(
    config: &Config,
    app_db: &AppDb,
    startup_uivk: &str,
    lightwalletd_url: &str,
    tip_height: u32,
    first_batch: ScanBatch,
    final_height: u32,
    catch_up_batch_size: u32,
) -> Result<(Vec<i64>, i64), AppError> {
    let (sender, mut receiver) =
        mpsc::channel::<Result<DownloadedBatch, AppError>>(DOWNLOAD_PIPELINE_DEPTH);
    let lightwalletd_url = lightwalletd_url.to_string();
    let startup_uivk = startup_uivk.to_string();
    let download_lightwalletd_url = lightwalletd_url.clone();
    let downloader: tokio::task::JoinHandle<Result<(), AppError>> = tokio::spawn(async move {
        let mut next_batch = Some(first_batch);
        while let Some(batch) = next_batch {
            let result = fetch_compact_blocks_for_height_range(
                &download_lightwalletd_url,
                batch.start_height,
                batch.end_height,
            )
            .await
            .map(|blocks| DownloadedBatch { batch, blocks });

            if sender.send(result).await.is_err() {
                return Ok(());
            }

            let following_start = batch.end_height.saturating_add(1);
            next_batch = next_scan_batch(following_start, final_height, catch_up_batch_size);
        }
        Ok(())
    });

    let scanning_keys = Arc::new(scanning_keys_from_uivk(&config.network, &startup_uivk)?);
    let mut affected_address_ids = std::collections::BTreeSet::new();
    let mut last_scanned_height = 0i64;
    let total_blocks = final_height
        .saturating_sub(first_batch.start_height)
        .saturating_add(1);
    let progress_log_interval_blocks = catch_up_batch_size
        .saturating_mul(10)
        .max(MIN_CATCH_UP_PROGRESS_LOG_INTERVAL_BLOCKS);
    let mut next_progress_log_at = progress_log_interval_blocks.min(total_blocks.max(1));
    let started_at = Instant::now();
    while let Some(batch_result) = receiver.recv().await {
        let downloaded = batch_result?;
        let app_db = app_db.clone();
        let network = config.network.clone();
        let batch_start_height = downloaded.batch.start_height;
        let batch_end_height = downloaded.batch.end_height;
        let scanning_keys = Arc::clone(&scanning_keys);
        let scanned_batch = task::spawn_blocking(move || {
            scan_compact_blocks_for_wallet(&network, &*scanning_keys, downloaded.blocks)
        })
        .await
        .map_err(join_error_to_wallet_error)??;
        let observed_at = time::OffsetDateTime::now_utc()
            .format(&time::format_description::well_known::Rfc3339)?;

        let attributed = attribute_scanned_receipts(
            &app_db,
            &scanned_batch.receipts,
            tip_height,
            config.webhook_report_confirmations,
            &observed_at,
        )?;
        let reconciliation = app_db.reconcile_attributed_receipts_in_range(
            &attributed,
            i64::from(batch_start_height),
            i64::from(batch_end_height),
            i64::from(tip_height),
            config.finality_confirmations,
        )?;
        let queued = app_db
            .queue_webhook_deliveries_for_dirty_addresses(&reconciliation.affected_address_ids)?;
        affected_address_ids.extend(reconciliation.affected_address_ids);
        last_scanned_height = i64::from(batch_end_height);
        let completed_blocks = batch_end_height
            .saturating_sub(first_batch.start_height)
            .saturating_add(1)
            .min(total_blocks);
        let remaining_blocks = total_blocks.saturating_sub(completed_blocks);
        let percent_complete = if total_blocks == 0 {
            100.0
        } else {
            (f64::from(completed_blocks) / f64::from(total_blocks)) * 100.0
        };

        let reason = format!(
            "catch-up scan processed compact blocks {}..={} with bounded download/scan pipelining, {} inserted, {} revoked, {} reactivated receipts, and queued {} webhook deliveries",
            downloaded.batch.start_height,
            downloaded.batch.end_height,
            reconciliation.inserted_receipt_count,
            reconciliation.revoked_receipt_count,
            reconciliation.reactivated_receipt_count,
            queued.len(),
        );
        app_db.record_sync_status(
            STATE_CATCHING_UP,
            &reason,
            i64::from(tip_height),
            last_scanned_height,
        )?;
        if completed_blocks >= next_progress_log_at || completed_blocks == total_blocks {
            tracing::info!(
                batch_start_height,
                batch_end_height,
                completed_blocks,
                total_blocks,
                remaining_blocks,
                percent_complete,
                elapsed_seconds = started_at.elapsed().as_secs(),
                inserted_receipt_count = reconciliation.inserted_receipt_count,
                revoked_receipt_count = reconciliation.revoked_receipt_count,
                reactivated_receipt_count = reconciliation.reactivated_receipt_count,
                queued_webhook_count = queued.len(),
                "wallet catch-up scan progress"
            );
            next_progress_log_at = next_progress_log_at
                .saturating_add(progress_log_interval_blocks)
                .min(total_blocks);
        }
    }

    downloader.await.map_err(join_error_to_wallet_error)??;
    Ok((
        affected_address_ids.into_iter().collect(),
        last_scanned_height,
    ))
}

fn join_error_to_wallet_error(error: JoinError) -> AppError {
    AppError::Wallet(format!("wallet sync worker subtask failed: {error}"))
}

#[cfg(test)]
mod tests {
    use std::{
        fs,
        sync::{
            atomic::{AtomicUsize, Ordering},
            Arc,
        },
    };

    use tempfile::tempdir;
    use tokio::sync::oneshot;

    use super::{
        build_sync_scan_decision, forward_scan_start_height, next_scan_batch, plan_scan_range,
        reorg_overlap_batch, run_single_sync_loop_iteration_with, sync_once,
        sync_with_lightwalletd_with, wait_for_next_sync_trigger_with, ScanBatch, SyncWakeStrategy,
    };
    use crate::{
        config::Config,
        db::{AppDb, AttributedScannedReceipt, NewAddressReceiver, NewIssuedAddress, ReceiptKind},
        error::AppError,
    };

    const MAINNET_UIVK_NO_TRANSPARENT: &str = "uivk1020vq9j5zeqxh303sxa0zv2hn9wm9fev8x0p8yqxdwyzde9r4c90fcglc63usj0ycl2scy8zxuhtser0qrq356xfy8x3vyuxu7f6gas75svl9v9m3ctuazsu0ar8e8crtx7x6zgh4kw8xm3q4rlkpm9er2wefxhhf9pn547gpuz9vw27gsdp6c03nwlrxgzhr2g6xek0x8l5avrx9ue9lf032tr7kmhqf3nfdxg7ldfgx6yf09g";

    fn config(temp: &std::path::Path) -> Config {
        Config {
            listen_addr: "127.0.0.1:0".into(),
            network: "mainnet".into(),
            startup_uivk: Some(MAINNET_UIVK_NO_TRANSPARENT.into()),
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

    fn insert_test_address(db: &AppDb, unified_address: &str, receiver_encoding: &str) -> i64 {
        let record = db
            .insert_issued_address(NewIssuedAddress {
                unified_address,
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
                receiver_encoding,
                receiver_fingerprint: receiver_encoding.as_bytes(),
            }],
        )
        .unwrap();
        record.address_id
    }

    fn mined_receipt(
        address_id: i64,
        txid_hex: &str,
        receipt_uid: &str,
        value_zat: i64,
        mined_height: i64,
        confirmation_depth: i64,
    ) -> AttributedScannedReceipt {
        AttributedScannedReceipt {
            address_id,
            txid_hex: txid_hex.into(),
            pool: "sapling".into(),
            receipt_uid: receipt_uid.into(),
            value_zat,
            mined_height,
            confirmation_depth,
            eligible_for_webhook: true,
            receipt_kind: ReceiptKind::Mined,
            first_observed_at: "2026-01-01T00:00:00Z".into(),
            last_observed_at: "2026-01-01T00:00:00Z".into(),
        }
    }

    fn current_total(db_path: &std::path::Path, address_id: i64) -> i64 {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        conn.query_row(
            "SELECT current_total_zat FROM address_totals WHERE address_id = ?1",
            rusqlite::params![address_id],
            |row| row.get(0),
        )
        .unwrap()
    }

    fn receipt_states(db_path: &std::path::Path) -> Vec<(String, String)> {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        let mut statement = conn
            .prepare(
                "SELECT receipt_uid, receipt_state FROM address_receipts_recent ORDER BY mined_height ASC, receipt_uid ASC",
            )
            .unwrap();
        let rows = statement
            .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
            .unwrap();
        rows.map(|row| row.unwrap()).collect()
    }

    fn reorg_events(db_path: &std::path::Path) -> Vec<(i64, i64, i64, i64, String)> {
        let conn = rusqlite::Connection::open(db_path).unwrap();
        let mut statement = conn
            .prepare(
                "SELECT previous_tip_height, new_tip_height, rewind_height, affected_address_count, notes FROM reorg_events ORDER BY reorg_id ASC",
            )
            .unwrap();
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get(0)?,
                    row.get(1)?,
                    row.get(2)?,
                    row.get(3)?,
                    row.get(4)?,
                ))
            })
            .unwrap();
        rows.map(|row| row.unwrap()).collect()
    }

    #[tokio::test]
    async fn sync_once_marks_lightwalletd_endpoint_blocked_when_url_is_missing() {
        let temp = tempdir().unwrap();
        let config = config(temp.path());
        let app_db = AppDb::open(&config.app_db_path).unwrap();
        app_db.ensure_wallet_identity(&config).unwrap();
        app_db.ensure_service_metadata(&config).unwrap();

        let wake_strategy = sync_once(&config).await.unwrap();

        let metadata = app_db.current_service_metadata().unwrap();
        assert_eq!(metadata.current_sync_state, "blocked_lightwalletd_endpoint");
        assert_eq!(wake_strategy, SyncWakeStrategy::PollFallback);
    }

    #[tokio::test]
    async fn sync_once_reports_corrupted_app_db_file() {
        let temp = tempdir().unwrap();
        let config = config(temp.path());
        fs::write(&config.app_db_path, b"not-a-sqlite-database").unwrap();

        let error = sync_once(&config).await.unwrap_err();

        assert!(matches!(error, AppError::Database(_)));
    }

    #[tokio::test]
    async fn sync_once_reports_corrupted_wallet_db_file() {
        let temp = tempdir().unwrap();
        let config = config(temp.path());
        fs::write(&config.wallet_db_path, b"not-a-sqlite-database").unwrap();

        let error = sync_once(&config).await.unwrap_err();

        assert!(matches!(error, AppError::Database(_) | AppError::Wallet(_)));
    }

    #[tokio::test]
    async fn deterministic_lightwalletd_cold_start_catch_up_reaches_tip_and_marks_synced() {
        let temp = tempdir().unwrap();
        let mut config = config(temp.path());
        config.birthday_height = Some(100);
        config.catch_up_batch_size = 3;
        config.finality_confirmations = 3;
        let app_db = AppDb::open(&config.app_db_path).unwrap();
        let wallet_identity = app_db.ensure_wallet_identity(&config).unwrap();
        let service_metadata = app_db.ensure_service_metadata(&config).unwrap();
        let address_id = insert_test_address(
            &app_db,
            "uaddr1deterministic-cold-start",
            "sapling-cold-start",
        );

        let wake_strategy = sync_with_lightwalletd_with(
            &config,
            &app_db,
            &wallet_identity,
            &service_metadata,
            "https://example.invalid",
            |_| async { Ok(102) },
            |_tip_height, overlap_batch| async move {
                panic!("unexpected overlap batch on cold start: {overlap_batch:?}")
            },
            |tip_height, first_batch, final_height, batch_size| {
                let app_db = app_db.clone();
                async move {
                    assert_eq!(tip_height, 102);
                    assert_eq!(
                        first_batch,
                        ScanBatch {
                            start_height: 100,
                            end_height: 102
                        }
                    );
                    assert_eq!(final_height, 102);
                    assert_eq!(batch_size, 3);
                    let reconciliation = app_db.reconcile_attributed_receipts_in_range(
                        &[mined_receipt(
                            address_id,
                            "tx-cold-start-102",
                            "tx-cold-start-102:sapling:0",
                            25,
                            102,
                            1,
                        )],
                        100,
                        102,
                        102,
                        config.finality_confirmations,
                    )?;
                    app_db.queue_webhook_deliveries_for_dirty_addresses(
                        &reconciliation.affected_address_ids,
                    )?;
                    Ok((reconciliation.affected_address_ids, 102))
                }
            },
            |_tip_height| async { unreachable!("mempool reconciliation should be disabled") },
        )
        .await
        .unwrap();

        assert_eq!(
            wake_strategy,
            SyncWakeStrategy::WaitForNextBlock {
                lightwalletd_url: "https://example.invalid".into(),
                expected_tip_height: 102,
            }
        );
        let metadata = app_db.current_service_metadata().unwrap();
        assert_eq!(metadata.current_sync_state, "synced");
        assert_eq!(metadata.last_seen_tip_height, 102);
        assert_eq!(metadata.last_scanned_height, 102);
        assert_eq!(current_total(&config.app_db_path, address_id), 25);
    }

    #[tokio::test]
    async fn deterministic_lightwalletd_handles_pathological_rewind_and_recovery_flaps_end_to_end()
    {
        let temp = tempdir().unwrap();
        let mut config = config(temp.path());
        config.birthday_height = Some(100);
        config.catch_up_batch_size = 4;
        config.finality_confirmations = 3;
        let app_db = AppDb::open(&config.app_db_path).unwrap();
        let wallet_identity = app_db.ensure_wallet_identity(&config).unwrap();
        app_db.ensure_service_metadata(&config).unwrap();
        let address_id = insert_test_address(&app_db, "uaddr1deterministic-flap", "sapling-flap");
        app_db
            .apply_attributed_receipts(
                &[
                    mined_receipt(
                        address_id,
                        "tx-stable-104",
                        "tx-stable-104:sapling:0",
                        5,
                        104,
                        7,
                    ),
                    mined_receipt(
                        address_id,
                        "tx-reorg-109",
                        "tx-reorg-109:sapling:0",
                        7,
                        109,
                        2,
                    ),
                    mined_receipt(
                        address_id,
                        "tx-reorg-110",
                        "tx-reorg-110:sapling:0",
                        11,
                        110,
                        1,
                    ),
                ],
                110,
                config.finality_confirmations,
            )
            .unwrap();
        app_db
            .record_sync_status("synced", "seeded at tip 110", 110, 110)
            .unwrap();

        let service_metadata = app_db.current_service_metadata().unwrap();
        sync_with_lightwalletd_with(
            &config,
            &app_db,
            &wallet_identity,
            &service_metadata,
            "https://example.invalid",
            |_| async { Ok(106) },
            |tip_height, overlap_batch| {
                let app_db = app_db.clone();
                async move {
                    assert_eq!(tip_height, 106);
                    assert_eq!(overlap_batch, ScanBatch { start_height: 104, end_height: 106 });
                    app_db.reconcile_attributed_receipts_in_range(
                        &[mined_receipt(address_id, "tx-stable-104", "tx-stable-104:sapling:0", 5, 104, 3)],
                        104,
                        106,
                        106,
                        config.finality_confirmations,
                    )
                }
            },
            |_tip_height, first_batch, final_height, batch_size| async move {
                panic!("unexpected forward scan during rewind: {first_batch:?} {final_height} {batch_size}")
            },
            |_tip_height| async { unreachable!("mempool reconciliation should be disabled") },
        )
        .await
        .unwrap();

        let metadata_after_rewind = app_db.current_service_metadata().unwrap();
        assert_eq!(metadata_after_rewind.current_sync_state, "synced");
        assert_eq!(metadata_after_rewind.last_seen_tip_height, 106);
        assert_eq!(metadata_after_rewind.last_scanned_height, 106);
        assert_eq!(current_total(&config.app_db_path, address_id), 5);
        assert_eq!(
            receipt_states(&config.app_db_path),
            vec![
                ("tx-stable-104:sapling:0".into(), "active".into()),
                ("tx-reorg-109:sapling:0".into(), "revoked".into()),
                ("tx-reorg-110:sapling:0".into(), "revoked".into()),
            ]
        );

        let service_metadata = app_db.current_service_metadata().unwrap();
        sync_with_lightwalletd_with(
            &config,
            &app_db,
            &wallet_identity,
            &service_metadata,
            "https://example.invalid",
            |_| async { Ok(111) },
            |tip_height, overlap_batch| {
                let app_db = app_db.clone();
                async move {
                    assert_eq!(tip_height, 111);
                    assert_eq!(
                        overlap_batch,
                        ScanBatch {
                            start_height: 104,
                            end_height: 106
                        }
                    );
                    app_db.reconcile_attributed_receipts_in_range(
                        &[mined_receipt(
                            address_id,
                            "tx-stable-104",
                            "tx-stable-104:sapling:0",
                            5,
                            104,
                            8,
                        )],
                        104,
                        106,
                        111,
                        config.finality_confirmations,
                    )
                }
            },
            |tip_height, first_batch, final_height, batch_size| {
                let app_db = app_db.clone();
                async move {
                    assert_eq!(tip_height, 111);
                    assert_eq!(
                        first_batch,
                        ScanBatch {
                            start_height: 107,
                            end_height: 110
                        }
                    );
                    assert_eq!(final_height, 111);
                    assert_eq!(batch_size, 4);
                    let first = app_db.reconcile_attributed_receipts_in_range(
                        &[
                            mined_receipt(
                                address_id,
                                "tx-reorg-109",
                                "tx-reorg-109:sapling:0",
                                7,
                                109,
                                3,
                            ),
                            mined_receipt(
                                address_id,
                                "tx-reorg-110",
                                "tx-reorg-110:sapling:0",
                                11,
                                110,
                                2,
                            ),
                        ],
                        107,
                        110,
                        111,
                        config.finality_confirmations,
                    )?;
                    let second = app_db.reconcile_attributed_receipts_in_range(
                        &[mined_receipt(
                            address_id,
                            "tx-reorg-111",
                            "tx-reorg-111:sapling:0",
                            13,
                            111,
                            1,
                        )],
                        111,
                        111,
                        111,
                        config.finality_confirmations,
                    )?;
                    let mut affected = first.affected_address_ids;
                    affected.extend(second.affected_address_ids);
                    affected.sort_unstable();
                    affected.dedup();
                    Ok((affected, 111))
                }
            },
            |_tip_height| async { unreachable!("mempool reconciliation should be disabled") },
        )
        .await
        .unwrap();

        let metadata_after_recovery = app_db.current_service_metadata().unwrap();
        assert_eq!(metadata_after_recovery.current_sync_state, "synced");
        assert_eq!(metadata_after_recovery.last_seen_tip_height, 111);
        assert_eq!(metadata_after_recovery.last_scanned_height, 111);
        assert_eq!(current_total(&config.app_db_path, address_id), 36);

        let service_metadata = app_db.current_service_metadata().unwrap();
        sync_with_lightwalletd_with(
            &config,
            &app_db,
            &wallet_identity,
            &service_metadata,
            "https://example.invalid",
            |_| async { Ok(105) },
            |tip_height, overlap_batch| {
                let app_db = app_db.clone();
                async move {
                    assert_eq!(tip_height, 105);
                    assert_eq!(overlap_batch, ScanBatch { start_height: 103, end_height: 105 });
                    app_db.reconcile_attributed_receipts_in_range(
                        &[mined_receipt(address_id, "tx-stable-104", "tx-stable-104:sapling:0", 5, 104, 2)],
                        103,
                        105,
                        105,
                        config.finality_confirmations,
                    )
                }
            },
            |_tip_height, first_batch, final_height, batch_size| async move {
                panic!("unexpected forward scan during second rewind: {first_batch:?} {final_height} {batch_size}")
            },
            |_tip_height| async { unreachable!("mempool reconciliation should be disabled") },
        )
        .await
        .unwrap();

        assert_eq!(
            app_db
                .current_service_metadata()
                .unwrap()
                .last_scanned_height,
            105
        );
        assert_eq!(current_total(&config.app_db_path, address_id), 5);

        let service_metadata = app_db.current_service_metadata().unwrap();
        sync_with_lightwalletd_with(
            &config,
            &app_db,
            &wallet_identity,
            &service_metadata,
            "https://example.invalid",
            |_| async { Ok(112) },
            |tip_height, overlap_batch| {
                let app_db = app_db.clone();
                async move {
                    assert_eq!(tip_height, 112);
                    assert_eq!(
                        overlap_batch,
                        ScanBatch {
                            start_height: 103,
                            end_height: 105
                        }
                    );
                    app_db.reconcile_attributed_receipts_in_range(
                        &[mined_receipt(
                            address_id,
                            "tx-stable-104",
                            "tx-stable-104:sapling:0",
                            5,
                            104,
                            9,
                        )],
                        103,
                        105,
                        112,
                        config.finality_confirmations,
                    )
                }
            },
            |tip_height, first_batch, final_height, batch_size| {
                let app_db = app_db.clone();
                async move {
                    assert_eq!(tip_height, 112);
                    assert_eq!(
                        first_batch,
                        ScanBatch {
                            start_height: 106,
                            end_height: 109
                        }
                    );
                    assert_eq!(final_height, 112);
                    assert_eq!(batch_size, 4);
                    let first = app_db.reconcile_attributed_receipts_in_range(
                        &[mined_receipt(
                            address_id,
                            "tx-reorg-109",
                            "tx-reorg-109:sapling:0",
                            7,
                            109,
                            4,
                        )],
                        106,
                        109,
                        112,
                        config.finality_confirmations,
                    )?;
                    let second = app_db.reconcile_attributed_receipts_in_range(
                        &[
                            mined_receipt(
                                address_id,
                                "tx-reorg-110",
                                "tx-reorg-110:sapling:0",
                                11,
                                110,
                                3,
                            ),
                            mined_receipt(
                                address_id,
                                "tx-reorg-111",
                                "tx-reorg-111:sapling:0",
                                13,
                                111,
                                2,
                            ),
                            mined_receipt(
                                address_id,
                                "tx-reorg-112",
                                "tx-reorg-112:sapling:0",
                                17,
                                112,
                                1,
                            ),
                        ],
                        110,
                        112,
                        112,
                        config.finality_confirmations,
                    )?;
                    let mut affected = first.affected_address_ids;
                    affected.extend(second.affected_address_ids);
                    affected.sort_unstable();
                    affected.dedup();
                    Ok((affected, 112))
                }
            },
            |_tip_height| async { unreachable!("mempool reconciliation should be disabled") },
        )
        .await
        .unwrap();

        let final_metadata = app_db.current_service_metadata().unwrap();
        assert_eq!(final_metadata.current_sync_state, "synced");
        assert_eq!(final_metadata.last_seen_tip_height, 112);
        assert_eq!(final_metadata.last_scanned_height, 112);
        assert_eq!(current_total(&config.app_db_path, address_id), 53);
        assert_eq!(
            receipt_states(&config.app_db_path),
            vec![
                ("tx-stable-104:sapling:0".into(), "active".into()),
                ("tx-reorg-109:sapling:0".into(), "active".into()),
                ("tx-reorg-110:sapling:0".into(), "active".into()),
                ("tx-reorg-111:sapling:0".into(), "active".into()),
                ("tx-reorg-112:sapling:0".into(), "active".into()),
            ]
        );

        let events = reorg_events(&config.app_db_path);
        assert_eq!(events.len(), 2);
        assert_eq!(events[0].0, 110);
        assert_eq!(events[0].1, 106);
        assert_eq!(events[0].2, 104);
        assert_eq!(events[0].3, 1);
        assert!(events[0].4.contains("2 revoked"));
        assert_eq!(events[1].0, 111);
        assert_eq!(events[1].1, 105);
        assert_eq!(events[1].2, 103);
        assert_eq!(events[1].3, 1);
        assert!(events[1].4.contains("3 revoked"));
    }

    #[tokio::test(start_paused = true)]
    async fn mempool_wait_loop_reconciles_periodically_while_waiting_for_next_block() {
        let reconcile_count = Arc::new(AtomicUsize::new(0));
        let reconcile_count_clone = Arc::clone(&reconcile_count);
        let (send_block, receive_block) = oneshot::channel::<()>();

        let waiter = tokio::spawn(async move {
            super::wait_for_next_block_with_periodic_mempool_reconciliation(
                5,
                || async move {
                    receive_block.await.unwrap();
                    Ok(())
                },
                move || {
                    let reconcile_count = Arc::clone(&reconcile_count_clone);
                    async move {
                        reconcile_count.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                },
            )
            .await
        });

        tokio::task::yield_now().await;
        tokio::time::advance(std::time::Duration::from_secs(5)).await;
        tokio::task::yield_now().await;

        assert_eq!(reconcile_count.load(Ordering::SeqCst), 1);

        send_block.send(()).unwrap();
        assert!(waiter.await.unwrap().is_ok());
    }

    #[tokio::test]
    async fn wait_for_next_sync_trigger_poll_fallback_uses_poll_sleep_only() {
        let temp = tempdir().unwrap();
        let config = config(temp.path());
        let slept = Arc::new(AtomicUsize::new(0));
        let waited_for_block = Arc::new(AtomicUsize::new(0));
        let waited_with_reconcile = Arc::new(AtomicUsize::new(0));

        wait_for_next_sync_trigger_with(
            &config,
            &SyncWakeStrategy::PollFallback,
            {
                let slept = Arc::clone(&slept);
                move |_| {
                    let slept = Arc::clone(&slept);
                    async move {
                        slept.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }
            },
            {
                let waited_for_block = Arc::clone(&waited_for_block);
                move |_, _| {
                    let waited_for_block = Arc::clone(&waited_for_block);
                    async move {
                        waited_for_block.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }
            },
            {
                let waited_with_reconcile = Arc::clone(&waited_with_reconcile);
                move |_, _| {
                    let waited_with_reconcile = Arc::clone(&waited_with_reconcile);
                    async move {
                        waited_with_reconcile.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(slept.load(Ordering::SeqCst), 1);
        assert_eq!(waited_for_block.load(Ordering::SeqCst), 0);
        assert_eq!(waited_with_reconcile.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn wait_for_next_sync_trigger_waits_on_block_stream_when_confirmations_are_nonzero() {
        let temp = tempdir().unwrap();
        let config = config(temp.path());
        let waited_for_block = Arc::new(AtomicUsize::new(0));
        let waited_with_reconcile = Arc::new(AtomicUsize::new(0));

        wait_for_next_sync_trigger_with(
            &config,
            &SyncWakeStrategy::WaitForNextBlock {
                lightwalletd_url: "https://example.invalid".into(),
                expected_tip_height: 321,
            },
            |_| async { Ok(()) },
            {
                let waited_for_block = Arc::clone(&waited_for_block);
                move |lightwalletd_url, expected_tip_height| {
                    let waited_for_block = Arc::clone(&waited_for_block);
                    async move {
                        assert_eq!(lightwalletd_url, "https://example.invalid");
                        assert_eq!(expected_tip_height, 321);
                        waited_for_block.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }
            },
            {
                let waited_with_reconcile = Arc::clone(&waited_with_reconcile);
                move |_, _| {
                    let waited_with_reconcile = Arc::clone(&waited_with_reconcile);
                    async move {
                        waited_with_reconcile.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(waited_for_block.load(Ordering::SeqCst), 1);
        assert_eq!(waited_with_reconcile.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn wait_for_next_sync_trigger_requires_uivk_for_zero_confirmation_mempool_mode() {
        let temp = tempdir().unwrap();
        let mut config = config(temp.path());
        config.webhook_report_confirmations = 0;
        config.startup_uivk = None;

        let error = wait_for_next_sync_trigger_with(
            &config,
            &SyncWakeStrategy::WaitForNextBlock {
                lightwalletd_url: "https://example.invalid".into(),
                expected_tip_height: 321,
            },
            |_| async { Ok(()) },
            |_, _| async { Ok(()) },
            |_, _| async { Ok(()) },
        )
        .await
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("ZCASH_UIVK is required for mempool receipt reconciliation"));
    }

    #[tokio::test]
    async fn wait_for_next_sync_trigger_uses_periodic_reconciliation_in_zero_confirmation_mode() {
        let temp = tempdir().unwrap();
        let mut config = config(temp.path());
        config.webhook_report_confirmations = 0;
        let waited_for_block = Arc::new(AtomicUsize::new(0));
        let waited_with_reconcile = Arc::new(AtomicUsize::new(0));

        wait_for_next_sync_trigger_with(
            &config,
            &SyncWakeStrategy::WaitForNextBlock {
                lightwalletd_url: "https://example.invalid".into(),
                expected_tip_height: 654,
            },
            |_| async { Ok(()) },
            {
                let waited_for_block = Arc::clone(&waited_for_block);
                move |_, _| {
                    let waited_for_block = Arc::clone(&waited_for_block);
                    async move {
                        waited_for_block.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }
            },
            {
                let waited_with_reconcile = Arc::clone(&waited_with_reconcile);
                move |lightwalletd_url, expected_tip_height| {
                    let waited_with_reconcile = Arc::clone(&waited_with_reconcile);
                    async move {
                        assert_eq!(lightwalletd_url, "https://example.invalid");
                        assert_eq!(expected_tip_height, 654);
                        waited_with_reconcile.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }
            },
        )
        .await
        .unwrap();

        assert_eq!(waited_for_block.load(Ordering::SeqCst), 0);
        assert_eq!(waited_with_reconcile.load(Ordering::SeqCst), 1);
    }

    #[tokio::test]
    async fn sync_loop_iteration_uses_poll_fallback_when_sync_once_fails() {
        let waited = Arc::new(AtomicUsize::new(0));
        let slept = Arc::new(AtomicUsize::new(0));

        run_single_sync_loop_iteration_with(
            || async { Err(AppError::Wallet("fake upstream flap".into())) },
            {
                let waited = Arc::clone(&waited);
                move |wake_strategy| {
                    let waited = Arc::clone(&waited);
                    let wake_strategy = wake_strategy.clone();
                    async move {
                        assert_eq!(wake_strategy, SyncWakeStrategy::PollFallback);
                        waited.fetch_add(1, Ordering::SeqCst);
                        Ok(())
                    }
                }
            },
            {
                let slept = Arc::clone(&slept);
                move || {
                    let slept = Arc::clone(&slept);
                    async move {
                        slept.fetch_add(1, Ordering::SeqCst);
                    }
                }
            },
        )
        .await;

        assert_eq!(waited.load(Ordering::SeqCst), 1);
        assert_eq!(slept.load(Ordering::SeqCst), 0);
    }

    #[tokio::test]
    async fn sync_loop_iteration_sleeps_after_wakeup_wait_failure() {
        let waited = Arc::new(AtomicUsize::new(0));
        let slept = Arc::new(AtomicUsize::new(0));

        run_single_sync_loop_iteration_with(
            || async {
                Ok(SyncWakeStrategy::WaitForNextBlock {
                    lightwalletd_url: "https://example.invalid".into(),
                    expected_tip_height: 456,
                })
            },
            {
                let waited = Arc::clone(&waited);
                move |wake_strategy| {
                    let waited = Arc::clone(&waited);
                    let wake_strategy = wake_strategy.clone();
                    async move {
                        assert_eq!(
                            wake_strategy,
                            SyncWakeStrategy::WaitForNextBlock {
                                lightwalletd_url: "https://example.invalid".into(),
                                expected_tip_height: 456,
                            }
                        );
                        waited.fetch_add(1, Ordering::SeqCst);
                        Err(AppError::Wallet("fake wakeup failure".into()))
                    }
                }
            },
            {
                let slept = Arc::clone(&slept);
                move || {
                    let slept = Arc::clone(&slept);
                    async move {
                        slept.fetch_add(1, Ordering::SeqCst);
                    }
                }
            },
        )
        .await;

        assert_eq!(waited.load(Ordering::SeqCst), 1);
        assert_eq!(slept.load(Ordering::SeqCst), 1);
    }

    #[test]
    fn plan_scan_range_starts_at_birthday_when_no_progress_is_recorded() {
        let plan = plan_scan_range(123, 130, 2, 3);

        assert_eq!(
            plan.first_batch,
            Some(ScanBatch {
                start_height: 123,
                end_height: 125
            })
        );
        assert_eq!(plan.final_height, Some(130));
        assert_eq!(plan.batch_size, 3);
        assert!(plan.should_mark_catching_up);
        assert!(!plan.is_caught_up);
    }

    #[test]
    fn plan_scan_range_resumes_after_last_scanned_height() {
        let plan = plan_scan_range(151, 155, 10, 2);

        assert_eq!(
            plan.first_batch,
            Some(ScanBatch {
                start_height: 151,
                end_height: 152
            })
        );
        assert_eq!(plan.final_height, Some(155));
        assert!(!plan.should_mark_catching_up);
        assert!(!plan.is_caught_up);
    }

    #[test]
    fn plan_scan_range_marks_caught_up_when_tip_is_already_scanned() {
        let plan = plan_scan_range(156, 155, 1, 10);

        assert_eq!(plan.first_batch, None);
        assert_eq!(plan.final_height, None);
        assert!(!plan.should_mark_catching_up);
        assert!(plan.is_caught_up);
    }

    #[test]
    fn forward_scan_start_height_uses_birthday_when_progress_is_missing_or_negative() {
        assert_eq!(forward_scan_start_height(123, 0, 200), 123);
        assert_eq!(forward_scan_start_height(123, -5, 200), 123);
    }

    #[test]
    fn forward_scan_start_height_clamps_rewound_or_ahead_of_tip_progress() {
        assert_eq!(forward_scan_start_height(123, 150, 140), 141);
        assert_eq!(forward_scan_start_height(123, i64::MAX, 140), 141);
    }

    #[test]
    fn forward_scan_start_height_never_starts_before_birthday() {
        assert_eq!(forward_scan_start_height(500, 100, 300), 500);
    }

    #[test]
    fn reorg_overlap_batch_returns_none_without_prior_progress_or_when_rewound_before_birthday() {
        assert_eq!(reorg_overlap_batch(123, 0, 200, 100), None);
        assert_eq!(reorg_overlap_batch(123, -1, 200, 100), None);
        assert_eq!(reorg_overlap_batch(123, 122, 122, 100), None);
    }

    #[test]
    fn reorg_overlap_batch_clamps_end_to_tip_when_chain_rewinds() {
        assert_eq!(
            reorg_overlap_batch(123, 200, 150, 10),
            Some(ScanBatch {
                start_height: 141,
                end_height: 150,
            })
        );
    }

    #[test]
    fn reorg_overlap_batch_respects_birthday_floor_and_single_block_window() {
        assert_eq!(
            reorg_overlap_batch(123, 125, 200, 10),
            Some(ScanBatch {
                start_height: 123,
                end_height: 125,
            })
        );
        assert_eq!(
            reorg_overlap_batch(123, 130, 200, 0),
            Some(ScanBatch {
                start_height: 130,
                end_height: 130,
            })
        );
    }

    #[test]
    fn sync_scan_decision_tracks_repeated_rewind_and_recovery_sequences() {
        let steady = build_sync_scan_decision(100, 110, 110, 3, 4, 5);
        assert_eq!(
            steady.overlap_batch,
            Some(ScanBatch {
                start_height: 106,
                end_height: 110,
            })
        );
        assert_eq!(steady.forward_start_height, 111);
        assert_eq!(steady.plan.first_batch, None);
        assert!(steady.plan.is_caught_up);

        let rewound = build_sync_scan_decision(100, 110, 108, 3, 4, 5);
        assert_eq!(
            rewound.overlap_batch,
            Some(ScanBatch {
                start_height: 104,
                end_height: 108,
            })
        );
        assert_eq!(rewound.forward_start_height, 109);
        assert_eq!(rewound.plan.first_batch, None);
        assert!(rewound.plan.is_caught_up);

        let recovered = build_sync_scan_decision(100, 108, 112, 3, 4, 5);
        assert_eq!(
            recovered.overlap_batch,
            Some(ScanBatch {
                start_height: 104,
                end_height: 108,
            })
        );
        assert_eq!(recovered.forward_start_height, 109);
        assert_eq!(
            recovered.plan.first_batch,
            Some(ScanBatch {
                start_height: 109,
                end_height: 112,
            })
        );
        assert_eq!(recovered.plan.final_height, Some(112));
        assert!(recovered.plan.should_mark_catching_up);
        assert!(!recovered.plan.is_caught_up);
    }

    #[test]
    fn next_scan_batch_splits_ranges_without_overlap() {
        let mut next_height = 123;
        let mut batches = Vec::new();

        while let Some(batch) = next_scan_batch(next_height, 130, 3) {
            batches.push(batch);
            next_height = batch.end_height + 1;
        }

        assert_eq!(
            batches,
            vec![
                ScanBatch {
                    start_height: 123,
                    end_height: 125
                },
                ScanBatch {
                    start_height: 126,
                    end_height: 128
                },
                ScanBatch {
                    start_height: 129,
                    end_height: 130
                },
            ]
        );
    }
}
