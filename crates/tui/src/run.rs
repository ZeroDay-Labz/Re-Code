//! The terminal itself: setup, restoration, and the event loop.

use crate::app::{Action, App, ToastLevel};
use crate::demo::Player;
use crate::link::{Link, UiCommand, UiEvent};
use crate::theme::ColorMode;
use crate::transcript::Entry;
use crate::view;
use anyhow::{Result, bail};
use crossterm::event::{
    DisableBracketedPaste, DisableFocusChange, DisableMouseCapture, EnableBracketedPaste, EnableFocusChange,
    EnableMouseCapture, Event, EventStream, KeyboardEnhancementFlags, MouseButton, MouseEventKind,
    PopKeyboardEnhancementFlags, PushKeyboardEnhancementFlags,
};
use crossterm::execute;
use crossterm::terminal::{BeginSynchronizedUpdate, EndSynchronizedUpdate};
use futures_util::StreamExt;
use ratatui::DefaultTerminal;
use recode_core::protocol::{AgentState, EventKind};
use std::io::{IsTerminal, stdout};
use std::time::{Duration, Instant};
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};

/// About 60 frames per second. Fast streams are combined rather than drawn piece by piece.
const FRAME: Duration = Duration::from_millis(16);
const TICK: Duration = Duration::from_millis(100);
const FAST_TICK: Duration = Duration::from_millis(33);
const WHEEL_LINES: isize = 3;

/// Whether re-code takes the mouse at all. `RE_CODE_DISABLE_MOUSE=1` leaves it to the terminal, for
/// people who rely on native selection; F2 toggles the same thing at run time.
fn mouse_wanted() -> bool {
    !std::env::var("RE_CODE_DISABLE_MOUSE").is_ok_and(|value| matches!(value.trim(), "1" | "true" | "yes"))
}

fn enable_terminal_modes(mouse: bool) -> std::io::Result<()> {
    execute!(
        stdout(),
        EnableBracketedPaste,
        EnableFocusChange,
        // Terminals that don't know this sequence ignore it. Those that do can report Shift+Enter.
        PushKeyboardEnhancementFlags(KeyboardEnhancementFlags::DISAMBIGUATE_ESCAPE_CODES)
    )?;
    if mouse {
        execute!(stdout(), EnableMouseCapture)?;
    }
    Ok(())
}

fn disable_terminal_modes() {
    // Best effort: this also runs while panicking, when nothing better can be done with an error.
    let _ =
        execute!(stdout(), PopKeyboardEnhancementFlags, DisableFocusChange, DisableMouseCapture, DisableBracketedPaste);
}

/// Restores the terminal however the interface ends, including a panic.
struct TerminalGuard;

impl TerminalGuard {
    fn new() -> Result<(Self, DefaultTerminal)> {
        let previous = std::panic::take_hook();
        std::panic::set_hook(Box::new(move |info| {
            disable_terminal_modes();
            ratatui::restore();
            previous(info);
        }));
        let terminal = ratatui::try_init()?;
        let guard = Self;
        enable_terminal_modes(mouse_wanted())?;
        Ok((guard, terminal))
    }
}

impl Drop for TerminalGuard {
    fn drop(&mut self) {
        disable_terminal_modes();
        ratatui::restore();
    }
}

fn project_name() -> String {
    std::env::current_dir()
        .ok()
        .and_then(|dir| Some(dir.file_name()?.to_string_lossy().into_owned()))
        .unwrap_or_else(|| "project".to_owned())
}

fn reduced_motion() -> bool {
    std::env::var_os("RE_CODE_REDUCED_MOTION").is_some_and(|value| !value.is_empty())
}

