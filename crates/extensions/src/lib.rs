//! Stores MCP servers, skills and custom tools, and tests MCP connections. MCP servers configured in
//! Claude Code are read live alongside re-code's own (see [`claude`]); only their on/off and exposure
//! choices are stored here.

pub mod catalog;
pub mod claude;
pub mod frontmatter;
pub mod hub;
pub mod paste;
mod probe;
pub mod registry;
pub mod scan;
pub mod skills;

pub use probe::probe;

use recode_core::extensions::{CustomTool, ExtensionsSnapshot, McpServer, ToolFilter, validate_name};
use recode_core::providers::CloudProvider;
use serde::{Deserialize, Serialize};
use std::collections::BTreeSet;
use std::path::{Path, PathBuf};

fn set_flag(set: &mut BTreeSet<String>, name: &str, on: bool) {
    if on {
        set.insert(name.to_owned());
    } else {
        set.remove(name);
    }
}

/// Errors, written to be shown to the user.
#[derive(Debug, thiserror::Error)]
pub enum ExtensionError {
    #[error("Can't save: {0} has a problem that needs fixing first, and saving would overwrite it. {1}")]
    Unreadable(PathBuf, String),
    #[error("Can't write {0}: {1}")]
    Write(PathBuf, std::io::Error),
    #[error("Can't encode the configuration: {0}")]
    Encode(#[from] toml::ser::Error),
    #[error("{0}")]
    Invalid(String),
    #[error("Can't change {0}: {1}")]
    Skill(PathBuf, std::io::Error),
}

/// What is written to `extensions.toml`.
#[derive(Debug, Default, PartialEq, Serialize, Deserialize)]
#[serde(default, deny_unknown_fields)]
struct FileContents {
    #[serde(rename = "mcp", skip_serializing_if = "Vec::is_empty")]
    servers: Vec<McpServer>,
    #[serde(rename = "tool", skip_serializing_if = "Vec::is_empty")]
    tools: Vec<CustomTool>,
    #[serde(rename = "provider", skip_serializing_if = "Vec::is_empty")]
    providers: Vec<CloudProvider>,
    #[serde(skip_serializing_if = "BTreeSet::is_empty")]
    disabled_skills: BTreeSet<String>,
    /// Servers read from Claude Code that are switched off here.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    external_disabled: BTreeSet<String>,
    /// Servers read from Claude Code whose tools go straight to the model.
    #[serde(default, skip_serializing_if = "BTreeSet::is_empty")]
    external_exposed: BTreeSet<String>,
}

/// Where everything lives.
#[derive(Debug, Clone)]
pub struct Locations {
    pub config_file: PathBuf,
    /// re-code's own skills, one directory each.
    pub user_skills: PathBuf,
    /// The current project's skills.
    pub project_skills: Option<PathBuf>,
    /// Claude Code's skills, listed read-only.
    pub claude_skills: Option<PathBuf>,
    /// Claude Code's configuration (`~/.claude.json`), whose MCP servers are used live.
    pub claude_config: Option<PathBuf>,
    /// The project folder, for Claude Code's per-project servers.
    pub project: Option<PathBuf>,
}

/// The stored configuration.
#[derive(Debug)]
pub struct Extensions {
    locations: Locations,
    contents: FileContents,
    load_error: Option<String>,
    /// Servers read from Claude Code, as found (before this file's on/off choices).
    external: Vec<McpServer>,
    /// Whether the project folder's configuration has been trusted (`recode-app`'s trust module decides).
    project_trusted: bool,
}

impl Extensions {
    /// Sets whether the project's skills may switch on. Off until the user trusts the folder.
    pub fn set_project_trusted(&mut self, trusted: bool) {
        self.project_trusted = trusted;
    }

    /// Loads the configuration. A damaged file is reported, kept, and protected from being overwritten.
    #[must_use]
    pub fn load(locations: Locations) -> Self {
        let (contents, load_error) = match std::fs::read_to_string(&locations.config_file) {
            Ok(text) => match toml::from_str(&text) {
                Ok(contents) => (contents, None),
                Err(error) => (FileContents::default(), Some(format!("{}: {error}", locations.config_file.display()))),
            },
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => (FileContents::default(), None),
            Err(error) => (FileContents::default(), Some(format!("{}: {error}", locations.config_file.display()))),
        };
        let external = locations
            .claude_config
            .as_deref()
            .map(|config| claude::servers(config, locations.project.as_deref()))
            .unwrap_or_default();
        Self { locations, contents, load_error, external, project_trusted: false }
    }

