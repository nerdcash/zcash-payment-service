use std::{
    collections::{BTreeMap, BTreeSet, HashMap},
    future::Future,
    hash::Hash,
    sync::{Mutex, OnceLock},
    time::Duration,
};

use http::Uri;
use tokio::time::sleep;
use tonic::transport::{Channel, ClientTlsConfig, Endpoint};
use tonic::{Code, Status};

use zcash_client_backend::{
    data_api::ScannedBlock,
    proto::{
        compact_formats::CompactBlock,
        service::{
            compact_tx_streamer_client::CompactTxStreamerClient, BlockId, BlockRange, ChainSpec,
            Empty, GetMempoolTxRequest, RawTransaction, TreeState, TxFilter,
        },
    },
    scanning::{scan_block, Nullifiers, ScanningKeys},
    wallet::Note,
};
use zcash_keys::{address::Receiver, keys::UnifiedIncomingViewingKey};
use zcash_primitives::transaction::Transaction;
use zcash_protocol::consensus::{BlockHeight, BranchId};
use zip32::Scope;

use crate::{
    error::AppError,
    zcash::{consensus_network, scanning_keys_from_uivk},
};

const LIGHTWALLETD_RETRY_ATTEMPT_LIMIT: u32 = 3;
const LIGHTWALLETD_RETRY_DELAY: Duration = Duration::from_secs(2);

fn lightwalletd_channels() -> &'static Mutex<HashMap<String, Channel>> {
    static CHANNELS: OnceLock<Mutex<HashMap<String, Channel>>> = OnceLock::new();
    CHANNELS.get_or_init(|| Mutex::new(HashMap::new()))
}

fn parse_lightwalletd_uri(lightwalletd_url: &str) -> Result<Uri, AppError> {
    lightwalletd_url.parse::<Uri>().map_err(|error| {
        AppError::InvalidConfig(format!(
            "LIGHTWALLETD_URL must be a valid URI; got {lightwalletd_url}: {error}"
        ))
    })
}

fn remove_lightwalletd_channel(lightwalletd_url: &str) {
    if let Ok(mut channels) = lightwalletd_channels().lock() {
        channels.remove(lightwalletd_url);
    }
}

async fn get_lightwalletd_channel(lightwalletd_url: &str) -> Result<Channel, AppError> {
    {
        let channels = lightwalletd_channels()
            .lock()
            .map_err(|_| AppError::Wallet("lightwalletd channel cache lock was poisoned".into()))?;
        if let Some(channel) = channels.get(lightwalletd_url) {
            return Ok(channel.clone());
        }
    }

    let uri = parse_lightwalletd_uri(lightwalletd_url)?;
    let mut endpoint = Endpoint::from_shared(lightwalletd_url.to_string()).map_err(|error| {
        AppError::InvalidConfig(format!(
            "LIGHTWALLETD_URL must be a valid endpoint; got {lightwalletd_url}: {error}"
        ))
    })?;

    if uri.scheme_str() == Some("https") {
        let host = uri.host().ok_or_else(|| {
            AppError::InvalidConfig(format!(
                "LIGHTWALLETD_URL must include a host when using https: {lightwalletd_url}"
            ))
        })?;
        let tls = ClientTlsConfig::new()
            .with_webpki_roots()
            .domain_name(host.to_string());
        endpoint = endpoint.tls_config(tls).map_err(|error| {
            AppError::InvalidConfig(format!(
                "failed to configure TLS for LIGHTWALLETD_URL {lightwalletd_url}: {error}"
            ))
        })?;
    }

    let channel = endpoint.connect().await.map_err(|error| {
        AppError::Wallet(format!(
            "failed to connect to lightwalletd at {lightwalletd_url}: {error}"
        ))
    })?;

    let mut channels = lightwalletd_channels()
        .lock()
        .map_err(|_| AppError::Wallet("lightwalletd channel cache lock was poisoned".into()))?;
    channels.insert(lightwalletd_url.to_string(), channel.clone());
    Ok(channel)
}

async fn get_lightwalletd_client(
    lightwalletd_url: &str,
) -> Result<CompactTxStreamerClient<Channel>, AppError> {
    Ok(CompactTxStreamerClient::new(
        get_lightwalletd_channel(lightwalletd_url).await?,
    ))
}

fn is_retryable_lightwalletd_error(error: &AppError) -> bool {
    matches!(error, AppError::Wallet(_))
}

