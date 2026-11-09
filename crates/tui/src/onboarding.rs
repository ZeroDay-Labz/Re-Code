//! The three-step tour on first run. It only describes what re-code found and where things are; it
//! starts nothing, downloads nothing and changes nothing but the record that it was shown.

use crate::app::{Action, App, Overlay};
use crate::theme::{self, Token};
use crate::view::{boxed, overlay_area};
use crate::welcome::{HomeTab, ProviderState};
use crate::widgets::legend;
use crate::wrap::wrap_plain;
use crossterm::event::{KeyCode, KeyEvent};
use ratatui::Frame;
use ratatui::layout::Rect;
use ratatui::style::{Modifier, Style};
use ratatui::text::{Line, Span};

/// The setting that records the tour was shown, so it never opens by itself again.
pub const SHOWN_KEY: &str = "onboarded";
const STEPS: usize = 3;

impl App {
    /// Opens the tour on a first run: nothing saved before (no sessions, no record of the tour), on
    /// the home screen, with nothing else open. `/tour` opens it any time.
    pub fn maybe_onboard(&mut self) {
        if self.demo
            || self.no_onboarding
            || !self.settings_loaded
            || self.settings.contains_key(SHOWN_KEY)
            || !self.recent_sessions.is_empty()
            || self.overlay.is_some()
            || !matches!(self.screen, crate::app::Screen::Welcome(_))
        {
            return;
        }
        self.overlay = Some(Overlay::Onboarding { step: 0 });
    }

    /// Ends the tour and records it, then does what the key asked for.
    fn finish_tour(&mut self, then: Option<Then>) -> Vec<Action> {
        self.overlay = None;
        self.settings.insert(SHOWN_KEY.to_owned(), "1".to_owned());
        let mut actions = vec![Action::SetSetting { key: SHOWN_KEY.to_owned(), value: "1".to_owned() }];
        match then {
            Some(Then::Models) => {
                if let crate::app::Screen::Welcome(welcome) = &mut self.screen {
                    welcome.tab = HomeTab::Models;
                }
            }
            Some(Then::Free) => self.open_free_setup(),
            Some(Then::Web) => self.open_web_setup(),
            Some(Then::Browse) => actions.extend(self.open_skill_search(None, None)),
            Some(Then::Help) => self.overlay = Some(Overlay::Help { offset: 0 }),
            None => {}
        }
        actions
    }

    pub(crate) fn onboarding_key(&mut self, key: KeyEvent) -> Vec<Action> {
        let Some(Overlay::Onboarding { step }) = &mut self.overlay else { return Vec::new() };
        let at = *step;
        match key.code {
            KeyCode::Esc => return self.finish_tour(None),
            KeyCode::Left | KeyCode::Char('b') if at > 0 => *step -= 1,
            KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') if at + 1 < STEPS => *step += 1,
            KeyCode::Right | KeyCode::Enter | KeyCode::Char(' ') => return self.finish_tour(None),
            KeyCode::Char('m') if at == 0 => return self.finish_tour(Some(Then::Models)),
            KeyCode::Char('f') if at == 0 => return self.finish_tour(Some(Then::Free)),
            KeyCode::Char('w') if at == 1 => return self.finish_tour(Some(Then::Web)),
            KeyCode::Char('s') if at == 1 => return self.finish_tour(Some(Then::Browse)),
            KeyCode::F(1) => return self.finish_tour(Some(Then::Help)),
            _ => {}
        }
        Vec::new()
    }
}

enum Then {
    Models,
    Free,
    Web,
    Browse,
    Help,
}

fn bullet(on: bool, text: &str, width: usize) -> Vec<Line<'static>> {
    let (mark, style) =
        if on { ("● ", Style::new().fg(theme::color(Token::Success))) } else { ("○ ", theme::muted()) };
    let mut lines = wrap_plain(text, Style::new(), width.saturating_sub(4));
    for (index, line) in lines.iter_mut().enumerate() {
        let lead = if index == 0 { Span::styled(format!("  {mark}"), style) } else { Span::raw("    ") };
        line.spans.insert(0, lead);
    }
    lines
}

fn plain(text: &str, width: usize) -> Vec<Line<'static>> {
    wrap_plain(text, Style::new(), width)
}

fn muted(text: &str, width: usize) -> Vec<Line<'static>> {
    wrap_plain(text, theme::muted(), width)
}

