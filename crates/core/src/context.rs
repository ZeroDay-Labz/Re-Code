//! The context window a conversation is budgeted against (`docs/CONTEXT_ENGINE.md`, Budgeting).
//!
//! Every model gets a window: for a local model it is the `num_ctx` sent to Ollama, for a cloud model
//! it is the limit its listing declares. "Auto" picks sensible figures from what is known; the user can
//! pin the size, the point where compaction starts and the room kept for the reply, per model. Nothing
//! here is a measurement: the sizes are policy, and the display says so.

use serde::{Deserialize, Serialize};

/// A size that is either chosen automatically or pinned by the user, in tokens.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "tokens")]
pub enum Size {
    #[default]
    Auto,
    Tokens(u64),
}

/// Where automatic compaction starts.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "mode", content = "tokens")]
pub enum Trigger {
    #[default]
    Auto,
    Tokens(u64),
    /// Never compact on its own; `/compact` still works.
    Off,
}

/// The user's choices for one model. Stored as JSON under the setting `ctx:<model>`.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct WindowSetting {
    #[serde(default)]
    pub size: Size,
    #[serde(default)]
    pub trigger: Trigger,
    #[serde(default)]
    pub reserve: Size,
}

impl WindowSetting {
    /// The setting key for a model.
    #[must_use]
    pub fn key(model: &str) -> String {
        format!("ctx:{model}")
    }

    /// Whether everything is left to auto.
    #[must_use]
    pub fn is_auto(&self) -> bool {
        *self == Self::default()
    }
}

/// The window in force for the current model.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextWindow {
    /// The most the model can hold, when its runtime or listing says.
    pub model_limit: Option<u64>,
    /// What requests are budgeted against. `None` when nothing is known and nothing was set, so no
    /// automatic compaction can run.
    pub window: Option<u64>,
    /// The `num_ctx` sent to a local model; `None` for a cloud model.
    pub local_num_ctx: Option<u32>,
    /// Tokens kept free for the reply.
    pub reserve: u64,
    /// Estimated tokens at which compaction starts; `None` when it is off or can't be judged.
    pub trigger: Option<u64>,
    /// Where the window came from, in words.
    pub source: String,
}

impl ContextWindow {
    /// Works out the window from the user's setting and what is known about the model.
    #[must_use]
    pub fn resolve(setting: &WindowSetting, model_limit: Option<u64>, is_local: bool, default_num_ctx: u32) -> Self {
        let (window, source) = match (setting.size, is_local, model_limit) {
            (Size::Tokens(tokens), _, Some(limit)) if tokens > limit => {
                (Some(limit), format!("your setting, capped at the model's {limit}-token limit"))
            }
            (Size::Tokens(tokens), _, _) => (Some(tokens), "your setting".to_owned()),
            (Size::Auto, true, Some(limit)) => {
                let wanted = u64::from(default_num_ctx);
                if wanted > limit {
                    (Some(limit), format!("the model's {limit}-token limit (smaller than the default)"))
                } else {
                    (Some(wanted), "the default context size".to_owned())
                }
            }
            (Size::Auto, true, None) => (Some(u64::from(default_num_ctx)), "the default context size".to_owned()),
            (Size::Auto, false, Some(limit)) => (Some(limit), "the model's listing".to_owned()),
            (Size::Auto, false, None) => (None, "unknown: the listing doesn't say".to_owned()),
        };
        let reserve = match (setting.reserve, window) {
            (Size::Tokens(tokens), Some(window)) => tokens.min(window / 2),
            (Size::Tokens(tokens), None) => tokens,
            (Size::Auto, Some(window)) => (window / 8).clamp(1024, 16384).min(window / 2),
            (Size::Auto, None) => 0,
        };
        let budget = window.map(|window| window.saturating_sub(reserve));
        let trigger = match (setting.trigger, budget) {
            (Trigger::Off, _) | (_, None) => None,
            (Trigger::Tokens(tokens), Some(budget)) => Some(tokens.min(budget)),
            (Trigger::Auto, Some(budget)) => Some(budget - budget / 10),
        };
        let local_num_ctx = if is_local { window.and_then(|w| u32::try_from(w).ok()) } else { None };
        Self { model_limit, window, local_num_ctx, reserve, trigger, source }
    }

