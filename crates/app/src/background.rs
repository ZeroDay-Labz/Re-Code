//! Background shell commands: a build or a server the model (or the user, with `!cmd &`) starts and
//! comes back to. Each runs in the same sandbox as a foreground command, in its own process group, with
//! its output collected as it arrives. Everything is killed when re-code exits.

use recode_core::message::{ToolCall, ToolDefinition};
use recode_tools::Workspace;
use recode_tools::sandbox::{self, Policy};
use serde_json::json;
use std::io::Read;
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::time::Instant;

pub const OUTPUT_TOOL: &str = "task_output";
pub const KILL_TOOL: &str = "task_kill";
/// The most output kept per task.
const OUTPUT_CAP: usize = 2 * 1024 * 1024;
/// The most of a task's output handed to the model at once.
const TAIL_CHARS: usize = 8_000;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum TaskStatus {
    Running,
    Finished { code: Option<i32> },
    Killed,
}

impl TaskStatus {
    #[must_use]
    pub fn label(&self) -> String {
        match self {
            Self::Running => "running".to_owned(),
            Self::Finished { code: Some(0) } => "finished".to_owned(),
            Self::Finished { code: Some(code) } => format!("exit {code}"),
            Self::Finished { code: None } => "ended".to_owned(),
            Self::Killed => "killed".to_owned(),
        }
    }
}

struct Task {
    id: String,
    command: String,
    started: Instant,
    child: Option<Child>,
    output: Arc<Mutex<String>>,
    status: TaskStatus,
    /// Whether the user has been told it ended.
    announced: bool,
}

/// The session's background commands.
#[derive(Default)]
pub struct BackgroundTasks {
    next: u32,
    tasks: Vec<Task>,
}

pub use recode_tui::link::TaskInfo;

impl BackgroundTasks {
    /// Whether anything is still running.
    #[must_use]
    pub fn any_running(&self) -> bool {
        self.tasks.iter().any(|task| task.status == TaskStatus::Running)
    }

    /// Starts a command in the background. Returns its id.
    pub fn spawn(
        &mut self,
        workspace: &Workspace,
        command: &str,
        network: bool,
        grants: &[sandbox::Grant],
    ) -> Result<String, String> {
        let argv = vec!["sh".to_owned(), "-c".to_owned(), command.to_owned()];
        let mut child = sandbox::spawn(workspace, &argv, Policy { network }, grants).map_err(|e| e.to_string())?;
        self.next += 1;
        let id = format!("b{}", self.next);
        let output = Arc::new(Mutex::new(String::new()));
        for pipe in [
            child.stdout.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
            child.stderr.take().map(|p| Box::new(p) as Box<dyn Read + Send>),
        ] {
            let Some(mut pipe) = pipe else { continue };
            let sink = Arc::clone(&output);
            std::thread::spawn(move || {
                let mut buffer = [0u8; 4096];
                loop {
                    match pipe.read(&mut buffer) {
                        Ok(0) | Err(_) => break,
                        Ok(n) => {
                            if let Ok(mut text) = sink.lock() {
                                text.push_str(&String::from_utf8_lossy(&buffer[..n]));
                                if text.len() > OUTPUT_CAP {
                                    let cut = text.len() - OUTPUT_CAP;
                                    let mut at = cut;
                                    while !text.is_char_boundary(at) {
                                        at += 1;
                                    }
                                    text.drain(..at);
                                }
                            }
                        }
                    }
                }
            });
        }
        self.tasks.push(Task {
            id: id.clone(),
            command: command.to_owned(),
            started: Instant::now(),
            child: Some(child),
            output,
            status: TaskStatus::Running,
            announced: false,
        });
        Ok(id)
    }

    /// Checks running tasks; returns the ones that ended since the last call, with their status.
    pub fn poll(&mut self) -> Vec<(String, TaskStatus, String)> {
        let mut ended = Vec::new();
        for task in &mut self.tasks {
            if task.status != TaskStatus::Running {
                continue;
            }
            if let Some(child) = &mut task.child
                && let Ok(Some(status)) = child.try_wait()
            {
                task.status = TaskStatus::Finished { code: status.code() };
                task.child = None;
            }
            if task.status != TaskStatus::Running && !task.announced {
                task.announced = true;
                ended.push((task.id.clone(), task.status.clone(), tail(&task.output, 600)));
            }
        }
        ended
    }

    /// The output of a task so far (the tail), for the model.
    #[must_use]
    pub fn output(&self, id: &str) -> Option<(TaskStatus, String)> {
        let task = self.tasks.iter().find(|task| task.id == id)?;
        Some((task.status.clone(), tail(&task.output, TAIL_CHARS)))
    }

    /// Stops a task. Returns whether it was running.
    pub fn kill(&mut self, id: &str) -> bool {
        let Some(task) = self.tasks.iter_mut().find(|task| task.id == id) else { return false };
        if task.status != TaskStatus::Running {
            return false;
        }
        if let Some(child) = &mut task.child {
            sandbox::kill_group(child);
            let _ = child.wait();
        }
        task.child = None;
        task.status = TaskStatus::Killed;
        task.announced = true;
        true
    }

    /// Every task, oldest first.
    #[must_use]
    pub fn list(&self) -> Vec<TaskInfo> {
        self.tasks
            .iter()
            .map(|task| TaskInfo {
                id: task.id.clone(),
                command: task.command.clone(),
                status: task.status.label(),
                elapsed_secs: task.started.elapsed().as_secs(),
                last_line: tail(&task.output, 400).lines().last().unwrap_or_default().to_owned(),
            })
            .collect()
    }

