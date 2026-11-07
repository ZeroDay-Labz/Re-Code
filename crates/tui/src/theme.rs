//! Colours and themes. Drawing code asks for a [`Token`] (`theme::color(Token::Accent)`), and the
//! active theme decides what that is. Amber ([`Token::Attention`]) is reserved for "needs you".
//!
//! A theme is a [`Palette`]: fourteen token colours plus a few looks (its logo gradient, border
//! style, density, code colours). Choosing one, a [`Variant`] (dim, normal, bright) and an optional
//! accent gives a [`Resolved`] theme, kept in a lock that every frame reads from.

use ratatui::buffer::Buffer;
use ratatui::style::{Color, Modifier, Style};
use std::borrow::Cow;
use std::sync::RwLock;

const fn rgb(r: u8, g: u8, b: u8) -> Color {
    Color::Rgb(r, g, b)
}

/// The things drawing code colours. The order is the order of a palette's `colors`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Token {
    Accent,
    Attention,
    Pink,
    Violet,
    Sky,
    Success,
    Danger,
    Text,
    Muted,
    Faint,
    UserBg,
    CodeBg,
    AddBg,
    RemoveBg,
}

impl Token {
    pub const ALL: [Token; 14] = [
        Token::Accent,
        Token::Attention,
        Token::Pink,
        Token::Violet,
        Token::Sky,
        Token::Success,
        Token::Danger,
        Token::Text,
        Token::Muted,
        Token::Faint,
        Token::UserBg,
        Token::CodeBg,
        Token::AddBg,
        Token::RemoveBg,
    ];

    /// The token's name in a theme file.
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Accent => "accent",
            Self::Attention => "attention",
            Self::Pink => "pink",
            Self::Violet => "violet",
            Self::Sky => "sky",
            Self::Success => "success",
            Self::Danger => "danger",
            Self::Text => "text",
            Self::Muted => "muted",
            Self::Faint => "faint",
            Self::UserBg => "user_bg",
            Self::CodeBg => "code_bg",
            Self::AddBg => "add_bg",
            Self::RemoveBg => "remove_bg",
        }
    }

    /// Backgrounds are shifted differently from text and marks by a variant.
    fn is_background(self) -> bool {
        matches!(self, Self::UserBg | Self::CodeBg | Self::AddBg | Self::RemoveBg)
    }
}

/// re-code's own colours, as drawn in the welcome logo: orange, through pink, to violet.
pub const DRIFTWAVE: [Color; 14] = [
    rgb(255, 85, 0),
    rgb(255, 160, 60),
    rgb(255, 50, 130),
    rgb(150, 90, 255),
    rgb(110, 190, 255),
    rgb(150, 220, 110),
    rgb(255, 90, 90),
    rgb(225, 225, 232),
    rgb(125, 125, 142),
    rgb(60, 60, 72),
    rgb(48, 34, 52),
    rgb(30, 28, 40),
    rgb(28, 50, 36),
    rgb(60, 28, 36),
];

/// How borders are drawn.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Borders {
    #[default]
    Rounded,
    Square,
}

/// A theme.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Palette {
    pub name: Cow<'static, str>,
    /// Made for a light terminal background.
    pub light: bool,
    /// The syntect theme for code, or "derived" to build one from the tokens.
    pub syntax: Cow<'static, str>,
    /// The background painted behind everything when "solid background" is on.
    pub base: Color,
    /// One colour per [`Token`], in `Token::ALL` order.
    pub colors: [Color; 14],
    /// The welcome logo's gradient stops; accent → pink → violet when unset.
    pub logo: Option<[Color; 3]>,
    pub borders: Borders,
    /// Less space between things.
    pub compact: bool,
}

impl Palette {
    const fn builtin(name: &'static str, light: bool, syntax: &'static str, base: Color, colors: [Color; 14]) -> Self {
        Self {
            name: Cow::Borrowed(name),
            light,
            syntax: Cow::Borrowed(syntax),
            base,
            colors,
            logo: None,
            borders: Borders::Rounded,
            compact: false,
        }
    }
}

