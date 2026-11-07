//! What re-code knows how to recognise. Recognition is by image path or program name, never by a
//! container's name or a port number, which anyone can set to anything.

use serde::Serialize;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize)]
#[serde(rename_all = "snake_case")]
pub enum RuntimeKind {
    Ollama,
    LlamaCpp,
    Vllm,
    LmStudio,
    /// Answers `/v1/models` but wasn't recognised more precisely (LocalAI, SGLang, a proxy…).
    OpenAiCompatible,
}

impl RuntimeKind {
    #[must_use]
    pub fn name(self) -> &'static str {
        match self {
            Self::Ollama => "Ollama",
            Self::LlamaCpp => "llama.cpp",
            Self::Vllm => "vLLM",
            Self::LmStudio => "LM Studio",
            Self::OpenAiCompatible => "OpenAI-compatible server",
        }
    }

    /// For ids and JSON.
    #[must_use]
    pub fn slug(self) -> &'static str {
        match self {
            Self::Ollama => "ollama",
            Self::LlamaCpp => "llama-cpp",
            Self::Vllm => "vllm",
            Self::LmStudio => "lm-studio",
            Self::OpenAiCompatible => "openai-compatible",
        }
    }

    /// Whether re-code has a tested adapter for it. Everything else is "detected", not "supported".
    #[must_use]
    pub fn supported(self) -> bool {
        matches!(self, Self::Ollama)
    }
}

/// Loopback ports the common runtimes default to. Probed in addition to configured addresses; what
/// answers decides, not the number.
pub const DEFAULT_PORTS: [u16; 5] = [11434, 8080, 8000, 1234, 8081];

/// Image repositories and what they run. Matched on the repository path without registry or tag, so
/// `docker.io/ollama/ollama:rocm` and `ollama/ollama` both match `ollama/ollama`.
pub const IMAGES: &[(&str, RuntimeKind)] = &[
    ("ollama/ollama", RuntimeKind::Ollama),
    ("ggml-org/llama.cpp", RuntimeKind::LlamaCpp),
    ("ggerganov/llama.cpp", RuntimeKind::LlamaCpp),
    ("vllm/vllm-openai", RuntimeKind::Vllm),
    ("lmsysorg/sglang", RuntimeKind::OpenAiCompatible),
    ("localai/localai", RuntimeKind::OpenAiCompatible),
    ("go-skynet/local-ai", RuntimeKind::OpenAiCompatible),
];

/// Programs on `PATH` or in `/proc` and what they are.
pub const BINARIES: &[(&str, RuntimeKind)] =
    &[("ollama", RuntimeKind::Ollama), ("llama-server", RuntimeKind::LlamaCpp), ("lms", RuntimeKind::LmStudio)];

/// What an image runs, from its repository path.
#[must_use]
pub fn kind_of_image(image: &str) -> Option<RuntimeKind> {
    // Strip a digest, then a tag (but not a registry port), then a registry host.
    let without_digest = image.split('@').next().unwrap_or(image);
    let path_start = without_digest.find('/').map_or(0, |slash| {
        let first = &without_digest[..slash];
        // The first segment is a registry when it has a dot or a port, or is "localhost".
        if first.contains('.') || first.contains(':') || first == "localhost" { slash + 1 } else { 0 }
    });
    let path = &without_digest[path_start..];
    let repo = match path.rfind(':') {
        Some(colon) if !path[colon..].contains('/') => &path[..colon],
        _ => path,
    };
    let repo = repo.strip_prefix("library/").unwrap_or(repo);
    IMAGES.iter().find(|(known, _)| *known == repo).map(|(_, kind)| *kind)
}

/// What a program is, from its path or name.
#[must_use]
pub fn kind_of_binary(exe: &str) -> Option<RuntimeKind> {
    let name = exe.rsplit('/').next().unwrap_or(exe);
    BINARIES.iter().find(|(known, _)| *known == name).map(|(_, kind)| *kind)
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn images_match_by_repository_path_only() {
        assert_eq!(kind_of_image("docker.io/ollama/ollama:rocm"), Some(RuntimeKind::Ollama));
        assert_eq!(kind_of_image("ollama/ollama"), Some(RuntimeKind::Ollama));
        assert_eq!(kind_of_image("localhost:5000/ollama/ollama:latest"), Some(RuntimeKind::Ollama));
        assert_eq!(kind_of_image("ghcr.io/ggml-org/llama.cpp:server-cuda"), Some(RuntimeKind::LlamaCpp));
        assert_eq!(kind_of_image("vllm/vllm-openai@sha256:abc"), Some(RuntimeKind::Vllm));
        assert_eq!(kind_of_image("ghcr.io/open-webui/open-webui:main"), None);
        assert_eq!(kind_of_image("example/llama-ollama-tools:1"), None, "a name containing 'ollama' isn't Ollama");
    }

    #[test]
    fn binaries_match_by_name() {
        assert_eq!(kind_of_binary("/bin/ollama"), Some(RuntimeKind::Ollama));
        assert_eq!(kind_of_binary("llama-server"), Some(RuntimeKind::LlamaCpp));
        assert_eq!(kind_of_binary("/usr/bin/pasta.avx2"), None);
    }
}