    /// re-code's own servers, then Claude Code's with this file's choices applied. A server of
    /// re-code's own with the same name wins.
    fn all_servers(&self) -> Vec<McpServer> {
        let mut servers = self.contents.servers.clone();
        for server in &self.external {
            if servers.iter().any(|own| own.name == server.name) {
                continue;
            }
            let mut server = server.clone();
            server.enabled = !self.contents.external_disabled.contains(&server.name);
            server.expose_to_agent = self.contents.external_exposed.contains(&server.name);
            servers.push(server);
        }
        servers
    }

    fn is_external(&self, name: &str) -> bool {
        !self.contents.servers.iter().any(|own| own.name == name) && self.external.iter().any(|s| s.name == name)
    }

    /// Everything, for display. Probe results are added by the caller.
    #[must_use]
    pub fn snapshot(&self) -> ExtensionsSnapshot {
        ExtensionsSnapshot {
            servers: self.all_servers(),
            tools: self.contents.tools.clone(),
            skills: skills::discover(&self.locations, &self.contents.disabled_skills, self.project_trusted),
            providers: self.contents.providers.clone(),
            config_path: self.locations.config_file.display().to_string(),
            load_error: self.load_error.clone(),
            ..ExtensionsSnapshot::default()
        }
    }

    /// Finds a server by name.
    #[must_use]
    pub fn server(&self, name: &str) -> Option<McpServer> {
        self.all_servers().into_iter().find(|server| server.name == name)
    }

    /// All configured servers.
    #[must_use]
    pub fn servers(&self) -> &[McpServer] {
        &self.contents.servers
    }

    /// Finds a provider by name.
    #[must_use]
    pub fn provider(&self, name: &str) -> Option<&CloudProvider> {
        self.contents.providers.iter().find(|provider| provider.name == name)
    }

    /// Adds or replaces a cloud provider.
    pub fn save_provider(&mut self, original: Option<&str>, provider: CloudProvider) -> Result<(), ExtensionError> {
        upsert(&mut self.contents.providers, original, provider, |item| &item.name, "provider")?;
        self.save()
    }

    pub fn delete_provider(&mut self, name: &str) -> Result<(), ExtensionError> {
        self.contents.providers.retain(|provider| provider.name != name);
        self.save()
    }

    pub fn set_provider_enabled(&mut self, name: &str, enabled: bool) -> Result<(), ExtensionError> {
        if let Some(provider) = self.contents.providers.iter_mut().find(|provider| provider.name == name) {
            provider.enabled = enabled;
        }
        self.save()
    }

    /// Adds servers from Claude Code that aren't already configured, returning how many were added.
    pub fn import_servers(&mut self, servers: Vec<McpServer>) -> Result<usize, ExtensionError> {
        let mut added = 0;
        for server in servers {
            if !self.contents.servers.iter().any(|own| own.name == server.name) {
                self.contents.servers.push(server);
                added += 1;
            }
        }
        if added > 0 {
            self.save()?;
        }
        Ok(added)
    }

    fn save(&self) -> Result<(), ExtensionError> {
        if let Some(error) = &self.load_error {
            return Err(ExtensionError::Unreadable(self.locations.config_file.clone(), error.clone()));
        }
        let path = &self.locations.config_file;
        let mut text = String::from(
            "# Managed by re-code. Comments added here are lost when it saves.\n\
             # Values can be written as ${VARIABLE} to read them from the environment instead of storing them.\n\n",
        );
        text.push_str(&toml::to_string_pretty(&self.contents)?);
        write_private(path, &text).map_err(|error| ExtensionError::Write(path.clone(), error))
    }

    /// Adds or replaces a server. `original` is the name it had before any rename.
    pub fn save_server(&mut self, original: Option<&str>, server: McpServer) -> Result<(), ExtensionError> {
        upsert(&mut self.contents.servers, original, server, |item| &item.name, "server")?;
        self.save()
    }

    pub fn delete_server(&mut self, name: &str) -> Result<(), ExtensionError> {
        if self.is_external(name) {
            return Err(ExtensionError::Invalid(format!(
                "{name} comes from Claude Code, so it can't be deleted here. Press space to turn it off, \
                 or remove it in Claude Code."
            )));
        }
        self.contents.servers.retain(|server| server.name != name);
        self.save()
    }

