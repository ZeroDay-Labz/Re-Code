//! Content search, using the same libraries as ripgrep (`ignore` + `grep-searcher`), honouring
//! ignore files and skipping binary files.

use crate::{ToolOutput, Workspace};
use grep_searcher::sinks::UTF8;
use grep_searcher::{BinaryDetection, SearcherBuilder};
use ignore::WalkBuilder;

const MAX_MATCHES: usize = 200;

/// The project's files, relative to its root, skipping what ignore files exclude (like `target/`).
/// Capped at `limit`, so a huge tree can't stall the interface.
#[must_use]
pub fn project_files(workspace: &Workspace, limit: usize) -> Vec<String> {
    let mut files = Vec::new();
    for entry in WalkBuilder::new(workspace.root()).hidden(true).build().flatten() {
        if files.len() >= limit {
            break;
        }
        if entry.file_type().is_some_and(|kind| kind.is_file())
            && let Ok(relative) = entry.path().strip_prefix(workspace.root())
        {
            files.push(relative.to_string_lossy().into_owned());
        }
    }
    files.sort();
    files
}

/// Searches file contents for a regular expression beneath `path` (default: the whole workspace).
pub(crate) fn grep(workspace: &Workspace, pattern: &str, path: Option<&str>) -> ToolOutput {
    let matcher = match grep_regex::RegexMatcher::new_line_matcher(pattern) {
        Ok(matcher) => matcher,
        Err(error) => return ToolOutput::error(format!("invalid search pattern: {error}")),
    };
    let location = match workspace.locate(path.unwrap_or(".")) {
        Ok(location) => location,
        Err(error) => return ToolOutput::error(error),
    };
    let start = match &location {
        // Inside the project the start is checked with the kernel's confinement first.
        crate::Location::Inside(relative) => {
            if workspace.list(relative).is_err() && workspace.read(relative).is_err() {
                return ToolOutput::error(format!("{relative} isn't in the workspace"));
            }
            workspace.root().join(relative)
        }
        crate::Location::Outside(path) if path.exists() => path.clone(),
        crate::Location::Outside(_) => return ToolOutput::error(format!("{} doesn't exist", location.display())),
    };
    let mut results = Vec::new();
    let mut truncated = false;

    let walk = WalkBuilder::new(&start).hidden(false).build();
    for entry in walk.flatten() {
        if !entry.file_type().is_some_and(|file_type| file_type.is_file()) {
            continue;
        }
        if results.len() >= MAX_MATCHES {
            truncated = true;
            break;
        }
        let display = match entry.path().strip_prefix(workspace.root()) {
            Ok(relative) => relative.display().to_string(),
            Err(_) => crate::workspace::tilde(entry.path()),
        };
        let mut searcher = SearcherBuilder::new().binary_detection(BinaryDetection::quit(0)).build();
        let _ = searcher.search_path(
            &matcher,
            entry.path(),
            UTF8(|line_number, line| {
                if results.len() < MAX_MATCHES {
                    results.push(format!("{display}:{line_number}: {}", line.trim_end()));
                }
                Ok(true)
            }),
        );
    }

    if results.is_empty() {
        return ToolOutput::ok("No matches.".to_owned(), "0 matches".to_owned());
    }
    let count = results.len();
    if truncated {
        results.push(format!("… (more than {MAX_MATCHES} matches; narrow the search)"));
    }
    ToolOutput::ok(results.join("\n"), format!("{count} matches"))
}
