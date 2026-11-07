//! Enforces the requirement that the interface fits and works at whatever size the terminal is.

use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
use ratatui::Terminal;
use ratatui::backend::TestBackend;
use recode_core::extensions::{
    ExtensionCommand, ExtensionsSnapshot, McpProbe, McpToolInfo, ProviderProbe, Skill, SkillSource, parse_mcp_form,
    parse_tool_form,
};
use recode_core::protocol::{AgentState, ApprovalDecision, EventKind};
use recode_core::providers::{ProviderKind, parse_provider_form};
use recode_tui::app::{Action, App, Overlay, Screen};
use recode_tui::demo::Player;
use recode_tui::form::{ConfirmState, FormState};
use recode_tui::theme::ColorMode;
use recode_tui::view;
use recode_tui::welcome::{GpuFit, HomeTab, Meter, ProviderState, WelcomeInfo, WelcomeModel, sort_models};
use std::time::Instant;

/// Plays the demo up to the approval prompt.
fn at_approval() -> App {
    let now = Instant::now();
    let mut app = App::new(now, "api-server", "demo-model", "recorded", true);
    let mut player = Player::new();
    player.start("The retry loop in auth.rs never backs off. Fix it and run the tests.", now);
    let mut clock = now;
    while let Some(due) = player.next_due() {
        clock = clock.max(due);
        for kind in player.tick(clock) {
            app.apply(kind);
        }
    }
    app.now = clock;
    app
}

/// Plays the whole demo to its end.
fn finished() -> App {
    let now = Instant::now();
    let mut app = App::new(now, "api-server", "demo-model", "recorded", true);
    let mut player = Player::new();
    player.start("The retry loop in auth.rs never backs off. Fix it and run the tests.", now);
    let mut clock = now;
    loop {
        while let Some(due) = player.next_due() {
            clock = clock.max(due);
            for kind in player.tick(clock) {
                app.apply(kind);
            }
        }
        match app.pending_approval().map(|(_, id)| id) {
            Some(id) => {
                for kind in player.resolve(id, ApprovalDecision::AllowOnce, clock) {
                    app.apply(kind);
                }
            }
            None => break,
        }
    }
    app.now = clock;
    app
}

fn draw(app: &mut App, width: u16, height: u16) -> Terminal<TestBackend> {
    let mut terminal = Terminal::new(TestBackend::new(width, height)).unwrap();
    terminal.draw(|frame| view::draw(frame, app, ColorMode::TrueColor)).unwrap();
    terminal
}

fn screen(terminal: &Terminal<TestBackend>) -> String {
    let buffer = terminal.backend().buffer();
    let mut out = String::new();
    for y in 0..buffer.area.height {
        let mut row = String::new();
        let mut x = 0;
        while x < buffer.area.width {
            let symbol = buffer[(x, y)].symbol();
            row.push_str(symbol);
            // A wide character occupies the next cell too.
            x += u16::try_from(unicode_width::UnicodeWidthStr::width(symbol).max(1)).unwrap_or(1);
        }
        out.push_str(row.trim_end());
        out.push('\n');
    }
    out
}

fn sample_model(name: &str, size: f64, fit: GpuFit, tools: bool) -> WelcomeModel {
    WelcomeModel {
        name: name.to_owned(),
        size_gib: Some(size),
        family: Some("qwen".to_owned()),
        parameters: Some("20B".to_owned()),
        quantization: Some("Q4_K_M".to_owned()),
        context_limit: Some(131_072),
        tools,
        vision: false,
        thinking: tools,
        fit,
        loaded: false,
        provider_label: None,
        free: false,
        needs_key: false,
        price: None,
        kv_bytes_per_token: None,
    }
}

fn sample_info() -> WelcomeInfo {
    WelcomeInfo {
        provider_name: "Ollama".to_owned(),
        provider: ProviderState::Ready { detail: "Ollama 0.40.14".to_owned() },
        models: sort_models(vec![
            sample_model("gpt-oss:20b", 12.8, GpuFit::FitsGpu, true),
            sample_model("qwen3.6:27b-coding", 16.5, GpuFit::PartlyCpu { over_gib: 2.5 }, true),
            sample_model("llama3.2:3b", 1.9, GpuFit::FitsGpu, true),
            sample_model("deephat:7b-q4", 4.4, GpuFit::FitsGpu, false),
            sample_model("a-very-long-model-name-that-will-not-fit-anywhere-narrow:latest", 8.0, GpuFit::Unknown, true),
        ]),
        hardware: vec![
            Meter {
                label: "GPU".to_owned(),
                used_gib: Some(2.0),
                total_gib: Some(16.0),
                detail: "AMD Radeon RX 6900 XT".to_owned(),
            },
            Meter {
                label: "Memory".to_owned(),
                used_gib: Some(19.0),
                total_gib: Some(94.0),
                detail: "Ryzen 9 5950X".to_owned(),
            },
        ],
        free_vram_gib: Some(14.0),
        last_model: None,
    }
}

fn sample_extensions() -> ExtensionsSnapshot {
    let connected = parse_mcp_form("rustrover", "http://127.0.0.1:64522/stream", "", true).unwrap();
    let failed = parse_mcp_form("broken", "no-such-program --flag", "SECRET=hunter2, TOKEN=${TOKEN}", true).unwrap();
    let untested =
        parse_mcp_form("a-server-with-a-very-long-name-that-cannot-fit-narrow-rows", "run", "", true).unwrap();
    let off = parse_mcp_form("off", "run", "", false).unwrap();
    let probes = [
        (
            "rustrover".to_owned(),
            McpProbe::Connected {
                server_name: Some("RustRover MCP Server".to_owned()),
                server_version: Some("2026.3".to_owned()),
                tools: (0..40)
                    .map(|index| McpToolInfo {
                        name: format!("tool_{index}"),
                        description: String::new(),
                        input_schema: serde_json::Value::Null,
                    })
                    .collect(),
                estimated_schema_tokens: 18_240,
            },
        ),
        ("broken".to_owned(), McpProbe::Failed { error: "Couldn't start no-such-program: not found".to_owned() }),
    ];
    let skill = |name: &str, source, enabled, problem: Option<&str>| Skill {
        name: name.to_owned(),
        description: "Use this when something needs doing, described at reasonable length.".to_owned(),
        path: format!("/home/someone/.config/re-code/skills/{name}/SKILL.md"),
        source,
        enabled,
        estimated_tokens: 420,
        problem: problem.map(str::to_owned),
        ..Skill::default()
    };
    ExtensionsSnapshot {
        servers: vec![connected, failed, untested, off],
        probes: probes.into_iter().collect(),
        probing: Vec::new(),
        tools: vec![
            parse_tool_form("search-docs", "Search the project documentation", "rg {query} docs", "query", true)
                .unwrap(),
            parse_tool_form("off-tool", "Disabled", "true", "", false).unwrap(),
        ],
        skills: vec![
            skill("review", SkillSource::User, true, None),
            skill("from-repo", SkillSource::Project, false, None),
            skill("borrowed", SkillSource::ClaudeCode, true, None),
            skill("damaged", SkillSource::User, false, Some("Has no description in its front matter.")),
        ],
        providers: vec![
            parse_provider_form("anthropic", ProviderKind::Anthropic, "", true).unwrap(),
            parse_provider_form("openrouter", ProviderKind::OpenaiCompatible, "https://openrouter.ai/api/v1", true)
                .unwrap(),
            parse_provider_form("off-provider", ProviderKind::Openai, "", false).unwrap(),
        ],
        provider_probes: [
            (
                "openrouter".to_owned(),
                ProviderProbe::Reachable {
                    model_count: 320,
                    free_model_count: 60,
                    note: Some("Free models: 3 of 50 requests used today (UTC).".to_owned()),
                },
            ),
            ("anthropic".to_owned(), ProviderProbe::Failed { error: "the server rejected the API key".to_owned() }),
        ]
        .into_iter()
        .collect(),
        providers_with_keys: ["anthropic".to_owned()].into_iter().collect(),
        config_path: "/home/someone/.config/re-code/extensions.toml".to_owned(),
        load_error: None,
    }
}

fn home_on(tab: HomeTab, extensions: ExtensionsSnapshot) -> App {
    let mut app = App::new(Instant::now(), "api-server", "", "", false);
    app.reduced_motion = true;
    app.open_welcome();
    app.set_welcome_info(sample_info());
    app.extensions = Some(extensions);
    if let Screen::Welcome(welcome) = &mut app.screen {
        welcome.tab = tab;
    }
    app
}

fn welcome_scenes() -> Vec<(&'static str, Option<WelcomeInfo>)> {
    let stopped = WelcomeInfo {
        provider_name: "Ollama".to_owned(),
        provider: ProviderState::Unavailable {
            headline: "Ollama is installed but stopped".to_owned(),
            details: vec![
                "To start it: systemctl --user start ollama.service".to_owned(),
                "This also starts the other containers in local-ai.pod.".to_owned(),
            ],
        },
        models: Vec::new(),
        hardware: vec![Meter {
            label: "GPU".to_owned(),
            used_gib: None,
            total_gib: None,
            detail: "None found".to_owned(),
        }],
        free_vram_gib: None,
        last_model: None,
    };
    // Ollama down, but cloud providers configured: their models must still be offered.
    let mut stopped_with_cloud = stopped.clone();
    for (name, free) in [("openrouter/free-coder", true), ("gpt-5-mini", false)] {
        let mut model = sample_model(name, 0.0, GpuFit::Cloud, true);
        model.size_gib = None;
        model.provider_label = Some(if free { "openrouter" } else { "openai" }.to_owned());
        model.free = free;
        stopped_with_cloud.models.push(model);
    }
    let mut many = sample_info();
    for index in 0..60 {
        many.models.push(sample_model(&format!("model-{index}"), 3.0, GpuFit::FitsGpu, true));
    }
    vec![
        ("welcome, scanning", None),
        ("welcome, ready", Some(sample_info())),
        ("welcome, stopped", Some(stopped)),
        ("welcome, stopped with cloud", Some(stopped_with_cloud)),
        ("welcome, many", Some(many)),
    ]
}

