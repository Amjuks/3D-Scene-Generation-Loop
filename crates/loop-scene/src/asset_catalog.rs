//! Curated source models are separate from generated, run-local component caches.
//! A source is never selected by approximate text similarity alone: plans name an
//! exact catalogue ID and its bytes, license and declared quality are checked.

use std::collections::{BTreeMap, BTreeSet};
use std::path::PathBuf;

use serde::{Deserialize, Serialize};
use serde_json::{json, Value};

use crate::config::SceneConfig;
use crate::spec::SceneSpec;
use crate::storage::hash_file;
use crate::{Result, SceneError};

/// Versioned inventory of artist-created or scanned source models.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceCatalog {
    /// Catalogue schema version (currently 1).
    pub version: u32,
    /// Models which may be referenced by exact ID.
    pub assets: Vec<SourceAsset>,
}

/// One self-contained, checksummed GLB or packed Blender source.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SourceAsset {
    /// Stable model ID, distinct from a scene node ID.
    pub id: String,
    /// Human-readable model name.
    pub name: String,
    /// Semantic type, such as oak tree or carved altar.
    pub category: String,
    /// Searchable physical and stylistic attributes.
    #[serde(default)]
    pub tags: Vec<String>,
    /// Self-contained file, relative to the catalogue or absolute.
    pub path: PathBuf,
    /// SHA-256 digest of the exact source file.
    pub sha256: String,
    /// License identifier allowed by AssetPolicy.
    pub license: String,
    /// Source provenance URL; local user assets can use a descriptive local URI.
    pub source_url: String,
    /// Source artist or publisher.
    pub creator: String,
    /// `production` or `draft`, declared by the asset curator, not inferred from polygon count.
    pub quality: String,
    /// Measured unscaled source dimensions in right-handed Z-up metres.
    #[serde(default)]
    pub dimensions_m: Option<[f64; 3]>,
    /// Optional exact object/subtree name for files containing several variants.
    #[serde(default)]
    pub object_name: Option<String>,
    /// Explicit source role; legacy catalogues may omit this field.
    #[serde(default)]
    pub kind: Option<String>,
    /// Curator-specified surface family, such as plaster or forest soil.
    #[serde(default)]
    pub material_family: Option<String>,
    /// Exact allowed material_usage values. Empty means not yet restricted.
    #[serde(default)]
    pub surface_uses: Vec<String>,
}

fn load(config: &SceneConfig) -> Result<Vec<SourceAsset>> {
    let Some(path) = &config.assets.catalog else {
        return Ok(vec![]);
    };
    let raw = std::fs::read(path).map_err(|e| {
        SceneError::WaitingExternal(format!(
            "asset catalogue {} unavailable: {e}; prepare one with scripts/scene_assets.py",
            path.display()
        ))
    })?;
    let catalog: SourceCatalog = serde_json::from_slice(&raw)
        .map_err(|e| SceneError::Validation(format!("asset catalogue schema: {e}")))?;
    if catalog.version != 1 {
        return Err(SceneError::Validation(
            "asset catalogue version must be 1".into(),
        ));
    }
    let mut ids = BTreeSet::new();
    let mut assets = catalog.assets;
    for asset in &mut assets {
        if asset.id.trim().is_empty()
            || !ids.insert(asset.id.clone())
            || asset.name.trim().is_empty()
            || asset.category.trim().is_empty()
            || asset.creator.trim().is_empty()
            || asset.source_url.trim().is_empty()
            || !matches!(asset.quality.as_str(), "production" | "draft")
            || asset.sha256.len() != 64
            || !asset.sha256.bytes().all(|b| b.is_ascii_hexdigit())
            || asset
                .dimensions_m
                .is_some_and(|s| s.iter().any(|v| !v.is_finite() || *v <= 0.0))
            || asset
                .object_name
                .as_ref()
                .is_some_and(|name| name.trim().is_empty())
            || asset
                .kind
                .as_deref()
                .is_some_and(|kind| !matches!(kind, "model" | "material"))
            || asset
                .material_family
                .as_ref()
                .is_some_and(|s| s.trim().is_empty())
            || asset.surface_uses.iter().any(|s| s.trim().is_empty())
        {
            return Err(SceneError::Validation(format!(
                "invalid/duplicate source asset {}",
                asset.id
            )));
        }
        let ext = asset
            .path
            .extension()
            .and_then(|x| x.to_str())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !matches!(ext.as_str(), "glb" | "blend") {
            return Err(SceneError::Validation(format!(
                "asset {} needs a self-contained .glb or packed .blend",
                asset.id
            )));
        }
        if asset.path.is_relative() {
            asset.path = path
                .parent()
                .unwrap_or(std::path::Path::new("."))
                .join(&asset.path);
        }
    }
    Ok(assets)
}

