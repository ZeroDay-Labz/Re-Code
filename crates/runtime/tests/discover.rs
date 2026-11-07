//! Discovery as a whole, on a stand-in for the development machine: a podman pod with an Ollama
//! container (GPU attached, Quadlet-managed) that answers, the container's own process visible in
//! `/proc`, an unrelated pod, and a stopped container. Everything must come out as one Ollama entry.

use recode_runtime::containers::{Engines, Found, parse_inspect_gpu, parse_podman_pods, parse_podman_ps};
use recode_runtime::processes::{Process, Processes};
use recode_runtime::systemd::{Services, UnitState};
use recode_runtime::{
    Engine, GpuAccess, Ownership, Report, RuntimeKind, RuntimeState, Scan, ServedBy, StartMethod, discover_with,
};
use std::collections::HashMap;
use std::path::PathBuf;
use wiremock::matchers::{method, path};
use wiremock::{Mock, MockServer, ResponseTemplate};

const PS: &str = include_str!("fixtures/podman_ps.json");
const PODS: &str = include_str!("fixtures/podman_pod_ps.json");
const INSPECT: &str = include_str!("fixtures/podman_inspect.json");
const QUADLET: &str = "[Container]\nImage=docker.io/ollama/ollama:rocm\nContainerName=ollama\nPod=local-ai.pod\n";

struct FakeEngines(Found);

impl Engines for FakeEngines {
    async fn list(self) -> Found {
        self.0
    }
}

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
        self.units.iter().find(|((n, u), _)| *n == unit && *u == user).map_or(UnitState::NotFound, |(_, s)| *s)
    }
}

#[derive(Default)]
struct FakeProcesses {
    processes: Vec<Process>,
    on_path: Vec<&'static str>,
}

impl Processes for FakeProcesses {
    fn list(&self) -> Vec<Process> {
        self.processes.clone()
    }
    fn on_path(&self, program: &str) -> bool {
        self.on_path.contains(&program)
    }
}

/// The fixtures, with the Ollama container publishing the mock server's port instead of 11434.
fn machine(ollama_port: u16, ollama_running: bool) -> Found {
    let pods = parse_podman_pods(PODS).unwrap();
    let mut containers = parse_podman_ps(PS, &pods).unwrap();
    let gpu = parse_inspect_gpu(INSPECT);
    for container in &mut containers {
        if let Some(access) = gpu.get(&container.id) {
            container.gpu = access.clone();
        }
        // Never probe this machine's real ports from a test: Ollama's port becomes the mock's, every
        // other published port a closed one.
        for port in &mut container.ports {
            port.host_port = if port.host_port == 11434 { ollama_port } else { 9 };
        }
        if container.name == "ollama" && !ollama_running {
            container.running = false;
            container.status = "Exited (0) 5 minutes ago".to_owned();
        }
    }
    Found {
        engines: vec![recode_runtime::EngineInfo {
            engine: Engine::Podman,
            version: Some("5.4.0".into()),
            rootless: Some(true),
        }],
        pods,
        containers,
        problems: Vec::new(),
    }
}

fn scan(url: &str) -> Scan {
    Scan {
        urls: vec![url.to_owned()],
        ports: Vec::new(),
        user_config_root: PathBuf::from("/home/u/.config"),
        app_prefix: "re-code-".into(),
    }
}

async fn ollama_server() -> MockServer {
    let server = MockServer::start().await;
    Mock::given(method("GET"))
        .and(path("/"))
        .respond_with(ResponseTemplate::new(200).set_body_string("Ollama is running"))
        .mount(&server)
        .await;
    Mock::given(method("GET"))
        .and(path("/api/version"))
        .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "0.40.14"})))
        .mount(&server)
        .await;
    server
}

fn ollama_entries(report: &Report) -> Vec<&recode_runtime::Runtime> {
    report.runtimes.iter().filter(|r| r.kind == RuntimeKind::Ollama).collect()
}