pub const THEMES: &[Palette] = &[
    Palette::builtin("Driftwave", false, "base16-ocean.dark", rgb(16, 14, 22), DRIFTWAVE),
    Palette::builtin(
        "Driftwave Light",
        true,
        "InspiredGitHub",
        rgb(250, 250, 252),
        [
            rgb(225, 75, 0),
            rgb(185, 100, 0),
            rgb(255, 50, 130),
            rgb(150, 90, 255),
            rgb(20, 110, 200),
            rgb(40, 140, 50),
            rgb(200, 40, 40),
            rgb(30, 30, 38),
            rgb(95, 95, 110),
            rgb(165, 165, 178),
            rgb(246, 236, 246),
            rgb(243, 243, 247),
            rgb(222, 245, 226),
            rgb(251, 226, 230),
        ],
    ),
    Palette::builtin(
        "Catppuccin Mocha",
        false,
        "base16-mocha.dark",
        rgb(30, 30, 46),
        [
            rgb(250, 179, 135),
            rgb(249, 226, 175),
            rgb(245, 194, 231),
            rgb(203, 166, 247),
            rgb(137, 220, 235),
            rgb(166, 227, 161),
            rgb(243, 139, 168),
            rgb(205, 214, 244),
            rgb(127, 132, 156),
            rgb(88, 91, 112),
            rgb(49, 50, 68),
            rgb(24, 24, 37),
            rgb(36, 52, 44),
            rgb(60, 38, 50),
        ],
    ),
    Palette::builtin(
        "Catppuccin Latte",
        true,
        "InspiredGitHub",
        rgb(239, 241, 245),
        [
            rgb(254, 100, 11),
            rgb(223, 142, 29),
            rgb(234, 118, 203),
            rgb(136, 57, 239),
            rgb(4, 165, 229),
            rgb(64, 160, 43),
            rgb(210, 15, 57),
            rgb(76, 79, 105),
            rgb(140, 143, 161),
            rgb(172, 176, 190),
            rgb(230, 233, 239),
            rgb(220, 224, 232),
            rgb(214, 240, 214),
            rgb(244, 214, 220),
        ],
    ),
    Palette::builtin(
        "Tokyo Night",
        false,
        "base16-ocean.dark",
        rgb(26, 27, 38),
        [
            rgb(255, 158, 100),
            rgb(224, 175, 104),
            rgb(187, 154, 247),
            rgb(157, 124, 216),
            rgb(122, 162, 247),
            rgb(158, 206, 106),
            rgb(247, 118, 142),
            rgb(192, 202, 245),
            rgb(86, 95, 137),
            rgb(59, 66, 97),
            rgb(41, 46, 66),
            rgb(22, 22, 30),
            rgb(32, 48, 59),
            rgb(55, 34, 44),
        ],
    ),
    Palette::builtin(
        "Gruvbox",
        false,
        "base16-eighties.dark",
        rgb(40, 40, 40),
        [
            rgb(254, 128, 25),
            rgb(250, 189, 47),
            rgb(211, 134, 155),
            rgb(177, 98, 134),
            rgb(131, 165, 152),
            rgb(184, 187, 38),
            rgb(251, 73, 52),
            rgb(235, 219, 178),
            rgb(146, 131, 116),
            rgb(80, 73, 69),
            rgb(60, 56, 54),
            rgb(29, 32, 33),
            rgb(50, 54, 26),
            rgb(60, 31, 30),
        ],
    ),
    Palette::builtin(
        "Nord",
        false,
        "base16-ocean.dark",
        rgb(46, 52, 64),
        [
            rgb(208, 135, 112),
            rgb(235, 203, 139),
            rgb(180, 142, 173),
            rgb(129, 161, 193),
            rgb(136, 192, 208),
            rgb(163, 190, 140),
            rgb(191, 97, 106),
            rgb(236, 239, 244),
            rgb(140, 150, 168),
            rgb(76, 86, 106),
            rgb(59, 66, 82),
            rgb(39, 44, 54),
            rgb(47, 59, 51),
            rgb(63, 46, 53),
        ],
    ),
    Palette::builtin(
        "Dracula",
        false,
        "base16-ocean.dark",
        rgb(40, 42, 54),
        [
            rgb(255, 184, 108),
            rgb(241, 250, 140),
            rgb(255, 121, 198),
            rgb(189, 147, 249),
            rgb(139, 233, 253),
            rgb(80, 250, 123),
            rgb(255, 85, 85),
            rgb(248, 248, 242),
            rgb(98, 114, 164),
            rgb(68, 71, 90),
            rgb(52, 55, 70),
            rgb(33, 34, 44),
            rgb(38, 55, 44),
            rgb(61, 37, 48),
        ],
    ),
    Palette::builtin(
        "Rosé Pine",
        false,
        "base16-ocean.dark",
        rgb(25, 23, 36),
        [
            rgb(235, 188, 186),
            rgb(246, 193, 119),
            rgb(235, 111, 146),
            rgb(196, 167, 231),
            rgb(156, 207, 216),
            rgb(86, 148, 159),
            rgb(235, 111, 146),
            rgb(224, 222, 244),
            rgb(144, 140, 170),
            rgb(64, 61, 82),
            rgb(38, 35, 58),
            rgb(31, 29, 46),
            rgb(35, 50, 52),
            rgb(55, 33, 46),
        ],
    ),
    Palette::builtin(
        "Solarized Dark",
        false,
        "Solarized (dark)",
        rgb(0, 43, 54),
        [
            rgb(203, 75, 22),
            rgb(181, 137, 0),
            rgb(211, 54, 130),
            rgb(108, 113, 196),
            rgb(38, 139, 210),
            rgb(133, 153, 0),
            rgb(220, 50, 47),
            rgb(147, 161, 161),
            rgb(88, 110, 117),
            rgb(40, 75, 85),
            rgb(7, 54, 66),
            rgb(0, 36, 46),
            rgb(20, 60, 40),
            rgb(70, 30, 35),
        ],
    ),
    Palette::builtin(
        "Solarized Light",
        true,
        "Solarized (light)",
        rgb(253, 246, 227),
        [
            rgb(203, 75, 22),
            rgb(181, 137, 0),
            rgb(211, 54, 130),
            rgb(108, 113, 196),
            rgb(38, 139, 210),
            rgb(133, 153, 0),
            rgb(220, 50, 47),
            rgb(7, 54, 66),
            rgb(101, 123, 131),
            rgb(196, 201, 184),
            rgb(238, 232, 213),
            rgb(245, 239, 220),
            rgb(227, 236, 200),
            rgb(245, 217, 208),
        ],
    ),
];