/// Ground planner choices in eligible source IDs and physical dimensions, never file paths.
pub fn catalog_context(config: &SceneConfig) -> Result<Value> {
    let assets = load(config)?;
    Ok(json!({
        "catalog_version": 1,
        "assets": assets.iter().filter(|a| config.assets.allowed_licenses.contains(&a.license)
            && (config.quality.fidelity != "realistic" || a.quality == "production"))
            .map(|a| json!({"id":a.id,"name":a.name,"category":a.category,"tags":a.tags,
                "quality":a.quality,"dimensions_m":a.dimensions_m,"sha256":a.sha256,"object_name":a.object_name,
                "kind":a.kind.as_deref().unwrap_or(if a.category.starts_with("surface material") {"material"} else {"model"}),
                "material_family":a.material_family,"surface_uses":a.surface_uses})).collect::<Vec<_>>(),
        "policy": "Use recipe asset with parameters.asset_id matching an exact listed ID. Preserve source aspect ratio and material textures. For authored surfaces, parameters.material_asset_id may reference a suitable source material model; optional material_name selects a material, and texture_scale_m sets metric UV tiling. Never use unrelated source textures simply because they are available. Do not rename an unrelated asset to satisfy the requested subject. For unavailable required models report unresolved_assets with precise species/type, style and dimensions. Authored mesh/profile geometry is appropriate for custom architecture, not a pretext to substitute crude organic proxies.",
    }))
}

/// Resolve all imported models atomically and verify each unique source once.
/// Missing, modified, unlicensed or draft models cannot silently become boxes.
pub fn resolve_scene_assets(spec: &mut SceneSpec, config: &SceneConfig) -> Result<()> {
    validate_planned_sources(config, spec.nodes.iter().map(|n| &n.generation.parameters))?;
    let sources: BTreeMap<_, _> = load(config)?
        .into_iter()
        .map(|a| (a.id.clone(), a))
        .collect();
    let mut verified = BTreeSet::new();
    let mut resolved = Vec::new();
    let mut missing = BTreeSet::new();
    for (index, node) in spec.nodes.iter().enumerate() {
        for prefix in ["asset", "material_asset"] {
            let key = format!("{prefix}_id");
            let id = match node.generation.parameters.get(&key).and_then(Value::as_str) {
                Some(id) => id,
                None if prefix == "asset" && node.generation.recipe == "asset" => {
                    return Err(SceneError::Validation(format!(
                        "asset node {} lacks exact asset_id",
                        node.node_id
                    )))
                }
                None => continue,
            };
            let Some(asset) = sources.get(id) else {
                missing.insert(id.to_owned());
                continue;
            };
            validate_source_usage(asset, prefix, &node.generation.parameters)?;
            if !config.assets.allowed_licenses.contains(&asset.license) {
                return Err(SceneError::Validation(format!(
                    "asset {id} license {} is not allowed",
                    asset.license
                )));
            }
            if config.quality.fidelity == "realistic" && asset.quality != "production" {
                return Err(SceneError::Validation(format!(
                    "asset {id} is a draft, not eligible for realistic fidelity"
                )));
            }
            if verified.insert(id.to_owned()) {
                let digest = hash_file(&asset.path)
                    .map_err(|e| SceneError::WaitingExternal(format!("source asset {id}: {e}")))?;
                if !digest.eq_ignore_ascii_case(&asset.sha256) {
                    return Err(SceneError::Validation(format!("source asset {id} checksum changed; curate a new version instead of reusing stale geometry")));
                }
            }
            resolved.push((index, asset, prefix));
        }
    }
    if !missing.is_empty() {
        return Err(SceneError::WaitingExternal(format!("missing source models: {}; add exact suitable models to assets.catalog using scripts/scene_assets.py; no proxy substitution was made", missing.into_iter().collect::<Vec<_>>().join(", "))));
    }
    for (index, asset, prefix) in resolved {
        let node = &mut spec.nodes[index];
        let path = std::fs::canonicalize(&asset.path)
            .map_err(|e| SceneError::WaitingExternal(format!("asset {} path: {e}", asset.id)))?;
        node.generation
            .parameters
            .insert(format!("{prefix}_path"), json!(path));
        node.generation.parameters.insert(
            format!("{prefix}_sha256"),
            json!(asset.sha256.to_lowercase()),
        );
        node.generation
            .parameters
            .insert(format!("{prefix}_quality"), json!(asset.quality));
        node.generation
            .parameters
            .insert(format!("{prefix}_license"), json!(asset.license));
        node.generation
            .parameters
            .insert(format!("{prefix}_source_url"), json!(asset.source_url));
        node.generation
            .parameters
            .remove(&format!("{prefix}_object"));
        if let Some(name) = &asset.object_name {
            node.generation
                .parameters
                .insert(format!("{prefix}_object"), json!(name));
        }
        node.provenance.source_url = Some(asset.source_url.clone());
        node.provenance.creator = Some(asset.creator.clone());
        node.provenance.license = Some(asset.license.clone());
        if !node.provenance.dependency_hashes.contains(&asset.sha256) {
            node.provenance.dependency_hashes.push(asset.sha256.clone());
        }
    }
    Ok(())
}