#[test]
fn the_completion_popup_shows_the_highlighted_suggestion_at_every_size() {
    for (name, app) in
        every_scene().into_iter().filter(|(name, _)| *name == "slash commands" || *name == "file mentions")
    {
        let mut app = app;
        for (width, height) in sizes() {
            let terminal = draw(&mut app, width, height);
            let text = screen(&terminal);
            // Wherever the conversation itself is drawn, the highlighted suggestion is on screen.
            if width >= 20 && height >= 8 {
                let wanted = if name == "slash commands" { "/help" } else { "module_" };
                assert!(text.contains(wanted), "{name} at {width}x{height}:\n{text}");
            }
        }
    }
}

#[test]
fn scrolling_up_shows_how_much_is_below() {
    for (width, height) in [(60, 12), (100, 30), (160, 45)] {
        let mut app = finished();
        draw(&mut app, width, height);
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::Home,
            crossterm::event::KeyModifiers::NONE,
        ));
        let text = screen(&draw(&mut app, width, height));
        assert!(text.contains('↓') && text.contains("more lines"), "{width}x{height}:\n{text}");
        app.handle_key(crossterm::event::KeyEvent::new(
            crossterm::event::KeyCode::End,
            crossterm::event::KeyModifiers::NONE,
        ));
        assert!(!screen(&draw(&mut app, width, height)).contains("more lines"), "gone when following again");
    }
}

#[test]
fn dragging_selects_what_is_on_screen_and_copies_it() {
    let mut app = finished();
    let terminal = draw(&mut app, 100, 30);
    let text = screen(&terminal);
    // Find a row of the conversation that has words on it, and drag across the whole of it.
    let (x, y, width, height) = app.transcript_rect;
    let row = (y..y + height)
        .find(|row| text.lines().nth(usize::from(*row)).is_some_and(|line| line.contains("retry")))
        .expect("a line mentioning the retry");
    assert!(app.mouse_down(x, row, false).is_empty());
    app.mouse_drag(x + width - 1, row + 1);
    let shown = screen(&draw(&mut app, 100, 30));
    let actions = app.mouse_up(x + width - 1, row + 1);
    let [recode_tui::app::Action::Copy(copied)] = &actions[..] else { panic!("expected a copy: {actions:?}") };
    assert!(copied.contains("retry"), "{copied}");
    assert!(copied.lines().count() == 2, "two rows were selected: {copied:?}");
    assert!(shown.contains("copies the selection"), "the status line says how to copy again");
    assert!(app.last_toast().is_some_and(|text| text.starts_with("Copied")));
    // A click without a drag copies nothing.
    app.mouse_down(x, row, false);
    assert!(!matches!(app.mouse_up(x, row)[..], [recode_tui::app::Action::Copy(_)]));
}

#[test]
fn dragging_past_the_bottom_scrolls_and_copies_more_than_a_screen() {
    let mut app = finished();
    draw(&mut app, 100, 16);
    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Home,
        crossterm::event::KeyModifiers::NONE,
    ));
    draw(&mut app, 100, 16);
    let (x, y, width, height) = app.transcript_rect;
    let first_screen = screen(&draw(&mut app, 100, 16));
    app.mouse_down(x, y, false);
    // Held below the conversation: each tick scrolls further and extends the selection.
    app.mouse_drag(x + width - 1, y + height + 2);
    assert!(app.edge_scrolling(), "dragging past the edge keeps scrolling");
    for _ in 0..40 {
        app.edge_scroll();
    }
    let scrolled = screen(&draw(&mut app, 100, 16));
    assert_ne!(first_screen, scrolled, "the view moved with the mouse");
    let [recode_tui::app::Action::Copy(copied)] = &app.mouse_up(x + width - 1, y + height + 2)[..] else {
        panic!("expected a copy")
    };
    assert!(!app.edge_scrolling(), "letting go stops the scrolling");
    assert!(
        copied.lines().count() > usize::from(height),
        "the copy covers more than one screen: {} lines from a {height}-line view",
        copied.lines().count()
    );
    assert!(copied.contains("The retry loop in auth.rs") && copied.contains("MAX_RETRIES"), "{copied}");
}

#[test]
fn a_triple_click_copies_a_whole_message_even_taller_than_the_screen() {
    let mut app = finished();
    let text = screen(&draw(&mut app, 100, 16));
    let (x, y, _, height) = app.transcript_rect;
    // The final answer is longer than this small view; click on one of its lines.
    let row = (y..y + height)
        .find(|row| text.lines().nth(usize::from(*row)).is_some_and(|line| line.contains("MAX_RETRIES")))
        .expect("part of the last answer is on screen");
    let mut copied = None;
    for _ in 0..3 {
        app.mouse_down(x + 6, row, false);
        if let [recode_tui::app::Action::Copy(text)] = &app.mouse_up(x + 6, row)[..] {
            copied = Some(text.clone());
        }
    }
    let copied = copied.expect("the third click copies");
    assert!(copied.lines().count() > usize::from(height), "the whole message, not just what's visible");
    assert!(copied.contains("MAX_RETRIES") && copied.contains("Things to check next"), "{copied}");
    assert!(!copied.contains("The retry loop in auth.rs never backs off"), "only that message, not the request");
}

#[test]
fn ctrl_click_and_the_badge_go_back_to_the_latest() {
    let mut app = finished();
    draw(&mut app, 100, 30);
    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Home,
        crossterm::event::KeyModifiers::NONE,
    ));
    draw(&mut app, 100, 30);
    let (row, from, _) = app.hits.badge.expect("the badge is shown when scrolled up");
    app.mouse_down(from + 1, row, false);
    assert!(!screen(&draw(&mut app, 100, 30)).contains("more lines"), "clicking the badge followed");

    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Home,
        crossterm::event::KeyModifiers::NONE,
    ));
    draw(&mut app, 100, 30);
    app.mouse_down(5, 5, true);
    assert!(!screen(&draw(&mut app, 100, 30)).contains("more lines"), "Ctrl+click followed");

    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Home,
        crossterm::event::KeyModifiers::NONE,
    ));
    app.input.insert("half a thought");
    app.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::End,
        crossterm::event::KeyModifiers::CONTROL,
    ));
    assert!(!screen(&draw(&mut app, 100, 30)).contains("more lines"), "Ctrl+End works even while typing");
}

#[test]
fn the_side_by_side_pager_and_line_numbers_fit_every_size() {
    let mut pager = every_scene().into_iter().find(|(name, _)| *name == "pager side by side").unwrap().1;
    let mut numbered = every_scene().into_iter().find(|(name, _)| *name == "line numbers").unwrap().1;
    for (width, height) in sizes() {
        let paged = screen(&draw(&mut pager, width, height));
        if width >= 60 && height >= 8 {
            assert!(paged.contains("retry_delay"), "{width}x{height}:\n{paged}");
        }
        draw(&mut numbered, width, height);
    }
    let wide = screen(&draw(&mut pager, 160, 45));
    assert!(wide.contains("let _delay") && wide.contains("tokio::time::sleep"), "{wide}");
    let row = wide.lines().find(|l| l.contains("let _delay")).unwrap();
    assert!(row.contains("tokio::time::sleep"), "old and new sit on one row when wide:\n{row}");
    snapshot("pager_side_by_side", &mut pager, 160, 45);
    let numbered_text = screen(&draw(&mut numbered, 120, 36));
    assert!(numbered_text.contains("1 │"), "{numbered_text}");
    snapshot("line_numbers", &mut numbered, 120, 36);
}

#[test]
fn the_home_and_settings_tabs_offer_their_rows_at_every_usable_size() {
    let mut home = every_scene().into_iter().find(|(name, _)| *name == "home dashboard").unwrap().1;
    let mut settings = every_scene().into_iter().find(|(name, _)| *name == "settings").unwrap().1;
    for (width, height) in sizes() {
        let dashboard = screen(&draw(&mut home, width, height));
        let settings_text = screen(&draw(&mut settings, width, height));
        if width >= 40 && height >= 14 {
            assert!(dashboard.contains("fix the retry loop"), "{width}x{height}:\n{dashboard}");
            assert!(dashboard.contains("free-coder"), "{width}x{height}:\n{dashboard}");
            assert!(settings_text.contains("Theme"), "{width}x{height}:\n{settings_text}");
        }
    }
    snapshot("home_dashboard", &mut home, 60, 20);
    snapshot("home_dashboard", &mut home, 120, 36);
    snapshot("settings", &mut settings, 60, 20);
    snapshot("settings", &mut settings, 120, 36);
    // Enter on a session continues it; Enter on a free model starts it.
    home.handle_key(crossterm::event::KeyEvent::new(
        crossterm::event::KeyCode::Enter,
        crossterm::event::KeyModifiers::NONE,
    ));
}