/// A theme shifted darker or brighter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Variant {
    Dim,
    #[default]
    Normal,
    Bright,
}

impl Variant {
    pub const ALL: [Variant; 3] = [Variant::Dim, Variant::Normal, Variant::Bright];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Dim => "dim",
            Self::Normal => "normal",
            Self::Bright => "bright",
        }
    }

    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|variant| variant.label() == text.trim().to_ascii_lowercase())
    }

    #[must_use]
    pub fn next(self) -> Self {
        match self {
            Self::Dim => Self::Normal,
            Self::Normal => Self::Bright,
            Self::Bright => Self::Dim,
        }
    }

    #[must_use]
    pub fn previous(self) -> Self {
        self.next().next()
    }
}

/// What the user chose: a theme by name, a variant, maybe their own accent, and whether the theme
/// paints its own background.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ThemeSelection {
    pub name: String,
    pub variant: Variant,
    pub accent: Option<Color>,
    pub solid: bool,
}

impl Default for ThemeSelection {
    fn default() -> Self {
        Self { name: "Driftwave".to_owned(), variant: Variant::Normal, accent: None, solid: false }
    }
}

impl ThemeSelection {
    #[must_use]
    pub fn named(name: &str) -> Self {
        Self { name: name.to_owned(), ..Self::default() }
    }

    /// `nord`, `nord:bright`, `nord+#ff8800` or `nord:dim+#ff8800`, as `config.toml` takes it.
    pub fn parse(text: &str) -> Result<Self, String> {
        let (rest, accent) = match text.split_once('+') {
            Some((rest, hex)) => {
                (rest, Some(parse_hex(hex).ok_or_else(|| format!("{hex} isn't a colour like #ff8800"))?))
            }
            None => (text, None),
        };
        let (name, variant) = match rest.split_once(':') {
            Some((name, variant)) => {
                (name, Variant::parse(variant).ok_or_else(|| format!("{variant} isn't dim, normal or bright"))?)
            }
            None => (rest, Variant::Normal),
        };
        let name = find_theme(name)
            .ok_or_else(|| format!("there's no theme called \"{name}\". Themes: {}.", names().join(", ")))?;
        Ok(Self { name, variant, accent, solid: false })
    }
}

