//! Forms for adding and editing extensions, and confirmations before anything risky.

use crate::welcome::HomeTab;
use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use recode_core::extensions::{
    CustomTool, ExtensionCommand, McpPreset, McpServer, parse_mcp_form, parse_tool_form, validate_name,
};
use recode_core::providers::{CloudProvider, ProviderKind, ProviderPreset, parse_provider_form};

/// One field.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormField {
    pub label: &'static str,
    pub hint: &'static str,
    pub value: String,
    /// When non-empty, this field is a choice cycled with left/right rather than typed.
    pub choices: Vec<&'static str>,
    /// A secret, shown as dots.
    pub secret: bool,
}

/// What a form produces.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum FormKind {
    Server { original: Option<String>, enabled: bool },
    Tool { original: Option<String>, enabled: bool },
    Provider { original: Option<String>, enabled: bool },
    NewSkill,
}

/// An open form.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormState {
    pub title: String,
    pub kind: FormKind,
    pub fields: Vec<FormField>,
    pub focus: usize,
    pub error: Option<String>,
}

/// The result of a key press.
#[derive(Debug, PartialEq, Eq)]
pub enum FormOutcome {
    Stay,
    Cancel,
    Submit(Box<ExtensionCommand>),
}

const CONNECTION_HINT: &str = "A command (npx -y …), an http(s) address, a GitHub repo or server.json link, a registry id \
    (io.github.owner/name), or a pasted mcpServers snippet.";

/// Whether a connection string is a link that needs resolving (vs a command or direct http endpoint).
fn connection_is_link(connection: &str) -> bool {
    let c = connection.trim();
    let registry_id = !c.contains("://")
        && !c.contains(char::is_whitespace)
        && c.split_once('/')
            .is_some_and(|(namespace, name)| namespace.contains('.') && !name.is_empty() && !name.contains('/'));
    c.ends_with(".json")
        || c.contains("server.json")
        || c.starts_with("https://github.com/")
        || c.starts_with('{')
        || c.starts_with("\"mcpServers\"")
        || registry_id
}
const SETTINGS_HINT: &str = "Optional. KEY=value, KEY2=${FROM_ENVIRONMENT}. Prefer ${...} for secrets.";

const SANDBOX_CHOICES: &[&str] = &["bubblewrap", "none (host)"];

fn sandbox_choice(sandbox: recode_core::extensions::McpSandbox) -> &'static str {
    match sandbox {
        recode_core::extensions::McpSandbox::Bubblewrap => SANDBOX_CHOICES[0],
        recode_core::extensions::McpSandbox::None => SANDBOX_CHOICES[1],
    }
}

fn parse_sandbox_choice(value: &str) -> recode_core::extensions::McpSandbox {
    if value.trim().starts_with("none") {
        recode_core::extensions::McpSandbox::None
    } else {
        recode_core::extensions::McpSandbox::Bubblewrap
    }
}

fn field(label: &'static str, hint: &'static str, value: String) -> FormField {
    FormField { label, hint, value, choices: Vec::new(), secret: false }
}

fn choice_field(label: &'static str, hint: &'static str, choices: Vec<&'static str>, value: String) -> FormField {
    FormField { label, hint, value, choices, secret: false }
}

fn secret_field(label: &'static str, hint: &'static str) -> FormField {
    FormField { label, hint, value: String::new(), choices: Vec::new(), secret: true }
}

const PROVIDER_KINDS: [&str; 4] = ["Anthropic", "OpenAI", "Google Gemini", "OpenAI-compatible"];

fn provider_kind_of(label: &str) -> ProviderKind {
    ProviderKind::ALL.into_iter().find(|kind| kind.label() == label).unwrap_or(ProviderKind::OpenaiCompatible)
}