/// Runs the interface, starting at the welcome screen.
pub async fn run_interactive(link: Link, theme: crate::background::ThemeChoice) -> Result<()> {
    let (_guard, mut terminal) = TerminalGuard::new()?;
    // Asked once the terminal is in raw mode and before anything reads keys, so the reply isn't lost.
    let (selection, locked) = theme.resolve();
    crate::theme::activate(&selection);
    let now = Instant::now();
    let mut app = App::new(now, &project_name(), "", "", false);
    app.reduced_motion = reduced_motion();
    app.select_mode = !mouse_wanted();
    // Only terminals speaking the kitty keyboard protocol can tell Ctrl+Enter from Enter.
    app.keyboard_enhanced = crossterm::terminal::supports_keyboard_enhancement().unwrap_or(false);
    app.theme = selection;
    app.theme_locked = locked;
    app.open_welcome();
    event_loop(&mut terminal, &mut app, link).await
}

/// Runs the interface with a recorded session.
pub async fn run_demo() -> Result<()> {
    if !stdout().is_terminal() {
        bail!("the demo needs a terminal, and stdout isn't one");
    }
    let (_guard, mut terminal) = TerminalGuard::new()?;
    let now = Instant::now();
    let mut app = App::new(now, &project_name(), "demo-model", "recorded", true);
    app.reduced_motion = reduced_motion();
    app.transcript.push(Entry::Notice(
        "This is a recorded demo. No model is connected and nothing on your disk changes. \
         Resize the window at any time. F1 lists the keys."
            .to_owned(),
    ));
    let (event_sender, events) = mpsc::unbounded_channel();
    let (commands, command_receiver) = mpsc::unbounded_channel();
    tokio::spawn(play_recording(event_sender, command_receiver));
    event_loop(&mut terminal, &mut app, Link { events, commands }).await
}

/// Drives the interface from the recording, in the same way a real session does.
async fn play_recording(events: UnboundedSender<UiEvent>, mut commands: UnboundedReceiver<UiCommand>) {
    let mut player = Player::new();
    player.start("The retry loop in auth.rs never backs off. Fix it and run the tests.", Instant::now());
    loop {
        let sleep = async {
            match player.next_due() {
                Some(at) => tokio::time::sleep_until(at.into()).await,
                None => std::future::pending().await,
            }
        };
        let produced = tokio::select! {
            command = commands.recv() => {
                let now = Instant::now();
                match command {
                    None => return,
                    Some(UiCommand::Send { text, .. }) => {
                        player.start(&text, now);
                        Vec::new()
                    }
                    Some(UiCommand::Interrupt) => player.interrupt(),
                    Some(UiCommand::ResolveApproval(id, decision)) => player.resolve(id, decision, now),
                    Some(
                        UiCommand::Compact { .. }
                        | UiCommand::NewConversation
                        | UiCommand::ContinueLast
                        | UiCommand::AddFolder(_)
                        | UiCommand::ListSessions
                        | UiCommand::ContinueSession(_)
                        | UiCommand::SetContextWindow { .. }
                        | UiCommand::ShowLastRequest
                        | UiCommand::ListCompactions
                        | UiCommand::ShowCompaction(_)
                        | UiCommand::RunShell { .. }
                        | UiCommand::ListTasks
                        | UiCommand::KillTask(_)
                        | UiCommand::ShowTaskOutput(_)
                        | UiCommand::Rewind { .. }
                        | UiCommand::LoadHistory
                        | UiCommand::Export { .. }
                        | UiCommand::SetPermissionMode(_)
                        | UiCommand::ListPermissions
                        | UiCommand::RevokePermission(_)
                        | UiCommand::ShowPlan
                        | UiCommand::Init
                        | UiCommand::WriteInit
                        | UiCommand::ListInstructions
                        | UiCommand::WriteNote { .. }
                        | UiCommand::ForgetMemory { .. }
                        | UiCommand::DeleteSession(_)
                        | UiCommand::UntrustProject,
                    ) => {
                        let _ = events.send(UiEvent::Notice("That isn't available in the demo recording.".to_owned()));
                        Vec::new()
                    }
                    // The demo can be re-themed; there's just nowhere to save it.
                    Some(UiCommand::SetTheme { .. } | UiCommand::SetSetting { .. }) => Vec::new(),
                    Some(UiCommand::Undo) => {
                        let _ = events.send(UiEvent::Notice("Undo isn't available in the demo recording.".to_owned()));
                        Vec::new()
                    }
                    Some(UiCommand::Refresh | UiCommand::ChooseModel(_) | UiCommand::Extension(_) | UiCommand::StartRuntime(_) | UiCommand::TrustProject) => Vec::new(),
                }
            }
            () = sleep => player.tick(Instant::now()),
        };
        for kind in produced {
            if events.send(UiEvent::Session(kind)).is_err() {
                return;
            }
        }
    }
}