#[test]
fn dragging_outside_the_conversation_copies_what_is_on_screen_within_that_region() {
    // The side panel: a drag over its rows copies them, and never bleeds into the conversation.
    let mut app = finished();
    app.panel_preference = Some(true);
    let terminal = draw(&mut app, 160, 40);
    let panel = app.hits.panel.expect("the panel is shown at this width");
    let text = screen(&terminal);
    assert!(text.contains("Tools"), "{text}");
    app.mouse_down(panel.x + 2, panel.y + 1, false);
    app.mouse_drag(panel.x + 2, panel.y + 4);
    // Dragging past the panel's left edge stays clipped to the panel.
    app.mouse_drag(panel.x.saturating_sub(20), panel.y + 4);
    let _ = draw(&mut app, 160, 40);
    let actions = app.mouse_up(panel.x.saturating_sub(20), panel.y + 4);
    let [Action::Copy(copied)] = &actions[..] else { panic!("{actions:?}") };
    assert!(!copied.trim().is_empty());
    assert!(!copied.contains("retry_delay"), "nothing from the conversation: {copied}");
    assert!(app.last_toast().is_some_and(|toast| toast.starts_with("Copied")));
    // A key other than Ctrl+C lets the selection go.
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(app.screen_selection.is_none());

    // The Settings tab on the home screen: rows can be copied too.
    let mut home = home_on(HomeTab::Settings, sample_extensions());
    let terminal = draw(&mut home, 100, 30);
    let text = screen(&terminal);
    let row = text.lines().position(|line| line.contains("Theme background")).expect("a settings row") as u16;
    home.mouse_down(4, row, false);
    home.mouse_drag(60, row);
    let _ = draw(&mut home, 100, 30);
    let actions = home.mouse_up(60, row);
    assert!(matches!(&actions[..], [Action::Copy(copied)] if copied.contains("Theme background")), "{actions:?}");

    // F2 hands the mouse to the terminal from any screen, including the home screen and overlays.
    assert_eq!(home.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)), [Action::SetMouseCapture(false)]);
    home.overlay = Some(Overlay::Help);
    assert_eq!(home.handle_key(KeyEvent::new(KeyCode::F(2), KeyModifiers::NONE)), [Action::SetMouseCapture(true)]);
}

#[test]
fn rewind_and_history_search_fit_every_size() {
    let mut rewind = every_scene().into_iter().find(|(name, _)| *name == "rewind").unwrap().1;
    let mut search = every_scene().into_iter().find(|(name, _)| *name == "history search").unwrap().1;
    for (width, height) in sizes() {
        let text = screen(&draw(&mut rewind, width, height));
        if width >= 60 && height >= 16 {
            assert!(text.contains("2. Now add tests"), "{width}x{height}:\n{text}");
            assert!(text.contains("◉ Restore the conversation and put the files back"), "{width}x{height}:\n{text}");
        }
        let text = screen(&draw(&mut search, width, height));
        if width >= 50 && height >= 12 {
            assert!(text.contains("fix the lexer") && text.contains("fix the parser"), "{width}x{height}:\n{text}");
            assert!(!text.contains("add tests for auth"), "{width}x{height}:\n{text}");
        }
    }
    snapshot("rewind", &mut rewind, 100, 30);
    snapshot("history_search", &mut search, 100, 30);
    let mut tasks = every_scene().into_iter().find(|(name, _)| *name == "tasks").unwrap().1;
    let text = screen(&draw(&mut tasks, 100, 30));
    assert!(text.contains("b1  cargo build --release") && text.contains("running · 42 s"), "{text}");
    assert!(text.contains("Address already in use"), "{text}");
    snapshot("tasks", &mut tasks, 100, 30);
    let actions = tasks.handle_key(KeyEvent::new(KeyCode::Char('k'), KeyModifiers::NONE));
    assert_eq!(actions, [Action::KillTask("b1".into())]);
    let mut transcript = every_scene().into_iter().find(|(name, _)| *name == "transcript mode").unwrap().1;
    let text = screen(&draw(&mut transcript, 120, 36));
    assert!(text.contains("Transcript · line 1 of") && text.contains("\"retry\": 1 of 2"), "{text}");
    snapshot("transcript_mode", &mut transcript, 120, 36);
}

#[test]
fn queued_messages_sit_above_the_prompt() {
    let mut app = every_scene().into_iter().find(|(name, _)| *name == "queued input").unwrap().1;
    for (width, height) in sizes() {
        let text = screen(&draw(&mut app, width, height));
        if width >= 60 && height >= 14 {
            assert!(text.contains("queued: \"also update the docs"), "{width}x{height}:\n{text}");
            assert!(text.contains("(+1 more)"), "{width}x{height}:\n{text}");
        }
    }
    snapshot("queued_input", &mut app, 100, 30);
}

#[test]
fn the_permission_mode_shows_in_the_status_line_and_the_prompt() {
    let mut plan = every_scene().into_iter().find(|(name, _)| *name == "plan mode").unwrap().1;
    let text = screen(&draw(&mut plan, 120, 36));
    assert!(text.contains("plan ⏸"), "{text}");
    assert!(text.contains(" ⏸  "), "the prompt glyph changes too: {text}");
    let mut auto = every_scene().into_iter().find(|(name, _)| *name == "auto mode").unwrap().1;
    let text = screen(&draw(&mut auto, 120, 36));
    assert!(text.contains("AUTO · sandbox") && text.contains(" ⚡  "), "{text}");
    snapshot("auto_mode", &mut auto, 120, 36);
    let mut permissions = every_scene().into_iter().find(|(name, _)| *name == "permissions").unwrap().1;
    for (width, height) in sizes() {
        let text = screen(&draw(&mut permissions, width, height));
        if width >= 60 && height >= 16 {
            assert!(text.contains("Mode: accept edits"), "{width}x{height}:\n{text}");
            assert!(text.contains("edit and create files in this project"), "{width}x{height}:\n{text}");
        }
    }
    snapshot("permissions", &mut permissions, 100, 30);
}

#[test]
fn the_memory_list_names_each_file_with_its_cost_or_why_it_is_off() {
    let mut app = every_scene().into_iter().find(|(name, _)| *name == "memory").unwrap().1;
    for (width, height) in sizes() {
        let text = screen(&draw(&mut app, width, height));
        if width >= 70 && height >= 14 {
            assert!(text.contains("off until /trust"), "{width}x{height}:\n{text}");
            assert!(text.contains("~180 tokens"), "{width}x{height}:\n{text}");
        }
    }
    snapshot("memory", &mut app, 100, 30);
    let mut note = every_scene().into_iter().find(|(name, _)| *name == "note target").unwrap().1;
    let text = screen(&draw(&mut note, 100, 30));
    assert!(text.contains("this machine only"), "{text}");
    snapshot("note_target", &mut note, 100, 30);
    let mut init = every_scene().into_iter().find(|(name, _)| *name == "init preview").unwrap().1;
    let text = screen(&draw(&mut init, 100, 30));
    assert!(text.contains("Write RE-CODE.md?") && text.contains("cargo test --workspace"), "{text}");
    snapshot("init_preview", &mut init, 100, 30);
}

#[test]
fn the_compactions_list_shows_each_one_and_enter_opens_what_it_replaced() {
    let mut app = every_scene().into_iter().find(|(name, _)| *name == "compactions").unwrap().1;
    for (width, height) in sizes() {
        let text = screen(&draw(&mut app, width, height));
        if width >= 60 && height >= 10 {
            assert!(text.contains("#2 · 2 minutes ago"), "{width}x{height}:\n{text}");
        }
        if width >= 80 && height >= 10 {
            assert!(text.contains("summarised 22"), "{width}x{height}:\n{text}");
        }
    }
    snapshot("compactions", &mut app, 100, 30);
    let actions = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(actions, [Action::ShowCompaction(2)], "newest first");
    assert!(app.overlay.is_none());
}

#[test]
fn the_status_line_says_how_much_room_is_left_before_compaction() {
    let mut warning = every_scene().into_iter().find(|(name, _)| *name == "context warning").unwrap().1;
    let mut critical = every_scene().into_iter().find(|(name, _)| *name == "context critical").unwrap().1;
    let wide = screen(&draw(&mut warning, 140, 30));
    assert!(wide.contains("ctx ~9.0k/16k · 30% left"), "{wide}");
    let wide = screen(&draw(&mut critical, 140, 30));
    assert!(wide.contains("ctx ~12k/16k · compacts soon"), "{wide}");
    snapshot("context_critical", &mut critical, 120, 36);
}

#[test]
fn the_context_breakdown_shows_the_total_and_the_biggest_parts_at_every_size() {
    let mut app = every_scene().into_iter().find(|(name, _)| *name == "context breakdown").unwrap().1;
    for (width, height) in sizes() {
        let text = screen(&draw(&mut app, width, height));
        if width >= 40 && height >= 8 {
            assert!(text.contains("Context ~8.8k"), "{width}x{height}:\n{text}");
            assert!(text.contains("Tool results"), "the biggest part is always shown {width}x{height}:\n{text}");
        }
        if width >= 90 && height >= 24 {
            assert!(text.contains("last reported 8902 in, 7410 from cache"), "{text}");
            assert!(text.contains("read src/auth.rs") && text.contains("Free until compaction: ~4.1k"), "{text}");
            assert!(text.contains("1 not counted"), "images are counted, not estimated: {text}");
        }
    }
    snapshot("context_breakdown", &mut app, 60, 20);
    snapshot("context_breakdown", &mut app, 120, 36);
    // c compacts, r shows the raw request, Esc closes. (Not in the demo, which has no live request.)
    app.demo = false;
    let actions = app.handle_key(KeyEvent::new(KeyCode::Char('r'), KeyModifiers::NONE));
    assert_eq!(actions, [Action::ShowLastRequest]);
    assert!(app.overlay.is_none());
    app.overlay = Some(Overlay::Context);
    app.handle_key(KeyEvent::new(KeyCode::Esc, KeyModifiers::NONE));
    assert!(app.overlay.is_none());
}

