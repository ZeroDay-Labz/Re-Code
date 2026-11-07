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
        }
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
    }
}
