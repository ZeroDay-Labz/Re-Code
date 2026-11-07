//! User configuration. A missing file means defaults. re-code never writes this file by itself.

use anyhow::{Context, Result};
use recode_providers::ollama::DEFAULT_URL;
use serde::Deserialize;
use std::path::Path;

#[derive(Debug, Default, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct Config {
    pub ollama: OllamaConfig,
    /// Colours: "auto" (suits the terminal; a theme picked with /theme wins), "dark", "light", or a
    /// theme's name such as "Catppuccin Mocha", which wins over /theme's choice.
    pub theme: String,
}

impl Config {
    /// The theme choice, checked.
    pub fn theme(&self) -> Result<recode_tui::background::ThemeChoice> {
        recode_tui::background::ThemeChoice::parse(&self.theme)
            .map_err(|error| anyhow::anyhow!("theme in config.toml: {error}"))
    }
}

#[derive(Debug, Deserialize)]
#[serde(default, deny_unknown_fields)]
pub struct OllamaConfig {
    pub url: String,
    /// Context size in tokens, sent with every request and kept stable for a session. Ollama silently drops
    /// the oldest content above this, and changing it reloads the model.
    pub num_ctx: u32,
}

impl Default for OllamaConfig {
    fn default() -> Self {
        Self { url: DEFAULT_URL.to_owned(), num_ctx: 16384 }
    }
}

impl Config {
    pub fn load(path: &Path) -> Result<Self> {
        match std::fs::read_to_string(path) {
            Ok(text) => toml::from_str(&text).with_context(|| format!("invalid configuration in {}", path.display())),
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => Ok(Self::default()),
            Err(error) => Err(error).with_context(|| format!("cannot read {}", path.display())),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn missing_file_gives_defaults() {
        let dir = tempfile::tempdir().unwrap();
        let config = Config::load(&dir.path().join("none.toml")).unwrap();
        assert_eq!(config.ollama.url, DEFAULT_URL);
    }

    #[test]
    fn values_are_read_and_mistakes_are_rejected() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("c.toml");
        std::fs::write(&path, "[ollama]\nurl = \"http://box:1\"\n").unwrap();
        assert_eq!(Config::load(&path).unwrap().ollama.url, "http://box:1");

        std::fs::write(&path, "theme = \"Nord\"\n").unwrap();
        assert!(matches!(Config::load(&path).unwrap().theme(), Ok(recode_tui::background::ThemeChoice::Named(_))));
        std::fs::write(&path, "theme = \"purple\"\n").unwrap();
        assert!(Config::load(&path).unwrap().theme().is_err(), "an unknown theme is reported");

        std::fs::write(&path, "[ollama]\nurll = \"typo\"\n").unwrap();
        let message = format!("{:#}", Config::load(&path).unwrap_err());
        assert!(message.contains("urll"), "{message}");
    }
}