#[test]
fn the_context_rows_step_the_window_and_a_puts_it_back_to_auto() {
    use recode_core::context::{Size, Trigger};
    let mut app = every_scene().into_iter().find(|(name, _)| *name == "settings with a model").unwrap().1;
    for _ in 0..4 {
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    let text = screen(&draw(&mut app, 100, 30));
    assert!(text.contains("gpt-oss:20b · auto (16k of 128k)"), "{text}");
    assert!(text.contains("compacts at ~13k") && text.contains("Reserved for the reply"), "{text}");

    // Right on "Context size" steps up to the next size; the window follows at once.
    let actions = app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(
        matches!(&actions[..], [Action::SetContextWindow { model, setting }] if model == "gpt-oss:20b" && setting.size == Size::Tokens(32768)),
        "{actions:?}"
    );
    assert_eq!(app.context_window.as_ref().unwrap().window, Some(32768));
    let text = screen(&draw(&mut app, 100, 30));
    assert!(text.contains("32k of 128k"), "{text}");
    // It never goes past the model's limit.
    for _ in 0..6 {
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    }
    assert_eq!(app.context_window.as_ref().unwrap().window, Some(131_072));
    let actions = app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    assert!(
        matches!(&actions[..], [Action::SetContextWindow { setting, .. }] if setting.size == Size::Auto),
        "{actions:?}"
    );
    assert_eq!(app.context_window.as_ref().unwrap().window, Some(16384));

    // Compaction: auto → 50 % of the window → … → off.
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    let actions = app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert!(
        matches!(&actions[..], [Action::SetContextWindow { setting, .. }] if setting.trigger == Trigger::Tokens(8192)),
        "{actions:?}"
    );
    for _ in 0..6 {
        app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    }
    assert_eq!(app.context_setting.trigger, Trigger::Off);
    let text = screen(&draw(&mut app, 100, 30));
    assert!(text.contains("no auto-compaction"), "{text}");
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    assert_eq!(app.context_setting.trigger, Trigger::Auto);

    // Reserve: auto → 1k.
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Right, KeyModifiers::NONE));
    assert_eq!(app.context_setting.reserve, Size::Tokens(1024));
    assert_eq!(app.context_window.as_ref().unwrap().reserve, 1024);
    snapshot("settings_with_model", &mut app, 100, 30);
}

#[test]
fn the_free_setup_names_the_providers_and_never_shows_the_key() {
    let mut pick = every_scene().into_iter().find(|(name, _)| *name == "free setup pick").unwrap().1;
    let mut key = every_scene().into_iter().find(|(name, _)| *name == "free setup key").unwrap().1;
    for (width, height) in sizes() {
        let picked = screen(&draw(&mut pick, width, height));
        let keyed = screen(&draw(&mut key, width, height));
        assert!(!keyed.contains("sk-secret"), "{width}x{height}: the key must never be shown:\n{keyed}");
        if width >= 40 && height >= 14 {
            assert!(picked.contains("openrouter"), "{width}x{height}:\n{picked}");
            assert!(!picked.contains("opencode-zen"), "Zen's free models only work in OpenCode's app:\n{picked}");
            assert!(keyed.contains("openrouter.ai/keys") && keyed.contains("••••"), "{width}x{height}:\n{keyed}");
        }
    }
    snapshot("free_setup_pick", &mut pick, 60, 20);
    snapshot("free_setup_key", &mut key, 120, 36);
}

#[test]
fn a_finished_turn_shows_its_cost_and_the_task_list() {
    let mut app = finished();
    let wide = screen(&draw(&mut app, 160, 45));
    assert!(wide.contains("Tasks"), "the side panel lists the model's tasks:\n{wide}");
    assert!(wide.contains("◐ Apply the delay") && wide.contains("☑ Read the retry loop"), "{wide}");
    assert!(wide.contains("tools ·") && wide.contains(" in") && wide.contains(" out"), "the turn footer:\n{wide}");
    // Without the panel, the status line keeps the count.
    let narrow = screen(&draw(&mut app, 100, 30));
    assert!(narrow.contains("tasks 1/3"), "{narrow}");
}

#[test]
fn toasts_show_in_the_corner_and_expire() {
    let mut app = every_scene().into_iter().find(|(name, _)| *name == "toasts").unwrap().1;
    for (width, height) in sizes() {
        let text = screen(&draw(&mut app, width, height));
        if width >= 30 && height >= 8 {
            assert!(text.contains("Copied 3 lines"), "{width}x{height}:\n{text}");
        }
    }
    snapshot("toasts", &mut app, 60, 20);
    snapshot("toasts", &mut app, 120, 36);
    app.now += std::time::Duration::from_secs(recode_tui::app::TOAST_SECONDS + 1);
    assert!(!screen(&draw(&mut app, 100, 30)).contains("Copied 3 lines"), "gone after a few seconds");
}

#[test]
fn snapshots_of_the_theme_picker() {
    let mut app = every_scene().into_iter().find(|(name, _)| *name == "themes").unwrap().1;
    for (width, height) in [(60, 20), (120, 36)] {
        snapshot("themes", &mut app, width, height);
    }
}

#[test]
fn snapshots_of_the_completion_popup() {
    for (name, mut app) in
        every_scene().into_iter().filter(|(name, _)| *name == "slash commands" || *name == "file mentions")
    {
        let label = name.replace(' ', "_");
        for (width, height) in [(60, 20), (120, 36)] {
            snapshot(&label, &mut app, width, height);
        }
    }
}

fn sizes() -> impl Iterator<Item = (u16, u16)> {
    let widths = (1..=70).chain((71..=260).step_by(13));
    widths.flat_map(|width| (1..=40).chain([48, 60, 75]).map(move |height| (width, height)))
}

