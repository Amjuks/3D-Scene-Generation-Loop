//! Versioned, strict scene configuration and deterministic overlays.

use std::collections::BTreeMap;
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::{Result, SceneError};

/// Top-level scene configuration schema.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneConfig {
    /// Schema version. Version 1 fixes meters, right-handed +Z-up coordinates.
    pub version: u32,
    /// Generation/export backend selection.
    #[serde(default)]
    pub backend: BackendConfig,
    /// Soket models assigned to typed roles.
    #[serde(default)]
    pub models: ModelsConfig,
    /// Local semantic embedding/index configuration.
    #[serde(default)]
    pub embedding: EmbeddingConfig,
    /// Process and queue limits.
    #[serde(default)]
    pub execution: ExecutionConfig,
    /// Global run budgets.
    #[serde(default)]
    pub budgets: BudgetConfig,
    /// Recovery policy.
    #[serde(default)]
    pub recovery: RecoveryConfig,
    /// Geometry and rendering acceptance settings.
    #[serde(default)]
    pub quality: QualityConfig,
    /// Durable output/library paths and retention.
    #[serde(default)]
    pub storage: StorageConfig,
    /// Reuse/download policy.
    #[serde(default)]
    pub assets: AssetPolicy,
}

impl Default for SceneConfig {
    fn default() -> Self {
        Self {
            version: 1,
            backend: BackendConfig::default(),
            models: ModelsConfig::default(),
            embedding: EmbeddingConfig::default(),
            execution: ExecutionConfig::default(),
            budgets: BudgetConfig::default(),
            recovery: RecoveryConfig::default(),
            quality: QualityConfig::default(),
            storage: StorageConfig::default(),
            assets: AssetPolicy::default(),
        }
    }
}

/// Blender backend configuration.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BackendConfig {
    /// Generator adapter. V1 supports `blender`.
    #[serde(default = "blender")]
    pub generator: String,
    /// Pinned variation/recipe family.
    #[serde(default = "variation")]
    pub variation: String,
    /// Preview renderer (`eevee`, `cycles`, or `workbench`).
    #[serde(default = "eevee")]
    pub preview_renderer: String,
    /// Inspection viewer (`threejs`).
    #[serde(default = "threejs")]
    pub viewer: String,
    /// Required final formats.
    #[serde(default = "exports")]
    pub exports: Vec<String>,
    /// Optional explicit Blender executable.
    #[serde(default)]
    pub blender_path: Option<PathBuf>,
    /// Accepted Blender major.minor prefix, when set.
    #[serde(default = "blender_version")]
    pub required_blender_version: Option<String>,
}

impl Default for BackendConfig {
    fn default() -> Self {
        Self {
            generator: blender(),
            variation: variation(),
            preview_renderer: eevee(),
            viewer: threejs(),
            exports: exports(),
            blender_path: None,
            required_blender_version: blender_version(),
        }
    }
}

fn blender() -> String {
    "blender".into()
}
fn variation() -> String {
    "procedural_pbr_v1".into()
}
fn eevee() -> String {
    "eevee".into()
}
fn threejs() -> String {
    "threejs".into()
}
fn exports() -> Vec<String> {
    vec!["blend".into(), "glb".into()]
}
fn blender_version() -> Option<String> {
    Some("4.5".into())
}

/// Provider and role model assignment.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ModelsConfig {
    /// Ordered alternate model IDs for transient/empty response recovery.
    #[serde(default)]
    pub fallback_models: Vec<String>,
    /// Request compact recipe layouts and expand bookkeeping deterministically.
    #[serde(default)]
    pub compact_planning: bool,
    /// Must be `soket` for v1.
    #[serde(default = "soket")]
    pub provider: String,
    /// Default Soket model ID.
    #[serde(default = "default_model")]
    pub default_model: String,
    /// Per-role overrides.
    #[serde(default)]
    pub roles: BTreeMap<String, RoleModelConfig>,
}
impl Default for ModelsConfig {
    fn default() -> Self {
        Self {
            provider: soket(),
            compact_planning: false,
            fallback_models: Vec::new(),
            default_model: default_model(),
            roles: BTreeMap::new(),
        }
    }
}
fn soket() -> String {
    "soket".into()
}
fn default_model() -> String {
    "qwen3-30b".into()
}