async fn with_lightwalletd_retry<T, Operation, OperationFuture>(
    lightwalletd_url: &str,
    operation_name: &str,
    mut operation: Operation,
) -> Result<T, AppError>
where
    Operation: FnMut(CompactTxStreamerClient<Channel>) -> OperationFuture,
    OperationFuture: Future<Output = Result<T, AppError>>,
{
    let mut attempt = 0;
    loop {
        attempt += 1;

        let result = match get_lightwalletd_client(lightwalletd_url).await {
            Ok(client) => operation(client).await,
            Err(error) => Err(error),
        };

        match result {
            Ok(value) => return Ok(value),
            Err(error)
                if attempt < LIGHTWALLETD_RETRY_ATTEMPT_LIMIT
                    && is_retryable_lightwalletd_error(&error) =>
            {
                remove_lightwalletd_channel(lightwalletd_url);
                tracing::warn!(
                    attempt,
                    max_attempts = LIGHTWALLETD_RETRY_ATTEMPT_LIMIT,
                    lightwalletd_url,
                    operation = operation_name,
                    error = %error,
                    "lightwalletd request failed; retrying"
                );
                sleep(LIGHTWALLETD_RETRY_DELAY).await;
            }
            Err(error) => return Err(error),
        }
    }
}

fn missing_mempool_transaction_is_benign(status: &Status) -> bool {
    matches!(status.code(), Code::NotFound)
        || (status.code() == Code::Unknown
            && (status
                .message()
                .contains("No such mempool or main chain transaction")
                || status.message().contains("Transaction not found")))
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingPayment {
    pub txid_hex: String,
    pub mined_height: u32,
    pub sapling_received_zat: u64,
    pub orchard_received_zat: u64,
    pub transaction_received_zat: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct IncomingReceipt {
    pub txid_hex: String,
    pub mined_height: u32,
    pub is_mempool: bool,
    pub pool: String,
    pub output_index: u16,
    pub value_zat: u64,
    pub receiver_fingerprint: Vec<u8>,
}

pub struct ScannedCompactBlockBatch<AccountId> {
    pub receipts: Vec<IncomingReceipt>,
    pub scanned_blocks: Vec<ScannedBlock<AccountId>>,
}

pub async fn fetch_compact_blocks_for_heights(
    lightwalletd_url: &str,
    heights: &[u32],
) -> Result<Vec<CompactBlock>, AppError> {
    let mut blocks = Vec::new();
    let unique_heights = heights.iter().copied().collect::<BTreeSet<_>>();
    for height in unique_heights {
        let mut fetched_blocks = with_lightwalletd_retry(
            lightwalletd_url,
            "fetch compact block by height",
            move |mut client| async move {
                let request = BlockRange {
                    start: Some(BlockId {
                        height: u64::from(height),
                        hash: vec![],
                    }),
                    end: Some(BlockId {
                        height: u64::from(height),
                        hash: vec![],
                    }),
                    pool_types: vec![],
                };

                let mut stream = client
                    .get_block_range(request)
                    .await
                    .map_err(|error| {
                        AppError::Wallet(format!(
                            "failed to request block {height} from {lightwalletd_url}: {error}"
                        ))
                    })?
                    .into_inner();

                let mut fetched_blocks = Vec::new();
                while let Some(block) = stream.message().await.map_err(|error| {
                    AppError::Wallet(format!(
                        "failed to read block stream for height {height} from {lightwalletd_url}: {error}"
                    ))
                })? {
                    fetched_blocks.push(block);
                }

                Ok(fetched_blocks)
            },
        )
        .await?;
        blocks.append(&mut fetched_blocks);
    }

    blocks.sort_by_key(|block| block.height);
    Ok(blocks)
}

pub async fn latest_block_height(lightwalletd_url: &str) -> Result<u32, AppError> {
    let response = with_lightwalletd_retry(
        lightwalletd_url,
        "fetch latest block",
        move |mut client| async move {
            client
                .get_latest_block(ChainSpec::default())
                .await
                .map_err(|error| {
                    AppError::Wallet(format!(
                        "failed to fetch latest block from {lightwalletd_url}: {error}"
                    ))
                })
        },
    )
    .await?;
    u32::try_from(response.get_ref().height)
        .map_err(|_| AppError::Wallet("latest lightwalletd height overflowed u32".into()))
}

pub async fn fetch_tree_state(lightwalletd_url: &str, height: u32) -> Result<TreeState, AppError> {
    with_lightwalletd_retry(
        lightwalletd_url,
        "fetch tree state",
        move |mut client| async move {
            client
                .get_tree_state(BlockId {
                    height: u64::from(height),
                    hash: vec![],
                })
                .await
                .map(|response| response.into_inner())
                .map_err(|error| {
                    AppError::Wallet(format!(
                    "failed to fetch tree state at height {height} from {lightwalletd_url}: {error}"
                ))
                })
        },
    )
    .await
}

pub fn tree_state_to_chain_state(
    tree_state: &TreeState,
) -> Result<zcash_client_backend::data_api::chain::ChainState, AppError> {
    tree_state.to_chain_state().map_err(|error| {
        AppError::Wallet(format!(
            "failed to convert tree state at height {} into chain state: {error}",
            tree_state.height
        ))
    })
}

pub async fn wait_for_block_mempool_stream(
    lightwalletd_url: &str,
    expected_tip_height: u32,
) -> Result<(), AppError> {
    let mut stream = with_lightwalletd_retry(
        lightwalletd_url,
        "open mempool stream",
        move |mut client| async move {
            client
                .get_mempool_stream(Empty {})
                .await
                .map(|response| response.into_inner())
                .map_err(|error| {
                    AppError::Wallet(format!(
                        "failed to open mempool stream from {lightwalletd_url}: {error}"
                    ))
                })
        },
    )
    .await?;

    wait_for_block_mempool_stream_with(
        expected_tip_height,
        || latest_block_height(lightwalletd_url),
        || async {
            while stream
                .message()
                .await
                .map_err(|error| {
                    AppError::Wallet(format!(
                        "failed while waiting on mempool stream from {lightwalletd_url}: {error}"
                    ))
                })?
                .is_some()
            {}

            Ok(())
        },
    )
    .await
}

async fn wait_for_block_mempool_stream_with<
    LatestHeight,
    LatestHeightFuture,
    WaitForStream,
    WaitForStreamFuture,
>(
    expected_tip_height: u32,
    latest_height: LatestHeight,
    wait_for_stream: WaitForStream,
) -> Result<(), AppError>
where
    LatestHeight: FnOnce() -> LatestHeightFuture,
    LatestHeightFuture: Future<Output = Result<u32, AppError>>,
    WaitForStream: FnOnce() -> WaitForStreamFuture,
    WaitForStreamFuture: Future<Output = Result<(), AppError>>,
{
    if latest_height().await? > expected_tip_height {
        return Ok(());
    }

    wait_for_stream().await
}

pub async fn fetch_current_mempool_txids(lightwalletd_url: &str) -> Result<Vec<Vec<u8>>, AppError> {
    with_lightwalletd_retry(
        lightwalletd_url,
        "fetch mempool snapshot",
        move |mut client| async move {
            let mut stream = client
                .get_mempool_tx(GetMempoolTxRequest::default())
                .await
                .map_err(|error| {
                    AppError::Wallet(format!(
                        "failed to request mempool snapshot from {lightwalletd_url}: {error}"
                    ))
                })?
                .into_inner();

            let mut txids = Vec::new();
            while let Some(transaction) = stream.message().await.map_err(|error| {
                AppError::Wallet(format!(
                    "failed to read mempool snapshot from {lightwalletd_url}: {error}"
                ))
            })? {
                txids.push(transaction.txid);
            }

            Ok(txids)
        },
    )
    .await
}

pub async fn fetch_raw_transactions_by_txids(
    lightwalletd_url: &str,
    txids: &[Vec<u8>],
) -> Result<Vec<RawTransaction>, AppError> {
    if txids.is_empty() {
        return Ok(Vec::new());
    }

    let mut transactions = Vec::with_capacity(txids.len());
    for txid in txids {
        let txid = txid.clone();
        let raw_transaction = with_lightwalletd_retry(
            lightwalletd_url,
            "fetch mempool transaction",
            move |mut client| {
                let txid = txid.clone();
                async move {
                    match client
                        .get_transaction(TxFilter {
                            hash: txid.clone(),
                            ..Default::default()
                        })
                        .await
                    {
                        Ok(response) => Ok(Some(response.into_inner())),
                        Err(status) if missing_mempool_transaction_is_benign(&status) => {
                            tracing::debug!(
                                txid = %hex::encode(&txid),
                                lightwalletd_url,
                                status_code = ?status.code(),
                                status_message = status.message(),
                                "mempool transaction disappeared before raw fetch; skipping"
                            );
                            Ok(None)
                        }
                        Err(status) => Err(AppError::Wallet(format!(
                            "failed to fetch mempool transaction from {lightwalletd_url}: {status}"
                        ))),
                    }
                }
            },
        )
        .await?;
        if let Some(raw_transaction) = raw_transaction {
            transactions.push(raw_transaction);
        }
    }

    Ok(transactions)
}

pub async fn fetch_compact_blocks_for_height_range(
    lightwalletd_url: &str,
    start_height: u32,
    end_height: u32,
) -> Result<Vec<CompactBlock>, AppError> {
    if start_height > end_height {
        return Ok(Vec::new());
    }

    with_lightwalletd_retry(
        lightwalletd_url,
        "fetch compact block range",
        move |mut client| async move {
            let request = BlockRange {
                start: Some(BlockId {
                    height: u64::from(start_height),
                    hash: vec![],
                }),
                end: Some(BlockId {
                    height: u64::from(end_height),
                    hash: vec![],
                }),
                pool_types: vec![],
            };

            let mut stream = client
                .get_block_range(request)
                .await
                .map_err(|error| {
                    AppError::Wallet(format!(
                        "failed to request compact blocks {start_height}..={end_height} from {lightwalletd_url}: {error}"
                    ))
                })?
                .into_inner();

            let mut blocks = Vec::new();
            while let Some(block) = stream.message().await.map_err(|error| {
                AppError::Wallet(format!(
                    "failed to read block stream for heights {start_height}..={end_height} from {lightwalletd_url}: {error}"
                ))
            })? {
                blocks.push(block);
            }

            blocks.sort_by_key(|block| block.height);
            Ok(blocks)
        },
    )
    .await
}

pub fn scan_incoming_compact_blocks(
    network_name: &str,
    encoded_uivk: &str,
    blocks: impl IntoIterator<Item = CompactBlock>,
) -> Result<Vec<IncomingPayment>, AppError> {
    let receipts = scan_incoming_receipts_from_compact_blocks(network_name, encoded_uivk, blocks)?;
    Ok(aggregate_receipts_by_transaction(&receipts))
}

fn scan_compact_blocks_with_scanning_keys<AccountId>(
    network_name: &str,
    scanning_keys: &ScanningKeys<AccountId, (AccountId, Scope)>,
    blocks: impl IntoIterator<Item = CompactBlock>,
) -> Result<ScannedCompactBlockBatch<AccountId>, AppError>
where
    AccountId: Copy + Default + Eq + Hash + Send + Sync + subtle::ConditionallySelectable + 'static,
{
    let network = consensus_network(network_name)?;
    let mut receipts = Vec::new();
    let mut scanned_blocks = Vec::new();

    for block in blocks {
        let scanned_block = scan_block(&network, block, scanning_keys, &Nullifiers::empty(), None)
            .map_err(|error| AppError::Wallet(format!("failed to scan compact block: {error}")))?;

        let mined_height = u32::from(scanned_block.height());
        for tx in scanned_block.transactions() {
            let txid_hex = tx.txid().to_string();

            for output in tx.sapling_outputs() {
                receipts.push(IncomingReceipt {
                    txid_hex: txid_hex.clone(),
                    mined_height,
                    is_mempool: false,
                    pool: "sapling".into(),
                    output_index: u16::try_from(output.index()).map_err(|_| {
                        AppError::Wallet("sapling output index overflowed u16".into())
                    })?,
                    value_zat: output.note().value().inner(),
                    receiver_fingerprint: receiver_fingerprint(note_receiver_bytes(
                        &Note::Sapling(output.note().clone()),
                    )),
                });
            }

            for output in tx.orchard_outputs() {
                receipts.push(IncomingReceipt {
                    txid_hex: txid_hex.clone(),
                    mined_height,
                    is_mempool: false,
                    pool: "orchard".into(),
                    output_index: u16::try_from(output.index()).map_err(|_| {
                        AppError::Wallet("orchard output index overflowed u16".into())
                    })?,
                    value_zat: output.note().value().inner(),
                    receiver_fingerprint: receiver_fingerprint(note_receiver_bytes(
                        &Note::Orchard(*output.note()),
                    )),
                });
            }
        }

        scanned_blocks.push(scanned_block);
    }

    receipts.sort_by(|left, right| {
        left.mined_height
            .cmp(&right.mined_height)
            .then_with(|| left.txid_hex.cmp(&right.txid_hex))
            .then_with(|| left.pool.cmp(&right.pool))
            .then_with(|| left.output_index.cmp(&right.output_index))
    });

    Ok(ScannedCompactBlockBatch {
        receipts,
        scanned_blocks,
    })
}

pub fn scan_incoming_receipts_from_compact_blocks(
    network_name: &str,
    encoded_uivk: &str,
    blocks: impl IntoIterator<Item = CompactBlock>,
) -> Result<Vec<IncomingReceipt>, AppError> {
    let scanning_keys = scanning_keys_from_uivk(network_name, encoded_uivk)?;
    Ok(scan_compact_blocks_with_scanning_keys(network_name, &scanning_keys, blocks)?.receipts)
}

pub fn scan_compact_blocks_for_wallet<AccountId>(
    network_name: &str,
    scanning_keys: &ScanningKeys<AccountId, (AccountId, Scope)>,
    blocks: impl IntoIterator<Item = CompactBlock>,
) -> Result<ScannedCompactBlockBatch<AccountId>, AppError>
where
    AccountId: Copy + Default + Eq + Hash + Send + Sync + subtle::ConditionallySelectable + 'static,
{
    scan_compact_blocks_with_scanning_keys(network_name, scanning_keys, blocks)
}

pub fn scan_incoming_mempool_receipts_from_raw_transactions(
    network_name: &str,
    encoded_uivk: &str,
    chain_tip_height: u32,
    transactions: impl IntoIterator<Item = RawTransaction>,
) -> Result<Vec<IncomingReceipt>, AppError> {
    let network = consensus_network(network_name)?;
    let uivk =
        UnifiedIncomingViewingKey::decode(&network, encoded_uivk).map_err(AppError::Wallet)?;
    let chain_tip_height = BlockHeight::from_u32(chain_tip_height);
    let mut receipts = Vec::new();

    // ZIP-212 has been universally enforced on mainnet and testnet since 2021.
    // Mempool scanning operates at the current chain tip, which is always past
    // the ZIP-212 activation period.
    let zip212_enforcement = sapling::note_encryption::Zip212Enforcement::On;
    let sapling_domain = sapling::note_encryption::SaplingDomain::new(zip212_enforcement);
    let sapling_pivk = uivk.sapling().as_ref().map(|ivk| ivk.prepare());
    let orchard_pivk = uivk.orchard().as_ref().map(|ivk| ivk.prepare());

    for raw_transaction in transactions {
        let transaction = Transaction::read(
            &raw_transaction.data[..],
            BranchId::for_height(&network, chain_tip_height),
        )
        .map_err(|error| {
            AppError::Wallet(format!("failed to parse mempool transaction: {error}"))
        })?;
        let txid_hex = transaction.txid().to_string();

        if let (Some(bundle), Some(pivk)) = (transaction.sapling_bundle(), sapling_pivk.as_ref()) {
            for (index, output) in bundle.shielded_outputs().iter().enumerate() {
                if let Some((note, _, _)) =
                    zcash_note_encryption::try_note_decryption(&sapling_domain, pivk, output)
                {
                    receipts.push(IncomingReceipt {
                        txid_hex: txid_hex.clone(),
                        mined_height: 0,
                        is_mempool: true,
                        pool: "sapling".into(),
                        output_index: u16::try_from(index).map_err(|_| {
                            AppError::Wallet("sapling mempool output index overflowed u16".into())
                        })?,
                        value_zat: note.value().inner(),
                        receiver_fingerprint: receiver_fingerprint(
                            note.recipient().to_bytes().to_vec(),
                        ),
                    });
                }
            }
        }

        if let (Some(bundle), Some(pivk)) = (transaction.orchard_bundle(), orchard_pivk.as_ref()) {
            for (index, action) in bundle.actions().iter().enumerate() {
                let domain = orchard::note_encryption::OrchardDomain::for_action(action);
                if let Some((note, _, _)) =
                    zcash_note_encryption::try_note_decryption(&domain, pivk, action)
                {
                    receipts.push(IncomingReceipt {
                        txid_hex: txid_hex.clone(),
                        mined_height: 0,
                        is_mempool: true,
                        pool: "orchard".into(),
                        output_index: u16::try_from(index).map_err(|_| {
                            AppError::Wallet("orchard mempool output index overflowed u16".into())
                        })?,
                        value_zat: note.value().inner(),
                        receiver_fingerprint: receiver_fingerprint(
                            note.recipient().to_raw_address_bytes().to_vec(),
                        ),
                    });
                }
            }
        }
    }

    receipts.sort_by(|left, right| {
        left.mined_height
            .cmp(&right.mined_height)
            .then_with(|| left.txid_hex.cmp(&right.txid_hex))
            .then_with(|| left.pool.cmp(&right.pool))
            .then_with(|| left.output_index.cmp(&right.output_index))
    });
    Ok(receipts)
}

pub async fn scan_incoming_payments_for_heights(
    lightwalletd_url: &str,
    network_name: &str,
    encoded_uivk: &str,
    heights: &[u32],
) -> Result<Vec<IncomingPayment>, AppError> {
    let blocks = fetch_compact_blocks_for_heights(lightwalletd_url, heights).await?;
    scan_incoming_compact_blocks(network_name, encoded_uivk, blocks)
}

pub async fn scan_incoming_receipts_for_heights(
    lightwalletd_url: &str,
    network_name: &str,
    encoded_uivk: &str,
    heights: &[u32],
) -> Result<Vec<IncomingReceipt>, AppError> {
    let blocks = fetch_compact_blocks_for_heights(lightwalletd_url, heights).await?;
    scan_incoming_receipts_from_compact_blocks(network_name, encoded_uivk, blocks)
}

pub async fn scan_incoming_receipts_for_range(
    lightwalletd_url: &str,
    network_name: &str,
    encoded_uivk: &str,
    start_height: u32,
    end_height: u32,
) -> Result<Vec<IncomingReceipt>, AppError> {
    scan_incoming_receipts_for_range_with(
        network_name,
        encoded_uivk,
        start_height,
        end_height,
        |start_height, end_height| {
            fetch_compact_blocks_for_height_range(lightwalletd_url, start_height, end_height)
        },
    )
    .await
}

async fn scan_incoming_receipts_for_range_with<FetchBlocks, FetchBlocksFuture>(
    network_name: &str,
    encoded_uivk: &str,
    start_height: u32,
    end_height: u32,
    fetch_blocks: FetchBlocks,
) -> Result<Vec<IncomingReceipt>, AppError>
where
    FetchBlocks: FnOnce(u32, u32) -> FetchBlocksFuture,
    FetchBlocksFuture: Future<Output = Result<Vec<CompactBlock>, AppError>>,
{
    let blocks = fetch_blocks(start_height, end_height).await?;
    scan_incoming_receipts_from_compact_blocks(network_name, encoded_uivk, blocks)
}

fn aggregate_receipts_by_transaction(receipts: &[IncomingReceipt]) -> Vec<IncomingPayment> {
    let mut grouped = BTreeMap::<(u32, String), IncomingPayment>::new();
    for receipt in receipts {
        let entry = grouped
            .entry((receipt.mined_height, receipt.txid_hex.clone()))
            .or_insert_with(|| IncomingPayment {
                txid_hex: receipt.txid_hex.clone(),
                mined_height: receipt.mined_height,
                sapling_received_zat: 0,
                orchard_received_zat: 0,
                transaction_received_zat: 0,
            });

        match receipt.pool.as_str() {
            "sapling" => entry.sapling_received_zat += receipt.value_zat,
            "orchard" => entry.orchard_received_zat += receipt.value_zat,
            _ => {}
        }
        entry.transaction_received_zat += receipt.value_zat;
    }

    grouped.into_values().collect()
}

fn note_receiver_bytes(note: &Note) -> Vec<u8> {
    match note.receiver() {
        Receiver::Orchard(address) => address.to_raw_address_bytes().to_vec(),
        Receiver::Sapling(address) => address.to_bytes().to_vec(),
        Receiver::Transparent(_) => Vec::new(),
    }
}

fn receiver_fingerprint(bytes: Vec<u8>) -> Vec<u8> {
    let mut state: u64 = 0xcbf29ce484222325;
    for byte in bytes {
        state ^= u64::from(byte);
        state = state.wrapping_mul(0x100000001b3);
    }
    state.to_be_bytes().to_vec()
}

#[cfg(test)]
mod tests {
    use std::sync::{
        atomic::{AtomicBool, Ordering},
        Arc,
    };

    use tonic::Status;
    use zcash_client_backend::proto::service::{RawTransaction, TreeState};

    use crate::error::AppError;

    use super::{
        missing_mempool_transaction_is_benign,
        scan_incoming_mempool_receipts_from_raw_transactions, scan_incoming_payments_for_heights,
        scan_incoming_receipts_for_range_with, tree_state_to_chain_state,
        wait_for_block_mempool_stream_with,
    };

    const TESTNET_LIGHTWALLETD_URL_ENV: &str = "ZCASH_TESTNET_LIGHTWALLETD_URL";
    const DEFAULT_TESTNET_LIGHTWALLETD_URL: &str = "https://testnet.lightwalletd.com:9067";
    const TESTNET_UFVK_NO_TRANSPARENT: &str = "uviewtest1tcygrtut692vqlx9nlyknx0fqq59am5vhaf97gxncqnfn87qrgey68777tumstc2lcp4r9yxd3fknkpmxtgw8awhcg40cw00ahvtaeqmpfqvjz6e3v234zsfvdvt6dm8dpzxv970wkdv2jrfm3t2m9cde9ry8mrxr286ns4yqwmcx3k4netqqhgldthnhzhlpg0lk00eruy4tf3fx3k9xn7fywppj8wyzjjc3dcrqe6kxnc6zxpfly9e2uk3k7jyy3n70zpj5zfheedzz0sw2pp96rvy9xt2dw94nplfx0usrwtshrmf5xwq84qcq459kvks5g28gvkxrjpujgc9gkjt5np5m4afruk0z8zlyd65hfqqu9pg3u9lkk26r7ad4l59yy3tn2xlmad42a6kee8l92ddj7tgf4fhv4x9thx7kf28jc6gvf3xr3lhtegd8ly590595g6dfh3w7nalmt8zx6yestgfqu9uvg8gkwkwmuc4u5jsecqu";
    const MAINNET_UIVK_NO_TRANSPARENT: &str = "uivk1020vq9j5zeqxh303sxa0zv2hn9wm9fev8x0p8yqxdwyzde9r4c90fcglc63usj0ycl2scy8zxuhtser0qrq356xfy8x3vyuxu7f6gas75svl9v9m3ctuazsu0ar8e8crtx7x6zgh4kw8xm3q4rlkpm9er2wefxhhf9pn547gpuz9vw27gsdp6c03nwlrxgzhr2g6xek0x8l5avrx9ue9lf032tr7kmhqf3nfdxg7ldfgx6yf09g";

    fn testnet_uivk() -> String {
        use zcash_keys::keys::UnifiedFullViewingKey;
        let network = super::consensus_network("testnet").unwrap();
        let ufvk = UnifiedFullViewingKey::decode(&network, TESTNET_UFVK_NO_TRANSPARENT).unwrap();
        ufvk.to_unified_incoming_viewing_key().encode(&network)
    }

    const TESTNET_VECTORS: &[(u32, &str, u64)] = &[
        (
            2502953,
            "df091cf84867ad0d5a07080ae981f9532c49466747aedd49240bec56ddadb1dc",
            12_722_688,
        ),
        (
            2502969,
            "2954f524217259cc42f9201290d7dc2bd4cea1da546741d9c0deddb42e01fb91",
            1_000_000_000,
        ),
        (
            2599919,
            "5df42435266727be5dbdbc1f40eef2dd30ec7618800f3b0c43f690aa0db978e9",
            40_000_000,
        ),
        (
            2605100,
            "6619dd52f86c1f2bf8e9cacaba25a62a0d939072eb7409389b780547165716b6",
            100_000_000,
        ),
        (
            2605107,
            "4bc2672439150c543325d460925cebace0779e132b6e7b9825027acbc78ad542",
            50_000_000,
        ),
        (
            2608989,
            "c85172ee1a4bf056b7210f67d94a87ef6b558bea3d9bf862d224e18535247204",
            20_000_000,
        ),
        (
            2609617,
            "a5caca5b44196a802da9b72a49ce9ac93de4e3ac2e14ba07db336ce005046863",
            35_000_000,
        ),
        (
            2613021,
            "3ca9437d7999687f2941d5b094e9429f6b6c306c0b0daffce1b7977dc3436f23",
            100_000_000,
        ),
        (
            2613094,
            "73af225dcd31e4f152c63f1b0a92365774dce768cbe3e3889365ced0dc1a37b8",
            50_000_000,
        ),
    ];

    #[tokio::test]
    #[ignore = "hits public testnet lightwalletd"]
    async fn live_testnet_compact_blocks_are_nonempty() {
        let lightwalletd_url = std::env::var(TESTNET_LIGHTWALLETD_URL_ENV)
            .unwrap_or_else(|_| DEFAULT_TESTNET_LIGHTWALLETD_URL.to_string());
        let heights = TESTNET_VECTORS
            .iter()
            .map(|(height, _, _)| *height)
            .collect::<Vec<_>>();

        let blocks = super::fetch_compact_blocks_for_heights(&lightwalletd_url, &heights)
            .await
            .unwrap();

        assert_eq!(
            blocks.len(),
            heights.len(),
            "expected one block per requested height"
        );
    }

    #[tokio::test]
    #[ignore = "hits public testnet lightwalletd"]
    async fn live_testnet_vectors_are_detected_with_expected_amounts() {
        let lightwalletd_url = std::env::var(TESTNET_LIGHTWALLETD_URL_ENV)
            .unwrap_or_else(|_| DEFAULT_TESTNET_LIGHTWALLETD_URL.to_string());
        let heights = TESTNET_VECTORS
            .iter()
            .map(|(height, _, _)| *height)
            .collect::<Vec<_>>();

        let observations = scan_incoming_payments_for_heights(
            &lightwalletd_url,
            "testnet",
            &testnet_uivk(),
            &heights,
        )
        .await
        .unwrap();

        for (height, txid_hex, transaction_received_zat) in TESTNET_VECTORS {
            let observation = observations
                .iter()
                .find(|observation| observation.txid_hex == *txid_hex)
                .unwrap_or_else(|| panic!("missing observation for txid {txid_hex}"));

            assert_eq!(observation.mined_height, *height);
            assert_eq!(
                observation.transaction_received_zat,
                *transaction_received_zat
            );
            assert!(observation.sapling_received_zat > 0 || observation.orchard_received_zat > 0);
        }
    }

    #[test]
    fn mempool_scan_returns_no_receipts_for_empty_input() {
        let receipts = scan_incoming_mempool_receipts_from_raw_transactions(
            "mainnet",
            MAINNET_UIVK_NO_TRANSPARENT,
            1,
            Vec::<RawTransaction>::new(),
        )
        .unwrap();

        assert!(receipts.is_empty());
    }

    #[test]
    fn mempool_scan_rejects_invalid_raw_transaction_bytes() {
        let error = scan_incoming_mempool_receipts_from_raw_transactions(
            "mainnet",
            MAINNET_UIVK_NO_TRANSPARENT,
            1,
            [RawTransaction {
                data: vec![0xde, 0xad, 0xbe, 0xef],
                height: 0,
            }],
        )
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("failed to parse mempool transaction"));
    }

    #[test]
    fn missing_mempool_transaction_status_is_treated_as_benign() {
        let status = Status::unknown(
            "GetTransaction: getrawtransaction deadbeef failed: -5: No such mempool or main chain transaction",
        );

        assert!(missing_mempool_transaction_is_benign(&status));
    }

    #[test]
    fn unrelated_get_transaction_status_is_not_treated_as_benign() {
        let status = Status::internal("backend unavailable");

        assert!(!missing_mempool_transaction_is_benign(&status));
    }

    #[tokio::test]
    async fn mempool_wait_returns_immediately_when_tip_already_advanced() {
        let stream_waited = Arc::new(AtomicBool::new(false));
        let stream_waited_clone = Arc::clone(&stream_waited);

        wait_for_block_mempool_stream_with(
            100,
            || async { Ok(101) },
            move || async move {
                stream_waited_clone.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .unwrap();

        assert!(!stream_waited.load(Ordering::SeqCst));
    }

    #[tokio::test]
    async fn mempool_wait_uses_stream_when_tip_has_not_advanced() {
        let stream_waited = Arc::new(AtomicBool::new(false));
        let stream_waited_clone = Arc::clone(&stream_waited);

        wait_for_block_mempool_stream_with(
            100,
            || async { Ok(100) },
            move || async move {
                stream_waited_clone.store(true, Ordering::SeqCst);
                Ok(())
            },
        )
        .await
        .unwrap();

        assert!(stream_waited.load(Ordering::SeqCst));
    }

    #[test]
    fn malformed_tree_state_is_reported_cleanly() {
        let error = tree_state_to_chain_state(&TreeState {
            network: "test".into(),
            height: 123,
            hash: "not-a-valid-hash".into(),
            time: 0,
            sapling_tree: "zz".into(),
            orchard_tree: "zz".into(),
        })
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("failed to convert tree state at height 123 into chain state"));
    }

    #[tokio::test]
    async fn range_scan_propagates_compact_block_fetch_failures() {
        let error = scan_incoming_receipts_for_range_with(
            "mainnet",
            MAINNET_UIVK_NO_TRANSPARENT,
            10,
            12,
            |_, _| async {
                Err(AppError::Wallet(
                    "failed to request compact blocks 10..=12: fake upstream failure".into(),
                ))
            },
        )
        .await
        .unwrap_err();

        assert!(error
            .to_string()
            .contains("failed to request compact blocks 10..=12: fake upstream failure"));
    }

    #[tokio::test]
    async fn range_scan_accepts_empty_compact_block_batches() {
        let receipts = scan_incoming_receipts_for_range_with(
            "mainnet",
            MAINNET_UIVK_NO_TRANSPARENT,
            10,
            12,
            |_, _| async {
                Ok(Vec::<
                    zcash_client_backend::proto::compact_formats::CompactBlock,
                >::new())
            },
        )
        .await
        .unwrap();

        assert!(receipts.is_empty());
    }
}
