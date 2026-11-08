//! Suggestions while typing: `/` commands at the start of the input, and `@` file mentions anywhere.
//!
//! `@` matches the project's files fuzzily (the driver sends their list); `@~/…` and `@/…` browse the
//! filesystem. Everything here is computed from the text and the cursor, so it has no state to go stale.

use std::path::{Path, PathBuf};

/// A command typed with `/`.
pub struct SlashCommand {
    pub name: &'static str,
    /// Shown after the name when the command takes an argument.
    pub argument: &'static str,
    pub description: &'static str,
}

pub const SLASH_COMMANDS: &[SlashCommand] = &[
    SlashCommand { name: "help", argument: "", description: "Keyboard shortcuts" },
    SlashCommand { name: "model", argument: "", description: "Switch model" },
    SlashCommand { name: "mcp", argument: "", description: "MCP servers" },
    SlashCommand { name: "tools", argument: "", description: "Custom tools" },
    SlashCommand { name: "skills", argument: "", description: "Skills" },
    SlashCommand { name: "providers", argument: "", description: "Model providers and API keys" },
    SlashCommand { name: "add-dir", argument: "<folder>", description: "Let the agent read and change a folder" },
    SlashCommand { name: "undo", argument: "", description: "Undo the last turn's file changes" },
    SlashCommand { name: "compact", argument: "[focus]", description: "Compact the conversation now" },
    SlashCommand { name: "context", argument: "[raw]", description: "What fills the context window" },
    SlashCommand { name: "compactions", argument: "", description: "What each compaction kept and replaced" },
    SlashCommand { name: "init", argument: "", description: "Write a first RE-CODE.md for this project" },
    SlashCommand { name: "permissions", argument: "", description: "What runs without asking, and the mode" },
    SlashCommand { name: "plan", argument: "", description: "The plan written in plan mode" },
    SlashCommand { name: "rewind", argument: "", description: "Go back to an earlier request (also Esc Esc)" },
    SlashCommand { name: "export", argument: "[path|clipboard]", description: "Save the conversation as Markdown" },
    SlashCommand { name: "transcript", argument: "", description: "Search the whole conversation (also Ctrl+X)" },
    SlashCommand { name: "tasks", argument: "", description: "Background commands (shell … background, or !cmd &)" },
    SlashCommand {
        name: "memory",
        argument: "",
        description: "The instruction files in force (RE-CODE.md, AGENTS.md…)",
    },
    SlashCommand {
        name: "autocompact",
        argument: "<size|auto|off>",
        description: "Where compaction starts, e.g. 100k",
    },
    SlashCommand { name: "new", argument: "", description: "Start a new conversation (also /clear)" },
    SlashCommand { name: "diff", argument: "", description: "Every change made this session" },
    SlashCommand { name: "continue", argument: "", description: "Pick up an earlier session here" },
    SlashCommand { name: "copy", argument: "", description: "Copy the last code block" },
    SlashCommand { name: "theme", argument: "", description: "Change the colours" },
    SlashCommand { name: "free", argument: "", description: "Get free models (OpenRouter)" },
    SlashCommand { name: "trust", argument: "", description: "Trust this folder's configuration (project skills)" },
    SlashCommand {
        name: "skill",
        argument: "install|search|update [what]",
        description: "Install a skill from a hub (Hermes, GitHub, skills.sh), search, or update one",
    },
    SlashCommand { name: "plugins", argument: "", description: "Plugins and marketplaces" },
    SlashCommand {
        name: "web", argument: "", description: "Give the model web search (SearXNG, DuckDuckGo, Brave…)"
    },
    SlashCommand {
        name: "import",
        argument: "opencode|claude",
        description: "Bring in OpenCode's servers and providers, or Claude Code's MCP servers",
    },
    SlashCommand {
        name: "plugin",
        argument: "install|search|browse|update|marketplace add|remove [what]",
        description: "Install a plugin (Hermes catalog, Claude Code marketplaces, GitHub), or follow a marketplace",
    },
    SlashCommand { name: "files", argument: "", description: "The project's files in the side panel" },
    SlashCommand { name: "reasoning", argument: "", description: "Show or hide reasoning" },
    SlashCommand { name: "panel", argument: "", description: "Show or hide the side panel" },
    SlashCommand { name: "quit", argument: "", description: "Quit" },
];