    pub fn set_server_enabled(&mut self, name: &str, enabled: bool) -> Result<(), ExtensionError> {
        if self.is_external(name) {
            set_flag(&mut self.contents.external_disabled, name, !enabled);
        } else if let Some(server) = self.contents.servers.iter_mut().find(|server| server.name == name) {
            server.enabled = enabled;
        }
        self.save()
    }

    /// Sets whether a server's tool schemas are sent to the model directly (vs discover-then-call).
    pub fn set_server_exposed(&mut self, name: &str, exposed: bool) -> Result<(), ExtensionError> {
        if self.is_external(name) {
            set_flag(&mut self.contents.external_exposed, name, exposed);
        } else if let Some(server) = self.contents.servers.iter_mut().find(|server| server.name == name) {
            server.expose_to_agent = exposed;
        }
        self.save()
    }

    /// Chooses which of a server's tools the model may use. `None` allows all. Only re-code's own
    /// servers can be filtered; copy an external one in first (`i`).
    pub fn set_tool_filter(&mut self, name: &str, tools: Option<ToolFilter>) -> Result<(), ExtensionError> {
        if self.is_external(name) {
            return Err(ExtensionError::Invalid(format!(
                "{name} is read from another tool's configuration. Press i to copy it into re-code, then filter the copy."
            )));
        }
        let server = self
            .contents
            .servers
            .iter_mut()
            .find(|server| server.name == name)
            .ok_or_else(|| ExtensionError::Invalid(format!("There's no server called {name}.")))?;
        server.tools = tools.filter(|filter| !filter.include.is_empty() || !filter.exclude.is_empty());
        self.save()
    }

    /// Adds or replaces a custom tool.
    pub fn save_tool(&mut self, original: Option<&str>, mut tool: CustomTool) -> Result<(), ExtensionError> {
        // The form has no network field, so an edit keeps whatever the config file says.
        if let Some(existing) = original.and_then(|name| self.contents.tools.iter().find(|t| t.name == name)) {
            tool.network |= existing.network;
        }
        upsert(&mut self.contents.tools, original, tool, |item| &item.name, "tool")?;
        self.save()
    }

    pub fn delete_tool(&mut self, name: &str) -> Result<(), ExtensionError> {
        self.contents.tools.retain(|tool| tool.name != name);
        self.save()
    }

    pub fn set_tool_enabled(&mut self, name: &str, enabled: bool) -> Result<(), ExtensionError> {
        if let Some(tool) = self.contents.tools.iter_mut().find(|tool| tool.name == name) {
            tool.enabled = enabled;
        }
        self.save()
    }

    /// Creates a skill from a template, and returns the file to edit.
    pub fn create_skill(&self, name: &str, description: &str) -> Result<PathBuf, ExtensionError> {
        validate_name("skill", name).map_err(ExtensionError::Invalid)?;
        skills::create(&self.locations.user_skills, name, description)
    }

    /// Deletes one of re-code's own skills. Others can't be deleted from here.
    pub fn delete_skill(&mut self, name: &str) -> Result<(), ExtensionError> {
        validate_name("skill", name).map_err(ExtensionError::Invalid)?;
        skills::delete(&self.locations.user_skills, name)?;
        if self.contents.disabled_skills.remove(name) {
            self.save()?;
        }
        Ok(())
    }

