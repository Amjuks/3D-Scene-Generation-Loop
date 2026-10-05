//! Canonical versioned scene contracts and coordinate math.

use crate::{Result, SceneError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};

/// Canonical local transform. Serialization always uses meters and quaternion
/// `[x,y,z,w]`; matrices use column vectors (`world = parent * local`).
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Transform {
    /// Translation in meters.
    pub translation: [f64; 3],
    /// Normalized quaternion in x,y,z,w order.
    pub rotation_xyzw: [f64; 4],
    /// Explicit scale; accepted generated assets require identity.
    pub scale: [f64; 3],
}
impl Default for Transform {
    fn default() -> Self {
        Self {
            translation: [0.0; 3],
            rotation_xyzw: [0.0, 0.0, 0.0, 1.0],
            scale: [1.0; 3],
        }
    }
}

/// Axis-aligned bounds in the coordinate space named by its containing field.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq)]
#[serde(deny_unknown_fields)]
pub struct Bounds {
    /// Minimum corner.
    pub min: [f64; 3],
    /// Maximum corner.
    pub max: [f64; 3],
}
impl Bounds {
    /// Extents along x/y/z.
    pub fn size(&self) -> [f64; 3] {
        [
            self.max[0] - self.min[0],
            self.max[1] - self.min[1],
            self.max[2] - self.min[2],
        ]
    }
    /// Eight corners.
    pub fn corners(&self) -> [[f64; 3]; 8] {
        let [a, b, c] = self.min;
        let [x, y, z] = self.max;
        [
            [a, b, c],
            [x, b, c],
            [a, y, c],
            [x, y, c],
            [a, b, z],
            [x, b, z],
            [a, y, z],
            [x, y, z],
        ]
    }
    /// Whether a point is contained under absolute-plus-relative tolerance.
    pub fn contains(&self, p: [f64; 3], abs: f64, rel: f64) -> bool {
        let s = self.size();
        (0..3).all(|i| {
            let t = abs + rel * s[i].abs().max(p[i].abs());
            p[i] >= self.min[i] - t && p[i] <= self.max[i] + t
        })
    }
}

/// Child allocation region in node-local coordinates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct AllocationRegion {
    /// Stable region ID.
    pub region_id: String,
    /// Allowed local volume.
    pub allowed: Bounds,
    /// Volumes forbidden to children.
    #[serde(default)]
    pub excluded: Vec<Bounds>,
    /// Required clearance in meters.
    #[serde(default)]
    pub clearance_m: f64,
    /// Space reserved for circulation/joinery.
    #[serde(default)]
    pub reservations: Vec<Bounds>,
    /// Maximum declared protrusion beyond allowed volume.
    #[serde(default)]
    pub permitted_protrusion_m: f64,
    /// Semantic classes allowed in this region.
    #[serde(default)]
    pub content_classes: Vec<String>,
}

/// A named attachment interface frame in node-local coordinates.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Interface {
    /// Name unique within the node.
    pub name: String,
    /// Local frame.
    pub frame: Transform,
    /// Semantic connector type.
    pub interface_type: String,
    /// Width/height/depth or profile measures in meters.
    pub size_m: Vec<f64>,
    /// Mating rule, e.g. `coincident_opposed_normal`.
    pub mating_rule: String,
    /// Translation tolerance in meters.
    pub tolerance_m: f64,
    /// Angular tolerance in degrees.
    pub tolerance_degrees: f64,
    /// Allowed degrees of freedom (`slide_x`, `rotate_z`, etc.).
    #[serde(default)]
    pub allowed_dof: Vec<String>,
    /// Stable partner `node_id#interface`, when connected.
    #[serde(default)]
    pub partner: Option<String>,
}

/// Cross-node semantic or spatial relation.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Relationship {
    /// Relation type: contains, supported_by, adjacent_to, aligned_with,
    /// connects_to, faces, or accessibility_path.
    pub relation: String,
    /// Stable target node/interface ID.
    pub target: String,
    /// Optional measured/semantic parameters.
    #[serde(default)]
    pub parameters: BTreeMap<String, serde_json::Value>,
}

