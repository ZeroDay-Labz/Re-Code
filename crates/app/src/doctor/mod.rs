//! `re-code doctor`: reports what is actually on this machine.
//!
//! Every probe is read-only and independent. A failed probe reports "unknown" rather than
//! guessing or stopping the others. This code moves to a `runtime` crate when the runtime manager
//! is built, which is when a second caller will exist.

pub mod hardware;
pub mod render;
pub mod runtime;

use crate::paths::Paths;
use recode_store::Store;
use serde::Serialize;

#[derive(Debug, Serialize)]
pub struct Report {
    pub version: &'static str,
    pub system: hardware::System,
    pub cpu: hardware::Cpu,
    pub memory: hardware::Memory,
    pub gpus: Vec<hardware::Gpu>,
    pub compute: hardware::Compute,
    pub disk: Option<hardware::Disk>,
    pub tools: hardware::Tools,
    pub terminal: hardware::Terminal,
    pub ollama: runtime::OllamaStatus,
    /// Every inference runtime found, how each is run, and the containers on this machine.
    pub runtimes: recode_runtime::Report,
    pub storage: Storage,
}

#[derive(Debug, Serialize)]
pub struct Storage {
    pub paths: Paths,
    pub database: Result<u32, String>,
}

/// Runs every probe.
pub async fn gather(paths: &Paths, ollama_url: &str) -> Report {
    let database =
        Store::open(&paths.database).and_then(|store| store.schema_version()).map_err(|error| error.to_string());
    let services = recode_runtime::systemd::SystemServices { user_config_root: paths.user_config_root.clone() };
    let scan = recode_runtime::Scan::default_for(ollama_url, paths.user_config_root.clone());
    let (ollama, runtimes) = tokio::join!(
        runtime::discover(ollama_url, &services, hardware::on_path("ollama")),
        recode_runtime::discover(&scan)
    );
    Report {
        version: env!("CARGO_PKG_VERSION"),
        system: hardware::system(),
        cpu: hardware::cpu(),
        memory: hardware::memory(),
        gpus: hardware::gpus(),
        compute: hardware::compute(),
        disk: hardware::disk(&paths.data_dir),
        tools: hardware::tools(),
        terminal: hardware::terminal(),
        ollama,
        runtimes,
        storage: Storage { paths: paths.clone(), database },
    }
}
