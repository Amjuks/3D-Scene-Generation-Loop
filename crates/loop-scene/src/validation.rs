//! Numerical, semantic, render-health, and final manifest validation.

use crate::config::SceneConfig;
use crate::roles::VisualReview;
use crate::spec::SceneSpec;
use crate::storage::{hash_file, ArtifactRecord};
use crate::{Result, SceneError};
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

/// One machine-readable finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct Finding {
    /// Stable check code.
    pub code: String,
    /// Severity.
    pub severity: String,
    /// Affected stable IDs.
    pub node_ids: Vec<String>,
    /// Measured evidence.
    pub evidence: serde_json::Value,
}

/// Aggregate validation evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ValidationReport {
    /// Contract version.
    pub version: u32,
    /// Whether all hard checks passed.
    pub passed: bool,
    /// Findings.
    pub findings: Vec<Finding>,
    /// Inspection file used.
    pub inspection: Option<PathBuf>,
    /// Cross-format report.
    pub parity: Option<PathBuf>,
    /// Actual preview files.
    pub previews: Vec<PathBuf>,
    /// Structured vision result if performed.
    pub visual_review: Option<VisualReview>,
}

/// Validate scene contract plus evaluated Blender inspection.
pub fn numerical(
    spec: &SceneSpec,
    config: &SceneConfig,
    inspection: &Path,
) -> Result<ValidationReport> {
    spec.validate(
        config.quality.containment_tolerance_m,
        config.quality.relative_tolerance,
    )?;
    let raw = std::fs::read(inspection)
        .map_err(|e| SceneError::Validation(format!("read inspection: {e}")))?;
    let i: serde_json::Value = serde_json::from_slice(&raw)
        .map_err(|e| SceneError::Validation(format!("inspection schema: {e}")))?;
    let mut findings = Vec::new();
    let mesh = i["mesh_count"].as_u64().unwrap_or(0);
    if mesh == 0 {
        findings.push(Finding {
            code: "empty_geometry".into(),
            severity: "error".into(),
            node_ids: vec![spec.root_id.clone()],
            evidence: i.clone(),
        });
    }
    let tris = i["triangles"].as_u64().unwrap_or(u64::MAX);
    if tris > config.quality.max_triangles {
        findings.push(Finding {
            code: "triangle_budget".into(),
            severity: "error".into(),
            node_ids: vec![spec.root_id.clone()],
            evidence: serde_json::json!({"actual":tris,"limit":config.quality.max_triangles}),
        });
    }
    let ids: i64 = i["semantic_node_ids"]
        .as_array()
        .map(|x| x.len() as i64)
        .unwrap_or(0);
    let expected = spec
        .nodes
        .iter()
        .filter(|n| n.generation.strategy != "assembly" || n.child_ids.is_empty())
        .count() as i64;
    if ids < expected {
        findings.push(Finding {
            code: "missing_semantic_geometry".into(),
            severity: "error".into(),
            node_ids: vec![spec.root_id.clone()],
            evidence: serde_json::json!({"expected_at_least":expected,"measured_ids":ids}),
        });
    }
    Ok(ValidationReport {
        version: 1,
        passed: findings
            .iter()
            .all(|f| f.severity != "error" && f.severity != "blocker"),
        findings,
        inspection: Some(inspection.to_path_buf()),
        parity: None,
        previews: vec![],
        visual_review: None,
    })
}

/// Reject black/tiny/missing render files before sending them to a model. PNG
/// decode is left to Blender/model, while file signatures and entropy are
/// checked locally without adding an image dependency.
pub fn render_health(paths: &[PathBuf]) -> Result<()> {
    if paths.len() < 3 {
        return Err(SceneError::Validation(
            "fewer than three review views".into(),
        ));
    }
    for p in paths {
        let b = std::fs::read(p)
            .map_err(|e| SceneError::Validation(format!("missing preview {}: {e}", p.display())))?;
        if b.len() < 1024 || !b.starts_with(b"\x89PNG\r\n\x1a\n") {
            return Err(SceneError::Validation(format!(
                "invalid/empty PNG {}",
                p.display()
            )));
        }
        let sample = &b[b.len().min(128)..];
        let distinct = sample
            .iter()
            .copied()
            .collect::<std::collections::HashSet<_>>()
            .len();
        if distinct < 8 {
            return Err(SceneError::Validation(format!(
                "low-entropy/black preview {}",
                p.display()
            )));
        }
    }
    Ok(())
}

/// Explicit final artifact manifest.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct FinalManifest {
    /// Manifest version.
    pub version: u32,
    /// Run ID.
    pub run_id: String,
    /// SHA-256 prompt snapshot.
    pub prompt_hash: String,
    /// SHA-256 resolved config.
    pub config_hash: String,
    /// Scene root/revision identity.
    pub scene_revision: String,
    /// Backend versions.
    pub backend: serde_json::Value,
    /// Required final deliverables.
    pub blend: ManifestFile,
    /// Portable deliverable.
    pub glb: ManifestFile,
    /// Evidence files.
    pub validation: Vec<ManifestFile>,
    /// Preview evidence.
    pub previews: Vec<ManifestFile>,
    /// Accepted asset records/attribution.
    pub assets: Vec<ArtifactRecord>,
    /// Outstanding findings (empty for acceptance).
    pub outstanding_findings: Vec<Finding>,
}
/// Checksummed manifest file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ManifestFile {
    /// Path.
    pub path: PathBuf,
    /// SHA-256.
    pub sha256: String,
    /// Size.
    pub bytes: u64,
}
impl ManifestFile {
    /// Measure a final file.
    pub fn from_path(path: PathBuf) -> Result<Self> {
        let bytes = std::fs::metadata(&path)
            .map_err(|e| SceneError::Validation(e.to_string()))?
            .len();
        Ok(Self {
            sha256: hash_file(&path)?,
            path,
            bytes,
        })
    }
}
