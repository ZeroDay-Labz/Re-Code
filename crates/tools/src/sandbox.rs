//! The OS sandbox for commands the agent runs (decision D5 in `docs/SECURITY.md`).
//!
//! Commands run under bubblewrap: the filesystem is read-only except the workspace, the home
//! directory is replaced by an empty one with only toolchain directories bound back read-only,
//! `/run` (D-Bus, keyring, Podman and Wayland sockets) is hidden, the network is off unless asked
//! for, the environment is an allowlist, and the command gets its own PID namespace so killing it
//! kills everything it started.
//!
//! When bubblewrap can't run, [`isolation`] says so and why. Commands then run without a sandbox,
//! and callers must tell the user that plainly; nothing here falls back silently.

use crate::Workspace;
use std::io::Read;
use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};
use std::sync::OnceLock;
use std::sync::atomic::{AtomicBool, Ordering};
use std::time::{Duration, Instant};

/// Whether commands can be sandboxed on this machine.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Isolation {
    /// bubblewrap works.
    Bubblewrap,
    /// No sandbox is available, with the reason in plain words.
    Unavailable { reason: String },
}

impl Isolation {
    #[must_use]
    pub fn is_sandboxed(&self) -> bool {
        matches!(self, Self::Bubblewrap)
    }
}

/// Whether commands can be sandboxed here, checked once by actually starting a sandbox.
pub fn isolation() -> &'static Isolation {
    static ISOLATION: OnceLock<Isolation> = OnceLock::new();
    ISOLATION.get_or_init(probe)
}

fn probe() -> Isolation {
    let output = Command::new("bwrap")
        .args(["--ro-bind", "/", "/", "--unshare-all", "--die-with-parent", "true"])
        .stdin(Stdio::null())
        .output();
    match output {
        Ok(output) if output.status.success() => Isolation::Bubblewrap,
        Ok(output) => Isolation::Unavailable {
            reason: format!(
                "bubblewrap is installed but couldn't start a sandbox: {}",
                String::from_utf8_lossy(&output.stderr).trim()
            ),
        },
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            Isolation::Unavailable { reason: "bubblewrap isn't installed (sudo dnf install bubblewrap)".to_owned() }
        }
        Err(error) => Isolation::Unavailable { reason: format!("couldn't run bubblewrap: {error}") },
    }
}

/// What a command is allowed beyond the defaults.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Policy {
    /// Allow network access. Off by default.
    pub network: bool,
}

/// A folder outside the project that the user has allowed, made visible to commands too.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Grant {
    pub path: PathBuf,
    /// Read-write, or read-only.
    pub writable: bool,
}

/// How a command ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Finished {
    /// The exit code, or `None` if it was killed.
    pub code: Option<i32>,
    /// Standard output and error, in that order.
    pub output: String,
    /// Standard output alone, for callers that read a protocol from it.
    pub stdout: String,
    /// Standard error alone.
    pub stderr: String,
    pub timed_out: bool,
    pub cancelled: bool,
    /// Whether it ran inside the sandbox.
    pub sandboxed: bool,
}

impl Finished {
    #[must_use]
    pub fn success(&self) -> bool {
        self.code == Some(0) && !self.timed_out && !self.cancelled
    }
}

/// Toolchain locations under `$HOME` made visible, read-only, inside the sandbox.
const TOOLCHAINS: &[&str] = &[
    ".cargo",
    ".rustup",
    ".local/bin",
    ".local/lib",
    ".local/share/mise",
    ".nvm",
    ".volta",
    ".bun",
    ".deno",
    ".pyenv",
    "go",
    ".gitconfig",
    ".config/git",
];

/// Toolchain caches that become writable when the network is allowed, so downloads can land.
const NETWORK_CACHES: &[&str] = &[".cargo/registry", ".cargo/git", ".npm"];

use crate::sensitive::HOME_SECRETS as SECRETS;