#[tokio::test]
async fn a_running_quadlet_pod_is_one_entry_with_its_container_gpu_and_version() {
    let server = ollama_server().await;
    let port = server.address().port();
    let container_id = "f64b81d1d0032be82eb4c393c41e27d8fae7baf343f69fa129db21be87ad7169";
    let services = FakeServices {
        quadlets: vec![(PathBuf::from("/home/u/.config/containers/systemd/ollama.container"), QUADLET.to_owned())],
        units: HashMap::from([(("ollama.service", true), UnitState::Active)]),
    };
    let procs = FakeProcesses {
        // The container's own process, as /proc shows it from the host.
        processes: vec![Process { pid: 2505194, exe: "/bin/ollama".into(), container: Some(container_id.into()) }],
        on_path: vec![],
    };
    let report = discover_with(&scan(&server.uri()), &services, FakeEngines(machine(port, true)), procs).await;

    let entries = ollama_entries(&report);
    assert_eq!(entries.len(), 1, "one Ollama, not one per source: {:#?}", report.runtimes);
    let ollama = entries[0];
    assert_eq!(ollama.state, RuntimeState::Running { version: Some("0.40.14".into()) });
    assert_eq!(ollama.ownership, Ownership::UserManaged);
    let Some(ServedBy::Container { engine, name, pod, gpu, .. }) = &ollama.served_by else { panic!("{ollama:#?}") };
    assert_eq!((*engine, name.as_str(), pod.as_deref()), (Engine::Podman, "ollama", Some("local-ai")));
    assert_eq!(*gpu, GpuAccess::Yes { devices: vec!["/dev/dri".into(), "/dev/kfd".into()] });
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(report.containers.iter().any(|c| c.name == "open-webui"), "unrelated containers are still listed");
    assert_eq!(report.engines[0].engine, Engine::Podman);
}

#[tokio::test]
async fn a_stopped_quadlet_container_starts_through_its_unit_and_names_the_pod() {
    let services = FakeServices {
        quadlets: vec![(PathBuf::from("/home/u/.config/containers/systemd/ollama.container"), QUADLET.to_owned())],
        units: HashMap::from([(("ollama.service", true), UnitState::Inactive)]),
    };
    let report = discover_with(
        &scan("http://127.0.0.1:9"),
        &services,
        FakeEngines(machine(11434, false)),
        FakeProcesses::default(),
    )
    .await;
    let entries = ollama_entries(&report);
    assert_eq!(entries.len(), 1, "{:#?}", report.runtimes);
    let RuntimeState::InstalledStopped { start: Some(start) } = &entries[0].state else { panic!("{:#?}", entries[0]) };
    assert_eq!(*start, StartMethod::SystemdUser { unit: "ollama.service".into() });
    assert_eq!(start.command_line(), "systemctl --user start ollama.service");
    assert!(entries[0].notes.iter().any(|n| n.contains("local-ai")), "{:?}", entries[0].notes);
}

#[tokio::test]
async fn a_native_process_and_a_container_are_told_apart() {
    let server = ollama_server().await;
    let port = server.address().port();
    // Nothing in containers runs Ollama; a native process does, and it's the one answering.
    let mut found = machine(port, true);
    found.containers.retain(|c| c.name != "ollama");
    let procs = FakeProcesses {
        processes: vec![Process { pid: 4242, exe: "/usr/bin/ollama".into(), container: None }],
        on_path: vec!["ollama"],
    };
    let report = discover_with(&scan(&server.uri()), &FakeServices::default(), FakeEngines(found), procs).await;
    let entries = ollama_entries(&report);
    assert_eq!(entries.len(), 1, "{:#?}", report.runtimes);
    assert_eq!(entries[0].served_by, Some(ServedBy::Process { pid: 4242, exe: "/usr/bin/ollama".into() }));
    assert_eq!(entries[0].id, "ollama@pid:4242");
}

