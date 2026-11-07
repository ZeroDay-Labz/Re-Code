//! Themes of the user's own, one TOML file each in the themes folder:
//!
//! ```toml
//! name = "Ember"
//! inherit = "Gruvbox"      # optional: start from a built-in, change what you list
//! light = false
//! base = "#1d2021"
//! borders = "square"       # rounded | square
//! density = "compact"      # roomy | compact
//! syntax = "derived"       # or one of syntect's theme names
//!
//! [colors]                 # any of the 14 tokens, #rrggbb
//! accent = "#fe8019"
//!
//! [logo]
//! stops = ["#fe8019", "#d3869b", "#b16286"]
//! ```

use crate::theme::{Borders, Palette, THEMES, Token, parse_hex};
use serde::Deserialize;
use std::borrow::Cow;
use std::collections::BTreeMap;
use std::path::Path;

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct ThemeFile {
    name: String,
    inherit: Option<String>,
    light: Option<bool>,
    base: Option<String>,
    borders: Option<String>,
    density: Option<String>,
    syntax: Option<String>,
    #[serde(default)]
    colors: BTreeMap<String, String>,
    logo: Option<Logo>,
}

#[derive(Debug, Default, Deserialize)]
#[serde(deny_unknown_fields)]
struct Logo {
    stops: Vec<String>,
}

/// Reads one theme file.
pub fn parse(text: &str) -> Result<Palette, String> {
    let file: ThemeFile = toml::from_str(text).map_err(|error| error.message().to_owned())?;
    let name = file.name.trim();
    if name.is_empty() {
        return Err("the theme needs a name".to_owned());
    }
    let mut palette = match &file.inherit {
        Some(parent) => THEMES
            .iter()
            .find(|theme| theme.name.eq_ignore_ascii_case(parent.trim()))
            .cloned()
            .ok_or_else(|| format!("inherit = \"{parent}\" isn't a built-in theme"))?,
        None => THEMES[0].clone(),
    };
    palette.name = Cow::Owned(name.to_owned());
    if let Some(light) = file.light {
        palette.light = light;
    }
    if let Some(base) = &file.base {
        palette.base = colour("base", base)?;
    }
    if let Some(borders) = &file.borders {
        palette.borders = match borders.trim().to_ascii_lowercase().as_str() {
            "rounded" => Borders::Rounded,
            "square" => Borders::Square,
            other => return Err(format!("borders = \"{other}\": use rounded or square")),
        };
    }
    if let Some(density) = &file.density {
        palette.compact = match density.trim().to_ascii_lowercase().as_str() {
            "roomy" => false,
            "compact" => true,
            other => return Err(format!("density = \"{other}\": use roomy or compact")),
        };
    }
    if let Some(syntax) = &file.syntax {
        let syntax = syntax.trim();
        let known = syntax == "derived" || syntect::highlighting::ThemeSet::load_defaults().themes.contains_key(syntax);
        if !known {
            return Err(format!("syntax = \"{syntax}\" isn't \"derived\" or a syntect theme name"));
        }
        palette.syntax = Cow::Owned(syntax.to_owned());
    } else if file.inherit.is_none() {
        palette.syntax = Cow::Borrowed("derived");
    }
    for (key, value) in &file.colors {
        let token = Token::ALL
            .into_iter()
            .find(|token| token.name() == key)
            .ok_or_else(|| format!("[colors] {key} isn't a colour name; use {}", names().join(", ")))?;
        palette.colors[token as usize] = colour(key, value)?;
    }
    if let Some(logo) = &file.logo {
        if logo.stops.len() != 3 {
            return Err("[logo] stops needs exactly three colours".to_owned());
        }
        let mut stops = [palette.base; 3];
        for (slot, stop) in stops.iter_mut().zip(&logo.stops) {
            *slot = colour("logo stop", stop)?;
        }
        palette.logo = Some(stops);
    }
    Ok(palette)
}

fn colour(what: &str, text: &str) -> Result<ratatui::style::Color, String> {
    parse_hex(text).ok_or_else(|| format!("{what} = \"{text}\" isn't a colour like #ff8800"))
}

fn names() -> Vec<&'static str> {
    Token::ALL.into_iter().map(Token::name).collect()
}