/// A theme as drawn: the selection applied to its palette.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct Resolved {
    pub colors: [Color; 14],
    pub base: Color,
    pub light: bool,
    pub borders: Borders,
    pub compact: bool,
    pub logo: [Color; 3],
    pub syntax: &'static str,
    pub solid: bool,
}

impl Resolved {
    fn from(palette: &Palette, selection: &ThemeSelection) -> Self {
        let mut colors = palette.colors;
        for (index, token) in Token::ALL.iter().enumerate() {
            colors[index] = shift(colors[index], *token, selection.variant, palette.base, palette.light);
        }
        if let Some(accent) = selection.accent {
            colors[0] = accent;
        }
        let logo = palette.logo.unwrap_or([colors[0], colors[2], colors[3]]);
        Self {
            colors,
            base: palette.base,
            light: palette.light,
            borders: palette.borders,
            compact: palette.compact,
            logo,
            syntax: leak(&palette.syntax),
            solid: selection.solid,
        }
    }
}

/// Syntax theme names are few and small; a user theme's lives for the run.
fn leak(name: &str) -> &'static str {
    for theme in THEMES {
        if theme.syntax == name
            && let Cow::Borrowed(fixed) = theme.syntax
        {
            return fixed;
        }
    }
    Box::leak(name.to_owned().into_boxed_str())
}

/// A variant moves text and marks toward the background (dim) or away from it (bright), and
/// backgrounds flatter or more distinct, so any palette gets the three looks without extra work.
fn shift(color: Color, token: Token, variant: Variant, base: Color, light: bool) -> Color {
    let extreme = if light { rgb(0, 0, 0) } else { rgb(255, 255, 255) };
    match (variant, token.is_background()) {
        (Variant::Normal, _) => color,
        (Variant::Dim, false) => lerp(color, base, 0.18),
        (Variant::Dim, true) => lerp(color, base, 0.35),
        (Variant::Bright, false) => lerp(color, extreme, 0.15),
        (Variant::Bright, true) => lerp(color, extreme, 0.12),
    }
}

static ACTIVE: RwLock<Option<Resolved>> = RwLock::new(None);
static USER_THEMES: RwLock<Vec<Palette>> = RwLock::new(Vec::new());

/// The theme in use.
#[must_use]
pub fn resolved() -> Resolved {
    if let Some(resolved) = *ACTIVE.read().unwrap_or_else(std::sync::PoisonError::into_inner) {
        return resolved;
    }
    Resolved::from(&THEMES[0], &ThemeSelection::default())
}

/// A token's colour in the theme in use.
#[must_use]
pub fn color(token: Token) -> Color {
    resolved().colors[token as usize]
}

#[must_use]
pub fn fg(token: Token) -> Style {
    Style::new().fg(color(token))
}

#[must_use]
pub fn bg(token: Token) -> Style {
    Style::new().bg(color(token))
}

/// Every theme: the built-in ones, then the user's (a user theme named like a built-in replaces it).
#[must_use]
pub fn themes() -> Vec<Palette> {
    let user = USER_THEMES.read().unwrap_or_else(std::sync::PoisonError::into_inner);
    merged(&user)
}

/// The built-in themes with the user's added, a user theme replacing a built-in of the same name.
#[must_use]
pub fn merged(user: &[Palette]) -> Vec<Palette> {
    let mut all: Vec<Palette> =
        THEMES.iter().filter(|theme| !user.iter().any(|mine| mine.name == theme.name)).cloned().collect();
    all.extend(user.iter().cloned());
    all
}

#[must_use]
pub fn names() -> Vec<String> {
    themes().into_iter().map(|theme| theme.name.into_owned()).collect()
}

/// Replaces the user's themes (from their theme files).
pub fn install_user_themes(themes: Vec<Palette>) {
    *USER_THEMES.write().unwrap_or_else(std::sync::PoisonError::into_inner) = themes;
}