/// A bell and/or a desktop notification for things worth looking up for, as the `notifications`
/// setting allows: `off`, `bell`, `desktop`, or `both` (the default).
fn notify(app: &App, title: &str, body: &str) {
    use std::io::Write;
    let mode = app.settings.get("notifications").map_or("both", String::as_str);
    if matches!(mode, "bell" | "both") {
        let mut out = stdout();
        let _ = out.write_all(b"\x07");
        let _ = out.flush();
    }
    if matches!(mode, "desktop" | "both") {
        let _ = std::process::Command::new("notify-send")
            .args(["--app-name", "re-code", title, body])
            .stdin(std::process::Stdio::null())
            .stdout(std::process::Stdio::null())
            .stderr(std::process::Stdio::null())
            .spawn();
    }
}

fn apply_event(app: &mut App, event: UiEvent) {
    match event {
        // A turn that needs the user, or finishes while they're away, says so out loud.
        UiEvent::Session(EventKind::StateChanged { state: AgentState::AwaitingApproval }) => {
            app.apply(EventKind::StateChanged { state: AgentState::AwaitingApproval });
            if let Some((tool, _)) = app.pending_approval() {
                let what = format!("{} {}", tool.name, tool.target);
                notify(app, "re-code needs your approval", &what);
            }
        }
        UiEvent::Session(EventKind::TurnFinished { turn, outcome }) => {
            let long = app.turn_elapsed().is_some_and(|elapsed| elapsed.as_secs() >= 20);
            let was_unfocused = !app.focused;
            app.apply(EventKind::TurnFinished { turn, outcome });
            if long || was_unfocused {
                notify(app, "re-code finished", app.title.as_str());
            }
        }
        UiEvent::Welcome(info) => app.set_welcome_info(info),
        UiEvent::Session(kind) => app.apply(kind),
        UiEvent::Notice(text) => app.transcript.push(Entry::Notice(crate::sanitize::sanitize(&text))),
        UiEvent::Pager { title, text } => app.open_pager(title, text),
        UiEvent::Compactions(reports) => app.compactions_listed(reports),
        UiEvent::Permissions { mode, session, project } => app.permissions_listed(mode, session, project),
        UiEvent::Tasks(tasks) => app.tasks_listed(tasks),
        UiEvent::InitPreview { path, text } => app.init_preview(path, text),
        UiEvent::InstructionFiles { files, user_path, project_path } => {
            app.instructions_listed(files, user_path, project_path);
        }
        UiEvent::Extensions(snapshot) => app.extensions = Some(snapshot),
        UiEvent::HomeMessage { text, is_error } => {
            app.home_message = Some((crate::sanitize::sanitize(&text), is_error))
        }
        UiEvent::Resumed { model } => app.resumed(model),
        UiEvent::ProjectFiles(files) => {
            app.project_file_set = files.iter().cloned().collect();
            app.project_files = files;
        }
        UiEvent::Sessions(sessions) => app.sessions_listed(sessions),
        UiEvent::Hardware(meters) => app.set_hardware(meters),
        UiEvent::Theme { name, variant, accent, solid } => app.saved_theme(crate::theme::ThemeSelection {
            name,
            variant: crate::theme::Variant::parse(&variant).unwrap_or_default(),
            accent: crate::theme::parse_hex(&accent),
            solid,
        }),
        UiEvent::ContextWindow { window, setting, kv_bytes_per_token } => {
            app.set_context_window(window, setting, kv_bytes_per_token);
        }
        UiEvent::Settings(settings) => {
            if let Some(width) = settings.get("panel_width").and_then(|value| value.parse::<u16>().ok()) {
                app.panel_width = width.clamp(crate::view::PANEL_MIN_WIDTH, crate::view::PANEL_MAX_WIDTH);
            }
            app.settings = settings;
        }
        UiEvent::UserThemes { themes, problems } => {
            crate::theme::install_user_themes(themes);
            // The theme in use may have just changed on disk, or appeared: draw it as it is now.
            if crate::theme::activate(&app.theme) {
                app.transcript.invalidate();
            }
            for problem in problems {
                app.toast(format!("Theme file {problem}"), ToastLevel::Warning);
            }
        }
        UiEvent::History(history) => app.history = history,
        UiEvent::HistoryAll(all) => app.history_all_loaded(all),
        UiEvent::Runtimes(info) => app.runtimes = Some(info),
        UiEvent::RuntimeStart(event) => app.runtime_start_event(event),
        UiEvent::ProjectTrust(trust) => app.project_trust = Some(trust),
        UiEvent::McpSearch { query, hits, error } => app.mcp_search_result(&query, hits, error),
        UiEvent::Install(outcome) => app.install_outcome(outcome),
        UiEvent::PluginInstall(outcome) => app.plugin_outcome(outcome),
        UiEvent::InstallBatch(items) => app.install_batch(items.into_iter().map(|(tap, o)| (tap, Ok(o))).collect()),
        UiEvent::PluginInstallBatch(items) => {
            app.install_batch(items.into_iter().map(|(tap, o)| (tap, Err(o))).collect())
        }
        UiEvent::PluginListings { title, listings, error } => app.plugin_listings(&title, listings, error),
        UiEvent::SkillSearch { query, hits, note, error } => app.skill_search_result(&query, hits, note, error),
        UiEvent::ToolStatus { active, summary } => {
            app.tool_status = Some((active, crate::sanitize::sanitize(&summary)))
        }
    }
}

