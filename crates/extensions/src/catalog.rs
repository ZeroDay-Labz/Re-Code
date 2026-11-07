//! The models.dev catalog: capabilities and pricing for cloud models, so re-code can flag which
//! models are free, vision-capable, or tool-capable — the same source OpenCode uses.

use serde::Deserialize;
use std::collections::HashMap;
use std::path::Path;
use std::time::{Duration, SystemTime};

const CATALOG_URL: &str = "https://models.dev/api.json";
const MAX_AGE: Duration = Duration::from_secs(24 * 60 * 60);
const FETCH_TIMEOUT: Duration = Duration::from_secs(30);

pub use recode_core::model_facts::ModelFacts;

/// Model facts, looked up by model id.
#[derive(Debug, Default, Clone)]
pub struct Catalog {
    models: HashMap<String, ModelFacts>,
}

impl Catalog {
    /// Loads the catalog, preferring a fresh cache and falling back to the network, then to a stale
    /// cache, then to empty. Never fails: missing facts just mean no extra flags.
    pub async fn load(cache_file: &Path) -> Self {
        if let Some(text) = fresh_cache(cache_file) {
            return Self::parse(&text);
        }
        match fetch().await {
            Ok(text) => {
                if let Some(parent) = cache_file.parent() {
                    let _ = std::fs::create_dir_all(parent);
                }
                let _ = std::fs::write(cache_file, &text);
                Self::parse(&text)
            }
            Err(error) => {
                tracing::warn!(%error, "couldn't refresh the models.dev catalog; using any cached copy");
                std::fs::read_to_string(cache_file).map(|text| Self::parse(&text)).unwrap_or_default()
            }
        }
    }

    /// Facts for a model id, trying the exact id then the part after the last `/`.
    #[must_use]
    pub fn facts(&self, model_id: &str) -> Option<ModelFacts> {
        self.models
            .get(model_id)
            .or_else(|| model_id.rsplit('/').next().and_then(|short| self.models.get(short)))
            .cloned()
    }

    fn parse(text: &str) -> Self {
        let Ok(raw) = serde_json::from_str::<HashMap<String, WireProvider>>(text) else {
            return Self::default();
        };
        let mut models = HashMap::new();
        for provider in raw.values() {
            for (id, model) in &provider.models {
                let facts = ModelFacts {
                    vision: Some(model.modalities.input.iter().any(|m| m == "image")),
                    tools: Some(model.tool_call),
                    reasoning: None,
                    context: model.limit.as_ref().and_then(|limit| limit.context),
                    free: model.cost.as_ref().map(|cost| cost.input == Some(0.0) && cost.output == Some(0.0)),
                    price: model.cost.as_ref().and_then(|cost| cost.input.zip(cost.output)),
                };
                // Full id wins; a bare short id is only inserted if not already present.
                if let Some(short) = id.rsplit('/').next().filter(|short| *short != id) {
                    models.entry(short.to_owned()).or_insert_with(|| facts.clone());
                }
                models.insert(id.clone(), facts);
            }
        }
        Self { models }
    }
}

fn fresh_cache(cache_file: &Path) -> Option<String> {
    let modified = std::fs::metadata(cache_file).ok()?.modified().ok()?;
    (SystemTime::now().duration_since(modified).unwrap_or(MAX_AGE) < MAX_AGE)
        .then(|| std::fs::read_to_string(cache_file).ok())
        .flatten()
}

async fn fetch() -> Result<String, String> {
    let client = reqwest::Client::builder().timeout(FETCH_TIMEOUT).build().map_err(|error| error.to_string())?;
    let response = client.get(CATALOG_URL).send().await.map_err(|error| error.to_string())?;
    if !response.status().is_success() {
        return Err(format!("HTTP {}", response.status().as_u16()));
    }
    response.text().await.map_err(|error| error.to_string())
}

#[derive(Deserialize)]
struct WireProvider {
    #[serde(default)]
    models: HashMap<String, WireModel>,
}

#[derive(Deserialize)]
struct WireModel {
    #[serde(default)]
    modalities: Modalities,
    #[serde(default)]
    tool_call: bool,
    limit: Option<Limit>,
    cost: Option<Cost>,
}

#[derive(Default, Deserialize)]
struct Modalities {
    #[serde(default)]
    input: Vec<String>,
}

#[derive(Deserialize)]
struct Limit {
    context: Option<u64>,
}

#[derive(Deserialize)]
struct Cost {
    input: Option<f64>,
    output: Option<f64>,
}

#[cfg(test)]
mod tests {
    use super::*;

    const SAMPLE: &str = r#"{
        "openrouter": {"models": {
            "vendor/free-vision": {"modalities": {"input": ["text","image"]}, "tool_call": true, "limit": {"context": 131072}, "cost": {"input": 0, "output": 0}},
            "vendor/paid-text": {"modalities": {"input": ["text"]}, "tool_call": false, "limit": {"context": 8192}, "cost": {"input": 0.5, "output": 1.5}}
        }}
    }"#;

    #[test]
    fn free_vision_and_tools_are_detected() {
        let catalog = Catalog::parse(SAMPLE);
        let free = catalog.facts("vendor/free-vision").unwrap();
        assert_eq!((free.free, free.vision, free.tools), (Some(true), Some(true), Some(true)));
        assert_eq!(free.context, Some(131072));
        let paid = catalog.facts("vendor/paid-text").unwrap();
        assert_eq!((paid.free, paid.vision, paid.tools), (Some(false), Some(false), Some(false)));
        // Short-id fallback.
        assert_eq!(catalog.facts("free-vision").and_then(|f| f.free), Some(true));
    }

    #[test]
    fn garbage_parses_to_an_empty_catalog() {
        assert!(Catalog::parse("not json").facts("x").is_none());
    }
}
