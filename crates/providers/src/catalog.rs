//! Reads what a provider's `/models` listing says about each model. Each provider has its own
//! schema, recognised by its shape rather than by host name, so a gateway using the same schema
//! is understood too. Anything a schema doesn't say stays unknown.

use recode_core::model_facts::ModelFacts;
use serde_json::Value;

/// Facts from one entry of a `/models` listing.
#[must_use]
pub fn facts_from_listing(entry: &Value) -> ModelFacts {
    if entry.get("capabilities").is_some() || entry.pointer("/cost/text").is_some() {
        return zen(entry);
    }
    if entry.get("pricing").is_some() || entry.get("supported_parameters").is_some() {
        return openrouter(entry);
    }
    ModelFacts::default()
}

/// OpenCode Zen (`opencode.ai/zen/v1/models`): `cost.text.{input,output}.price`, `capabilities`,
/// `modalities.input` and `limit.context`.
fn zen(entry: &Value) -> ModelFacts {
    let price = |side: &str| entry.pointer(&format!("/cost/text/{side}/price")).and_then(Value::as_f64);
    let (input, output) = (price("input"), price("output"));
    let modalities = entry.pointer("/modalities/input").and_then(Value::as_array);
    ModelFacts {
        tools: entry.pointer("/capabilities/tools/supported").and_then(Value::as_bool),
        vision: modalities.map(|kinds| kinds.iter().filter_map(Value::as_str).any(|kind| kind.starts_with("image"))),
        reasoning: entry.pointer("/capabilities/reasoning/supported").and_then(Value::as_bool),
        context: entry.pointer("/limit/context").and_then(Value::as_u64),
        free: match (input, output) {
            (Some(input), Some(output)) => Some(input == 0.0 && output == 0.0),
            _ => None,
        },
        price: input.zip(output),
    }
}

/// OpenRouter (`openrouter.ai/api/v1/models`): `pricing.{prompt,completion}` as strings per token,
/// `supported_parameters`, `architecture.input_modalities` and `context_length`.
fn openrouter(entry: &Value) -> ModelFacts {
    let per_token = |key: &str| entry.pointer(&format!("/pricing/{key}")).and_then(Value::as_str)?.parse::<f64>().ok();
    let (prompt, completion) = (per_token("prompt"), per_token("completion"));
    let parameters = entry.get("supported_parameters").and_then(Value::as_array);
    let modalities = entry.pointer("/architecture/input_modalities").and_then(Value::as_array);
    ModelFacts {
        tools: parameters.map(|list| list.iter().filter_map(Value::as_str).any(|p| p == "tools")),
        vision: modalities.map(|kinds| kinds.iter().filter_map(Value::as_str).any(|kind| kind == "image")),
        reasoning: parameters.map(|list| list.iter().filter_map(Value::as_str).any(|p| p == "reasoning")),
        context: entry.get("context_length").and_then(Value::as_u64),
        free: match (prompt, completion) {
            (Some(prompt), Some(completion)) => Some(prompt == 0.0 && completion == 0.0),
            _ => None,
        },
        // Per-token prices become per million, as everyone quotes them.
        price: prompt.zip(completion).map(|(p, c)| (p * 1_000_000.0, c * 1_000_000.0)),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use serde_json::json;

    #[test]
    fn opencode_zen_entries_are_understood() {
        // Trimmed from the live listing on 2026-11-06.
        let free = json!({
            "id": "deepseek-v4.1-flash-free",
            "cost": {"text": {"input": {"price": 0}, "output": {"price": 0}}},
            "capabilities": {"tools": {"supported": true}, "reasoning": {"supported": true}},
            "modalities": {"input": ["text", "image/jpeg", "image/png"], "output": ["text"]},
            "limit": {"context": 200000, "output": 32000}
        });
        let facts = facts_from_listing(&free);
        assert_eq!(facts.free, Some(true));
        assert_eq!((facts.tools, facts.vision, facts.reasoning), (Some(true), Some(true), Some(true)));
        assert_eq!(facts.context, Some(200000));
        let paid = json!({"id": "claude-sonnet-5-5", "cost": {"text": {"input": {"price": 2}, "output": {"price": 10}}},
            "capabilities": {"tools": {"supported": true}}, "modalities": {"input": ["text"]}});
        let facts = facts_from_listing(&paid);
        assert_eq!(facts.free, Some(false));
        assert_eq!(facts.price, Some((2.0, 10.0)));
        assert_eq!(facts.vision, Some(false));
    }

    #[test]
    fn openrouter_entries_are_understood() {
        let free = json!({
            "id": "nvidia/nemotron-3.5-lightning:free",
            "pricing": {"prompt": "0", "completion": "0"},
            "supported_parameters": ["tools", "reasoning", "max_tokens"],
            "architecture": {"input_modalities": ["text"]},
            "context_length": 1000000
        });
        let facts = facts_from_listing(&free);
        assert_eq!(facts.free, Some(true));
        assert_eq!((facts.tools, facts.vision, facts.reasoning), (Some(true), Some(false), Some(true)));
        assert_eq!(facts.context, Some(1000000));
        let paid = json!({"id": "x", "pricing": {"prompt": "0.0000015", "completion": "0.000006"},
            "supported_parameters": ["max_tokens"], "architecture": {"input_modalities": ["text", "image"]}});
        let facts = facts_from_listing(&paid);
        assert_eq!(facts.free, Some(false));
        assert_eq!(facts.tools, Some(false));
        assert_eq!(facts.vision, Some(true));
        let (input, output) = facts.price.unwrap();
        assert!((input - 1.5).abs() < 1e-9 && (output - 6.0).abs() < 1e-9, "per million tokens");
    }

    #[test]
    fn a_plain_listing_claims_nothing() {
        assert!(facts_from_listing(&json!({"id": "gpt-4o", "object": "model"})).is_empty());
    }
}