async fn event_loop(terminal: &mut DefaultTerminal, app: &mut App, mut link: Link) -> Result<()> {
    let colors = ColorMode::detect();
    let mut terminal_events = EventStream::new();
    let mut dirty = true;
    let mut last_draw = Instant::now().checked_sub(FRAME).unwrap_or_else(Instant::now);
    let mut next_tick = Instant::now();

    loop {
        let now = Instant::now();
        app.now = now;
        let ticking = app.needs_ticks();
        if ticking && now >= next_tick {
            // Dragging a selection past the edge scrolls smoothly, so it ticks fast.
            let fast = matches!(app.screen, crate::app::Screen::Welcome(_)) || app.edge_scrolling();
            next_tick = now + if fast { FAST_TICK } else { TICK };
            app.edge_scroll();
            dirty = true;
        }
        if dirty && now.duration_since(last_draw) >= FRAME {
            // The theme lives on the app (so tests stay independent); drawing reads it from here.
            crate::theme::activate(&app.theme);
            // A synchronized update lets the terminal show the frame all at once: no tearing while streaming.
            let _ = execute!(stdout(), BeginSynchronizedUpdate);
            let drawn = terminal.draw(|frame| view::draw(frame, app, colors));
            let _ = execute!(stdout(), EndSynchronizedUpdate);
            drawn?;
            last_draw = Instant::now();
            dirty = false;
        }

        // Sleeps until there is something to do, so an idle session uses no CPU at all.
        let mut wake: Option<Instant> = None;
        if dirty {
            wake = Some(last_draw + FRAME);
        }
        if ticking {
            wake = Some(wake.map_or(next_tick, |at| at.min(next_tick)));
        }
        let sleep = async {
            match wake {
                Some(at) => tokio::time::sleep_until(at.into()).await,
                None => std::future::pending().await,
            }
        };
        let mut edit = None;
        let mut dump = None;
        let mut paste_image = false;
        tokio::select! {
            event = terminal_events.next() => {
                let Some(event) = event else { return Ok(()) };
                dirty = true;
                let mut clicked = Vec::new();
                app.now = Instant::now();
                let actions = match event? {
                    Event::Key(key) => app.handle_key(key),
                    Event::Paste(text) => {
                        app.paste(&text);
                        Vec::new()
                    }
                    Event::Mouse(mouse) => {
                        match mouse.kind {
                            MouseEventKind::ScrollUp => app.wheel(mouse.column, mouse.row, -WHEEL_LINES),
                            MouseEventKind::ScrollDown => app.wheel(mouse.column, mouse.row, WHEEL_LINES),
                            MouseEventKind::Down(MouseButton::Left) => {
                                let ctrl = mouse.modifiers.contains(crossterm::event::KeyModifiers::CONTROL);
                                clicked = app.mouse_down(mouse.column, mouse.row, ctrl);
                            }
                            MouseEventKind::Drag(MouseButton::Left) => app.mouse_drag(mouse.column, mouse.row),
                            MouseEventKind::Up(MouseButton::Left) => clicked = app.mouse_up(mouse.column, mouse.row),
                            // Middle-click pastes the primary selection, as it does elsewhere on Linux.
                            MouseEventKind::Down(MouseButton::Middle) => {
                                if let Some(text) = read_primary_selection() {
                                    app.paste(&text);
                                }
                            }
                            _ => {}
                        }
                        std::mem::take(&mut clicked)
                    }
                    // Layout comes entirely from the size at draw time, so a resize is just a redraw. A burst of them
                    // while dragging collapses into one frame.
                    Event::FocusGained => {
                        app.focused = true;
                        Vec::new()
                    }
                    Event::FocusLost => {
                        app.focused = false;
                        Vec::new()
                    }
                    Event::Resize(..) => Vec::new(),
                };
                for action in actions {
                    match action {
                        Action::EditFile(path) => edit = Some(path),
                        Action::DumpScrollback(text) => dump = Some(text),
                        Action::OpenUrl(url) => {
                            if !open_url(&url) {
                                app.toast(format!("Couldn't open a browser. Go to {url}"), ToastLevel::Warning);
                            }
                        }
                        Action::PasteImage => paste_image = true,
                        Action::Copy(text) => {
                            let how = copy_to_clipboard(&text);
                            app.amend_last_toast(&format!(" · {how}"));
                        }
                        other => {
                            if perform(other, &link.commands)? {
                                return Ok(());
                            }
                        }
                    }
                }
            }
            event = link.events.recv() => {
                if let Some(event) = event {
                    apply_event(app, event);
                    // Everything already waiting is applied together, so a fast stream costs one frame.
                    while let Ok(event) = link.events.try_recv() {
                        apply_event(app, event);
                    }
                    // An event can call for something of the driver (a staged skill nobody is waiting for is discarded).
                    for action in std::mem::take(&mut app.pending_actions) {
                        if perform(action, &link.commands)? {
                            return Ok(());
                        }
                    }
                    dirty = true;
                } else {
                    bail!("the session stopped unexpectedly");
                }
            }
            () = sleep => {}
        }

        if let Some(text) = dump.take() {
            drop(terminal_events);
            let result = dump_to_scrollback(terminal, &text, !app.select_mode);
            terminal_events = EventStream::new();
            if let Err(error) = result {
                app.home_message = Some((error.to_string(), true));
            }
            dirty = true;
        }
        if let Some(path) = edit.take() {
            // The event reader would compete with the editor for the keyboard, so it has to stop first.
            drop(terminal_events);
            let result = run_editor(terminal, &path, !app.select_mode);
            terminal_events = EventStream::new();
            match result {
                Ok(()) => {
                    let _ = link.commands.send(UiCommand::Refresh);
                }
                Err(error) => app.home_message = Some((error.to_string(), true)),
            }
            dirty = true;
        }
        if paste_image {
            // Ctrl+V pastes whatever is on the clipboard: an image is attached, text is typed in.
            match read_clipboard_image() {
                Some(image) => app.attach_image(Some(image)),
                None => match read_clipboard_text() {
                    Some(text) => app.paste(&text),
                    None => app.attach_image(None),
                },
            }
            dirty = true;
        }
    }
}

