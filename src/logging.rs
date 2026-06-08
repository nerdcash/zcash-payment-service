use std::fs;

use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::{fmt, layer::SubscriberExt, util::SubscriberInitExt, EnvFilter};

use crate::{config::Config, error::AppError};

pub fn init_logging(config: &Config) -> Result<WorkerGuard, AppError> {
    fs::create_dir_all(&config.log_dir)?;

    let file_appender = tracing_appender::rolling::daily(&config.log_dir, "zcash-payment.log");
    let (file_writer, guard) = tracing_appender::non_blocking(file_appender);

    let env_filter = EnvFilter::try_from_default_env().unwrap_or_else(|_| EnvFilter::new("info"));

    tracing_subscriber::registry()
        .with(env_filter)
        .with(fmt::layer().with_writer(std::io::stdout).compact())
        .with(fmt::layer().json().with_writer(file_writer))
        .try_init()
        .map_err(|error| {
            AppError::InvalidConfig(format!("failed to initialize logging: {error}"))
        })?;

    Ok(guard)
}