    pub fn set_skill_enabled(&mut self, name: &str, enabled: bool) -> Result<(), ExtensionError> {
        if enabled {
            self.contents.disabled_skills.remove(name);
        } else {
            self.contents.disabled_skills.insert(name.to_owned());
        }
        self.save()
    }
}

fn upsert<T>(
    items: &mut Vec<T>,
    original: Option<&str>,
    item: T,
    name_of: impl Fn(&T) -> &String,
    kind: &str,
) -> Result<(), ExtensionError> {
    let new_name = name_of(&item).clone();
    let clash = items.iter().any(|existing| *name_of(existing) == new_name && original != Some(new_name.as_str()));
    if clash {
        return Err(ExtensionError::Invalid(format!("There's already a {kind} called {new_name}.")));
    }
    match original.and_then(|name| items.iter().position(|existing| name_of(existing) == name)) {
        Some(index) => items[index] = item,
        None => items.push(item),
    }
    Ok(())
}

/// Writes through a temporary file, so a crash can't leave a half-written configuration. Values in this file can be
/// secrets, so only the owner can read it.
fn write_private(path: &Path, text: &str) -> std::io::Result<()> {
    if let Some(parent) = path.parent() {
        std::fs::create_dir_all(parent)?;
    }
    let temporary = path.with_extension("toml.tmp");
    let mut options = std::fs::OpenOptions::new();
    options.write(true).create(true).truncate(true);
    #[cfg(unix)]
    std::os::unix::fs::OpenOptionsExt::mode(&mut options, 0o600);
    {
        use std::io::Write;
        let mut file = options.open(&temporary)?;
        file.write_all(text.as_bytes())?;
        file.sync_all()?;
    }
    std::fs::rename(&temporary, path)
}

#[cfg(test)]
mod tests {
    use super::*;
    use recode_core::extensions::{SkillSource, parse_mcp_form, parse_tool_form};

    fn locations(dir: &Path) -> Locations {
        Locations {
            config_file: dir.join("config/extensions.toml"),
            user_skills: dir.join("config/skills"),
            project_skills: Some(dir.join("project/.re-code/skills")),
            claude_skills: Some(dir.join("claude/skills")),
            claude_config: Some(dir.join("claude.json")),
            project: Some(dir.join("project")),
        }
    }

    #[test]
    fn claude_code_servers_are_used_live_with_choices_stored_here() {
        let dir = tempfile::tempdir().unwrap();
        let project = dir.path().join("project").to_string_lossy().into_owned();
        let claude = serde_json::json!({
            "mcpServers": {
                "rustrover": {"type": "http", "url": "http://127.0.0.1:64522/stream"},
                "mine": {"command": "from-claude"}
            },
            "projects": {project: {"mcpServers": {"rea": {"command": "npx", "args": ["rea"]}}}}
        });
        std::fs::write(dir.path().join("claude.json"), claude.to_string()).unwrap();
        let mut extensions = Extensions::load(locations(dir.path()));
        extensions.save_server(None, server("mine")).unwrap();

        let snapshot = extensions.snapshot();
        let names: Vec<&str> = snapshot.servers.iter().map(|s| s.name.as_str()).collect();
        assert_eq!(names, ["mine", "rea", "rustrover"], "re-code's own `mine` wins over Claude Code's");
        assert!(snapshot.servers[0].external.is_none());
        assert!(snapshot.servers[1..].iter().all(|s| s.enabled && s.external.is_some()));

        extensions.set_server_enabled("rustrover", false).unwrap();
        extensions.set_server_exposed("rea", true).unwrap();
        assert!(extensions.delete_server("rea").unwrap_err().to_string().contains("Claude Code"));

        // Choices survive a restart; the servers themselves are never copied into re-code's file.
        let reloaded = Extensions::load(locations(dir.path()));
        let rustrover = reloaded.server("rustrover").unwrap();
        assert!(!rustrover.enabled);
        assert!(reloaded.server("rea").unwrap().expose_to_agent);
        let file = std::fs::read_to_string(dir.path().join("config/extensions.toml")).unwrap();
        assert!(!file.contains("64522"), "{file}");
    }

    fn server(name: &str) -> McpServer {
        parse_mcp_form(name, "run --flag", "KEY=value", true).unwrap()
    }

    #[test]
    fn servers_and_tools_survive_a_restart() {
        let dir = tempfile::tempdir().unwrap();
        let mut extensions = Extensions::load(locations(dir.path()));
        extensions.save_server(None, server("one")).unwrap();
        extensions.save_tool(None, parse_tool_form("t", "d", "x {a}", "a", true).unwrap()).unwrap();
        extensions.set_server_enabled("one", false).unwrap();

        let reloaded = Extensions::load(locations(dir.path())).snapshot();
        assert_eq!(reloaded.servers.len(), 1);
        assert!(!reloaded.servers[0].enabled);
        assert_eq!(reloaded.tools[0].parameters, ["a"]);
    }