/// Environment variables passed through. Everything else, including API keys and tokens, is dropped.
const ENV_ALLOW: &[&str] = &[
    "PATH",
    "HOME",
    "USER",
    "LOGNAME",
    "LANG",
    "LANGUAGE",
    "TERM",
    "TZ",
    "CARGO_HOME",
    "RUSTUP_HOME",
    "RUSTUP_TOOLCHAIN",
    "GOPATH",
    "GOROOT",
    "JAVA_HOME",
    "VIRTUAL_ENV",
    "NODE_PATH",
    "PYTHONPATH",
];

/// Whether a variable name looks like it holds a secret; such variables are dropped even when allowed.
fn looks_secret(name: &str) -> bool {
    let upper = name.to_ascii_uppercase();
    ["KEY", "TOKEN", "SECRET", "PASSWORD", "CREDENTIAL"].iter().any(|word| upper.contains(word))
}

/// The environment a sandboxed command gets.
fn allowed_env() -> Vec<(String, String)> {
    let mut env: Vec<(String, String)> = std::env::vars()
        .filter(|(name, _)| (ENV_ALLOW.contains(&name.as_str()) || name.starts_with("LC_")) && !looks_secret(name))
        .collect();
    env.push(("TMPDIR".to_owned(), "/tmp".to_owned()));
    env
}

/// The bubblewrap arguments for running `argv` in `workspace` under `policy`.
#[must_use]
pub fn bwrap_args(
    workspace: &Path,
    home: Option<&Path>,
    argv: &[String],
    policy: Policy,
    grants: &[Grant],
) -> Vec<String> {
    fn push(args: &mut Vec<String>, items: &[&str]) {
        args.extend(items.iter().map(|item| (*item).to_owned()));
    }
    let mut args: Vec<String> = Vec::new();
    push(
        &mut args,
        &["--ro-bind", "/", "/", "--dev", "/dev", "--proc", "/proc", "--tmpfs", "/tmp", "--tmpfs", "/var/tmp"],
    );
    push(&mut args, &["--tmpfs", "/run"]);
    if policy.network {
        // Fedora's /etc/resolv.conf points into /run, so DNS needs the resolver's directory back.
        push(&mut args, &["--ro-bind-try", "/run/systemd/resolve", "/run/systemd/resolve"]);
    }

    let mut visible: Vec<PathBuf> = vec![workspace.to_path_buf()];
    if let Some(home) = home {
        let home_text = home.to_string_lossy().into_owned();
        args.extend(["--tmpfs".to_owned(), home_text]);
        for entry in TOOLCHAINS {
            let path = home.join(entry);
            if path.exists() {
                let text = path.to_string_lossy().into_owned();
                args.extend(["--ro-bind".to_owned(), text.clone(), text]);
                visible.push(path);
            }
        }
        if policy.network {
            for entry in NETWORK_CACHES {
                let path = home.join(entry);
                if path.is_dir() {
                    let text = path.to_string_lossy().into_owned();
                    args.extend(["--bind".to_owned(), text.clone(), text]);
                }
            }
        }
    }

    let root = workspace.to_string_lossy().into_owned();
    args.extend(["--bind".to_owned(), root.clone(), root.clone()]);
    // Folders the user allowed outside the project, read-write or read-only as granted.
    for grant in grants.iter().filter(|grant| grant.path.exists()) {
        let path = grant.path.to_string_lossy().into_owned();
        let bind = if grant.writable { "--bind" } else { "--ro-bind" };
        args.extend([bind.to_owned(), path.clone(), path]);
        visible.push(grant.path.clone());
    }

    // Hide secrets that would otherwise be visible through the workspace or a toolchain directory.
    if let Some(home) = home {
        for entry in SECRETS {
            let path = home.join(entry);
            if !visible.iter().any(|dir| path.starts_with(dir)) {
                continue;
            }
            let text = path.to_string_lossy().into_owned();
            if path.is_dir() {
                args.extend(["--tmpfs".to_owned(), text]);
            } else if path.exists() {
                args.extend(["--ro-bind".to_owned(), "/dev/null".to_owned(), text]);
            }
        }
    }

    args.extend(["--chdir".to_owned(), root]);
    push(&mut args, &["--unshare-all"]);
    if policy.network {
        push(&mut args, &["--share-net"]);
    }
    push(&mut args, &["--die-with-parent", "--new-session", "--clearenv"]);
    for (name, value) in allowed_env() {
        args.extend(["--setenv".to_owned(), name, value]);
    }
    args.extend(argv.iter().cloned());
    args
}

