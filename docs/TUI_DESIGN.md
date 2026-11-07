# TUI Design

Status: **proposed, awaiting agreement.**

## Design goals, in priority order

1. **Never lie, never leave the user guessing.** At any moment you can tell what is happening and why.
2. **Never block the keyboard.** Input and interrupt respond instantly, even during a heavy render.
3. **Calm by default, deep on demand.** Details are one key away, not on the screen all the time.
4. **Identity.** It should be recognisable from a screenshot.

## The main decision: full-screen or inline

| | Full-screen (alternate screen) | Inline (terminal scrollback) |
| --- | --- | --- |
| Native scrollback and search | No, we must build them | Yes |
| Native text selection | Conflicts with mouse capture | Yes |
| Panels, palette, overlays, session switching | Natural | Awkward, confined to the bottom region |
| Resize behaviour | Fully under our control | Terminal reflow can garble old output |
| Output after exit | Vanishes unless we print it | Stays in scrollback |

**Recommendation: full-screen.** The spec asks for multiple sessions, navigation, a palette,
inline diff review and configurable layouts, and those are full-screen features. The cost is real,
so these are requirements of the decision, not extras:

- Our own scrollback, with search, must be excellent and virtualised.
- Mouse selection is implemented by us (2026-11-07): the conversation from its own line model, so a drag
  can scroll beyond the view; everything else from the frame's cells, clipped to the region the drag began
  in. Copies go to the clipboard and the primary selection (`wl-copy`, `xclip`/`xsel`, tmux's buffer, OSC 52).
  F2 releases the mouse for native selection from any screen; `RE_CODE_DISABLE_MOUSE=1` starts released; the
  status line names the terminal's Shift/Option/Fn key while dragging. Ctrl+X opens transcript mode: search,
  `[` writes the conversation into the native scrollback, `v` opens it in the editor.
- On exit we print a brief summary and the exact command to resume the session.
- Headless mode covers everything that wants plain output.

## Layout

The conversation is always the hero. Panels appear only when there is room and a reason.

```text
 ◆ re-code   api-server ▸ fix auth retry                                       2 sessions
────────────────────────────────────────────────────────────────────────────────────────
  you  The retry loop in auth.rs never backs off. Fix it and run the tests.

  ◇ The delay is computed but never applied. I'll fix the loop and run the tests.

  ✓ read    src/auth.rs                                               214 lines   12ms
  ✓ grep    "retry_delay"                                              3 matches   4ms
  ┌ edit    src/auth.rs ────────────────────────────────────────── needs approval ─┐
  │  41    for attempt in 0..MAX_RETRIES {                                          │
  │  42 -      let _delay = retry_delay(attempt);                                   │
  │  42 +      tokio::time::sleep(retry_delay(attempt)).await;                      │
  │                                                                                 │
  │  y approve   a always for this session   n deny   e edit   d full diff          │
  └─────────────────────────────────────────────────────────────────────────────────┘

────────────────────────────────────────────────────────────────────────────────────────
 ❯ _
────────────────────────────────────────────────────────────────────────────────────────
 ⏸ awaiting approval │ qwen3/4b · Ollama │ ctx ▓▓▓░░░░░░░ ~9.1k/32k │ 31 tok/s │ esc stop
```

(Illustrative mockup. The figures in it are not measurements.)

The status line is the one thing that is always visible. It answers the spec's list of questions
(model, provider, state, context, whether approval is needed).

### Resizing is a hard requirement

**Developer requirement (2026-11-06): the program always fits the terminal and looks good at whatever
size it is resized to.** Other features give way to this one, and every view must meet the rules below.

| Width | Behaviour |
| --- | --- |
| Under 40 columns | Conversation, input and a one-segment status (state only) |
| 40–59 | Single column, shortened status, icons without labels |
| 60–119 | Layout as above. Panels open as overlays. |
| 120 or more | Optional right panel for files changed, task list, or context breakdown |
| Above about 160 | Conversation text stops growing at a readable measure and centres. Wide diffs and code can use the full width. |

| Height | Behaviour |
| --- | --- |
| Under 24 rows | Header disappears first |
| Under 12 rows | Rules between regions disappear. Input is limited to 3 lines. |
| Under 6 rows | Conversation tail, one input line and status only |
| Very small (under about 20×4) | A plain "Terminal too small" message. Nothing panics, and work continues underneath. |

Rules, enforced by tests:

1. **Nothing overflows, wraps badly or panics at any size,** including 1×1. A property test renders every
   view across a sweep of sizes and asserts this.
2. **Status segments have a priority order.** They drop in order, lowest first, rather than being clipped:
   state and interrupt hint, then context, model, provider, then speed. An approval indicator is never dropped.
3. **Resizing keeps your place.** The message at the top of the view stays there after re-wrapping, and
   following the tail stays on.
4. **Input is never lost.** Text and cursor position survive any resize, and the input box scrolls internally.
5. **Approval cards remain actionable at any size.** The keys stay visible, and the diff scrolls within the card.
6. **Tables and diffs have narrow forms.** Tables reflow to stacked rows, and diffs go from side by side to
   unified, with long lines wrapping at a visible marker. Code blocks scroll sideways rather than wrapping when
   the user prefers.
7. **Width means display cells.** All measurement uses Unicode display width, so CJK and emoji can't break a layout.
8. **Overlays adapt.** A palette is a centred box when there's room, and takes the full screen when there isn't.
9. **Resizing is smooth.** Bursts of resize events are coalesced, each cached message re-wraps lazily, and only the
   visible lines are computed. The app redraws cleanly, and a drag on a huge session stays responsive.

## Agent states

Local models spend long stretches silent, and this is exactly where other tools feel broken. We
separate these states so the user is never staring at an unexplained spinner:

| State | Display | Source |
| --- | --- | --- |
| Loading model | `Loading qwen3/4b into memory… 12s` | Time to first byte with an unloaded model |
| Evaluating prompt | `Reading context, 8.2k tokens…` | Expected when many tokens are uncached |
| Thinking | Collapsed, with an elapsed time. Expands on key press. | Reasoning stream |
| Responding | Streaming text with a live tokens-per-second figure | Text stream |
| Running tool | Timeline row with its live output tail | Tool events |
| Awaiting approval | Accent colour, status line, and an optional terminal bell | Permission engine |
| Queued | `Waiting for Ollama. Another request is running.` | Inferred, and labelled as such |
| Stalled | `No output for 90s. Keep waiting, or esc to stop?` | Watchdog |
| Interrupted, failed, done | Plain final state, with the reason on failure | Terminal events |

Measured values and estimates look different everywhere. Estimates carry a `~` (`~9.1k`), and
provider-reported figures do not.

## Conversation rendering

- **Markdown:** parsed once a block is complete. Only the block still streaming is re-parsed, so
  long messages stay cheap.
- **Code:** syntax highlighted with a language tag and a copy key.
- **Tool timeline:** one line each when finished, expanding on demand. Consecutive reads and searches
  collapse into a group (`✓ read 6 files`).
- **Diffs:** inline with word-level highlights, and a full-screen review mode for large changes.
- **Long output:** tail shown, with the complete text available in a pager.

## Keyboard model

This table is the plan. The interface currently implements everything except `Ctrl+P`, `Ctrl+O` as a session switcher
(it opens the last tool output for now), `Ctrl+F` and `Ctrl+Z`. `Ctrl+E` opens MCP servers, skills and tools, and `Tab` moves between tabs on the home screen.

| Key | Action |
| --- | --- |
| `Enter` | Send |
| `Alt+Enter`, `Ctrl+J` | New line |
| `Esc` | Interrupt the agent, or close the top overlay |
| `Ctrl+K` | Command palette |
| `Ctrl+P` | File picker, inserts an `@file` reference |
| `Ctrl+O` | Session switcher |
| `Ctrl+L` | Model switcher |
| `Ctrl+F` | Search the conversation |
| `PgUp` / `PgDn`, `Home` / `End` | Scroll, `End` resumes following output |
| `Ctrl+Z` | Undo the last agent turn's file changes (with confirmation) |
| `Ctrl+C` | Clear input. A second press exits. Never kills the session silently. |
| `/` at line start | Slash commands, the same list as the palette |

**Terminal limits shape this table.** `Shift+Enter` can't be our only newline key:
the Kitty keyboard protocol is what makes it detectable, and a KDE bug report from November 2025
says Konsole doesn't support that protocol. We'll enable the protocol where available and treat
`Alt+Enter` as the guaranteed path. Everything is rebindable.

## Rendering architecture

- Dedicated input task. Engine events and input go through channels to a single UI state owner.
- **Frames are event-driven,** not on a timer, and capped near 60 fps. Stream deltas are coalesced
  so a fast model can't saturate the terminal.
- **Virtualised scrollback:** each message caches its wrapped lines per width. Only visible lines are
  drawn, so a 10,000-line session costs the same as a short one.
- Synchronised output (mode 2026) is used when the terminal reports support, and it is never relied
  upon. Support in Konsole is reported inconsistently, so we'll test it directly.
- All model and tool output goes through a **sanitiser that strips terminal escape sequences** before
  rendering. See [SECURITY](SECURITY.md).

**Performance targets, to be measured (these are targets, not results):** keypress to screen under
16 ms; frame under 8 ms in a 10,000-line session; idle CPU effectively zero, with no timer redraws
when nothing is animating.

## Terminal compatibility

The test matrix is **Konsole, the RustRover (JediTerm) terminal, kitty, and tmux.** All were observed
present on this machine, and you're in the JetBrains terminal right now, so it is a first-class
target rather than an afterthought.

- **Colour:** truecolor, falling back to 256, and honouring `NO_COLOR`.
- **Glyphs:** the default set uses common Unicode only (`✓ ◆ ▓ ❯`). Nerd Font icons are opt-in, since
  a terminal app can't reliably detect installed fonts.
- **Motion:** spinners respect a reduced-motion setting.

## Visual identity

The look follows driftwave, the developer's own ratatui music player: rounded panels with titles set into the border, orange for focus, solid tabs with counts, black-on-colour chips, a legend that always shows the keys, and an orange, pink and violet gradient.

- **Every animated or graphed thing shows real data.** The "Model output" graph is real output per 100 ms, and it stays flat during silence. Hardware meters are real figures. Nothing moves just to look busy.
- **Amber means "needs you"** when it's on an approval, and also marks small section labels, following driftwave. Approval cards keep a full amber border, which nothing else has.
- **Colours are tokens** in `theme.rs`. Only the dark palette exists so far, so a light terminal may look wrong.

Screens can be checked visually by capturing a `tmux` pane with `capture-pane -e` and rendering it with `scripts/screenshots/render.py`.

## Onboarding

First launch is a short guided flow, and every step can be skipped:

1. Detect hardware and existing runtimes. If Ollama is installed but stopped, say so and offer to
   start it, naming exactly what that will do.
2. Recommend a model in plain language, or suggest an installed one.
3. Explain permissions in two sentences.
4. Drop into a working session, with an example prompt that suits the current directory.

The goal is under a minute from launch to a useful answer, for a machine that already has a model.

## Quality bar

Before Milestone 1 is called done, we record real terminal sessions and critique them. Screenshots
that merely look good aren't enough. Input latency, resize behaviour and interrupt speed get tested
in each terminal in the matrix.