/// A theme's canonical name, found ignoring case, spaces, hyphens and accents ("rose-pine" finds "Rosé Pine").
#[must_use]
pub fn find_theme(name: &str) -> Option<String> {
    let key = |text: &str| -> String {
        text.chars()
            .map(|c| if c == 'é' { 'e' } else { c.to_ascii_lowercase() })
            .filter(char::is_ascii_alphanumeric)
            .collect()
    };
    let wanted = key(name);
    themes().into_iter().find(|theme| key(&theme.name) == wanted).map(|theme| theme.name.into_owned())
}

/// The theme a selection resolves to, or `None` if no theme has that name.
#[must_use]
pub fn resolve(selection: &ThemeSelection) -> Option<Resolved> {
    let palette = themes().into_iter().find(|theme| theme.name == selection.name)?;
    Some(Resolved::from(&palette, selection))
}

/// Makes a selection the theme in use. Returns whether anything changed; an unknown name changes nothing.
pub fn activate(selection: &ThemeSelection) -> bool {
    let Some(next) = resolve(selection) else { return false };
    let mut active = ACTIVE.write().unwrap_or_else(std::sync::PoisonError::into_inner);
    if *active == Some(next) {
        return false;
    }
    *active = Some(next);
    true
}

/// The default dark or light theme, for a detected terminal background.
pub fn set_light(light: bool) {
    activate(&ThemeSelection::named(if light { "Driftwave Light" } else { "Driftwave" }));
}

#[must_use]
pub fn is_light() -> bool {
    resolved().light
}

/// Black or white, whichever reads on this background (for labels drawn on a coloured chip).
#[must_use]
pub fn contrast_for(background: Color) -> Color {
    if luminance(background) > 0.55 { rgb(10, 10, 14) } else { rgb(250, 250, 250) }
}

fn luminance(color: Color) -> f32 {
    match color {
        Color::Rgb(r, g, b) => (0.2126 * f32::from(r) + 0.7152 * f32::from(g) + 0.0722 * f32::from(b)) / 255.0,
        _ => 0.0,
    }
}

/// `#rrggbb` to a colour.
#[must_use]
pub fn parse_hex(text: &str) -> Option<Color> {
    let hex = text.trim().trim_start_matches('#');
    if hex.len() != 6 || !hex.chars().all(|c| c.is_ascii_hexdigit()) {
        return None;
    }
    let channel = |at: usize| u8::from_str_radix(&hex[at..at + 2], 16).ok();
    Some(Color::Rgb(channel(0)?, channel(2)?, channel(4)?))
}

/// A colour as `#rrggbb` (other kinds of colour have no such form).
#[must_use]
pub fn hex(color: Color) -> Option<String> {
    match color {
        Color::Rgb(r, g, b) => Some(format!("#{r:02x}{g:02x}{b:02x}")),
        _ => None,
    }
}

/// Blends two colours.
#[must_use]
pub fn lerp(from: Color, to: Color, amount: f32) -> Color {
    let (Color::Rgb(r1, g1, b1), Color::Rgb(r2, g2, b2)) = (from, to) else { return from };
    let mix = |a: u8, b: u8| {
        let value = f32::from(a) + (f32::from(b) - f32::from(a)) * amount.clamp(0.0, 1.0);
        // The clamp above keeps this within the range of a u8.
        #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
        let value = value.round() as u8;
        value
    };
    Color::Rgb(mix(r1, r2), mix(g1, g2), mix(b1, b2))
}

/// The theme's logo gradient: its first stop, through the second, to the third.
#[must_use]
pub fn gradient(position: f32) -> Color {
    let [first, second, third] = resolved().logo;
    if position < 0.5 { lerp(first, second, position * 2.0) } else { lerp(second, third, (position - 0.5) * 2.0) }
}

#[must_use]
pub fn muted() -> Style {
    fg(Token::Muted)
}

#[must_use]
pub fn faint() -> Style {
    fg(Token::Faint)
}

#[must_use]
pub fn accent() -> Style {
    fg(Token::Accent)
}

#[must_use]
pub fn attention() -> Style {
    fg(Token::Attention).add_modifier(Modifier::BOLD)
}

/// The corner glyphs for the theme's border style: (top-left, top-right, bottom-left, bottom-right).
#[must_use]
pub fn corners() -> (&'static str, &'static str, &'static str, &'static str) {
    match resolved().borders {
        Borders::Rounded => ("╭", "╮", "╰", "╯"),
        Borders::Square => ("┌", "┐", "└", "┘"),
    }
}

