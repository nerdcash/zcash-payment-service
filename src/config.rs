use std::{env, path::PathBuf};

use http::Uri;

use crate::{
    constants::{
        DEFAULT_CATCH_UP_BATCH_SIZE, DEFAULT_CATCH_UP_THRESHOLD_BLOCKS,
        DEFAULT_SYNC_POLL_INTERVAL_SECONDS, DEFAULT_WEBHOOK_MAX_ATTEMPTS,
        DEFAULT_WEBHOOK_POLL_INTERVAL_SECONDS, DEFAULT_WEBHOOK_RETRY_DELAY_SECONDS,
        DEFAULT_WEBHOOK_RETRY_MAX_DELAY_SECONDS, FINALITY_CONFIRMATIONS,
        WEBHOOK_REPORT_CONFIRMATIONS,
    },
    error::AppError,
};

#[derive(Clone, Debug)]
pub struct Config {
    pub listen_addr: String,
    pub network: String,
    pub startup_uivk: Option<String>,
    pub lightwalletd_url: Option<String>,
    pub birthday_height: Option<u32>,
    pub wallet_db_path: PathBuf,
    pub app_db_path: PathBuf,
    pub log_dir: PathBuf,
    pub catch_up_threshold_blocks: u32,
    pub catch_up_batch_size: u32,
    pub sync_poll_interval_seconds: u64,
    pub webhook_url: Option<String>,
    pub webhook_secret: Option<String>,
    pub webhook_poll_interval_seconds: u64,
    pub webhook_retry_delay_seconds: u64,
    pub webhook_retry_max_delay_seconds: u64,
    pub webhook_max_attempts: u32,
    pub webhook_report_confirmations: u32,
    pub finality_confirmations: u32,
}