/// One role's model controls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RoleModelConfig {
    /// Model ID.
    pub model: String,
    /// Reasoning level (`off`, `low`, `medium`, `high`).
    #[serde(default = "off")]
    pub reasoning: String,
    /// Whether streamed transport is requested.
    #[serde(default = "yes")]
    pub stream: bool,
    /// Require verified image modality.
    #[serde(default)]
    pub requires_images: bool,
    /// Request timeout.
    #[serde(default = "model_timeout")]
    pub timeout_seconds: u64,
    /// Maximum output tokens.
    #[serde(default = "model_tokens")]
    pub max_tokens: u32,
}
fn off() -> String {
    "off".into()
}
fn yes() -> bool {
    true
}
pub(crate) fn model_timeout() -> u64 {
    600
}
fn model_tokens() -> u32 {
    8192
}

/// Local embedding backend.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct EmbeddingConfig {
    /// `semantic_hash_v1` or `command`.
    #[serde(default = "semantic_hash")]
    pub backend: String,
    /// Versioned model/feature set name.
    #[serde(default = "semantic_hash_model")]
    pub model: String,
    /// Vector dimension.
    #[serde(default = "embedding_dim")]
    pub dimensions: usize,
    /// External command for `command`; receives text on stdin and returns JSON float array.
    #[serde(default)]
    pub command: Option<PathBuf>,
}
impl Default for EmbeddingConfig {
    fn default() -> Self {
        Self {
            backend: semantic_hash(),
            model: semantic_hash_model(),
            dimensions: embedding_dim(),
            command: None,
        }
    }
}
fn semantic_hash() -> String {
    "semantic_hash_v1".into()
}
fn semantic_hash_model() -> String {
    "loop-semantic-hash-en-1".into()
}
fn embedding_dim() -> usize {
    384
}

/// Concurrency and subprocess controls.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExecutionConfig {
    /// Maximum simultaneous model tasks.
    #[serde(default = "one")]
    pub max_agent_tasks: usize,
    /// Maximum Blender processes.
    #[serde(default = "one")]
    pub max_blender_processes: usize,
    /// Maximum GPU renders.
    #[serde(default = "one")]
    pub max_gpu_renders: usize,
    /// Per-attempt process timeout.
    #[serde(default = "task_timeout")]
    pub task_timeout_seconds: u64,
    /// Lease timeout for interrupted attempts.
    #[serde(default = "lease_timeout")]
    pub lease_timeout_seconds: u64,
}
impl Default for ExecutionConfig {
    fn default() -> Self {
        Self {
            max_agent_tasks: 1,
            max_blender_processes: 1,
            max_gpu_renders: 1,
            task_timeout_seconds: task_timeout(),
            lease_timeout_seconds: lease_timeout(),
        }
    }
}
fn one() -> usize {
    1
}
fn task_timeout() -> u64 {
    600
}
fn lease_timeout() -> u64 {
    900
}

/// Global circuit-breaker budgets.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct BudgetConfig {
    /// Maximum model calls over the whole run.
    #[serde(default = "model_calls")]
    pub max_model_calls: u32,
    /// Maximum reported tokens over the whole run.
    #[serde(default = "total_tokens")]
    pub max_total_tokens: u64,
    /// Maximum wall time.
    #[serde(default = "wall_time")]
    pub max_wall_seconds: u64,
    /// Scene node circuit breaker.
    #[serde(default = "nodes")]
    pub max_nodes: usize,
    /// Hierarchy depth circuit breaker.
    #[serde(default = "depth")]
    pub max_depth: usize,
}
impl Default for BudgetConfig {
    fn default() -> Self {
        Self {
            max_model_calls: model_calls(),
            max_total_tokens: total_tokens(),
            max_wall_seconds: wall_time(),
            max_nodes: nodes(),
            max_depth: depth(),
        }
    }
}
fn model_calls() -> u32 {
    64
}
fn total_tokens() -> u64 {
    500_000
}
fn wall_time() -> u64 {
    14_400
}
fn nodes() -> usize {
    512
}
fn depth() -> usize {
    8
}

/// Durable recovery policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RecoveryConfig {
    /// Same-strategy transient attempts.
    #[serde(default = "three")]
    pub transient_retries_before_escalation: u32,
    /// Attempts per repair strategy.
    #[serde(default = "three")]
    pub repair_attempts_per_strategy: u32,
    /// Maximum external retry backoff.
    #[serde(default = "backoff")]
    pub external_retry_max_backoff_seconds: u64,
}
impl Default for RecoveryConfig {
    fn default() -> Self {
        Self {
            transient_retries_before_escalation: 3,
            repair_attempts_per_strategy: 3,
            external_retry_max_backoff_seconds: backoff(),
        }
    }
}
fn three() -> u32 {
    3
}
fn backoff() -> u64 {
    300
}

