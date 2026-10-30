use std::{
    collections::HashSet,
    fs,
    path::{Path, PathBuf},
    sync::{Mutex, MutexGuard, OnceLock},
};

use rand::{rand_core::UnwrapErr, rngs::SysRng};
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

type SqliteWalletDb = WalletDb<
    rusqlite::Connection,
    zcash_protocol::consensus::Network,
    SystemClock,
    UnwrapErr<SysRng>,
>;

fn wallet_db_init_lock() -> Result<MutexGuard<'static, ()>, AppError> {
    static WALLET_DB_INIT_LOCK: OnceLock<Mutex<()>> = OnceLock::new();
    WALLET_DB_INIT_LOCK
        .get_or_init(|| Mutex::new(()))
        .lock()
        .map_err(|_| AppError::Wallet("wallet DB initialization lock was poisoned".into()))
}

fn migrated_wallet_db_paths() -> &'static Mutex<HashSet<PathBuf>> {
    static MIGRATED_PATHS: OnceLock<Mutex<HashSet<PathBuf>>> = OnceLock::new();
    MIGRATED_PATHS.get_or_init(|| Mutex::new(HashSet::new()))
}

fn normalize_wallet_db_path(path: &Path) -> PathBuf {
    path.canonicalize().unwrap_or_else(|_| path.to_path_buf())
}

fn path_already_migrated(normalized_path: &Path) -> Result<bool, AppError> {
    Ok(migrated_wallet_db_paths()
        .lock()
        .map_err(|_| AppError::Wallet("wallet DB migration cache lock was poisoned".into()))?
        .contains(normalized_path))
}

fn mark_path_migrated(normalized_path: PathBuf) -> Result<(), AppError> {
    migrated_wallet_db_paths()
        .lock()
        .map_err(|_| AppError::Wallet("wallet DB migration cache lock was poisoned".into()))?
        .insert(normalized_path);
    Ok(())
}

fn migrate_wallet_db(wallet_db: &mut SqliteWalletDb) -> Result<(), AppError> {
    init_wallet_db(wallet_db, None).map_err(|error| {
        AppError::Wallet(format!("failed to initialize wallet DB schema: {error}"))
    })
}

fn open_wallet_db(path: &Path, network_name: &str) -> Result<SqliteWalletDb, AppError> {
    if let Some(parent) = path.parent() {
        fs::create_dir_all(parent)?;
    }

    let network = consensus_network(network_name)?;
    let conn = open_wallet_connection(path)?;
    rusqlite::vtab::array::load_module(&conn)?;
    Ok(WalletDb::from_connection(
        conn,
        network,
        SystemClock,
        UnwrapErr(SysRng),
    ))
}

/// How long a wallet database connection waits for another connection's lock
/// before failing with "database is locked". Matches the app database.
const WALLET_DB_BUSY_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(5);

/// Opens a wallet database connection that waits for locks instead of failing
/// at once. `WalletDb::for_path` sets no busy timeout, so a scan or migration
/// overlapping another open (as at startup) could fail immediately.
fn open_wallet_connection(path: &Path) -> Result<Connection, AppError> {
    let conn = Connection::open(path)?;
    conn.busy_timeout(WALLET_DB_BUSY_TIMEOUT)?;
    Ok(conn)
}

fn open_migrated_wallet_db(path: &Path, network_name: &str) -> Result<SqliteWalletDb, AppError> {
    let normalized_path = normalize_wallet_db_path(path);
    if path_already_migrated(&normalized_path)? {
        return open_wallet_db(path, network_name);
    }

    // Lock before opening. Opening first, then waiting on the init mutex, leaves
    // extra SQLite connections live while init_wallet_db needs an exclusive lock
    // (a lock inversion Ironwood schema migrations hit reliably).
    let _guard = wallet_db_init_lock()?;

    let normalized_path = normalize_wallet_db_path(path);
    if path_already_migrated(&normalized_path)? {
        return open_wallet_db(path, network_name);
    }

    let mut wallet_db = open_wallet_db(path, network_name)?;
    migrate_wallet_db(&mut wallet_db)?;
    mark_path_migrated(normalize_wallet_db_path(path))?;
    Ok(wallet_db)
}

pub fn initialize_wallet_db(config: &Config) -> Result<(), AppError> {
    open_migrated_wallet_db(&config.wallet_db_path, &config.network).map(|_| ())
}

pub fn wallet_db_state(config: &Config) -> Result<WalletDbState, AppError> {
    let wallet_db = open_migrated_wallet_db(&config.wallet_db_path, &config.network)?;

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

    let conn = open_wallet_connection(path)?;
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

#[cfg(test)]
mod tests {
    use std::{
        path::Path,
        sync::{Arc, Barrier},
        thread,
    };

    #[test]
    fn wallet_connections_wait_for_locks() {
        let temp = tempfile::tempdir().unwrap();
        let conn = super::open_wallet_connection(&temp.path().join("wallet.db")).unwrap();
        let timeout: i64 = conn
            .query_row("PRAGMA busy_timeout", [], |row| row.get(0))
            .unwrap();
        assert_eq!(timeout, 5_000);
    }

    #[test]
    fn a_wallet_open_waits_out_a_short_write_lock() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("wallet.db");
        let holder = rusqlite::Connection::open(&path).unwrap();
        holder
            .execute_batch("CREATE TABLE t (x); BEGIN EXCLUSIVE;")
            .unwrap();
        let release = std::thread::spawn(move || {
            std::thread::sleep(std::time::Duration::from_millis(200));
            holder.execute_batch("COMMIT").unwrap();
        });

        let conn = super::open_wallet_connection(&path).unwrap();
        conn.execute("INSERT INTO t (x) VALUES (1)", []).unwrap();
        release.join().unwrap();
    }

    use tempfile::tempdir;

    use super::initialize_wallet_db;
    use crate::config::Config;

    fn config(temp: &Path) -> Config {
        Config {
            listen_addr: "127.0.0.1:0".into(),
            network: "mainnet".into(),
            startup_uivk: None,
            lightwalletd_url: None,
            birthday_height: Some(123),
            wallet_db_path: temp.join("wallet.db"),
            app_db_path: temp.join("app.db"),
            log_dir: temp.join("logs"),
            catch_up_threshold_blocks: 1,
            catch_up_batch_size: 100,
            scan_parallelism: 2,
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
    fn concurrent_initialize_of_the_same_wallet_db_completes() {
        let temp = tempdir().unwrap();
        let config = Arc::new(config(temp.path()));
        let barrier = Arc::new(Barrier::new(8));

        let handles: Vec<_> = (0..8)
            .map(|_| {
                let config = Arc::clone(&config);
                let barrier = Arc::clone(&barrier);
                thread::spawn(move || {
                    barrier.wait();
                    initialize_wallet_db(&config)
                })
            })
            .collect();

        for handle in handles {
            handle.join().unwrap().unwrap();
        }
    }
}
