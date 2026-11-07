//! Lists containers and pods through the engines' own command-line tools. The CLIs work rootless
//! without an API socket (podman's socket is usually off), and `--format json` is a stable contract.
//! Nothing here starts, stops or changes a container.

use crate::known::{self, RuntimeKind};
use crate::{Container, Engine, EngineInfo, GpuAccess, Pod, PortMap, STEP_TIMEOUT};
use serde::Deserialize;
use std::collections::HashMap;
use tokio::process::Command;

/// What the engines reported.
#[derive(Debug, Default)]
pub struct Found {
    pub engines: Vec<EngineInfo>,
    pub pods: Vec<Pod>,
    pub containers: Vec<Container>,
    pub problems: Vec<String>,
}

/// Access to the container engines, replaceable in tests.
pub trait Engines {
    fn list(self) -> impl Future<Output = Found> + Send;
}

pub struct RealEngines;

impl Engines for RealEngines {
    async fn list(self) -> Found {
        let mut found = Found::default();
        if on_path("podman") {
            podman(&mut found).await;
        }
        if on_path("docker") {
            docker(&mut found).await;
        }
        found
    }
}

fn on_path(program: &str) -> bool {
    crate::processes::Processes::on_path(&crate::processes::RealProcesses, program)
}

/// Runs an engine command read-only, with the step timeout. `None` when it failed or hung.
async fn run(engine: Engine, args: &[&str]) -> Result<String, String> {
    let mut command = Command::new(engine.command());
    command.args(args);
    match tokio::time::timeout(STEP_TIMEOUT, command.output()).await {
        Ok(Ok(output)) if output.status.success() => Ok(String::from_utf8_lossy(&output.stdout).into_owned()),
        Ok(Ok(output)) => Err(format!(
            "`{} {}` failed: {}",
            engine.command(),
            args.join(" "),
            String::from_utf8_lossy(&output.stderr).lines().next().unwrap_or("no message")
        )),
        Ok(Err(error)) => Err(format!("`{}` couldn't run: {error}", engine.command())),
        Err(_) => {
            Err(format!("`{} {}` didn't answer within {} s", engine.command(), args.join(" "), STEP_TIMEOUT.as_secs()))
        }
    }
}

async fn podman(found: &mut Found) {
    let version = run(Engine::Podman, &["version", "--format", "json"]).await.ok();
    found.engines.push(EngineInfo {
        engine: Engine::Podman,
        version: version.as_deref().and_then(parse_podman_version),
        rootless: version.as_deref().and_then(|_| rootless_hint()),
    });
    match run(Engine::Podman, &["pod", "ps", "--format", "json"]).await {
        Ok(text) => match parse_podman_pods(&text) {
            Ok(pods) => found.pods.extend(pods),
            Err(error) => found.problems.push(format!("couldn't read `podman pod ps` output: {error}")),
        },
        Err(problem) => found.problems.push(problem),
    }
    match run(Engine::Podman, &["ps", "-a", "--format", "json"]).await {
        Ok(text) => {
            let mut containers = match parse_podman_ps(&text, &found.pods) {
                Ok(containers) => containers,
                Err(error) => {
                    found.problems.push(format!("couldn't read `podman ps` output: {error}"));
                    Vec::new()
                }
            };
            // How each known container was created tells whether it got GPU devices.
            let ids: Vec<&str> = containers.iter().filter(|c| c.runs.is_some()).map(|c| c.id.as_str()).collect();
            if !ids.is_empty() {
                let mut args = vec!["inspect", "--format", "json"];
                args.extend(ids.iter().copied());
                match run(Engine::Podman, &args).await {
                    Ok(text) => {
                        let gpu = parse_inspect_gpu(&text);
                        for container in &mut containers {
                            if let Some(access) = gpu.get(&container.id) {
                                container.gpu = access.clone();
                            }
                        }
                    }
                    Err(problem) => found.problems.push(problem),
                }
            }
            found.containers.extend(containers);
        }
        Err(problem) => found.problems.push(problem),
    }
}