/// Material reference with physical scale.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct MaterialRef {
    /// Stable material ID.
    pub material_id: String,
    /// Human-readable PBR material description.
    pub description: String,
    /// Base color in linear RGBA.
    pub base_color: [f64; 4],
    /// Metallic factor.
    pub metallic: f64,
    /// Roughness factor.
    pub roughness: f64,
    /// Real-world repeat size in meters.
    pub texture_scale_m: f64,
    /// Optional content hash/source reference.
    #[serde(default)]
    pub texture_ref: Option<String>,
}

/// Deterministic generation or retrieval intent.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct GenerationStrategy {
    /// `recipe`, `retrieve_or_recipe`, or `assembly`.
    pub strategy: String,
    /// Versioned recipe/asset reference.
    pub recipe: String,
    /// Validated recipe parameters.
    #[serde(default)]
    pub parameters: BTreeMap<String, serde_json::Value>,
    /// Procedural seed.
    pub seed: u64,
    /// Detail policy.
    pub detail_policy: String,
    /// Required backend capabilities.
    #[serde(default)]
    pub required_capabilities: Vec<String>,
}

/// Numeric and visual acceptance contract.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Acceptance {
    /// Numeric rules identified by stable names.
    #[serde(default)]
    pub numeric_rules: BTreeMap<String, f64>,
    /// Required visible/semantic features.
    #[serde(default)]
    pub visual_requirements: Vec<String>,
    /// Requested camera/view types.
    #[serde(default)]
    pub review_views: Vec<String>,
    /// Evidence artifact hashes/paths once validated.
    #[serde(default)]
    pub evidence_refs: Vec<String>,
}

/// Source and validation provenance.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Provenance {
    /// Owning task ID.
    pub owning_task: String,
    /// Parent contract revision used by the proposal.
    #[serde(default)]
    pub parent_revision: Option<u32>,
    /// Effective dependency hashes.
    #[serde(default)]
    pub dependency_hashes: Vec<String>,
    /// Provider/model ID.
    pub model: String,
    /// Versioned role prompt.
    pub prompt_version: String,
    /// Versioned recipe.
    pub recipe_version: String,
    /// Optional external source URL.
    #[serde(default)]
    pub source_url: Option<String>,
    /// Source creator.
    #[serde(default)]
    pub creator: Option<String>,
    /// Established license.
    #[serde(default)]
    pub license: Option<String>,
    /// Current validation status.
    pub validation_status: String,
}

/// One authoritative scene graph node.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneNode {
    /// Schema version.
    pub schema_version: u32,
    /// Stable hierarchical ID.
    pub node_id: String,
    /// Monotonic revision.
    pub revision: u32,
    /// Semantic kind.
    pub kind: String,
    /// Unique transform parent, absent only for root.
    pub parent_id: Option<String>,
    /// Ordered owned children.
    pub child_ids: Vec<String>,
    /// Prompt-derived purpose.
    pub purpose: String,
    /// Required semantic features.
    pub required_features: Vec<String>,
    /// Explicit autonomous assumptions.
    pub assumptions: Vec<String>,
    /// Design intent.
    pub design: String,
    /// Construction guidance subordinate to numeric fields.
    pub construction_instructions: String,
    /// Fixed to `m` in v1.
    pub units: String,
    /// Node-local to parent transform.
    pub transform: Transform,
    /// Explicit local pivot in geometry-local coordinates.
    pub pivot_local: [f64; 3],
    /// Declared local front vector.
    pub local_front: [f64; 3],
    /// Declared local up vector.
    pub local_up: [f64; 3],
    /// Occupied geometry bounds in node-local coordinates.
    pub bounds_local: Bounds,
    /// Usable interior in node-local coordinates, if this node is a space.
    pub usable_volume_local: Option<Bounds>,
    /// Child allocations established before child design.
    pub child_regions: Vec<AllocationRegion>,
    /// Region ID selected in the parent.
    pub placement_region: Option<String>,
    /// Named connection frames.
    pub interfaces: Vec<Interface>,
    /// Non-ownership relationships.
    pub relationships: Vec<Relationship>,
    /// Typed construction dimensions/profiles/joints/gaps/layers.
    pub construction: BTreeMap<String, serde_json::Value>,
    /// Physically scaled PBR references.
    pub materials: Vec<MaterialRef>,
    /// Generation/retrieval contract.
    pub generation: GenerationStrategy,
    /// Numeric and visual acceptance.
    pub acceptance: Acceptance,
    /// Full provenance.
    pub provenance: Provenance,
}

