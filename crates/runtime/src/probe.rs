//! Asks an address what it is. Each runtime has an endpoint only it answers in its own shape, tried
//! from most to least specific, so a port number never decides.

use crate::STEP_TIMEOUT;
use crate::known::RuntimeKind;

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Outcome {
    Identified {
        kind: RuntimeKind,
        version: Option<String>,
    },
    /// Something spoke HTTP, but not like any runtime re-code knows.
    Unidentified {
        summary: String,
    },
    /// Connection refused, or no answer in time.
    Silent,
}

/// Turns what people write into a URL: `http://host:port`, `host:port`, `:port`, or `0.0.0.0:port`
/// (which means "this machine" for a client).
#[must_use]
pub fn normalize_url(raw: &str) -> Option<String> {
    let raw = raw.trim().trim_end_matches('/');
    if raw.is_empty() {
        return None;
    }
    let with_scheme = if raw.starts_with("http://") || raw.starts_with("https://") {
        raw.to_owned()
    } else if let Some(port) = raw.strip_prefix(':') {
        format!("http://127.0.0.1:{port}")
    } else {
        format!("http://{raw}")
    };
    let url = reqwest::Url::parse(&with_scheme).ok()?;
    let host = url.host_str()?;
    let host = match host {
        "0.0.0.0" | "[::]" | "::" => "127.0.0.1",
        other => other,
    };
    let port = url.port_or_known_default()?;
    Some(format!("{}://{host}:{port}", url.scheme()))
}

/// The port in a URL, if any.
#[must_use]
pub fn port_of(url: &str) -> Option<u16> {
    reqwest::Url::parse(url).ok()?.port_or_known_default()
}

/// Whether two URLs reach the same service: same scheme and port, and hosts that are the same or
/// are both this machine (`localhost`, `127.0.0.1`, `::1`).
#[must_use]
pub fn same_endpoint(a: &str, b: &str) -> bool {
    let (Ok(a), Ok(b)) = (reqwest::Url::parse(a), reqwest::Url::parse(b)) else { return a == b };
    if a.scheme() != b.scheme() || a.port_or_known_default() != b.port_or_known_default() {
        return false;
    }
    let loopback = |url: &reqwest::Url| matches!(url.host_str(), Some("localhost" | "127.0.0.1" | "[::1]" | "::1"));
    a.host_str() == b.host_str() || (loopback(&a) && loopback(&b))
}

fn client() -> Option<reqwest::Client> {
    reqwest::Client::builder().timeout(STEP_TIMEOUT).connect_timeout(STEP_TIMEOUT).build().ok()
}

enum Answer {
    Json(serde_json::Value),
    Text(String),
    Other { status: u16 },
    Silent,
}

async fn get(client: &reqwest::Client, url: &str, path: &str) -> Answer {
    match client.get(format!("{url}{path}")).send().await {
        Ok(response) => {
            let status = response.status().as_u16();
            if !(200..300).contains(&status) {
                return Answer::Other { status };
            }
            // Every endpoint asked here answers briefly; cap the body anyway.
            match response.text().await {
                Ok(text) if text.len() <= 64 * 1024 => match serde_json::from_str::<serde_json::Value>(&text) {
                    Ok(value) => Answer::Json(value),
                    Err(_) => Answer::Text(text),
                },
                _ => Answer::Other { status },
            }
        }
        Err(_) => Answer::Silent,
    }
}

