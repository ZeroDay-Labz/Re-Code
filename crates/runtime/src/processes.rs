//! Native processes and programs. A process inside a container shows up in `/proc` too; its cgroup
//! says so, and then it belongs to the container rather than being a second runtime.

use std::path::Path;

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Process {
    pub pid: u32,
    /// The program's path, or its name when the path can't be read.
    pub exe: String,
    /// The container id, when the process runs inside one.
    pub container: Option<String>,
}

/// Read-only access to processes and `PATH`, replaceable in tests.
pub trait Processes {
    /// Processes whose program is a known runtime (see [`crate::known::BINARIES`]).
    fn list(&self) -> Vec<Process>;
    fn on_path(&self, program: &str) -> bool;
}

pub struct RealProcesses;

impl Processes for RealProcesses {
    fn list(&self) -> Vec<Process> {
        let Ok(entries) = std::fs::read_dir("/proc") else { return Vec::new() };
        let mut found = Vec::new();
        for entry in entries.flatten() {
            let Ok(pid) = entry.file_name().to_string_lossy().parse::<u32>() else { continue };
            let dir = entry.path();
            // `exe` needs permission; `comm` is readable for every process but truncated to 15 bytes.
            let exe = std::fs::read_link(dir.join("exe"))
                .ok()
                .map(|path| path.to_string_lossy().trim_end_matches(" (deleted)").to_owned())
                .or_else(|| std::fs::read_to_string(dir.join("comm")).ok().map(|comm| comm.trim().to_owned()));
            let Some(exe) = exe else { continue };
            if crate::known::kind_of_binary(&exe).is_none() {
                continue;
            }
            let cgroup = std::fs::read_to_string(dir.join("cgroup")).unwrap_or_default();
            found.push(Process { pid, exe, container: container_of(&cgroup) });
        }
        found.sort_by_key(|p| p.pid);
        found
    }

    fn on_path(&self, program: &str) -> bool {
        std::env::var_os("PATH")
            .is_some_and(|path| std::env::split_paths(&path).any(|dir| is_executable(&dir.join(program))))
    }
}

/// The container id named in a process's cgroup, for podman (`libpod-<id>.scope`,
/// `libpod-payload-<id>`) and docker (`docker-<id>.scope`, `/docker/<id>`).
#[must_use]
pub fn container_of(cgroup: &str) -> Option<String> {
    for line in cgroup.lines() {
        let path = line.rsplit(':').next().unwrap_or(line);
        for segment in path.split('/') {
            let segment = segment.strip_suffix(".scope").unwrap_or(segment);
            for prefix in ["libpod-payload-", "libpod-", "docker-"] {
                if let Some(id) = segment.strip_prefix(prefix)
                    && id.len() >= 12
                    && id.chars().all(|c| c.is_ascii_hexdigit())
                {
                    return Some(id.to_owned());
                }
            }
        }
        if let Some(rest) = path.strip_prefix("/docker/")
            && rest.len() >= 12
            && rest.chars().all(|c| c.is_ascii_hexdigit())
        {
            return Some(rest.to_owned());
        }
    }
    None
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else { return false };
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        metadata.is_file() && metadata.permissions().mode() & 0o111 != 0
    }
    #[cfg(not(unix))]
    {
        metadata.is_file()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn container_processes_are_recognised_from_their_cgroup() {
        // The development machine's Ollama container, seen from the host.
        let podman = "0::/user.slice/user-1000.slice/user@1000.service/app.slice/ollama.service/libpod-payload-f64b81d1d0032be82eb4c393c41e27d8fae7baf343f69fa129db21be87ad7169\n";
        assert_eq!(
            container_of(podman).as_deref(),
            Some("f64b81d1d0032be82eb4c393c41e27d8fae7baf343f69fa129db21be87ad7169")
        );
        assert_eq!(
            container_of("0::/system.slice/docker-0123456789abcdef0123.scope\n").as_deref(),
            Some("0123456789abcdef0123")
        );
        assert_eq!(container_of("0::/docker/0123456789abcdef0123\n").as_deref(), Some("0123456789abcdef0123"));
        assert_eq!(container_of("0::/user.slice/user-1000.slice/user@1000.service/app.slice/ollama.service\n"), None);
    }
}
