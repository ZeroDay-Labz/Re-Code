//! The same extension management as the home screen, from the shell: `re-code mcp|skill|plugin|memory
//! list|add|install|…`, plus `re-code trust` and `untrust`. Lists take `--json`. Installs print the same
//! preview the interface shows, then ask (or take `--yes` / `--keep-off`); without a terminal and
//! without a flag they stop with exit code 4.
//!
//! Exit codes: 0 done, 1 error, 2 refused (a dangerous skill, an unsupported source, a name that isn't
//! there), 4 a decision was needed and no terminal was available to ask.

use crate::paths::Paths;
use clap::{Args, Subcommand, ValueEnum};
use recode_core::extensions::{InstallOutcome, McpSandbox, PluginOutcome, Verdict};
use recode_extensions::Extensions;
use recode_extensions::fetch::{Bases, Fetcher};
use recode_extensions::plugins::PluginHub;
use recode_extensions::skillhub::Hub;
use recode_store::Store;
use std::io::{IsTerminal, Write};
use std::path::PathBuf;

pub const OK: i32 = 0;
pub const FAILED: i32 = 1;
pub const REFUSED: i32 = 2;
pub const NEEDS_TTY: i32 = 4;

#[derive(Debug, Subcommand)]
pub enum Extension {
    /// MCP servers: list, add, remove, switch, test, import.
    Mcp {
        #[command(subcommand)]
        action: McpAction,
    },
    /// Skills: list, search the hubs, install, update, remove, switch.
    Skill {
        #[command(subcommand)]
        action: SkillAction,
    },
    /// Plugins and marketplaces.
    Plugin {
        #[command(subcommand)]
        action: PluginAction,
    },
    /// What re-code remembers between sessions.
    Memory {
        #[command(subcommand)]
        action: MemoryAction,
    },
    /// Trust a project folder's configuration (its skills, hooks and instruction files) as it is now.
    Trust {
        /// The folder; the current one when left out.
        path: Option<PathBuf>,
        /// Don't ask, even though trusting lets the folder's hooks and skills run.
        #[arg(long)]
        yes: bool,
    },
    /// Stop trusting a project folder's configuration.
    Untrust { path: Option<PathBuf> },
}