fn every_scene() -> Vec<(&'static str, App)> {
    let mut scenes = vec![("approval", at_approval()), ("finished", finished())];

    let mut app = App::new(Instant::now(), "p", "demo-model", "recorded", true);
    app.apply(EventKind::StateChanged { state: AgentState::LoadingModel });
    scenes.push(("empty, loading", app));

    let mut app = finished();
    app.input.insert("a message long enough to wrap at most sizes\nwith a second line\n世界世界世界 🦀🦀🦀");
    scenes.push(("long input", app));

    let mut app = finished();
    app.input.insert("/");
    scenes.push(("slash commands", app));

    let mut app = finished();
    app.open_free_setup();
    scenes.push(("free setup pick", app));

    let mut app = finished();
    app.ledger = Some(recode_core::usage::Ledger {
        components: vec![
            recode_core::usage::Component { kind: recode_core::usage::ComponentKind::System, tokens: 410, count: 1 },
            recode_core::usage::Component {
                kind: recode_core::usage::ComponentKind::SkillsIndex,
                tokens: 120,
                count: 1,
            },
            recode_core::usage::Component {
                kind: recode_core::usage::ComponentKind::ToolDefinitions,
                tokens: 1800,
                count: 11,
            },
            recode_core::usage::Component {
                kind: recode_core::usage::ComponentKind::Conversation,
                tokens: 2300,
                count: 9,
            },
            recode_core::usage::Component {
                kind: recode_core::usage::ComponentKind::ToolResults,
                tokens: 4100,
                count: 6,
            },
            recode_core::usage::Component { kind: recode_core::usage::ComponentKind::Elided, tokens: 90, count: 3 },
            recode_core::usage::Component { kind: recode_core::usage::ComponentKind::Images, tokens: 0, count: 1 },
        ],
        total: recode_core::usage::TokenCount::Estimated { tokens: 8820, method: "3 characters per token".into() },
        window: Some(16384),
        reserve: 2048,
        trigger: Some(12903),
        largest_results: vec![("read src/auth.rs".into(), 2100), ("shell cargo test".into(), 1500)],
    });
    app.last_usage = Some(recode_core::usage::ReportedUsage {
        prompt_tokens: Some(8902),
        cached_prompt_tokens: Some(7410),
        ..recode_core::usage::ReportedUsage::default()
    });
    app.overlay = Some(Overlay::Context);
    scenes.push(("context breakdown", app));

    let mut app = finished();
    app.compactions_listed(vec![
        (
            "20 minutes ago".into(),
            recode_core::usage::CompactionReport {
                seq: 1,
                at_ms: 0,
                before_tokens: 14_200,
                after_tokens: 6_100,
                elided: 9,
                summarised: 0,
                focus: None,
                problem: None,
                kept_objective: "Fix the retry loop in auth.rs".into(),
            },
        ),
        (
            "2 minutes ago".into(),
            recode_core::usage::CompactionReport {
                seq: 2,
                at_ms: 0,
                before_tokens: 15_800,
                after_tokens: 4_900,
                elided: 3,
                summarised: 22,
                focus: Some("keep the exact cargo commands".into()),
                problem: None,
                kept_objective: "Fix the retry loop in auth.rs".into(),
            },
        ),
    ]);
    scenes.push(("compactions", app));

    let mut app = finished();
    app.instructions_listed(
        vec![
            recode_tui::link::InstructionFileInfo {
                scope: "yours".into(),
                path: "/home/someone/.config/re-code/RE-CODE.md".into(),
                tokens: 180,
                trusted: true,
                problem: None,
            },
            recode_tui::link::InstructionFileInfo {
                scope: "project".into(),
                path: "/home/someone/code/api-server/AGENTS.md".into(),
                tokens: 2400,
                trusted: false,
                problem: None,
            },
            recode_tui::link::InstructionFileInfo {
                scope: "project".into(),
                path: "/home/someone/code/api-server/CLAUDE.md".into(),
                tokens: 12_000,
                trusted: true,
                problem: Some("48 KB is too long to send; only its path is mentioned".into()),
            },
        ],
        "/home/someone/.config/re-code/RE-CODE.md".into(),
        Some("/home/someone/code/api-server/RE-CODE.md".into()),
    );
    scenes.push(("memory", app));

    for (name, mode) in [
        ("plan mode", recode_core::protocol::PermissionMode::Plan),
        ("auto mode", recode_core::protocol::PermissionMode::AutoSandbox),
    ] {
        let mut app = finished();
        app.demo = false;
        app.apply(EventKind::PermissionModeChanged { mode });
        scenes.push((name, app));
    }

    let mut app = finished();
    app.permissions_listed(
        recode_core::protocol::PermissionMode::AcceptEdits,
        vec!["run `cargo test` with network".into(), "read files in ~/notes".into()],
        vec![
            ("{\"kind\":\"project_edits\"}".into(), "edit and create files in this project".into()),
            ("{\"kind\":\"exact\",\"0\":\"shell:offline:cargo fmt\"}".into(), "run `cargo fmt`".into()),
        ],
    );
    scenes.push(("permissions", app));

    let mut app = finished();
    app.demo = false;
    app.apply(EventKind::StateChanged { state: AgentState::Thinking });
    app.queue.push_back(("also update the docs when you're done".into(), Vec::new()));
    app.queue.push_back(("and run the tests".into(), Vec::new()));
    scenes.push(("queued input", app));

    let mut app = finished();
    app.demo = false;
    app.overlay = Some(Overlay::Rewind {
        turns: vec![
            (1, "Now add tests for the backoff".into()),
            (0, "The retry loop in auth.rs never backs off".into()),
        ],
        selected: 0,
        choice: 1,
    });
    scenes.push(("rewind", app));

    let mut app = finished();
    app.history = vec!["fix the parser".into(), "add tests for auth".into(), "fix the lexer".into()];
    app.overlay = Some(Overlay::HistorySearch { query: "fix".into(), all_projects: false, selected: 0 });
    scenes.push(("history search", app));

    let mut app = finished();
    app.overlay =
        Some(Overlay::Transcript { offset: 0, typing: None, query: "retry".into(), matches: vec![0, 2], current: 0 });
    scenes.push(("transcript mode", app));

    let mut app = finished();
    app.tasks_listed(vec![
        recode_tui::link::TaskInfo {
            id: "b1".into(),
            command: "cargo build --release".into(),
            status: "running".into(),
            elapsed_secs: 42,
            last_line: "   Compiling recode-tui v0.1.0".into(),
        },
        recode_tui::link::TaskInfo {
            id: "b2".into(),
            command: "python -m http.server 8000".into(),
            status: "exit 1".into(),
            elapsed_secs: 3,
            last_line: "OSError: [Errno 98] Address already in use".into(),
        },
    ]);
    scenes.push(("tasks", app));

    let mut app = finished();
    app.overlay =
        Some(Overlay::NoteTarget { text: "prefer small diffs and run ./scripts/check.sh".into(), selected: 1 });
    scenes.push(("note target", app));

    let mut app = finished();
    app.init_preview(
        "/home/someone/code/api-server/RE-CODE.md".into(),
        "# api-server\n\nInstructions for re-code (also read: AGENTS.md, CLAUDE.md).\n\n## About\n\n- A Rust workspace with crates: crates/core, crates/app.\n\n## Commands\n\n- Test: `cargo test --workspace`\n- Full check before finishing: `./scripts/check.sh`\n\n## Compact instructions\n\nKeep the exact commands and file paths from this file.\n".into(),
    );
    scenes.push(("init preview", app));

    // Nearly out of room, and getting there: the status line says so, louder as it fills.
    for (name, used) in [("context warning", 9_000u64), ("context critical", 12_400u64)] {
        let mut app = finished();
        let setting = recode_core::context::WindowSetting::default();
        let window = recode_core::context::ContextWindow::resolve(&setting, Some(16384), true, 16384);
        app.set_context_window(window, setting, None);
        app.context =
            Some((recode_core::usage::TokenCount::Estimated { tokens: used, method: "3 chars".into() }, 16384));
        scenes.push((name, app));
    }

    let mut app = finished();
    app.overlay = Some(recode_tui::app::Overlay::Pager {
        title: "edit src/auth.rs".into(),
        text: "--- a/src/auth.rs\n+++ b/src/auth.rs\n@@ -39,7 +39,7 @@\n     for attempt in 0..MAX_RETRIES {\n-                let _delay = retry_delay(attempt);\n+                tokio::time::sleep(retry_delay(attempt)).await;\n     }\n".into(),
        is_diff: true,
        side_by_side: true,
        offset: 0,
    });
    scenes.push(("pager side by side", app));

    let mut app = finished();
    app.settings.insert("line_numbers".into(), "on".into());
    scenes.push(("line numbers", app));

    let mut app = home_on(HomeTab::Home, sample_extensions());
    app.sessions_listed(vec![
        recode_tui::link::SessionSummary {
            id: "a".into(),
            title: "fix the retry loop".into(),
            age: "5 minutes ago".into(),
            messages: 12,
        },
        recode_tui::link::SessionSummary {
            id: "b".into(),
            title: "add tests".into(),
            age: "2 hours ago".into(),
            messages: 4,
        },
    ]);
    if let Some(info) = &mut app.welcome_info {
        let mut free = sample_model("openrouter/free-coder", 0.0, GpuFit::Cloud, true);
        free.size_gib = None;
        free.provider_label = Some("openrouter".into());
        free.free = true;
        info.models.push(free);
    }
    scenes.push(("home dashboard", app));

    scenes.push(("home dashboard, empty", home_on(HomeTab::Home, sample_extensions())));
    scenes.push(("settings", home_on(HomeTab::Settings, sample_extensions())));

    // With a model active, its context window has its own rows.
    let mut app = home_on(HomeTab::Settings, sample_extensions());
    app.model = "gpt-oss:20b".into();
    let setting = recode_core::context::WindowSetting::default();
    let window = recode_core::context::ContextWindow::resolve(&setting, Some(131_072), true, 16384);
    app.set_context_window(window, setting, None);
    scenes.push(("settings with a model", app));

    let mut app = finished();
    app.open_key_step("openrouter");
    app.paste("sk-secret-123456");
    scenes.push(("free setup key", app));

    // The last preset selected: the list is longer than most terminals, so it must window.
    let mut app = finished();
    app.overlay = Some(recode_tui::app::Overlay::ProviderPresets {
        selected: recode_core::providers::PROVIDER_PRESETS.len() - 1,
    });
    scenes.push(("provider presets, last", app));

    let mut app = finished();
    app.toast("Copied 3 lines", recode_tui::app::ToastLevel::Success);
    app.toast(
        "This model can't see images. Switch to a vision model with Ctrl+L.",
        recode_tui::app::ToastLevel::Warning,
    );
    scenes.push(("toasts", app));

    let mut app = finished();
    app.overlay =
        Some(recode_tui::app::Overlay::Themes { selected: 4, original: recode_tui::theme::ThemeSelection::default() });
    scenes.push(("themes", app));

    let mut app = finished();
    app.project_files = (0..40).map(|n| format!("src/module_{n}/a_rather_long_file_name_{n}.rs")).collect();
    app.input.insert("look at @mod");
    scenes.push(("file mentions", app));

    let mut app = finished();
    app.model_vision = true;
    app.pending_images = vec![
        recode_core::message::ImageData {
            media_type: "image/png".into(),
            data_base64: "AAAA".into(),
            label: "diagram.png".into(),
        },
        recode_core::message::ImageData {
            media_type: "image/png".into(),
            data_base64: "AAAA".into(),
            label: "pasted".into(),
        },
    ];
    app.input.insert("what is in these");
    scenes.push(("with image chips", app));

    let mut app = finished();
    app.overlay = Some(Overlay::Help);
    scenes.push(("help", app));

    let mut app = finished();
    app.overlay = Some(Overlay::Palette { query: "o".into(), selected: 0 });
    scenes.push(("palette", app));

    let mut app = finished();
    app.overlay = Some(Overlay::Pager {
        title: "shell cargo test".into(),
        text: "output line\n".repeat(200),
        is_diff: false,
        side_by_side: false,
        offset: 10_000,
    });
    scenes.push(("pager", app));

    for (name, info) in welcome_scenes() {
        let mut app = App::new(Instant::now(), "api-server", "", "", false);
        app.open_welcome();
        if let Some(info) = info {
            app.set_welcome_info(info);
        }
        scenes.push((name, app));
    }

    for tab in [HomeTab::Providers, HomeTab::Mcp, HomeTab::Skills, HomeTab::Tools] {
        scenes.push(("extensions tab", home_on(tab, sample_extensions())));
        scenes.push(("empty extensions tab", home_on(tab, ExtensionsSnapshot::default())));
        let mut app = home_on(tab, sample_extensions());
        app.overlay = Some(Overlay::Form(FormState::new_for(tab)));
        scenes.push(("form", app));
    }
    let mut app = home_on(HomeTab::Mcp, sample_extensions());
    let mut form = FormState::new_for(HomeTab::Mcp);
    form.error = Some("Something about this field isn't right, and here's a long explanation of why.".to_owned());
    form.fields[1].value = "npx -y a-really-long-command-line --with many --flags ".repeat(4);
    app.overlay = Some(Overlay::Form(form));
    scenes.push(("form with an error", app));
    let mut app = home_on(HomeTab::Mcp, sample_extensions());
    app.overlay = Some(Overlay::Confirm(ConfirmState {
        title: "Test local?".to_owned(),
        body: vec!["This runs the following program:".to_owned(), "npx -y a-long-command ".repeat(10)],
        command: ExtensionCommand::TestServer("local".to_owned()).into(),
    }));
    app.home_message =
        Some(("A long result message that needs to be cut to fit within narrower terminals".to_owned(), true));
    scenes.push(("confirm", app));

    let mut app = finished();
    app.panel_preference = Some(true);
    app.options.show_reasoning = true;
    scenes.push(("panel and reasoning", app));

    let mut app = files_pane();
    app.panel_width = 60;
    scenes.push(("files pane, wide panel", app));

    scenes.push(("runtimes tab", runtimes_tab()));
    scenes.push(("mcp registry search", mcp_search()));
    let mut app = home_on(HomeTab::Mcp, sample_extensions());
    app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
    scenes.push(("mcp tool filter", app));
    scenes.push(("skills tab with scan verdicts", flagged_skills_tab()));
    let mut app = flagged_skills_tab();
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    scenes.push(("trust confirm", app));
    let mut app = runtimes_tab();
    app.runtimes = Some(recode_tui::link::RuntimesInfo::default());
    scenes.push(("runtimes tab, nothing found", app));

    scenes
}