/// Reads a PNG image from the clipboard via `wl-paste` (Wayland) or `xclip` (X11). Returns None when
/// there is no image or the tools aren't present.
fn read_clipboard_image() -> Option<recode_core::message::ImageData> {
    use std::process::Command;
    let attempts =
        [("wl-paste", vec!["-t", "image/png"]), ("xclip", vec!["-selection", "clipboard", "-t", "image/png", "-o"])];
    for (program, args) in attempts {
        if let Ok(output) = Command::new(program).args(&args).output()
            && output.status.success()
            && output.stdout.starts_with(b"\x89PNG")
        {
            return Some(recode_core::message::ImageData {
                media_type: "image/png".to_owned(),
                data_base64: recode_core::base64::encode(&output.stdout),
                label: "pasted".to_owned(),
            });
        }
    }
    None
}

/// Reads text from the clipboard via `wl-paste` (Wayland) or `xclip` (X11).
fn read_clipboard_text() -> Option<String> {
    use std::process::Command;
    let attempts = [("wl-paste", vec!["--no-newline"]), ("xclip", vec!["-selection", "clipboard", "-o"])];
    for (program, args) in attempts {
        if let Ok(output) = Command::new(program).args(&args).output()
            && output.status.success()
            && !output.stdout.is_empty()
            && let Ok(text) = String::from_utf8(output.stdout)
        {
            return Some(text);
        }
    }
    None
}

