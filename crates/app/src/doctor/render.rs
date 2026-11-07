//! Human-readable output for `re-code doctor`.

use super::Report;
use super::hardware::{Gpu, GpuVendor};
use super::runtime::{Installation, OllamaStatus, display_path};
use std::fmt::Write;
use std::io::IsTerminal;

const GIB: f64 = 1024.0 * 1024.0 * 1024.0;

/// Whether to use colour.
#[derive(Debug, Clone, Copy)]
pub struct Style {
    pub color: bool,
}

impl Style {
    pub fn detect() -> Self {
        let no_color = std::env::var_os("NO_COLOR").is_some_and(|value| !value.is_empty());
        Self { color: !no_color && std::io::stdout().is_terminal() }
    }

    fn paint(self, code: &str, text: &str) -> String {
        if self.color { format!("\x1b[{code}m{text}\x1b[0m") } else { text.to_owned() }
    }
    fn heading(self, text: &str) -> String {
        self.paint("1", text)
    }
    fn dim(self, text: &str) -> String {
        self.paint("2", text)
    }
    fn good(self, text: &str) -> String {
        self.paint("32", text)
    }
    fn warn(self, text: &str) -> String {
        self.paint("33", text)
    }
}

fn gib(bytes: u64) -> String {
    #[allow(clippy::cast_precision_loss)]
    let value = bytes as f64 / GIB;
    format!("{value:.1} GiB")
}

fn or_unknown(value: Option<String>) -> String {
    value.unwrap_or_else(|| "unknown".to_owned())
}

/// Renders the report. Meaning is never carried by colour alone.
pub fn render(report: &Report, style: Style) -> String {
    let mut out = String::new();
    let _ = render_into(&mut out, report, style);
    out
}

fn render_into(out: &mut String, report: &Report, style: Style) -> std::fmt::Result {
    let row =
        |out: &mut String, key: &str, value: &str| writeln!(out, "  {} {value}", style.dim(&format!("{key:<10}")));
    writeln!(out, "{} {}", style.heading("re-code doctor"), style.dim(report.version))?;

    writeln!(out, "\n{}", style.heading("System"))?;
    row(
        out,
        "OS",
        &format!(
            "{}, kernel {}, {}",
            or_unknown(report.system.os.clone()),
            or_unknown(report.system.kernel.clone()),
            report.system.arch
        ),
    )?;
    let cores = match (report.cpu.cores, report.cpu.threads) {
        (Some(cores), Some(threads)) => format!("{cores} cores / {threads} threads"),
        (None, Some(threads)) => format!("{threads} threads"),
        _ => "core count unknown".to_owned(),
    };
    row(out, "CPU", &format!("{}, {cores}", or_unknown(report.cpu.model.clone())))?;
    if !report.cpu.features.is_empty() {
        row(out, "", &report.cpu.features.join(" "))?;
    }
    row(
        out,
        "Memory",
        &format!(
            "{} available of {}",
            or_unknown(report.memory.available_bytes.map(gib)),
            or_unknown(report.memory.total_bytes.map(gib)),
        ),
    )?;
    if let Some(disk) = &report.disk {
        row(
            out,
            "Disk",
            &format!("{} free of {} at {}", gib(disk.available_bytes), gib(disk.total_bytes), display_path(&disk.path)),
        )?;
    }

    writeln!(out, "\n{}", style.heading("GPU"))?;
    if report.gpus.is_empty() {
        row(out, "", "No GPU found. Models will run on the CPU.")?;
    }
    for gpu in &report.gpus {
        render_gpu(out, gpu, style)?;
    }
    let compute = &report.compute;
    let mut facts = vec![format!("{} render node(s)", compute.render_nodes)];
    if compute.kfd_present {
        facts.push("/dev/kfd present".to_owned());
    }
    facts.extend(compute.runtimes_on_path.iter().map(|tool| (*tool).to_owned()));
    row(out, "Compute", &facts.join(", "))?;

    writeln!(out, "\n{}", style.heading("Ollama"))?;
    render_ollama(out, &report.ollama, style)?;

    writeln!(out, "\n{}", style.heading("Runtimes and containers"))?;
    render_runtimes(out, &report.runtimes, style)?;

    writeln!(out, "\n{}", style.heading("Tools"))?;
    let tick = |present: bool| if present { style.good("yes") } else { style.warn("no") };
    row(out, "git", &tick(report.tools.git))?;
    row(out, "podman", &tick(report.tools.podman))?;
    row(out, "docker", &tick(report.tools.docker))?;
    row(out, "bwrap", &format!("{}  {}", tick(report.tools.bwrap), style.dim("sandbox for shell commands")))?;
    if let Some(active) = report.tools.landlock_active {
        row(out, "landlock", &tick(active))?;
    }

    writeln!(out, "\n{}", style.heading("Terminal"))?;
    row(out, "Type", &or_unknown(report.terminal.emulator.clone().or_else(|| report.terminal.term.clone())))?;
    row(out, "Colour", if report.terminal.truecolor { "truecolor" } else { "256 colours or fewer" })?;

    writeln!(out, "\n{}", style.heading("re-code"))?;
    let paths = &report.storage.paths;
    row(out, "Config", &display_path(&paths.config_file))?;
    row(out, "Logs", &display_path(&paths.log_dir))?;
    match &report.storage.database {
        Ok(version) => row(out, "Database", &format!("{} (schema {version})", display_path(&paths.database)))?,
        Err(error) => row(out, "Database", &style.warn(&format!("problem: {error}")))?,
    }
    Ok(())
}

