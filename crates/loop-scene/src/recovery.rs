//! Failure classification, deduplication, backoff, and escalation.

use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};

/// Stable failure classes controlling autonomous response.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum FailureClass {
    /// Provider timeout/rate/server failure.
    TransientApi,
    /// Invalid typed model response.
    ModelSchema,
    /// Blender Python/process error.
    BlenderScript,
    /// Crash, OOM, or timeout.
    Resource,
    /// Bounds/connector/collision failure.
    Spatial,
    /// Appearance or missing semantic feature.
    Visual,
    /// Missing/corrupt external asset.
    ExternalAsset,
    /// Native/interchange mismatch.
    Export,
    /// Local vector indexing failure.
    Index,
    /// Auth/disk/hard incompatibility.
    ExternalBlocker,
    /// Explicit cancellation.
    Cancelled,
    /// Unclassified local error.
    Unknown,
}

/// Recovery escalation selected after fingerprint history.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RecoveryAction {
    /// Retry identical transient operation after backoff.
    Retry,
    /// Adjust parameters or generated code.
    RepairLocal,
    /// Select another recipe/retrieved candidate.
    ReplaceStrategy,
    /// Redesign lowest affected subtree.
    RedesignSubtree,
    /// Re-open parent allocation revision.
    ReviseParent,
    /// Wait for external/resource condition.
    WaitExternal,
    /// Never restart after explicit cancellation.
    StopCancelled,
}

/// Classify an error without persisting secrets/provider bodies.
pub fn classify(message: &str) -> FailureClass {
    let s = message.to_lowercase();
    if s.contains("cancel") {
        FailureClass::Cancelled
    } else if s.contains("auth")
        || s.contains("api key")
        || s.contains("disk")
        || s.contains("no space")
        || s.contains("not found; set backend.blender")
        || s.contains("text-only")
    {
        FailureClass::ExternalBlocker
    } else if s.contains("429")
        || s.contains("rate limit")
        || s.contains("timeout")
        || s.contains("temporar")
        || s.contains("503")
    {
        FailureClass::TransientApi
    } else if s.contains("json") || s.contains("schema") || s.contains("model returned") {
        FailureClass::ModelSchema
    } else if s.contains("connector")
        || s.contains("bounds")
        || s.contains("collision")
        || s.contains("contain")
    {
        FailureClass::Spatial
    } else if s.contains("glb") || s.contains("export") || s.contains("parity") {
        FailureClass::Export
    } else if s.contains("embedding") || s.contains("index") {
        FailureClass::Index
    } else if s.contains("texture") || s.contains("asset") {
        FailureClass::ExternalAsset
    } else if s.contains("blender exited") || s.contains("traceback") || s.contains("python") {
        FailureClass::BlenderScript
    } else if s.contains("oom") || s.contains("out of memory") || s.contains("killed") {
        FailureClass::Resource
    } else {
        FailureClass::Unknown
    }
}

/// Privacy-safe repeat fingerprint from class, task/node, and normalized error.
pub fn fingerprint(class: FailureClass, scope: &str, message: &str) -> String {
    let normalized = message
        .to_lowercase()
        .split_whitespace()
        .map(|w| {
            if w.chars().any(|c| c.is_ascii_digit()) {
                w.chars()
                    .map(|c| if c.is_ascii_digit() { '#' } else { c })
                    .collect()
            } else {
                w.into()
            }
        })
        .collect::<Vec<String>>()
        .join(" ");
    let digest = Sha256::digest(format!("{class:?}\0{scope}\0{normalized}").as_bytes());
    hex::encode(digest)
}

/// Select escalation. Identical attempts do not count as new repairs; callers
/// pass how often the same fingerprint has recurred.
pub fn action(
    class: FailureClass,
    same_fingerprint_count: u32,
    attempts_per_strategy: u32,
) -> RecoveryAction {
    if class == FailureClass::Cancelled {
        return RecoveryAction::StopCancelled;
    }
    if class == FailureClass::ExternalBlocker {
        return RecoveryAction::WaitExternal;
    }
    let tier = same_fingerprint_count / attempts_per_strategy.max(1);
    match tier {
        0 => RecoveryAction::Retry,
        1 => RecoveryAction::RepairLocal,
        2 => RecoveryAction::ReplaceStrategy,
        3 => RecoveryAction::RedesignSubtree,
        _ => RecoveryAction::ReviseParent,
    }
}

/// Exponential backoff with deterministic fingerprint jitter, capped by policy.
pub fn backoff_ms(attempt: u32, max_seconds: u64, fingerprint: &str) -> u64 {
    let base = 500u64.saturating_mul(1u64 << attempt.min(16));
    let jitter =
        u64::from_str_radix(&fingerprint[..fingerprint.len().min(8)], 16).unwrap_or(0) % 501;
    (base + jitter).min(max_seconds.saturating_mul(1000))
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn repeated_failure_escalates() {
        let f = fingerprint(FailureClass::Spatial, "node", "bounds 2.01 exceeded");
        assert_eq!(
            f,
            fingerprint(FailureClass::Spatial, "node", "bounds 9.44 exceeded")
        );
        assert_eq!(
            action(FailureClass::Spatial, 6, 3),
            RecoveryAction::ReplaceStrategy
        );
        assert!(backoff_ms(20, 10, &f) <= 10_000);
    }
}