/// Complete authoritative graph snapshot.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneSpec {
    /// Contract version.
    pub schema_version: u32,
    /// Root node ID.
    pub root_id: String,
    /// Immutable source prompt summary.
    pub prompt_summary: String,
    /// Explicit canonical convention.
    pub coordinate_system: String,
    /// Nodes keyed separately by stable IDs on disk; represented as a list on wire.
    pub nodes: Vec<SceneNode>,
}

impl Transform {
    /// Validate finite translation, identity scale, and normalized quaternion.
    pub fn validate(&self) -> Result<()> {
        if !self
            .translation
            .iter()
            .chain(self.rotation_xyzw.iter())
            .chain(self.scale.iter())
            .all(|v| v.is_finite())
        {
            return Err(SceneError::Validation("non-finite transform".into()));
        }
        if self.scale.iter().any(|v| *v <= 0.0) {
            return Err(SceneError::Validation(
                "reflection/zero scale is forbidden".into(),
            ));
        }
        if self.scale.iter().any(|v| (*v - 1.0).abs() > 1e-9) {
            return Err(SceneError::Validation(
                "accepted generated assets require identity scale".into(),
            ));
        }
        let norm = self.rotation_xyzw.iter().map(|v| v * v).sum::<f64>().sqrt();
        if (norm - 1.0).abs() > 1e-6 {
            return Err(SceneError::Validation(format!(
                "quaternion is not normalized (norm {norm})"
            )));
        }
        Ok(())
    }
    /// Compose parent-world and child-local transforms.
    pub fn compose(self, child: Self) -> Self {
        let rotated = rotate(self.rotation_xyzw, child.translation);
        Self {
            translation: [
                self.translation[0] + rotated[0],
                self.translation[1] + rotated[1],
                self.translation[2] + rotated[2],
            ],
            rotation_xyzw: quat_mul(self.rotation_xyzw, child.rotation_xyzw),
            scale: [1.0; 3],
        }
    }
    /// Transform a point from local into parent/world coordinates.
    pub fn point(&self, p: [f64; 3]) -> [f64; 3] {
        let r = rotate(self.rotation_xyzw, p);
        [
            r[0] + self.translation[0],
            r[1] + self.translation[1],
            r[2] + self.translation[2],
        ]
    }
}