    /// The most a request may hold: the window minus the reserve.
    #[must_use]
    pub fn budget(&self) -> Option<u64> {
        self.window.map(|window| window.saturating_sub(self.reserve))
    }

    /// One line for the Settings tab: `auto (16k of 128k) · compacts at ~13k · reserve 2k`.
    #[must_use]
    pub fn describe(&self, setting: &WindowSetting) -> String {
        let size = match (self.window, self.model_limit, setting.size) {
            (Some(window), Some(limit), Size::Auto) if window < limit => {
                format!("auto ({} of {})", tokens(window), tokens(limit))
            }
            (Some(window), _, Size::Auto) => format!("auto ({})", tokens(window)),
            (Some(window), Some(limit), Size::Tokens(_)) => format!("{} of {}", tokens(window), tokens(limit)),
            (Some(window), None, Size::Tokens(_)) => tokens(window),
            (None, _, _) => "unknown".to_owned(),
        };
        let compaction = match (self.trigger, setting.trigger) {
            (None, Trigger::Off) => "no auto-compaction".to_owned(),
            (None, _) => "can't auto-compact".to_owned(),
            (Some(at), Trigger::Auto) => format!("compacts at ~{}", tokens(at)),
            (Some(at), Trigger::Tokens(_)) => format!("compacts at {} (yours)", tokens(at)),
            (Some(at), Trigger::Off) => format!("compacts at ~{}", tokens(at)),
        };
        let reserve = match setting.reserve {
            Size::Auto => format!("reserve {}", tokens(self.reserve)),
            Size::Tokens(_) => format!("reserve {} (yours)", tokens(self.reserve)),
        };
        format!("{size} · {compaction} · {reserve}")
    }
}

/// `16k`, `128k`, `1.0M`, or the plain number under a thousand.
#[must_use]
pub fn tokens(value: u64) -> String {
    if value < 1000 {
        return value.to_string();
    }
    if value >= 1_000_000 {
        #[allow(clippy::cast_precision_loss)]
        let millions = value as f64 / 1_000_000.0;
        return format!("{millions:.1}M");
    }
    if value.is_multiple_of(1024) {
        return format!("{}k", value / 1024);
    }
    #[allow(clippy::cast_precision_loss)]
    let thousands = value as f64 / 1000.0;
    if thousands < 10.0 { format!("{thousands:.1}k") } else { format!("{thousands:.0}k") }
}