impl FormState {
    /// An empty form for this tab.
    #[must_use]
    pub fn new_for(tab: HomeTab) -> Self {
        match tab {
            HomeTab::Skills => Self {
                title: "New skill".to_owned(),
                kind: FormKind::NewSkill,
                fields: vec![
                    field("Name", "Letters, numbers, - and _.", String::new()),
                    field("When to use it", "The model reads this to decide when the skill applies.", String::new()),
                    field(
                        "Or install from",
                        "official/security/godmode, a name, owner/repo/folder, a GitHub or skills.sh link, or a folder. It's fetched and shown before anything is installed.",
                        String::new(),
                    ),
                ],
                focus: 0,
                error: None,
            },
            HomeTab::Tools => Self::tool_form("New tool", None, "", "", "", "", true),
            HomeTab::Providers => Self::provider_form("Add provider", None, "", ProviderKind::Anthropic, "", true),
            HomeTab::Models
            | HomeTab::Home
            | HomeTab::Settings
            | HomeTab::Runtimes
            | HomeTab::Mcp
            | HomeTab::Plugins => Self::server_form("Add MCP server", None, "", "", "", true),
        }
    }

    /// A form holding an existing server.
    #[must_use]
    pub fn edit_server(server: &McpServer) -> Self {
        let mut form = Self::server_form(
            &format!("Edit {}", server.name),
            Some(server.name.clone()),
            &server.name,
            &server.connection_text(),
            &server.settings_text(),
            server.enabled,
        );
        if let Some(field) = form.fields.get_mut(3) {
            field.value = sandbox_choice(server.sandbox).to_owned();
        }
        form
    }

    /// A form holding an existing tool.
    #[must_use]
    pub fn edit_tool(tool: &CustomTool) -> Self {
        Self::tool_form(
            &format!("Edit {}", tool.name),
            Some(tool.name.clone()),
            &tool.name,
            &tool.description,
            &tool.command,
            &tool.parameters.join(", "),
            tool.enabled,
        )
    }

    /// A form holding an existing provider. The key field is blank, since the stored key isn't shown.
    #[must_use]
    pub fn edit_provider(provider: &CloudProvider) -> Self {
        Self::provider_form(
            &format!("Edit {}", provider.name),
            Some(provider.name.clone()),
            &provider.name,
            provider.kind,
            provider.base_url.as_deref().unwrap_or(""),
            provider.enabled,
        )
    }

    /// A provider form pre-filled from a preset.
    #[must_use]
    pub fn from_provider_preset(preset: &ProviderPreset) -> Self {
        Self::provider_form("Add provider", None, preset.name, preset.kind, preset.base_url, true)
    }

    fn provider_form(
        title: &str,
        original: Option<String>,
        name: &str,
        kind: ProviderKind,
        base_url: &str,
        enabled: bool,
    ) -> Self {
        let key_hint = if original.is_some() { "Leave blank to keep the current key." } else { kind.key_help() };
        Self {
            title: title.to_owned(),
            kind: FormKind::Provider { original, enabled },
            fields: vec![
                field("Name", "A label, such as anthropic or openrouter.", name.to_owned()),
                choice_field("Kind", "Left and right change this.", PROVIDER_KINDS.to_vec(), kind.label().to_owned()),
                field(
                    "Base URL",
                    "Leave blank for the default. For OpenAI-compatible, e.g. http://localhost:1234/v1",
                    base_url.to_owned(),
                ),
                secret_field("API key", key_hint),
            ],
            focus: 0,
            error: None,
        }
    }

    /// A server form pre-filled from a preset.
    #[must_use]
    pub fn from_mcp_preset(preset: &McpPreset) -> Self {
        Self::server_form("Add MCP server", None, preset.name, preset.connection, preset.settings, true)
    }

    fn server_form(
        title: &str,
        original: Option<String>,
        name: &str,
        connection: &str,
        settings: &str,
        enabled: bool,
    ) -> Self {
        Self {
            title: title.to_owned(),
            kind: FormKind::Server { original, enabled },
            fields: vec![
                field("Name", "Letters, numbers, - and _.", name.to_owned()),
                field("Command, address, link or pasted config", CONNECTION_HINT, connection.to_owned()),
                field("Environment or headers", SETTINGS_HINT, settings.to_owned()),
                choice_field(
                    "Sandbox (stdio servers)",
                    "bubblewrap: project writable, home emptied except toolchains and package caches, secrets hidden, network on. none: runs on the host with everything you can see. ←→ to change.",
                    SANDBOX_CHOICES.to_vec(),
                    SANDBOX_CHOICES[0].to_owned(),
                ),
            ],
            focus: 0,
            error: None,
        }
    }

