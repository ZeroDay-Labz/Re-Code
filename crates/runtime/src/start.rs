//! Starting a runtime the user has approved. This is the only write action in the crate: it runs
//! exactly the command that was shown, then waits for the API to answer. There is deliberately no
//! stop, restart or reconfigure here.

use crate::known::RuntimeKind;
use crate::{StartMethod, probe};
use std::path::Path;
use std::time::Duration;
use tokio::process::Command;

/// How long a start command itself may run before it's considered stuck.
pub const COMMAND_TIMEOUT: Duration = Duration::from_secs(30);
/// How long to wait for the API after the command has returned.
pub const READY_TIMEOUT: Duration = Duration::from_secs(90);

/// What the start command did.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Started {
    /// A service manager or container engine command ran and returned.
    Command { command_line: String },
    /// A program was launched directly and left running, detached from re-code.
    Process { pid: u32, log: Option<String> },
}

/// Runs the approved start command. For a bare program (`ollama serve`) the process is detached in
/// its own process group with its output in `log_dir`, so closing re-code doesn't stop it.
pub async fn start(method: &StartMethod, log_dir: Option<&Path>) -> Result<Started, String> {
    let argv = method.argv();
    let (program, args) = argv.split_first().ok_or("empty command")?;
    match method {
        StartMethod::Binary { .. } => {
            let mut command = Command::new(program);
            command.args(args).stdin(std::process::Stdio::null());
            let mut log_path = None;
            match log_dir {
                Some(dir) => {
                    let path = dir.join(format!("{}.log", program.rsplit('/').next().unwrap_or(program)));
                    let file = std::fs::OpenOptions::new()
                        .create(true)
                        .append(true)
                        .open(&path)
                        .map_err(|error| format!("couldn't open {}: {error}", path.display()))?;
                    let err = file.try_clone().map_err(|error| error.to_string())?;
                    command.stdout(file).stderr(err);
                    log_path = Some(path.display().to_string());
                }
                None => {
                    command.stdout(std::process::Stdio::null()).stderr(std::process::Stdio::null());
                }
            }
            #[cfg(unix)]
            command.process_group(0);
            let child =
                command.spawn().map_err(|error| format!("couldn't run `{}`: {error}", method.command_line()))?;
            let pid = child.id().ok_or("the program exited at once")?;
            // Dropping the handle leaves the process running; re-code never kills what it started for the user.
            drop(child);
            Ok(Started::Process { pid, log: log_path })
        }
        _ => {
            let mut command = Command::new(program);
            command.args(args).stdin(std::process::Stdio::null());
            let output = tokio::time::timeout(COMMAND_TIMEOUT, command.output())
                .await
                .map_err(|_| {
                    format!("`{}` didn't finish within {} s", method.command_line(), COMMAND_TIMEOUT.as_secs())
                })?
                .map_err(|error| format!("couldn't run `{}`: {error}", method.command_line()))?;
            if output.status.success() {
                Ok(Started::Command { command_line: method.command_line() })
            } else {
                let stderr = String::from_utf8_lossy(&output.stderr);
                let reason = stderr.lines().find(|line| !line.trim().is_empty()).unwrap_or("no message");
                Err(format!("`{}` failed: {reason}", method.command_line()))
            }
        }
    }
}

/// Polls `url` until something of `kind` answers, calling `on_wait` with the seconds elapsed every
/// few seconds so the interface can show that it's still waiting.
pub async fn wait_until_answering(
    url: &str,
    kind: RuntimeKind,
    deadline: Duration,
    mut on_wait: impl FnMut(u64),
) -> Result<Option<String>, String> {
    let started = std::time::Instant::now();
    let mut last_report = 0;
    loop {
        if let probe::Outcome::Identified { kind: found, version } = probe::identify(url).await {
            if found == kind {
                return Ok(version);
            }
            return Err(format!("{url} answered, but as {} rather than {}", found.name(), kind.name()));
        }
        let elapsed = started.elapsed();
        if elapsed >= deadline {
            return Err(format!(
                "nothing answered at {url} after {} s. It may still be loading; the Runtimes tab shows when it's up.",
                deadline.as_secs()
            ));
        }
        let seconds = elapsed.as_secs();
        if seconds >= last_report + 5 {
            last_report = seconds;
            on_wait(seconds);
        }
        tokio::time::sleep(Duration::from_secs(1)).await;
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[tokio::test]
    async fn waiting_ends_when_the_right_runtime_answers() {
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
        let version = wait_until_answering(&server.uri(), RuntimeKind::Ollama, Duration::from_secs(5), |_| {}).await;
        assert_eq!(version, Ok(Some("0.40.14".into())));
        let wrong = wait_until_answering(&server.uri(), RuntimeKind::Vllm, Duration::from_secs(5), |_| {}).await;
        assert!(wrong.unwrap_err().contains("Ollama rather than vLLM"));
    }

    #[tokio::test]
    async fn waiting_gives_up_at_the_deadline_with_a_plain_message() {
        let result =
            wait_until_answering("http://127.0.0.1:9", RuntimeKind::Ollama, Duration::from_millis(1500), |_| {}).await;
        assert!(result.unwrap_err().contains("nothing answered"));
    }

    #[tokio::test]
    async fn a_failing_command_reports_its_first_error_line() {
        let method = StartMethod::ContainerStart {
            engine: crate::Engine::Podman,
            name: "no-such-container-re-code-test".into(),
        };
        // `false` stands in for an engine that isn't there; either way the result must be an error naming the command.
        let result = if crate::processes::Processes::on_path(&crate::processes::RealProcesses, "podman") {
            start(&method, None).await
        } else {
            start(&StartMethod::Binary { argv: vec!["false".into()] }, None).await.and_then(|_| Err("ran".to_owned()))
        };
        let error = result.expect_err("starting a missing container fails");
        assert!(error.contains("podman start") || error.contains("ran"), "{error}");
    }
}