#[derive(Debug, Subcommand)]
pub enum McpAction {
    List(ListArgs),
    /// Add a server from a command line, an http(s) address, a registry id, a server.json link, a GitHub
    /// repo or a pasted config snippet.
    Add {
        what: String,
        /// The name to store it under (derived when left out).
        #[arg(long)]
        name: Option<String>,
        /// Run its program on the host instead of in bubblewrap.
        #[arg(long)]
        no_sandbox: bool,
    },
    Remove {
        name: String,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
    /// Connect to a server and list its tools (a stdio server's program is started).
    Test {
        name: String,
        #[arg(long)]
        json: bool,
    },
    /// Bring servers over from another tool's configuration.
    Import {
        source: ImportSource,
    },
}

#[derive(Debug, Clone, Copy, ValueEnum)]
pub enum ImportSource {
    /// Claude Code's MCP servers (`~/.claude.json`).
    Claude,
    /// OpenCode's MCP servers and providers (`~/.config/opencode/opencode.json`).
    Opencode,
}

#[derive(Debug, Subcommand)]
pub enum SkillAction {
    List(ListArgs),
    /// Search the Hermes skills index.
    Search {
        query: Vec<String>,
    },
    /// Install from the Hermes index (official/…, a name), owner/repo/folder, a GitHub or skills.sh
    /// link, a SKILL.md link or a folder. Nothing fetched is run.
    Install(InstallArgs),
    /// Fetch the newest commit of an installed skill and show what changes.
    Update {
        name: String,
        #[command(flatten)]
        decide: Decide,
    },
    Remove {
        name: String,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
}

#[derive(Debug, Subcommand)]
pub enum PluginAction {
    List(ListArgs),
    /// Search the Hermes catalog and the marketplaces you follow.
    Search {
        query: Vec<String>,
    },
    /// Install from the Hermes catalog (a name), name@marketplace, owner/repo[/folder] or a folder.
    Install(InstallArgs),
    Update {
        name: String,
        #[command(flatten)]
        decide: Decide,
    },
    Remove {
        name: String,
    },
    Enable {
        name: String,
    },
    Disable {
        name: String,
    },
    /// Claude Code marketplaces to install plugins from.
    Marketplace {
        #[command(subcommand)]
        action: MarketAction,
    },
}

#[derive(Debug, Subcommand)]
pub enum MarketAction {
    List(ListArgs),
    /// Follow a marketplace: owner/repo[@ref], a GitHub link, or a folder with .claude-plugin/marketplace.json.
    Add {
        what: String,
    },
    Remove {
        name: String,
    },
    /// What a marketplace offers (`hermes` for the Hermes catalog).
    Browse {
        name: String,
        #[arg(long)]
        json: bool,
    },
}

#[derive(Debug, Subcommand)]
pub enum MemoryAction {
    List(ListArgs),
    Show {
        name: String,
    },
    Forget {
        name: String,
        /// `project` or `user`; both are tried when left out.
        #[arg(long)]
        scope: Option<String>,
    },
}

#[derive(Debug, Args)]
pub struct ListArgs {
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args)]
pub struct InstallArgs {
    pub what: String,
    #[command(flatten)]
    pub decide: Decide,
    /// Print the preview as JSON too.
    #[arg(long)]
    pub json: bool,
}

#[derive(Debug, Args, Clone, Copy)]
pub struct Decide {
    /// Install and switch on without asking (a dangerous verdict still keeps it off).
    #[arg(long, conflicts_with = "keep_off")]
    pub yes: bool,
    /// Install but leave it off.
    #[arg(long)]
    pub keep_off: bool,
}

/// Everything the subcommands need, opened once.
struct Session {
    paths: Paths,
    extensions: Extensions,
    store: Option<Store>,
    project: Option<PathBuf>,
}

impl Session {
    fn open(paths: &Paths) -> Self {
        let store = Store::open(&paths.database).ok();
        let project = std::env::current_dir().ok();
        let mut extensions = Extensions::load(crate::chat::locations(paths));
        let trusted = project.as_deref().is_some_and(|project| {
            let config = crate::trust::project_config(project);
            crate::trust::is_trusted(store.as_ref(), project, &config)
        });
        extensions.set_project_trusted(trusted);
        Self { paths: paths.clone(), extensions, store, project }
    }

    fn fetcher(&self) -> Result<Fetcher, String> {
        Fetcher::new(Bases::from_env())
    }

    fn skill_hub(&self) -> Result<Hub, String> {
        Ok(Hub::new(self.fetcher()?, self.paths.cache_dir.clone(), self.paths.skills_dir.clone()))
    }

    fn plugin_hub(&self) -> Result<PluginHub, String> {
        Ok(PluginHub::new(self.fetcher()?, self.paths.cache_dir.clone(), self.paths.data_dir.join("plugins")))
    }