fn validate_source_usage(
    asset: &SourceAsset,
    prefix: &str,
    parameters: &BTreeMap<String, Value>,
) -> Result<()> {
    if (prefix == "asset" && asset.kind.as_deref() == Some("material"))
        || (prefix == "material_asset" && asset.kind.as_deref() == Some("model"))
    {
        return Err(SceneError::Validation(format!("source {} kind {:?} is incompatible with {prefix}; curate a separate material entry when extracting a model finish", asset.id, asset.kind)));
    }
    if prefix == "material_asset" && !asset.surface_uses.is_empty() {
        let usage = parameters
            .get("material_usage")
            .and_then(Value::as_str)
            .unwrap_or("");
        if !asset.surface_uses.iter().any(|value| value == usage) {
            return Err(SceneError::Validation(format!("material {} does not permit usage {usage:?}; exact curator-approved surface_uses: {:?}. Select an appropriate finish instead of assigning an unrelated available texture.",asset.id,asset.surface_uses)));
        }
    }
    Ok(())
}

/// Validate semantic source restrictions before spending time on mesh generation.
pub(crate) fn validate_planned_sources<'a>(
    config: &SceneConfig,
    parameters: impl Iterator<Item = &'a BTreeMap<String, Value>>,
) -> Result<()> {
    let sources: BTreeMap<_, _> = load(config)?
        .into_iter()
        .map(|a| (a.id.clone(), a))
        .collect();
    for parameters in parameters {
        if config.quality.preserve_source_detail && parameters.contains_key("asset_lod") {
            return Err(SceneError::Validation("preserve_source_detail forbids simplified source derivatives; omit asset_lod and retain full source geometry".into()));
        }
        for prefix in ["asset", "material_asset"] {
            if let Some(id) = parameters
                .get(&format!("{prefix}_id"))
                .and_then(Value::as_str)
            {
                if let Some(asset) = sources.get(id) {
                    validate_source_usage(asset, prefix, parameters)?;
                }
            }
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    fn fixture(dir: &std::path::Path) -> (SceneConfig, SceneSpec) {
        let mut config = SceneConfig::default();
        let file = dir.join("source.glb");
        std::fs::write(&file, b"source-test-bytes").unwrap();
        let catalog = SourceCatalog {
            version: 1,
            assets: vec![SourceAsset {
                id: "tree".into(),
                name: "Scanned tree".into(),
                category: "tree".into(),
                tags: vec![],
                path: "source.glb".into(),
                sha256: hash_file(&file).unwrap(),
                license: "CC0-1.0".into(),
                source_url: "https://example.com/tree".into(),
                creator: "artist".into(),
                quality: "production".into(),
                dimensions_m: Some([2.0, 3.0, 5.0]),
                object_name: None,
                kind: None,
                material_family: None,
                surface_uses: vec![],
            }],
        };
        let catalog_path = dir.join("catalog.json");
        std::fs::write(&catalog_path, serde_json::to_vec(&catalog).unwrap()).unwrap();
        config.assets.catalog = Some(catalog_path);
        let mut spec: SceneSpec =
            serde_json::from_str(include_str!("../examples/complete-scene.json")).unwrap();
        let n = spec.nodes.last_mut().unwrap();
        n.generation.recipe = "asset".into();
        n.generation
            .parameters
            .insert("asset_id".into(), json!("tree"));
        (config, spec)
    }
    #[test]
    fn resolves_catalog_relative_paths_and_pins_source_bytes() {
        let d = tempfile::tempdir().unwrap();
        let (config, mut spec) = fixture(d.path());
        resolve_scene_assets(&mut spec, &config).unwrap();
        let n = spec.nodes.last().unwrap();
        assert_eq!(n.provenance.license.as_deref(), Some("CC0-1.0"));
        assert!(
            PathBuf::from(n.generation.parameters["asset_path"].as_str().unwrap()).is_absolute()
        );
        std::fs::write(d.path().join("source.glb"), b"mutated").unwrap();
        assert!(resolve_scene_assets(&mut spec, &config)
            .unwrap_err()
            .to_string()
            .contains("checksum"));
    }
    #[test]
    fn authored_surfaces_resolve_verified_material_sources() {
        let d = tempfile::tempdir().unwrap();
        let (config, mut spec) = fixture(d.path());
        let node = spec.nodes.last_mut().unwrap();
        node.generation.recipe = "heightfield".into();
        node.generation.parameters.clear();
        node.generation
            .parameters
            .insert("material_asset_id".into(), json!("tree"));
        resolve_scene_assets(&mut spec, &config).unwrap();
        let p = &spec.nodes.last().unwrap().generation.parameters;
        assert!(p.contains_key("material_asset_path"));
        assert!(p.contains_key("material_asset_sha256"));
        assert!(!p.contains_key("asset_path"));
        std::fs::write(d.path().join("source.glb"), b"changed").unwrap();
        assert!(resolve_scene_assets(&mut spec, &config).is_err());
    }
    #[test]
    fn unavailable_or_disallowed_models_do_not_fall_back() {
        let d = tempfile::tempdir().unwrap();
        let (mut config, mut spec) = fixture(d.path());
        config.assets.allowed_licenses.clear();
        assert!(resolve_scene_assets(&mut spec, &config).is_err());
        config.assets.catalog = None;
        assert!(matches!(
            resolve_scene_assets(&mut spec, &config),
            Err(SceneError::WaitingExternal(_))
        ));
    }
    #[test]
    fn surface_restrictions_and_source_roles_prevent_unrelated_material_reuse() {
        let d = tempfile::tempdir().unwrap();
        let (config, mut spec) = fixture(d.path());
        let path = config.assets.catalog.as_ref().unwrap();
        let mut catalog: SourceCatalog =
            serde_json::from_slice(&std::fs::read(path).unwrap()).unwrap();
        catalog.assets[0].kind = Some("material".into());
        catalog.assets[0].surface_uses = vec!["landscape_ground".into()];
        std::fs::write(path, serde_json::to_vec(&catalog).unwrap()).unwrap();
        assert!(resolve_scene_assets(&mut spec, &config).is_err());
        let node = spec.nodes.last_mut().unwrap();
        node.generation.recipe = "heightfield".into();
        node.generation.parameters.clear();
        node.generation
            .parameters
            .insert("material_asset_id".into(), json!("tree"));
        node.generation
            .parameters
            .insert("material_usage".into(), json!("exhibition_wall"));
        assert!(resolve_scene_assets(&mut spec, &config)
            .unwrap_err()
            .to_string()
            .contains("does not permit"));
        spec.nodes
            .last_mut()
            .unwrap()
            .generation
            .parameters
            .insert("material_usage".into(), json!("landscape_ground"));
        resolve_scene_assets(&mut spec, &config).unwrap();
    }
    #[test]
    fn draft_models_are_filtered_and_rejected_in_realistic_mode() {
        let d = tempfile::tempdir().unwrap();
        let (mut config, mut spec) = fixture(d.path());
        let p = config.assets.catalog.as_ref().unwrap();
        let raw = std::fs::read_to_string(p)
            .unwrap()
            .replace("production", "draft");
        std::fs::write(p, raw).unwrap();
        config.quality.fidelity = "realistic".into();
        assert!(catalog_context(&config).unwrap()["assets"]
            .as_array()
            .unwrap()
            .is_empty());
        assert!(resolve_scene_assets(&mut spec, &config).is_err());
    }
}
