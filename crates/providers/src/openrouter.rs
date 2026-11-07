//! What OpenRouter knows about a key: whether credits have ever been bought (which sets the daily
//! allowance for its free models) and how much of that allowance is used today. Read from
//! `GET /api/v1/key` (OpenRouter's documented endpoint, 2026-11), so the figures are OpenRouter's
//! own, never estimated here. Nothing in this module applies to other hosts.

use crate::ProviderError;
use crate::http::decode_json;
use serde::Deserialize;
use std::time::Duration;

const TIMEOUT: Duration = Duration::from_secs(10);

/// Whether a base URL is OpenRouter's API.
#[must_use]
pub fn is_openrouter(base_url: &str) -> bool {
    recode_core::providers::host(base_url)
        .is_some_and(|host| host == "openrouter.ai" || host.ends_with(".openrouter.ai"))
}

/// A key's standing, as OpenRouter reports it.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyStatus {
    /// `true` until credits have been bought at least once.
    pub free_tier: bool,
    /// Requests to free models today (UTC): used, limit, remaining.
    pub free_daily: Option<(u64, u64, u64)>,
    /// Credits used in all, in US dollars.
    pub usage: Option<f64>,
    /// Credits left on the key's limit, in US dollars; `None` when the key has no limit.
    pub limit_remaining: Option<f64>,
}

impl KeyStatus {
    /// One or two sentences for the Providers tab and the home message.
    #[must_use]
    pub fn summary(&self) -> String {
        let mut out = String::new();
        match self.free_daily {
            Some((used, limit, _)) => {
                out.push_str(&format!("Free models: {used} of {limit} requests used today (UTC)"))
            }
            None => out.push_str("Free models: today's allowance wasn't reported"),
        }
        if self.free_tier {
            out.push_str(". No credits bought yet, so the daily allowance is the small one; OpenRouter raises it once $10 of credits has ever been bought.");
        } else {
            out.push('.');
            if let Some(left) = self.limit_remaining {
                out.push_str(&format!(" Credits left on this key: ${left:.2}."));
            }
        }
        out
    }
}

/// Asks OpenRouter about the key. Needs the key; an empty one is refused before any request.
pub async fn key_status(base_url: &str, api_key: &str) -> Result<KeyStatus, ProviderError> {
    let url = format!("{}/key", base_url.trim_end_matches('/'));
    if api_key.trim().is_empty() {
        return Err(ProviderError::NeedsKey { url, message: "no key to check".to_owned() });
    }
    let client = reqwest::Client::builder()
        .connect_timeout(TIMEOUT)
        .build()
        .map_err(|error| ProviderError::Unreachable { url: url.clone(), reason: error.to_string() })?;
    let response = client.get(&url).bearer_auth(api_key.trim()).timeout(TIMEOUT).send().await;
    let wire: Wire = decode_json(&url, response).await?;
    Ok(wire.data.into())
}

#[derive(Deserialize)]
struct Wire {
    data: WireData,
}

#[derive(Deserialize, Default)]
struct WireData {
    #[serde(default)]
    is_free_tier: bool,
    free_model_daily_requests: Option<WireDaily>,
    usage: Option<f64>,
    limit_remaining: Option<f64>,
}

#[derive(Deserialize)]
struct WireDaily {
    used: u64,
    limit: u64,
    remaining: u64,
}

impl From<WireData> for KeyStatus {
    fn from(data: WireData) -> Self {
        Self {
            free_tier: data.is_free_tier,
            free_daily: data.free_model_daily_requests.map(|d| (d.used, d.limit, d.remaining)),
            usage: data.usage,
            limit_remaining: data.limit_remaining,
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{header, method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn only_openrouter_hosts_match() {
        assert!(is_openrouter("https://openrouter.ai/api/v1"));
        assert!(is_openrouter("https://openrouter.ai/api/v1/"));
        assert!(!is_openrouter("https://opencode.ai/inference/openai/v1"));
        assert!(!is_openrouter("http://localhost:11434/v1"));
        assert!(!is_openrouter("https://notopenrouter.ai/v1"));
    }

    #[tokio::test]
    async fn the_key_endpoint_is_read_and_summarised() {
        let server = MockServer::start().await;
        // Shape from OpenRouter's documentation, 2026-11.
        Mock::given(method("GET"))
            .and(path("/key"))
            .and(header("authorization", "Bearer sk-or-test"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"data": {
                "label": "sk-or-v1-abc", "limit": null, "limit_remaining": null, "usage": 0.0,
                "is_free_tier": true,
                "free_model_daily_requests": {"used": 3, "limit": 50, "remaining": 47},
                "rate_limit": {"requests": 20, "interval": "10s"}
            }})))
            .mount(&server)
            .await;
        let status = key_status(&server.uri(), "sk-or-test").await.unwrap();
        assert_eq!(status.free_daily, Some((3, 50, 47)));
        assert!(status.free_tier);
        let text = status.summary();
        assert!(text.contains("3 of 50 requests used today"), "{text}");
        assert!(text.contains("No credits bought yet"), "{text}");

        let paid = KeyStatus {
            free_tier: false,
            free_daily: Some((10, 1000, 990)),
            usage: Some(2.5),
            limit_remaining: Some(7.5),
        };
        let text = paid.summary();
        assert!(text.contains("10 of 1000") && text.contains("$7.50"), "{text}");
        assert!(!text.contains("No credits"), "{text}");

        let error = key_status(&server.uri(), "  ").await.unwrap_err();
        assert!(matches!(error, ProviderError::NeedsKey { .. }));
    }
}