/// Opens a web page in the user's browser, detached. Returns whether a browser could be started.
fn open_url(url: &str) -> bool {
    use std::process::{Command, Stdio};
    Command::new("xdg-open").arg(url).stdin(Stdio::null()).stdout(Stdio::null()).stderr(Stdio::null()).spawn().is_ok()
}

/// Reads the primary selection (the last text selected anywhere), for middle-click paste.
fn read_primary_selection() -> Option<String> {
    use std::process::Command;
    let attempts = [("wl-paste", vec!["--primary", "--no-newline"]), ("xclip", vec!["-selection", "primary", "-o"])];
    attempts.into_iter().find_map(|(program, args)| {
        let output = Command::new(program).args(&args).output().ok()?;
        (output.status.success() && !output.stdout.is_empty()).then(|| String::from_utf8(output.stdout).ok()).flatten()
    })
}

/// Copies text to the clipboard: through `wl-copy` (Wayland) or `xclip` (X11) when present, and also
/// with OSC 52, which terminals honour even over SSH.
/// Copies to the clipboard and to the primary selection (so a middle click pastes it elsewhere, as
/// text selected in any terminal would), through whatever this session has: `wl-copy` on Wayland,
/// `xclip` or `xsel` on X11, tmux's buffer inside tmux, and always OSC 52 for terminals that take it
/// (the only path over SSH). Returns the path used, in words.
fn copy_to_clipboard(text: &str) -> String {
    use std::io::Write;
    use std::process::{Command, Stdio};
    let feed = |program: &str, args: &[&str]| -> bool {
        let Ok(mut child) =
            Command::new(program).args(args).stdin(Stdio::piped()).stdout(Stdio::null()).stderr(Stdio::null()).spawn()
        else {
            return false;
        };
        if let Some(mut stdin) = child.stdin.take() {
            let _ = stdin.write_all(text.as_bytes());
        }
        // These hand the text to a background process and exit; reap them without blocking the interface.
        std::thread::spawn(move || {
            let _ = child.wait();
        });
        true
    };
    let mut used: Vec<&str> = Vec::new();
    if std::env::var_os("WAYLAND_DISPLAY").is_some() && feed("wl-copy", &[]) {
        feed("wl-copy", &["--primary"]);
        used.push("wl-copy + primary");
    } else if std::env::var_os("DISPLAY").is_some() {
        if feed("xclip", &["-selection", "clipboard"]) {
            feed("xclip", &["-selection", "primary"]);
            used.push("xclip + primary");
        } else if feed("xsel", &["--clipboard", "--input"]) {
            feed("xsel", &["--primary", "--input"]);
            used.push("xsel + primary");
        }
    }
    if std::env::var_os("TMUX").is_some() && feed("tmux", &["load-buffer", "-w", "-"]) {
        used.push("tmux buffer");
    }
    let encoded = recode_core::base64::encode(text.as_bytes());
    // OSC 52: ESC ] 52 ; c ; <base64> BEL, for the clipboard (c) and the primary selection (p).
    let mut out = stdout();
    let _ = write!(out, "\x1b]52;c;{encoded}\x07\x1b]52;p;{encoded}\x07");
    let _ = out.flush();
    used.push("OSC 52");
    used.join(", ")
}

