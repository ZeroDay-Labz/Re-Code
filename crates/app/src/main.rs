//! The `re-code` command.

mod background;
mod chat;
mod cli;
mod compact;
mod config;
mod crash;
mod doctor;
mod headless;
mod hooks;
mod init;
mod instructions;
mod logging;
mod memory;
mod paths;
mod permissions;
mod plan_tool;
mod secrets;
mod session;
mod skill_tool;
mod subagent;
mod themes;
mod todo;
mod trust;

use anyhow::Result;
use clap::{CommandFactory, Parser, Subcommand};
use std::io::IsTerminal;
use std::path::PathBuf;

/// `0.1.2 (7f894dcadc, 2026-11-08)`: the version, the commit and the build date, for bug reports.
const VERSION: &str =
    concat!(env!("CARGO_PKG_VERSION"), " (", env!("RE_CODE_GIT_SHA"), ", ", env!("RE_CODE_BUILD_DATE"), ")");

#[derive(Parser)]
#[command(name = "re-code", version = VERSION, about = "A terminal-native AI coding agent")]
struct Cli {
    /// Read configuration from this file instead of the default location.
    #[arg(long, global = true, value_name = "FILE")]
    config: Option<PathBuf>,

    /// Use this Ollama address instead of the configured one.
    #[arg(long, global = true, value_name = "URL")]
    ollama_url: Option<String>,

    /// Continue the last session in this folder.
    #[arg(short = 'c', long = "continue")]
    resume: bool,

    /// Allow the agent to read and change files in this folder (and commands to reach it) for the
    /// whole session, without asking each time. Can be given more than once.
    #[arg(long = "add-dir", value_name = "DIR")]
    add_dir: Vec<PathBuf>,

    /// Run one prompt without the interface and print the result (`-` reads the prompt from stdin).
    /// Exit codes: 0 done, 1 failed, 2 done but a tool needing approval was denied, 3 no such model.
    #[arg(short = 'p', long = "print", value_name = "PROMPT")]
    print: Option<String>,

    /// With --print: how to print the result.
    #[arg(long, value_enum, default_value = "text", requires = "print")]
    output_format: headless::OutputFormat,

    /// The model to use (otherwise the one used last in this folder).
    #[arg(long, value_name = "NAME")]
    model: Option<String>,

    /// With --print: the most tool calls in one turn (the default is 24).
    #[arg(long, value_name = "N", requires = "print")]
    max_turns: Option<usize>,

    /// Don't use or change re-code's memory for this run (its index stays out of the prompt).
    #[arg(long)]
    no_memory: bool,

    /// With --print: tools that may run without asking, by name (others that would ask are denied).
    #[arg(long, value_delimiter = ',', value_name = "TOOL", requires = "print")]
    allowed_tools: Vec<String>,

    /// How much the agent may do without asking: ask, accept-edits, plan, or auto (needs the sandbox).
    #[arg(long, value_name = "MODE")]
    permission_mode: Option<String>,

    #[command(subcommand)]
    command: Option<Command>,
}

#[derive(Subcommand)]
enum Command {
    /// Report the state of this machine and its AI runtimes. Changes nothing outside re-code's own files.
    Doctor {
        /// Output JSON instead of text.
        #[arg(long)]
        json: bool,
        /// Write a bundle for a bug report (this report, the version, recent log lines with secrets
        /// hidden, and any crash files) and print its path.
        #[arg(long)]
        report: bool,
    },
    /// Try the terminal interface with a recorded session. No model is connected and no files change.
    Demo,
    /// Print a shell completion script.
    Completions {
        #[arg(value_enum)]
        shell: clap_complete::Shell,
    },
    /// MCP servers, skills, plugins, memory and trust, from the shell.
    #[command(flatten)]
    Extension(cli::Extension),
}

/// `ask`, `accept-edits`, `plan` or `auto`, as the command line spells them.
fn parse_permission_mode(text: Option<&str>) -> Result<Option<recode_core::protocol::PermissionMode>> {
    use recode_core::protocol::PermissionMode;
    Ok(match text.map(|t| t.trim().to_ascii_lowercase()).as_deref() {
        None => None,
        Some("ask") => Some(PermissionMode::Ask),
        Some("accept-edits" | "accept_edits" | "edits") => Some(PermissionMode::AcceptEdits),
        Some("plan") => Some(PermissionMode::Plan),
        Some("auto" | "auto-sandbox") => Some(PermissionMode::AutoSandbox),
        Some(other) => anyhow::bail!("--permission-mode {other}: use ask, accept-edits, plan or auto."),
    })
}