/// The step's lines: what re-code found, what it means, and the keys.
fn step_lines(app: &App, step: usize, width: usize) -> (String, Vec<Line<'static>>, Vec<(&'static str, &'static str)>) {
    let mut lines = Vec::new();
    match step {
        0 => {
            lines.extend(plain(
                "re-code is a coding agent for your own machine. It reads and changes files in this folder with your approval, using a model on your GPU or one in the cloud.",
                width,
            ));
            lines.push(Line::default());
            lines.push(Line::styled("What it found", Style::new().fg(theme::color(Token::Attention))));
            match &app.welcome_info {
                None => lines.extend(bullet(false, "Still looking for a local runtime…", width)),
                Some(info) => {
                    let local = info.models.iter().filter(|m| m.provider_label.is_none()).count();
                    let cloud = info.models.len() - local;
                    match &info.provider {
                        ProviderState::Ready { detail } => {
                            let models =
                                if local == 1 { "1 local model".to_owned() } else { format!("{local} local models") };
                            lines.extend(bullet(local > 0, &format!("{detail}: {models}"), width));
                        }
                        ProviderState::Unavailable { headline, .. } => lines.extend(bullet(false, headline, width)),
                    }
                    if cloud > 0 {
                        lines.extend(bullet(true, &format!("{cloud} cloud models from the keys you've added"), width));
                    } else {
                        lines.extend(bullet(
                            false,
                            "No cloud keys yet. Some providers have free models; f sets one up.",
                            width,
                        ));
                    }
                }
            }
            lines.push(Line::default());
            lines.extend(muted(
                "Nothing starts by itself. When you're ready, pick a model on the Models tab: ● fits your GPU, ◐ spills into memory, ○ is too large.",
                width,
            ));
            (
                "Welcome to re-code".to_owned(),
                lines,
                vec![("enter", "next"), ("m", "models"), ("f", "free cloud models"), ("esc", "skip")],
            )
        }
        1 => {
            lines.extend(plain(
                "The model gets tools of its own (read, search, edit, run, git) and whatever you connect.",
                width,
            ));
            lines.push(Line::default());
            lines.push(Line::styled("Already here", Style::new().fg(theme::color(Token::Attention))));
            match &app.extensions {
                None => lines.extend(bullet(false, "Still reading your configuration…", width)),
                Some(extensions) => {
                    let servers = extensions.servers.len();
                    let borrowed = extensions.servers.iter().filter(|s| s.external.is_some()).count();
                    let from =
                        if borrowed > 0 { format!(", {borrowed} read live from Claude Code") } else { String::new() };
                    lines.extend(bullet(servers > 0, &format!("{servers} MCP servers{from}"), width));
                    let skills = extensions.skills.len();
                    lines.extend(bullet(
                        skills > 0,
                        &format!("{skills} skills, loaded only when a task matches"),
                        width,
                    ));
                    let plugins = extensions.plugins.len();
                    lines.extend(bullet(plugins > 0, &format!("{plugins} plugins"), width));
                }
            }
            lines.push(Line::default());
            lines.extend(muted(
                "w gives the model web search (your own SearXNG, DuckDuckGo, Brave…). s browses the Hermes index of 100 000 skills; every install is fetched, scanned and shown before it's enabled.",
                width,
            ));
            (
                "What it can reach".to_owned(),
                lines,
                vec![("enter", "next"), ("w", "web search"), ("s", "browse skills"), ("←", "back"), ("esc", "skip")],
            )
        }
        _ => {
            lines.extend(bullet(true, "Edits, deletes and commands wait for you, with the diff. y once, a for the session, p for this project.", width));
            lines.extend(bullet(true, "Commands run in a bubblewrap sandbox: the project writable, the rest read-only, no network unless you allow it.", width));
            lines.extend(bullet(
                true,
                "Ctrl+Z takes back a turn's file changes; Esc Esc rewinds to an earlier request.",
                width,
            ));
            lines.extend(bullet(true, "A folder's own skills and settings stay off until you /trust it.", width));
            lines.extend(bullet(
                true,
                "Every token sent is counted and shown; estimates are labelled as estimates.",
                width,
            ));
            lines.push(Line::default());
            lines.extend(muted("F1 lists every key; Ctrl+K opens the command palette. /tour shows this again.", width));
            ("How it stays in your hands".to_owned(), lines, vec![("enter", "start"), ("F1", "keys"), ("←", "back")])
        }
    }
}

/// The tour window: a step marker, the step, its keys.
pub fn draw(frame: &mut Frame<'_>, area: Rect, app: &App, step: usize) {
    let rect = overlay_area(area, 78, 24);
    let inner = usize::from(rect.width).saturating_sub(4);
    let (title, body, keys) = step_lines(app, step, inner);
    let mut lines = Vec::new();
    let mut marker = Vec::new();
    for index in 0..STEPS {
        let style = if index == step { theme::accent().add_modifier(Modifier::BOLD) } else { theme::faint() };
        marker.push(Span::styled(if index == step { "●" } else { "○" }, style));
        marker.push(Span::raw(" "));
    }
    marker.push(Span::styled(format!(" {} of {STEPS}", step + 1), theme::muted()));
    lines.push(Line::from(marker));
    lines.push(Line::default());
    lines.extend(body);
    lines.push(Line::default());
    lines.push(legend(&keys, inner));
    let wanted = u16::try_from(lines.len() + 2).unwrap_or(24);
    let rect = overlay_area(area, 78, wanted.max(8));
    boxed(frame, rect, &title, lines);
}
