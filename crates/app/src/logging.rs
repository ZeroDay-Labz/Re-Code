//! Structured logs, written to a file. Logging must never reach stdout or stderr, because that
//! would corrupt the terminal interface.

use anyhow::{Context, Result};
use std::path::Path;
use tracing_appender::non_blocking::WorkerGuard;
use tracing_subscriber::EnvFilter;

const FILTER_VARIABLE: &str = "RE_CODE_LOG";

/// Starts logging. The returned guard flushes pending lines when dropped.
pub fn init(log_dir: &Path) -> Result<WorkerGuard> {
    // Logs can mention project details, so only the owner may read them.
    let mut builder = std::fs::DirBuilder::new();
    builder.recursive(true);
    #[cfg(unix)]
    std::os::unix::fs::DirBuilderExt::mode(&mut builder, 0o700);
    builder.create(log_dir).with_context(|| format!("cannot create log directory {}", log_dir.display()))?;
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        std::fs::set_permissions(log_dir, std::fs::Permissions::from_mode(0o700))
            .with_context(|| format!("cannot secure log directory {}", log_dir.display()))?;
    }
    let appender = tracing_appender::rolling::daily(log_dir, "re-code.log");
    let (writer, guard) = tracing_appender::non_blocking(appender);
    let filter = EnvFilter::try_from_env(FILTER_VARIABLE).unwrap_or_else(|_| EnvFilter::new("info"));
    tracing_subscriber::fmt().json().with_env_filter(filter).with_writer(writer).init();
    Ok(guard)
}