impl Config {
    pub fn from_env() -> Result<Self, AppError> {
        let listen_addr = env::var("LISTEN_ADDR")
            .or_else(|_| env::var("PORT").map(|port| format!("0.0.0.0:{port}")))
            .unwrap_or_else(|_| "0.0.0.0:8787".to_string());

        let data_dir = env::var_os("APP_DATA_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| PathBuf::from("data/zcash-payment"));

        let app_db_path = env::var_os("APP_DB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("app.db"));
        let wallet_db_path = env::var_os("WALLET_DB_PATH")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("wallet.db"));
        let log_dir = env::var_os("LOG_DIR")
            .map(PathBuf::from)
            .unwrap_or_else(|| data_dir.join("logs"));

        if app_db_path == wallet_db_path {
            return Err(AppError::InvalidConfig(
                "APP_DB_PATH and WALLET_DB_PATH must point to different files".into(),
            ));
        }

        let network = env::var("ZCASH_NETWORK").unwrap_or_else(|_| "testnet".into());
        if network != "testnet" && network != "mainnet" {
            return Err(AppError::InvalidConfig(
                "ZCASH_NETWORK must be 'testnet' or 'mainnet'".into(),
            ));
        }

        let startup_uivk = env::var("ZCASH_UIVK")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let lightwalletd_url = env::var("LIGHTWALLETD_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        let webhook_url = env::var("ZCASH_WEBHOOK_URL")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());
        if let Some(webhook_url) = webhook_url.as_deref() {
            validate_webhook_url(webhook_url)?;
        }
        let webhook_secret = env::var("ZCASH_WEBHOOK_SECRET")
            .ok()
            .map(|value| value.trim().to_string())
            .filter(|value| !value.is_empty());

        let birthday_height = env::var("ZCASH_BIRTHDAY_HEIGHT")
            .ok()
            .map(|value| {
                value.parse::<u32>().map_err(|_| {
                    AppError::InvalidConfig(
                        "ZCASH_BIRTHDAY_HEIGHT must be a positive integer".into(),
                    )
                })
            })
            .transpose()?;

        let catch_up_threshold_blocks = env::var("CATCH_UP_THRESHOLD_BLOCKS")
            .ok()
            .map(|value| {
                value.parse::<u32>().map_err(|_| {
                    AppError::InvalidConfig(
                        "CATCH_UP_THRESHOLD_BLOCKS must be a non-negative integer".into(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(DEFAULT_CATCH_UP_THRESHOLD_BLOCKS);

        let catch_up_batch_size = env::var("CATCH_UP_BATCH_SIZE")
            .ok()
            .map(|value| {
                value.parse::<u32>().map_err(|_| {
                    AppError::InvalidConfig("CATCH_UP_BATCH_SIZE must be a positive integer".into())
                })
            })
            .transpose()?
            .unwrap_or(DEFAULT_CATCH_UP_BATCH_SIZE);
        if catch_up_batch_size == 0 {
            return Err(AppError::InvalidConfig(
                "CATCH_UP_BATCH_SIZE must be greater than zero".into(),
            ));
        }

        let sync_poll_interval_seconds = env::var("SYNC_POLL_INTERVAL_SECONDS")
            .ok()
            .map(|value| {
                value.parse::<u64>().map_err(|_| {
                    AppError::InvalidConfig(
                        "SYNC_POLL_INTERVAL_SECONDS must be a positive integer".into(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(DEFAULT_SYNC_POLL_INTERVAL_SECONDS);
        if sync_poll_interval_seconds == 0 {
            return Err(AppError::InvalidConfig(
                "SYNC_POLL_INTERVAL_SECONDS must be greater than zero".into(),
            ));
        }

        let webhook_poll_interval_seconds = env::var("WEBHOOK_POLL_INTERVAL_SECONDS")
            .ok()
            .map(|value| {
                value.parse::<u64>().map_err(|_| {
                    AppError::InvalidConfig(
                        "WEBHOOK_POLL_INTERVAL_SECONDS must be a positive integer".into(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(DEFAULT_WEBHOOK_POLL_INTERVAL_SECONDS);
        if webhook_poll_interval_seconds == 0 {
            return Err(AppError::InvalidConfig(
                "WEBHOOK_POLL_INTERVAL_SECONDS must be greater than zero".into(),
            ));
        }

        let webhook_retry_delay_seconds = env::var("WEBHOOK_RETRY_DELAY_SECONDS")
            .ok()
            .map(|value| {
                value.parse::<u64>().map_err(|_| {
                    AppError::InvalidConfig(
                        "WEBHOOK_RETRY_DELAY_SECONDS must be a positive integer".into(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(DEFAULT_WEBHOOK_RETRY_DELAY_SECONDS);
        if webhook_retry_delay_seconds == 0 {
            return Err(AppError::InvalidConfig(
                "WEBHOOK_RETRY_DELAY_SECONDS must be greater than zero".into(),
            ));
        }

        let webhook_retry_max_delay_seconds = env::var("WEBHOOK_RETRY_MAX_DELAY_SECONDS")
            .ok()
            .map(|value| {
                value.parse::<u64>().map_err(|_| {
                    AppError::InvalidConfig(
                        "WEBHOOK_RETRY_MAX_DELAY_SECONDS must be a positive integer".into(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(DEFAULT_WEBHOOK_RETRY_MAX_DELAY_SECONDS);
        if webhook_retry_max_delay_seconds == 0 {
            return Err(AppError::InvalidConfig(
                "WEBHOOK_RETRY_MAX_DELAY_SECONDS must be greater than zero".into(),
            ));
        }

        let webhook_max_attempts = env::var("WEBHOOK_MAX_ATTEMPTS")
            .ok()
            .map(|value| {
                value.parse::<u32>().map_err(|_| {
                    AppError::InvalidConfig(
                        "WEBHOOK_MAX_ATTEMPTS must be a positive integer".into(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(DEFAULT_WEBHOOK_MAX_ATTEMPTS);
        if webhook_max_attempts == 0 {
            return Err(AppError::InvalidConfig(
                "WEBHOOK_MAX_ATTEMPTS must be greater than zero".into(),
            ));
        }

        let webhook_report_confirmations = env::var("WEBHOOK_REPORT_CONFIRMATIONS")
            .ok()
            .map(|value| {
                value.parse::<u32>().map_err(|_| {
                    AppError::InvalidConfig(
                        "WEBHOOK_REPORT_CONFIRMATIONS must be a non-negative integer".into(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(WEBHOOK_REPORT_CONFIRMATIONS);

        let finality_confirmations = env::var("FINALITY_CONFIRMATIONS")
            .ok()
            .map(|value| {
                value.parse::<u32>().map_err(|_| {
                    AppError::InvalidConfig(
                        "FINALITY_CONFIRMATIONS must be a non-negative integer".into(),
                    )
                })
            })
            .transpose()?
            .unwrap_or(FINALITY_CONFIRMATIONS);

        Ok(Self {
            listen_addr,
            network,
            startup_uivk,
            lightwalletd_url,
            birthday_height,
            wallet_db_path,
            app_db_path,
            log_dir,
            catch_up_threshold_blocks,
            catch_up_batch_size,
            sync_poll_interval_seconds,
            webhook_url,
            webhook_secret,
            webhook_poll_interval_seconds,
            webhook_retry_delay_seconds,
            webhook_retry_max_delay_seconds,
            webhook_max_attempts,
            webhook_report_confirmations,
            finality_confirmations,
        })
    }
}

fn validate_webhook_url(webhook_url: &str) -> Result<(), AppError> {
    let uri = webhook_url.parse::<Uri>().map_err(|error| {
        AppError::InvalidConfig(format!(
            "ZCASH_WEBHOOK_URL must be a valid absolute http(s) URL; got {webhook_url}: {error}"
        ))
    })?;

    let scheme = uri.scheme_str().ok_or_else(|| {
        AppError::InvalidConfig(format!(
            "ZCASH_WEBHOOK_URL must be an absolute http(s) URL; got {webhook_url}"
        ))
    })?;

    if scheme != "http" && scheme != "https" {
        return Err(AppError::InvalidConfig(format!(
            "ZCASH_WEBHOOK_URL must use http or https; got {webhook_url}"
        )));
    }

    if uri.authority().is_none() {
        return Err(AppError::InvalidConfig(format!(
            "ZCASH_WEBHOOK_URL must include a host; got {webhook_url}"
        )));
    }

    Ok(())
}

#[cfg(test)]
mod tests {
    use super::validate_webhook_url;

    #[test]
    fn webhook_url_accepts_absolute_http_url() {
        validate_webhook_url("http://localhost:8080/webhooks/zcash-payments").unwrap();
    }

    #[test]
    fn webhook_url_rejects_relative_path() {
        let error = validate_webhook_url("/webhooks/zcash-payments").unwrap_err();
        assert!(error
            .to_string()
            .contains("ZCASH_WEBHOOK_URL must be an absolute http(s) URL"));
    }
}
