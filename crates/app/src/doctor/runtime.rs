//! Ollama's state at one address, without assuming how it was installed and without changing anything.
//!
//! The API is the only source of truth for models: Ollama's on-disk layout changes between versions.
//! When the API doesn't answer, the `recode-runtime` crate's view of systemd and Quadlet units says
//! whether it's installed but stopped. The wider picture (containers, other runtimes) is
//! [`recode_runtime::Report`], gathered alongside this.

use recode_core::capability::ModelCapabilities;
use recode_providers::ollama::{LoadedModel, OllamaProvider};
use recode_providers::{ModelInfo, Provider, ProviderError};
use recode_runtime::RuntimeKind;
use recode_runtime::systemd::{Services, UnitState, find_units};
use serde::Serialize;
use std::path::{Path, PathBuf};

/// What is known about Ollama on this machine.
#[derive(Debug, PartialEq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum OllamaStatus {
    Running {
        url: String,
        version: String,
        models: Vec<ModelEntry>,
        loaded: Vec<LoadedModel>,
    },
    /// Installed, and not answering. re-code never starts it without asking.
    InstalledStopped {
        url: String,
        install: Installation,
    },
    /// A service claims to be active, but its API doesn't answer.
    ActiveNotAnswering {
        url: String,
        install: Installation,
        error: String,
    },
    /// Something answered, but with an error or in an unexpected format.
    Unhealthy {
        url: String,
        error: String,
    },
    Absent {
        url: String,
        error: String,
    },
}

/// A model, and what is declared about it. Declared tool support hasn't been tested.
#[derive(Debug, PartialEq, Serialize)]
pub struct ModelEntry {
    pub info: ModelInfo,
    pub capabilities: Result<ModelCapabilities, String>,
}

/// How an Ollama installation was found.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Installation {
    Quadlet { unit: String, file: PathBuf, pod: Option<String> },
    SystemdUnit { unit: String, user: bool },
    Binary,
}

impl Installation {
    /// The command the user could run. It is shown and never executed from here.
    pub fn start_command(&self) -> String {
        match self {
            Self::Quadlet { unit, .. } | Self::SystemdUnit { unit, user: true } => {
                format!("systemctl --user start {unit}")
            }
            Self::SystemdUnit { unit, user: false } => format!("sudo systemctl start {unit}"),
            Self::Binary => "ollama serve".to_owned(),
        }
    }
}

/// Reports Ollama's state at the given address. `binary_on_path` says whether the `ollama` program
/// is installed natively, for when nothing else is found.
pub async fn discover(url: &str, services: &impl Services, binary_on_path: bool) -> OllamaStatus {
    let provider = match OllamaProvider::new(url) {
        Ok(provider) => provider,
        Err(error) => return OllamaStatus::Unhealthy { url: url.to_owned(), error: error.to_string() },
    };
    let error = match provider.health().await {
        Ok(health) => return describe_running(&provider, health.version).await,
        Err(error) => error,
    };
    if !matches!(error, ProviderError::Unreachable { .. }) {
        return OllamaStatus::Unhealthy { url: url.to_owned(), error: error.to_string() };
    }
    match find_installation(services, binary_on_path).await {
        Some((install, UnitState::Active)) => {
            OllamaStatus::ActiveNotAnswering { url: url.to_owned(), install, error: error.to_string() }
        }
        Some((install, _)) => OllamaStatus::InstalledStopped { url: url.to_owned(), install },
        None => OllamaStatus::Absent { url: url.to_owned(), error: error.to_string() },
    }
}

async fn describe_running(provider: &OllamaProvider, version: String) -> OllamaStatus {
    let url = provider.base_url().to_owned();
    let models = match provider.list_models().await {
        Ok(models) => models,
        Err(error) => return OllamaStatus::Unhealthy { url, error: error.to_string() },
    };
    let mut entries = Vec::with_capacity(models.len());
    for info in models {
        let capabilities = provider.capabilities(&info.name).await.map_err(|error| error.to_string());
        entries.push(ModelEntry { info, capabilities });
    }
    entries.sort_by(|a, b| a.info.name.cmp(&b.info.name));
    // A failure here only loses the memory display, so it shouldn't hide everything else.
    let loaded = provider.loaded_models().await.unwrap_or_default();
    OllamaStatus::Running { url, version, models: entries, loaded }
}

