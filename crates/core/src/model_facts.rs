//! What a provider says about one of its models: whether it costs anything, what it can do, and how
//! much it can hold. `None` means the provider didn't say, which is different from "no".

use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelFacts {
    pub tools: Option<bool>,
    pub vision: Option<bool>,
    pub reasoning: Option<bool>,
    /// The largest context, in tokens.
    pub context: Option<u64>,
    /// Both input and output cost nothing.
    pub free: Option<bool>,
    /// Price per million tokens: (input, output), in US dollars.
    pub price: Option<(f64, f64)>,
    /// Price per million input tokens served from the provider's cache.
    #[serde(default)]
    pub cached_price: Option<f64>,
    /// The provider's own name for the model, when it differs from the id.
    #[serde(default)]
    pub display_name: Option<String>,
    /// The provider's description of the model.
    #[serde(default)]
    pub description: Option<String>,
    /// The longest reply, in tokens.
    #[serde(default)]
    pub max_output: Option<u64>,
    /// What goes in and comes out, such as "text, image → text".
    #[serde(default)]
    pub modalities: Option<String>,
    /// When the model came out (YYYY-MM-DD).
    #[serde(default)]
    pub released: Option<String>,
    /// How recent its training data is, as the provider puts it.
    #[serde(default)]
    pub knowledge: Option<String>,
    /// When the provider stops serving it.
    #[serde(default)]
    pub retires: Option<String>,
    /// Who actually runs it, for a router that passes requests on (Hugging Face).
    #[serde(default)]
    pub hosts: Option<String>,
}

impl ModelFacts {
    /// These facts, with anything unknown filled in from `fallback` (a catalog, say).
    #[must_use]
    pub fn merge(self, fallback: Option<ModelFacts>) -> ModelFacts {
        let Some(other) = fallback else { return self };
        ModelFacts {
            tools: self.tools.or(other.tools),
            vision: self.vision.or(other.vision),
            reasoning: self.reasoning.or(other.reasoning),
            context: self.context.or(other.context),
            free: self.free.or(other.free),
            price: self.price.or(other.price),
            cached_price: self.cached_price.or(other.cached_price),
            display_name: self.display_name.or(other.display_name),
            description: self.description.or(other.description),
            max_output: self.max_output.or(other.max_output),
            modalities: self.modalities.or(other.modalities),
            released: self.released.or(other.released),
            knowledge: self.knowledge.or(other.knowledge),
            retires: self.retires.or(other.retires),
            hosts: self.hosts.or(other.hosts),
        }
    }

    /// The same facts without what depends on who serves the model. The models.dev catalog is matched by
    /// name, possibly another provider's listing of it, so its price and "free" can't be trusted for
    /// this one; what the model can do and how much it holds still can.
    #[must_use]
    pub fn without_pricing(self) -> ModelFacts {
        ModelFacts { free: None, price: None, cached_price: None, retires: None, hosts: None, ..self }
    }

    /// The parameter count a model's name gives, such as "235B (22B active)" from
    /// `Qwen3-235B-A22B-Instruct`. `None` when the name doesn't say.
    #[must_use]
    pub fn parameters_from_name(name: &str) -> Option<String> {
        let tail = name.rsplit('/').next().unwrap_or(name);
        let parts: Vec<&str> = tail.split(['-', '_', ':', ' ']).collect();
        let size = |part: &str| -> Option<String> {
            let digits = part.strip_suffix(['B', 'b'])?;
            (!digits.is_empty()
                && digits.chars().all(|c| c.is_ascii_digit() || c == '.')
                && digits.chars().next()?.is_ascii_digit())
            .then(|| format!("{digits}B"))
        };
        let total = parts.iter().find_map(|part| size(part))?;
        let active = parts.iter().find_map(|part| part.strip_prefix(['A', 'a']).and_then(size));
        Some(match active {
            Some(active) => format!("{total} ({active} active)"),
            None => total,
        })
    }

    /// "text, image → text" from the two lists of modalities.
    #[must_use]
    pub fn modalities_of(input: &[&str], output: &[&str]) -> Option<String> {
        (!input.is_empty() && !output.is_empty()).then(|| format!("{} → {}", input.join(", "), output.join(", ")))
    }

    /// A Unix time as YYYY-MM-DD (UTC), for dates a listing gives as seconds.
    #[must_use]
    pub fn date_from_unix(seconds: u64) -> String {
        // Howard Hinnant's civil-from-days.
        let days = i64::try_from(seconds / 86_400).unwrap_or(0) + 719_468;
        let era = days.div_euclid(146_097);
        let day_of_era = days.rem_euclid(146_097);
        let year_of_era = (day_of_era - day_of_era / 1460 + day_of_era / 36_524 - day_of_era / 146_096) / 365;
        let day_of_year = day_of_era - (365 * year_of_era + year_of_era / 4 - year_of_era / 100);
        let mp = (5 * day_of_year + 2) / 153;
        let day = day_of_year - (153 * mp + 2) / 5 + 1;
        let month = if mp < 10 { mp + 3 } else { mp - 9 };
        let year = year_of_era + era * 400 + i64::from(month <= 2);
        format!("{year:04}-{month:02}-{day:02}")
    }

    /// Whether anything is known at all.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        *self == ModelFacts::default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn live_facts_win_and_a_catalog_only_fills_gaps() {
        let live = ModelFacts { tools: Some(true), free: Some(true), ..ModelFacts::default() };
        let catalog =
            ModelFacts { tools: Some(false), vision: Some(true), context: Some(8192), ..ModelFacts::default() };
        let merged = live.merge(Some(catalog));
        assert_eq!(merged.tools, Some(true), "the provider's own word wins");
        assert_eq!((merged.vision, merged.context, merged.free), (Some(true), Some(8192), Some(true)));
        assert!(ModelFacts::default().is_empty());
        assert_eq!(ModelFacts::date_from_unix(1_794_009_477), "2026-11-06");
        assert_eq!(ModelFacts::date_from_unix(0), "1970-01-01");
        assert_eq!(
            ModelFacts::parameters_from_name("Qwen/Qwen3-235B-A22B-Instruct-2507").as_deref(),
            Some("235B (22B active)")
        );
        assert_eq!(ModelFacts::parameters_from_name("meta-llama/Llama-3.1-8B-Instruct").as_deref(), Some("8B"));
        assert_eq!(ModelFacts::parameters_from_name("gpt-5-mini"), None);
        assert_eq!(ModelFacts::parameters_from_name("mixtral-8x7b"), None);
        let priced =
            ModelFacts { free: Some(true), price: Some((0.0, 0.0)), context: Some(8), ..ModelFacts::default() };
        let kept = priced.without_pricing();
        assert_eq!((kept.free, kept.price, kept.context), (None, None, Some(8)));
        assert_eq!(ModelFacts::modalities_of(&["text", "image"], &["text"]).as_deref(), Some("text, image → text"));
    }
}
