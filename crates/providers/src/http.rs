//! Small HTTP helpers shared by the cloud adapters.

use crate::ProviderError;

/// The most specific cause, since reqwest's top-level message is rarely the useful one.
pub(crate) fn root_cause(error: &(dyn std::error::Error + 'static)) -> String {
    let mut current = error;
    while let Some(next) = current.source() {
        current = next;
    }
    current.to_string()
}

/// Checks the response status, turning a non-success into a readable error. 401/403 become
/// [`ProviderError::NeedsKey`]; 429, or an error body that names a usage or rate limit, becomes
/// [`ProviderError::RateLimited`] with the server's `Retry-After` when it gives one.
pub(crate) async fn check(
    url: &str,
    response: reqwest::Result<reqwest::Response>,
) -> Result<reqwest::Response, ProviderError> {
    let response =
        response.map_err(|error| ProviderError::Unreachable { url: url.to_owned(), reason: root_cause(&error) })?;
    let status = response.status();
    if status.is_success() {
        return Ok(response);
    }
    let retry_after = response
        .headers()
        .get("retry-after")
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(std::time::Duration::from_secs);
    let body = response.text().await.unwrap_or_default();
    let message = extract_message(&body);
    let kind = extract_kind(&body);
    Err(classify(url, status.as_u16(), kind.as_deref(), retry_after, message))
}

/// The error for a failed request, from its status and the kind of error the body names.
pub(crate) fn classify(
    url: &str,
    status: u16,
    kind: Option<&str>,
    retry_after: Option<std::time::Duration>,
    message: String,
) -> ProviderError {
    let url = url.to_owned();
    if is_client_only_message(&message) {
        return ProviderError::ClientOnly { url, message };
    }
    match (status, kind.map(str::to_ascii_lowercase)) {
        (429, _) => ProviderError::RateLimited { url, retry_after, message },
        (402, _) => ProviderError::PaymentRequired { url, message },
        (404, _) if is_policy_message(&message) => ProviderError::PolicyBlocked { url, message },
        (_, Some(kind)) if is_limit_kind(&kind) => ProviderError::RateLimited { url, retry_after, message },
        (401 | 403, _) => ProviderError::NeedsKey { url, message },
        (_, Some(kind)) if kind.contains("authentication") => ProviderError::NeedsKey { url, message },
        _ => ProviderError::Http { url, status, message },
    }
}

/// Whether an error message says the model is served only to the provider's own app. OpenCode Zen,
/// 2026-11: "Free usage limit reached. Free models work in OpenCode (https://opencode.ai); subscribe to
/// OpenCode Go to keep going" — sent to any other client at once, with a retry-after of hours.
pub(crate) fn is_client_only_message(message: &str) -> bool {
    let lower = message.to_ascii_lowercase();
    lower.contains("free models work in") || lower.contains("require opencode") || lower.contains("requires opencode")
}

/// Whether an error message says the account's data policy rules out every host for the model
/// (OpenRouter: "No endpoints found matching your data policy").
pub(crate) fn is_policy_message(message: &str) -> bool {
    message.to_ascii_lowercase().contains("data policy")
}

/// Whether an error kind (such as OpenCode's `FreeUsageLimitError`) means "slow down" rather than "wrong".
pub(crate) fn is_limit_kind(kind: &str) -> bool {
    kind.contains("limit") && (kind.contains("usage") || kind.contains("rate") || kind.contains("quota"))
}

/// The `error` object of a JSON error body. Gemini's OpenAI-compatible endpoint wraps it in an array.
fn error_object(body: &str) -> Option<serde_json::Value> {
    let value: serde_json::Value = serde_json::from_str(body.trim()).ok()?;
    let object = match value {
        serde_json::Value::Array(items) => items.into_iter().next()?,
        other => other,
    };
    object.get("error").cloned()
}

/// The `error.type` (or `error.status`) of a provider's JSON error body, when it names one.
fn extract_kind(body: &str) -> Option<String> {
    let error = error_object(body)?;
    error.get("type").or_else(|| error.get("status"))?.as_str().map(str::to_owned)
}

/// Pulls a human message out of a provider's JSON error body (`error.message`, as OpenAI, Anthropic
/// and Gemini all shape it), falling back to the raw text.
fn extract_message(body: &str) -> String {
    error_object(body)
        .and_then(|error| error.get("message")?.as_str().map(str::to_owned))
        .filter(|message| !message.is_empty())
        .unwrap_or_else(|| body.chars().take(200).collect())
}

pub(crate) async fn decode_json<T: serde::de::DeserializeOwned>(
    url: &str,
    response: reqwest::Result<reqwest::Response>,
) -> Result<T, ProviderError> {
    let response = check(url, response).await?;
    let bytes = response
        .bytes()
        .await
        .map_err(|error| ProviderError::Unreachable { url: url.to_owned(), reason: root_cause(&error) })?;
    serde_json::from_slice(&bytes).map_err(|error| ProviderError::Decode(format!("{url}: {error}")))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn statuses_and_bodies_are_classified() {
        let policy = classify(
            "https://openrouter.ai/api/v1/chat/completions",
            404,
            None,
            None,
            "No endpoints found matching your data policy (Free model publication). Configure: https://openrouter.ai/settings/privacy".to_owned(),
        );
        assert!(matches!(policy, ProviderError::PolicyBlocked { .. }), "{policy:?}");
        assert!(policy.to_string().contains("openrouter.ai/settings/privacy"));
        assert!(matches!(
            classify("u", 404, None, None, "model not found".to_owned()),
            ProviderError::Http { status: 404, .. }
        ));
        assert!(matches!(classify("u", 429, None, None, "slow".to_owned()), ProviderError::RateLimited { .. }));
        // Seen live from Zen on 2026-11-07, with a retry-after of 17071 s: a gate, not a wait.
        let gate = classify(
            "https://opencode.ai/zen/v1/chat/completions",
            429,
            Some("FreeUsageLimitError"),
            Some(std::time::Duration::from_secs(17071)),
            "Free usage limit reached.\nFree models work in OpenCode (https://opencode.ai); subscribe to OpenCode Go to keep going:\nhttps://opencode.ai/go".to_owned(),
        );
        assert!(matches!(gate, ProviderError::ClientOnly { .. }), "{gate:?}");
        assert!(gate.to_string().contains("only to its own app") && !gate.to_string().contains("Try again shortly"));
        assert!(matches!(classify("u", 401, None, None, "no".to_owned()), ProviderError::NeedsKey { .. }));
        // Hugging Face, for a model its listing doesn't call free (2026-11-07).
        let paid = classify(
            "https://router.huggingface.co/v1/chat/completions",
            402,
            None,
            None,
            "Purchase pre-paid credits to continue using Inference Providers.".to_owned(),
        );
        assert!(matches!(paid, ProviderError::PaymentRequired { .. }), "{paid:?}");
        assert!(paid.to_string().contains("isn't free on your account"), "{paid}");
        // Gemini's OpenAI-compatible endpoint answers with an array around the error object.
        let gemini = r#"[{"error": {"code": 500, "message": "Internal error encountered.", "status": "INTERNAL"}}]"#;
        assert_eq!(extract_message(gemini), "Internal error encountered.");
        assert_eq!(extract_kind(gemini).as_deref(), Some("INTERNAL"));
        assert_eq!(extract_message("plain text"), "plain text");
    }
}