    fn memory_places(&self) -> crate::memory::Places {
        let key = self.project.as_deref().map(crate::trust::canonical_key).unwrap_or_default();
        crate::memory::Places::new(&self.paths.data_dir, &key)
    }
}

/// Runs one subcommand and returns the exit code.
pub async fn run(paths: &Paths, command: Extension) -> i32 {
    let mut session = Session::open(paths);
    let result = match command {
        Extension::Mcp { action } => mcp(&mut session, action).await,
        Extension::Skill { action } => skill(&mut session, action).await,
        Extension::Plugin { action } => plugin(&mut session, action).await,
        Extension::Memory { action } => memory(&session, action),
        Extension::Trust { path, yes } => trust(&session, path, yes),
        Extension::Untrust { path } => untrust(&session, path),
    };
    match result {
        Ok(code) => code,
        Err(error) => {
            eprintln!("{error}");
            FAILED
        }
    }
}

fn print_json<T: serde::Serialize>(value: &T) -> Result<(), String> {
    println!("{}", serde_json::to_string_pretty(value).map_err(|error| error.to_string())?);
    Ok(())
}

// ---------------------------------------------------------------------------------------------

async fn mcp(session: &mut Session, action: McpAction) -> Result<i32, String> {
    match action {
        McpAction::List(ListArgs { json }) => {
            let servers = session.extensions.snapshot().servers;
            if json {
                print_json(&servers)?;
            } else if servers.is_empty() {
                println!("No MCP servers. Add one with `re-code mcp add <command|address|link>`.");
            } else {
                for server in &servers {
                    let state = if server.enabled { "on " } else { "off" };
                    let source = server.external.as_ref().map(|e| format!("  [{}]", e.label)).unwrap_or_default();
                    let sandbox = match (&server.transport, server.sandbox) {
                        (recode_core::extensions::McpTransport::Stdio { .. }, McpSandbox::None) => "  unsandboxed",
                        _ => "",
                    };
                    println!("{state}  {:<24} {}{source}{sandbox}", server.name, server.connection_text());
                }
            }
            Ok(OK)
        }
        McpAction::Add { what, name, no_sandbox } => {
            // A command line (`npx -y thing-mcp`) is taken as such; everything else resolves like the add form.
            let trimmed = what.trim();
            let looks_like_command = trimmed.contains(char::is_whitespace)
                && !trimmed.starts_with("http://")
                && !trimmed.starts_with("https://")
                && !trimmed.starts_with('{');
            let resolved = if looks_like_command {
                recode_extensions::registry::Resolved {
                    servers: vec![recode_extensions::paste::from_command_line(name.as_deref(), trimmed)?],
                    note: None,
                }
            } else {
                recode_extensions::registry::resolve_any(name.as_deref(), &what).await?
            };
            let mut added = Vec::new();
            for mut server in resolved.servers {
                if no_sandbox {
                    server.sandbox = McpSandbox::None;
                }
                let server_name = server.name.clone();
                session.extensions.save_server(None, server).map_err(|error| error.to_string())?;
                added.push(server_name);
            }
            println!("Added {}.", added.join(", "));
            if let Some(note) = resolved.note {
                println!("{note}");
            }
            println!("Test one with `re-code mcp test <name>`.");
            Ok(OK)
        }
        McpAction::Remove { name } => {
            session.extensions.delete_server(&name).map_err(|error| error.to_string())?;
            println!("Removed {name}.");
            Ok(OK)
        }
        McpAction::Enable { name } => {
            session.extensions.set_server_enabled(&name, true).map_err(|error| error.to_string())?;
            println!("{name} is on.");
            Ok(OK)
        }
        McpAction::Disable { name } => {
            session.extensions.set_server_enabled(&name, false).map_err(|error| error.to_string())?;
            println!("{name} is off.");
            Ok(OK)
        }
        McpAction::Test { name, json } => {
            let server =
                session.extensions.server(&name).ok_or_else(|| format!("There's no MCP server called {name}."))?;
            let launch = recode_extensions::Launch::from_environment();
            let probe = recode_extensions::probe_with(&server, &launch).await;
            if json {
                print_json(&probe)?;
            }
            match probe {
                recode_core::extensions::McpProbe::Connected {
                    server_name,
                    server_version,
                    tools,
                    estimated_schema_tokens,
                } => {
                    if !json {
                        println!(
                            "Connected to {}{}: {} tools (~{estimated_schema_tokens} tokens of schemas).",
                            server_name.unwrap_or_else(|| name.clone()),
                            server_version.map(|v| format!(" {v}")).unwrap_or_default(),
                            tools.len()
                        );
                        for tool in &tools {
                            println!("  {}", tool.signature());
                        }
                    }
                    Ok(OK)
                }
                recode_core::extensions::McpProbe::Failed { error } => {
                    if !json {
                        eprintln!("{name} failed: {error}");
                    }
                    Ok(FAILED)
                }
            }
        }
        McpAction::Import { source } => match source {
            ImportSource::Claude => {
                let home = std::env::var_os("HOME").map(PathBuf::from).unwrap_or_default();
                let copies: Vec<_> =
                    recode_extensions::claude::servers(&home.join(".claude.json"), session.project.as_deref())
                        .into_iter()
                        .filter(|server| server.external.as_ref().is_none_or(|e| e.unsupported.is_none()))
                        .map(|server| recode_core::extensions::McpServer { external: None, ..server })
                        .collect();
                let count = session.extensions.import_servers(copies).map_err(|error| error.to_string())?;
                println!("Imported {count} MCP server(s) from Claude Code.");
                Ok(OK)
            }
            ImportSource::Opencode => {
                let trusted = session.project.as_deref().is_some_and(|project| {
                    let config = crate::trust::project_config(project);
                    crate::trust::is_trusted(session.store.as_ref(), project, &config)
                });
                let import = recode_extensions::opencode::read(
                    Some(&session.paths.user_config_root),
                    session.project.as_deref(),
                    trusted,
                );
                let (text, is_error) = crate::chat::summarise_opencode_import(&mut session.extensions, import);
                println!("{text}");
                Ok(if is_error { FAILED } else { OK })
            }
        },
    }
}

// ---------------------------------------------------------------------------------------------

async fn skill(session: &mut Session, action: SkillAction) -> Result<i32, String> {
    match action {
        SkillAction::List(ListArgs { json }) => {
            let skills = session.extensions.snapshot().skills;
            if json {
                print_json(&skills)?;
            } else if skills.is_empty() {
                println!("No skills. Install one with `re-code skill install official/<category>/<name>`.");
            } else {
                for skill in &skills {
                    let state = if skill.enabled { "on " } else { "off" };
                    let source = match &skill.installed_from {
                        Some(from) => from.pin_text(),
                        None => format!("{:?}", skill.source).to_lowercase(),
                    };
                    let verdict = match &skill.scan {
                        Verdict::Clean => String::new(),
                        other => format!("  {}", other.label()),
                    };
                    println!("{state}  {:<28} {source}{verdict}", skill.name);
                }
            }
            Ok(OK)
        }
        SkillAction::Search { query } => {
            let hub = session.skill_hub()?;
            let search = hub.search(&query.join(" "), 20).await?;
            if let Some(note) = search.note {
                eprintln!("{note}");
            }
            for hit in search.hits {
                println!("{:<32} [{}]  {}", hit.tap, hit.label, hit.description);
            }
            Ok(OK)
        }
        SkillAction::Install(args) => {
            let hub = session.skill_hub()?;
            let outcome = hub.stage(&args.what).await;
            finish_skill(session, &hub, outcome, args.decide, args.json)
        }
        SkillAction::Update { name, decide } => {
            let hub = session.skill_hub()?;
            let outcome = hub.update(&name).await;
            finish_skill(session, &hub, outcome, decide, false)
        }
        SkillAction::Remove { name } => {
            session.extensions.delete_skill(&name).map_err(|error| error.to_string())?;
            println!("Removed {name}.");
            Ok(OK)
        }
        SkillAction::Enable { name } => {
            session.extensions.set_skill_enabled(&name, true).map_err(|error| error.to_string())?;
            println!("{name} is on.");
            Ok(OK)
        }
        SkillAction::Disable { name } => {
            session.extensions.set_skill_enabled(&name, false).map_err(|error| error.to_string())?;
            println!("{name} is off.");
            Ok(OK)
        }
    }
}

fn finish_skill(
    session: &mut Session,
    hub: &Hub,
    outcome: InstallOutcome,
    decide: Decide,
    json: bool,
) -> Result<i32, String> {
    match outcome {
        InstallOutcome::Failed { tap, error } => {
            eprintln!("{tap}: {error}");
            Ok(REFUSED)
        }
        InstallOutcome::UpToDate { name, sha } => {
            println!("{name} is already at the newest commit ({}).", &sha[..8.min(sha.len())]);
            Ok(OK)
        }
        InstallOutcome::Choose { tap, candidates, note } => {
            println!("{tap} holds {} skills; install one by its folder:", candidates.len());
            if let Some(note) = note {
                println!("{note}");
            }
            for candidate in candidates {
                println!("  re-code skill install {}    {}", candidate.tap, candidate.description);
            }
            Ok(NEEDS_TTY)
        }
        InstallOutcome::Preview(preview) => {
            if json {
                print_json(&preview)?;
            }
            println!("{}  [{}]{}", preview.name, preview.source.label, pin(&preview.source.sha));
            if !preview.description.is_empty() {
                println!("{}", preview.description);
            }
            println!(
                "{} · {} files · ~{} tokens when loaded",
                preview.declares.summary(),
                preview.files.len(),
                preview.estimated_tokens
            );
            print_verdict(&preview.scan);
            if let Some(problem) = &preview.problem {
                println!("It will stay off: {problem}");
            }
            if let Some(replacing) = &preview.replaces {
                println!(
                    "Update from {}: {}",
                    pin(&replacing.sha).trim_start_matches(" pinned @ "),
                    replacing.changed.join(", ")
                );
            }
            if let Some(note) = &preview.note {
                println!("{note}");
            }
            println!("Location  {}", preview.location);
            let dangerous = preview.scan.blocks();
            match decision(decide, dangerous)? {
                Choice::Enable | Choice::KeepOff => {
                    let enable = matches!(decision(decide, dangerous)?, Choice::Enable);
                    let committed = hub.commit(&preview.staging, enable)?;
                    session
                        .extensions
                        .set_skill_enabled(&committed.name, committed.enabled)
                        .map_err(|e| e.to_string())?;
                    println!(
                        "Installed {} ({}).{}",
                        committed.name,
                        if committed.enabled { "on" } else { "off" },
                        committed.note.map(|n| format!(" {n}")).unwrap_or_default()
                    );
                    Ok(OK)
                }
                Choice::Discard => {
                    hub.discard(&preview.staging)?;
                    println!("Discarded.");
                    Ok(OK)
                }
                Choice::NoTerminal => {
                    hub.discard(&preview.staging)?;
                    eprintln!("Not installed: say --yes or --keep-off, or run this in a terminal to be asked.");
                    Ok(NEEDS_TTY)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------

async fn plugin(session: &mut Session, action: PluginAction) -> Result<i32, String> {
    let markets = session.extensions.marketplaces().to_vec();
    match action {
        PluginAction::List(ListArgs { json }) => {
            let snapshot = session.extensions.snapshot();
            if json {
                print_json(&serde_json::json!({
                    "plugins": snapshot.plugins,
                    "marketplaces": snapshot.marketplaces,
                    "claude_code": snapshot.claude_plugins,
                }))?;
            } else {
                if snapshot.plugins.is_empty() {
                    println!("No plugins. Install one with `re-code plugin install <name>`.");
                }
                for plugin in &snapshot.plugins {
                    let state = if plugin.enabled { "on " } else { "off" };
                    println!("{state}  {}", plugin.header());
                    println!("     {}", plugin.capability_sentence());
                }
                for market in &snapshot.marketplaces {
                    println!("marketplace  {:<24} {} plugins{}", market.name, market.plugin_count, pin(&market.sha));
                }
                for info in &snapshot.claude_plugins {
                    println!("claude code  {:<24} {}", info.key, info.install_path);
                }
            }
            Ok(OK)
        }
        PluginAction::Search { query } => {
            let hub = session.plugin_hub()?;
            for listing in hub.search(&query.join(" "), &markets, 30).await? {
                let refused = listing
                    .unsupported
                    .as_ref()
                    .map(|why| format!("  (can't be installed: {why})"))
                    .unwrap_or_default();
                println!("{:<40} [{}]  {}{refused}", listing.tap, listing.label, listing.description);
            }
            Ok(OK)
        }
        PluginAction::Install(args) => {
            let hub = session.plugin_hub()?;
            let outcome = hub.stage(&args.what, &markets).await;
            finish_plugin(session, &hub, outcome, args.decide, args.json)
        }
        PluginAction::Update { name, decide } => {
            let hub = session.plugin_hub()?;
            let outcome = hub.update(&name, &markets).await;
            finish_plugin(session, &hub, outcome, decide, false)
        }
        PluginAction::Remove { name } => {
            let hub = session.plugin_hub()?;
            hub.remove(&name)?;
            let _ = session.extensions.forget_plugin(&name);
            println!("Removed {name} and everything it brought.");
            Ok(OK)
        }
        PluginAction::Enable { name } => {
            session.extensions.set_plugin_enabled(&name, true).map_err(|error| error.to_string())?;
            println!("{name} is on.");
            Ok(OK)
        }
        PluginAction::Disable { name } => {
            session.extensions.set_plugin_enabled(&name, false).map_err(|error| error.to_string())?;
            println!("{name} is off.");
            Ok(OK)
        }
        PluginAction::Marketplace { action } => match action {
            MarketAction::List(ListArgs { json }) => {
                if json {
                    print_json(&markets)?;
                } else if markets.is_empty() {
                    println!(
                        "No marketplaces. Follow one with `re-code plugin marketplace add anthropics/claude-plugins-official`."
                    );
                } else {
                    for market in &markets {
                        println!(
                            "{:<28} {}  {} plugins{}",
                            market.name,
                            market.repo,
                            market.plugin_count,
                            pin(&market.sha)
                        );
                    }
                }
                Ok(OK)
            }
            MarketAction::Add { what } => {
                let hub = session.plugin_hub()?;
                let market = hub.add_marketplace(&what).await?;
                println!("Following {} ({} plugins{}).", market.name, market.plugin_count, pin(&market.sha));
                session.extensions.add_marketplace(market).map_err(|error| error.to_string())?;
                Ok(OK)
            }
            MarketAction::Remove { name } => {
                session.extensions.remove_marketplace(&name).map_err(|error| error.to_string())?;
                if let Ok(hub) = session.plugin_hub() {
                    hub.forget_marketplace(&name);
                }
                println!("No longer following {name}.");
                Ok(OK)
            }
            MarketAction::Browse { name, json } => {
                let hub = session.plugin_hub()?;
                let listings = hub.browse(&name, &markets).await?;
                if json {
                    print_json(&listings)?;
                } else {
                    for listing in listings {
                        let refused = listing
                            .unsupported
                            .as_ref()
                            .map(|why| format!("  (can't be installed: {why})"))
                            .unwrap_or_default();
                        println!("{:<40} [{}]  {}{refused}", listing.tap, listing.label, listing.description);
                    }
                }
                Ok(OK)
            }
        },
    }
}

fn finish_plugin(
    session: &mut Session,
    hub: &PluginHub,
    outcome: PluginOutcome,
    decide: Decide,
    json: bool,
) -> Result<i32, String> {
    match outcome {
        PluginOutcome::Failed { tap, error } => {
            eprintln!("{tap}: {error}");
            Ok(REFUSED)
        }
        PluginOutcome::UpToDate { name, sha } => {
            println!("{name} is already at the newest commit ({}).", &sha[..8.min(sha.len())]);
            Ok(OK)
        }
        PluginOutcome::Preview(preview) => {
            if json {
                print_json(&preview)?;
            }
            let plugin = &preview.plugin;
            println!("{}", plugin.header());
            if !plugin.description.is_empty() {
                println!("{}", plugin.description);
            }
            println!("{} · {} files", plugin.capability_sentence(), preview.files);
            for part in &plugin.parts {
                println!("  {} {}", if part.runs_here() { "•" } else { "·" }, part.describe());
            }
            if let Some(replacing) = &preview.replaces {
                println!(
                    "Update from {}: {}",
                    &replacing.sha[..8.min(replacing.sha.len())],
                    replacing.changed.join(", ")
                );
            }
            if let Some(note) = &preview.note {
                println!("{note}");
            }
            println!("Location  {}", preview.location);
            let dangerous = plugin
                .parts
                .iter()
                .any(|part| matches!(part, recode_core::extensions::PluginPart::Skill { scan, .. } if scan.blocks()));
            match decision(decide, dangerous)? {
                Choice::Enable | Choice::KeepOff => {
                    let enable = matches!(decision(decide, dangerous)?, Choice::Enable);
                    let committed = hub.commit(&preview.staging, enable)?;
                    session
                        .extensions
                        .set_plugin_enabled(&committed.name, committed.enabled)
                        .map_err(|e| e.to_string())?;
                    println!(
                        "Installed {} ({}).{}",
                        committed.name,
                        if committed.enabled { "on" } else { "off" },
                        committed.note.map(|n| format!(" {n}")).unwrap_or_default()
                    );
                    Ok(OK)
                }
                Choice::Discard => {
                    hub.discard(&preview.staging)?;
                    println!("Discarded.");
                    Ok(OK)
                }
                Choice::NoTerminal => {
                    hub.discard(&preview.staging)?;
                    eprintln!("Not installed: say --yes or --keep-off, or run this in a terminal to be asked.");
                    Ok(NEEDS_TTY)
                }
            }
        }
    }
}

// ---------------------------------------------------------------------------------------------

fn memory(session: &Session, action: MemoryAction) -> Result<i32, String> {
    let places = session.memory_places();
    let memories = crate::memory::list(&places);
    match action {
        MemoryAction::List(ListArgs { json }) => {
            if json {
                let rows: Vec<serde_json::Value> = memories
                    .iter()
                    .map(|memory| {
                        serde_json::json!({
                            "name": memory.name,
                            "description": memory.description,
                            "type": memory.kind,
                            "scope": memory.scope.label(),
                            "path": memory.path,
                            "tokens_estimated": memory.tokens(),
                        })
                    })
                    .collect();
                print_json(&rows)?;
            } else if memories.is_empty() {
                println!(
                    "Nothing remembered yet. The model saves memories as it works; `/memory` in the program lists them."
                );
            } else {
                for memory in &memories {
                    println!(
                        "{:<8} {:<28} {}  ({}, ~{} tokens)",
                        memory.scope.label(),
                        memory.name,
                        memory.description,
                        memory.kind,
                        memory.tokens()
                    );
                }
            }
            Ok(OK)
        }
        MemoryAction::Show { name } => match memories.iter().find(|memory| memory.name == name) {
            Some(memory) => {
                println!("{}", std::fs::read_to_string(&memory.path).map_err(|error| error.to_string())?);
                Ok(OK)
            }
            None => {
                eprintln!("No memory called {name}.");
                Ok(REFUSED)
            }
        },
        MemoryAction::Forget { name, scope } => {
            let scopes: Vec<crate::memory::Scope> = match scope.as_deref() {
                Some("project") => vec![crate::memory::Scope::Project],
                Some("user") => vec![crate::memory::Scope::User],
                Some(other) => return Err(format!("--scope {other}: use project or user.")),
                None => memories.iter().filter(|m| m.name == name).map(|m| m.scope).collect(),
            };
            if scopes.is_empty() {
                eprintln!("No memory called {name}.");
                return Ok(REFUSED);
            }
            for scope in scopes {
                crate::memory::forget(&places, scope, &name)?;
                println!("Forgot {name} ({}).", scope.label());
            }
            Ok(OK)
        }
    }
}

// ---------------------------------------------------------------------------------------------

fn trust(session: &Session, path: Option<PathBuf>, yes: bool) -> Result<i32, String> {
    let project = resolve_project(session, path)?;
    let store = session.store.as_ref().ok_or("The settings database couldn't be opened, so trust can't be saved.")?;
    let config = crate::trust::project_config(&project);
    if config.skills.is_empty() && config.files.is_empty() {
        println!("{} has no .re-code configuration or instruction files to trust.", project.display());
        return Ok(OK);
    }
    println!("{} carries configuration re-code would start using:", project.display());
    for skill in &config.skills {
        println!("  skill  {skill}");
    }
    for file in &config.files {
        println!("  file   {file}");
    }
    if !yes {
        if !std::io::stdin().is_terminal() {
            eprintln!("Not trusted: say --yes, or run this in a terminal to be asked.");
            return Ok(NEEDS_TTY);
        }
        if !ask("Trust it? Its hooks and skills will run. [y/N] ")?.starts_with('y') {
            println!("Left untrusted.");
            return Ok(OK);
        }
    }
    crate::trust::trust(store, &project, &config)?;
    println!("Trusted {} as it is now; any change to its configuration asks again.", project.display());
    Ok(OK)
}

fn untrust(session: &Session, path: Option<PathBuf>) -> Result<i32, String> {
    let project = resolve_project(session, path)?;
    let store = session.store.as_ref().ok_or("The settings database couldn't be opened.")?;
    crate::trust::revoke(store, &project)?;
    println!("{} is no longer trusted; its hooks, skills and instruction files stay off.", project.display());
    Ok(OK)
}

fn resolve_project(session: &Session, path: Option<PathBuf>) -> Result<PathBuf, String> {
    let project = path.or_else(|| session.project.clone()).ok_or("There's no project folder.")?;
    project.canonicalize().map_err(|error| format!("{}: {error}", project.display()))
}

// ---------------------------------------------------------------------------------------------

enum Choice {
    Enable,
    KeepOff,
    Discard,
    NoTerminal,
}

/// What to do with a staged install: the flags decide, else the terminal is asked.
fn decision(decide: Decide, dangerous: bool) -> Result<Choice, String> {
    if decide.keep_off || (decide.yes && dangerous) {
        return Ok(Choice::KeepOff);
    }
    if decide.yes {
        return Ok(Choice::Enable);
    }
    if !std::io::stdin().is_terminal() {
        return Ok(Choice::NoTerminal);
    }
    let prompt =
        if dangerous { "Keep it, off? [k]eep off / [n]o: " } else { "Enable now? [y]es / [k]eep off / [n]o: " };
    let answer = ask(prompt)?;
    Ok(match answer.chars().next() {
        Some('y') if !dangerous => Choice::Enable,
        Some('k') => Choice::KeepOff,
        _ => Choice::Discard,
    })
}

fn ask(prompt: &str) -> Result<String, String> {
    print!("{prompt}");
    std::io::stdout().flush().map_err(|error| error.to_string())?;
    let mut line = String::new();
    std::io::stdin().read_line(&mut line).map_err(|error| error.to_string())?;
    Ok(line.trim().to_lowercase())
}

fn pin(sha: &str) -> String {
    if sha.len() >= 8 { format!(" pinned @ {}", &sha[..8]) } else { String::new() }
}

fn print_verdict(verdict: &Verdict) {
    match verdict {
        Verdict::Clean => println!("scan: clean"),
        other => {
            let level = if other.blocks() { "dangerous (it can't be enabled)" } else { "caution" };
            for (index, finding) in other.findings().iter().enumerate() {
                let place =
                    if finding.line > 0 { format!("{}:{}", finding.file, finding.line) } else { finding.file.clone() };
                if index == 0 {
                    println!("scan: {level} — {place} {}", finding.reason);
                } else {
                    println!("      {place} {}", finding.reason);
                }
            }
        }
    }
}
