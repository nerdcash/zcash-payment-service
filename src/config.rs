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
    /// Maximum number of threads used to trial-decrypt compact blocks.
    pub scan_parallelism: usize,
    pub sync_poll_interval_seconds: u64,
    pub webhook_url: Option<String>,
    pub webhook_secret: Option<String>,
    pub webhook_poll_interval_seconds: u64,
    pub webhook_retry_delay_seconds: u64,
    pub webhook_retry_max_delay_seconds: u64,
    pub webhook_max_attempts: u32,
    pub webhook_report_confirmations: u32,
    pub finality_confirmations: u32,
    /// Public origin (and optional path prefix) where payers reach the pay page.
    pub public_base_url: Option<String>,
    /// Enables `POST /dev/simulate-payment`. Testnet only; for automated tests.
    pub dev_simulated_payments: bool,
    /// Origins (`scheme://host[:port]`) a session's `return_url` may point to. Empty allows any
    /// `http(s)` URL.
    pub return_url_allowed_origins: Vec<String>,
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
        let public_base_url = env::var("PUBLIC_BASE_URL")
            .ok()
            .map(|value| value.trim().trim_end_matches('/').to_string())
            .filter(|value| !value.is_empty());
        if let Some(public_base_url) = public_base_url.as_deref() {
            validate_public_base_url(public_base_url).map_err(AppError::InvalidConfig)?;
        }
        let dev_simulated_payments = match env::var("ZCASH_DEV_SIMULATED_PAYMENTS")
            .ok()
            .map(|value| value.trim().to_ascii_lowercase())
            .as_deref()
        {
            None | Some("") | Some("false") | Some("0") => false,
            Some("true") | Some("1") => true,
            Some(_) => {
                return Err(AppError::InvalidConfig(
                    "ZCASH_DEV_SIMULATED_PAYMENTS must be 'true' or 'false'".into(),
                ))
            }
        };
        let return_url_allowed_origins = env::var("RETURN_URL_ALLOWED_ORIGINS")
            .map(|value| parse_allowed_origins(&value))
            .unwrap_or_else(|_| Ok(Vec::new()))
            .map_err(AppError::InvalidConfig)?;
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

        let scan_parallelism = parse_scan_parallelism(
            env::var("SCAN_PARALLELISM").ok().as_deref(),
            default_scan_parallelism(),
        )?;

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

        if dev_simulated_payments {
            validate_dev_simulated_payments(&network, lightwalletd_url.as_deref())
                .map_err(AppError::InvalidConfig)?;
        }

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
            scan_parallelism,
            sync_poll_interval_seconds,
            webhook_url,
            webhook_secret,
            webhook_poll_interval_seconds,
            webhook_retry_delay_seconds,
            webhook_retry_max_delay_seconds,
            webhook_max_attempts,
            webhook_report_confirmations,
            finality_confirmations,
            public_base_url,
            dev_simulated_payments,
            return_url_allowed_origins,
        })
    }
}

/// The default scan parallelism: every core the process may use.
pub fn default_scan_parallelism() -> usize {
    std::thread::available_parallelism()
        .map(std::num::NonZeroUsize::get)
        .unwrap_or(1)
}

fn parse_scan_parallelism(value: Option<&str>, default: usize) -> Result<usize, AppError> {
    let Some(value) = value else {
        return Ok(default);
    };
    let parsed = value.trim().parse::<usize>().map_err(|_| {
        AppError::InvalidConfig("SCAN_PARALLELISM must be a positive integer".into())
    })?;
    if parsed == 0 {
        return Err(AppError::InvalidConfig(
            "SCAN_PARALLELISM must be greater than zero".into(),
        ));
    }
    Ok(parsed)
}

fn validate_webhook_url(webhook_url: &str) -> Result<(), AppError> {
    validate_http_url("ZCASH_WEBHOOK_URL", webhook_url).map_err(AppError::InvalidConfig)
}

/// Checks that `value` is an absolute `http` or `https` URL with a host.
/// The error text names the setting or field so it can be reported as-is.
/// Simulated payments are a test hook. They are refused on mainnet, and alongside a real chain
/// scanner, which would race with them and could later reconcile the fake receipts away.
pub fn validate_dev_simulated_payments(
    network: &str,
    lightwalletd_url: Option<&str>,
) -> Result<(), String> {
    if network != "testnet" {
        return Err(
            "ZCASH_DEV_SIMULATED_PAYMENTS is only allowed with ZCASH_NETWORK=testnet".into(),
        );
    }
    if lightwalletd_url.is_some() {
        return Err("ZCASH_DEV_SIMULATED_PAYMENTS cannot be combined with LIGHTWALLETD_URL".into());
    }
    Ok(())
}

/// `PUBLIC_BASE_URL` is a prefix that `/pay/<id>` is appended to, so it may hold an origin and a
/// path but no query or fragment.
pub fn validate_public_base_url(value: &str) -> Result<(), String> {
    validate_http_url("PUBLIC_BASE_URL", value)?;
    if value.contains('?') || value.contains('#') {
        return Err(format!(
            "PUBLIC_BASE_URL must not include a query or fragment; got {value}"
        ));
    }
    Ok(())
}

pub fn validate_http_url(name: &str, value: &str) -> Result<(), String> {
    let uri = value.parse::<Uri>().map_err(|error| {
        format!("{name} must be a valid absolute http(s) URL; got {value}: {error}")
    })?;

    let scheme = uri
        .scheme_str()
        .ok_or_else(|| format!("{name} must be an absolute http(s) URL; got {value}"))?;

    if scheme != "http" && scheme != "https" {
        return Err(format!("{name} must use http or https; got {value}"));
    }

    if uri.authority().is_none() {
        return Err(format!("{name} must include a host; got {value}"));
    }

    Ok(())
}

