//! Watches the user's themes folder, so a theme file takes effect the moment it's saved.

use recode_tui::link::UiEvent;
use std::collections::BTreeMap;
use std::path::{Path, PathBuf};
use std::time::{Duration, SystemTime};
use tokio::sync::mpsc::UnboundedSender;

/// The folder's files and when they changed: the cheap way to notice an edit without a watcher crate.
fn stamp(dir: &Path) -> BTreeMap<PathBuf, SystemTime> {
    let Ok(entries) = std::fs::read_dir(dir) else { return BTreeMap::new() };
    entries
        .flatten()
        .filter(|entry| entry.path().extension().is_some_and(|ext| ext == "toml"))
        .filter_map(|entry| Some((entry.path(), entry.metadata().ok()?.modified().ok()?)))
        .collect()
}

/// Sends the themes now, then again whenever a file in the folder changes.
pub fn watch(dir: PathBuf, events: UnboundedSender<UiEvent>) {
    tokio::spawn(async move {
        let mut last = None;
        loop {
            let now = stamp(&dir);
            if last.as_ref() != Some(&now) {
                let (themes, problems) = recode_tui::theme_file::load_dir(&dir);
                if events.send(UiEvent::UserThemes { themes, problems }).is_err() {
                    return;
                }
                last = Some(now);
            }
            tokio::time::sleep(Duration::from_secs(1)).await;
        }
    });
}
