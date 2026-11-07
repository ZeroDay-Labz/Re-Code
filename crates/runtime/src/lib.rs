//! Finds the inference runtimes on this machine and reports how each one is run, without changing
//! anything. Every step is read-only and time-boxed, and a failed step reports what it couldn't learn
//! rather than guessing.
//!
//! The order follows `docs/HARDWARE_AND_RUNTIME.md`: probe addresses and identify each service by
//! what it answers (never by its port number); list containers and recognise known images; read
//! systemd and Quadlet units; look for native processes and binaries. The pieces are then matched
//! up, so "Ollama 0.40 at :11434, served by the podman container `ollama` in pod `local-ai`, GPU
//! attached" is one entry rather than four.
//!
//! Starting a runtime is a separate, explicit action ([`StartMethod`] says how); stopping,
//! restarting or reconfiguring a user-managed runtime has no representation here at all.

pub mod containers;
pub mod known;
pub mod probe;
pub mod processes;
pub mod start;
pub mod systemd;

use serde::Serialize;
use std::collections::BTreeSet;
use std::path::PathBuf;
use std::time::Duration;

pub use known::RuntimeKind;

/// How long any one external step (an HTTP probe, a `podman` call, a `systemctl` call) may take.
pub const STEP_TIMEOUT: Duration = Duration::from_secs(3);

/// A container engine.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Engine {
    Podman,
    Docker,
}

impl Engine {
    #[must_use]
    pub fn command(self) -> &'static str {
        match self {
            Self::Podman => "podman",
            Self::Docker => "docker",
        }
    }
}

/// Whether an engine is installed, and what it said about itself.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct EngineInfo {
    pub engine: Engine,
    /// `None` when the engine is on `PATH` but didn't answer in time.
    pub version: Option<String>,
    pub rootless: Option<bool>,
}

/// Who manages a runtime. re-code only ever starts, stops or changes what it created itself.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum Ownership {
    UserManaged,
    AppManaged,
}

/// The one thing re-code may do to a stopped runtime, once the user has said yes to this exact command.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum StartMethod {
    SystemdUser { unit: String },
    SystemdSystem { unit: String },
    ContainerStart { engine: Engine, name: String },
    PodStart { engine: Engine, pod: String },
    Binary { argv: Vec<String> },
}

impl StartMethod {
    /// The exact command, for showing before asking and for running after.
    #[must_use]
    pub fn argv(&self) -> Vec<String> {
        match self {
            Self::SystemdUser { unit } => vec!["systemctl".into(), "--user".into(), "start".into(), unit.clone()],
            Self::SystemdSystem { unit } => vec!["sudo".into(), "systemctl".into(), "start".into(), unit.clone()],
            Self::ContainerStart { engine, name } => vec![engine.command().into(), "start".into(), name.clone()],
            Self::PodStart { engine, pod } => vec![engine.command().into(), "pod".into(), "start".into(), pod.clone()],
            Self::Binary { argv } => argv.clone(),
        }
    }

    /// The command as the user would type it.
    #[must_use]
    pub fn command_line(&self) -> String {
        self.argv().join(" ")
    }

    /// Whether the command needs root. Shown so the password prompt isn't a surprise.
    #[must_use]
    pub fn needs_root(&self) -> bool {
        matches!(self, Self::SystemdSystem { .. })
    }
}

/// What runs a runtime, when it could be worked out.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum ServedBy {
    Container { engine: Engine, name: String, pod: Option<String>, image: String, gpu: GpuAccess },
    SystemdUnit { unit: String, user: bool, file: Option<PathBuf> },
    Process { pid: u32, exe: String },
}

/// Whether a container was given GPU devices. Read from how it was created, never assumed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum GpuAccess {
    /// The devices or CDI names it was given, such as `/dev/kfd` or `nvidia.com/gpu=all`.
    Yes {
        devices: Vec<String>,
    },
    No,
    Unknown,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case", tag = "state")]
pub enum RuntimeState {
    /// Answered at `url` and was identified by its response.
    Running { version: Option<String> },
    /// Found but not running. `start` is `None` when re-code doesn't know a safe way to start it.
    InstalledStopped { start: Option<StartMethod> },
    /// Its service or container is running, but the API didn't answer where expected.
    ActiveNotAnswering { error: String },
}

/// One inference runtime, with everything learnt about it merged into one entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Runtime {
    /// Stable within a report: the kind plus what serves it, so the interface can refer back to it.
    pub id: String,
    pub kind: RuntimeKind,
    /// Where its API answers, or is expected to.
    pub url: Option<String>,
    pub state: RuntimeState,
    pub ownership: Ownership,
    pub served_by: Option<ServedBy>,
    /// Facts worth showing that don't fit the fields: "also publishes :3000", "needs a model to start".
    pub notes: Vec<String>,
}