#[tokio::test]
async fn a_bare_binary_offers_ollama_serve_and_nothing_else_is_invented() {
    let procs = FakeProcesses { processes: vec![], on_path: vec!["ollama", "llama-server"] };
    let found = Found::default();
    let report = discover_with(&scan("http://127.0.0.1:9"), &FakeServices::default(), FakeEngines(found), procs).await;
    let ollama = ollama_entries(&report)[0];
    assert_eq!(
        ollama.state,
        RuntimeState::InstalledStopped {
            start: Some(StartMethod::Binary { argv: vec!["ollama".into(), "serve".into()] })
        }
    );
    let llama = report.runtimes.iter().find(|r| r.kind == RuntimeKind::LlamaCpp).unwrap();
    assert_eq!(
        llama.state,
        RuntimeState::InstalledStopped { start: None },
        "llama-server needs a model: no start command is invented"
    );
    assert!(llama.notes[0].contains("needs a model"));
}

#[tokio::test]
async fn a_running_container_whose_port_answers_nothing_is_reported_as_such() {
    let found = machine(9, true); // port 9: nothing listens
    let report = discover_with(
        &scan("http://127.0.0.1:9"),
        &FakeServices::default(),
        FakeEngines(found),
        FakeProcesses::default(),
    )
    .await;
    let ollama = ollama_entries(&report)[0];
    let RuntimeState::ActiveNotAnswering { error } = &ollama.state else { panic!("{ollama:#?}") };
    assert!(error.contains("`ollama` is running") && error.contains('9'), "{error}");
}

#[tokio::test]
async fn an_app_managed_container_is_marked_as_ours() {
    let mut found = machine(11434, false);
    for c in &mut found.containers {
        if c.name == "ollama" {
            c.name = "re-code-ollama".into();
            c.systemd_unit = None;
            c.pod = None;
        }
    }
    let report = discover_with(
        &scan("http://127.0.0.1:9"),
        &FakeServices::default(),
        FakeEngines(found),
        FakeProcesses::default(),
    )
    .await;
    let ollama = ollama_entries(&report)[0];
    assert_eq!(ollama.ownership, Ownership::AppManaged);
    assert_eq!(
        ollama.state,
        RuntimeState::InstalledStopped {
            start: Some(StartMethod::ContainerStart { engine: Engine::Podman, name: "re-code-ollama".into() })
        }
    );
}

#[tokio::test]
async fn localhost_and_loopback_are_the_same_address() {
    let server = ollama_server().await;
    let port = server.address().port();
    let scan = Scan {
        urls: vec![format!("http://localhost:{port}")],
        ports: vec![port],
        user_config_root: PathBuf::from("/home/u/.config"),
        app_prefix: "re-code-".into(),
    };
    let procs = FakeProcesses {
        processes: vec![Process { pid: 7, exe: "/usr/bin/ollama".into(), container: None }],
        on_path: vec![],
    };
    let report = discover_with(&scan, &FakeServices::default(), FakeEngines(Found::default()), procs).await;
    assert_eq!(ollama_entries(&report).len(), 1, "{:#?}", report.runtimes);
    assert_eq!(
        ollama_entries(&report)[0].url.as_deref(),
        Some(format!("http://localhost:{port}").as_str()),
        "the configured spelling is kept"
    );
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
}

#[tokio::test]
async fn a_running_container_is_probed_on_the_port_it_publishes() {
    let server = ollama_server().await;
    let port = server.address().port();
    // The scan knows nothing about this port; the container's published port is the only lead.
    let scan = Scan {
        urls: vec![],
        ports: vec![],
        user_config_root: PathBuf::from("/home/u/.config"),
        app_prefix: "re-code-".into(),
    };
    let report =
        discover_with(&scan, &FakeServices::default(), FakeEngines(machine(port, true)), FakeProcesses::default())
            .await;
    let entries = ollama_entries(&report);
    assert_eq!(entries.len(), 1, "{:#?}", report.runtimes);
    assert_eq!(entries[0].state, RuntimeState::Running { version: Some("0.40.14".into()) });
    assert!(
        matches!(&entries[0].served_by, Some(ServedBy::Container { name, .. }) if name == "ollama"),
        "{:#?}",
        entries[0]
    );
}
