//! Model capabilities, each recording where the claim came from.
//!
//! Measured on 2026-11-06: 3 of 8 local models that declared `tools` failed every tool-call
//! test. A declared capability is a hint, so callers need to know how far to trust it.

use crate::ids::UnixMillis;
use serde::{Deserialize, Serialize};

/// Where a capability claim came from, weakest first.
#[derive(Debug, Clone, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case", tag = "kind")]
pub enum Source {
    /// The server or model metadata says so.
    Declared,
    /// From compatibility data shipped with re-code.
    Profile { id: String },
    /// Tested by the conformance probe.
    Probed { at: UnixMillis, runtime_version: String },
    /// Accumulated from real use.
    Measured { samples: u32 },
}

/// A value and its provenance.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Capability<T> {
    pub value: T,
    pub source: Source,
}

impl<T> Capability<T> {
    #[must_use]
    pub fn declared(value: T) -> Self {
        Self { value, source: Source::Declared }
    }
}

/// What is known about a model. `None` means unknown, which is different from unsupported.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCapabilities {
    pub completion: Option<Capability<bool>>,
    pub tools: Option<Capability<bool>>,
    pub vision: Option<Capability<bool>>,
    pub thinking: Option<Capability<bool>>,
    /// The largest context the model supports, in tokens.
    pub context_limit: Option<Capability<u64>>,
    /// Bytes of key/value cache per token of context: from the model's layer and head counts
    /// (declared, assuming 16-bit cache entries), or measured from a loaded model.
    #[serde(default)]
    pub kv_bytes_per_token: Option<Capability<u64>>,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn stronger_evidence_sorts_higher() {
        let declared = Source::Declared;
        let profile = Source::Profile { id: "x".into() };
        let probed = Source::Probed { at: UnixMillis(0), runtime_version: "0".into() };
        let measured = Source::Measured { samples: 1 };
        assert!(declared < profile && profile < probed && probed < measured);
    }
}