/// Runs `argv` in the workspace — in the sandbox when one is available — until it exits, the
/// timeout passes, or `cancel` is set. A timed-out or cancelled command is killed along with
/// everything it started.
pub fn run(
    workspace: &Workspace,
    argv: &[String],
    policy: Policy,
    grants: &[Grant],
    cancel: &AtomicBool,
    timeout: Duration,
) -> Finished {
    run_with_input(workspace, argv, policy, grants, cancel, timeout, None)
}

/// As [`run`], with text for the command's standard input (closed when `None`).
pub fn run_with_input(
    workspace: &Workspace,
    argv: &[String],
    policy: Policy,
    grants: &[Grant],
    cancel: &AtomicBool,
    timeout: Duration,
    input: Option<&str>,
) -> Finished {
    let sandboxed = isolation().is_sandboxed();
    let mut command = if sandboxed {
        let home = std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute());
        let mut command = Command::new("bwrap");
        command.args(bwrap_args(workspace.root(), home.as_deref(), argv, policy, grants));
        command
    } else {
        let Some((program, rest)) = argv.split_first() else {
            return failed("there's no command to run", sandboxed);
        };
        let mut command = Command::new(program);
        command.args(rest).current_dir(workspace.root());
        command
    };
    run_command(&mut command, cancel, timeout, sandboxed, input)
}

/// Starts a command without waiting for it: the same sandbox and process group as [`run`], with its
/// output piped for the caller to read. For commands that run in the background.
pub fn spawn(
    workspace: &Workspace,
    argv: &[String],
    policy: Policy,
    grants: &[Grant],
) -> std::io::Result<std::process::Child> {
    use std::os::unix::process::CommandExt;
    let mut command = if isolation().is_sandboxed() {
        let home = std::env::var_os("HOME").map(PathBuf::from).filter(|home| home.is_absolute());
        let mut command = Command::new("bwrap");
        command.args(bwrap_args(workspace.root(), home.as_deref(), argv, policy, grants));
        command
    } else {
        let Some((program, rest)) = argv.split_first() else {
            return Err(std::io::Error::other("there's no command to run"));
        };
        let mut command = Command::new(program);
        command.args(rest).current_dir(workspace.root());
        command
    };
    command.stdin(Stdio::null()).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    command.spawn()
}

fn failed(message: &str, sandboxed: bool) -> Finished {
    Finished {
        code: None,
        output: message.to_owned(),
        stdout: String::new(),
        stderr: message.to_owned(),
        timed_out: false,
        cancelled: false,
        sandboxed,
    }
}