/// What a suggestion is for.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Kind {
    Command,
    File,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Suggestion {
    /// Replaces the token being typed.
    pub replacement: String,
    pub label: String,
    pub detail: String,
    /// A command that runs as soon as it's chosen (it takes no argument).
    pub runs: bool,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Completion {
    pub kind: Kind,
    /// Byte offset where the token being completed starts.
    pub start: usize,
    pub items: Vec<Suggestion>,
}

/// At most this many suggestions are offered.
pub const LIMIT: usize = 8;

/// Suggestions for the text with the cursor at `cursor`, if the cursor is on a `/` or `@` token.
/// `skills` are the usable skills as (name, description); each is offered as `/name`.
#[must_use]
pub fn complete(text: &str, cursor: usize, files: &[String], skills: &[(String, String)]) -> Option<Completion> {
    let before = &text[..cursor.min(text.len())];
    let start = before.rfind(char::is_whitespace).map_or(0, |index| index + 1);
    let token = &before[start..];
    if let Some(query) = token.strip_prefix('/')
        && start == 0
        && !query.starts_with('/')
    {
        let items = commands(query, skills);
        return (!items.is_empty()).then_some(Completion { kind: Kind::Command, start, items });
    }
    let query = token.strip_prefix('@')?;
    let items = if query.starts_with('~') || query.starts_with('/') { browse(query) } else { project(query, files) };
    (!items.is_empty()).then_some(Completion { kind: Kind::File, start, items })
}

fn commands(query: &str, skills: &[(String, String)]) -> Vec<Suggestion> {
    let mut scored: Vec<(i64, Suggestion)> = SLASH_COMMANDS
        .iter()
        .filter_map(|command| {
            score(query, command.name).map(|score| {
                (
                    score,
                    Suggestion {
                        replacement: format!("/{} ", command.name),
                        label: if command.argument.is_empty() {
                            format!("/{}", command.name)
                        } else {
                            format!("/{} {}", command.name, command.argument)
                        },
                        detail: command.description.to_owned(),
                        runs: command.argument.is_empty(),
                    },
                )
            })
        })
        .collect();
    for (name, description) in skills {
        if let Some(score) = score(query, name) {
            scored.push((
                score,
                Suggestion {
                    replacement: format!("/{name} "),
                    label: format!("/{name} <request>"),
                    detail: format!("Skill: {description}"),
                    runs: false,
                },
            ));
        }
    }
    scored.sort_by_key(|(score, _)| std::cmp::Reverse(*score));
    scored.into_iter().take(LIMIT).map(|(_, suggestion)| suggestion).collect()
}

fn project(query: &str, files: &[String]) -> Vec<Suggestion> {
    let mut scored: Vec<(i64, &String)> = files
        .iter()
        .filter_map(|file| {
            let name = file.rsplit('/').next().unwrap_or(file);
            // A match in the file name counts for more than one in the folders around it.
            let best = [score(query, name).map(|s| s + 200), score(query, file)].into_iter().flatten().max()?;
            Some((best, file))
        })
        .collect();
    scored.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.len().cmp(&b.1.len())));
    scored
        .into_iter()
        .take(LIMIT)
        .map(|(_, file)| Suggestion {
            replacement: format!("@{file} "),
            label: file.clone(),
            detail: String::new(),
            runs: false,
        })
        .collect()
}

/// Lists a folder for `@~/…` or `@/…`, matching the last part of the path.
fn browse(query: &str) -> Vec<Suggestion> {
    let (folder, partial) = match query.rfind('/') {
        Some(index) => (&query[..=index], &query[index + 1..]),
        None => (query, ""),
    };
    let real: PathBuf = match folder.strip_prefix('~') {
        Some(rest) => match std::env::var_os("HOME") {
            Some(home) => Path::new(&home).join(rest.trim_start_matches('/')),
            None => return Vec::new(),
        },
        None => PathBuf::from(folder),
    };
    let Ok(entries) = std::fs::read_dir(&real) else { return Vec::new() };
    let show_hidden = partial.starts_with('.');
    let mut found: Vec<(i64, String, bool)> = entries
        .flatten()
        .filter_map(|entry| {
            let name = entry.file_name().to_string_lossy().into_owned();
            if name.starts_with('.') && !show_hidden {
                return None;
            }
            let score = if partial.is_empty() { Some(0) } else { score(partial, &name) }?;
            Some((score, name, entry.file_type().is_ok_and(|kind| kind.is_dir())))
        })
        .collect();
    found.sort_by(|a, b| b.0.cmp(&a.0).then_with(|| a.1.cmp(&b.1)));
    let folder = if folder.ends_with('/') { folder.to_owned() } else { format!("{folder}/") };
    found
        .into_iter()
        .take(LIMIT)
        .map(|(_, name, is_dir)| {
            let path = format!("{folder}{name}{}", if is_dir { "/" } else { "" });
            // A folder keeps the popup open for the next level, so no trailing space.
            let replacement = if is_dir { format!("@{path}") } else { format!("@{path} ") };
            Suggestion {
                replacement,
                label: path,
                detail: if is_dir { "folder".into() } else { String::new() },
                runs: false,
            }
        })
        .collect()
}