async fn find_installation(services: &impl Services, binary_on_path: bool) -> Option<(Installation, UnitState)> {
    let unit = find_units(services).await.into_iter().find(|unit| unit.kind == RuntimeKind::Ollama);
    if let Some(unit) = unit {
        let install = match unit.file {
            Some(file) => Installation::Quadlet { unit: unit.unit, file, pod: unit.pod },
            None => Installation::SystemdUnit { unit: unit.unit, user: unit.user },
        };
        return Some((install, unit.state));
    }
    binary_on_path.then_some((Installation::Binary, UnitState::NotFound))
}

/// Used by rendering, so the file's location can be shown relative to home.
pub fn display_path(path: &Path) -> String {
    match std::env::var_os("HOME").map(PathBuf::from) {
        Some(home) if path.starts_with(&home) => {
            format!("~/{}", path.strip_prefix(&home).unwrap_or(path).display())
        }
        _ => path.display().to_string(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// The Quadlet from the development machine, abridged.
    const QUADLET: &str = "[Container]\nImage=docker.io/ollama/ollama:rocm\nContainerName=ollama\nPod=local-ai.pod\n";

    #[derive(Default)]
    struct FakeServices {
        quadlets: Vec<(PathBuf, String)>,
        units: HashMap<(&'static str, bool), UnitState>,
    }

    impl Services for FakeServices {
        fn quadlets(&self) -> Vec<(PathBuf, String)> {
            self.quadlets.clone()
        }
        async fn unit_state(&self, unit: &str, user: bool) -> UnitState {
            self.units
                .iter()
                .find(|((name, is_user), _)| *name == unit && *is_user == user)
                .map_or(UnitState::NotFound, |(_, state)| *state)
        }
    }

    const NOTHING_THERE: &str = "http://127.0.0.1:9";

    fn quadlet_machine(state: UnitState) -> FakeServices {
        FakeServices {
            quadlets: vec![(PathBuf::from("/home/u/.config/containers/systemd/ollama.container"), QUADLET.to_owned())],
            units: HashMap::from([(("ollama.service", true), state)]),
        }
    }

    #[tokio::test]
    async fn stopped_quadlet_is_reported_as_installed_with_its_pod() {
        let status = discover(NOTHING_THERE, &quadlet_machine(UnitState::Inactive), false).await;
        let OllamaStatus::InstalledStopped { install, .. } = status else { panic!("{status:?}") };
        assert_eq!(install.start_command(), "systemctl --user start ollama.service");
        assert!(matches!(install, Installation::Quadlet { pod: Some(pod), .. } if pod == "local-ai.pod"));
    }

    #[tokio::test]
    async fn active_service_that_does_not_answer_is_its_own_state() {
        let status = discover(NOTHING_THERE, &quadlet_machine(UnitState::Active), false).await;
        assert!(matches!(status, OllamaStatus::ActiveNotAnswering { .. }), "{status:?}");
    }

    #[tokio::test]
    async fn unrelated_quadlets_are_ignored() {
        let services = FakeServices {
            quadlets: vec![(
                PathBuf::from("/x/web.container"),
                "[Container]\nImage=docker.io/library/caddy:2\n".to_owned(),
            )],
            ..FakeServices::default()
        };
        assert!(matches!(discover(NOTHING_THERE, &services, false).await, OllamaStatus::Absent { .. }));
    }

    #[tokio::test]
    async fn native_service_and_bare_binary_are_found() {
        let services = FakeServices {
            units: HashMap::from([(("ollama.service", false), UnitState::Inactive)]),
            ..FakeServices::default()
        };
        let status = discover(NOTHING_THERE, &services, false).await;
        let OllamaStatus::InstalledStopped { install, .. } = status else { panic!("{status:?}") };
        assert_eq!(install.start_command(), "sudo systemctl start ollama.service");

        let status = discover(NOTHING_THERE, &FakeServices::default(), true).await;
        assert!(matches!(status, OllamaStatus::InstalledStopped { install: Installation::Binary, .. }), "{status:?}");
    }
}
