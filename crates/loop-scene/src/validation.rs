//! Numerical, semantic, render-health, and final manifest validation.

use crate::config::SceneConfig;
use crate::roles::VisualReview;
use crate::spec::SceneSpec;
use crate::storage::{hash_file, ArtifactRecord};
use crate::{Result, SceneError};
use serde::{Deserialize, Serialize};
use std::{
    collections::BTreeSet,
    path::{Path, PathBuf},
};

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

/// Enforce the declared fidelity tier independently of a model's assessment.
/// Authored geometry is eligible for review, not automatically photorealistic.
pub fn fidelity_findings(spec: &SceneSpec, config: &SceneConfig) -> Vec<Finding> {
    if config.quality.fidelity != "realistic" {
        return Vec::new();
    }
    let mut findings = Vec::new();
    for node in &spec.nodes {
        let generation = &node.generation;
        if generation.strategy == "assembly" && !node.child_ids.is_empty() {
            continue;
        }
        let recipe = generation.recipe.as_str();
        let semantic_proxy = node
            .kind
            .to_lowercase()
            .split(|c: char| !c.is_alphanumeric())
            .any(|word| {
                matches!(
                    word,
                    "tree"
                        | "plant"
                        | "vegetation"
                        | "foliage"
                        | "bush"
                        | "grass"
                        | "chair"
                        | "table"
                        | "sofa"
                        | "bed"
                        | "cabinet"
                        | "bookshelf"
                        | "animal"
                        | "person"
                )
            });
        let authored = matches!(
            recipe,
            "asset" | "mesh" | "extrusion" | "lathe" | "sweep" | "heightfield"
        );
        let supported = matches!(
            recipe,
            "asset"
                | "mesh"
                | "extrusion"
                | "lathe"
                | "sweep"
                | "heightfield"
                | "slab"
                | "wall"
                | "floor"
                | "ceiling"
                | "door"
                | "window"
                | "stairs"
                | "light"
        ) && (authored || !semantic_proxy);
        if !supported {
            findings.push(Finding {
                code: "realistic_proxy_geometry".into(), severity: "error".into(),
                node_ids: vec![node.node_id.clone()],
                evidence: serde_json::json!({"recipe":recipe,"kind":node.kind,
                    "requirement":"Use a verified production asset or authored geometry that reproduces this object's morphology; legacy furnishing and vegetation recipes are concept proxies."}),
            });
        } else if recipe == "asset"
            && (generation
                .parameters
                .get("asset_quality")
                .and_then(|v| v.as_str())
                != Some("production")
                || !generation
                    .parameters
                    .get("asset_sha256")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| s.len() == 64 && s.bytes().all(|b| b.is_ascii_hexdigit()))
                || !generation
                    .parameters
                    .get("asset_license")
                    .and_then(|v| v.as_str())
                    .is_some_and(|s| !s.trim().is_empty()))
        {
            findings.push(Finding {
                code: "unverified_asset_quality".into(), severity: "error".into(),
                node_ids: vec![node.node_id.clone()],
                evidence: serde_json::json!({"asset_id":generation.parameters.get("asset_id"),
                    "quality":generation.parameters.get("asset_quality"),
                    "requirement":"Realistic scenes require production quality asset catalogue evidence."}),
            });
        }
    }
    findings
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn measured_texture_budget_rejects_overlarge_asset_payloads() {
        let dir = tempfile::tempdir().unwrap();
        let path = dir.path().join("inspection.json");
        std::fs::write(&path,serde_json::json!({"mesh_count":1,"triangles":100,"semantic_node_ids":["scene"],"texture_bytes":257_000_000}).to_string()).unwrap();
        let report = numerical(&spec(), &SceneConfig::default(), &path).unwrap();
        assert!(!report.passed);
        assert_eq!(report.findings[0].code, "texture_budget");
    }
    use crate::roles::VisualFinding;
    use serde_json::json;

    fn spec() -> SceneSpec {
        serde_json::from_str(include_str!("../examples/complete-scene.json")).unwrap()
    }

    #[test]
    fn semantic_geometry_requires_the_actual_ids_not_only_a_count() {
        let directory = tempfile::tempdir().unwrap();
        let path = directory.path().join("inspection.json");
        std::fs::write(&path, json!({"mesh_count":3,"triangles":100,"semantic_node_ids":["wrong", "wrong", "another"]}).to_string()).unwrap();
        let report = numerical(&spec(), &SceneConfig::default(), &path).unwrap();
        assert!(!report.passed);
        assert_eq!(report.findings[0].node_ids, ["scene"]);
        std::fs::write(
            &path,
            json!({"mesh_count":3,"triangles":100,"semantic_node_ids":["scene"]}).to_string(),
        )
        .unwrap();
        assert!(
            numerical(&spec(), &SceneConfig::default(), &path)
                .unwrap()
                .passed
        );
    }

    #[test]
    fn visual_review_cannot_accept_missing_views_or_unknown_severities() {
        let mut review = VisualReview {
            findings: vec![],
            coverage_sufficient: false,
            summary: "Subject hidden".into(),
        };
        assert_eq!(
            visual_findings(&review)[0].code,
            "insufficient_visual_coverage"
        );
        review.coverage_sufficient = true;
        review.findings.push(VisualFinding {
            severity: "warning".into(),
            affected_ids: vec!["scene".into()],
            evidence: "The rear structure is outside the supplied cameras".into(),
            confidence: 0.8,
            suggested_correction: "Supply the rear camera".into(),
            needs_another_view: true,
        });
        assert!(visual_findings(&review)
            .iter()
            .any(|f| f.code == "additional_visual_evidence_required" && f.severity == "error"));
        review.findings[0].needs_another_view = false;
        assert_eq!(
            visual_findings_for_policy(&review, false)[0].severity,
            "warning"
        );
        let strict = visual_findings_for_policy(&review, true);
        assert_eq!(strict[0].code, "unresolved_delivery_warning");
        assert_eq!(strict[0].severity, "error");
        assert_eq!(strict[0].evidence["severity"], "warning");
        review.findings[0].severity = "failed".into();
        assert_eq!(visual_findings(&review)[0].code, "invalid_visual_evidence");
        review.findings[0].severity = "warning".into();
        review.findings[0].confidence = 1.5;
        assert_eq!(visual_findings(&review)[0].severity, "error");
        review.findings.clear();
        assert!(visual_findings(&review).is_empty());
    }

    #[test]
    fn realistic_tier_requires_authored_or_verified_geometry() {
        let mut config = SceneConfig::default();
        let mut scene = spec();
        assert!(fidelity_findings(&scene, &config).is_empty());
        config.quality.fidelity = "realistic".into();
        assert_eq!(
            fidelity_findings(&scene, &config)[0].code,
            "realistic_proxy_geometry"
        );
        scene.nodes[0].generation.recipe = "mesh".into();
        assert!(fidelity_findings(&scene, &config).is_empty());
        scene.nodes[0].generation.recipe = "asset".into();
        assert_eq!(
            fidelity_findings(&scene, &config)[0].code,
            "unverified_asset_quality"
        );
        scene.nodes[0]
            .generation
            .parameters
            .insert("asset_quality".into(), json!("production"));
        assert!(!fidelity_findings(&scene, &config).is_empty());
        scene.nodes[0]
            .generation
            .parameters
            .insert("asset_sha256".into(), json!("a".repeat(64)));
        scene.nodes[0]
            .generation
            .parameters
            .insert("asset_license".into(), json!("CC0-1.0"));
        assert!(fidelity_findings(&scene, &config).is_empty());
        scene.nodes[0].generation.recipe = "slab".into();
        scene.nodes[0].kind = "oak_tree".into();
        assert!(!fidelity_findings(&scene, &config).is_empty());
        scene.nodes[0].kind = "foundation".into();
        assert!(fidelity_findings(&scene, &config).is_empty());
    }
}

