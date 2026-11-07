//! Read-only questions to systemd: which Quadlet files run a known image, and what state a unit is
//! in. Quadlet containers are usually removed when they stop, so `podman ps -a` can't see them;
//! their unit files can.

use crate::STEP_TIMEOUT;
use crate::known::{self, RuntimeKind};
use std::path::PathBuf;
use tokio::process::Command;

/// A unit's state, as reported by systemd.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnitState {
    Active,
    Inactive,
    NotFound,
}

/// A systemd unit that runs a known runtime.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RuntimeUnit {
    pub kind: RuntimeKind,
    pub unit: String,
    pub user: bool,
    /// The Quadlet file, when the unit comes from one.
    pub file: Option<PathBuf>,
    /// The pod named in the Quadlet file, if any.
    pub pod: Option<String>,
    pub state: UnitState,
}

/// Read-only access to the service manager, replaceable in tests.
pub trait Services: Sync {
    /// Every Quadlet `.container` file with its text.
    fn quadlets(&self) -> Vec<(PathBuf, String)>;
    fn unit_state(&self, unit: &str, user: bool) -> impl Future<Output = UnitState> + Send;
}

/// The real system.
pub struct SystemServices {
    pub user_config_root: PathBuf,
}

impl Services for SystemServices {
    fn quadlets(&self) -> Vec<(PathBuf, String)> {
        let dirs = [self.user_config_root.join("containers/systemd"), PathBuf::from("/etc/containers/systemd")];
        let mut found = Vec::new();
        for dir in dirs {
            let Ok(entries) = std::fs::read_dir(&dir) else { continue };
            for path in entries.flatten().map(|entry| entry.path()) {
                if path.extension().is_some_and(|extension| extension == "container")
                    && let Ok(text) = std::fs::read_to_string(&path)
                {
                    found.push((path, text));
                }
            }
        }
        found.sort();
        found
    }

    async fn unit_state(&self, unit: &str, user: bool) -> UnitState {
        let mut command = Command::new("systemctl");
        if user {
            command.arg("--user");
        }
        command.args(["show", unit, "--property=ActiveState,LoadState", "--value"]);
        // A wedged service manager must not hang the caller.
        match tokio::time::timeout(STEP_TIMEOUT, command.output()).await {
            Ok(Ok(output)) => parse_unit_state(&String::from_utf8_lossy(&output.stdout)),
            _ => UnitState::NotFound,
        }
    }
}

/// `systemctl show --value` prints one value per line, ordered by the unit's own property order,
/// so the values are matched by content.
#[must_use]
pub fn parse_unit_state(text: &str) -> UnitState {
    let values: Vec<&str> = text.lines().map(str::trim).collect();
    if values.contains(&"not-found") || values.iter().all(|value| value.is_empty()) {
        UnitState::NotFound
    } else if values.contains(&"active") || values.contains(&"activating") || values.contains(&"reloading") {
        UnitState::Active
    } else {
        UnitState::Inactive
    }
}

/// The value of `Key=` in a unit file, if present.
#[must_use]
pub fn quadlet_value(text: &str, key: &str) -> Option<String> {
    text.lines().find_map(|line| Some(line.trim().strip_prefix(key)?.trim_start().strip_prefix('=')?.trim().to_owned()))
}

/// What a Quadlet file runs, from its `Image=` line.
#[must_use]
pub fn quadlet_kind(text: &str) -> Option<RuntimeKind> {
    known::kind_of_image(&quadlet_value(text, "Image")?)
}

/// The units that run known runtimes: Quadlets first, then the conventional native unit names.
pub async fn find_units(services: &impl Services) -> Vec<RuntimeUnit> {
    let mut units = Vec::new();
    for (file, text) in services.quadlets() {
        let Some(kind) = quadlet_kind(&text) else { continue };
        let Some(stem) = file.file_stem().map(|stem| stem.to_string_lossy().into_owned()) else { continue };
        let unit = format!("{stem}.service");
        let user = !file.starts_with("/etc");
        let state = services.unit_state(&unit, user).await;
        units.push(RuntimeUnit { kind, unit, user, file: Some(file), pod: quadlet_value(&text, "Pod"), state });
    }
    for (unit, kind) in [("ollama.service", RuntimeKind::Ollama), ("llama-server.service", RuntimeKind::LlamaCpp)] {
        if units.iter().any(|u| u.unit == unit) {
            continue;
        }
        for user in [true, false] {
            let state = services.unit_state(unit, user).await;
            if state != UnitState::NotFound {
                units.push(RuntimeUnit { kind, unit: unit.to_owned(), user, file: None, pod: None, state });
                break;
            }
        }
    }
    units
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::HashMap;

    /// The Quadlet from the development machine, abridged.
    pub const QUADLET: &str =
        "[Container]\nImage=docker.io/ollama/ollama:rocm\nContainerName=ollama\nPod=local-ai.pod\n";

    #[derive(Default)]
    pub struct FakeServices {
        pub quadlets: Vec<(PathBuf, String)>,
        pub units: HashMap<(&'static str, bool), UnitState>,
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

    #[tokio::test]
    async fn quadlets_and_native_units_are_found_with_their_state() {
        let services = FakeServices {
            quadlets: vec![
                (PathBuf::from("/home/u/.config/containers/systemd/ollama.container"), QUADLET.to_owned()),
                (
                    PathBuf::from("/home/u/.config/containers/systemd/web.container"),
                    "[Container]\nImage=docker.io/library/caddy:2\n".to_owned(),
                ),
            ],
            units: HashMap::from([
                (("ollama.service", true), UnitState::Inactive),
                (("llama-server.service", false), UnitState::Active),
            ]),
        };
        let units = find_units(&services).await;
        assert_eq!(units.len(), 2, "{units:?}");
        assert_eq!(units[0].kind, RuntimeKind::Ollama);
        assert_eq!(units[0].pod.as_deref(), Some("local-ai.pod"));
        assert_eq!(units[0].state, UnitState::Inactive);
        assert!(units[0].user);
        assert_eq!(units[1].unit, "llama-server.service");
        assert!(!units[1].user);
    }

    #[test]
    fn systemd_output_is_understood() {
        assert_eq!(parse_unit_state("inactive\nloaded\n"), UnitState::Inactive);
        assert_eq!(parse_unit_state("loaded\nactive\n"), UnitState::Active);
        assert_eq!(parse_unit_state("inactive\nnot-found\n"), UnitState::NotFound);
        assert_eq!(parse_unit_state(""), UnitState::NotFound);
    }
}