/// Geometry and preview quality policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct QualityConfig {
    /// General absolute containment tolerance in meters.
    #[serde(default = "containment")]
    pub containment_tolerance_m: f64,
    /// Relative bounds tolerance multiplied by the larger compared extent.
    #[serde(default = "relative")]
    pub relative_tolerance: f64,
    /// Connector translation tolerance.
    #[serde(default = "connector")]
    pub connector_tolerance_m: f64,
    /// Connector angular tolerance in degrees.
    #[serde(default = "angle")]
    pub connector_tolerance_degrees: f64,
    /// Preview pixel dimensions.
    #[serde(default = "resolution")]
    pub preview_resolution: [u32; 2],
    /// Samples per preview.
    #[serde(default = "samples")]
    pub preview_samples: u32,
    /// `draft`, `standard`, or `high`.
    #[serde(default = "standard")]
    pub preset: String,
    /// Triangle budget.
    #[serde(default = "triangles")]
    pub max_triangles: u64,
    /// Texture byte budget.
    #[serde(default = "texture_bytes")]
    pub max_texture_bytes: u64,
    /// Require image-model findings before acceptance.
    #[serde(default = "yes")]
    pub require_visual_review: bool,
}
impl Default for QualityConfig {
    fn default() -> Self {
        Self {
            containment_tolerance_m: containment(),
            relative_tolerance: relative(),
            connector_tolerance_m: connector(),
            connector_tolerance_degrees: angle(),
            preview_resolution: resolution(),
            preview_samples: samples(),
            preset: standard(),
            max_triangles: triangles(),
            max_texture_bytes: texture_bytes(),
            require_visual_review: true,
        }
    }
}
fn containment() -> f64 {
    0.005
}
fn relative() -> f64 {
    1e-5
}
fn connector() -> f64 {
    0.001
}
fn angle() -> f64 {
    0.1
}
fn resolution() -> [u32; 2] {
    [1280, 720]
}
fn samples() -> u32 {
    32
}
fn standard() -> String {
    "standard".into()
}
fn triangles() -> u64 {
    2_000_000
}
fn texture_bytes() -> u64 {
    256_000_000
}

/// Output and asset-library locations.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct StorageConfig {
    /// Root for immutable run directories.
    #[serde(default = "output_root")]
    pub output_root: PathBuf,
    /// Library location for standalone/preflight use. Scene execution overrides
    /// this with the individual run's `asset-library` directory to isolate reuse.
    #[serde(default = "library_root")]
    pub asset_library: PathBuf,
    /// Minimum free bytes required before a run.
    #[serde(default = "min_free")]
    pub min_free_bytes: u64,
    /// Retain failed attempts for diagnosis.
    #[serde(default = "yes")]
    pub retain_failed_attempts: bool,
}
impl Default for StorageConfig {
    fn default() -> Self {
        Self {
            output_root: output_root(),
            asset_library: library_root(),
            min_free_bytes: min_free(),
            retain_failed_attempts: true,
        }
    }
}
fn output_root() -> PathBuf {
    PathBuf::from("scene-runs")
}
fn library_root() -> PathBuf {
    PathBuf::from("scene-library")
}
fn min_free() -> u64 {
    1_000_000_000
}

/// Reuse and external acquisition policy.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AssetPolicy {
    /// Enable reuse from accepted library entries.
    #[serde(default = "yes")]
    pub reuse: bool,
    /// Permit external downloads.
    #[serde(default)]
    pub allow_downloads: bool,
    /// Maximum one-file download size.
    #[serde(default = "download_bytes")]
    pub max_download_bytes: u64,
    /// SPDX-like licenses allowed for automatic acquisition.
    #[serde(default = "licenses")]
    pub allowed_licenses: Vec<String>,
}
impl Default for AssetPolicy {
    fn default() -> Self {
        Self {
            reuse: true,
            allow_downloads: false,
            max_download_bytes: download_bytes(),
            allowed_licenses: licenses(),
        }
    }
}
fn download_bytes() -> u64 {
    50_000_000
}
fn licenses() -> Vec<String> {
    vec![
        "CC0-1.0".into(),
        "CC-BY-4.0".into(),
        "user-provided".into(),
        "generated-local".into(),
    ]
}