#[tokio::main]
async fn main() -> Result<()> {
    let cli = Cli::parse();
    let paths = paths::Paths::from_environment()?;
    let _log_guard = logging::init(&paths.log_dir)?;
    crash::install_hook(&paths);
    tracing::info!(version = env!("CARGO_PKG_VERSION"), "starting");
    let config = config::Config::load(cli.config.as_deref().unwrap_or(&paths.config_file))?;

    let ollama_url = cli.ollama_url.clone().unwrap_or_else(|| config.ollama.url.clone());
    // The user's own themes, so `theme = "Ember"` in config.toml and the picker know them from the start.
    let (user_themes, theme_problems) = recode_tui::theme_file::load_dir(&paths.themes_dir);
    recode_tui::theme::install_user_themes(user_themes);
    for problem in &theme_problems {
        tracing::warn!(%problem, "theme file skipped");
    }

    match cli.command {
        Some(Command::Doctor { json, report }) => {
            let gathered = doctor::gather(&paths, &ollama_url).await;
            if report {
                let path = crash::write_report(&paths, &serde_json::to_string_pretty(&gathered)?)?;
                println!(
                    "Wrote {}. Read it before sharing it: it names your runtimes, containers and models.",
                    path.display()
                );
            } else if json {
                println!("{}", serde_json::to_string_pretty(&gathered)?);
            } else {
                print!("{}", doctor::render::render(&gathered, doctor::render::Style::detect()));
            }
        }
        Some(Command::Demo) => recode_tui::run_demo().await?,
        Some(Command::Completions { shell }) => {
            clap_complete::generate(shell, &mut Cli::command(), "re-code", &mut std::io::stdout());
        }
        Some(Command::Extension(extension)) => {
            let code = cli::run(&paths, extension).await;
            std::process::exit(code);
        }
        None if cli.print.is_some() => {
            let mut prompt = cli.print.clone().unwrap_or_default();
            if prompt.trim() == "-" {
                prompt.clear();
                std::io::Read::read_to_string(&mut std::io::stdin(), &mut prompt)?;
            }
            if prompt.trim().is_empty() {
                anyhow::bail!("--print needs a prompt (or `-` with the prompt on stdin).");
            }
            let permission_mode = parse_permission_mode(cli.permission_mode.as_deref())?;
            let link = chat::spawn(
                paths,
                ollama_url,
                config.ollama.num_ctx,
                cli.resume,
                cli.add_dir,
                cli.max_turns,
                cli.no_memory,
            );
            let options = headless::Options {
                prompt,
                format: cli.output_format,
                model: cli.model.clone(),
                permission_mode,
                allowed_tools: cli.allowed_tools.clone(),
            };
            let code = headless::run(link, options).await;
            std::process::exit(code);
        }
        None => {
            if !std::io::stdout().is_terminal() {
                anyhow::bail!(
                    "re-code needs a terminal. Try `re-code -p \"prompt\"` or `re-code doctor --json` for scripted use."
                );
            }
            let theme = config.theme()?;
            let link =
                chat::spawn(paths, ollama_url, config.ollama.num_ctx, cli.resume, cli.add_dir, None, cli.no_memory);
            recode_tui::run_interactive(link, theme).await?;
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_print_flags_parse_and_the_mode_names_are_checked() {
        let cli = Cli::try_parse_from([
            "re-code",
            "-p",
            "list the crates",
            "--output-format",
            "json",
            "--model",
            "gpt-oss:20b",
            "--max-turns",
            "5",
            "--allowed-tools",
            "read,grep",
            "--permission-mode",
            "accept-edits",
        ])
        .unwrap();
        assert_eq!(cli.print.as_deref(), Some("list the crates"));
        assert_eq!(cli.output_format, headless::OutputFormat::Json);
        assert_eq!(cli.max_turns, Some(5));
        assert_eq!(cli.allowed_tools, ["read", "grep"]);
        assert_eq!(
            parse_permission_mode(cli.permission_mode.as_deref()).unwrap(),
            Some(recode_core::protocol::PermissionMode::AcceptEdits)
        );
        assert!(Cli::try_parse_from(["re-code", "--output-format", "json"]).is_err(), "only with --print");
        assert!(parse_permission_mode(Some("yolo")).is_err());
        assert_eq!(parse_permission_mode(None).unwrap(), None);
    }
}