fn render_gpu(out: &mut String, gpu: &Gpu, style: Style) -> std::fmt::Result {
    let vendor = match gpu.vendor {
        GpuVendor::Amd => "AMD",
        GpuVendor::Nvidia => "NVIDIA",
        GpuVendor::Intel => "Intel",
        GpuVendor::Other => "Other",
    };
    let name = gpu.name.clone().unwrap_or_else(|| gpu.pci_id.clone());
    let driver = gpu.driver.clone().map_or_else(String::new, |driver| format!(", {driver} driver"));
    writeln!(out, "  {} {vendor} {name}{driver}", style.dim(&format!("{:<10}", "Card")))?;
    let vram = match (gpu.vram_total_bytes, gpu.vram_used_bytes) {
        (Some(total), Some(used)) => format!("{} total, {} already in use", gib(total), gib(used)),
        (Some(total), None) => format!("{} total", gib(total)),
        _ => "not reported by this driver".to_owned(),
    };
    writeln!(out, "  {} {vram}", style.dim(&format!("{:<10}", "VRAM")))
}

fn render_ollama(out: &mut String, status: &OllamaStatus, style: Style) -> std::fmt::Result {
    match status {
        OllamaStatus::Running { url, version, models, loaded } => {
            writeln!(out, "  {} version {version} at {url}", style.good("running"))?;
            if loaded.is_empty() {
                writeln!(out, "  {}", style.dim("No model is loaded right now."))?;
            }
            for model in loaded {
                let on_gpu = (model.size_vram_bytes * 100).checked_div(model.size_bytes).unwrap_or(0);
                let context = model.context_length.map_or_else(String::new, |length| format!(", context {length}"));
                writeln!(out, "  Loaded: {} ({}, {on_gpu}% on GPU{context})", model.name, gib(model.size_bytes))?;
            }
            writeln!(out)?;
            writeln!(
                out,
                "  {}",
                style.dim(&format!("{:<34}{:>10}  {:>10}  {}", "Model", "Size", "Context", "Declares"))
            )?;
            for model in models {
                let (context, declares) = match &model.capabilities {
                    Ok(capabilities) => {
                        let context = capabilities
                            .context_limit
                            .as_ref()
                            .map_or_else(|| "?".to_owned(), |limit| limit.value.to_string());
                        let flags = [
                            ("tools", &capabilities.tools),
                            ("vision", &capabilities.vision),
                            ("thinking", &capabilities.thinking),
                        ];
                        let names: Vec<_> = flags
                            .iter()
                            .filter(|(_, flag)| flag.as_ref().is_some_and(|flag| flag.value))
                            .map(|(name, _)| *name)
                            .collect();
                        (context, names.join(" "))
                    }
                    Err(_) => ("?".to_owned(), style.warn("details unavailable")),
                };
                let size = model.info.size_bytes.map_or_else(|| "?".to_owned(), gib);
                writeln!(out, "  {:<34}{size:>10}  {context:>10}  {declares}", model.info.name)?;
            }
            writeln!(
                out,
                "\n  {}",
                style.dim("\"Declares\" is what each model claims. It hasn't been tested on this machine.")
            )
        }
        OllamaStatus::InstalledStopped { url, install } => {
            writeln!(out, "  {} nothing is answering at {url}", style.warn("installed but stopped:"))?;
            render_installation(out, install)?;
            writeln!(out, "  To start it: {}", install.start_command())?;
            if let Installation::Quadlet { pod: Some(pod), .. } = install {
                writeln!(out, "  {}", style.dim(&format!("This also starts the other containers in {pod}.")))?;
            }
            writeln!(out, "  {}", style.dim("re-code doesn't start services you manage yourself without asking."))
        }
        OllamaStatus::ActiveNotAnswering { url, install, error } => {
            writeln!(out, "  {} the service is active, but {url} doesn't answer.", style.warn("not responding:"))?;
            render_installation(out, install)?;
            writeln!(out, "  {error}")?;
            writeln!(out, "  It may still be starting, or it may listen on a different address.")
        }
        OllamaStatus::Unhealthy { url, error } => {
            writeln!(
                out,
                "  {} something answered at {url}, but it didn't behave like Ollama.",
                style.warn("problem:")
            )?;
            writeln!(out, "  {error}")
        }
        OllamaStatus::Absent { url, error } => {
            writeln!(out, "  {} no installation found, and nothing is answering at {url}.", style.warn("not found:"))?;
            writeln!(out, "  {}", style.dim(error))?;
            writeln!(out, "  If it runs elsewhere, set the address with --ollama-url or in the config file.")
        }
    }
}

