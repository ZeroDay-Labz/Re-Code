//! Cloud provider configuration. Secrets are never stored here: an API key lives in the OS keyring,
//! and this only records which providers exist and how to reach them.

use serde::{Deserialize, Serialize};

/// A family of cloud API, which decides the wire format and default address.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProviderKind {
    /// Anthropic's native Messages API.
    Anthropic,
    /// OpenAI's Chat Completions API.
    Openai,
    /// Google Gemini, through its OpenAI-compatible endpoint.
    Gemini,
    /// Any other server that speaks the OpenAI Chat Completions API.
    OpenaiCompatible,
}

impl ProviderKind {
    /// Every kind, for listing in a form.
    pub const ALL: [Self; 4] = [Self::Anthropic, Self::Openai, Self::Gemini, Self::OpenaiCompatible];

    #[must_use]
    pub fn label(self) -> &'static str {
        match self {
            Self::Anthropic => "Anthropic",
            Self::Openai => "OpenAI",
            Self::Gemini => "Google Gemini",
            Self::OpenaiCompatible => "OpenAI-compatible",
        }
    }

    /// The address used when none is given. `None` means one must be supplied.
    #[must_use]
    pub fn default_base_url(self) -> Option<&'static str> {
        match self {
            Self::Anthropic => Some("https://api.anthropic.com"),
            Self::Openai => Some("https://api.openai.com/v1"),
            Self::Gemini => Some("https://generativelanguage.googleapis.com/v1beta/openai"),
            Self::OpenaiCompatible => None,
        }
    }

    /// Where someone creates a key, shown as guidance.
    #[must_use]
    pub fn key_help(self) -> &'static str {
        match self {
            Self::Anthropic => "Create an API key at console.anthropic.com. Claude subscriptions can't be used here.",
            Self::Openai => "Create an API key at platform.openai.com.",
            Self::Gemini => "Create an API key at aistudio.google.com.",
            Self::OpenaiCompatible => "Use the key from your provider. Leave it blank if the server needs none.",
        }
    }

    /// Whether this kind speaks the OpenAI wire format.
    #[must_use]
    pub fn is_openai_style(self) -> bool {
        !matches!(self, Self::Anthropic)
    }
}

/// A one-click cloud provider offered as a preset.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ProviderPreset {
    pub name: &'static str,
    pub kind: ProviderKind,
    /// Overrides the kind's default address; empty uses the default.
    pub base_url: &'static str,
    pub note: &'static str,
    /// Where to sign up and get a key.
    pub signup_url: &'static str,
    /// Offers models that cost nothing (a free account key is still needed).
    pub free_models: bool,
}

/// The host of a base URL (`https://openrouter.ai/api/v1` → `openrouter.ai`), without a port.
#[must_use]
pub fn host(base_url: &str) -> Option<&str> {
    let rest = base_url.trim().split_once("://").map_or(base_url.trim(), |(_, rest)| rest);
    let host = rest.split(['/', '?', '#']).next()?.split(':').next()?.trim();
    (!host.is_empty()).then_some(host)
}

/// Why a provider's $0 models can't be used from here, when its terms say so. OpenCode's
/// documentation (console/models, 2026-11): "Free-tier chat models require OpenCode; use paid
/// models or Go from other clients." Nothing in its listing marks this, so it's known here.
#[must_use]
pub fn free_models_client_only(base_url: &str) -> Option<&'static str> {
    match host(base_url)? {
        "opencode.ai" => Some("OpenCode serves its free models only to its own app; its paid models work from here"),
        _ => None,
    }
}

impl ProviderPreset {
    /// The preset with this name.
    #[must_use]
    pub fn named(name: &str) -> Option<&'static ProviderPreset> {
        PROVIDER_PRESETS.iter().find(|preset| preset.name == name)
    }
}

