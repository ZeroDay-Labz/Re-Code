//! Hardware and system probes. Parsing is separated from reading so it can be tested.

use serde::Serialize;
use std::collections::HashSet;
use std::path::{Path, PathBuf};

#[derive(Debug, Serialize)]
pub struct System {
    pub os: Option<String>,
    pub kernel: Option<String>,
    pub arch: &'static str,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Cpu {
    pub model: Option<String>,
    pub cores: Option<usize>,
    pub threads: Option<usize>,
    /// Instruction sets that matter for CPU inference.
    pub features: Vec<String>,
}

#[derive(Debug, Default, PartialEq, Eq, Serialize)]
pub struct Memory {
    pub total_bytes: Option<u64>,
    /// What is really usable. Total memory isn't a useful figure for fitting a model.
    pub available_bytes: Option<u64>,
    pub swap_used_bytes: Option<u64>,
}

#[derive(Debug, PartialEq, Eq, Serialize)]
pub struct Gpu {
    pub vendor: GpuVendor,
    pub pci_id: String,
    pub name: Option<String>,
    pub driver: Option<String>,
    pub vram_total_bytes: Option<u64>,
    pub vram_used_bytes: Option<u64>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum GpuVendor {
    Amd,
    Nvidia,
    Intel,
    Other,
}

#[derive(Debug, Serialize)]
pub struct Compute {
    pub kfd_present: bool,
    pub render_nodes: usize,
    pub runtimes_on_path: Vec<&'static str>,
}

#[derive(Debug, Serialize)]
pub struct Disk {
    pub path: PathBuf,
    pub available_bytes: u64,
    pub total_bytes: u64,
}

#[derive(Debug, Serialize)]
pub struct Tools {
    pub podman: bool,
    pub docker: bool,
    pub git: bool,
    pub bwrap: bool,
    pub landlock_active: Option<bool>,
}

#[derive(Debug, Serialize)]
pub struct Terminal {
    pub term: Option<String>,
    pub truecolor: bool,
    pub emulator: Option<String>,
    pub tmux: bool,
}

pub fn system() -> System {
    System {
        os: read("/etc/os-release").and_then(|text| parse_os_release(&text)),
        kernel: read("/proc/sys/kernel/osrelease").map(|text| text.trim().to_owned()),
        arch: std::env::consts::ARCH,
    }
}

pub fn cpu() -> Cpu {
    read("/proc/cpuinfo").map(|text| parse_cpuinfo(&text)).unwrap_or_default()
}

pub fn memory() -> Memory {
    read("/proc/meminfo").map(|text| parse_meminfo(&text)).unwrap_or_default()
}

pub fn gpus() -> Vec<Gpu> {
    let pci_ids = read("/usr/share/hwdata/pci.ids");
    let Ok(entries) = std::fs::read_dir("/sys/class/drm") else {
        return Vec::new();
    };
    let mut cards: Vec<_> = entries
        .flatten()
        .filter(|entry| is_card_name(&entry.file_name().to_string_lossy()))
        .map(|entry| entry.path())
        .collect();
    cards.sort();
    cards.iter().filter_map(|card| read_gpu(&card.join("device"), pci_ids.as_deref())).collect()
}

/// True for `card0` but not connectors such as `card0-DP-1`.
fn is_card_name(name: &str) -> bool {
    name.strip_prefix("card").is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
}

fn read_gpu(device: &Path, pci_ids: Option<&str>) -> Option<Gpu> {
    let id = |file: &str| read(device.join(file)).map(|text| text.trim().trim_start_matches("0x").to_lowercase());
    let vendor_id = id("vendor")?;
    let device_id = id("device")?;
    let number = |file: &str| read(device.join(file)).and_then(|text| text.trim().parse().ok());
    Some(Gpu {
        vendor: match vendor_id.as_str() {
            "1002" => GpuVendor::Amd,
            "10de" => GpuVendor::Nvidia,
            "8086" => GpuVendor::Intel,
            _ => GpuVendor::Other,
        },
        name: pci_ids.and_then(|ids| lookup_pci_name(ids, &vendor_id, &device_id)),
        pci_id: format!("{vendor_id}:{device_id}"),
        driver: std::fs::read_link(device.join("driver"))
            .ok()
            .and_then(|link| Some(link.file_name()?.to_string_lossy().into_owned())),
        // Only some drivers publish these. Absent means unknown, not zero.
        vram_total_bytes: number("mem_info_vram_total"),
        vram_used_bytes: number("mem_info_vram_used"),
    })
}

pub fn compute() -> Compute {
    let render_nodes = std::fs::read_dir("/dev/dri")
        .map(|entries| {
            entries.flatten().filter(|entry| entry.file_name().to_string_lossy().starts_with("renderD")).count()
        })
        .unwrap_or(0);
    let runtimes_on_path =
        ["rocminfo", "nvidia-smi", "vulkaninfo", "clinfo"].into_iter().filter(|tool| on_path(tool)).collect();
    Compute { kfd_present: Path::new("/dev/kfd").exists(), render_nodes, runtimes_on_path }
}

/// Reports the filesystem holding `path`, or the nearest parent that exists yet.
pub fn disk(path: &Path) -> Option<Disk> {
    let existing = path.ancestors().find(|ancestor| ancestor.exists())?;
    let stats = rustix::fs::statvfs(existing).ok()?;
    Some(Disk {
        path: existing.to_owned(),
        available_bytes: stats.f_bavail.saturating_mul(stats.f_frsize),
        total_bytes: stats.f_blocks.saturating_mul(stats.f_frsize),
    })
}

pub fn tools() -> Tools {
    Tools {
        podman: on_path("podman"),
        docker: on_path("docker"),
        git: on_path("git"),
        bwrap: on_path("bwrap"),
        landlock_active: read("/sys/kernel/security/lsm")
            .map(|text| text.trim().split(',').any(|lsm| lsm == "landlock")),
    }
}

pub fn terminal() -> Terminal {
    let var = |name: &str| std::env::var(name).ok().filter(|value| !value.is_empty());
    let emulator = var("TERMINAL_EMULATOR")
        .or_else(|| var("TERM_PROGRAM"))
        .or_else(|| var("KONSOLE_VERSION").map(|version| format!("Konsole {version}")));
    Terminal {
        term: var("TERM"),
        truecolor: var("COLORTERM").is_some_and(|value| value == "truecolor" || value == "24bit"),
        emulator,
        tmux: var("TMUX").is_some(),
    }
}

/// Reports whether an executable with this name is on `PATH`.
pub fn on_path(tool: &str) -> bool {
    std::env::var_os("PATH").is_some_and(|path| std::env::split_paths(&path).any(|dir| is_executable(&dir.join(tool))))
}

fn is_executable(path: &Path) -> bool {
    let Ok(metadata) = std::fs::metadata(path) else {
        return false;
    };
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

fn read(path: impl AsRef<Path>) -> Option<String> {
    std::fs::read_to_string(path).ok()
}

fn parse_os_release(text: &str) -> Option<String> {
    text.lines().find_map(|line| line.strip_prefix("PRETTY_NAME=")).map(|value| value.trim_matches('"').to_owned())
}

fn parse_cpuinfo(text: &str) -> Cpu {
    let field = |line: &str| line.split_once(':').map(|(_, value)| value.trim().to_owned());
    let mut cpu = Cpu::default();
    let mut threads = 0;
    let mut physical = String::new();
    let mut cores = HashSet::new();
    for line in text.lines() {
        if line.starts_with("processor") {
            threads += 1;
        } else if line.starts_with("model name") && cpu.model.is_none() {
            cpu.model = field(line);
        } else if line.starts_with("physical id") {
            physical = field(line).unwrap_or_default();
        } else if line.starts_with("core id") {
            cores.insert((physical.clone(), field(line).unwrap_or_default()));
        } else if line.starts_with("flags") && cpu.features.is_empty() {
            let flags: HashSet<&str> =
                line.split_once(':').map(|(_, value)| value.split_whitespace().collect()).unwrap_or_default();
            cpu.features = ["avx", "avx2", "fma", "f16c", "avx512f", "avx512_vnni", "amx_tile"]
                .into_iter()
                .filter(|feature| flags.contains(feature))
                .map(str::to_owned)
                .collect();
        }
    }
    cpu.threads = (threads > 0).then_some(threads);
    cpu.cores = (!cores.is_empty()).then_some(cores.len());
    cpu
}

fn parse_meminfo(text: &str) -> Memory {
    let kib = |key: &str| {
        text.lines()
            .find_map(|line| line.strip_prefix(key)?.strip_prefix(':'))
            .and_then(|value| value.split_whitespace().next()?.parse::<u64>().ok())
            .map(|value| value * 1024)
    };
    Memory {
        total_bytes: kib("MemTotal"),
        available_bytes: kib("MemAvailable"),
        swap_used_bytes: kib("SwapTotal").zip(kib("SwapFree")).map(|(total, free)| total.saturating_sub(free)),
    }
}

/// Finds a device name in the `pci.ids` database format.
fn lookup_pci_name(ids: &str, vendor: &str, device: &str) -> Option<String> {
    let mut in_vendor = false;
    for line in ids.lines() {
        if line.starts_with('#') || line.is_empty() {
            continue;
        }
        if !line.starts_with('\t') {
            if in_vendor {
                return None;
            }
            in_vendor = line.split_whitespace().next() == Some(vendor);
        } else if in_vendor && !line.starts_with("\t\t") {
            let (id, name) = line.trim_start().split_once(char::is_whitespace)?;
            if id == device {
                return Some(name.trim().to_owned());
            }
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn cpuinfo_counts_physical_cores_and_threads_separately() {
        let text = "\
processor\t: 0\nmodel name\t: Test CPU\nphysical id\t: 0\ncore id\t: 0\nflags\t\t: fpu avx avx2 fma\n\n\
processor\t: 1\nmodel name\t: Test CPU\nphysical id\t: 0\ncore id\t: 0\nflags\t\t: fpu avx avx2 fma\n\n\
processor\t: 2\nmodel name\t: Test CPU\nphysical id\t: 0\ncore id\t: 1\nflags\t\t: fpu avx avx2 fma\n\n\
processor\t: 3\nmodel name\t: Test CPU\nphysical id\t: 1\ncore id\t: 0\nflags\t\t: fpu avx avx2 fma\n";
        let cpu = parse_cpuinfo(text);
        assert_eq!(cpu.model.as_deref(), Some("Test CPU"));
        assert_eq!((cpu.cores, cpu.threads), (Some(3), Some(4)));
        assert_eq!(cpu.features, ["avx", "avx2", "fma"]);
    }

    #[test]
    fn cpuinfo_without_details_is_unknown_not_zero() {
        assert_eq!(parse_cpuinfo("nonsense\n"), Cpu::default());
    }

    #[test]
    fn meminfo_reports_available_and_swap_in_use() {
        let memory = parse_meminfo("MemTotal: 1024 kB\nMemAvailable: 512 kB\nSwapTotal: 100 kB\nSwapFree: 40 kB\n");
        assert_eq!(memory.total_bytes, Some(1024 * 1024));
        assert_eq!(memory.available_bytes, Some(512 * 1024));
        assert_eq!(memory.swap_used_bytes, Some(60 * 1024));
    }

    #[test]
    fn os_release_name_is_unquoted() {
        assert_eq!(
            parse_os_release("ID=fedora\nPRETTY_NAME=\"Fedora Linux 44\"\n").as_deref(),
            Some("Fedora Linux 44")
        );
    }

    #[test]
    fn only_whole_cards_count_as_gpus() {
        assert!(is_card_name("card1"));
        assert!(!is_card_name("card1-DP-1"));
        assert!(!is_card_name("card"));
        assert!(!is_card_name("renderD128"));
    }

    #[test]
    fn pci_names_match_within_the_right_vendor_only() {
        let ids = "# comment\n1002  AMD\n\t73af  Navi 21\n\t\t1002 0e3a  Some subsystem\n10de  NVIDIA\n\t2204  GA102\n";
        assert_eq!(lookup_pci_name(ids, "1002", "73af").as_deref(), Some("Navi 21"));
        assert_eq!(lookup_pci_name(ids, "1002", "2204"), None);
        assert_eq!(lookup_pci_name(ids, "1002", "0e3a"), None);
        assert_eq!(lookup_pci_name(ids, "ffff", "73af"), None);
    }

    #[test]
    fn gpu_without_vram_files_reports_unknown() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("vendor"), "0x10de\n").unwrap();
        std::fs::write(dir.path().join("device"), "0x2204\n").unwrap();
        let gpu = read_gpu(dir.path(), None).unwrap();
        assert_eq!(gpu.vendor, GpuVendor::Nvidia);
        assert_eq!(gpu.pci_id, "10de:2204");
        assert_eq!(gpu.vram_total_bytes, None);
    }

    #[test]
    fn disk_uses_the_nearest_existing_parent() {
        let dir = tempfile::tempdir().unwrap();
        let disk = disk(&dir.path().join("not/yet/created")).unwrap();
        assert_eq!(disk.path, dir.path());
        assert!(disk.total_bytes > 0);
    }
}