async fn docker(found: &mut Found) {
    let version = run(Engine::Docker, &["version", "--format", "{{json .}}"]).await;
    let version = match version {
        Ok(text) => serde_json::from_str::<serde_json::Value>(&text)
            .ok()
            .and_then(|v| v.pointer("/Server/Version").and_then(|v| v.as_str()).map(str::to_owned)),
        Err(problem) => {
            // The docker CLI without a reachable daemon is the common case on a podman machine.
            found.problems.push(problem);
            None
        }
    };
    found.engines.push(EngineInfo { engine: Engine::Docker, version: version.clone(), rootless: None });
    if version.is_none() {
        return;
    }
    match run(Engine::Docker, &["ps", "-a", "--no-trunc", "--format", "{{json .}}"]).await {
        Ok(text) => {
            let mut containers = parse_docker_ps(&text);
            let ids: Vec<&str> = containers.iter().filter(|c| c.runs.is_some()).map(|c| c.id.as_str()).collect();
            if !ids.is_empty() {
                let mut args = vec!["inspect", "--format", "{{json .}}"];
                args.extend(ids.iter().copied());
                if let Ok(text) = run(Engine::Docker, &args).await {
                    let gpu = parse_inspect_gpu(&text);
                    for container in &mut containers {
                        if let Some(access) = gpu.get(&container.id) {
                            container.gpu = access.clone();
                        }
                    }
                }
            }
            found.containers.extend(containers);
        }
        Err(problem) => found.problems.push(problem),
    }
}

fn rootless_hint() -> Option<bool> {
    // Rootless podman keeps its state under the user's runtime directory.
    std::env::var_os("XDG_RUNTIME_DIR").map(|dir| std::path::Path::new(&dir).join("containers").exists())
}

fn parse_podman_version(text: &str) -> Option<String> {
    let value: serde_json::Value = serde_json::from_str(text).ok()?;
    value.pointer("/Client/Version").and_then(|v| v.as_str()).map(str::to_owned)
}

#[derive(Deserialize)]
struct PodmanPod {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Name")]
    name: String,
    #[serde(rename = "Status", default)]
    status: String,
    #[serde(rename = "Labels", default)]
    labels: HashMap<String, String>,
    #[serde(rename = "Containers", default)]
    containers: Vec<PodmanPodContainer>,
}

#[derive(Deserialize)]
struct PodmanPodContainer {
    #[serde(rename = "Names", default)]
    names: String,
}

/// `podman pod ps --format json`.
pub fn parse_podman_pods(text: &str) -> Result<Vec<Pod>, String> {
    let pods = serde_json::from_str::<Vec<PodmanPod>>(text).map_err(|error| error.to_string())?;
    Ok(pods
        .into_iter()
        .map(|pod| Pod {
            engine: Engine::Podman,
            id: pod.id,
            name: pod.name,
            running: pod.status.eq_ignore_ascii_case("running") || pod.status.eq_ignore_ascii_case("degraded"),
            systemd_unit: pod.labels.get("PODMAN_SYSTEMD_UNIT").cloned(),
            containers: pod.containers.into_iter().map(|c| c.names).filter(|n| !n.is_empty()).collect(),
        })
        .collect())
}

#[derive(Deserialize)]
struct PodmanContainer {
    #[serde(rename = "Id")]
    id: String,
    #[serde(rename = "Names", default)]
    names: Vec<String>,
    #[serde(rename = "Image", default)]
    image: String,
    #[serde(rename = "State", default)]
    state: String,
    #[serde(rename = "Status", default)]
    status: String,
    #[serde(rename = "Pod", default)]
    pod: String,
    #[serde(rename = "PodName", default)]
    pod_name: String,
    #[serde(rename = "Ports", default)]
    ports: Option<Vec<PodmanPort>>,
    #[serde(rename = "Labels", default)]
    labels: Option<HashMap<String, String>>,
    #[serde(rename = "IsInfra", default)]
    is_infra: bool,
}

#[derive(Deserialize)]
struct PodmanPort {
    #[serde(default)]
    host_ip: String,
    host_port: u16,
    container_port: u16,
    #[serde(default)]
    protocol: String,
    #[serde(default = "one")]
    range: u16,
}

fn one() -> u16 {
    1
}

/// `podman ps -a --format json`. Pods are passed in because podman 5 leaves `PodName` empty here.
pub fn parse_podman_ps(text: &str, pods: &[Pod]) -> Result<Vec<Container>, String> {
    let containers = serde_json::from_str::<Vec<PodmanContainer>>(text).map_err(|error| error.to_string())?;
    Ok(containers
        .into_iter()
        .map(|c| {
            let pod = if !c.pod_name.is_empty() {
                Some(c.pod_name)
            } else if c.pod.is_empty() {
                None
            } else {
                pods.iter()
                    .find(|p| p.id == c.pod)
                    .map(|p| p.name.clone())
                    .or(Some(c.pod[..c.pod.len().min(12)].to_owned()))
            };
            let mut ports = Vec::new();
            for port in c.ports.unwrap_or_default() {
                for offset in 0..port.range.max(1) {
                    ports.push(PortMap {
                        host_ip: port.host_ip.clone(),
                        host_port: port.host_port.saturating_add(offset),
                        container_port: port.container_port.saturating_add(offset),
                        protocol: if port.protocol.is_empty() { "tcp".to_owned() } else { port.protocol.clone() },
                    });
                }
            }
            let labels = c.labels.unwrap_or_default();
            Container {
                engine: Engine::Podman,
                id: c.id,
                name: c.names.first().cloned().unwrap_or_default(),
                runs: known::kind_of_image(&c.image),
                image: c.image,
                running: c.state.eq_ignore_ascii_case("running"),
                status: c.status,
                pod,
                ports,
                systemd_unit: labels.get("PODMAN_SYSTEMD_UNIT").cloned(),
                gpu: GpuAccess::Unknown,
                is_infra: c.is_infra,
            }
        })
        .collect())
}