impl SceneSpec {
    /// Validate schema/references, ownership acyclicity, transforms, regions,
    /// containment, interfaces, and required v1 invariants.
    pub fn validate(&self, abs_tol: f64, rel_tol: f64) -> Result<()> {
        if self.schema_version != 1 || self.coordinate_system != "RH_M_ZUP_XEAST_YNORTH" {
            return Err(SceneError::Validation(
                "scene must use v1 RH_M_ZUP_XEAST_YNORTH".into(),
            ));
        }
        let map: HashMap<_, _> = self.nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
        if map.len() != self.nodes.len() || !map.contains_key(self.root_id.as_str()) {
            return Err(SceneError::Validation(
                "duplicate node ID or missing root".into(),
            ));
        }
        let mut referenced = HashSet::new();
        for n in &self.nodes {
            if n.schema_version != 1 || n.revision == 0 || n.units != "m" {
                return Err(SceneError::Validation(format!(
                    "{} has invalid schema/revision/units",
                    n.node_id
                )));
            }
            n.transform.validate()?;
            validate_bounds(&n.bounds_local, &n.node_id)?;
            if length(n.local_front) - 1.0 > 1e-6
                || (length(n.local_front) - 1.0).abs() > 1e-6
                || (length(n.local_up) - 1.0).abs() > 1e-6
                || dot(n.local_front, n.local_up).abs() > 1e-6
            {
                return Err(SceneError::Validation(format!(
                    "{} front/up must be normalized and orthogonal",
                    n.node_id
                )));
            }
            if n.node_id == self.root_id {
                if n.parent_id.is_some() {
                    return Err(SceneError::Validation("root cannot have parent".into()));
                }
            } else if n.parent_id.as_deref().and_then(|p| map.get(p)).is_none() {
                return Err(SceneError::Validation(format!(
                    "{} has missing parent",
                    n.node_id
                )));
            }
            let mut child_set = HashSet::new();
            for child in &n.child_ids {
                if !child_set.insert(child)
                    || map.get(child.as_str()).and_then(|c| c.parent_id.as_deref())
                        != Some(n.node_id.as_str())
                {
                    return Err(SceneError::Validation(format!(
                        "{} child ownership mismatch: {child}",
                        n.node_id
                    )));
                }
                referenced.insert(child.as_str());
            }
            for r in &n.relationships {
                if !map.contains_key(r.target.split('#').next().unwrap_or("")) {
                    return Err(SceneError::Validation(format!(
                        "{} relationship target missing: {}",
                        n.node_id, r.target
                    )));
                }
            }
            for region in &n.child_regions {
                validate_bounds(
                    &region.allowed,
                    &format!("{}#{}", n.node_id, region.region_id),
                )?;
            }
            for interface in &n.interfaces {
                interface.frame.validate()?;
                if interface.tolerance_m <= 0.0 || interface.tolerance_degrees <= 0.0 {
                    return Err(SceneError::Validation(format!(
                        "{} interface tolerance invalid",
                        n.node_id
                    )));
                }
            }
        }
        if referenced.len() + 1 != self.nodes.len() {
            return Err(SceneError::Validation(
                "ownership graph is disconnected or multiply owned".into(),
            ));
        }
        let mut visiting = HashSet::new();
        let mut visited = HashSet::new();
        self.visit(&self.root_id, &map, &mut visiting, &mut visited)?;
        let worlds = self.world_transforms()?;
        for n in &self.nodes {
            if let Some(parent_id) = &n.parent_id {
                let parent = map[parent_id.as_str()];
                let parent_world = worlds[parent_id];
                let world = worlds[&n.node_id];
                let allowed = match &n.placement_region {
                    Some(id) => parent
                        .child_regions
                        .iter()
                        .find(|r| r.region_id == *id)
                        .map(|r| r.allowed)
                        .unwrap_or(parent.bounds_local),
                    None => parent.bounds_local,
                };
                // Compare in parent-local space: child transform is already relative to parent.
                for c in n.bounds_local.corners().map(|p| n.transform.point(p)) {
                    if !allowed.contains(c, abs_tol, rel_tol) {
                        return Err(SceneError::Validation(format!(
                            "{} exceeds parent allocation",
                            n.node_id
                        )));
                    }
                }
                let _ = (parent_world, world); // documented world composition is exercised above and by tests.
            }
        }
        self.validate_interfaces(abs_tol)?;
        Ok(())
    }

    fn visit<'a>(
        &self,
        id: &'a str,
        map: &HashMap<&'a str, &'a SceneNode>,
        visiting: &mut HashSet<&'a str>,
        visited: &mut HashSet<&'a str>,
    ) -> Result<()> {
        if visited.contains(id) {
            return Ok(());
        }
        if !visiting.insert(id) {
            return Err(SceneError::Validation("ownership cycle".into()));
        }
        for child in &map[id].child_ids {
            self.visit(child, map, visiting, visited)?;
        }
        visiting.remove(id);
        visited.insert(id);
        Ok(())
    }

    /// Compute every node's local-to-world transform.
    pub fn world_transforms(&self) -> Result<HashMap<String, Transform>> {
        let map: HashMap<_, _> = self.nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
        let mut out = HashMap::new();
        fn calc(
            id: &str,
            map: &HashMap<&str, &SceneNode>,
            out: &mut HashMap<String, Transform>,
        ) -> Result<Transform> {
            if let Some(t) = out.get(id) {
                return Ok(*t);
            }
            let n = map
                .get(id)
                .ok_or_else(|| SceneError::Validation(format!("missing node {id}")))?;
            let t = if let Some(p) = &n.parent_id {
                calc(p, map, out)?.compose(n.transform)
            } else {
                n.transform
            };
            out.insert(id.into(), t);
            Ok(t)
        }
        for n in &self.nodes {
            calc(&n.node_id, &map, &mut out)?;
        }
        Ok(out)
    }

    fn validate_interfaces(&self, default_tol: f64) -> Result<()> {
        let map: HashMap<_, _> = self.nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
        let worlds = self.world_transforms()?;
        for n in &self.nodes {
            for i in &n.interfaces {
                if let Some(partner) = &i.partner {
                    let (pid, pname) = partner.split_once('#').ok_or_else(|| {
                        SceneError::Validation(format!("bad interface partner {partner}"))
                    })?;
                    let pn = map
                        .get(pid)
                        .ok_or_else(|| SceneError::Validation(format!("missing partner {pid}")))?;
                    let pi = pn
                        .interfaces
                        .iter()
                        .find(|x| x.name == pname)
                        .ok_or_else(|| {
                            SceneError::Validation(format!("missing partner interface {partner}"))
                        })?;
                    let a = worlds[&n.node_id].compose(i.frame);
                    let b = worlds[pid].compose(pi.frame);
                    let d = distance(a.translation, b.translation);
                    let qa = quat_dot(a.rotation_xyzw, b.rotation_xyzw).abs().min(1.0);
                    let deg = 2.0 * qa.acos().to_degrees();
                    let tol = i.tolerance_m.min(pi.tolerance_m).min(default_tol);
                    let at = i.tolerance_degrees.min(pi.tolerance_degrees);
                    if d > tol || (deg > at && (180.0 - deg).abs() > at) {
                        return Err(SceneError::Validation(format!(
                            "connector residual {}#{} to {partner}: {d:.6}m {deg:.4}deg",
                            n.node_id, i.name
                        )));
                    }
                }
            }
        }
        Ok(())
    }
}