impl SceneConfig {
    /// Load, strict-validate, and resolve relative paths against the config file.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| SceneError::Validation(format!("read {}: {e}", path.display())))?;
        let mut value: serde_yaml::Value = serde_yaml::from_str(&raw)
            .map_err(|e| SceneError::Validation(format!("parse {}: {e}", path.display())))?;
        let defaults = serde_yaml::to_value(Self::default())
            .map_err(|e| SceneError::Validation(e.to_string()))?;
        value = merge_yaml(defaults, value);
        let mut config: Self = serde_yaml::from_value(value)
            .map_err(|e| SceneError::Validation(format!("config schema: {e}")))?;
        let base = path.parent().unwrap_or_else(|| Path::new("."));
        config.resolve_paths(base);
        config.validate()?;
        Ok(config)
    }

    /// Apply a task override after defaults/config and validate the result.
    pub fn with_override(&self, value: &serde_yaml::Value, base: &Path) -> Result<Self> {
        let current =
            serde_yaml::to_value(self).map_err(|e| SceneError::Validation(e.to_string()))?;
        let mut out: Self = serde_yaml::from_value(merge_yaml(current, value.clone()))
            .map_err(|e| SceneError::Validation(format!("task override schema: {e}")))?;
        out.resolve_paths(base);
        out.validate()?;
        Ok(out)
    }

    fn resolve_paths(&mut self, base: &Path) {
        if self.storage.output_root.is_relative() {
            self.storage.output_root = base.join(&self.storage.output_root);
        }
        if self.storage.asset_library.is_relative() {
            self.storage.asset_library = base.join(&self.storage.asset_library);
        }
        if let Some(path) = &self.backend.blender_path {
            if path.is_relative() {
                self.backend.blender_path = Some(base.join(path));
            }
        }
        if let Some(path) = &self.embedding.command {
            if path.is_relative() {
                self.embedding.command = Some(base.join(path));
            }
        }
    }

    /// Validate ranges and v1 hard invariants.
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(SceneError::Validation(format!(
                "unsupported config version {}",
                self.version
            )));
        }
        if self.backend.generator != "blender" || self.backend.variation != "procedural_pbr_v1" {
            return Err(SceneError::Validation(
                "v1 requires blender/procedural_pbr_v1".into(),
            ));
        }
        let formats: std::collections::HashSet<_> =
            self.backend.exports.iter().map(String::as_str).collect();
        if !formats.contains("blend") || !formats.contains("glb") {
            return Err(SceneError::Validation(
                "exports must include both blend and glb".into(),
            ));
        }
        if self.models.provider != "soket" {
            return Err(SceneError::Validation(
                "v1 scene roles must use provider soket".into(),
            ));
        }
        if self.execution.max_agent_tasks == 0 || self.execution.max_blender_processes == 0 {
            return Err(SceneError::Validation(
                "concurrency must be at least one".into(),
            ));
        }
        if self.budgets.max_nodes == 0
            || self.budgets.max_depth == 0
            || self.budgets.max_model_calls == 0
        {
            return Err(SceneError::Validation("budgets must be positive".into()));
        }
        if !(0.0..=0.05).contains(&self.quality.containment_tolerance_m)
            || self.quality.connector_tolerance_m <= 0.0
            || self.quality.connector_tolerance_m > self.quality.containment_tolerance_m
        {
            return Err(SceneError::Validation("connector tolerance must be positive and no larger than containment tolerance (max 0.05m)".into()));
        }
        if self.quality.preview_resolution.contains(&0) {
            return Err(SceneError::Validation(
                "preview resolution must be positive".into(),
            ));
        }
        if self.embedding.dimensions < 64 || self.embedding.dimensions > 4096 {
            return Err(SceneError::Validation(
                "embedding dimensions must be 64..=4096".into(),
            ));
        }
        if self.embedding.backend == "command" && self.embedding.command.is_none() {
            return Err(SceneError::Validation(
                "embedding.command is required for command backend".into(),
            ));
        }
        Ok(())
    }
}

fn merge_yaml(base: serde_yaml::Value, overlay: serde_yaml::Value) -> serde_yaml::Value {
    match (base, overlay) {
        (serde_yaml::Value::Mapping(mut a), serde_yaml::Value::Mapping(b)) => {
            for (k, v) in b {
                let previous = a.remove(&k).unwrap_or(serde_yaml::Value::Null);
                a.insert(k, merge_yaml(previous, v));
            }
            serde_yaml::Value::Mapping(a)
        }
        (_, v) => v,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn unknown_fields_are_rejected() {
        let value: serde_yaml::Value = serde_yaml::from_str("version: 1\nunknown: true\n").unwrap();
        let merged = merge_yaml(serde_yaml::to_value(SceneConfig::default()).unwrap(), value);
        assert!(serde_yaml::from_value::<SceneConfig>(merged).is_err());
    }
    #[test]
    fn omitted_values_keep_defaults() {
        let value: serde_yaml::Value =
            serde_yaml::from_str("version: 1\nquality:\n  preview_samples: 7\n").unwrap();
        let cfg: SceneConfig = serde_yaml::from_value(merge_yaml(
            serde_yaml::to_value(SceneConfig::default()).unwrap(),
            value,
        ))
        .unwrap();
        assert_eq!(cfg.quality.preview_samples, 7);
        assert_eq!(cfg.quality.preview_resolution, [1280, 720]);
    }
}