#[derive(Deserialize)]
struct DockerContainer {
    #[serde(rename = "ID", default)]
    id: String,
    #[serde(rename = "Names", default)]
    names: String,
    #[serde(rename = "Image", default)]
    image: String,
    #[serde(rename = "State", default)]
    state: String,
    #[serde(rename = "Status", default)]
    status: String,
    #[serde(rename = "Ports", default)]
    ports: String,
    #[serde(rename = "Labels", default)]
    labels: String,
}

/// `docker ps -a --format '{{json .}}'`: one JSON object per line.
#[must_use]
pub fn parse_docker_ps(text: &str) -> Vec<Container> {
    text.lines()
        .filter_map(|line| serde_json::from_str::<DockerContainer>(line).ok())
        .map(|c| {
            let labels: HashMap<String, String> = c
                .labels
                .split(',')
                .filter_map(|pair| pair.split_once('='))
                .map(|(k, v)| (k.to_owned(), v.to_owned()))
                .collect();
            Container {
                engine: Engine::Docker,
                id: c.id,
                name: c.names.split(',').next().unwrap_or_default().to_owned(),
                runs: known::kind_of_image(&c.image),
                image: c.image,
                running: c.state.eq_ignore_ascii_case("running"),
                status: c.status,
                pod: None,
                ports: parse_docker_ports(&c.ports),
                systemd_unit: labels.get("PODMAN_SYSTEMD_UNIT").cloned(),
                gpu: GpuAccess::Unknown,
                is_infra: false,
            }
        })
        .collect()
}

/// Docker's `Ports` column: `0.0.0.0:11434->11434/tcp, :::11434->11434/tcp, 8080/tcp`.
#[must_use]
pub fn parse_docker_ports(text: &str) -> Vec<PortMap> {
    let mut ports = Vec::new();
    for item in text.split(',').map(str::trim).filter(|s| !s.is_empty()) {
        let Some((host, container)) = item.split_once("->") else { continue };
        let (host_ip, host_port) = host.rsplit_once(':').unwrap_or(("", host));
        let (container_port, protocol) = container.split_once('/').unwrap_or((container, "tcp"));
        let (Ok(host_port), Ok(container_port)) = (host_port.parse::<u16>(), container_port.parse::<u16>()) else {
            continue;
        };
        if ports.iter().any(|p: &PortMap| p.host_port == host_port && p.container_port == container_port) {
            continue; // the IPv6 duplicate
        }
        ports.push(PortMap { host_ip: host_ip.to_owned(), host_port, container_port, protocol: protocol.to_owned() });
    }
    ports
}

/// From `inspect` output (podman or docker, an array of objects): whether each container was given
/// GPU devices. Podman reports Quadlet `AddDevice=` only in `Config.CreateCommand`, so both the
/// device list and the creating command line are read.
#[must_use]
pub fn parse_inspect_gpu(text: &str) -> HashMap<String, GpuAccess> {
    let Ok(items) = serde_json::from_str::<Vec<serde_json::Value>>(text) else { return HashMap::new() };
    items
        .iter()
        .filter_map(|item| {
            let id = item.get("Id")?.as_str()?.to_owned();
            let mut devices: Vec<String> = Vec::new();
            if let Some(list) = item.pointer("/HostConfig/Devices").and_then(|v| v.as_array()) {
                devices.extend(
                    list.iter().filter_map(|d| d.get("PathOnHost").and_then(|p| p.as_str()).map(str::to_owned)),
                );
            }
            if let Some(requests) = item.pointer("/HostConfig/DeviceRequests").and_then(|v| v.as_array())
                && !requests.is_empty()
            {
                devices.push("gpu (device request)".to_owned());
            }
            if let Some(argv) = item.pointer("/Config/CreateCommand").and_then(|v| v.as_array()) {
                let args: Vec<&str> = argv.iter().filter_map(|a| a.as_str()).collect();
                for (index, arg) in args.iter().enumerate() {
                    if (*arg == "--device" || *arg == "--gpus")
                        && let Some(value) = args.get(index + 1)
                    {
                        devices.push((*value).to_owned());
                    } else if let Some(value) = arg.strip_prefix("--device=").or_else(|| arg.strip_prefix("--gpus=")) {
                        devices.push(value.to_owned());
                    }
                }
            }
            devices.sort();
            devices.dedup();
            let gpu_like = devices.iter().filter(|d| is_gpu_device(d)).cloned().collect::<Vec<_>>();
            Some((id, if gpu_like.is_empty() { GpuAccess::No } else { GpuAccess::Yes { devices: gpu_like } }))
        })
        .collect()
}