/// How well `query` matches `candidate`, case-insensitively: a prefix beats a substring, which beats
/// the letters appearing in order. `None` if they don't appear at all.
#[must_use]
pub fn score(query: &str, candidate: &str) -> Option<i64> {
    if query.is_empty() {
        return Some(0);
    }
    let query = query.to_lowercase();
    let candidate = candidate.to_lowercase();
    let length = i64::try_from(candidate.len()).unwrap_or(i64::MAX);
    if candidate.starts_with(&query) {
        return Some(3000 - length);
    }
    if let Some(index) = candidate.find(&query) {
        return Some(2000 - i64::try_from(index).unwrap_or(0) * 10 - length);
    }
    // The letters in order, as tightly as possible: try each place the first letter appears.
    let letters: Vec<char> = candidate.chars().collect();
    let wanted: Vec<char> = query.chars().collect();
    let best_gaps = (0..letters.len())
        .filter(|&start| letters[start] == wanted[0])
        .filter_map(|start| {
            let mut gaps = 0i64;
            let mut position = start;
            for letter in &wanted[1..] {
                let next = letters[position + 1..].iter().position(|c| c == letter)?;
                gaps += i64::try_from(next).unwrap_or(0);
                position += next + 1;
            }
            Some(gaps)
        })
        .min()?;
    Some(1000 - best_gaps * 5 - length)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<String> {
        ["src/main.rs", "src/chat.rs", "crates/app/src/chat.rs", "README.md", "docs/CHANGELOG.md"]
            .iter()
            .map(|f| (*f).to_owned())
            .collect()
    }

    #[test]
    fn a_slash_at_the_start_suggests_commands_and_elsewhere_does_not() {
        let found = complete("/un", 3, &[], &[]).unwrap();
        assert_eq!(found.kind, Kind::Command);
        assert_eq!(found.items[0].label, "/undo");
        assert!(found.items[0].runs);
        assert_eq!(complete("/", 1, &[], &[]).unwrap().items.len(), LIMIT);
        assert!(complete("see /un", 7, &[], &[]).is_none(), "only at the start");
        assert!(complete("//escaped", 9, &[], &[]).is_none(), "// sends a literal slash");
        let add = complete("/add", 4, &[], &[]).unwrap();
        assert!(!add.items[0].runs && add.items[0].label == "/add-dir <folder>", "it needs a folder");
    }

    #[test]
    fn at_mentions_match_file_names_first() {
        let found = complete("look at @chat", 13, &files(), &[]).unwrap();
        assert_eq!(found.kind, Kind::File);
        assert_eq!(found.start, 8);
        assert_eq!(found.items[0].label, "src/chat.rs", "shorter path wins between equal name matches");
        assert_eq!(found.items[0].replacement, "@src/chat.rs ");
        let fuzzy = complete("@rdme", 5, &files(), &[]).unwrap();
        assert_eq!(fuzzy.items[0].label, "README.md");
        assert!(complete("@zzzz", 5, &files(), &[]).is_none());
    }

    #[test]
    fn at_a_path_browses_the_filesystem() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::create_dir(dir.path().join("notes")).unwrap();
        std::fs::write(dir.path().join("notes.txt"), "").unwrap();
        std::fs::write(dir.path().join(".hidden"), "").unwrap();
        let query = format!("@{}/no", dir.path().display());
        let found = complete(&query, query.len(), &[], &[]).unwrap();
        let labels: Vec<&str> =
            found.items.iter().map(|s| s.label.rsplit('/').find(|p| !p.is_empty()).unwrap()).collect();
        assert_eq!(labels, ["notes", "notes.txt"]);
        assert!(found.items[0].replacement.ends_with("notes/"), "a folder keeps completing");
        let everything = format!("@{}/", dir.path().display());
        assert_eq!(
            complete(&everything, everything.len(), &[], &[]).unwrap().items.len(),
            2,
            "hidden files only when asked"
        );
    }

    #[test]
    fn prefixes_beat_substrings_beat_scattered_letters() {
        let prefix = score("cha", "chat.rs").unwrap();
        let inside = score("cha", "mychat.rs").unwrap();
        let scattered = score("cha", "c_h_a.rs").unwrap();
        assert!(prefix > inside && inside > scattered);
        assert_eq!(score("xyz", "chat.rs"), None);
    }
}

#[cfg(test)]
mod skill_command_tests {
    use super::*;

    #[test]
    fn usable_skills_are_offered_as_slash_commands() {
        let skills = vec![("review".to_owned(), "Review a diff for bugs.".to_owned())];
        let found = complete("/rev", 4, &[], &skills).unwrap();
        let skill = found.items.iter().find(|item| item.replacement == "/review ").expect("the skill");
        assert_eq!(skill.detail, "Skill: Review a diff for bugs.");
        assert!(!skill.runs, "a skill takes the request as its argument");
        assert!(complete("/tru", 4, &[], &[]).unwrap().items.iter().any(|item| item.replacement == "/trust "));
    }
}