/// Identifies what answers at `url` (scheme, host and port, no path).
pub async fn identify(url: &str) -> Outcome {
    let Some(client) = client() else { return Outcome::Silent };

    // Ollama greets with plain text at the root, which nothing else does. (Open WebUI copies
    // `/api/version`, so that alone would misidentify it.)
    let mut saw_http: Option<u16> = match get(&client, url, "/").await {
        Answer::Text(text) if text.trim() == "Ollama is running" => {
            let version = match get(&client, url, "/api/version").await {
                Answer::Json(value) => value.get("version").and_then(|v| v.as_str()).map(str::to_owned),
                _ => None,
            };
            return Outcome::Identified { kind: RuntimeKind::Ollama, version };
        }
        Answer::Json(_) | Answer::Text(_) => Some(200),
        Answer::Other { status } => Some(status),
        // A refused connection here means nothing is listening; skip the rest quickly.
        Answer::Silent => return Outcome::Silent,
    };
    // llama.cpp's server: /props carries its build and generation settings.
    if let Answer::Json(value) = get(&client, url, "/props").await
        && (value.get("default_generation_settings").is_some() || value.get("build_info").is_some())
    {
        let version = value.get("build_info").and_then(|v| v.as_str()).map(str::to_owned);
        return Outcome::Identified { kind: RuntimeKind::LlamaCpp, version };
    }
    // LM Studio: its own REST API lists models with a `type`.
    if let Answer::Json(value) = get(&client, url, "/api/v0/models").await
        && value
            .get("data")
            .and_then(|d| d.as_array())
            .is_some_and(|models| models.iter().all(|m| m.get("type").is_some()))
    {
        return Outcome::Identified { kind: RuntimeKind::LmStudio, version: None };
    }
    // vLLM: /version.
    if let Answer::Json(value) = get(&client, url, "/version").await
        && let Some(version) = value.get("version").and_then(|v| v.as_str())
    {
        return Outcome::Identified { kind: RuntimeKind::Vllm, version: Some(version.to_owned()) };
    }
    // Anything speaking the OpenAI models API.
    match get(&client, url, "/v1/models").await {
        Answer::Json(value) if value.get("data").is_some_and(serde_json::Value::is_array) => {
            return Outcome::Identified { kind: RuntimeKind::OpenAiCompatible, version: None };
        }
        Answer::Json(_) | Answer::Text(_) | Answer::Other { .. } => {
            saw_http.get_or_insert(200);
        }
        Answer::Silent => {}
    }
    match saw_http {
        Some(status) => {
            Outcome::Unidentified { summary: format!("answers HTTP {status}, but not like a runtime re-code knows") }
        }
        None => Outcome::Silent,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use wiremock::matchers::{method, path};
    use wiremock::{Mock, MockServer, ResponseTemplate};

    #[test]
    fn addresses_are_normalised_for_a_client() {
        assert_eq!(normalize_url("http://localhost:11434/").as_deref(), Some("http://localhost:11434"));
        assert_eq!(normalize_url("0.0.0.0:11434").as_deref(), Some("http://127.0.0.1:11434"));
        assert_eq!(normalize_url(":8080").as_deref(), Some("http://127.0.0.1:8080"));
        assert_eq!(normalize_url("https://ai.example.com").as_deref(), Some("https://ai.example.com:443"));
        assert_eq!(normalize_url(""), None);
        assert_eq!(port_of("http://127.0.0.1:11434"), Some(11434));
        assert!(same_endpoint("http://localhost:11434", "http://127.0.0.1:11434"));
        assert!(!same_endpoint("http://localhost:11434", "http://127.0.0.1:8080"));
        assert!(!same_endpoint("http://ai.lan:11434", "http://127.0.0.1:11434"));
    }

    async fn server_with(routes: &[(&str, serde_json::Value)]) -> MockServer {
        let server = MockServer::start().await;
        for (route, body) in routes {
            Mock::given(method("GET"))
                .and(path(*route))
                .respond_with(ResponseTemplate::new(200).set_body_json(body))
                .mount(&server)
                .await;
        }
        Mock::given(method("GET")).respond_with(ResponseTemplate::new(404)).mount(&server).await;
        server
    }

    #[tokio::test]
    async fn each_runtime_is_recognised_by_its_own_endpoint() {
        let ollama = MockServer::start().await;
        Mock::given(method("GET"))
            .and(path("/"))
            .respond_with(ResponseTemplate::new(200).set_body_string("Ollama is running"))
            .mount(&ollama)
            .await;
        Mock::given(method("GET"))
            .and(path("/api/version"))
            .respond_with(ResponseTemplate::new(200).set_body_json(serde_json::json!({"version": "0.40.14"})))
            .mount(&ollama)
            .await;
        assert_eq!(
            identify(&ollama.uri()).await,
            Outcome::Identified { kind: RuntimeKind::Ollama, version: Some("0.40.14".into()) }
        );
        // Open WebUI answers /api/version in the same shape, but it isn't Ollama.
        let webui = server_with(&[
            ("/api/version", serde_json::json!({"version": "0.13.0"})),
            ("/", serde_json::json!({"html": true})),
        ])
        .await;
        assert!(matches!(identify(&webui.uri()).await, Outcome::Unidentified { .. }));
        let llama = server_with(&[
            ("/props", serde_json::json!({"build_info": "b5000-abc", "default_generation_settings": {}})),
            ("/v1/models", serde_json::json!({"data": []})),
        ])
        .await;
        assert_eq!(
            identify(&llama.uri()).await,
            Outcome::Identified { kind: RuntimeKind::LlamaCpp, version: Some("b5000-abc".into()) }
        );
        let lm = server_with(&[("/api/v0/models", serde_json::json!({"data": [{"id": "m", "type": "llm"}]}))]).await;
        assert_eq!(identify(&lm.uri()).await, Outcome::Identified { kind: RuntimeKind::LmStudio, version: None });
        let vllm = server_with(&[
            ("/version", serde_json::json!({"version": "0.8.0"})),
            ("/v1/models", serde_json::json!({"data": []})),
        ])
        .await;
        assert_eq!(
            identify(&vllm.uri()).await,
            Outcome::Identified { kind: RuntimeKind::Vllm, version: Some("0.8.0".into()) }
        );
        let generic = server_with(&[("/v1/models", serde_json::json!({"data": [{"id": "x"}]}))]).await;
        assert_eq!(
            identify(&generic.uri()).await,
            Outcome::Identified { kind: RuntimeKind::OpenAiCompatible, version: None }
        );
    }

    #[tokio::test]
    async fn an_unknown_web_server_is_reported_not_guessed() {
        let web = server_with(&[("/", serde_json::json!({"hello": "world"}))]).await;
        assert!(
            matches!(identify(&web.uri()).await, Outcome::Unidentified { summary } if summary.contains("HTTP 200"))
        );
        assert_eq!(identify("http://127.0.0.1:9").await, Outcome::Silent);
    }
}
