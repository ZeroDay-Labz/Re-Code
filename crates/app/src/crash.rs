//! What happens when re-code itself fails: a crash file with the message, the place in the code, the
//! version and the system, and nothing from the conversation or your files. `re-code doctor --report`
//! writes a bundle for a bug report the same way, with secrets hidden from the log lines it includes.

use crate::paths::Paths;
use std::path::{Path, PathBuf};
use std::time::{SystemTime, UNIX_EPOCH};

/// The folder crash files and reports go in: next to the logs.
fn state_dir(paths: &Paths) -> PathBuf {
    paths.log_dir.parent().map(Path::to_path_buf).unwrap_or_else(|| paths.log_dir.clone())
}

fn millis() -> u128 {
    SystemTime::now().duration_since(UNIX_EPOCH).map(|d| d.as_millis()).unwrap_or(0)
}

/// The version, commit and build date, as `--version` prints them.
pub fn version() -> &'static str {
    concat!(env!("CARGO_PKG_VERSION"), " (", env!("RE_CODE_GIT_SHA"), ", ", env!("RE_CODE_BUILD_DATE"), ")")
}

/// The operating system, from `/etc/os-release` and the kernel release.
pub fn system() -> String {
    let os = std::fs::read_to_string("/etc/os-release")
        .ok()
        .and_then(|text| {
            text.lines().find_map(|line| line.strip_prefix("PRETTY_NAME=").map(|v| v.trim_matches('"').to_owned()))
        })
        .unwrap_or_else(|| "unknown OS".to_owned());
    let kernel = std::fs::read_to_string("/proc/sys/kernel/osrelease").map(|k| k.trim().to_owned()).unwrap_or_default();
    if kernel.is_empty() { os } else { format!("{os}, kernel {kernel}") }
}

/// Installs a panic hook that writes a crash file and names it on stderr once the terminal has been
/// restored (the interface's own hook runs first and chains to this one).
pub fn install_hook(paths: &Paths) {
    let dir = state_dir(paths);
    let previous = std::panic::take_hook();
    std::panic::set_hook(Box::new(move |info| {
        let message = info
            .payload()
            .downcast_ref::<&str>()
            .map(|s| (*s).to_owned())
            .or_else(|| info.payload().downcast_ref::<String>().cloned())
            .unwrap_or_else(|| "(no message)".to_owned());
        // Rust ignores SIGPIPE, so `re-code skill list | head` ends in a print panic when head closes
        // the pipe. That's not a crash: leave quietly with the exit code a SIGPIPE death would give.
        if message.contains("Broken pipe") {
            std::process::exit(141);
        }
        let location = info.location().map(|l| format!("{}:{}:{}", l.file(), l.line(), l.column())).unwrap_or_default();
        let text = format!(
            "re-code crashed.\n\nversion: {}\nsystem:  {}\nwhere:   {location}\nmessage: {message}\n\nThis file holds no prompts, replies or file contents. Please attach it to a bug report at\nhttps://github.com/ZeroDay-Labz/Re-Code/issues.\n",
            version(),
            system()
        );
        let path = dir.join(format!("crash-{}.txt", millis()));
        let written = std::fs::create_dir_all(&dir).and_then(|()| std::fs::write(&path, text)).is_ok();
        previous(info);
        if written {
            eprintln!(
                "\nA crash file was written to {}. It contains no conversation or file contents.",
                path.display()
            );
        }
    }));
}

/// Writes a bug-report bundle: the doctor report, the version and system, the last log lines with
/// secrets hidden, and the contents of any crash files. Returns the path.
pub fn write_report(paths: &Paths, doctor_json: &str) -> anyhow::Result<PathBuf> {
    let dir = state_dir(paths);
    std::fs::create_dir_all(&dir)?;
    let mut text =
        format!("re-code report\n\nversion: {}\nsystem:  {}\n\n## doctor\n\n{doctor_json}\n", version(), system());
    text.push_str("\n## recent log lines (secrets hidden)\n\n");
    let mut logs: Vec<PathBuf> = std::fs::read_dir(&paths.log_dir)
        .map(|entries| entries.flatten().map(|e| e.path()).filter(|p| p.is_file()).collect())
        .unwrap_or_default();
    logs.sort();
    if let Some(latest) = logs.last()
        && let Ok(log) = std::fs::read_to_string(latest)
    {
        let lines: Vec<&str> = log.lines().collect();
        let tail = lines.len().saturating_sub(200);
        for line in &lines[tail..] {
            text.push_str(&crate::compact::redact(line));
            text.push('\n');
        }
    } else {
        text.push_str("(no log yet)\n");
    }
    text.push_str("\n## crash files\n\n");
    let mut crashes: Vec<PathBuf> = std::fs::read_dir(&dir)
        .map(|entries| {
            entries
                .flatten()
                .map(|e| e.path())
                .filter(|p| p.file_name().and_then(|n| n.to_str()).is_some_and(|n| n.starts_with("crash-")))
                .collect()
        })
        .unwrap_or_default();
    crashes.sort();
    if crashes.is_empty() {
        text.push_str("(none)\n");
    }
    for crash in crashes.iter().rev().take(5) {
        text.push_str(&format!("### {}\n{}\n", crash.display(), std::fs::read_to_string(crash).unwrap_or_default()));
    }
    let path = dir.join(format!("report-{}.txt", millis()));
    std::fs::write(&path, text)?;
    Ok(path)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn version_and_system_read_well() {
        assert!(version().starts_with(env!("CARGO_PKG_VERSION")));
        assert!(version().contains('(') && version().contains(','));
        assert!(!system().is_empty());
    }
}