fn render_installation(out: &mut String, install: &Installation) -> std::fmt::Result {
    match install {
        Installation::Quadlet { unit, file, .. } => writeln!(out, "  Quadlet service {unit} ({})", display_path(file)),
        Installation::SystemdUnit { unit, user } => {
            writeln!(out, "  systemd {} service {unit}", if *user { "user" } else { "system" })
        }
        Installation::Binary => writeln!(out, "  The `ollama` program is on your PATH"),
    }
}

/// Every runtime found and how it's run, then the containers, then what couldn't be learnt.
fn render_runtimes(out: &mut String, report: &recode_runtime::Report, style: Style) -> std::fmt::Result {
    use recode_runtime::{GpuAccess, Ownership, RuntimeState, ServedBy};
    for engine in &report.engines {
        let version = engine.version.as_deref().map_or_else(|| style.warn("didn't answer"), str::to_owned);
        let rootless = match engine.rootless {
            Some(true) => " rootless",
            _ => "",
        };
        writeln!(out, "  {} {version}{rootless}", engine.engine.command())?;
    }
    if report.engines.is_empty() {
        writeln!(out, "  {}", style.dim("No container engine on PATH."))?;
    }
    if report.runtimes.is_empty() {
        writeln!(out, "  {}", style.dim("No inference runtime found."))?;
    }
    for runtime in &report.runtimes {
        let (mark, state) = match &runtime.state {
            RuntimeState::Running { version } => {
                (style.good("●"), format!("running{}", version.as_deref().map(|v| format!(" {v}")).unwrap_or_default()))
            }
            RuntimeState::InstalledStopped { .. } => (style.warn("◐"), "installed, stopped".to_owned()),
            RuntimeState::ActiveNotAnswering { .. } => (style.warn("✗"), "active, not answering".to_owned()),
        };
        let supported = if runtime.kind.supported() { "" } else { "  (detected; not supported yet)" };
        let url = runtime.url.as_deref().map(|u| format!(" at {u}")).unwrap_or_default();
        writeln!(out, "  {mark} {} {state}{url}{}", runtime.kind.name(), style.dim(supported))?;
        match &runtime.served_by {
            Some(ServedBy::Container { engine, name, pod, image, gpu }) => {
                let pod = pod.as_deref().map(|p| format!(" in pod `{p}`")).unwrap_or_default();
                let gpu = match gpu {
                    GpuAccess::Yes { devices } => format!("GPU attached ({})", devices.join(", ")),
                    GpuAccess::No => "no GPU devices".to_owned(),
                    GpuAccess::Unknown => "GPU access unknown".to_owned(),
                };
                writeln!(out, "      {} container `{name}`{pod}, {image}", engine.command())?;
                writeln!(out, "      {gpu}")?;
            }
            Some(ServedBy::SystemdUnit { unit, user, file }) => {
                let scope = if *user { "user" } else { "system" };
                let file = file.as_deref().map(|f| format!(" ({})", display_path(f))).unwrap_or_default();
                writeln!(out, "      systemd {scope} unit {unit}{file}")?;
            }
            Some(ServedBy::Process { pid, exe }) => writeln!(out, "      process {pid} ({exe})")?,
            None => {}
        }
        if let RuntimeState::InstalledStopped { start: Some(start) } = &runtime.state {
            writeln!(out, "      To start it: {}", start.command_line())?;
        }
        if let RuntimeState::ActiveNotAnswering { error } = &runtime.state {
            writeln!(out, "      {error}")?;
        }
        if runtime.ownership == Ownership::UserManaged && !matches!(runtime.state, RuntimeState::Running { .. }) {
            writeln!(
                out,
                "      {}",
                style.dim("Yours: re-code only starts it when you say so, and never changes it.")
            )?;
        }
        for note in &runtime.notes {
            writeln!(out, "      {}", style.dim(note))?;
        }
    }
    for unknown in &report.unidentified {
        writeln!(out, "  {} {}: {}", style.dim("?"), unknown.url, unknown.summary)?;
    }
    let listed: Vec<&recode_runtime::Container> = report.containers.iter().filter(|c| !c.is_infra).collect();
    if !listed.is_empty() {
        writeln!(out, "  {}", style.dim(&format!("{} container(s):", listed.len())))?;
        let mut pods: Vec<Option<&str>> = listed.iter().map(|c| c.pod.as_deref()).collect();
        pods.sort();
        pods.dedup();
        for pod in pods {
            let members: Vec<&&recode_runtime::Container> = listed.iter().filter(|c| c.pod.as_deref() == pod).collect();
            if let Some(pod) = pod {
                // Published ports belong to the pod, and podman repeats them on every member.
                let ports = port_list(members.first().map_or(&[][..], |c| &c.ports));
                writeln!(out, "    pod {pod}{ports}")?;
            }
            for container in members {
                let indent = if pod.is_some() { "      " } else { "    " };
                let ports = if pod.is_some() { String::new() } else { port_list(&container.ports) };
                let runs = container.runs.map(|k| format!("  {}", style.good(k.name()))).unwrap_or_default();
                let state = if container.running { container.status.clone() } else { style.warn(&container.status) };
                writeln!(out, "{indent}{} {}  {}{ports}{runs}", container.name, style.dim(&container.image), state)?;
            }
        }
    }
    for warning in &report.warnings {
        writeln!(out, "  {} {warning}", style.warn("!"))?;
    }
    for problem in &report.problems {
        writeln!(out, "  {} {problem}", style.dim("couldn't check:"))?;
    }
    Ok(())
}

fn port_list(ports: &[recode_runtime::PortMap]) -> String {
    let ports: Vec<String> = ports.iter().map(|p| format!(":{}", p.host_port)).collect();
    if ports.is_empty() { String::new() } else { format!("  {}", ports.join(" ")) }
}