/// Reads every `*.toml` in a folder. A bad file is reported by name and skipped; a missing folder is empty.
#[must_use]
pub fn load_dir(dir: &Path) -> (Vec<Palette>, Vec<String>) {
    let (mut themes, mut problems) = (Vec::new(), Vec::new());
    let Ok(entries) = std::fs::read_dir(dir) else { return (themes, problems) };
    let mut files: Vec<_> = entries.flatten().map(|entry| entry.path()).collect();
    files.sort();
    for path in files.into_iter().filter(|path| path.extension().is_some_and(|ext| ext == "toml")) {
        let shown = path.file_name().map(|name| name.to_string_lossy().into_owned()).unwrap_or_default();
        match std::fs::read_to_string(&path).map_err(|error| error.to_string()).and_then(|text| parse(&text)) {
            Ok(theme) => themes.push(theme),
            Err(error) => problems.push(format!("{shown}: {error}")),
        }
    }
    (themes, problems)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_theme_file_inherits_overrides_and_is_checked() {
        let ember = parse(
            "name = \"Ember\"\ninherit = \"gruvbox\"\nborders = \"square\"\ndensity = \"compact\"\n[colors]\naccent = \"#fe8019\"\n[logo]\nstops = [\"#fe8019\", \"#d3869b\", \"#b16286\"]\n",
        )
        .unwrap();
        let gruvbox = THEMES.iter().find(|t| t.name == "Gruvbox").unwrap();
        assert_eq!(ember.name, "Ember");
        assert_eq!(ember.colors[Token::Text as usize], gruvbox.colors[Token::Text as usize], "inherited");
        assert_eq!(ember.colors[0], parse_hex("#fe8019").unwrap(), "overridden");
        assert_eq!((ember.borders, ember.compact), (Borders::Square, true));
        assert_eq!(ember.logo.map(|l| l[2]), parse_hex("#b16286"));
        assert_eq!(ember.syntax, gruvbox.syntax, "an inherited syntax theme stays unless set");

        let plain = parse("name = \"Mine\"\n").unwrap();
        assert_eq!(plain.syntax, "derived", "code colours come from the tokens unless a syntect theme is named");

        assert!(parse("name = \"x\"\n[colors]\npurple = \"#000000\"\n").unwrap_err().contains("isn't a colour name"));
        assert!(parse("name = \"x\"\n[colors]\naccent = \"red\"\n").unwrap_err().contains("#ff8800"));
        assert!(parse("name = \"x\"\ninherit = \"nothing\"\n").unwrap_err().contains("built-in"));
        assert!(parse("name = \"x\"\nbogus = 1\n").is_err(), "unknown keys are mistakes, not ignored");
    }

    #[test]
    fn a_folder_of_themes_loads_the_good_ones_and_names_the_bad() {
        let dir = tempfile::tempdir().unwrap();
        std::fs::write(dir.path().join("a.toml"), "name = \"Alpha\"\n").unwrap();
        std::fs::write(dir.path().join("broken.toml"), "name = \"Broken\"\nbase = \"blue\"\n").unwrap();
        std::fs::write(dir.path().join("notes.txt"), "not a theme").unwrap();
        let (themes, problems) = load_dir(dir.path());
        assert_eq!(themes.iter().map(|t| t.name.as_ref()).collect::<Vec<_>>(), ["Alpha"]);
        assert_eq!(problems.len(), 1);
        assert!(problems[0].starts_with("broken.toml:"), "{problems:?}");
        assert!(load_dir(&dir.path().join("missing")).0.is_empty());
    }

    #[test]
    fn a_user_theme_shadows_a_built_in_of_the_same_name() {
        let mine = parse("name = \"Nord\"\n[colors]\naccent = \"#123456\"\n").unwrap();
        let all = crate::theme::merged(&[mine]);
        let nords: Vec<&Palette> = all.iter().filter(|t| t.name == "Nord").collect();
        assert_eq!(nords.len(), 1, "one Nord, the user's");
        assert_eq!(nords[0].colors[0], parse_hex("#123456").unwrap());
        assert_eq!(all.len(), THEMES.len());
    }
}