/// What the terminal can display. Applied to each finished frame, so widgets don't need to care.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ColorMode {
    TrueColor,
    Indexed,
    /// `NO_COLOR` is set. Bold and similar attributes remain.
    None,
}

impl ColorMode {
    #[must_use]
    pub fn detect() -> Self {
        let var = |name: &str| std::env::var(name).unwrap_or_default();
        if !var("NO_COLOR").is_empty() {
            Self::None
        } else if matches!(var("COLORTERM").as_str(), "truecolor" | "24bit") {
            Self::TrueColor
        } else {
            Self::Indexed
        }
    }

    /// Converts a rendered frame to what this terminal can show, painting the theme's background
    /// behind everything when the theme is set to.
    pub fn apply(self, buffer: &mut Buffer) {
        self.apply_with(buffer, &resolved());
    }

    /// [`Self::apply`] for a given theme.
    pub fn apply_with(self, buffer: &mut Buffer, theme: &Resolved) {
        if self == Self::TrueColor && !theme.solid {
            return;
        }
        let text = theme.colors[Token::Text as usize];
        for cell in &mut buffer.content {
            let (mut fg, mut bg) = (cell.fg, cell.bg);
            if theme.solid {
                if bg == Color::Reset {
                    bg = theme.base;
                }
                if fg == Color::Reset {
                    fg = text;
                }
            }
            cell.fg = self.convert(fg);
            cell.bg = self.convert(bg);
        }
    }

    fn convert(self, color: Color) -> Color {
        match (self, color) {
            (Self::None, _) => Color::Reset,
            (Self::Indexed, Color::Rgb(r, g, b)) => Color::Indexed(nearest_256(r, g, b)),
            (_, other) => other,
        }
    }
}