fn run_command(
    command: &mut Command,
    cancel: &AtomicBool,
    timeout: Duration,
    sandboxed: bool,
    input: Option<&str>,
) -> Finished {
    use std::os::unix::process::CommandExt;
    let stdin = if input.is_some() { Stdio::piped() } else { Stdio::null() };
    command.stdin(stdin).stdout(Stdio::piped()).stderr(Stdio::piped()).process_group(0);
    let mut child = match command.spawn() {
        Ok(child) => child,
        Err(error) => return failed(&format!("couldn't start the command: {error}"), sandboxed),
    };
    if let (Some(text), Some(mut pipe)) = (input, child.stdin.take()) {
        let text = text.to_owned();
        std::thread::spawn(move || {
            use std::io::Write;
            let _ = pipe.write_all(text.as_bytes());
        });
    }
    // Read both pipes on their own threads so a chatty command can't fill one and stall.
    let drain = |pipe: Option<Box<dyn Read + Send>>| {
        std::thread::spawn(move || {
            let mut bytes = Vec::new();
            if let Some(mut pipe) = pipe {
                let _ = pipe.read_to_end(&mut bytes);
            }
            bytes
        })
    };
    let stdout = drain(child.stdout.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));
    let stderr = drain(child.stderr.take().map(|pipe| Box::new(pipe) as Box<dyn Read + Send>));

    let deadline = Instant::now() + timeout;
    let (mut timed_out, mut cancelled) = (false, false);
    let status = loop {
        match child.try_wait() {
            Ok(Some(status)) => break Some(status),
            Ok(None) => {}
            Err(_) => break None,
        }
        cancelled = cancel.load(Ordering::Relaxed);
        timed_out = Instant::now() >= deadline;
        if cancelled || timed_out {
            kill_group(&child);
            break child.wait().ok();
        }
        std::thread::sleep(Duration::from_millis(25));
    };
    // Anything the command left running is in its process group; stop it so the pipes close.
    kill_group(&child);
    let stdout = String::from_utf8_lossy(&stdout.join().unwrap_or_default()).into_owned();
    let stderr = String::from_utf8_lossy(&stderr.join().unwrap_or_default()).into_owned();
    let mut output = stdout.clone();
    output.push_str(&stderr);
    let code = if timed_out || cancelled { None } else { status.and_then(|status| status.code()) };
    Finished { code, output, stdout, stderr, timed_out, cancelled, sandboxed }
}

/// Stops a command and everything it started (its process group).
pub fn kill_group(child: &std::process::Child) {
    if let Some(pid) = rustix::process::Pid::from_raw(i32::try_from(child.id()).unwrap_or(0)) {
        let _ = rustix::process::kill_process_group(pid, rustix::process::Signal::KILL);
    }
}