    /// One line per running task, for the model after a compaction or when it asks.
    #[must_use]
    pub fn describe_running(&self) -> Vec<String> {
        self.tasks
            .iter()
            .filter(|task| task.status == TaskStatus::Running)
            .map(|task| {
                format!("{} · `{}` · running for {} s", task.id, task.command, task.started.elapsed().as_secs())
            })
            .collect()
    }

    /// Runs a `task_output` or `task_kill` call. Returns the text for the model and whether it succeeded.
    #[must_use]
    pub fn run_tool(&mut self, call: &ToolCall) -> (String, bool) {
        let id = call.arguments.get("id").and_then(|v| v.as_str()).unwrap_or_default().trim().to_owned();
        let known: Vec<String> = self.tasks.iter().map(|task| task.id.clone()).collect();
        if call.name == KILL_TOOL {
            return if self.kill(&id) {
                (format!("Stopped background task {id}."), true)
            } else if known.contains(&id) {
                (format!("Background task {id} had already ended."), true)
            } else {
                (format!("Error: there's no background task '{id}'. Known: {}.", known_list(&known)), false)
            };
        }
        match self.output(&id) {
            Some((status, output)) => {
                let header = match &status {
                    TaskStatus::Running => format!("Task {id} is still running. Output so far:"),
                    other => format!("Task {id} has ended ({}). Output:", other.label()),
                };
                (format!("{header}\n{output}"), true)
            }
            None => (format!("Error: there's no background task '{id}'. Known: {}.", known_list(&known)), false),
        }
    }
}

impl Drop for BackgroundTasks {
    fn drop(&mut self) {
        for task in &mut self.tasks {
            if let Some(child) = &mut task.child {
                sandbox::kill_group(child);
                let _ = child.wait();
            }
        }
    }
}

fn known_list(known: &[String]) -> String {
    if known.is_empty() { "none".to_owned() } else { known.join(", ") }
}

fn tail(output: &Arc<Mutex<String>>, chars: usize) -> String {
    let Ok(text) = output.lock() else { return String::new() };
    let count = text.chars().count();
    if count <= chars {
        return text.clone();
    }
    let skip = count - chars;
    format!("…\n{}", text.chars().skip(skip).collect::<String>())
}

/// Whether a shell call asks to run in the background.
#[must_use]
pub fn wants_background(arguments: &serde_json::Value) -> bool {
    arguments.get("background").and_then(serde_json::Value::as_bool).unwrap_or(false)
}

/// The two task tools.
#[must_use]
pub fn tool_definitions() -> Vec<ToolDefinition> {
    vec![
        ToolDefinition {
            name: OUTPUT_TOOL.to_owned(),
            description:
                "Read the output so far of a background shell command started with shell's background option, \
                          by its task id (such as b1), and whether it has finished."
                    .to_owned(),
            parameters: json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
        },
        ToolDefinition {
            name: KILL_TOOL.to_owned(),
            description: "Stop a background shell command by its task id.".to_owned(),
            parameters: json!({"type": "object", "properties": {"id": {"type": "string"}}, "required": ["id"]}),
        },
    ]
}

#[cfg(test)]
mod tests {
    use super::*;

    fn call(name: &str, id: &str) -> ToolCall {
        ToolCall { id: "c".into(), name: name.into(), arguments: json!({"id": id}) }
    }

    #[test]
    fn a_background_command_runs_reports_and_can_be_stopped() {
        let dir = tempfile::tempdir().unwrap();
        let workspace = Workspace::new(dir.path()).unwrap();
        let mut tasks = BackgroundTasks::default();
        let id = tasks.spawn(&workspace, "echo started; sleep 30", false, &[]).unwrap();
        assert_eq!(id, "b1");
        assert!(tasks.any_running());
        // Output arrives while it runs.
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        while !tasks.output(&id).unwrap().1.contains("started") && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
        }
        let (text, ok) = tasks.run_tool(&call(OUTPUT_TOOL, "b1"));
        assert!(ok && text.contains("still running") && text.contains("started"), "{text}");
        assert!(tasks.poll().is_empty(), "nothing ended yet");
        assert_eq!(tasks.describe_running().len(), 1);
        let (text, ok) = tasks.run_tool(&call(KILL_TOOL, "b1"));
        assert!(ok && text.contains("Stopped"), "{text}");
        assert_eq!(tasks.list()[0].status, "killed");
        assert!(!tasks.any_running());
        let (text, ok) = tasks.run_tool(&call(OUTPUT_TOOL, "b9"));
        assert!(!ok && text.contains("Known: b1"), "{text}");

        // A quick command ends on its own and is reported once.
        let id = tasks.spawn(&workspace, "echo done", false, &[]).unwrap();
        let deadline = Instant::now() + std::time::Duration::from_secs(5);
        let mut ended = Vec::new();
        while ended.is_empty() && Instant::now() < deadline {
            std::thread::sleep(std::time::Duration::from_millis(20));
            ended = tasks.poll();
        }
        assert_eq!(ended.len(), 1);
        assert_eq!(ended[0].0, id);
        assert_eq!(ended[0].1, TaskStatus::Finished { code: Some(0) });
        assert!(ended[0].2.contains("done"));
        assert!(tasks.poll().is_empty(), "announced once");
        assert!(wants_background(&json!({"command": "x", "background": true})));
        assert!(!wants_background(&json!({"command": "x"})));
    }
}
