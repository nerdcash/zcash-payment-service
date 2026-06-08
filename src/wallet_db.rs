use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, OnceLock},
};

use rand_core::OsRng;
use rusqlite::{Connection, Error as SqliteError, OptionalExtension};
use zcash_client_backend::data_api::{wallet::ConfirmationsPolicy, WalletRead};
use zcash_client_sqlite::{util::SystemClock, wallet::init::init_wallet_db, WalletDb};

use crate::{config::Config, error::AppError, zcash::consensus_network};

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletDbIdentity {
    pub uivk: String,
    pub birthday_height: u32,
}

#[derive(Clone, Debug, PartialEq, Eq)]
pub struct WalletDbState {
    pub birthday_height: Option<u32>,
    pub chain_tip_height: Option<u32>,
    pub fully_scanned_height: Option<u32>,
}

type SqliteWalletDb =
    WalletDb<rusqlite::Connection, zcash_protocol::consensus::Network, SystemClock, OsRng>;

fn migrate_wallet_db(wallet_db: &mut SqliteWalletDb) -> Result<(), AppError> {
    static WALLET_DB_INIT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    let _guard = WALLET_DB_INIT_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| AppError::Wallet("wallet DB initialization lock was poisoned".into()))?;
    init_wallet_db(wallet_db, None).map_err(|error| {
        AppError::Wallet(format!("failed to initialize wallet DB schema: {error}"))
    })
}

fn migrated_wallet_db_paths() -> &'static Mutex<HashSet<PathBuf>> {
    static MIGRATED_PATHS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    MIGRATED_PATHS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn normalize_wallet_db_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn ensure_wallet_db_schema(path: &Path, wallet_db: &mut SqliteWalletDb) -> Result<(), AppError> {
    let normalized_path = normalize_wallet_db_path(path);

    {
        let migrated_paths = migrated_wallet_db_paths()
            .lock()
            .map_err(|_| AppError::Wallet("wallet DB migration cache lock was poisoned".into()))?;
        if migrated_paths.contains(&normalized_path) {
            return Ok(());
        }
    }

    migrate_wallet_db(wallet_db)?;

    let mut migrated_paths = migrated_wallet_db_paths()
        .lock()
        .map_err(|_| AppError::Wallet("wallet DB migration cache lock was poisoned".into()))?;
    migrated_paths.insert(normalized_path);
    Ok(())
}

fn open_wallet_db(path: &Path, network_name: &str) -> Result<SqliteWalletDb, AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let network = consensus_network(network_name)?;
    WalletDb::for_path(path, network, SystemClock, OsRng).map_err(AppError::Database)
}

pub fn initialize_wallet_db(config: &Config) -> Result<(), AppError> {
    let mut wallet_db = open_wallet_db(&config.wallet_db_path, &config.network)?;
    ensure_wallet_db_schema(&config.wallet_db_path, &mut wallet_db)?;
    Ok(())
}

pub fn wallet_db_state(config: &Config) -> Result<WalletDbState, AppError> {
    let mut wallet_db = open_wallet_db(&config.wallet_db_path, &config.network)?;
    ensure_wallet_db_schema(&config.wallet_db_path, &mut wallet_db)?;

    let birthday_height = wallet_db
        .get_wallet_birthday()
        .map_err(|error| AppError::Wallet(format!("failed to read wallet DB birthday: {error}")))?
        .map(u32::from);
    let summary = wallet_db
        .get_wallet_summary(ConfirmationsPolicy::default())
        .map_err(|error| AppError::Wallet(format!("failed to read wallet DB summary: {error}")))?;

    Ok(WalletDbState {
        birthday_height,
        chain_tip_height: summary
            .as_ref()
            .map(|summary| u32::from(summary.chain_tip_height())),
        fully_scanned_height: summary
            .as_ref()
            .map(|summary| u32::from(summary.fully_scanned_height())),
    })
}

pub fn read_wallet_db_identity(path: &Path) -> Result<Option<WalletDbIdentity>, AppError> {
    if !path.exists() {
        return Ok(None);
    }

    let conn = Connection::open(path)?;
    let query_result = conn
        .query_row(
            "SELECT uivk, birthday_height FROM accounts ORDER BY rowid ASC LIMIT 1",
            [],
            |row| {
                Ok(WalletDbIdentity {
                    uivk: row.get(0)?,
                    birthday_height: row.get(1)?,
                })
            },
        )
        .optional();

    match query_result {
        Ok(identity) => Ok(identity),
        Err(SqliteError::SqlInputError { .. }) | Err(SqliteError::SqliteFailure(_, Some(_))) => {
            Ok(None)
        }
        Err(SqliteError::QueryReturnedNoRows) => Ok(None),
        Err(error) => Err(AppError::Database(error)),
    }
}