/// Finds the nearest entry in the xterm 256-colour cube or grey ramp.
fn nearest_256(r: u8, g: u8, b: u8) -> u8 {
    const LEVELS: [i32; 6] = [0, 95, 135, 175, 215, 255];
    let level = |value: u8| {
        (0..6).min_by_key(|&index| (LEVELS[index] - i32::from(value)).abs()).expect("the range is not empty")
    };
    let (ri, gi, bi) = (level(r), level(g), level(b));
    let distance =
        |x: i32, y: i32, z: i32| (x - i32::from(r)).pow(2) + (y - i32::from(g)).pow(2) + (z - i32::from(b)).pow(2);
    let cube = distance(LEVELS[ri], LEVELS[gi], LEVELS[bi]);
    let average = (i32::from(r) + i32::from(g) + i32::from(b)) / 3;
    let grey_index = ((average - 8 + 5) / 10).clamp(0, 23);
    let grey = 8 + grey_index * 10;
    if distance(grey, grey, grey) < cube {
        u8::try_from(232 + grey_index).expect("grey ramp index fits in u8")
    } else {
        u8::try_from(16 + 36 * ri + 6 * gi + bi).expect("colour cube index fits in u8")
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn brightness(color: Color) -> u32 {
        match color {
            Color::Rgb(r, g, b) => (u32::from(r) * 299 + u32::from(g) * 587 + u32::from(b) * 114) / 1000,
            _ => 0,
        }
    }

    #[test]
    fn a_selection_resolves_to_its_palette_variant_and_accent() {
        let mocha = resolve(&ThemeSelection::named("Catppuccin Mocha")).unwrap();
        let palette = THEMES.iter().find(|t| t.name == "Catppuccin Mocha").unwrap();
        assert_eq!(mocha.colors, palette.colors, "normal is the palette as written");
        assert_eq!(mocha.logo, [palette.colors[0], palette.colors[2], palette.colors[3]]);

        let dim =
            resolve(&ThemeSelection { variant: Variant::Dim, ..ThemeSelection::named("Catppuccin Mocha") }).unwrap();
        let bright =
            resolve(&ThemeSelection { variant: Variant::Bright, ..ThemeSelection::named("Catppuccin Mocha") }).unwrap();
        let text = Token::Text as usize;
        assert!(brightness(dim.colors[text]) < brightness(mocha.colors[text]));
        assert!(brightness(bright.colors[text]) > brightness(mocha.colors[text]));

        let orange = parse_hex("#ff8800").unwrap();
        let custom = resolve(&ThemeSelection { accent: Some(orange), ..ThemeSelection::named("Nord") }).unwrap();
        assert_eq!(custom.colors[0], orange);
        assert_eq!(custom.logo[0], orange, "the logo follows the accent");
        assert!(resolve(&ThemeSelection::named("No Such Theme")).is_none());
    }

    #[test]
    fn every_theme_and_variant_stays_readable() {
        for theme in THEMES {
            for variant in Variant::ALL {
                let resolved = resolve(&ThemeSelection { variant, ..ThemeSelection::named(&theme.name) }).unwrap();
                let (text, muted) = (resolved.colors[Token::Text as usize], resolved.colors[Token::Muted as usize]);
                let contrast = brightness(text).abs_diff(brightness(resolved.base));
                assert!(contrast > 80, "{} {}: text must stand out ({contrast})", theme.name, variant.label());
                assert!(
                    brightness(muted).abs_diff(brightness(resolved.base)) > 35,
                    "{} {}: muted text",
                    theme.name,
                    variant.label()
                );
            }
            assert_eq!(theme.light, brightness(theme.base) > 128, "{}: light flag", theme.name);
            assert!(
                syntect::highlighting::ThemeSet::load_defaults().themes.contains_key(theme.syntax.as_ref()),
                "{}",
                theme.syntax
            );
        }
    }

    #[test]
    fn names_hex_and_the_config_form_round_trip() {
        assert_eq!(find_theme("rose-pine").as_deref(), Some("Rosé Pine"));
        assert_eq!(find_theme("CATPPUCCIN mocha").as_deref(), Some("Catppuccin Mocha"));
        assert_eq!(find_theme("nope"), None);
        assert_eq!(hex(parse_hex("#1A2b3C").unwrap()).as_deref(), Some("#1a2b3c"));
        assert_eq!(parse_hex("#12345"), None);
        let parsed = ThemeSelection::parse("nord:bright+#ff8800").unwrap();
        assert_eq!(
            (parsed.name.as_str(), parsed.variant, parsed.accent),
            ("Nord", Variant::Bright, parse_hex("#ff8800"))
        );
        assert!(ThemeSelection::parse("nord:loud").unwrap_err().contains("dim, normal or bright"));
        assert!(ThemeSelection::parse("purple").unwrap_err().contains("Gruvbox"));
    }

    #[test]
    fn chips_get_readable_text_on_any_background() {
        assert_eq!(contrast_for(rgb(255, 255, 255)), rgb(10, 10, 14));
        assert_eq!(contrast_for(rgb(20, 20, 30)), rgb(250, 250, 250));
    }

    #[test]
    fn a_solid_background_fills_the_terminal_default_and_a_plain_frame_is_untouched() {
        use ratatui::layout::Rect;
        let theme = resolve(&ThemeSelection::named("Nord")).unwrap();
        let mut buffer = Buffer::empty(Rect::new(0, 0, 1, 1));
        ColorMode::TrueColor.apply_with(&mut buffer, &theme);
        assert_eq!((buffer.content[0].fg, buffer.content[0].bg), (Color::Reset, Color::Reset));
        let solid = Resolved { solid: true, ..theme };
        ColorMode::TrueColor.apply_with(&mut buffer, &solid);
        assert_eq!(buffer.content[0].bg, theme.base);
        assert_eq!(buffer.content[0].fg, theme.colors[Token::Text as usize]);
    }

    #[test]
    fn colours_map_to_the_expected_palette_entries() {
        assert_eq!(nearest_256(0, 0, 0), 16);
        assert_eq!(nearest_256(255, 255, 255), 231);
        assert_eq!(nearest_256(255, 0, 0), 196);
        assert_eq!(nearest_256(128, 128, 128), 244);
    }

    #[test]
    fn no_color_removes_colour_and_keeps_attributes() {
        let mut buffer = Buffer::empty(ratatui::layout::Rect::new(0, 0, 1, 1));
        buffer[(0, 0)].set_style(attention());
        ColorMode::None.apply(&mut buffer);
        assert_eq!(buffer[(0, 0)].fg, Color::Reset);
        assert!(buffer[(0, 0)].modifier.contains(Modifier::BOLD));
    }
}