    fn tool_form(
        title: &str,
        original: Option<String>,
        name: &str,
        description: &str,
        command: &str,
        parameters: &str,
        enabled: bool,
    ) -> Self {
        Self {
            title: title.to_owned(),
            kind: FormKind::Tool { original, enabled },
            fields: vec![
                field("Name", "Letters, numbers, - and _.", name.to_owned()),
                field("What it does", "The model reads this to decide when to use it.", description.to_owned()),
                field(
                    "Command",
                    "Like: rg {query} docs. {name} becomes a parameter. Never run through a shell.",
                    command.to_owned(),
                ),
                field("Parameters", "Optional. Comma separated, like: query, path", parameters.to_owned()),
            ],
            focus: 0,
            error: None,
        }
    }

    /// Adds pasted text to the current field. Fields are one line.
    pub fn paste(&mut self, text: &str) {
        let clean: String =
            text.chars().map(|c| if c == '\n' || c == '\t' { ' ' } else { c }).filter(|c| !c.is_control()).collect();
        self.fields[self.focus].value.push_str(clean.trim_end());
        self.error = None;
    }

    /// Handles a key.
    pub fn handle_key(&mut self, key: KeyEvent) -> FormOutcome {
        let last = self.fields.len() - 1;
        let ctrl = key.modifiers.contains(KeyModifiers::CONTROL);
        match key.code {
            KeyCode::Esc => return FormOutcome::Cancel,
            KeyCode::Char('s') if ctrl => return self.submit(),
            KeyCode::Enter if self.focus == last => return self.submit(),
            KeyCode::Enter | KeyCode::Tab | KeyCode::Down => self.focus = (self.focus + 1).min(last),
            KeyCode::BackTab | KeyCode::Up => self.focus = self.focus.saturating_sub(1),
            KeyCode::Backspace => {
                self.fields[self.focus].value.pop();
                self.error = None;
            }
            KeyCode::Left if !self.fields[self.focus].choices.is_empty() => self.cycle_choice(-1),
            KeyCode::Right if !self.fields[self.focus].choices.is_empty() => self.cycle_choice(1),
            KeyCode::Char('u') if ctrl => {
                self.fields[self.focus].value.clear();
                self.error = None;
            }
            KeyCode::Char(c) if !ctrl && self.fields[self.focus].choices.is_empty() => {
                self.fields[self.focus].value.push(c);
                self.error = None;
            }
            _ => {}
        }
        FormOutcome::Stay
    }

    fn cycle_choice(&mut self, delta: isize) {
        let field = &mut self.fields[self.focus];
        let current = field.choices.iter().position(|choice| *choice == field.value).unwrap_or(0);
        let count = i32::try_from(field.choices.len()).unwrap_or(1).max(1);
        let next = usize::try_from(
            (i32::try_from(current).unwrap_or(0) + i32::try_from(delta).unwrap_or(0)).rem_euclid(count),
        )
        .unwrap_or(0);
        field.value = field.choices[next].to_owned();
        self.error = None;
    }

    fn submit(&mut self) -> FormOutcome {
        let value = |index: usize| self.fields[index].value.as_str();
        let result = match &self.kind {
            FormKind::Server { original, enabled } => {
                let connection = value(1).trim();
                if original.is_none() && connection_is_link(connection) {
                    let name = value(0).trim();
                    let name = (!name.is_empty()).then(|| name.to_owned());
                    Ok(ExtensionCommand::AddMcpFromLink { name, link: connection.to_owned() })
                } else {
                    let sandbox = parse_sandbox_choice(value(3));
                    parse_mcp_form(value(0), value(1), value(2), *enabled).map(|mut server| {
                        server.sandbox = sandbox;
                        ExtensionCommand::SaveServer { original: original.clone(), server }
                    })
                }
            }
            FormKind::Tool { original, enabled } => parse_tool_form(value(0), value(1), value(2), value(3), *enabled)
                .map(|tool| ExtensionCommand::SaveTool { original: original.clone(), tool }),
            FormKind::Provider { original, enabled } => {
                let key = value(3).trim();
                let key = if key.is_empty() { None } else { Some(key.to_owned()) };
                parse_provider_form(value(0), provider_kind_of(value(1)), value(2), *enabled)
                    .map(|provider| ExtensionCommand::SaveProvider { original: original.clone(), provider, key })
            }
            FormKind::NewSkill => {
                let import = value(2).trim();
                if import.is_empty() {
                    validate_name("skill", value(0).trim()).and_then(|()| {
                        if value(1).trim().is_empty() {
                            Err("Describe when this skill should be used, or fill in 'Or install from'.".to_owned())
                        } else {
                            Ok(ExtensionCommand::CreateSkill {
                                name: value(0).trim().to_owned(),
                                description: value(1).trim().to_owned(),
                            })
                        }
                    })
                } else {
                    Ok(ExtensionCommand::StageSkill(import.to_owned()))
                }
            }
        };
        match result {
            Ok(command) => FormOutcome::Submit(Box::new(command)),
            Err(error) => {
                self.error = Some(error);
                FormOutcome::Stay
            }
        }
    }
}