/// Built-in cloud providers. OpenRouter and OpenCode Zen host free models (a free account key is
/// still needed); the catalog flags which models are free.
pub const PROVIDER_PRESETS: &[ProviderPreset] = &[
    ProviderPreset {
        name: "openrouter",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://openrouter.ai/api/v1",
        note: "Hundreds of models, including many free ones. Key from openrouter.ai.",
        signup_url: "https://openrouter.ai/keys",
        free_models: true,
    },
    ProviderPreset {
        name: "opencode-zen",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://opencode.ai/inference/openai/v1",
        note: "OpenCode's gateway. Paid models work from here; its free models need the OpenCode app. Key from opencode.ai.",
        signup_url: "https://opencode.ai/auth",
        free_models: false,
    },
    ProviderPreset {
        name: "ollama-cloud",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://ollama.com/v1",
        note: "Ollama's hosted models (free plan with limits). Key from ollama.com; or `ollama signin` and use them through local Ollama.",
        signup_url: "https://ollama.com/settings/keys",
        free_models: false,
    },
    ProviderPreset {
        name: "nvidia",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://integrate.api.nvidia.com/v1",
        note: "NVIDIA Build hosted models, with trial credits. Key from build.nvidia.com.",
        signup_url: "https://build.nvidia.com/settings/api-keys",
        free_models: false,
    },
    ProviderPreset {
        name: "huggingface",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://router.huggingface.co/v1",
        note: "Hugging Face Inference Providers router, with a small free monthly credit. Token from huggingface.co.",
        signup_url: "https://huggingface.co/settings/tokens",
        free_models: false,
    },
    ProviderPreset {
        name: "mistral",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://api.mistral.ai/v1",
        note: "Mistral models, with a free experiment tier. Key from console.mistral.ai.",
        signup_url: "https://console.mistral.ai/api-keys",
        free_models: false,
    },
    ProviderPreset {
        name: "deepseek",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://api.deepseek.com/v1",
        note: "DeepSeek's own API. Key from platform.deepseek.com.",
        signup_url: "https://platform.deepseek.com/api_keys",
        free_models: false,
    },
    ProviderPreset {
        name: "together",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://api.together.xyz/v1",
        note: "Together AI open-weights models. Key from api.together.ai.",
        signup_url: "https://api.together.ai/settings/api-keys",
        free_models: false,
    },
    ProviderPreset {
        name: "fireworks",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://api.fireworks.ai/inference/v1",
        note: "Fireworks AI. Key from app.fireworks.ai.",
        signup_url: "https://app.fireworks.ai/account/api-keys",
        free_models: false,
    },
    ProviderPreset {
        name: "moonshot",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://api.moonshot.ai/v1",
        note: "Moonshot's Kimi models. Key from platform.kimi.ai.",
        signup_url: "https://platform.kimi.ai/console/api-keys",
        free_models: false,
    },
    ProviderPreset {
        name: "zai",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://api.z.ai/api/paas/v4",
        note: "Z.AI's GLM models. Key from z.ai.",
        signup_url: "https://z.ai/manage-apikey/apikey-list",
        free_models: false,
    },
    ProviderPreset {
        name: "minimax",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://api.minimax.io/v1",
        note: "MiniMax models. Key from platform.minimax.io.",
        signup_url: "https://platform.minimax.io/console/keys",
        free_models: false,
    },
    ProviderPreset {
        name: "xai",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://api.x.ai/v1",
        note: "xAI's Grok models. Key from console.x.ai.",
        signup_url: "https://console.x.ai",
        free_models: false,
    },
    ProviderPreset {
        name: "novita",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://api.novita.ai/openai/v1",
        note: "Novita AI open-weights models. Key from novita.ai.",
        signup_url: "https://novita.ai/settings/key-management",
        free_models: false,
    },
    ProviderPreset {
        name: "groq",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://api.groq.com/openai/v1",
        note: "Very fast inference, generous free tier. Key from console.groq.com.",
        signup_url: "https://console.groq.com/keys",
        free_models: false,
    },
    ProviderPreset {
        name: "deepinfra",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://api.deepinfra.com/v1/openai",
        note: "Open-weights models, some free. Key from deepinfra.com.",
        signup_url: "https://deepinfra.com/dash/api_keys",
        free_models: false,
    },
    ProviderPreset {
        name: "cerebras",
        kind: ProviderKind::OpenaiCompatible,
        base_url: "https://api.cerebras.ai/v1",
        note: "Fast inference with a free tier. Key from cloud.cerebras.ai.",
        signup_url: "https://cloud.cerebras.ai",
        free_models: false,
    },
    ProviderPreset {
        name: "anthropic",
        kind: ProviderKind::Anthropic,
        base_url: "",
        note: "Claude models. API key from console.anthropic.com (not a Claude subscription).",
        signup_url: "https://console.anthropic.com/settings/keys",
        free_models: false,
    },
    ProviderPreset {
        name: "openai",
        kind: ProviderKind::Openai,
        base_url: "",
        note: "GPT models. Key from platform.openai.com.",
        signup_url: "https://platform.openai.com/api-keys",
        free_models: false,
    },
    ProviderPreset {
        name: "gemini",
        kind: ProviderKind::Gemini,
        base_url: "",
        note: "Google Gemini, with a free tier. Key from aistudio.google.com.",
        signup_url: "https://aistudio.google.com/apikey",
        free_models: false,
    },
];