/// Parses a comma-separated list of origins into normalized `scheme://host[:port]` strings.
pub fn parse_allowed_origins(value: &str) -> Result<Vec<String>, String> {
    let mut origins = Vec::new();
    for item in value
        .split(',')
        .map(str::trim)
        .filter(|item| !item.is_empty())
    {
        let item = item.trim_end_matches('/');
        validate_http_url("RETURN_URL_ALLOWED_ORIGINS", item)?;
        let uri = item.parse::<Uri>().map_err(|error| error.to_string())?;
        if uri.path() != "/" && !uri.path().is_empty() || uri.query().is_some() {
            return Err(format!(
                "RETURN_URL_ALLOWED_ORIGINS entries must be origins without a path; got {item}"
            ));
        }
        origins.push(url_origin(&uri)?);
    }
    Ok(origins)
}

/// The normalized origin of an absolute `http(s)` URL: lowercase scheme and host, default
/// port removed. Rejects URLs with user info, which can disguise the real host.
pub fn url_origin(uri: &Uri) -> Result<String, String> {
    let authority = uri
        .authority()
        .ok_or_else(|| "URL must include a host".to_string())?;
    if authority.as_str().contains('@') {
        return Err("URL must not include user info".to_string());
    }
    let scheme = uri.scheme_str().unwrap_or_default().to_ascii_lowercase();
    let host = authority.host().to_ascii_lowercase();
    match (scheme.as_str(), authority.port_u16()) {
        ("http", Some(80)) | ("https", Some(443)) | (_, None) => Ok(format!("{scheme}://{host}")),
        (_, Some(port)) => Ok(format!("{scheme}://{host}:{port}")),
    }
}

/// Checks a session's `return_url`: absolute `http(s)`, no user info, and, when `allowed` is
/// not empty, on one of those origins.
pub fn validate_return_url(value: &str, allowed: &[String]) -> Result<(), String> {
    validate_http_url("return_url", value)?;
    let uri = value
        .parse::<Uri>()
        .map_err(|error| format!("return_url is invalid: {error}"))?;
    let origin = url_origin(&uri).map_err(|error| format!("return_url: {error}"))?;
    if !allowed.is_empty() && !allowed.iter().any(|item| item == &origin) {
        return Err(format!(
            "return_url origin {origin} is not in RETURN_URL_ALLOWED_ORIGINS"
        ));
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{
        parse_scan_parallelism, validate_dev_simulated_payments, validate_public_base_url,
        validate_webhook_url,
    };

    #[test]
    fn dev_simulated_payments_needs_testnet_without_scanner() {
        assert!(validate_dev_simulated_payments("testnet", None).is_ok());
        assert!(validate_dev_simulated_payments("mainnet", None).is_err());
        assert!(
            validate_dev_simulated_payments("testnet", Some("https://lwd.example:443")).is_err()
        );
    }
    #[test]
    fn return_url_allow_list() {
        use super::{parse_allowed_origins, validate_return_url};
        let allowed =
            parse_allowed_origins("https://Shop.Example, http://localhost:8080/").unwrap();
        assert_eq!(allowed, ["https://shop.example", "http://localhost:8080"]);
        assert!(validate_return_url("https://shop.example/orders/1?key=x", &allowed).is_ok());
        assert!(validate_return_url("https://shop.example:443/x", &allowed).is_ok());
        assert!(validate_return_url("http://localhost:8080/x", &allowed).is_ok());
        for bad in [
            "https://evil.example/x",
            "https://shop.example.evil.example/x",
            "http://shop.example/x",
            "https://shop.example@evil.example/x",
            "http://localhost:8081/x",
        ] {
            assert!(validate_return_url(bad, &allowed).is_err(), "{bad}");
        }
        assert!(validate_return_url("https://any.example/x", &[]).is_ok());
        assert!(validate_return_url("https://user@any.example/x", &[]).is_err());
        assert!(parse_allowed_origins("https://shop.example/path").is_err());
        assert!(parse_allowed_origins("ftp://shop.example").is_err());
        assert!(parse_allowed_origins(" , ").unwrap().is_empty());
    }


    #[test]
    fn public_base_url_rejects_query_and_fragment() {
        assert!(validate_public_base_url("https://pay.example").is_ok());
        assert!(validate_public_base_url("https://pay.example/zec").is_ok());
        assert!(validate_public_base_url("https://pay.example/checkout?lang=en").is_err());
        assert!(validate_public_base_url("https://pay.example/#x").is_err());
    }

    #[test]
    fn scan_parallelism_defaults_when_unset() {
        assert_eq!(parse_scan_parallelism(None, 6).unwrap(), 6);
    }

    #[test]
    fn scan_parallelism_accepts_positive_integers() {
        assert_eq!(parse_scan_parallelism(Some("1"), 6).unwrap(), 1);
        assert_eq!(parse_scan_parallelism(Some(" 4 "), 6).unwrap(), 4);
    }

    #[test]
    fn scan_parallelism_rejects_zero_negative_and_garbage() {
        let zero = parse_scan_parallelism(Some("0"), 6).unwrap_err();
        assert!(zero
            .to_string()
            .contains("SCAN_PARALLELISM must be greater than zero"));
        for bad in ["-1", "two", "", "1.5"] {
            let error = parse_scan_parallelism(Some(bad), 6).unwrap_err();
            assert!(error
                .to_string()
                .contains("SCAN_PARALLELISM must be a positive integer"));
        }
    }

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