/// Something answered at an address but wasn't recognised. Reported, never guessed at.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Unidentified {
    pub url: String,
    pub summary: String,
}

/// A container on this machine, whatever it runs.
#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Container {
    pub engine: Engine,
    pub id: String,
    pub name: String,
    pub image: String,
    pub running: bool,
    /// The engine's own words, such as "Up 22 hours" or "Exited (0) 2 hours ago".
    pub status: String,
    pub pod: Option<String>,
    pub ports: Vec<PortMap>,
    /// The systemd unit that manages it (Quadlet sets `PODMAN_SYSTEMD_UNIT`).
    pub systemd_unit: Option<String>,
    pub gpu: GpuAccess,
    pub is_infra: bool,
    /// What the image is known to run, if anything.
    pub runs: Option<RuntimeKind>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct PortMap {
    pub host_ip: String,
    pub host_port: u16,
    pub container_port: u16,
    pub protocol: String,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize)]
pub struct Pod {
    pub engine: Engine,
    pub id: String,
    pub name: String,
    pub running: bool,
    pub systemd_unit: Option<String>,
    pub containers: Vec<String>,
}

/// Everything found, in one place.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize)]
pub struct Report {
    pub engines: Vec<EngineInfo>,
    pub pods: Vec<Pod>,
    pub containers: Vec<Container>,
    pub runtimes: Vec<Runtime>,
    pub unidentified: Vec<Unidentified>,
    /// Things that look wrong: two of the same runtime, a container whose port nothing answers on.
    pub warnings: Vec<String>,
    /// Steps that couldn't run, in plain words ("podman didn't answer within 3 s").
    pub problems: Vec<String>,
}

impl Report {
    /// The running runtime of a kind, if there is exactly one answering.
    #[must_use]
    pub fn running(&self, kind: RuntimeKind) -> Option<&Runtime> {
        self.runtimes.iter().find(|r| r.kind == kind && matches!(r.state, RuntimeState::Running { .. }))
    }
}

/// What to look at. Everything here is read-only input to discovery.
#[derive(Debug, Clone)]
pub struct Scan {
    /// Addresses to probe first, such as the configured Ollama URL and `OLLAMA_HOST`.
    pub urls: Vec<String>,
    /// Loopback ports to probe as well as the configured addresses.
    pub ports: Vec<u16>,
    /// Where user Quadlet files live (`~/.config`).
    pub user_config_root: PathBuf,
    /// Container and unit names with this prefix are re-code's own.
    pub app_prefix: String,
}

impl Scan {
    /// The usual scan: the given address, `OLLAMA_HOST`, and the ports the common runtimes default to.
    #[must_use]
    pub fn default_for(configured_url: &str, user_config_root: PathBuf) -> Self {
        let mut urls = vec![configured_url.to_owned()];
        if let Some(host) = std::env::var_os("OLLAMA_HOST") {
            urls.push(host.to_string_lossy().into_owned());
        }
        Self { urls, ports: known::DEFAULT_PORTS.to_vec(), user_config_root, app_prefix: "re-code-".to_owned() }
    }
}

/// Runs every step and merges what they found.
pub async fn discover(scan: &Scan) -> Report {
    let system = systemd::SystemServices { user_config_root: scan.user_config_root.clone() };
    discover_with(scan, &system, containers::RealEngines, processes::RealProcesses).await
}