    #[test]
    fn renaming_replaces_and_duplicates_are_refused() {
        let dir = tempfile::tempdir().unwrap();
        let mut extensions = Extensions::load(locations(dir.path()));
        extensions.save_server(None, server("one")).unwrap();
        extensions.save_server(None, server("two")).unwrap();
        extensions.save_server(Some("one"), server("uno")).unwrap();
        let names: Vec<_> = extensions.snapshot().servers.into_iter().map(|s| s.name).collect();
        assert_eq!(names, ["uno", "two"]);
        assert!(extensions.save_server(None, server("two")).is_err());
        assert!(extensions.save_server(Some("uno"), server("two")).is_err());
        assert!(extensions.save_server(Some("two"), server("two")).is_ok());
    }

    #[test]
    fn deleting_removes_only_that_entry() {
        let dir = tempfile::tempdir().unwrap();
        let mut extensions = Extensions::load(locations(dir.path()));
        extensions.save_server(None, server("one")).unwrap();
        extensions.save_server(None, server("two")).unwrap();
        extensions.delete_server("one").unwrap();
        assert_eq!(Extensions::load(locations(dir.path())).snapshot().servers[0].name, "two");
    }

    #[test]
    fn a_damaged_file_is_reported_and_never_overwritten() {
        let dir = tempfile::tempdir().unwrap();
        let places = locations(dir.path());
        std::fs::create_dir_all(places.config_file.parent().unwrap()).unwrap();
        std::fs::write(&places.config_file, "[[mcp]\nbroken").unwrap();
        let mut extensions = Extensions::load(places.clone());
        assert!(extensions.snapshot().load_error.is_some());
        assert!(extensions.save_server(None, server("one")).is_err());
        assert_eq!(std::fs::read_to_string(&places.config_file).unwrap(), "[[mcp]\nbroken");
    }

    #[cfg(unix)]
    #[test]
    fn the_file_is_private_and_no_temporary_remains() {
        use std::os::unix::fs::PermissionsExt;
        let dir = tempfile::tempdir().unwrap();
        let places = locations(dir.path());
        let mut extensions = Extensions::load(places.clone());
        extensions.save_server(None, server("one")).unwrap();
        let mode = std::fs::metadata(&places.config_file).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600);
        assert!(!places.config_file.with_extension("toml.tmp").exists());
    }

    #[test]
    fn skills_can_be_created_found_disabled_and_deleted() {
        let dir = tempfile::tempdir().unwrap();
        let mut extensions = Extensions::load(locations(dir.path()));
        let path = extensions.create_skill("review", "Review a diff").unwrap();
        assert!(path.ends_with("review/SKILL.md"));
        assert!(extensions.create_skill("review", "again").is_err(), "an existing skill must not be replaced");

        let found = &extensions.snapshot().skills[0];
        assert_eq!((found.name.as_str(), found.description.as_str()), ("review", "Review a diff"));
        assert_eq!(found.source, SkillSource::User);
        assert!(found.enabled);

        extensions.set_skill_enabled("review", false).unwrap();
        assert!(!Extensions::load(locations(dir.path())).snapshot().skills[0].enabled);

        extensions.delete_skill("review").unwrap();
        assert!(extensions.snapshot().skills.is_empty());
        assert!(!path.exists());
    }

    #[test]
    fn skills_outside_our_directory_are_read_only_and_project_ones_stay_off() {
        let dir = tempfile::tempdir().unwrap();
        let places = locations(dir.path());
        for root in [places.project_skills.as_ref().unwrap(), places.claude_skills.as_ref().unwrap()] {
            let skill = root.join("borrowed");
            std::fs::create_dir_all(&skill).unwrap();
            std::fs::write(skill.join("SKILL.md"), "---\nname: borrowed\ndescription: From elsewhere\n---\nBody\n")
                .unwrap();
        }
        let mut extensions = Extensions::load(places.clone());
        let skills = extensions.snapshot().skills;
        let project = skills.iter().find(|skill| skill.source == SkillSource::Project).unwrap();
        assert!(!project.enabled, "a repository must not switch on its own instructions");

        extensions.delete_skill("borrowed").unwrap_err();
        assert!(places.claude_skills.unwrap().join("borrowed/SKILL.md").exists());
    }

    #[test]
    fn deleting_cannot_reach_outside_the_skills_directory() {
        let dir = tempfile::tempdir().unwrap();
        let mut extensions = Extensions::load(locations(dir.path()));
        std::fs::write(dir.path().join("precious"), "keep").unwrap();
        assert!(extensions.delete_skill("../../precious").is_err());
        assert!(dir.path().join("precious").exists());
    }
}