/// The MCP tab with a registry search showing one addable hit and one that can't be added.
fn mcp_search() -> App {
    use recode_core::extensions::{McpSearchHit, McpTransport};
    let mut app = home_on(HomeTab::Mcp, sample_extensions());
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
    for c in "filesystem".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    let hits = vec![
        McpSearchHit {
            name: "io.github.modelcontextprotocol/server-filesystem".to_owned(),
            description: "Secure file operations with configurable access controls.".to_owned(),
            version: Some("2025.8.21".to_owned()),
            repository: Some("https://github.com/modelcontextprotocol/servers".to_owned()),
            server: Some(recode_core::extensions::McpServer {
                name: "server-filesystem".to_owned(),
                transport: McpTransport::Stdio {
                    command: "npx".to_owned(),
                    args: vec!["-y".to_owned(), "@modelcontextprotocol/server-filesystem@2025.8.21".to_owned()],
                    env: Default::default(),
                },
                enabled: true,
                expose_to_agent: false,
                external: None,
                tools: None,
            }),
            unsupported: None,
        },
        McpSearchHit {
            name: "ai.example/legacy-fs".to_owned(),
            description: "An older server.".to_owned(),
            version: Some("0.1.0".to_owned()),
            repository: None,
            server: None,
            unsupported: Some("only an SSE remote is offered, and re-code speaks streamable HTTP".to_owned()),
        },
    ];
    app.mcp_search_result("filesystem", hits, None);
    app
}

#[test]
fn registry_search_adds_the_chosen_server_and_the_tool_filter_saves_a_choice() {
    use recode_tui::app::Action;
    let mut app = mcp_search();
    let text = screen(&draw(&mut app, 120, 36));
    assert!(text.contains("Search the MCP registry") && text.contains("server-filesystem"), "{text}");
    assert!(text.contains("can't be added"), "{text}");
    let actions = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(&actions[..], [Action::Extension(ExtensionCommand::SaveServer { original: None, server })] if server.name == "server-filesystem"),
        "{actions:?}"
    );
    assert!(app.overlay.is_none());
    // Searching: Enter with a query and no hits sends the search; a typed character clears old hits.
    let mut app = home_on(HomeTab::Mcp, sample_extensions());
    app.handle_key(KeyEvent::new(KeyCode::Char('s'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('g'), KeyModifiers::NONE));
    let actions = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert_eq!(actions, vec![Action::Extension(ExtensionCommand::SearchMcp("g".to_owned()))]);
    app.mcp_search_result("g", Vec::new(), None);
    let text = screen(&draw(&mut app, 120, 36));
    assert!(text.contains("Nothing in the registry matches"), "{text}");

    // The tool filter: the first sample server is connected with many tools.
    let mut app = home_on(HomeTab::Mcp, sample_extensions());
    app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
    let text = screen(&draw(&mut app, 120, 36));
    assert!(text.contains("Tools of") && text.contains("☑"), "{text}");
    app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE));
    let actions = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    match &actions[..] {
        [Action::Extension(ExtensionCommand::SetToolFilter { tools: Some(filter), .. })] => {
            assert_eq!(filter.include.len(), 39, "one of forty tools switched off: {filter:?}");
        }
        other => panic!("{other:?}"),
    }
    // Every tool on means no filter at all.
    app.handle_key(KeyEvent::new(KeyCode::Char('f'), KeyModifiers::NONE));
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    let actions = app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE));
    assert!(
        matches!(&actions[..], [Action::Extension(ExtensionCommand::SetToolFilter { tools: None, .. })]),
        "{actions:?}"
    );
}

/// The Skills tab with flagged skills and an untrusted project folder reported by the driver.
fn flagged_skills_tab() -> App {
    let mut app = home_on(HomeTab::Skills, flagged_skills());
    app.project_trust = Some(recode_tui::link::ProjectTrust {
        folder: "/home/someone/code/api-server".to_owned(),
        trusted: false,
        skills: vec!["from-repo".to_owned()],
        files: vec!["instructions.md".to_owned()],
    });
    app
}

/// The sample extensions, with a project skill flagged by the scan and a dangerous one.
fn flagged_skills() -> ExtensionsSnapshot {
    use recode_core::extensions::{Finding, Severity, Verdict};
    let mut snapshot = sample_extensions();
    let finding = |file: &str, line: usize, reason: &str, excerpt: &str, severity| Finding {
        file: file.to_owned(),
        line,
        reason: reason.to_owned(),
        excerpt: excerpt.to_owned(),
        severity,
    };
    if let Some(project) = snapshot.skills.iter_mut().find(|s| s.name == "from-repo") {
        project.scan = Verdict::Caution {
            findings: vec![finding(
                "SKILL.md",
                12,
                "runs a package installer, which executes code from the network",
                "pip install requests",
                Severity::Caution,
            )],
        };
    }
    snapshot.skills.push(Skill {
        name: "helpful-cleanup".to_owned(),
        description: "Cleans the workspace.".to_owned(),
        path: "/home/someone/.config/re-code/skills/helpful-cleanup/SKILL.md".to_owned(),
        source: SkillSource::User,
        enabled: false,
        estimated_tokens: 90,
        scan: Verdict::Dangerous {
            findings: vec![finding(
                "scripts/clean.sh",
                3,
                "deletes the home folder or the whole filesystem",
                "rm -rf ~/",
                Severity::Dangerous,
            )],
        },
        trusted: true,
        ..Skill::default()
    });
    snapshot
}

#[test]
fn flagged_skills_are_marked_and_trust_asks_before_enabling_project_skills() {
    use recode_tui::app::Action;
    let mut app = home_on(HomeTab::Skills, flagged_skills());
    let text = screen(&draw(&mut app, 120, 36));
    assert!(text.contains("⚠ helpful-cleanup") && text.contains("yours · dangerous"), "{text}");
    assert!(text.contains("project · caution"), "{text}");
    // A dangerous skill can't be switched on.
    for _ in 0..4 {
        app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    }
    let text = screen(&draw(&mut app, 120, 36));
    assert!(text.contains("Scan: dangerous"), "{text}");
    assert!(app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)).is_empty());
    assert!(
        app.home_message.as_ref().is_some_and(|(m, _)| m.contains("flagged as dangerous")),
        "{:?}",
        app.home_message
    );
    // Switching on a project skill means trusting the folder. Before the driver has reported what the
    // folder holds, that's a wait; afterwards it shows exactly what would load and asks.
    for _ in 0..3 {
        app.handle_key(KeyEvent::new(KeyCode::Up, KeyModifiers::NONE));
    }
    assert!(app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)).is_empty());
    assert!(app.last_toast().is_some_and(|t| t.contains("Still looking")), "{:?}", app.last_toast());
    app.project_trust = Some(recode_tui::link::ProjectTrust {
        folder: "/home/someone/code/api-server".to_owned(),
        trusted: false,
        skills: vec!["from-repo".to_owned()],
        files: vec![],
    });
    assert!(app.handle_key(KeyEvent::new(KeyCode::Char(' '), KeyModifiers::NONE)).is_empty());
    let text = screen(&draw(&mut app, 120, 36));
    assert!(text.contains("Trust this folder's configuration?") && text.contains("from-repo"), "{text}");
    assert_eq!(app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE)), vec![Action::TrustProject]);
}