/// Convert all visual evidence to acceptance findings, including missing views.
/// A reviewer cannot approve by returning no errors while admitting it cannot see
/// the requested subject, or by inventing a severity the controller ignores.
pub fn visual_findings(review: &VisualReview) -> Vec<Finding> {
    let mut findings = Vec::new();
    if !review.coverage_sufficient {
        findings.push(Finding {
            code: "insufficient_visual_coverage".into(),
            severity: "error".into(),
            node_ids: vec![],
            evidence: serde_json::json!({"summary":review.summary}),
        });
    }
    for finding in &review.findings {
        let valid = matches!(
            finding.severity.as_str(),
            "info" | "warning" | "error" | "blocker"
        ) && finding.confidence.is_finite()
            && (0.0..=1.0).contains(&finding.confidence)
            && !finding.evidence.trim().is_empty();
        findings.push(Finding {
            code: if valid {
                "visual_review"
            } else {
                "invalid_visual_evidence"
            }
            .into(),
            severity: if valid {
                finding.severity.clone()
            } else {
                "error".into()
            },
            node_ids: finding.affected_ids.clone(),
            evidence: serde_json::json!(finding),
        });
        if finding.needs_another_view {
            findings.push(Finding {
                code: "additional_visual_evidence_required".into(),
                severity: "error".into(),
                node_ids: finding.affected_ids.clone(),
                evidence: serde_json::json!({"evidence":finding.evidence}),
            });
        }
    }
    findings
}

/// Apply the optional strict delivery policy without altering original evidence.
pub fn visual_findings_for_policy(
    review: &VisualReview,
    require_no_warnings: bool,
) -> Vec<Finding> {
    let mut findings = visual_findings(review);
    if require_no_warnings {
        for finding in &mut findings {
            if finding.severity == "warning" {
                finding.severity = "error".into();
                finding.code = "unresolved_delivery_warning".into();
            }
        }
    }
    findings
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
    let mut findings = fidelity_findings(spec, config);
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
    if i["texture_bytes"].as_u64().unwrap_or(0) > config.quality.max_texture_bytes {
        findings.push(Finding {
            code: "texture_budget".into(), severity: "error".into(),
            node_ids: vec![spec.root_id.clone()],
            evidence: serde_json::json!({"actual":i["texture_bytes"],"limit":config.quality.max_texture_bytes}),
        });
    }
    if tris > config.quality.max_triangles {
        findings.push(Finding {
            code: "triangle_budget".into(),
            severity: "error".into(),
            node_ids: vec![spec.root_id.clone()],
            evidence: serde_json::json!({"actual":tris,"limit":config.quality.max_triangles}),
        });
    }
    let ids: BTreeSet<&str> = i["semantic_node_ids"]
        .as_array()
        .map(|values| values.iter().filter_map(|v| v.as_str()).collect())
        .unwrap_or_default();
    let expected: BTreeSet<&str> = spec
        .nodes
        .iter()
        .filter(|n| n.generation.strategy != "assembly" || n.child_ids.is_empty())
        .map(|n| n.node_id.as_str())
        .collect();
    let missing: Vec<String> = expected
        .difference(&ids)
        .map(|id| (*id).to_string())
        .collect();
    if !missing.is_empty() {
        findings.push(Finding {
            code: "missing_semantic_geometry".into(),
            severity: "error".into(),
            node_ids: missing,
            evidence: serde_json::json!({"expected_ids":expected,"measured_ids":ids}),
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