fn is_gpu_device(device: &str) -> bool {
    device.starts_with("/dev/kfd")
        || device.starts_with("/dev/dri")
        || device.starts_with("/dev/nvidia")
        || device.contains("nvidia.com/gpu")
        || device.contains("amd.com/gpu")
        || device.starts_with("gpu")
        || device == "all"
}

/// Convenience for callers that only need to know what a kind of image is.
#[must_use]
pub fn kind_of_image(image: &str) -> Option<RuntimeKind> {
    known::kind_of_image(image)
}

#[cfg(test)]
mod tests {
    use super::*;

    const PS: &str = include_str!("../tests/fixtures/podman_ps.json");
    const PODS: &str = include_str!("../tests/fixtures/podman_pod_ps.json");
    const INSPECT: &str = include_str!("../tests/fixtures/podman_inspect.json");

    #[test]
    fn podman_output_from_the_development_machine_is_understood() {
        let pods = parse_podman_pods(PODS).unwrap();
        let local_ai = pods.iter().find(|p| p.name == "local-ai").expect("the local-ai pod");
        assert!(local_ai.running);
        assert_eq!(local_ai.systemd_unit.as_deref(), Some("local-ai-pod.service"));
        assert!(local_ai.containers.iter().any(|c| c == "ollama"));

        let containers = parse_podman_ps(PS, &pods).unwrap();
        let ollama = containers.iter().find(|c| c.name == "ollama").expect("the ollama container");
        assert_eq!(ollama.runs, Some(RuntimeKind::Ollama));
        assert!(ollama.running);
        assert_eq!(ollama.pod.as_deref(), Some("local-ai"), "the pod name comes from `pod ps` when `PodName` is empty");
        assert_eq!(ollama.systemd_unit.as_deref(), Some("ollama.service"));
        assert!(ollama.ports.iter().any(|p| p.host_port == 11434 && p.container_port == 11434));
        let webui = containers.iter().find(|c| c.name == "open-webui").unwrap();
        assert_eq!(webui.runs, None);
        let stopped = containers.iter().find(|c| c.name == "webui-proxy").unwrap();
        assert!(!stopped.running);
        assert!(containers.iter().any(|c| c.is_infra));
    }

    #[test]
    fn gpu_devices_are_read_from_the_create_command_when_devices_is_empty() {
        let gpu = parse_inspect_gpu(INSPECT);
        let (_, access) = gpu.iter().next().expect("one container");
        assert_eq!(*access, GpuAccess::Yes { devices: vec!["/dev/dri".into(), "/dev/kfd".into()] });
        let none = parse_inspect_gpu(
            r#"[{"Id":"abc","HostConfig":{"Devices":[]},"Config":{"CreateCommand":["podman","run","x"]}}]"#,
        );
        assert_eq!(none["abc"], GpuAccess::No);
        let nvidia = parse_inspect_gpu(
            r#"[{"Id":"d","HostConfig":{"Devices":[],"DeviceRequests":[{"Driver":"nvidia","Count":-1}]}}]"#,
        );
        assert!(matches!(&nvidia["d"], GpuAccess::Yes { .. }));
    }

    #[test]
    fn docker_lines_and_ports_are_understood() {
        let text = r#"{"ID":"0123456789abcdef","Names":"ollama","Image":"ollama/ollama:latest","State":"running","Status":"Up 3 hours","Ports":"0.0.0.0:11434->11434/tcp, :::11434->11434/tcp","Labels":"a=b,c=d"}
{"ID":"fedcba9876543210","Names":"db","Image":"postgres:16","State":"exited","Status":"Exited (0) 2 days ago","Ports":"","Labels":""}"#;
        let containers = parse_docker_ps(text);
        assert_eq!(containers.len(), 2);
        assert_eq!(containers[0].runs, Some(RuntimeKind::Ollama));
        assert_eq!(
            containers[0].ports,
            vec![PortMap {
                host_ip: "0.0.0.0".into(),
                host_port: 11434,
                container_port: 11434,
                protocol: "tcp".into()
            }]
        );
        assert!(!containers[1].running);
        assert_eq!(parse_docker_ports("8080/tcp"), Vec::new(), "an unpublished port isn't reachable from the host");
    }
}