/// Convert a canonical point to glTF coordinates exactly once: `(x,y,z)` to
/// `(x,z,-y)`.
pub fn canonical_to_gltf(p: [f64; 3]) -> [f64; 3] {
    [p[0], p[2], -p[1]]
}

fn validate_bounds(b: &Bounds, id: &str) -> Result<()> {
    if !b.min.iter().chain(b.max.iter()).all(|v| v.is_finite())
        || (0..3).any(|i| b.min[i] > b.max[i])
    {
        Err(SceneError::Validation(format!("invalid bounds for {id}")))
    } else {
        Ok(())
    }
}
fn length(a: [f64; 3]) -> f64 {
    dot(a, a).sqrt()
}
fn dot(a: [f64; 3], b: [f64; 3]) -> f64 {
    a[0] * b[0] + a[1] * b[1] + a[2] * b[2]
}
fn distance(a: [f64; 3], b: [f64; 3]) -> f64 {
    length([a[0] - b[0], a[1] - b[1], a[2] - b[2]])
}
fn quat_dot(a: [f64; 4], b: [f64; 4]) -> f64 {
    a.iter().zip(b).map(|(x, y)| x * y).sum()
}
fn quat_mul(a: [f64; 4], b: [f64; 4]) -> [f64; 4] {
    let [x, y, z, w] = a;
    let [q, r, s, t] = b;
    [
        w * q + x * t + y * s - z * r,
        w * r - x * s + y * t + z * q,
        w * s + x * r - y * q + z * t,
        w * t - x * q - y * r - z * s,
    ]
}
fn rotate(q: [f64; 4], v: [f64; 3]) -> [f64; 3] {
    let p = [v[0], v[1], v[2], 0.0];
    let qi = [-q[0], -q[1], -q[2], q[3]];
    let r = quat_mul(quat_mul(q, p), qi);
    [r[0], r[1], r[2]]
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn rotated_composition_and_axis_fixture() {
        let q = [
            0.0,
            0.0,
            std::f64::consts::FRAC_1_SQRT_2,
            std::f64::consts::FRAC_1_SQRT_2,
        ];
        let p = Transform {
            translation: [10.0, 0.0, 0.0],
            rotation_xyzw: q,
            scale: [1.0; 3],
        };
        let c = Transform {
            translation: [2.0, 0.0, 0.0],
            ..Default::default()
        };
        let w = p.compose(c);
        assert!((w.translation[0] - 10.0).abs() < 1e-9);
        assert!((w.translation[1] - 2.0).abs() < 1e-9);
        assert_eq!(canonical_to_gltf([1.0, 2.0, 3.0]), [1.0, 3.0, -2.0]);
    }
    #[test]
    fn reflection_is_rejected() {
        let t = Transform {
            scale: [-1.0, 1.0, 1.0],
            ..Default::default()
        };
        assert!(t.validate().is_err());
    }
    #[test]
    fn shipped_complete_example_matches_contract() {
        let spec: SceneSpec =
            serde_json::from_str(include_str!("../examples/complete-scene.json")).unwrap();
        spec.validate(0.005, 1e-5).unwrap();
    }
}