/// Reads a size as `/autocompact` takes it: `500k`, `1M`, `200000`, a bare number from 1 to 999
/// meaning thousands (`200` is 200k), `auto`, or `off`. Returns the trigger to store.
pub fn parse_autocompact(text: &str) -> Result<Trigger, String> {
    let text = text.trim().to_ascii_lowercase();
    match text.as_str() {
        "" => return Err("Give a size, such as /autocompact 100k, or auto, or off.".to_owned()),
        "auto" => return Ok(Trigger::Auto),
        "off" | "never" => return Ok(Trigger::Off),
        _ => {}
    }
    let (digits, factor) = if let Some(rest) = text.strip_suffix('k') {
        (rest, 1_000)
    } else if let Some(rest) = text.strip_suffix('m') {
        (rest, 1_000_000)
    } else {
        (text.as_str(), 1)
    };
    let number: f64 = digits.trim().parse().map_err(|_| format!("`{text}` isn't a size. Try 100k, 1M or auto."))?;
    if number <= 0.0 {
        return Err(format!("`{text}` isn't a size. Try 100k, 1M or auto."));
    }
    #[allow(clippy::cast_possible_truncation, clippy::cast_sign_loss)]
    let mut value = (number * f64::from(factor)) as u64;
    if factor == 1 && value < 1000 {
        value *= 1000;
    }
    if value < 1024 {
        return Err(format!("{value} tokens is too small to work in; the least is 1k."));
    }
    Ok(Trigger::Tokens(value))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn auto_follows_the_default_for_local_models_and_the_listing_for_cloud_ones() {
        let local = ContextWindow::resolve(&WindowSetting::default(), Some(131_072), true, 16384);
        assert_eq!((local.window, local.local_num_ctx), (Some(16384), Some(16384)));
        assert_eq!(local.reserve, 2048, "an eighth, within 1k..16k");
        assert_eq!(local.budget(), Some(14336));
        assert_eq!(local.trigger, Some(14336 - 1433), "nine tenths of the budget");
        assert_eq!(local.describe(&WindowSetting::default()), "auto (16k of 128k) · compacts at ~13k · reserve 2k");

        let small = ContextWindow::resolve(&WindowSetting::default(), Some(8192), true, 16384);
        assert_eq!(small.window, Some(8192), "never more than the model holds");
        assert!(small.source.contains("limit"), "{}", small.source);

        let cloud = ContextWindow::resolve(&WindowSetting::default(), Some(200_000), false, 16384);
        assert_eq!((cloud.window, cloud.local_num_ctx), (Some(200_000), None));
        assert_eq!(cloud.reserve, 16384, "capped at 16k");
        assert!(cloud.trigger.is_some());

        let unknown = ContextWindow::resolve(&WindowSetting::default(), None, false, 16384);
        assert_eq!((unknown.window, unknown.trigger), (None, None), "nothing is guessed");
        assert_eq!(unknown.describe(&WindowSetting::default()), "unknown · can't auto-compact · reserve 0");
    }

    #[test]
    fn the_user_can_pin_size_trigger_and_reserve() {
        let setting =
            WindowSetting { size: Size::Tokens(32768), trigger: Trigger::Tokens(20000), reserve: Size::Tokens(4096) };
        let window = ContextWindow::resolve(&setting, Some(131_072), true, 16384);
        assert_eq!((window.window, window.reserve, window.trigger), (Some(32768), 4096, Some(20000)));
        assert_eq!(window.describe(&setting), "32k of 128k · compacts at 20k (yours) · reserve 4k (yours)");

        let capped = ContextWindow::resolve(
            &WindowSetting { size: Size::Tokens(1_000_000), ..setting },
            Some(131_072),
            true,
            16384,
        );
        assert_eq!(capped.window, Some(131_072));
        assert!(capped.source.contains("capped"));

        let off = ContextWindow::resolve(
            &WindowSetting { trigger: Trigger::Off, ..WindowSetting::default() },
            Some(8192),
            true,
            8192,
        );
        assert_eq!(off.trigger, None);
        assert!(
            off.describe(&WindowSetting { trigger: Trigger::Off, ..WindowSetting::default() })
                .contains("no auto-compaction")
        );

        let pinned_cloud = ContextWindow::resolve(
            &WindowSetting { size: Size::Tokens(128_000), ..WindowSetting::default() },
            None,
            false,
            16384,
        );
        assert_eq!(pinned_cloud.window, Some(128_000), "a pinned size works without a listing");
        assert!(pinned_cloud.trigger.is_some());
    }

    #[test]
    fn autocompact_sizes_are_read_the_way_people_type_them() {
        assert_eq!(parse_autocompact("500k"), Ok(Trigger::Tokens(500_000)));
        assert_eq!(parse_autocompact("1M"), Ok(Trigger::Tokens(1_000_000)));
        assert_eq!(parse_autocompact("200"), Ok(Trigger::Tokens(200_000)), "bare numbers mean thousands");
        assert_eq!(parse_autocompact("200000"), Ok(Trigger::Tokens(200_000)));
        assert_eq!(parse_autocompact("auto"), Ok(Trigger::Auto));
        assert_eq!(parse_autocompact("off"), Ok(Trigger::Off));
        assert!(parse_autocompact("lots").is_err());
        assert!(parse_autocompact("").is_err());
        assert_eq!(tokens(131_072), "128k");
        assert_eq!(tokens(1_000_000), "1.0M");
        assert_eq!(tokens(9_140), "9.1k");
    }

    #[test]
    fn settings_round_trip_through_json_with_defaults() {
        let text = serde_json::to_string(&WindowSetting { size: Size::Tokens(1), ..WindowSetting::default() }).unwrap();
        let back: WindowSetting = serde_json::from_str(&text).unwrap();
        assert_eq!(back.size, Size::Tokens(1));
        let partial: WindowSetting = serde_json::from_str(r#"{"trigger":{"mode":"off"}}"#).unwrap();
        assert_eq!(partial.trigger, Trigger::Off);
        assert!(WindowSetting::default().is_auto() && !partial.is_auto());
    }
}