/// A hint for the model when a failure looks like the sandbox's doing.
#[must_use]
pub fn hint(finished: &Finished, policy: Policy) -> Option<&'static str> {
    if !finished.sandboxed || finished.success() {
        return None;
    }
    let text = finished.output.to_ascii_lowercase();
    let network_words = [
        "could not resolve host",
        "temporary failure in name resolution",
        "network is unreachable",
        "failed to lookup address",
        "name or service not known",
        "getaddrinfo",
    ];
    if !policy.network && network_words.iter().any(|word| text.contains(word)) {
        return Some(
            "Network access is off in the sandbox. If this command really needs the internet, run it again with \"network\": true — the user will be asked.",
        );
    }
    if text.contains("read-only file system") {
        return Some("The sandbox makes everything outside the project folder read-only.");
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    fn pairs(args: &[String]) -> Vec<(String, String, String)> {
        args.windows(3).map(|w| (w[0].clone(), w[1].clone(), w[2].clone())).collect()
    }

    #[test]
    fn the_network_is_off_unless_asked_for() {
        let argv = vec!["true".to_owned()];
        let off = bwrap_args(Path::new("/w"), None, &argv, Policy::default(), &[]);
        assert!(off.contains(&"--unshare-all".to_owned()));
        assert!(!off.contains(&"--share-net".to_owned()));
        let on = bwrap_args(Path::new("/w"), None, &argv, Policy { network: true }, &[]);
        assert!(on.contains(&"--share-net".to_owned()));
    }

    #[test]
    fn the_workspace_is_the_writable_bind_and_the_working_directory() {
        let args = bwrap_args(Path::new("/w"), None, &["ls".to_owned()], Policy::default(), &[]);
        assert!(pairs(&args).contains(&("--bind".to_owned(), "/w".to_owned(), "/w".to_owned())));
        let chdir = args.iter().position(|arg| arg == "--chdir").unwrap();
        assert_eq!(args[chdir + 1], "/w");
        assert_eq!(args.last().unwrap(), "ls", "the command comes last");
    }

    #[test]
    fn home_is_emptied_and_secrets_inside_the_workspace_are_hidden() {
        let home = tempfile::tempdir().unwrap();
        std::fs::create_dir(home.path().join(".ssh")).unwrap();
        std::fs::write(home.path().join(".netrc"), "machine x").unwrap();
        // Running in the home folder itself is the worst case: the workspace bind exposes everything.
        let args = bwrap_args(home.path(), Some(home.path()), &["true".to_owned()], Policy::default(), &[]);
        let home_text = home.path().to_string_lossy().into_owned();
        let ssh = home.path().join(".ssh").to_string_lossy().into_owned();
        let netrc = home.path().join(".netrc").to_string_lossy().into_owned();
        let tmpfs_home = args.windows(2).position(|w| w[0] == "--tmpfs" && w[1] == home_text).unwrap();
        let bind_ws = args.windows(3).position(|w| w[0] == "--bind" && w[1] == home_text).unwrap();
        let hide_ssh = args.windows(2).position(|w| w[0] == "--tmpfs" && w[1] == ssh).expect("~/.ssh hidden");
        let hide_netrc =
            args.windows(3).position(|w| w[0] == "--ro-bind" && w[1] == "/dev/null" && w[2] == netrc).unwrap();
        assert!(tmpfs_home < bind_ws && bind_ws < hide_ssh && bind_ws < hide_netrc, "masks must come after binds");
    }

    #[test]
    fn granted_folders_are_bound_as_granted_and_their_secrets_stay_hidden() {
        let home = tempfile::tempdir().unwrap();
        let notes = home.path().join("notes");
        std::fs::create_dir_all(&notes).unwrap();
        let docs = home.path().join("docs");
        std::fs::create_dir_all(&docs).unwrap();
        let grants = [Grant { path: notes.clone(), writable: true }, Grant { path: docs.clone(), writable: false }];
        let args = bwrap_args(Path::new("/w"), Some(home.path()), &["true".to_owned()], Policy::default(), &grants);
        let text = |p: &Path| p.to_string_lossy().into_owned();
        assert!(pairs(&args).contains(&("--bind".to_owned(), text(&notes), text(&notes))));
        assert!(pairs(&args).contains(&("--ro-bind".to_owned(), text(&docs), text(&docs))));
        // Granting the home folder itself still doesn't reveal ~/.ssh.
        std::fs::create_dir(home.path().join(".ssh")).unwrap();
        let everything = [Grant { path: home.path().to_path_buf(), writable: true }];
        let args = bwrap_args(Path::new("/w"), Some(home.path()), &["true".to_owned()], Policy::default(), &everything);
        let ssh = text(&home.path().join(".ssh"));
        assert!(args.windows(2).any(|w| w[0] == "--tmpfs" && w[1] == ssh), "~/.ssh is masked");
    }

    #[test]
    fn credentials_and_tokens_are_dropped_from_the_environment() {
        assert!(looks_secret("OPENAI_API_KEY"));
        assert!(looks_secret("GITHUB_TOKEN"));
        assert!(!looks_secret("PATH"));
        let names: Vec<String> = allowed_env().into_iter().map(|(name, _)| name).collect();
        assert!(names.iter().all(|name| !looks_secret(name)));
        assert!(names.contains(&"TMPDIR".to_owned()));
    }

    #[test]
    fn a_network_failure_in_the_sandbox_gets_a_hint() {
        let finished = Finished {
            stdout: String::new(),
            stderr: String::new(),
            code: Some(6),
            output: "curl: (6) Could not resolve host: example.com".to_owned(),
            timed_out: false,
            cancelled: false,
            sandboxed: true,
        };
        assert!(hint(&finished, Policy::default()).unwrap().contains("network"));
        assert_eq!(hint(&finished, Policy { network: true }), None);
    }

    // These run real commands; they skip themselves where bubblewrap can't start.

    fn workspace() -> (tempfile::TempDir, Workspace) {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        (dir, workspace)
    }

    fn sh(script: &str) -> Vec<String> {
        vec!["sh".to_owned(), "-c".to_owned(), script.to_owned()]
    }

    #[test]
    fn a_sandboxed_command_writes_the_workspace_but_nothing_else() {
        if !isolation().is_sandboxed() {
            eprintln!("skipped: {:?}", isolation());
            return;
        }
        let (dir, workspace) = workspace();
        let never = AtomicBool::new(false);
        let finished = run(
            &workspace,
            &sh("echo made > inside.txt && touch /usr/escape 2>&1; ls \"$HOME/.ssh\" 2>&1; echo done"),
            Policy::default(),
            &[],
            &never,
            Duration::from_secs(30),
        );
        assert!(finished.sandboxed);
        assert_eq!(std::fs::read_to_string(dir.path().join("inside.txt")).unwrap(), "made\n");
        assert!(finished.output.contains("Read-only file system"), "{}", finished.output);
        assert!(finished.output.contains("No such file"), "~/.ssh must not be visible: {}", finished.output);
        assert!(!Path::new("/usr/escape").exists());
    }

    #[test]
    fn a_sandboxed_command_has_no_network_by_default() {
        if !isolation().is_sandboxed() {
            return;
        }
        let (_dir, workspace) = workspace();
        let never = AtomicBool::new(false);
        // With no network namespace access there is only a loopback device, and it's down.
        let finished =
            run(&workspace, &sh("cat /proc/net/dev"), Policy::default(), &[], &never, Duration::from_secs(30));
        let devices: Vec<&str> = finished.output.lines().skip(2).filter_map(|l| l.split(':').next()).collect();
        assert!(devices.iter().all(|d| d.trim() == "lo"), "only loopback expected: {}", finished.output);
    }

    #[test]
    fn a_command_is_killed_at_the_timeout_and_on_cancel() {
        let (_dir, workspace) = workspace();
        let never = AtomicBool::new(false);
        let started = Instant::now();
        let finished = run(&workspace, &sh("sleep 30"), Policy::default(), &[], &never, Duration::from_millis(300));
        assert!(finished.timed_out && !finished.success());
        assert!(started.elapsed() < Duration::from_secs(10));

        let cancel = AtomicBool::new(true);
        let finished = run(&workspace, &sh("sleep 30"), Policy::default(), &[], &cancel, Duration::from_secs(60));
        assert!(finished.cancelled);
    }

    #[test]
    fn background_children_die_with_the_command() {
        let (dir, workspace) = workspace();
        let never = AtomicBool::new(false);
        // The background job would write the file after 1s if it survived.
        let script = "(sleep 1; echo late > late.txt) & echo started";
        let finished = run(&workspace, &sh(script), Policy::default(), &[], &never, Duration::from_secs(30));
        assert!(finished.success(), "{finished:?}");
        std::thread::sleep(Duration::from_millis(1500));
        assert!(!dir.path().join("late.txt").exists(), "a background child outlived the command");
    }
}

#[cfg(test)]
mod toolchain_tests {
    use super::*;

    #[test]
    fn cargo_builds_a_project_offline_in_the_sandbox() {
        if !isolation().is_sandboxed() || Command::new("cargo").arg("--version").output().is_err() {
            return;
        }
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(
            dir.path().join("Cargo.toml"),
            "[package]\nname = \"probe\"\nversion = \"0.1.0\"\nedition = \"2021\"\n",
        )
        .unwrap();
        std::fs::create_dir(dir.path().join("src")).unwrap();
        std::fs::write(dir.path().join("src/main.rs"), "fn main() { println!(\"built inside\"); }\n").unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let never = AtomicBool::new(false);
        let argv = vec!["sh".to_owned(), "-c".to_owned(), "cargo run --offline --quiet".to_owned()];
        let finished = run(&workspace, &argv, Policy::default(), &[], &never, Duration::from_secs(120));
        assert!(finished.success(), "cargo failed in the sandbox: {}", finished.output);
        assert!(finished.output.contains("built inside"), "{}", finished.output);
    }
}
