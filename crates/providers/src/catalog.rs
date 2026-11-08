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
    if entry.get("providers").is_some_and(Value::is_array) {
        return huggingface(entry);
    }
    ModelFacts::default()
}

/// Hugging Face's router (`router.huggingface.co/v1/models`): `providers[]`, each with `status`,
/// `pricing.{input,output}` per million, `is_free`, `context_length` and `supports_tools`. A model is
/// free only when every live host says so; the price shown is the cheapest live host's.
fn huggingface(entry: &Value) -> ModelFacts {
    let hosts: Vec<&Value> = entry
        .get("providers")
        .and_then(Value::as_array)
        .map(|hosts| {
            hosts.iter().filter(|host| host.get("status").and_then(Value::as_str) != Some("offline")).collect()
        })
        .unwrap_or_default();
    if hosts.is_empty() {
        return ModelFacts::default();
    }
    let flag = |host: &Value, key: &str| host.get(key).and_then(Value::as_bool);
    let price = |host: &Value| {
        let side = |key: &str| host.pointer(&format!("/pricing/{key}")).and_then(Value::as_f64);
        side("input").zip(side("output"))
    };
    let cheapest = hosts.iter().filter_map(|host| price(host)).min_by(|a, b| (a.0 + a.1).total_cmp(&(b.0 + b.1)));
    let names: Vec<String> =
        hosts.iter().filter_map(|host| host.get("provider").and_then(Value::as_str)).map(str::to_owned).collect();
    ModelFacts {
        tools: Some(hosts.iter().any(|host| flag(host, "supports_tools") == Some(true))),
        vision: entry
            .pointer("/architecture/input_modalities")
            .and_then(Value::as_array)
            .map(|kinds| kinds.iter().filter_map(Value::as_str).any(|kind| kind == "image")),
        reasoning: None,
        context: hosts.iter().filter_map(|host| host.get("context_length").and_then(Value::as_u64)).max(),
        free: Some(hosts.iter().all(|host| flag(host, "is_free") == Some(true))),
        price: cheapest,
        modalities: modalities_at(entry, "/architecture/input_modalities", "/architecture/output_modalities"),
        released: entry.get("created").and_then(Value::as_u64).map(ModelFacts::date_from_unix),
        hosts: (!names.is_empty()).then(|| names.join(", ")),
        ..ModelFacts::default()
    }
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
        cached_price: entry.pointer("/cost/text/input/cache/read").and_then(Value::as_f64),
        display_name: text(entry, "/name"),
        description: text(entry, "/description"),
        max_output: entry.pointer("/limit/output").and_then(Value::as_u64),
        modalities: modalities_at(entry, "/modalities/input", "/modalities/output"),
        released: text(entry, "/release_date"),
        knowledge: text(entry, "/knowledge"),
        retires: None,
        hosts: None,
    }
}

fn text(entry: &Value, pointer: &str) -> Option<String> {
    entry.pointer(pointer).and_then(Value::as_str).map(str::trim).filter(|t| !t.is_empty()).map(str::to_owned)
}

fn modalities_at(entry: &Value, input: &str, output: &str) -> Option<String> {
    let list = |pointer: &str| -> Vec<&str> {
        entry
            .pointer(pointer)
            .and_then(Value::as_array)
            .map(|a| a.iter().filter_map(Value::as_str).collect())
            .unwrap_or_default()
    };
    ModelFacts::modalities_of(&list(input), &list(output))
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
        cached_price: per_token("input_cache_read").map(|p| p * 1_000_000.0),
        display_name: text(entry, "/name"),
        description: text(entry, "/description"),
        max_output: entry.pointer("/top_provider/max_completion_tokens").and_then(Value::as_u64),
        modalities: modalities_at(entry, "/architecture/input_modalities", "/architecture/output_modalities"),
        released: entry.get("created").and_then(Value::as_u64).map(ModelFacts::date_from_unix),
        knowledge: text(entry, "/knowledge_cutoff"),
        retires: text(entry, "/expiration_date"),
        hosts: None,
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
        // The details, from the live Big Pickle entry (2026-11-07).
        let pickle = json!({"id": "big-pickle", "name": "Big Pickle", "description": "Reasoning model for deliberate analysis",
            "knowledge": "2025-01", "release_date": "2025-10-17",
            "modalities": {"input": ["text"], "output": ["text"]}, "capabilities": {"tools": {"supported": true}},
            "limit": {"context": 200000, "output": 32000},
            "cost": {"text": {"input": {"price": 0, "cache": {"read": 0}}, "output": {"price": 0}}}});
        let facts = facts_from_listing(&pickle);
        assert_eq!(facts.display_name.as_deref(), Some("Big Pickle"));
        assert_eq!(facts.description.as_deref(), Some("Reasoning model for deliberate analysis"));
        assert_eq!((facts.max_output, facts.cached_price), (Some(32000), Some(0.0)));
        assert_eq!(facts.modalities.as_deref(), Some("text → text"));
        assert_eq!((facts.released.as_deref(), facts.knowledge.as_deref()), (Some("2025-10-17"), Some("2025-01")));
    }

    #[test]
    fn hugging_face_hosts_decide_price_and_free() {
        // The live entry a user was told was free (2026-11-07), and then billed for.
        let entry = json!({"id": "Qwen/Qwen3-235B-A22B-Instruct-2507", "created": 1753080416u64,
            "architecture": {"input_modalities": ["text"], "output_modalities": ["text"]},
            "providers": [{"provider": "deepinfra", "status": "live", "context_length": 262144,
                           "pricing": {"input": 0.09, "output": 0.55}, "is_free": false, "supports_tools": true}]});
        let facts = facts_from_listing(&entry);
        assert_eq!(facts.free, Some(false));
        assert_eq!(facts.price, Some((0.09, 0.55)));
        assert_eq!((facts.tools, facts.context), (Some(true), Some(262144)));
        assert_eq!(facts.hosts.as_deref(), Some("deepinfra"));
        let free = json!({"id": "x", "providers": [
            {"provider": "a", "status": "live", "is_free": true}, {"provider": "b", "status": "offline", "is_free": false}]});
        assert_eq!(facts_from_listing(&free).free, Some(true));
    }

    #[test]
    fn openrouter_details_are_read() {
        // Trimmed from the live listing (liquid/lfm-3-1.7b, 2026-11-06).
        let entry = json!({"id": "liquid/lfm-3-1.7b", "name": "LiquidAI: LFM3-1.7B", "created": 1794009477u64,
            "description": "A compact 1.7B-parameter text model.", "context_length": 262144,
            "architecture": {"input_modalities": ["text"], "output_modalities": ["text"]},
            "pricing": {"prompt": "0.00000008", "completion": "0.0000002", "input_cache_read": "0.00000002"},
            "top_provider": {"max_completion_tokens": 8192}, "supported_parameters": ["tools"],
            "knowledge_cutoff": null, "expiration_date": "2027-01-31"});
        let facts = facts_from_listing(&entry);
        assert_eq!(facts.display_name.as_deref(), Some("LiquidAI: LFM3-1.7B"));
        assert_eq!(facts.max_output, Some(8192));
        assert!((facts.cached_price.unwrap() - 0.02).abs() < 1e-9);
        assert_eq!(facts.released.as_deref(), Some("2026-11-06"));
        assert_eq!(facts.knowledge, None);
        assert_eq!(facts.retires.as_deref(), Some("2027-01-31"));
        assert_eq!(facts.modalities.as_deref(), Some("text → text"));
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