/// A configured cloud provider.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CloudProvider {
    pub name: String,
    pub kind: ProviderKind,
    /// Overrides the kind's default address.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub base_url: Option<String>,
    #[serde(default = "default_true")]
    pub enabled: bool,
}

fn default_true() -> bool {
    true
}

impl CloudProvider {
    /// The address this provider is reached at.
    #[must_use]
    pub fn base_url(&self) -> Option<String> {
        self.base_url.clone().or_else(|| self.kind.default_base_url().map(str::to_owned))
    }
}

/// Builds a provider from what was typed. An empty address falls back to the kind's default.
pub fn parse_provider_form(
    name: &str,
    kind: ProviderKind,
    base_url: &str,
    enabled: bool,
) -> Result<CloudProvider, String> {
    let name = name.trim();
    super::extensions::validate_name("provider", name)?;
    let base_url = base_url.trim();
    let base_url = if base_url.is_empty() {
        if kind.default_base_url().is_none() {
            return Err("This kind needs a base URL, such as http://localhost:1234/v1.".to_owned());
        }
        None
    } else {
        if !base_url.starts_with("http://") && !base_url.starts_with("https://") {
            return Err("The base URL has to start with http:// or https://.".to_owned());
        }
        Some(base_url.trim_end_matches('/').to_owned())
    };
    Ok(CloudProvider { name: name.to_owned(), kind, base_url, enabled })
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_default_address_is_used_when_none_is_given() {
        let provider = parse_provider_form("claude", ProviderKind::Anthropic, "", true).unwrap();
        assert_eq!(provider.base_url().as_deref(), Some("https://api.anthropic.com"));
    }

    #[test]
    fn a_custom_kind_requires_an_address_and_checks_the_scheme() {
        assert!(parse_provider_form("x", ProviderKind::OpenaiCompatible, "", true).unwrap_err().contains("base URL"));
        assert!(
            parse_provider_form("x", ProviderKind::OpenaiCompatible, "ftp://x", true).unwrap_err().contains("http")
        );
        let provider =
            parse_provider_form("lm", ProviderKind::OpenaiCompatible, "http://localhost:1234/v1/", true).unwrap();
        assert_eq!(provider.base_url().as_deref(), Some("http://localhost:1234/v1"));
    }

    #[test]
    fn a_given_address_overrides_the_default() {
        let provider = parse_provider_form("p", ProviderKind::Openai, "https://proxy.example/v1", true).unwrap();
        assert_eq!(provider.base_url().as_deref(), Some("https://proxy.example/v1"));
    }

    #[test]
    fn names_are_checked() {
        assert!(parse_provider_form("", ProviderKind::Openai, "", true).is_err());
        assert!(parse_provider_form("../x", ProviderKind::Openai, "", true).is_err());
    }

    #[test]
    fn hosts_are_read_and_client_only_free_models_are_known_by_host() {
        assert_eq!(host("https://openrouter.ai/api/v1"), Some("openrouter.ai"));
        assert_eq!(host("http://localhost:11434/v1"), Some("localhost"));
        assert_eq!(host("opencode.ai/inference/openai/v1"), Some("opencode.ai"));
        assert_eq!(host("https://"), None);
        assert!(free_models_client_only("https://opencode.ai/inference/openai/v1").is_some());
        assert!(free_models_client_only("https://opencode.ai/zen/v1").is_some(), "the older path is the same host");
        assert!(free_models_client_only("https://openrouter.ai/api/v1").is_none());
        assert!(free_models_client_only("https://ollama.com/v1").is_none());
    }

    #[test]
    fn every_preset_has_an_https_address_and_a_sign_up_page() {
        for preset in PROVIDER_PRESETS {
            let address =
                if preset.base_url.is_empty() { preset.kind.default_base_url().unwrap() } else { preset.base_url };
            assert!(address.starts_with("https://"), "{}: {address}", preset.name);
            assert!(preset.signup_url.starts_with("https://"), "{}", preset.name);
            assert!(!preset.note.is_empty(), "{}", preset.name);
            assert!(ProviderPreset::named(preset.name).is_some());
        }
        let names: std::collections::BTreeSet<&str> = PROVIDER_PRESETS.iter().map(|p| p.name).collect();
        assert_eq!(names.len(), PROVIDER_PRESETS.len(), "preset names are unique");
        assert!(
            !ProviderPreset::named("opencode-zen").unwrap().free_models,
            "Zen's free models can't be used from here"
        );
    }
}
