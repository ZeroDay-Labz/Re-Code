//! The project's files as a folding tree, for the side panel. Built from the flat list the driver
//! sends; folders start closed except the top level, and clicking opens them or the file.

use std::collections::BTreeSet;

/// One visible row of the tree.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TreeRow {
    pub depth: usize,
    pub name: String,
    /// The full path relative to the project.
    pub path: String,
    pub is_dir: bool,
    pub open: bool,
}

/// Which folders are open.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct Tree {
    pub open: BTreeSet<String>,
    /// The row at the top of the pane, for scrolling.
    pub scroll: usize,
}

impl Tree {
    /// Opens a folder if closed, closes it if open.
    pub fn toggle(&mut self, dir: &str) {
        if !self.open.remove(dir) {
            self.open.insert(dir.to_owned());
        }
    }

    /// The rows to show, given the project's files (relative paths, any order).
    #[must_use]
    pub fn rows(&self, files: &[String]) -> Vec<TreeRow> {
        let mut sorted: Vec<&str> = files.iter().map(String::as_str).collect();
        sorted.sort_by_key(|path| sort_key(path));
        let mut rows = Vec::new();
        let mut seen_dirs: BTreeSet<String> = BTreeSet::new();
        for path in sorted {
            let parts: Vec<&str> = path.split('/').filter(|part| !part.is_empty()).collect();
            if parts.is_empty() {
                continue;
            }
            // Each folder on the way appears once; what's under a closed folder stays hidden.
            let mut hidden = false;
            for depth in 0..parts.len() - 1 {
                let dir = parts[..=depth].join("/");
                if !hidden && seen_dirs.insert(dir.clone()) {
                    rows.push(TreeRow {
                        depth,
                        name: parts[depth].to_owned(),
                        path: dir.clone(),
                        is_dir: true,
                        open: self.open.contains(&dir),
                    });
                }
                if !self.open.contains(&dir) {
                    hidden = true;
                }
            }
            if !hidden {
                rows.push(TreeRow {
                    depth: parts.len() - 1,
                    name: parts[parts.len() - 1].to_owned(),
                    path: path.to_owned(),
                    is_dir: false,
                    open: false,
                });
            }
        }
        rows
    }
}

/// Folders before files at each level, then alphabetical, like a file manager.
fn sort_key(path: &str) -> Vec<(bool, String)> {
    let parts: Vec<&str> = path.split('/').collect();
    parts.iter().enumerate().map(|(index, part)| (index + 1 == parts.len(), part.to_lowercase())).collect()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn files() -> Vec<String> {
        ["src/main.rs", "src/ui/view.rs", "README.md", "src/ui/app.rs", "docs/guide.md"]
            .iter()
            .map(|f| (*f).to_owned())
            .collect()
    }

    #[test]
    fn folders_start_closed_and_open_on_toggle() {
        let mut tree = Tree::default();
        let names: Vec<String> =
            tree.rows(&files()).iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.name)).collect();
        assert_eq!(names, ["docs", "src", "README.md"], "folders first, closed, then files");
        tree.toggle("src");
        let names: Vec<String> =
            tree.rows(&files()).iter().map(|r| format!("{}{}", "  ".repeat(r.depth), r.name)).collect();
        assert_eq!(names, ["docs", "src", "  ui", "  main.rs", "README.md"]);
        tree.toggle("src/ui");
        let rows = tree.rows(&files());
        assert!(rows.iter().any(|r| r.path == "src/ui/app.rs" && r.depth == 2));
        tree.toggle("src");
        assert!(!tree.rows(&files()).iter().any(|r| r.path.starts_with("src/")), "closing the parent hides it all");
    }
}
