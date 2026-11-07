//! Where re-code keeps its files. Everything follows the XDG base directory conventions, and
//! nothing requires root.

use anyhow::{Context, Result};
use etcetera::{BaseStrategy, choose_base_strategy};
use serde::Serialize;
use std::path::PathBuf;

const APP_DIR: &str = "re-code";

#[derive(Debug, Clone, Serialize)]
pub struct Paths {
    pub config_file: PathBuf,
    pub extensions_file: PathBuf,
    pub skills_dir: PathBuf,
    /// The user's own themes, one TOML file each.
    pub themes_dir: PathBuf,
    pub database: PathBuf,
    pub log_dir: PathBuf,
    pub cache_dir: PathBuf,
    pub data_dir: PathBuf,
    /// The user's config directory itself, used to find Quadlet units.
    pub user_config_root: PathBuf,
}

impl Paths {
    pub fn from_environment() -> Result<Self> {
        let base = choose_base_strategy().context("cannot determine the home directory")?;
        let data_dir = base.data_dir().join(APP_DIR);
        // `state_dir` is None on platforms without the concept, so fall back to the data directory.
        let log_dir = base.state_dir().unwrap_or_else(|| data_dir.clone()).join(APP_DIR).join("logs");
        Ok(Self {
            config_file: base.config_dir().join(APP_DIR).join("config.toml"),
            cache_dir: base.cache_dir().join(APP_DIR),
            extensions_file: base.config_dir().join(APP_DIR).join("extensions.toml"),
            skills_dir: base.config_dir().join(APP_DIR).join("skills"),
            themes_dir: base.config_dir().join(APP_DIR).join("themes"),
            database: data_dir.join("re-code.sqlite3"),
            log_dir,
            user_config_root: base.config_dir(),
            data_dir,
        })
    }
}