/// Leaves the alternate screen, prints the conversation into the terminal's own scrollback, and waits
/// for a key before coming back. What was printed stays there, so the terminal's search and selection
/// (and tmux copy mode) work on it afterwards.
fn dump_to_scrollback(terminal: &mut DefaultTerminal, text: &str, mouse: bool) -> Result<()> {
    use std::io::Write;
    disable_terminal_modes();
    ratatui::restore();
    {
        let mut out = stdout();
        for line in text.lines() {
            let _ = write!(out, "{line}\r\n");
        }
        let _ = write!(
            out,
            "\r\n— re-code: the conversation above now lives in your terminal's scrollback; press any key to return —\r\n"
        );
        let _ = out.flush();
    }
    // A single key, without echo: raw mode just for the wait.
    crossterm::terminal::enable_raw_mode()?;
    let waited = tokio::task::block_in_place(|| {
        loop {
            match crossterm::event::read() {
                Ok(Event::Key(key)) if key.kind != crossterm::event::KeyEventKind::Release => break Ok(()),
                Ok(_) => continue,
                Err(error) => break Err(error),
            }
        }
    });
    crossterm::terminal::disable_raw_mode()?;
    waited?;
    *terminal = ratatui::try_init()?;
    enable_terminal_modes(mouse)?;
    terminal.clear()?;
    Ok(())
}

/// Hands the terminal to the user's editor, and takes it back.
fn run_editor(terminal: &mut DefaultTerminal, path: &str, mouse: bool) -> Result<()> {
    let configured =
        ["VISUAL", "EDITOR"].iter().find_map(|name| std::env::var(name).ok().filter(|value| !value.trim().is_empty()));
    let line = configured.unwrap_or_else(|| "nano".to_owned());
    let mut words = line.split_whitespace();
    let program = words.next().unwrap_or("nano").to_owned();
    let arguments: Vec<&str> = words.collect();

    disable_terminal_modes();
    ratatui::restore();
    let status =
        tokio::task::block_in_place(|| std::process::Command::new(&program).args(&arguments).arg(path).status());
    *terminal = ratatui::try_init()?;
    enable_terminal_modes(mouse)?;
    terminal.clear()?;
    match status {
        Ok(status) if status.success() => Ok(()),
        Ok(status) => bail!("{program} exited with {status}"),
        Err(error) => bail!("Couldn't open {program}: {error}. Set $EDITOR to choose an editor."),
    }
}