/// [`discover`] with the system parts replaceable, for tests.
pub async fn discover_with(
    scan: &Scan,
    services: &impl systemd::Services,
    engines: impl containers::Engines,
    procs: impl processes::Processes,
) -> Report {
    let mut report = Report::default();

    // 1. Addresses, identified by their answers.
    let mut urls: Vec<String> = Vec::new();
    for raw in &scan.urls {
        if let Some(url) = probe::normalize_url(raw)
            && !urls.iter().any(|known| probe::same_endpoint(known, &url))
        {
            urls.push(url);
        }
    }
    for port in &scan.ports {
        let url = format!("http://127.0.0.1:{port}");
        if !urls.iter().any(|known| probe::same_endpoint(known, &url)) {
            urls.push(url);
        }
    }
    let probes = futures_util::future::join_all(urls.iter().map(|url| probe::identify(url))).await;
    for (url, outcome) in urls.iter().zip(probes) {
        match outcome {
            probe::Outcome::Identified { kind, version } => report.runtimes.push(Runtime {
                id: format!("{}@{url}", kind.slug()),
                kind,
                url: Some(url.clone()),
                state: RuntimeState::Running { version },
                ownership: Ownership::UserManaged,
                served_by: None,
                notes: Vec::new(),
            }),
            probe::Outcome::Unidentified { summary } => {
                report.unidentified.push(Unidentified { url: url.clone(), summary });
            }
            probe::Outcome::Silent => {}
        }
    }

    // 2. Containers and pods. A running container with a known image is probed on the ports it
    // publishes too, since those needn't be the usual ones.
    let found = engines.list().await;
    report.engines = found.engines;
    report.pods = found.pods;
    report.containers = found.containers;
    report.problems.extend(found.problems);
    let mut extra: Vec<String> = Vec::new();
    for container in report.containers.iter().filter(|c| c.running && c.runs.is_some()) {
        for port in &container.ports {
            let url = format!("http://127.0.0.1:{}", port.host_port);
            if !urls.iter().chain(extra.iter()).any(|known| probe::same_endpoint(known, &url)) {
                extra.push(url);
            }
        }
    }
    let probes = futures_util::future::join_all(extra.iter().map(|url| probe::identify(url))).await;
    for (url, outcome) in extra.iter().zip(probes) {
        if let probe::Outcome::Identified { kind, version } = outcome {
            report.runtimes.push(Runtime {
                id: format!("{}@{url}", kind.slug()),
                kind,
                url: Some(url.clone()),
                state: RuntimeState::Running { version },
                ownership: Ownership::UserManaged,
                served_by: None,
                notes: Vec::new(),
            });
        }
    }
    let pods = report.pods.clone();
    for container in report.containers.clone() {
        let Some(kind) = container.runs else { continue };
        let ownership = ownership_of(&container.name, &scan.app_prefix);
        let served_by = ServedBy::Container {
            engine: container.engine,
            name: container.name.clone(),
            pod: container.pod.clone(),
            image: container.image.clone(),
            gpu: container.gpu.clone(),
        };
        let pod_unit = container
            .pod
            .as_ref()
            .and_then(|name| pods.iter().find(|p| &p.name == name))
            .and_then(|p| p.systemd_unit.clone());
        if container.running {
            // Match the answering runtime by a published host port.
            let ports: BTreeSet<u16> = container.ports.iter().map(|p| p.host_port).collect();
            let matched = report.runtimes.iter_mut().find(|r| {
                r.kind == kind
                    && r.served_by.is_none()
                    && r.url.as_deref().and_then(probe::port_of).is_some_and(|port| ports.contains(&port))
            });
            if let Some(runtime) = matched {
                runtime.served_by = Some(served_by);
                runtime.ownership = ownership;
                runtime.id = format!("{}@{}:{}", kind.slug(), container.engine.command(), container.name);
            } else {
                let where_ = if ports.is_empty() {
                    "it publishes no ports, so its API can't be reached from here".to_owned()
                } else {
                    format!("nothing recognisable answered on its published port(s) {}", join_ports(&ports))
                };
                report.runtimes.push(Runtime {
                    id: format!("{}@{}:{}", kind.slug(), container.engine.command(), container.name),
                    kind,
                    url: ports.first().map(|port| format!("http://127.0.0.1:{port}")),
                    state: RuntimeState::ActiveNotAnswering {
                        error: format!(
                            "the {} container `{}` is running, but {where_}",
                            container.engine.command(),
                            container.name
                        ),
                    },
                    ownership,
                    served_by: Some(served_by),
                    notes: Vec::new(),
                });
            }
        } else {
            let start = match (&container.systemd_unit, &container.pod, pod_unit) {
                (Some(unit), _, _) => StartMethod::SystemdUser { unit: unit.clone() },
                (None, Some(_), Some(unit)) => StartMethod::SystemdUser { unit },
                (None, Some(pod), None) => StartMethod::PodStart { engine: container.engine, pod: pod.clone() },
                (None, None, _) => {
                    StartMethod::ContainerStart { engine: container.engine, name: container.name.clone() }
                }
            };
            let mut notes = Vec::new();
            if let Some(pod) = &container.pod {
                notes.push(format!("Starting it also starts the other containers in pod `{pod}`."));
            }
            report.runtimes.push(Runtime {
                id: format!("{}@{}:{}", kind.slug(), container.engine.command(), container.name),
                kind,
                url: container.ports.first().map(|p| format!("http://127.0.0.1:{}", p.host_port)),
                state: RuntimeState::InstalledStopped { start: Some(start) },
                ownership,
                served_by: Some(served_by),
                notes,
            });
        }
    }

    // 3. systemd and Quadlet units that no container accounted for.
    let claimed_units: BTreeSet<String> = report
        .containers
        .iter()
        .filter_map(|c| c.systemd_unit.clone())
        .chain(report.pods.iter().filter_map(|p| p.systemd_unit.clone()))
        .collect();
    for unit in systemd::find_units(services).await {
        if claimed_units.contains(&unit.unit) {
            continue;
        }
        let served_by = ServedBy::SystemdUnit { unit: unit.unit.clone(), user: unit.user, file: unit.file.clone() };
        let ownership = ownership_of(&unit.unit, &scan.app_prefix);
        let id = format!("{}@systemd:{}", unit.kind.slug(), unit.unit);
        match unit.state {
            systemd::UnitState::Active => {
                // An active unit should be one of the answering runtimes of its kind.
                if let Some(runtime) = report.runtimes.iter_mut().find(|r| r.kind == unit.kind && r.served_by.is_none())
                {
                    runtime.served_by = Some(served_by);
                    runtime.ownership = ownership;
                    runtime.id = id;
                } else {
                    report.runtimes.push(Runtime {
                        id,
                        kind: unit.kind,
                        url: None,
                        state: RuntimeState::ActiveNotAnswering {
                            error: format!(
                                "{} is active, but no {} API answered at the addresses probed",
                                unit.unit,
                                unit.kind.name()
                            ),
                        },
                        ownership,
                        served_by: Some(served_by),
                        notes: Vec::new(),
                    });
                }
            }
            systemd::UnitState::Inactive => {
                let start = if unit.user {
                    StartMethod::SystemdUser { unit: unit.unit.clone() }
                } else {
                    StartMethod::SystemdSystem { unit: unit.unit.clone() }
                };
                report.runtimes.push(Runtime {
                    id,
                    kind: unit.kind,
                    url: None,
                    state: RuntimeState::InstalledStopped { start: Some(start) },
                    ownership,
                    served_by: Some(served_by),
                    notes: Vec::new(),
                });
            }
            systemd::UnitState::NotFound => {}
        }
    }

    // 4. Native processes and binaries.
    let found = procs.list();
    for process in found {
        if process.container.is_some() {
            // The container step already covers it; a container's process isn't a second runtime.
            continue;
        }
        let Some(kind) = known::kind_of_binary(&process.exe) else { continue };
        let served_by = ServedBy::Process { pid: process.pid, exe: process.exe.clone() };
        if let Some(runtime) = report.runtimes.iter_mut().find(|r| r.kind == kind && r.served_by.is_none()) {
            runtime.served_by = Some(served_by);
            runtime.id = format!("{}@pid:{}", kind.slug(), process.pid);
        } else if report.runtimes.iter().any(|r| r.kind == kind && matches!(r.state, RuntimeState::Running { .. })) {
            report.warnings.push(format!(
                "Two {}s seem to be running: process {} ({}) as well as the one already found. Only the one answering is used.",
                kind.name(),
                process.pid,
                process.exe
            ));
        } else {
            report.runtimes.push(Runtime {
                id: format!("{}@pid:{}", kind.slug(), process.pid),
                kind,
                url: None,
                state: RuntimeState::ActiveNotAnswering {
                    error: format!(
                        "process {} ({}) is running, but no {} API answered at the addresses probed",
                        process.pid,
                        process.exe,
                        kind.name()
                    ),
                },
                ownership: Ownership::UserManaged,
                served_by: Some(served_by),
                notes: Vec::new(),
            });
        }
    }
    for (binary, kind) in known::BINARIES {
        if report.runtimes.iter().any(|r| r.kind == *kind) || !procs.on_path(binary) {
            continue;
        }
        let (start, note) = match kind {
            RuntimeKind::Ollama => (Some(StartMethod::Binary { argv: vec!["ollama".into(), "serve".into()] }), None),
            _ => (None, Some(format!("`{binary}` is on your PATH, but it needs a model to start; start it yourself."))),
        };
        report.runtimes.push(Runtime {
            id: format!("{}@path:{binary}", kind.slug()),
            kind: *kind,
            url: None,
            state: RuntimeState::InstalledStopped { start },
            ownership: Ownership::UserManaged,
            served_by: None,
            notes: note.into_iter().collect(),
        });
    }

    for runtime in &report.runtimes {
        if let (RuntimeState::Running { .. }, None) = (&runtime.state, &runtime.served_by) {
            report.warnings.push(format!(
                "{} answers at {}, but nothing on this machine was found serving it. It may run elsewhere, or in a way re-code doesn't recognise.",
                runtime.kind.name(),
                runtime.url.as_deref().unwrap_or("?")
            ));
        }
    }
    report
}

fn ownership_of(name: &str, app_prefix: &str) -> Ownership {
    if name.starts_with(app_prefix) { Ownership::AppManaged } else { Ownership::UserManaged }
}

fn join_ports(ports: &BTreeSet<u16>) -> String {
    ports.iter().map(u16::to_string).collect::<Vec<_>>().join(", ")
}