/// The Runtimes tab on a stand-in for the development machine: Ollama in a podman pod, a stopped
/// llama.cpp unit, an unrelated pod.
fn runtimes_tab() -> App {
    use recode_tui::link::{ContainerRow, PodRow, RuntimeRow, RuntimeRowState, RuntimesInfo};
    let mut app = home_on(HomeTab::Runtimes, sample_extensions());
    app.runtimes = Some(RuntimesInfo {
        engines: vec!["podman 5.8.9 rootless".to_owned()],
        runtimes: vec![
            RuntimeRow {
                id: "ollama@podman:ollama".to_owned(),
                name: "Ollama".to_owned(),
                supported: true,
                state: RuntimeRowState::Running { version: Some("0.40.14".to_owned()) },
                url: Some("http://localhost:11434".to_owned()),
                served_by: Some("podman container `ollama` in pod `local-ai`".to_owned()),
                image: Some("docker.io/ollama/ollama:rocm".to_owned()),
                gpu: Some("GPU attached (/dev/dri, /dev/kfd)".to_owned()),
                yours: true,
                notes: Vec::new(),
            },
            RuntimeRow {
                id: "llama-cpp@systemd:llama-server.service".to_owned(),
                name: "llama.cpp".to_owned(),
                supported: false,
                state: RuntimeRowState::Stopped {
                    start_command: Some("sudo systemctl start llama-server.service".to_owned()),
                    needs_root: true,
                },
                url: None,
                served_by: Some("systemd system unit llama-server.service".to_owned()),
                image: None,
                gpu: None,
                yours: true,
                notes: Vec::new(),
            },
        ],
        pods: vec![
            PodRow { name: "local-ai".to_owned(), running: true, ports: vec![3000, 8880, 11434] },
            PodRow { name: "pod_firecrawl".to_owned(), running: true, ports: vec![3002] },
        ],
        containers: vec![
            ContainerRow {
                name: "webui-proxy".to_owned(),
                image: "docker.io/library/caddy:2-alpine".to_owned(),
                status: "Up 4 days".to_owned(),
                running: true,
                pod: None,
                ports: vec![8443],
                runs: None,
            },
            ContainerRow {
                name: "ollama".to_owned(),
                image: "docker.io/ollama/ollama:rocm".to_owned(),
                status: "Up 23 hours".to_owned(),
                running: true,
                pod: Some("local-ai".to_owned()),
                ports: vec![3000, 8880, 11434],
                runs: Some("Ollama".to_owned()),
            },
            ContainerRow {
                name: "open-webui".to_owned(),
                image: "ghcr.io/open-webui/open-webui:main".to_owned(),
                status: "Up 23 hours".to_owned(),
                running: true,
                pod: Some("local-ai".to_owned()),
                ports: vec![3000, 8880, 11434],
                runs: None,
            },
            ContainerRow {
                name: "firecrawl_api_1".to_owned(),
                image: "localhost/firecrawl_api:latest".to_owned(),
                status: "Exited (0) 2 hours ago".to_owned(),
                running: false,
                pod: Some("pod_firecrawl".to_owned()),
                ports: vec![3002],
                runs: None,
            },
        ],
        unidentified: vec!["http://127.0.0.1:8080: answers HTTP 404, but not like a runtime re-code knows".to_owned()],
        warnings: Vec::new(),
        problems: Vec::new(),
    });
    app
}

#[test]
fn the_runtimes_tab_lists_what_runs_and_enter_asks_before_starting() {
    use recode_tui::app::Action;
    use recode_tui::link::RuntimeStartEvent;
    let mut app = runtimes_tab();
    let text = screen(&draw(&mut app, 120, 36));
    assert!(text.contains("Ollama") && text.contains("running 0.40.14"), "{text}");
    assert!(text.contains("pod local-ai") && text.contains("open-webui"), "{text}");
    assert!(text.contains("llama.cpp (detected)") && text.contains("stopped"), "{text}");
    assert!(text.contains("GPU attached"), "the details pane shows the container's GPU: {text}");
    // Enter on the running one only explains; on the stopped one it asks, showing the exact command.
    assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).is_empty());
    assert!(app.home_message.as_ref().is_some_and(|(m, _)| m.contains("already running")), "{:?}", app.home_message);
    app.handle_key(KeyEvent::new(KeyCode::Down, KeyModifiers::NONE));
    assert!(app.handle_key(KeyEvent::new(KeyCode::Enter, KeyModifiers::NONE)).is_empty());
    let text = screen(&draw(&mut app, 120, 36));
    assert!(text.contains("Start llama.cpp?") && text.contains("sudo systemctl start llama-server.service"), "{text}");
    assert!(text.contains("password"), "a sudo command warns about the prompt: {text}");
    let actions = app.handle_key(KeyEvent::new(KeyCode::Char('y'), KeyModifiers::NONE));
    assert_eq!(actions, vec![Action::StartRuntime("llama-cpp@systemd:llama-server.service".to_owned())]);
    // While it starts, the row says so; when it's up, a toast says so.
    let id = "llama-cpp@systemd:llama-server.service".to_owned();
    app.runtime_start_event(RuntimeStartEvent::Waiting { id: id.clone(), seconds: 10 });
    let text = screen(&draw(&mut app, 120, 36));
    assert!(text.contains("waiting 10 s"), "{text}");
    app.runtime_start_event(RuntimeStartEvent::Ready {
        id,
        name: "llama.cpp".to_owned(),
        version: Some("b5000".to_owned()),
        verified: true,
    });
    assert!(app.runtime_starting.is_none());
    assert!(app.last_toast().is_some_and(|t| t.contains("llama.cpp b5000 is up")), "{:?}", app.last_toast());
    // Narrow terminals still show the list without the details pane.
    let text = screen(&draw(&mut app, 50, 16));
    assert!(text.contains("Ollama"), "{text}");
}

/// A finished session with the Files pane open on a small project tree.
fn files_pane() -> App {
    let mut app = finished();
    app.panel_preference = Some(true);
    app.panel_tab = recode_tui::app::PanelTab::Files;
    app.project_files = ["src/main.rs", "src/ui/view.rs", "src/ui/app.rs", "docs/guide.md", "README.md", "Cargo.toml"]
        .iter()
        .map(|f| (*f).to_owned())
        .collect();
    app.project_file_set = app.project_files.iter().cloned().collect();
    app.tree.toggle("src");
    app
}

#[test]
fn the_files_pane_lists_the_tree_and_clicks_open_files() {
    use recode_tui::app::Action;
    let mut app = files_pane();
    let text = screen(&draw(&mut app, 120, 36));
    assert!(text.contains("Files"), "{text}");
    assert!(text.contains("▾ src") && text.contains("▸ ui") && text.contains("main.rs"), "{text}");
    assert!(!text.contains("view.rs"), "a closed folder hides its files: {text}");
    // Click the "ui" folder: it opens.
    let row = app
        .hits
        .tree_rows
        .iter()
        .find(|(_, _, _, index)| app.tree.rows(&app.project_files)[*index].path == "src/ui")
        .copied()
        .unwrap();
    // The press acts (clicks outside the conversation can't start a selection); the release does nothing more.
    assert!(app.mouse_down(row.1 + 2, row.0, false).is_empty());
    assert!(app.mouse_up(row.1 + 2, row.0).is_empty());
    let text = screen(&draw(&mut app, 120, 36));
    assert!(text.contains("view.rs"), "{text}");
    // Click a file: it opens in the editor, once.
    let row = app
        .hits
        .tree_rows
        .iter()
        .find(|(_, _, _, index)| app.tree.rows(&app.project_files)[*index].path == "src/main.rs")
        .copied()
        .unwrap();
    assert_eq!(app.mouse_down(row.1 + 2, row.0, false), vec![Action::EditFile("src/main.rs".to_owned())]);
    assert!(app.mouse_up(row.1 + 2, row.0).is_empty());
    // Narrow terminals drop the panel rather than squeezing it.
    let text = screen(&draw(&mut app, 50, 20));
    assert!(!text.contains("▾ src"), "{text}");
}

#[test]
fn dragging_the_panel_edge_resizes_it_and_saves_the_width() {
    use recode_tui::app::Action;
    let mut app = files_pane();
    draw(&mut app, 120, 36);
    let panel = app.hits.panel.unwrap();
    assert_eq!(panel.width, 34);
    app.mouse_down(panel.x, panel.y + 3, false);
    app.mouse_drag(panel.x - 10, panel.y + 3);
    assert_eq!(
        app.mouse_up(panel.x - 10, panel.y + 3),
        vec![Action::SetSetting { key: "panel_width".to_owned(), value: "44".to_owned() }]
    );
    draw(&mut app, 120, 36);
    assert_eq!(app.hits.panel.unwrap().width, 44);
    // Never narrower or wider than the allowed range.
    app.mouse_down(app.hits.panel.unwrap().x, 5, false);
    app.mouse_drag(0, 5);
    app.mouse_up(0, 5);
    assert_eq!(app.panel_width, 60);
}

#[test]
fn project_paths_in_the_conversation_are_clickable() {
    use recode_tui::app::Action;
    use recode_tui::transcript::Entry;
    let mut app = files_pane();
    app.panel_preference = Some(false);
    app.transcript.push(Entry::Assistant("The bug is in src/ui/view.rs:42, not in README.md.".to_owned()));
    draw(&mut app, 100, 30);
    let paths: Vec<&str> = app.hits.paths.iter().map(|(_, _, _, p)| p.as_str()).collect();
    assert_eq!(paths, ["src/ui/view.rs", "README.md"]);
    let (row, from, _, _) = app.hits.paths[0].clone();
    app.mouse_down(from + 1, row, false);
    assert_eq!(app.mouse_up(from + 1, row), vec![Action::EditFile("src/ui/view.rs".to_owned())]);
}

#[test]
fn the_palette_lists_recent_commands_first() {
    use recode_tui::app::PaletteCommand;
    let mut app = finished();
    app.palette_recent = vec![PaletteCommand::Theme];
    let entries = app.palette_entries("");
    assert_eq!(entries[0].0, PaletteCommand::Theme);
    assert_ne!(app.palette_entries("he")[0].0, PaletteCommand::Theme, "a typed query ranks normally");
}

#[test]
fn nothing_panics_or_escapes_at_any_size() {
    for (name, mut app) in every_scene() {
        for (width, height) in sizes() {
            let result = std::panic::catch_unwind(std::panic::AssertUnwindSafe(|| {
                draw(&mut app, width, height);
            }));
            assert!(result.is_ok(), "{name} panicked at {width}x{height}");
        }
    }
}