/// A question before doing something that can't be undone or that runs a program.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ConfirmState {
    pub title: String,
    pub body: Vec<String>,
    pub command: Confirmed,
}

/// What a Confirm overlay does on `y`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Confirmed {
    Extension(ExtensionCommand),
    /// Start the runtime with this id.
    StartRuntime(String),
    TrustProject,
    /// Write the `RE-CODE.md` that `/init` previewed.
    WriteInit,
    /// Delete a saved session (its messages and snapshot) by id.
    DeleteSession(String),
    /// Delete a memory re-code saved.
    ForgetMemory {
        scope: String,
        name: String,
    },
}

impl From<ExtensionCommand> for Confirmed {
    fn from(command: ExtensionCommand) -> Self {
        Self::Extension(command)
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use recode_core::extensions::McpTransport;

    fn key(code: KeyCode) -> KeyEvent {
        KeyEvent::new(code, KeyModifiers::NONE)
    }

    fn type_text(form: &mut FormState, text: &str) {
        for c in text.chars() {
            assert_eq!(form.handle_key(key(KeyCode::Char(c))), FormOutcome::Stay);
        }
    }

    #[test]
    fn filling_in_a_server_produces_a_command() {
        let mut form = FormState::new_for(HomeTab::Mcp);
        type_text(&mut form, "docs");
        form.handle_key(key(KeyCode::Tab));
        type_text(&mut form, "http://127.0.0.1:1/mcp");
        form.handle_key(key(KeyCode::Enter)); // settings
        form.handle_key(key(KeyCode::Enter)); // sandbox choice (left at bubblewrap)
        let FormOutcome::Submit(submitted) = form.handle_key(key(KeyCode::Enter)) else {
            panic!("the form didn't submit");
        };
        let ExtensionCommand::SaveServer { original: None, server } = *submitted else {
            panic!("the form didn't submit");
        };
        assert_eq!(server.name, "docs");
        assert!(matches!(server.transport, McpTransport::Http { .. }));
    }

    #[test]
    fn a_mistake_stays_on_the_form_with_a_message_and_typing_clears_it() {
        let mut form = FormState::new_for(HomeTab::Mcp);
        assert_eq!(form.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL)), FormOutcome::Stay);
        assert!(form.error.as_deref().unwrap().contains("needs a name"));
        type_text(&mut form, "a");
        assert_eq!(form.error, None);
    }

    #[test]
    fn editing_keeps_the_original_name_and_its_state() {
        let server = parse_mcp_form("one", "run --x", "K=v", false).unwrap();
        let mut form = FormState::edit_server(&server);
        assert_eq!(form.fields[2].value, "K=v");
        type_text(&mut form, "-renamed");
        let FormOutcome::Submit(submitted) = form.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::CONTROL))
        else {
            panic!("the form didn't submit");
        };
        let ExtensionCommand::SaveServer { original, server } = *submitted else {
            panic!("the form didn't submit");
        };
        assert_eq!(original.as_deref(), Some("one"));
        assert_eq!(server.name, "one-renamed");
        assert!(!server.enabled);
    }

    #[test]
    fn pasting_stays_on_one_line() {
        let mut form = FormState::new_for(HomeTab::Mcp);
        form.paste("a\nb\tc\x1b\n");
        assert_eq!(form.fields[0].value, "a b c");
    }

    #[test]
    fn escape_cancels() {
        let mut form = FormState::new_for(HomeTab::Skills);
        assert_eq!(form.handle_key(key(KeyCode::Esc)), FormOutcome::Cancel);
    }
}