/// Carries out an action. Returns true when the interface should exit.
fn perform(action: Action, commands: &UnboundedSender<UiCommand>) -> Result<bool> {
    let command = match action {
        Action::Quit => return Ok(true),
        Action::SetMouseCapture(capture) => {
            if capture {
                execute!(stdout(), EnableMouseCapture)?;
            } else {
                execute!(stdout(), DisableMouseCapture)?;
            }
            return Ok(false);
        }
        Action::Interrupt => UiCommand::Interrupt,
        Action::ResolveApproval(approval, decision) => UiCommand::ResolveApproval(approval, decision),
        Action::Send { text, images } => UiCommand::Send { text, images },
        Action::ChooseModel(name) => UiCommand::ChooseModel(name),
        Action::Refresh => UiCommand::Refresh,
        Action::StartRuntime(id) => UiCommand::StartRuntime(id),
        Action::TrustProject => UiCommand::TrustProject,
        Action::Undo => UiCommand::Undo,
        Action::Compact { focus } => UiCommand::Compact { focus },
        Action::ShowLastRequest => UiCommand::ShowLastRequest,
        Action::ListCompactions => UiCommand::ListCompactions,
        Action::RunShell { command, network, background } => UiCommand::RunShell { command, network, background },
        Action::ListTasks => UiCommand::ListTasks,
        Action::KillTask(id) => UiCommand::KillTask(id),
        Action::ShowTaskOutput(id) => UiCommand::ShowTaskOutput(id),
        Action::Rewind { to_turn, files, summarize } => UiCommand::Rewind { to_turn, files, summarize },
        Action::LoadHistory => UiCommand::LoadHistory,
        Action::Export { path, text } => UiCommand::Export { path, text },
        Action::SetPermissionMode(mode) => UiCommand::SetPermissionMode(mode),
        Action::ListPermissions => UiCommand::ListPermissions,
        Action::RevokePermission(json) => UiCommand::RevokePermission(json),
        Action::ShowPlan => UiCommand::ShowPlan,
        Action::Init => UiCommand::Init,
        Action::WriteInit => UiCommand::WriteInit,
        Action::ListInstructions => UiCommand::ListInstructions,
        Action::WriteNote { scope, text } => UiCommand::WriteNote { scope, text },
        Action::ForgetMemory { scope, name } => UiCommand::ForgetMemory { scope, name },
        Action::DeleteSession(id) => UiCommand::DeleteSession(id),
        Action::UntrustProject => UiCommand::UntrustProject,
        Action::ShowCompaction(seq) => UiCommand::ShowCompaction(seq),
        Action::NewConversation => UiCommand::NewConversation,
        Action::ContinueLast => UiCommand::ContinueLast,
        Action::AddFolder(path) => UiCommand::AddFolder(path),
        Action::ListSessions => UiCommand::ListSessions,
        Action::SetSetting { key, value } => UiCommand::SetSetting { key, value },
        Action::SetContextWindow { model, setting } => UiCommand::SetContextWindow { model, setting },
        Action::SetTheme(selection) => UiCommand::SetTheme {
            name: selection.name,
            variant: selection.variant.label().to_owned(),
            accent: selection.accent.and_then(crate::theme::hex).unwrap_or_default(),
            solid: selection.solid,
        },
        Action::ContinueSession(id) => UiCommand::ContinueSession(id),
        Action::Extension(command) => UiCommand::Extension(command),
        // Handled by the event loop, which owns the terminal.
        Action::EditFile(_) | Action::Copy(_) | Action::PasteImage | Action::OpenUrl(_) | Action::DumpScrollback(_) => {
            return Ok(false);
        }
    };
    // If the driver has gone, the next read from its channel reports that, with a proper message.
    let _ = commands.send(command);
    Ok(false)
}