#[test]
fn the_terminal_too_small_message_appears_only_when_needed() {
    let mut app = at_approval();
    assert!(screen(&draw(&mut app, 19, 10)).contains("Terminal"));
    assert!(screen(&draw(&mut app, 80, 3)).contains("Terminal too small"));
    assert!(!screen(&draw(&mut app, 20, 4)).contains("too small"));
}

#[test]
fn approval_keys_and_state_are_visible_at_every_usable_size() {
    let mut app = at_approval();
    for (width, height) in sizes().filter(|(w, h)| *w >= view::MIN_WIDTH && *h >= view::MIN_HEIGHT) {
        let text = screen(&draw(&mut app, width, height));
        let has_keys = text.contains("y approve") || text.contains("y/a/n/d") || text.contains("y/a/p/n/d");
        assert!(has_keys, "no approval keys at {width}x{height}:\n{text}");
        assert!(text.contains('▲'), "no state at {width}x{height}:\n{text}");
    }
}

#[test]
fn a_demo_session_is_always_labelled_where_it_fits() {
    let mut app = finished();
    for (width, height) in sizes().filter(|(w, h)| *w >= 30 && *h >= view::MIN_HEIGHT) {
        let text = screen(&draw(&mut app, width, height));
        assert!(text.contains("DEMO"), "unlabelled at {width}x{height}:\n{text}");
    }
}

#[test]
fn the_cursor_stays_inside_the_screen() {
    let mut app = finished();
    app.input.insert("some text 世界 that wraps\nand has lines\nand more lines\nand more\nand more");
    for (width, height) in sizes().filter(|(w, h)| *w >= view::MIN_WIDTH && *h >= view::MIN_HEIGHT) {
        let mut terminal = draw(&mut app, width, height);
        let position = terminal.get_cursor_position().unwrap();
        assert!(position.x < width && position.y < height, "cursor {position:?} at {width}x{height}");
    }
}

#[test]
fn input_survives_a_resize_exactly() {
    let mut app = finished();
    app.input.insert("keep this text");
    for (width, height) in [(100, 30), (20, 4), (5, 2), (1, 1), (200, 60), (40, 10)] {
        draw(&mut app, width, height);
        assert_eq!(app.input.text(), "keep this text");
    }
}

#[test]
fn the_side_panel_appears_only_on_wide_terminals_and_can_be_toggled() {
    let mut app = finished();
    assert!(!screen(&draw(&mut app, 100, 40)).contains("Files changed"));
    assert!(screen(&draw(&mut app, 140, 40)).contains("Files changed"));
    app.handle_key(KeyEvent::new(KeyCode::Char('b'), KeyModifiers::CONTROL));
    assert!(!screen(&draw(&mut app, 140, 40)).contains("Files changed"));
}

#[test]
fn estimates_are_marked_in_the_status_line() {
    let mut app = finished();
    assert!(screen(&draw(&mut app, 120, 40)).contains("~6.0k/32k"));
}

fn snapshot(name: &str, app: &mut App, width: u16, height: u16) {
    insta::assert_snapshot!(format!("{name}_{width}x{height}"), screen(&draw(app, width, height)));
}

#[test]
fn the_selected_model_and_how_to_start_are_visible_at_every_usable_size() {
    let mut app = App::new(Instant::now(), "p", "", "", false);
    app.open_welcome();
    app.set_welcome_info(sample_info());
    for (width, height) in sizes().filter(|(w, h)| *w >= view::MIN_WIDTH && *h >= view::MIN_HEIGHT) {
        let text = screen(&draw(&mut app, width, height));
        assert!(text.contains('❯'), "no selection at {width}x{height}:\n{text}");
        assert!(text.contains("enter"), "no start hint at {width}x{height}:\n{text}");
    }
}

#[test]
fn an_api_key_typed_into_the_provider_form_is_never_shown() {
    use crossterm::event::{KeyCode, KeyEvent, KeyModifiers};
    let mut app = home_on(HomeTab::Providers, sample_extensions());
    app.handle_key(KeyEvent::new(KeyCode::Char('a'), KeyModifiers::NONE));
    // Move to the API key field and type a secret.
    for _ in 0..3 {
        app.handle_key(KeyEvent::new(KeyCode::Tab, KeyModifiers::NONE));
    }
    for c in "sk-secret-value".chars() {
        app.handle_key(KeyEvent::new(KeyCode::Char(c), KeyModifiers::NONE));
    }
    for (width, height) in [(60, 20), (100, 30), (160, 45)] {
        let text = screen(&draw(&mut app, width, height));
        assert!(!text.contains("sk-secret-value"), "the key was shown at {width}x{height}:\n{text}");
    }
}

#[test]
fn secrets_never_appear_on_any_extensions_screen() {
    for (width, height) in [(30, 12), (60, 20), (100, 30), (160, 45)] {
        let mut app = home_on(HomeTab::Mcp, sample_extensions());
        if let Screen::Welcome(welcome) = &mut app.screen {
            welcome.set_extension_index(1);
        }
        let text = screen(&draw(&mut app, width, height));
        assert!(!text.contains("hunter2"), "a secret was shown at {width}x{height}:\n{text}");
    }
}

#[test]
fn every_extensions_tab_shows_its_keys_at_every_usable_size() {
    for tab in [HomeTab::Providers, HomeTab::Mcp, HomeTab::Skills, HomeTab::Tools] {
        let mut app = home_on(tab, sample_extensions());
        for (width, height) in sizes().filter(|(w, h)| *w >= view::MIN_WIDTH && *h >= view::MIN_HEIGHT) {
            let text = screen(&draw(&mut app, width, height));
            assert!(text.contains("a add"), "no keys on {tab:?} at {width}x{height}:\n{text}");
            assert!(text.contains('❯'), "no selection on {tab:?} at {width}x{height}:\n{text}");
        }
    }
}

#[test]
fn the_form_keeps_the_field_being_edited_and_its_save_key_visible() {
    let mut app = home_on(HomeTab::Mcp, sample_extensions());
    let mut form = FormState::new_for(HomeTab::Mcp);
    form.focus = 2;
    app.overlay = Some(Overlay::Form(form));
    for (width, height) in sizes().filter(|(w, h)| *w >= 30 && *h >= 8) {
        let text = screen(&draw(&mut app, width, height));
        assert!(text.contains("Environment"), "lost the field at {width}x{height}:\n{text}");
        assert!(text.contains("^S save"), "lost the key at {width}x{height}:\n{text}");
    }
}

#[test]
fn snapshots_of_the_extensions_tabs() {
    for (tab, name) in [
        (HomeTab::Providers, "providers"),
        (HomeTab::Mcp, "mcp"),
        (HomeTab::Skills, "skills"),
        (HomeTab::Tools, "tools"),
    ] {
        for (width, height) in [(60, 20), (120, 36)] {
            snapshot(name, &mut home_on(tab, sample_extensions()), width, height);
        }
    }
}

#[test]
fn snapshots_of_the_welcome_screen() {
    for (name, info) in welcome_scenes() {
        for (width, height) in [(30, 12), (60, 20), (100, 30), (160, 45)] {
            let mut app = App::new(Instant::now(), "api-server", "", "", false);
            app.reduced_motion = true;
            app.open_welcome();
            if let Some(info) = info.clone() {
                app.set_welcome_info(info);
            }
            let name = name.replace([',', ' '], "_");
            snapshot(&name, &mut app, width, height);
        }
    }
}

#[test]
fn snapshots_at_typical_sizes() {
    for (width, height) in [(30, 12), (60, 20), (100, 30), (160, 45)] {
        snapshot("approval", &mut at_approval(), width, height);
        snapshot("finished", &mut finished(), width, height);
    }
    for (width, height) in [(60, 20), (120, 36)] {
        snapshot("files_pane", &mut files_pane(), width, height);
        snapshot("runtimes_tab", &mut runtimes_tab(), width, height);
        snapshot("mcp_search", &mut mcp_search(), width, height);
        snapshot("skills_flagged", &mut home_on(HomeTab::Skills, flagged_skills()), width, height);
    }
}

/// Reports draw time on a large session. Run with:
/// `cargo test --release -p recode-tui --test any_size -- --ignored --nocapture frame_time`
#[test]
#[ignore = "a measurement, only meaningful in a release build"]
fn frame_time_on_a_long_session() {
    use recode_tui::transcript::Entry;
    let mut app = finished();
    for index in 0..3000 {
        app.transcript.push(Entry::User(format!("message {index}")));
        app.transcript.push(Entry::Assistant(
            "Some text that's long enough to wrap a few times at most sizes, so the session has plenty of lines.\n\n\
             - item one\n- item two\n"
                .to_owned(),
        ));
    }
    let mut terminal = Terminal::new(TestBackend::new(140, 45)).unwrap();
    let start = Instant::now();
    terminal.draw(|frame| view::draw(frame, &mut app, ColorMode::TrueColor)).unwrap();
    println!("first frame (renders everything): {:?}", start.elapsed());
    let lines = app.transcript.total_lines(app.view_size.0, app.options);
    let start = Instant::now();
    for _ in 0..200 {
        terminal.draw(|frame| view::draw(frame, &mut app, ColorMode::TrueColor)).unwrap();
    }
    println!("cached frame, {lines} lines: {:?}", start.elapsed() / 200);
    let start = Instant::now();
    for index in 0..20u16 {
        let mut resized = Terminal::new(TestBackend::new(100 + index, 45)).unwrap();
        resized.draw(|frame| view::draw(frame, &mut app, ColorMode::TrueColor)).unwrap();
    }
    println!("frame after a width change: {:?}", start.elapsed() / 20);
}
