//! Scene-specific spatial programs, scoped specialists, and deterministic compilation.
use crate::{
    config::SceneConfig,
    roles::{Role, RoleInvoker},
    spec::*,
    storage::{atomic_write, hash_bytes, RunStore},
    tasks::SceneTask,
    Result, SceneError,
};
use futures::{stream, StreamExt};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, HashSet},
    path::Path,
};
use tokio_util::sync::CancellationToken;

const VERSION: &str = "adaptive-program-v2-presentation";
const GEOMETRY: &[&str] = &[
    "asset",
    "mesh",
    "extrusion",
    "lathe",
    "sweep",
    "heightfield",
    "wall",
    "slab",
    "stairs",
    "window",
    "door",
    "light",
];
const VOXEL: &[&str] = &[
    "voxel_terrain",
    "voxel_tower",
    "voxel_wall",
    "voxel_gatehouse",
    "voxel_house",
    "voxel_pyramid",
    "voxel_tree",
    "voxel_bridge",
    "voxel_waterfall",
    "voxel_stairs",
];

#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct SceneProgram {
    description: String,
    scene_kind: String,
    visual_style: String,
    topology: String,
    #[serde(default)]
    environment: Option<crate::presentation::Environment>,
    required_features: Vec<String>,
    #[serde(default)]
    unresolved_assets: Vec<MissingAsset>,
    regions: Vec<Region>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct MissingAsset {
    name: String,
    category: String,
    reason: String,
    #[serde(default = "missing_model_type")]
    asset_type: String,
}
fn missing_model_type() -> String {
    "models".into()
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Region {
    id: String,
    name: String,
    purpose: String,
    position: [f64; 3],
    size: [f64; 3],
    #[serde(default)]
    rotation: [f64; 3],
    required_features: Vec<String>,
    detail_policy: String,
    object_budget: usize,
    #[serde(default)]
    connections: Vec<Connection>,
    #[serde(default)]
    review_cameras: Vec<ReviewCamera>,
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ReviewCamera {
    id: String,
    position: [f64; 3],
    target: [f64; 3],
    #[serde(default = "camera_lens")]
    lens_mm: f64,
    #[serde(default)]
    coverage: String,
    #[serde(default)]
    allow_subsurface: bool,
}
fn camera_lens() -> f64 {
    35.0
}
#[derive(Clone, Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Connection {
    target: String,
    at: [f64; 3],
    target_at: [f64; 3],
    width_m: f64,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct ProgramReview {
    approved: bool,
    issues: Vec<String>,
    #[serde(default)]
    revised_program: Option<SceneProgram>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(untagged)]
enum ReviewedProgram {
    Review(ProgramReview),
    // Some reviewers return the complete corrected program directly. This is
    // still typed, independently authored and fully validated, not a free-text
    // approval; accept it without paying for an envelope-only correction.
    Complete(SceneProgram),
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegionPlan {
    #[serde(default)]
    description: String,
    objects: Vec<Object>,
    #[serde(default)]
    unresolved_assets: Vec<MissingAsset>,
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct RegionPatch {
    changed_objects: Vec<Object>,
    #[serde(default)]
    removed_object_ids: Vec<String>,
    #[serde(default)]
    unresolved_assets: Vec<MissingAsset>,
}
#[derive(Serialize)]
#[serde(untagged)]
enum RegionRevision {
    Patch(RegionPatch),
    Complete(RegionPlan),
}
impl<'de> Deserialize<'de> for RegionRevision {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let value = Value::deserialize(deserializer)?;
        if value.get("changed_objects").is_some() {
            serde_json::from_value(value)
                .map(Self::Patch)
                .map_err(serde::de::Error::custom)
        } else {
            serde_json::from_value(value)
                .map(Self::Complete)
                .map_err(serde::de::Error::custom)
        }
    }
}
impl RegionRevision {
    fn apply(self, previous: Option<RegionPlan>) -> Result<RegionPlan> {
        let patch = match self {
            Self::Complete(plan) => return Ok(plan),
            Self::Patch(patch) => patch,
        };
        let mut plan =
            previous.ok_or_else(|| err("initial design must return a complete RegionPlan"))?;
        let mut changed = HashSet::new();
        for object in &patch.changed_objects {
            if !changed.insert(object.id.clone()) || patch.removed_object_ids.contains(&object.id) {
                return Err(err("conflicting object patch IDs"));
            }
        }
        if patch
            .removed_object_ids
            .iter()
            .any(|id| !plan.objects.iter().any(|o| &o.id == id))
        {
            return Err(err("patch removes unknown object"));
        }
        plan.objects
            .retain(|o| !patch.removed_object_ids.contains(&o.id));
        for object in patch.changed_objects {
            if let Some(old) = plan.objects.iter_mut().find(|o| o.id == object.id) {
                *old = object;
            } else {
                plan.objects.push(object);
            }
        }
        plan.unresolved_assets = patch.unresolved_assets;
        Ok(plan)
    }
}
#[derive(Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct Object {
    id: String,
    name: String,
    semantic_class: String,
    recipe: String,
    size: [f64; 3],
    position: [f64; 3],
    #[serde(default)]
    support: Option<String>,
    #[serde(default)]
    distribution: Option<crate::distribution::Distribution>,
    #[serde(default)]
    rotation: [f64; 3],
    #[serde(default)]
    parameters: BTreeMap<String, Value>,
    #[serde(default = "neutral_color")]
    color: [f64; 3],
    #[serde(default = "default_roughness")]
    roughness: f64,
    #[serde(default)]
    metallic: f64,
    required_features: Vec<String>,
}
#[derive(Serialize, Deserialize)]
struct Checkpoint<T> {
    input_hash: String,
    output: T,
    model: String,
    prompt_version: String,
}

// Accept unambiguous recipe-parameter shorthand without spending another full
// model call solely to move a coordinate array under `parameters`. Conflicts
// and all unknown fields still fail the original typed schema.
struct AdaptiveResponse<T>(T);
impl<'de, T: DeserializeOwned> Deserialize<'de> for AdaptiveResponse<T> {
    fn deserialize<D: serde::Deserializer<'de>>(
        deserializer: D,
    ) -> std::result::Result<Self, D::Error> {
        let mut value = Value::deserialize(deserializer)?;
        let object_key = if value.get("changed_objects").is_some() {
            "changed_objects"
        } else {
            "objects"
        };
        if let Some(objects) = value.get_mut(object_key).and_then(Value::as_array_mut) {
            for object in objects {
                let Some(object) = object.as_object_mut() else {
                    continue;
                };
                if let Some(recipe) = object.get("recipe").and_then(Value::as_object) {
                    // A source reference alone identifies exactly one recipe;
                    // never infer a recipe from arbitrary geometry or a label.
                    if recipe.len() == 1 && recipe.get("asset_id").is_some_and(Value::is_string) {
                        let asset_id = recipe["asset_id"].clone();
                        if object.get("asset_id").is_some_and(|v| v != &asset_id) {
                            return Err(serde::de::Error::custom("conflicting asset references"));
                        }
                        object.insert("asset_id".into(), asset_id);
                        object.insert("recipe".into(), json!("asset"));
                    }
                }
                if let Some(distribution) = object.remove("optional_asset_distribution") {
                    if object
                        .get("distribution")
                        .is_some_and(|v| v != &distribution)
                    {
                        return Err(serde::de::Error::custom("conflicting source distributions"));
                    }
                    object.insert("distribution".into(), distribution);
                }
                let fields: &[&str] = match object.get("recipe").and_then(Value::as_str) {
                    Some("asset") => &["asset_id"],
                    Some("heightfield") => &[
                        "heights",
                        "smooth",
                        "material_asset_id",
                        "texture_scale_m",
                        "material_name",
                    ],
                    Some("mesh") => &[
                        "vertices",
                        "faces",
                        "material_indices",
                        "smooth",
                        "material_asset_id",
                        "texture_scale_m",
                        "material_name",
                    ],
                    Some("extrusion") => &[
                        "profile",
                        "height",
                        "base_z",
                        "material_asset_id",
                        "texture_scale_m",
                        "material_name",
                    ],
                    Some("lathe") => &[
                        "profile",
                        "segments",
                        "cap_ends",
                        "material_asset_id",
                        "texture_scale_m",
                        "material_name",
                    ],
                    Some("sweep") => &[
                        "profile",
                        "path",
                        "path_arc",
                        "closed",
                        "material_asset_id",
                        "texture_scale_m",
                        "material_name",
                    ],
                    _ => &[],
                };
                let recipe = object
                    .get("recipe")
                    .and_then(Value::as_str)
                    .unwrap_or("")
                    .to_owned();
                // Some planners use the recipe name as the parameter envelope.
                // This is lossless only for the object's exact declared recipe.
                let envelopes = if fields.is_empty() {
                    vec!["geometry"]
                } else {
                    vec!["geometry", recipe.as_str()]
                };
                for envelope in envelopes {
                    if let Some(geometry) = object.remove(envelope) {
                        let geometry = geometry.as_object().ok_or_else(|| {
                            serde::de::Error::custom("geometry must be a recipe parameter object")
                        })?;
                        for (key, value) in geometry {
                            if !fields.contains(&key.as_str()) {
                                return Err(serde::de::Error::custom(format!(
                                    "unsupported geometry parameter {key}"
                                )));
                            }
                            if object.get(key).is_some_and(|v| v != value) {
                                return Err(serde::de::Error::custom(format!(
                                    "conflicting {key} geometry"
                                )));
                            }
                            object.insert(key.clone(), value.clone());
                        }
                    }
                }
                for key in fields {
                    if let Some(shorthand) = object.remove(*key) {
                        let parameters = object
                            .entry("parameters")
                            .or_insert_with(|| json!({}))
                            .as_object_mut()
                            .ok_or_else(|| {
                                serde::de::Error::custom("parameters must be an object")
                            })?;
                        if parameters.get(*key).is_some_and(|v| v != &shorthand) {
                            return Err(serde::de::Error::custom(format!(
                                "conflicting {key} parameters"
                            )));
                        }
                        parameters.insert((*key).to_owned(), shorthand);
                    }
                }
            }
        }
        serde_json::from_value(value)
            .map(Self)
            .map_err(serde::de::Error::custom)
    }
}

fn neutral_color() -> [f64; 3] {
    [0.5; 3]
}
fn default_roughness() -> f64 {
    0.6
}

fn err(e: impl std::fmt::Display) -> SceneError {
    SceneError::Validation(e.to_string())
}
fn check_cancel(root: &Path, cancel: &CancellationToken) -> Result<()> {
    if cancel.is_cancelled() || root.join("CANCELLED").exists() {
        Err(SceneError::Cancelled)
    } else {
        Ok(())
    }
}

async fn cached_call<T: Serialize + DeserializeOwned>(
    path: &Path,
    role: Role,
    payload: &Value,
    config: &SceneConfig,
    root: &Path,
    invoker: &RoleInvoker,
    store: &RunStore,
    cancel: &CancellationToken,
) -> Result<Checkpoint<T>> {
    check_cancel(root, cancel)?;
    let input_hash = hash_bytes(&serde_json::to_vec(&json!({"version":VERSION,"role_prompt_version":crate::roles::role_registry()[&role].prompt_version,"role_system":crate::roles::role_registry()[&role].system,"payload":payload,"models":config.models,"planning":config.planning,"quality":config.quality})).map_err(err)?);
    if let Ok(bytes) = std::fs::read(path) {
        if let Ok(saved) = serde_json::from_slice::<Checkpoint<T>>(&bytes) {
            if saved.input_hash == input_hash {
                return Ok(saved);
            }
        }
    }
    let raw_path = path.with_extension("raw.json");
    let response = tokio::select! {
        _ = cancel.cancelled() => return Err(SceneError::Cancelled),
        result = invoker.invoke_json_checkpointed::<AdaptiveResponse<T>>(role, payload, Some((&raw_path, &input_hash))) => result?,
    };
    let checkpoint = Checkpoint {
        input_hash,
        output: response.output.0,
        model: response.model,
        prompt_version: response.prompt_version,
    };
    // Save successful paid work before checking the cumulative budget.
    atomic_write(path, &serde_json::to_vec_pretty(&checkpoint).map_err(err)?)?;
    let (calls, tokens) = store.add_model_usage(response.tokens)?;
    if calls > config.budgets.max_model_calls || tokens > config.budgets.max_total_tokens {
        return Err(SceneError::WaitingExternal(
            "model budget exhausted; adaptive checkpoint preserved".into(),
        ));
    }
    Ok(checkpoint)
}

/// Plan arbitrary scene regions without imposing occupied rooms or preset scenery.
pub(crate) async fn design_scene(
    task: &SceneTask,
    config: &SceneConfig,
    root: &Path,
    invoker: &RoleInvoker,
    store: &RunStore,
    cancel: &CancellationToken,
) -> Result<SceneSpec> {
    let catalog = crate::asset_catalog::catalog_context(config)?;
    let contract = program_contract(task, config, &catalog);
    let folder = root.join("designs/adaptive");
    let draft: Checkpoint<SceneProgram> = cached_call(
        &folder.join("program.json"),
        Role::SceneDirector,
        &contract,
        config,
        root,
        invoker,
        store,
        cancel,
    )
    .await?;
    let review_payload = json!({"contract":contract,"program":draft.output,"required_output":{"approved":true,"issues":[],"revised_program":null},
        "instructions":"Independently review prompt fidelity, plausible real-world scale, site topology, negative space, structural connections, purpose-specific landmarks, asset availability and detail budgets. Reject an unnecessary room-and-interior template, interchangeable themed furniture, generic shapes posing as realistic organisms, or omitted required features. With issues, set approved=false and return a COMPLETE corrected revised_program in the original schema. Do not reduce requested scope to pass. If correct, approved=true and revised_program=null. Budget constraints are hard."});
    let reviewed: Checkpoint<ReviewedProgram> = cached_call(
        &folder.join("program-review.json"),
        Role::StructureDesigner,
        &review_payload,
        config,
        root,
        invoker,
        store,
        cancel,
    )
    .await?;
    let mut program = match reviewed.output {
        ReviewedProgram::Complete(program) => program,
        ReviewedProgram::Review(review) => match review.revised_program {
            Some(program) => program,
            None if review.approved => draft.output,
            None => {
                return Err(err(format!(
                    "scene program review failed: {}",
                    review.issues.join("; ")
                )))
            }
        },
    };
    for attempt in 0..2 {
        let Err(error) = validate_program(&program, config) else {
            break;
        };
        let payload = json!({"contract":contract,"program":program,"validation_error":error.to_string(),"instructions":"Return the COMPLETE corrected SceneProgram in the original schema. Preserve the scene-specific design, required features, regions and assets. Correct all occurrences of the measured constraint without adding a room template or reducing user scope."});
        let repaired: Checkpoint<SceneProgram> = cached_call(
            &folder.join(format!("program-repair-{attempt}.json")),
            Role::StructureDesigner,
            &payload,
            config,
            root,
            invoker,
            store,
            cancel,
        )
        .await?;
        program = repaired.output;
    }
    validate_program(&program, config)?;
    atomic_write(
        &folder.join("accepted-program.json"),
        &serde_json::to_vec_pretty(&program).map_err(err)?,
    )?;
    record_asset_requirements(&folder, "program", &program.unresolved_assets)?;
    ensure_assets_available(&program.unresolved_assets)?;

    let jobs = program.regions.iter().enumerate().map(|(index, region)| {
        let folder = &folder; let catalog = &catalog; let program = &program;
        async move {
            let base = region_contract(task, config, program, region, catalog);
            let mut payload = base.clone();
            let mut previous=None;
            for attempt in 0..=config.recovery.repair_attempts_per_strategy {
                let response: Checkpoint<RegionRevision> = cached_call(&folder.join(format!("region-{}-{attempt}.json", region.id)), Role::DesignCoordinator, &payload, config, root, invoker, store, cancel).await?;
                let plan=response.output.apply(previous.take())?;
                record_asset_requirements(folder, &format!("region-{}",region.id), &plan.unresolved_assets)?;
                ensure_assets_available(&plan.unresolved_assets)?;
                match compile_region(region, &plan, program, config, task.seed.unwrap_or(0), &response.model, &response.prompt_version) {
                    Ok(nodes) => return Ok((index, nodes)),
                    Err(error) if attempt < config.recovery.repair_attempts_per_strategy => {
                        payload = json!({"contract":base,"previous_plan":plan,"validation_error":error.to_string(),"required_output":{"changed_objects":["complete Object records ONLY for changed or added objects"],"removed_object_ids":[],"unresolved_assets":[]},"instructions":"Return a RegionPatch containing only changed objects, using the Object schema from the contract. Omitted objects are preserved exactly; do not reprint them. Correct every instance of the measured issue and all missing required feature references. Preserve required scene features and design. Do not add a shell, unrelated furniture or proxy shapes. A complete RegionPlan is also accepted if necessary."});
                        previous=Some(plan);
                    },
                    Err(error) => return Err(error),
                }
            }
            unreachable!()
        }
    });
    let mut pending = stream::iter(jobs).buffer_unordered(config.execution.max_agent_tasks.max(1));
    let mut completed = Vec::new();
    let mut failure = None;
    // Drain independent tasks so already-running specialists retain checkpoints
    // even if a sibling fails; no background tasks outlive the controller.
    while let Some(result) = pending.next().await {
        check_cancel(root, cancel)?;
        match result {
            Ok(nodes) => completed.push(nodes),
            Err(error) => {
                if failure.is_none() {
                    failure = Some(error);
                }
            }
        }
    }
    if let Some(error) = failure {
        return Err(error);
    }
    completed.sort_by_key(|(index, _)| *index);
    let mut root_node = node(
        "scene",
        None,
        &program.scene_kind,
        &program.description,
        program_bounds(&program),
        task.seed.unwrap_or(0),
    );
    root_node.required_features = program.required_features.clone();
    if let Some(environment) = &program.environment {
        root_node
            .generation
            .parameters
            .insert("environment".into(), json!(environment));
    }
    root_node.design = format!(
        "{}\nTopology: {}\nStyle: {}",
        program.description, program.topology, program.visual_style
    );
    root_node.child_ids = program
        .regions
        .iter()
        .map(|r| format!("region-{}", r.id))
        .collect();
    root_node.provenance.model = reviewed.model;
    root_node.provenance.prompt_version = VERSION.into();
    let mut nodes = vec![root_node];
    for (_, region_nodes) in completed {
        nodes.extend(region_nodes);
    }
    if nodes.len() > config.budgets.max_nodes {
        return Err(err("adaptive scene exceeds configured node budget"));
    }
    let spec = SceneSpec {
        schema_version: 1,
        root_id: "scene".into(),
        prompt_summary: task.prompt.clone(),
        coordinate_system: "RH_M_ZUP_XEAST_YNORTH".into(),
        nodes,
    };
    spec.validate(
        config.quality.containment_tolerance_m,
        config.quality.relative_tolerance,
    )?;
    atomic_write(
        &folder.join("geometry-signature.json"),
        &serde_json::to_vec_pretty(&signature(&spec, &program)).map_err(err)?,
    )?;
    Ok(spec)
}

fn ensure_assets_available(missing: &[MissingAsset]) -> Result<()> {
    if missing.is_empty() {
        return Ok(());
    }
    Err(SceneError::WaitingExternal(format!("required scene assets are unavailable; add matching production models to assets.catalog: {}", missing.iter().map(|a| format!("{} ({}): {}", a.name, a.category, a.reason)).collect::<Vec<_>>().join("; "))))
}

fn record_asset_requirements(folder: &Path, scope: &str, missing: &[MissingAsset]) -> Result<()> {
    for requirement in missing {
        if !matches!(requirement.asset_type.as_str(), "models" | "textures") {
            return Err(err("missing asset_type must be models or textures"));
        }
    }
    atomic_write(
        &folder.join(format!("asset-requirements-{scope}.json")),
        &serde_json::to_vec_pretty(&json!({"requirements":missing})).map_err(err)?,
    )
}

/// Redesign only the responsible region using actual visual findings. Parent
/// topology, feature ownership and allocation remain the accepted contract.
pub(crate) async fn repair_region(
    task: &SceneTask,
    config: &SceneConfig,
    root: &Path,
    invoker: &RoleInvoker,
    store: &RunStore,
    cancel: &CancellationToken,
    spec: &SceneSpec,
    affected: &str,
    findings: &Value,
) -> Result<Vec<SceneNode>> {
    let folder = root.join("designs/adaptive");
    let mut program: SceneProgram =
        serde_json::from_slice(&std::fs::read(folder.join("accepted-program.json")).map_err(err)?)
            .map_err(err)?;
    // Current presentation revisions are authoritative; a later geometry repair
    // must not restore cameras/lighting from the original planning checkpoint.
    for region in &mut program.regions {
        if let Some(value) = spec
            .nodes
            .iter()
            .find(|n| n.node_id == format!("region-{}", region.id))
            .and_then(|n| n.generation.parameters.get("review_cameras"))
        {
            region.review_cameras = serde_json::from_value(value.clone()).map_err(err)?;
        }
    }
    if let Some(value) = spec
        .nodes
        .iter()
        .find(|n| n.node_id == spec.root_id)
        .and_then(|n| n.generation.parameters.get("environment"))
    {
        program.environment = Some(serde_json::from_value(value.clone()).map_err(err)?);
    }
    let target = spec
        .nodes
        .iter()
        .find(|n| n.node_id == affected)
        .ok_or_else(|| err("visual repair references unknown node"))?;
    let region_id = if target.kind == "scene_region" {
        &target.node_id
    } else {
        target
            .parent_id
            .as_ref()
            .ok_or_else(|| err("scene-wide topology revision requires a new program"))?
    };
    let region = program
        .regions
        .iter()
        .find(|r| format!("region-{}", r.id) == *region_id)
        .ok_or_else(|| err("visual repair must target an existing region or its object"))?;
    let catalog = crate::asset_catalog::catalog_context(config)?;
    let contract = region_contract(task, config, &program, region, &catalog);
    let previous = spec
        .nodes
        .iter()
        .filter(|n| n.node_id == *region_id || n.parent_id.as_ref() == Some(region_id))
        .collect::<Vec<_>>();
    let revision = previous.iter().map(|n| n.revision).max().unwrap_or(1) + 1;
    let local_ids = previous
        .iter()
        .filter(|n| n.parent_id.as_ref() == Some(region_id))
        .filter_map(|n| {
            n.node_id
                .strip_prefix(&format!("{region_id}-"))
                .map(|id| (n.node_id.clone(), id.to_owned()))
        })
        .collect::<BTreeMap<_, _>>();
    let mut payload = json!({"contract":contract,"previous_region":previous,"existing_object_ids":local_ids,"visual_findings":findings,
        "instructions":"Return a complete corrected RegionPlan. Reproduce the existing intended scene and required features, correct the camera-visible failures and preserve all unaffected objects and IDs. Keep the accepted region bounds and transform. Use explicit support references for contact and verified material_asset_id for textured authored surfaces. Do not invent assets, downgrade realism or remove requested features to pass. If essential sources are absent, report unresolved_assets."});
    for attempt in 0..=config.recovery.repair_attempts_per_strategy {
        let response: Checkpoint<RegionPlan> = cached_call(
            &folder.join(format!(
                "visual-repair-{}-{revision}-{attempt}.json",
                region.id
            )),
            Role::DesignCoordinator,
            &payload,
            config,
            root,
            invoker,
            store,
            cancel,
        )
        .await?;
        let mut plan = response.output;
        // Existing graph IDs are region-prefixed; Object IDs are region-local.
        // Normalize exact known references only, avoiding prefix growth across
        // repeated visual repairs while retaining genuinely new object IDs.
        for object in &mut plan.objects {
            if let Some(local) = local_ids.get(&object.id) {
                object.id = local.clone();
            }
            if let Some(local) = object.support.as_ref().and_then(|id| local_ids.get(id)) {
                object.support = Some(local.clone());
            }
        }
        record_asset_requirements(
            &folder,
            &format!("visual-{}-{revision}", region.id),
            &plan.unresolved_assets,
        )?;
        ensure_assets_available(&plan.unresolved_assets)?;
        match compile_region(
            region,
            &plan,
            &program,
            config,
            task.seed.unwrap_or(0),
            &response.model,
            &response.prompt_version,
        ) {
            Ok(mut nodes) => {
                for n in &mut nodes {
                    n.revision = revision;
                }
                return Ok(nodes);
            }
            Err(error) if attempt < config.recovery.repair_attempts_per_strategy => {
                payload = json!({"contract":contract,"previous_plan":plan,"visual_findings":findings,"validation_error":error.to_string(),"instructions":"Return a complete corrected RegionPlan; preserve scene features and fix the measured constraint."});
            }
            Err(error) => return Err(error),
        }
    }
    unreachable!()
}
fn valid_id(id: &str) -> bool {
    !id.is_empty()
        && id.len() <= 64
        && id
            .bytes()
            .all(|b| b.is_ascii_alphanumeric() || b == b'-' || b == b'_')
}

/// Revise only presentation metadata, preserving every geometry transform/bound.
pub(crate) fn repair_presentation(
    spec: &mut SceneSpec,
    affected: &str,
    parameters: &BTreeMap<String, Value>,
    config: &SceneConfig,
) -> Result<Vec<String>> {
    if parameters.is_empty()
        || parameters
            .keys()
            .any(|k| !matches!(k.as_str(), "review_cameras" | "environment"))
    {
        return Err(err(
            "presentation repair permits only review_cameras or environment",
        ));
    }
    let index = spec
        .nodes
        .iter()
        .position(|n| n.node_id == affected)
        .ok_or_else(|| err("presentation repair references unknown node"))?;
    let node = &spec.nodes[index];
    let mut parameters = parameters.clone();
    if let Some(update) = parameters.get("environment") {
        let mut environment = node
            .generation
            .parameters
            .get("environment")
            .cloned()
            .unwrap_or(json!({}));
        merge_presentation_fields(&mut environment, update);
        parameters.insert("environment".into(), environment);
    }
    // A root-level response may bundle cameras with exposure. Route cameras
    // only when their region is unambiguous; never guess between world zones.
    if affected == spec.root_id && parameters.contains_key("review_cameras") {
        let regions = spec
            .nodes
            .iter()
            .filter(|n| n.kind == "scene_region" && n.node_id != spec.root_id)
            .map(|n| n.node_id.clone())
            .collect::<Vec<_>>();
        if regions.len() != 1 {
            return Err(err(
                "root camera repair needs an exact region when multiple regions exist",
            ));
        }
        let mut revised = spec.clone();
        let mut root_parameters = parameters.clone();
        let cameras = root_parameters.remove("review_cameras").unwrap();
        let mut changed = Vec::new();
        if !root_parameters.is_empty() {
            changed.extend(repair_presentation(
                &mut revised,
                affected,
                &root_parameters,
                config,
            )?);
        }
        changed.extend(repair_presentation(
            &mut revised,
            &regions[0],
            &BTreeMap::from([("review_cameras".into(), cameras)]),
            config,
        )?);
        *spec = revised;
        return Ok(changed);
    }
    if let Some(value) = parameters.get("environment") {
        if node.node_id != spec.root_id {
            return Err(err("environment belongs to the scene root"));
        }
        let environment: crate::presentation::Environment =
            serde_json::from_value(value.clone()).map_err(err)?;
        environment.validate(config.planning.max_extent_m)?;
    }
    if let Some(value) = parameters.get("review_cameras").cloned() {
        if node.kind != "scene_region" {
            return Err(err("review cameras belong to a scene region"));
        }
        let updates: Vec<Value> = serde_json::from_value(value).map_err(err)?;
        if updates.is_empty() {
            return Err(err("review camera patch is empty"));
        }
        let mut cameras = node
            .generation
            .parameters
            .get("review_cameras")
            .and_then(Value::as_array)
            .cloned()
            .unwrap_or_default();
        let mut updated = HashSet::new();
        for update in updates {
            let id = update
                .get("id")
                .and_then(Value::as_str)
                .ok_or_else(|| err("camera patch requires ID"))?;
            if !updated.insert(id.to_owned()) {
                return Err(err("duplicate camera patch ID"));
            }
            if let Some(existing) = cameras
                .iter_mut()
                .find(|c| c.get("id").and_then(Value::as_str) == Some(id))
            {
                merge_presentation_fields(existing, &update);
            } else {
                cameras.push(update);
            }
        }
        let value = json!(cameras);
        let cameras: Vec<ReviewCamera> = serde_json::from_value(value.clone()).map_err(err)?;
        let mut ids = HashSet::new();
        if cameras.is_empty()
            || cameras.len() > 3
            || cameras.iter().any(|c| {
                !valid_id(&c.id)
                    || !ids.insert(&c.id)
                    || !(12.0..=120.0).contains(&c.lens_mm)
                    || c.position.iter().chain(&c.target).any(|v| !v.is_finite())
                    || (0..3).all(|i| (c.position[i] - c.target[i]).abs() < 0.001)
            })
        {
            return Err(err("invalid presentation review cameras"));
        }
        let others: usize = spec
            .nodes
            .iter()
            .filter(|n| n.node_id != affected)
            .filter_map(|n| {
                n.generation
                    .parameters
                    .get("review_cameras")
                    .and_then(Value::as_array)
            })
            .map(Vec::len)
            .sum();
        if others + cameras.len() > 12 {
            return Err(err("at most twelve scene review cameras"));
        }
        parameters.insert("review_cameras".into(), value);
    }
    if parameters
        .iter()
        .all(|(key, value)| node.generation.parameters.get(key) == Some(value))
    {
        return Ok(vec![]);
    }
    let node = &mut spec.nodes[index];
    node.generation.parameters.extend(parameters.clone());
    node.revision += 1;
    node.provenance.validation_status = "presentation_repair_proposed".into();
    Ok(vec![affected.to_owned()])
}

fn merge_presentation_fields(base: &mut Value, update: &Value) {
    if let (Some(base), Some(update)) = (base.as_object_mut(), update.as_object()) {
        for (key, value) in update {
            merge_presentation_fields(base.entry(key.clone()).or_insert(Value::Null), value);
        }
    } else {
        *base = update.clone();
    }
}
fn bounds(size: [f64; 3]) -> Bounds {
    Bounds {
        min: [-size[0] / 2.0, -size[1] / 2.0, 0.0],
        max: [size[0] / 2.0, size[1] / 2.0, size[2]],
    }
}
fn transform(position: [f64; 3], rotation: [f64; 3]) -> Transform {
    let [x, y, z] = rotation.map(|v| v.to_radians() / 2.0);
    let (sx, cx) = x.sin_cos();
    let (sy, cy) = y.sin_cos();
    let (sz, cz) = z.sin_cos();
    Transform {
        translation: position,
        rotation_xyzw: [
            sx * cy * cz - cx * sy * sz,
            cx * sy * cz + sx * cy * sz,
            cx * cy * sz - sx * sy * cz,
            cx * cy * cz + sx * sy * sz,
        ],
        scale: [1.0; 3],
    }
}
fn validate_program(program: &SceneProgram, config: &SceneConfig) -> Result<()> {
    if let Some(environment) = &program.environment {
        environment.validate(config.planning.max_extent_m)?;
    }
    if program.description.trim().is_empty()
        || program.scene_kind.trim().is_empty()
        || program.topology.trim().is_empty()
        || program.required_features.is_empty()
    {
        return Err(err(
            "scene program needs specific design, kind, topology and required features",
        ));
    }
    if !["realistic", "stylized", "voxel", "concept"].contains(&program.visual_style.as_str()) {
        return Err(err(
            "visual_style must be realistic, stylized, voxel or concept",
        ));
    }
    if config.quality.fidelity == "realistic" && program.visual_style != "realistic" {
        return Err(err(
            "realistic fidelity cannot be downgraded by the scene planner",
        ));
    }
    if program.regions.is_empty() || program.regions.len() > config.planning.max_regions {
        return Err(err("scene region count exceeds configured planning limits"));
    }
    let mut ids = HashSet::new();
    let mut allocated = 1usize;
    for region in &program.regions {
        if !valid_id(&region.id) || !ids.insert(&region.id) {
            return Err(err("region IDs must be unique safe identifiers"));
        }
        if region
            .size
            .iter()
            .any(|v| !v.is_finite() || *v <= 0.0 || *v > config.planning.max_extent_m)
            || region
                .position
                .iter()
                .chain(region.rotation.iter())
                .any(|v| !v.is_finite())
        {
            return Err(err(format!("invalid region dimensions: {}", region.id)));
        }
        if region.required_features.is_empty()
            || region.object_budget == 0
            || region.object_budget > config.planning.max_objects_per_region
        {
            return Err(err(format!(
                "invalid region feature/detail allocation: {}",
                region.id
            )));
        }
        allocated = allocated
            .checked_add(region.object_budget + 1)
            .ok_or_else(|| err("node allocation overflow"))?;
        if region.review_cameras.len() > 3 {
            return Err(err("at most three authored review cameras per region"));
        }
        let mut camera_ids = HashSet::new();
        for camera in &region.review_cameras {
            if !valid_id(&camera.id)
                || !camera_ids.insert(&camera.id)
                || camera
                    .position
                    .iter()
                    .chain(camera.target.iter())
                    .any(|v| !v.is_finite())
                || !(12.0..=120.0).contains(&camera.lens_mm)
                || (0..3).all(|i| (camera.position[i] - camera.target[i]).abs() < 0.001)
            {
                return Err(err(format!("invalid regional camera {}", camera.id)));
            }
        }
    }
    if allocated > config.budgets.max_nodes {
        return Err(err(format!(
            "region allocations need {allocated} nodes, budget is {}",
            config.budgets.max_nodes
        )));
    }
    if program
        .regions
        .iter()
        .map(|r| r.review_cameras.len())
        .sum::<usize>()
        > 12
    {
        return Err(err("at most twelve authored scene review cameras"));
    }
    let missing = program
        .required_features
        .iter()
        .filter(|feature| {
            !program
                .regions
                .iter()
                .any(|r| r.required_features.contains(feature))
        })
        .collect::<Vec<_>>();
    if !missing.is_empty() {
        return Err(err(format!("Global features lack exact regional references: {}. Copy EACH of these exact strings into the appropriate region.required_features array; retain regional descriptive features too. Do not paraphrase references.",serde_json::to_string(&missing).unwrap_or_default())));
    }
    if config.budgets.max_depth < 3 {
        return Err(err(
            "adaptive planning needs at least three hierarchy levels",
        ));
    }
    if program_bounds(program)
        .size()
        .iter()
        .any(|v| !v.is_finite() || *v > config.planning.max_extent_m)
    {
        return Err(err("scene world extent exceeds planning.max_extent_m"));
    }
    for region in &program.regions {
        for connection in &region.connections {
            let target = program
                .regions
                .iter()
                .find(|r| r.id == connection.target)
                .ok_or_else(|| err("connection references missing region"))?;
            if target.id == region.id
                || !connection.width_m.is_finite()
                || connection.width_m <= 0.0
                || !bounds(region.size).contains(connection.at, 0.001, 0.0)
                || !bounds(target.size).contains(connection.target_at, 0.001, 0.0)
            {
                return Err(err("invalid region connection"));
            }
            let a = transform(region.position, region.rotation).point(connection.at);
            let b = transform(target.position, target.rotation).point(connection.target_at);
            if (0..3).any(|i| {
                !a[i].is_finite()
                    || !b[i].is_finite()
                    || (a[i] - b[i]).abs() > config.quality.containment_tolerance_m.max(0.01)
            }) {
                return Err(err(format!(
                    "{} and {} connection points do not meet in world space",
                    region.id, target.id
                )));
            }
        }
    }
    Ok(())
}
fn program_bounds(program: &SceneProgram) -> Bounds {
    let mut result = Bounds {
        min: [f64::INFINITY; 3],
        max: [f64::NEG_INFINITY; 3],
    };
    for region in &program.regions {
        for point in bounds(region.size)
            .corners()
            .map(|p| transform(region.position, region.rotation).point(p))
        {
            for i in 0..3 {
                result.min[i] = result.min[i].min(point[i]);
                result.max[i] = result.max[i].max(point[i]);
            }
        }
    }
    result
}
fn node(
    id: &str,
    parent: Option<&str>,
    kind: &str,
    description: &str,
    bounds_local: Bounds,
    seed: u64,
) -> SceneNode {
    SceneNode {schema_version:1,node_id:id.into(),revision:1,kind:kind.into(),parent_id:parent.map(str::to_owned),child_ids:vec![],purpose:description.into(),required_features:vec![],assumptions:vec![],design:description.into(),construction_instructions:"Preserve the authored geometry and metric placement; never substitute an approximate recipe".into(),units:"m".into(),transform:Transform::default(),pivot_local:[0.0;3],local_front:[0.0,-1.0,0.0],local_up:[0.0,0.0,1.0],bounds_local,usable_volume_local:None,child_regions:vec![],placement_region:None,interfaces:vec![],relationships:vec![],construction:BTreeMap::new(),materials:vec![],generation:GenerationStrategy {strategy:"assembly".into(),recipe:"assembly".into(),parameters:BTreeMap::new(),seed,detail_policy:"scene-specific".into(),required_capabilities:vec![]},acceptance:Acceptance {numeric_rules:BTreeMap::new(),visual_requirements:vec![description.into()],review_views:vec!["exterior_three_quarter".into(),"overhead".into()],evidence_refs:vec![]},provenance:Provenance {owning_task:format!("design:{id}"),parent_revision:parent.map(|_|1),dependency_hashes:vec![],model:String::new(),prompt_version:VERSION.into(),recipe_version:"adaptive-geometry-v1".into(),source_url:None,creator:None,license:None,validation_status:"planned".into()} }
}
fn compile_region(
    region: &Region,
    plan: &RegionPlan,
    program: &SceneProgram,
    config: &SceneConfig,
    seed: u64,
    model: &str,
    prompt_version: &str,
) -> Result<Vec<SceneNode>> {
    crate::asset_catalog::validate_planned_sources(
        config,
        plan.objects.iter().map(|object| &object.parameters),
    )?;
    let mut objects = expand_distributions(&plan.objects, region.object_budget, seed)?;
    if objects.is_empty() || objects.len() > region.object_budget {
        return Err(err(format!(
            "region {} needs 1..{} meaningful geometry assemblies",
            region.id, region.object_budget
        )));
    }
    let region_id = format!("region-{}", region.id);
    let mut parent = node(
        &region_id,
        Some("scene"),
        "scene_region",
        &region.purpose,
        bounds(region.size),
        seed,
    );
    parent.transform = transform(region.position, region.rotation);
    parent.design = plan.description.clone();
    parent.required_features = region.required_features.clone();
    parent.generation.detail_policy = region.detail_policy.clone();
    parent
        .generation
        .parameters
        .insert("review_cameras".into(), json!(region.review_cameras));
    parent.relationships = region
        .connections
        .iter()
        .map(|c| Relationship {
            relation: "connects_to".into(),
            target: format!("region-{}", c.target),
            parameters: BTreeMap::from([
                ("at".into(), json!(c.at)),
                ("target_at".into(), json!(c.target_at)),
                ("width_m".into(), json!(c.width_m)),
            ]),
        })
        .collect();
    let mut children = Vec::new();
    let mut ids = HashSet::new();
    let mut errors = region
        .required_features
        .iter()
        .filter(|feature| {
            !objects
                .iter()
                .any(|o| o.required_features.contains(feature))
        })
        .map(|feature| {
            format!(
                "region {} omitted exact required feature reference: {feature}",
                region.id
            )
        })
        .collect::<Vec<_>>();
    for object in &mut objects {
        match canonical_geometry(&object.recipe, &mut object.parameters, object.size) {
            Ok(Some((size, offset))) => {
                object.position = transform(object.position, object.rotation).point(offset);
                object.size = size;
            }
            Ok(None) => (),
            Err(error) => {
                errors.push(format!("{}: {error}", object.id));
                continue;
            }
        }
        if let Err(error) = validate_authored_geometry(object) {
            errors.push(error.to_string());
        }
    }
    if !errors.is_empty() {
        return Err(err(errors.join("\n")));
    }
    let objects = resolve_supports(&objects)?;
    for object in &objects {
        if !valid_id(&object.id) || !ids.insert(&object.id) {
            return Err(err(
                "object IDs must be unique safe identifiers within their region",
            ));
        }
        if !GEOMETRY.contains(&object.recipe.as_str())
            && !(program.visual_style == "voxel" && VOXEL.contains(&object.recipe.as_str()))
        {
            return Err(err(format!(
                "unsupported adaptive recipe {} for {}",
                object.recipe, object.name
            )));
        }
        if object
            .size
            .iter()
            .any(|v| !v.is_finite() || *v <= 0.0 || *v > config.planning.max_extent_m)
            || object
                .position
                .iter()
                .chain(object.rotation.iter())
                .any(|v| !v.is_finite())
            || object
                .color
                .iter()
                .chain([&object.roughness, &object.metallic])
                .any(|v| !(0.0..=1.0).contains(v))
        {
            return Err(err(format!(
                "invalid object dimensions/material: {}",
                object.name
            )));
        }
        if object.parameters.keys().any(|k| {
            k.ends_with("_path")
                || k == "component_glb_path"
                || k == "force_regenerate"
                || k == "support_node_id"
        }) {
            return Err(err("model-authored filesystem paths and regeneration flags are forbidden; use catalog asset_id"));
        }
        if object.recipe == "asset"
            && object
                .parameters
                .get("asset_id")
                .and_then(Value::as_str)
                .is_none_or(str::is_empty)
        {
            return Err(err(format!("{} needs a catalog asset_id", object.name)));
        }
        validate_authored_geometry(object)?;
        let id = format!("{region_id}-{}", object.id);
        let mut child = node(
            &id,
            Some(&region_id),
            &object.semantic_class,
            &object.name,
            bounds(object.size),
            seed,
        );
        child.transform = transform(object.position, object.rotation);
        child.required_features = object.required_features.clone();
        child.generation.strategy = "recipe".into();
        child.generation.recipe = object.recipe.clone();
        child.generation.parameters = object.parameters.clone();
        child.generation.detail_policy = region.detail_policy.clone();
        if let Some(support) = &object.support {
            child.generation.parameters.insert(
                "support_node_id".into(),
                json!(format!("{region_id}-{support}")),
            );
        }
        child.materials = vec![MaterialRef {
            material_id: format!("{id}-material"),
            description: object.name.clone(),
            base_color: [object.color[0], object.color[1], object.color[2], 1.0],
            metallic: object.metallic,
            roughness: object.roughness,
            texture_scale_m: 1.0,
            texture_ref: None,
        }];
        if !child.bounds_local.corners().iter().all(|p| {
            parent.bounds_local.contains(
                child.transform.point(*p),
                config.quality.containment_tolerance_m,
                config.quality.relative_tolerance,
            )
        }) {
            let corners = child
                .bounds_local
                .corners()
                .map(|p| child.transform.point(p));
            let measured = Bounds {
                min: std::array::from_fn(|i| {
                    corners.iter().map(|p| p[i]).fold(f64::INFINITY, f64::min)
                }),
                max: std::array::from_fn(|i| {
                    corners
                        .iter()
                        .map(|p| p[i])
                        .fold(f64::NEG_INFINITY, f64::max)
                }),
            };
            errors.push(format!("{} exceeds region {}: rotated bounds {:?}, allowed {:?}. position is BOTTOM centre, not geometric centre. Rotation is [X tilt,Y tilt,Z yaw] in degrees; [0,0,90] turns an upright object sideways in plan. Correct placement or allocation without shrinking real assets arbitrarily.",object.name,region.name,measured,parent.bounds_local));
        }
        children.push(child);
    }
    if !errors.is_empty() {
        return Err(err(errors.join("\n")));
    }
    parent.child_ids = children.iter().map(|n| n.node_id.clone()).collect();
    let mut nodes = vec![parent];
    nodes.extend(children);
    for n in &mut nodes {
        n.provenance.model = model.into();
        n.provenance.prompt_version = prompt_version.into();
    }
    Ok(nodes)
}

fn expand_distributions(objects: &[Object], budget: usize, seed: u64) -> Result<Vec<Object>> {
    let count = objects
        .iter()
        .try_fold(0usize, |n, o| {
            n.checked_add(o.distribution.as_ref().map_or(1, |d| d.count))
        })
        .ok_or_else(|| err("distribution object count overflow"))?;
    if count > budget {
        return Err(err(format!("expanded source distributions require {count} objects; region object budget is {budget}")));
    }
    let mut expanded = Vec::with_capacity(count);
    let mut spacing = crate::distribution::SpacingIndex::new(
        objects
            .iter()
            .filter_map(|o| o.distribution.as_ref())
            .map(|d| d.min_spacing_m)
            .fold(0.001_f64, f64::max),
    )?;
    for object in objects {
        let Some(distribution) = &object.distribution else {
            expanded.push(object.clone());
            continue;
        };
        if object.recipe != "asset"
            || !valid_id(&object.id)
            || object.id.len() > 54
            || object.rotation[0] != 0.0
            || object.rotation[1] != 0.0
        {
            return Err(err("distribution requires an upright source asset prototype with safe ID of at most 54 characters"));
        }
        for (i, placement) in crate::distribution::placements_with_spacing(
            distribution,
            object.size,
            seed,
            &mut spacing,
        )?
        .iter()
        .enumerate()
        {
            let mut instance = object.clone();
            instance.id = format!("{}-i{i:06}", object.id);
            instance.name = format!("{} instance {}", object.name, i + 1);
            instance.distribution = None;
            instance.position = [placement.xy[0], placement.xy[1], object.position[2]];
            instance.rotation[2] += placement.yaw;
            instance.size = instance.size.map(|v| v * placement.scale);
            expanded.push(instance);
        }
    }
    Ok(expanded)
}

// Resolve explicit contact intent. No ground plane or building is invented.
// Heightfield interpolation matches the backend's two triangles per grid cell.
// Normalize only the representation of authored geometry. Every world-space
// vertex stays fixed; parent containment remains the hard design constraint.
fn canonical_geometry(
    recipe: &str,
    p: &mut BTreeMap<String, Value>,
    size: [f64; 3],
) -> Result<Option<([f64; 3], [f64; 3])>> {
    if recipe == "sweep" {
        // Older contracts labeled the arc example `path_arc_alternative`.
        // Normalize that unambiguous representation without another paid call.
        if let Some(arc) = p.remove("path_arc_alternative") {
            if p.get("path_arc").is_some_and(|existing| existing != &arc) {
                return Err(err("conflicting arc definitions"));
            }
            p.insert("path_arc".into(), arc);
        }
        if p.get("path")
            .and_then(Value::as_array)
            .is_some_and(Vec::is_empty)
        {
            p.remove("path");
        }
        if let Some(arc) = p.remove("path_arc") {
            if p.get("path").is_some_and(|v| !v.is_null()) {
                return Err(err("choose path or path_arc, not both"));
            }
            #[derive(Deserialize)]
            #[serde(deny_unknown_fields)]
            struct ArcPath {
                radius: f64,
                start_degrees: f64,
                end_degrees: f64,
                #[serde(default)]
                center: [f64; 3],
                #[serde(default = "arc_segments")]
                segments: usize,
            }
            fn arc_segments() -> usize {
                64
            }
            let arc: ArcPath = serde_json::from_value(arc).map_err(err)?;
            let span = arc.end_degrees - arc.start_degrees;
            if !arc.radius.is_finite()
                || arc.radius <= 0.0
                || !arc.start_degrees.is_finite()
                || !span.is_finite()
                || span.abs() < 1e-6
                || span.abs() > 360.0
                || !(8..=512).contains(&arc.segments)
                || arc.center.iter().any(|v| !v.is_finite())
            {
                return Err(err(
                    "invalid sweep arc radius, angles, centre or segment budget",
                ));
            }
            let count = if p.get("closed").and_then(Value::as_bool).unwrap_or(false)
                && (span.abs() - 360.0).abs() < 1e-6
            {
                arc.segments
            } else {
                arc.segments + 1
            };
            let path = (0..count)
                .map(|i| {
                    let angle =
                        (arc.start_degrees + span * i as f64 / arc.segments as f64).to_radians();
                    [
                        arc.center[0] + arc.radius * angle.cos(),
                        arc.center[1] + arc.radius * angle.sin(),
                        arc.center[2],
                    ]
                })
                .collect::<Vec<_>>();
            p.insert("path".into(), json!(path));
        }
    }
    let vectors = |key: &str, dim: usize, min: usize| -> Result<Vec<Vec<f64>>> {
        let rows: Vec<Vec<f64>> = serde_json::from_value(
            p.get(key)
                .cloned()
                .ok_or_else(|| err(format!("{recipe} requires {key}")))?,
        )
        .map_err(err)?;
        if rows.len() < min
            || rows.len() > 250_000
            || rows
                .iter()
                .any(|r| r.len() != dim || r.iter().any(|v| !v.is_finite()))
        {
            return Err(err(format!("invalid {recipe} {key} coordinates")));
        }
        Ok(rows)
    };
    let mut points = Vec::<[f64; 3]>::new();
    match recipe {
        "mesh" => points.extend(
            vectors("vertices", 3, 3)?
                .iter()
                .map(|v| [v[0], v[1], v[2]]),
        ),
        "extrusion" => {
            let profile = vectors("profile", 2, 3)?;
            let height = p
                .get("height")
                .and_then(Value::as_f64)
                .filter(|v| v.is_finite() && *v > 0.0)
                .ok_or_else(|| err("extrusion requires positive finite height"))?;
            let base = p.get("base_z").and_then(Value::as_f64).unwrap_or(0.0);
            for v in profile {
                points.extend([[v[0], v[1], base], [v[0], v[1], base + height]]);
            }
        }
        "lathe" => {
            for v in vectors("profile", 2, 2)? {
                if v[0] < 0.0 {
                    return Err(err("lathe radius must be nonnegative"));
                }
                points.extend([[-v[0], -v[0], v[1]], [v[0], v[0], v[1]]]);
            }
        }
        "heightfield" => {
            let rows: Vec<Vec<f64>> = serde_json::from_value(
                p.get("heights")
                    .cloned()
                    .ok_or_else(|| err("heightfield requires heights"))?,
            )
            .map_err(err)?;
            let cols = rows.first().map(Vec::len).unwrap_or(0);
            if rows.len() < 2
                || cols < 2
                || rows.len() * cols > 250_000
                || rows.iter().any(|r| r.len() != cols)
            {
                return Err(err("heightfield requires a bounded rectangular grid"));
            }
            for z in rows.iter().flatten() {
                points.extend([
                    [-size[0] / 2.0, -size[1] / 2.0, *z],
                    [size[0] / 2.0, size[1] / 2.0, *z],
                ]);
            }
        }
        "sweep" => {
            let path = vectors("path", 3, 2)?
                .into_iter()
                .map(|v| [v[0], v[1], v[2]])
                .collect::<Vec<_>>();
            let profile = vectors("profile", 2, 3)?;
            if path.len() > 4096 || profile.len() > 4096 || path.len() * profile.len() > 250_000 {
                return Err(err("sweep vertex budget exceeded"));
            }
            let closed = p.get("closed").and_then(Value::as_bool).unwrap_or(false);
            if closed && path.len() < 3 {
                return Err(err("closed sweep needs at least three points"));
            }
            let sub = |a: [f64; 3], b: [f64; 3]| std::array::from_fn::<_, 3, _>(|i| a[i] - b[i]);
            let dot = |a: [f64; 3], b: [f64; 3]| (0..3).map(|i| a[i] * b[i]).sum::<f64>();
            let cross = |a: [f64; 3], b: [f64; 3]| {
                [
                    a[1] * b[2] - a[2] * b[1],
                    a[2] * b[0] - a[0] * b[2],
                    a[0] * b[1] - a[1] * b[0],
                ]
            };
            let unit = |v: [f64; 3]| -> Result<[f64; 3]> {
                let length = dot(v, v).sqrt();
                if length < 1e-8 {
                    Err(err("sweep has coincident or reversing path points"))
                } else {
                    Ok(v.map(|x| x / length))
                }
            };
            let mut previous = None;
            let mut normal = [0.0; 3];
            for (i, point) in path.iter().enumerate() {
                let tangent = unit(if closed {
                    sub(
                        path[(i + 1) % path.len()],
                        path[(i + path.len() - 1) % path.len()],
                    )
                } else if i == 0 {
                    sub(path[1], *point)
                } else if i == path.len() - 1 {
                    sub(*point, path[i - 1])
                } else {
                    sub(path[i + 1], path[i - 1])
                })?;
                normal = if let Some(prior) = previous {
                    let axis = cross(prior, tangent);
                    let cosine = dot(prior, tangent);
                    if cosine < -0.999999 {
                        return Err(err("sweep reverses direction"));
                    }
                    let first = cross(axis, normal);
                    let second = cross(axis, first);
                    let transported =
                        std::array::from_fn(|j| normal[j] + first[j] + second[j] / (1.0 + cosine));
                    unit(std::array::from_fn(|j| {
                        transported[j] - tangent[j] * dot(transported, tangent)
                    }))?
                } else {
                    unit(cross(
                        if tangent[2].abs() < 0.9 {
                            [0.0, 0.0, 1.0]
                        } else {
                            [0.0, 1.0, 0.0]
                        },
                        tangent,
                    ))?
                };
                let binormal = unit(cross(tangent, normal))?;
                for uv in &profile {
                    points.push(std::array::from_fn(|j| {
                        point[j] + normal[j] * uv[0] + binormal[j] * uv[1]
                    }));
                }
                previous = Some(tangent);
            }
        }
        _ => return Ok(None),
    }
    if points.iter().flatten().any(|v| !v.is_finite()) {
        return Err(err("authored geometry must be finite"));
    }
    let low: [f64; 3] =
        std::array::from_fn(|i| points.iter().map(|p| p[i]).fold(f64::INFINITY, f64::min));
    let high: [f64; 3] = std::array::from_fn(|i| {
        points
            .iter()
            .map(|p| p[i])
            .fold(f64::NEG_INFINITY, f64::max)
    });
    let offset = [(low[0] + high[0]) * 0.5, (low[1] + high[1]) * 0.5, low[2]];
    let dimensions = std::array::from_fn(|i| (high[i] - low[i]).max(0.001));
    let shift_vectors = |value: &mut Value, indices: &[(usize, usize)]| {
        for row in value.as_array_mut().unwrap() {
            for &(column, axis) in indices {
                row[column] = json!(row[column].as_f64().unwrap() - offset[axis]);
            }
        }
    };
    match recipe {
        "mesh" => shift_vectors(p.get_mut("vertices").unwrap(), &[(0, 0), (1, 1), (2, 2)]),
        "sweep" => shift_vectors(p.get_mut("path").unwrap(), &[(0, 0), (1, 1), (2, 2)]),
        "lathe" => shift_vectors(p.get_mut("profile").unwrap(), &[(1, 2)]),
        "extrusion" => {
            shift_vectors(p.get_mut("profile").unwrap(), &[(0, 0), (1, 1)]);
            p.insert(
                "base_z".into(),
                json!(p.get("base_z").and_then(Value::as_f64).unwrap_or(0.0) - offset[2]),
            );
        }
        "heightfield" => {
            for row in p.get_mut("heights").unwrap().as_array_mut().unwrap() {
                for height in row.as_array_mut().unwrap() {
                    *height = json!(height.as_f64().unwrap() - offset[2]);
                }
            }
        }
        _ => (),
    }
    Ok(Some((dimensions, offset)))
}

/// Upgrade/resume already-authored coordinates without changing world geometry.
pub(crate) fn canonicalize_saved_geometry(spec: &mut SceneSpec) -> Result<Vec<String>> {
    let mut changed = Vec::new();
    for node in &mut spec.nodes {
        let before = (node.bounds_local, node.generation.parameters.clone());
        if let Some((size, offset)) = canonical_geometry(
            &node.generation.recipe,
            &mut node.generation.parameters,
            node.bounds_local.size(),
        )? {
            let new_bounds = bounds(size);
            if (0..3).any(|i| {
                (new_bounds.min[i] - before.0.min[i]).abs() > 1e-6
                    || (new_bounds.max[i] - before.0.max[i]).abs() > 1e-6
                    || offset[i].abs() > 1e-6
            }) {
                node.transform.translation = node.transform.point(offset);
                node.bounds_local = new_bounds;
                node.revision += 1;
                node.generation.parameters.remove("component_glb_path");
                changed.push(node.node_id.clone());
            } else {
                node.generation.parameters = before.1;
            }
        }
    }
    Ok(changed)
}

fn resolve_supports(objects: &[Object]) -> Result<Vec<Object>> {
    let mut objects = objects.to_vec();
    let mut completed = HashSet::<String>::new();
    for _ in 0..=objects.len() {
        let mut progress = false;
        for index in 0..objects.len() {
            if completed.contains(&objects[index].id) {
                continue;
            }
            let Some(support_id) = objects[index].support.clone() else {
                completed.insert(objects[index].id.clone());
                progress = true;
                continue;
            };
            let support = objects.iter().find(|o| o.id == support_id).ok_or_else(|| {
                err(format!(
                    "{} support {support_id} does not exist",
                    objects[index].id
                ))
            })?;
            if !completed.contains(&support_id) {
                continue;
            }
            if support.rotation[0].abs() > 1e-6 || support.rotation[1].abs() > 1e-6 {
                return Err(err("support surfaces must be upright; tilted contact needs explicitly authored placement"));
            }
            let dx = objects[index].position[0] - support.position[0];
            let dy = objects[index].position[1] - support.position[1];
            let yaw = support.rotation[2].to_radians();
            let (s, c) = yaw.sin_cos();
            let x = c * dx + s * dy;
            let y = -s * dx + c * dy;
            if x.abs() > support.size[0] * 0.5 + 1e-6 || y.abs() > support.size[1] * 0.5 + 1e-6 {
                return Err(err(format!(
                    "{} lies outside its support footprint",
                    objects[index].id
                )));
            }
            validate_authored_geometry(support)?;
            let height=match support.recipe.as_str() {
                "heightfield"=>{
                    let rows=support.parameters["heights"].as_array().unwrap();let columns=rows[0].as_array().unwrap().len();
                    let gx=((x/support.size[0]+0.5)*(columns-1) as f64).clamp(0.0,(columns-1) as f64);
                    let gy=((y/support.size[1]+0.5)*(rows.len()-1) as f64).clamp(0.0,(rows.len()-1) as f64);
                    let ix=(gx.floor() as usize).min(columns-2);let iy=(gy.floor() as usize).min(rows.len()-2);
                    let tx=gx-ix as f64;let ty=gy-iy as f64;
                    let z=|xx:usize,yy:usize|rows[yy][xx].as_f64().unwrap();
                    let a=z(ix,iy);let b=z(ix+1,iy);let c=z(ix,iy+1);let d=z(ix+1,iy+1);
                    if ty<=tx {a+(b-a)*tx+(d-b)*ty} else {a+(d-c)*tx+(c-a)*ty}
                },
                "slab"|"floor"|"ceiling"=>support.size[2],
                "lathe"=>{
                    let radius=x.hypot(y);
                    let profile=support.parameters["profile"].as_array().unwrap();
                    let mut heights=Vec::new();
                    for pair in profile.windows(2) {
                        let (r0,z0)=(pair[0][0].as_f64().unwrap(),pair[0][1].as_f64().unwrap());
                        let (r1,z1)=(pair[1][0].as_f64().unwrap(),pair[1][1].as_f64().unwrap());
                        if radius>=r0.min(r1)-1e-6 && radius<=r0.max(r1)+1e-6 {
                            heights.push(if (r1-r0).abs()<1e-9 {z0.max(z1)} else {z0+(z1-z0)*(radius-r0)/(r1-r0)});
                        }
                    }
                    if support.parameters.get("cap_ends").and_then(Value::as_bool).unwrap_or(true) {
                        for end in [profile.first().unwrap(),profile.last().unwrap()] {
                            if radius<=end[0].as_f64().unwrap() {heights.push(end[1].as_f64().unwrap());}
                        }
                    }
                    heights.into_iter().reduce(f64::max).ok_or_else(||err("object lies outside the actual lathe support surface"))?
                },
                _=>return Err(err(format!("support {} must be a heightfield, slab or lathe; author explicit contact placement for other shapes",support.id))),
            };
            // Contact the lowest point of the rotated allocation rather than
            // its unrotated origin, which can bury tilted objects underground.
            let rotation = transform([0.0; 3], objects[index].rotation);
            let bottom = bounds(objects[index].size)
                .corners()
                .iter()
                .map(|p| rotation.point(*p)[2])
                .fold(f64::INFINITY, f64::min);
            objects[index].position[2] = support.position[2] + height - bottom;
            completed.insert(objects[index].id.clone());
            progress = true;
        }
        if completed.len() == objects.len() {
            return Ok(objects);
        }
        if !progress {
            return Err(err(
                "support references are cyclic or object IDs are duplicated",
            ));
        }
    }
    Err(err("could not resolve support contacts"))
}

// Catch malformed or out-of-bounds authored coordinates in the scoped repair
// loop, before spending a Blender process on every invalid component. Blender
// remains authoritative for mesh topology and evaluated modifier bounds.
fn validate_authored_geometry(object: &Object) -> Result<()> {
    let p = &object.parameters;
    let b = bounds(object.size);
    let vectors = |name: &str, dimensions: usize, minimum: usize| -> Result<Vec<Vec<f64>>> {
        let rows = p
            .get(name)
            .and_then(Value::as_array)
            .ok_or_else(|| err(format!("{}: {name} must be coordinate arrays", object.name)))?;
        if rows.len() < minimum || rows.len() > 250_000 {
            return Err(err(format!("{}: invalid {name} count", object.name)));
        }
        rows.iter()
            .map(|row| {
                let row = row
                    .as_array()
                    .ok_or_else(|| err(format!("{}: invalid {name} vector", object.name)))?;
                if row.len() != dimensions {
                    return Err(err(format!(
                        "{}: {name} requires {dimensions} coordinates per vector",
                        object.name
                    )));
                }
                row.iter()
                    .map(|v| {
                        v.as_f64()
                            .filter(|v| v.is_finite())
                            .ok_or_else(|| err("geometry coordinates must be finite numbers"))
                    })
                    .collect()
            })
            .collect()
    };
    let scalar = |name: &str, default: Option<f64>| -> Result<f64> {
        p.get(name)
            .and_then(Value::as_f64)
            .or(default)
            .filter(|v| v.is_finite())
            .ok_or_else(|| err(format!("{}: {name} must be a finite number", object.name)))
    };
    let contained = |points: &[Vec<f64>]| -> Result<()> {
        if points
            .iter()
            .any(|v| !b.contains([v[0], v[1], v[2]], 1e-5, 0.0))
        {
            return Err(err(format!("{}: authored coordinates escape object-local bounds {:?}; recenter the geometry or enlarge its allocation while keeping it inside the region",object.name,b)));
        }
        Ok(())
    };
    match object.recipe.as_str() {
        "mesh" => {
            let vertices = vectors("vertices", 3, 3)?;
            contained(&vertices)?;
            let faces = p
                .get("faces")
                .and_then(Value::as_array)
                .ok_or_else(|| err("mesh requires faces"))?;
            if faces.is_empty() || faces.len() > 500_000 {
                return Err(err("mesh face count outside 1..500000"));
            }
            for face in faces {
                let face = face
                    .as_array()
                    .ok_or_else(|| err("mesh face must be an index array"))?;
                if face.len() < 3
                    || face.len() > 4096
                    || face
                        .iter()
                        .any(|v| v.as_u64().is_none_or(|i| i as usize >= vertices.len()))
                {
                    return Err(err(format!("{}: face indices must be integers addressing at least three existing vertices",object.name)));
                }
            }
        }
        "extrusion" => {
            let profile = vectors("profile", 2, 3)?;
            let height = scalar("height", None)?;
            let base = scalar("base_z", Some(0.0))?;
            if height <= 0.0 {
                return Err(err("extrusion height must be positive"));
            }
            let points = profile
                .iter()
                .flat_map(|p| [vec![p[0], p[1], base], vec![p[0], p[1], base + height]])
                .collect::<Vec<_>>();
            contained(&points)?;
        }
        "lathe" => {
            let profile = vectors("profile", 2, 2)?;
            let segments = p.get("segments").and_then(Value::as_u64).unwrap_or(64);
            if !(8..=512).contains(&segments) || profile.len() * segments as usize > 250_000 {
                return Err(err("lathe segment/vertex budget exceeded"));
            }
            if profile.iter().any(|p| {
                p[0] < 0.0
                    || p[0] > object.size[0].min(object.size[1]) * 0.5 + 1e-5
                    || p[1] < 0.0
                    || p[1] > object.size[2] + 1e-5
            }) {
                return Err(err(format!(
                    "{}: lathe [radius,Z] profile must fit half the XY width and local height",
                    object.name
                )));
            }
        }
        "sweep" => {
            let path = vectors("path", 3, 2)?;
            let profile = vectors("profile", 2, 3)?;
            contained(&path)?;
            if path.len() * profile.len() > 250_000
                || path.windows(2).any(|pair| pair[0] == pair[1])
            {
                return Err(err(
                    "sweep path has duplicate points or exceeds vertex budget",
                ));
            }
        }
        "heightfield" => {
            let rows = p
                .get("heights")
                .and_then(Value::as_array)
                .ok_or_else(|| err("heightfield requires heights grid"))?;
            let columns = rows
                .first()
                .and_then(Value::as_array)
                .map(Vec::len)
                .unwrap_or(0);
            if !(2..=1024).contains(&rows.len())
                || !(2..=1024).contains(&columns)
                || rows.len() * columns > 250_000
                || rows.iter().any(|r| {
                    r.as_array().is_none_or(|r| {
                        r.len() != columns
                            || r.iter().any(|v| {
                                v.as_f64().is_none_or(|z| {
                                    !z.is_finite() || z < 0.0 || z > object.size[2] + 1e-5
                                })
                            })
                    })
                })
            {
                return Err(err(format!(
                    "{}: heightfield needs a rectangular grid of finite local heights within 0..{}",
                    object.name, object.size[2]
                )));
            }
        }
        _ => (),
    }
    Ok(())
}

fn program_contract(task: &SceneTask, config: &SceneConfig, catalog: &Value) -> Value {
    json!({"original_prompt":task.prompt,"seed":task.seed.unwrap_or(0),"fidelity":config.quality.fidelity,"asset_catalog":catalog,
        "required_output":{"description":"specific interpretation of the requested scene","scene_kind":"prompt-derived domain, never a universal building label","visual_style":"realistic | stylized | voxel | concept","topology":"actual spatial organization and construction logic","environment":{"purpose":"prompt-specific location, time of day and lighting intent","sky":"physical_sky","background_color":[0.1,0.1,0.1],"strength":0.3,"exposure":0,"sun":{"elevation_degrees":35,"azimuth_degrees":120,"energy":2,"color":[1,0.95,0.88],"angular_size_degrees":0.53},"lights":[]},"required_features":["concrete prompt-critical feature"],"unresolved_assets":[{"name":"required model absent from catalog","category":"semantic category","asset_type":"models or textures","reason":"why authored surfaces or available models cannot faithfully replace it"}],"regions":[{"id":"safe-unique-id","name":"specific functional or landscape region","purpose":"what belongs here and why","position":[0,0,0],"size":[10,10,5],"rotation":[0,0,0],"required_features":["copy exact concrete feature strings to owned objects later"],"detail_policy":"allocate detail by visible scale, hero objects, foreground/midground/background","object_budget":12,"review_cameras":[{"id":"local-hero","position":[0,-3,1.7],"target":[0,0,1.2],"lens_mm":35,"coverage":"required interior or object feature"}],"connections":[{"target":"other-region-id","at":[0,0,0],"target_at":[0,0,0],"width_m":1.5}]}]},
        "limits":{"max_regions":config.planning.max_regions,"max_objects_per_region":config.planning.max_objects_per_region,"total_nodes_including_root_regions_and_objects":config.budgets.max_nodes,"max_world_extent_m":config.planning.max_extent_m},
        "instructions":"Create a scene-specific spatial program before geometry. Author an environment appropriate to the actual location, time, mood and visibility, never copy a universal sun/fill rig. environment.sky is physical_sky (requires sun) or constant; constant can omit sun for interiors, night and controlled lighting. Colors are linear RGB 0..1, strength 0..10, exposure -16..16 stops. Start physical-sky daylight near strength 0.2 and exposure 0, adjusting for the actual scene. Positive exposure brightens all surfaces exponentially and can wash out material detail; do not assume golden hour requires positive exposure. Sun elevation 0..90 and azimuth -360..360 degrees from +X toward +Y, energy 0..100, angular_size_degrees .01..45. Up to64 lights use {id,purpose,kind:area|spot|point,position:[x,y,z],target:[x,y,z],color:[r,g,b],power_w,size_m,spot_angle_degrees:45}; all positions/targets are WORLD metres. Place plausible sources deliberately; do not add generic fill when incompatible with the scene. World shaders and area lights are native Blender features; the native file is authoritative for offline delivery. Full original source detail is the default; high render cost is acceptable. Allocate actual surroundings and detail, not just isolated hero props. Any scale or domain is valid: object study, sacred site, museum, wild landscape, streets, industrial plant, fictional world, interior, exterior or mixed. Regions are semantic work allocations and bounding envelopes, NOT rooms and NOT geometry. Never impose walls, roofs, room interiors, terrain platforms, bridges, furniture counts, symmetry or a fixed family of layouts. A real meadow stays outdoors; a prayer hall follows its ritual and architectural context; a museum contains its specific collection and circulation. Use only as many regions as needed. Prefer ONE region for a small coherent object study, installation or garden; splitting it into arbitrary rectangular bands creates needless seams. Decompose larger scenes by genuinely independent spatial work, not one region per feature. Allocate bounded object budgets by scene complexity, salient detail and size; root+regions+SUM(object_budget) must fit total_nodes. Bounds are meters, X/Y centred, bottom Z; region positions are world coordinates, rotations XYZ Euler degrees. A connection names two coincident local boundary points in adjacent regions and their traversable width; omit connections if unrelated. Preserve spatial negative space, true proportions, access, engineering support, and domain-specific features. Every global required feature must appear VERBATIM in at least one region.required_features list; these strings are exact references, not paraphrased descriptions. Region-local additional features may also be listed. Catalogue assets are actual available models; never invent IDs. For realistic scenes use production assets for complex organic/hero objects (trees, people, sculptures, vehicles, intricate furniture) unless explicit authored geometry can faithfully describe the object. Do not substitute a primitive assembly, a generic room, or a recolored prop. List missing essential assets in unresolved_assets rather than pretending to produce realism. Geometry specialists can author arbitrary mesh, profile extrusion, lathe, sweep and heightfield surfaces; nothing requires rectangular occupied volumes. Choose region-local review_cameras that expose required occupied interiors, hero morphology and remote scene zones; at most three per region and twelve overall. Do not rely on exterior overview cameras to reveal hidden interiors. Review cameras are lifted above actual heightfield terrain if accidentally below ground; set allow_subsurface=true only for intentional underground views. Regional specialists execute independently, so ownership, shared seams, supports and cross-region dependencies must be resolved now. Return JSON only."})
}
fn region_contract(
    task: &SceneTask,
    config: &SceneConfig,
    program: &SceneProgram,
    region: &Region,
    catalog: &Value,
) -> Value {
    json!({"original_prompt":task.prompt,"fidelity":config.quality.fidelity,"scene_program":program,"owned_region":region,"asset_catalog":catalog,
        "required_output":{"description":"detailed design of this region","objects":[{"id":"unique-safe-id","name":"actual semantic object name","semantic_class":"tree, roof, terrain, sculpture, bench, etc","recipe":"listed recipe","size":[2,2,2],"position":[0,0,0],"support":null,"distribution":null,"rotation":[0,0,0],"parameters":{},"color":[0.4,0.4,0.4],"roughness":0.5,"metallic":0.0,"required_features":["exact owned_region.required_features string that this geometry realizes"]}],"unresolved_assets":[]},
        "geometry_contract":{"asset":{"asset_id":"exact available catalog ID"},"distribution":{"count":40,"polygon":[[-10,-10],[10,-10],[10,10],[-10,10]],"exclusions":[[[-1,-10],[1,-10],[1,10],[-1,10]]],"min_spacing_m":1.5,"seed":1,"yaw_degrees":[0,360],"uniform_scale":[1,1],"note":"Put distribution on an Object whose recipe is asset, alongside its existing size, support and parameters.asset_id; NOT inside parameters. This example is optional, no domain or rectangular shape is mandatory. Polygon and exclusions are region-local XY. Prototype size is ONE actual source instance; prototype Z is retained or resolved by support. Instance X/Y comes from the polygon. Original full-detail meshes are linked, never replaced with proxy shapes. Source footprint stays inside polygon and outside exclusions; min_spacing_m is center spacing shared across ALL distributed source prototypes in this region; each pair uses the larger requested spacing. This is not a general collision proof. Every expanded instance counts toward owned_region.object_budget and total_nodes. Unique instances get IDs prototype-i000000 etc. Distribution is suitable only for repeated exact source types; use several actual variant prototypes for morphological variation, not duplicate-only forests."},"mesh":{"vertices":[[0,0,0],[1,0,0],[0,1,0]],"faces":[[0,1,2]]},"extrusion":{"profile":[[-1,-1],[1,-1],[0,1]],"height":2.0,"base_z":0},"lathe":{"profile":[[0,0],[1,0],[0.7,2],[0,2]],"segments":48},"sweep":{"profile":[[-0.1,-0.1],[0.1,-0.1],[0.1,0.1],[-0.1,0.1]],"closed":false,"path_arc":{"radius":2,"start_degrees":20,"end_degrees":160,"center":[0,0,0],"segments":64},"note":"This is a complete analytic arc parameter example. For noncircular curves, remove path_arc and instead supply path as an array of local [x,y,z] points; never supply both. An analytic path_arc is preferred for smooth circular screens, rails and architectural arcs. profile [u,v] is perpendicular to the path; for a horizontal path v is vertical Z. Keep path Z plus profile v within 0..size[2]; use profile v from 0 to wall height for a wall at ground level. Include profile thickness in XY bounds."},"heightfield":{"heights":[[0,0],[0,0]],"note":"XY spans the declared object bounds; heights are object-local metre Z coordinates"},"structural_recipes":["wall","slab","stairs","window","door","light"],"voxel_recipes_only_for_voxel_style":VOXEL},
        "instructions":"Design ONLY this region in its LOCAL metre coordinates. Each object's size is its tight local geometry bounds, centred X/Y, bottom Z=0; position is local bottom-centre and rotation is [X tilt,Y tilt,Z yaw] Euler DEGREES. An upright tree rotated around the vertical axis uses [0,0,yaw], NEVER [0,yaw,0]. Explicit mesh/profile/path/heights are object-LOCAL meters and must fit its declared bounds before rotation. All rotated object bounds must fit the region. Region envelopes are invisible allocations: do not model a bounding box around them. No automatic room, shell, furniture minimum, central cross, or landscape platform. Place and shape every object for the actual scene function. Use assemblies with accurate silhouettes and construction, coherent materials, grounded contact, and believable scale. Author curves, irregular footprints, landscape relief and unique architecture using explicit geometry. Structural recipes only make their named structural part, never rename a slab into a tree or sculpture. Realistic complex hero/organic items need catalog production models; missing ones go into unresolved_assets. Preserve existing imported asset materials. For textured authored geometry use parameters.material_asset_id referencing a suitable catalog surface material, optional material_name when the source contains multiple materials, and positive texture_scale_m for metric UV tiling. For catalogue entries with surface_uses, set parameters.material_usage to the EXACT appropriate allowed value; do not mislabel a wall as terrain to evade the restriction. Entries with kind=model cannot be surface swatches and kind=material cannot be object models. Do not emit asset_lod; preserve original mesh detail. Choose the actual subject-specific surface; plain uniform colors do not reproduce soil, stone or detailed wood. Missing essential surface materials go in unresolved_assets. Do not emit file paths, scripts, external URLs or hidden placeholders. Cover every required regional feature by copying its exact string into one or more object required_features. This is an ownership trace, not proof of appearance: actual geometry and rendered views must match it. For objects resting on a heightfield, slab or lathe surface, set support to that exact object id in this region. The controller samples the actual triangular heightfield at the object XY and resolves an initial bottom Z, then Blender refines vertical contact against actual mesh vertices; do not guess terrain height. support is optional and creates no implicit terrain. It does not attach side-mounted or hanging objects; author those placements explicitly. Resolve all contacts and circulation numerically; respect the accepted neighboring regions and shared connection points. The object budget is an upper limit, not a target or minimum. Return complete RegionPlan JSON only."})
}
fn signature(spec: &SceneSpec, program: &SceneProgram) -> Value {
    let mut recipes = BTreeMap::<String, usize>::new();
    for n in spec
        .nodes
        .iter()
        .filter(|n| n.generation.strategy != "assembly")
    {
        *recipes.entry(n.generation.recipe.clone()).or_default() += 1;
    }
    json!({"version":1,"scene_kind":program.scene_kind,"topology":program.topology,"regions":program.regions.len(),"objects":spec.nodes.len()-program.regions.len()-1,"extent_m":program_bounds(program).size(),"recipe_histogram":recipes,"shape_fingerprint":hash_bytes(&serde_json::to_vec(&spec.nodes.iter().map(|n|json!({"recipe":n.generation.recipe,"bounds":n.bounds_local,"transform":n.transform,"parameters":n.generation.parameters})).collect::<Vec<_>>()).unwrap_or_default())})
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn presentation_repair_preserves_geometry_and_rejects_contract_changes() {
        let mut spec: SceneSpec =
            serde_json::from_str(include_str!("../examples/complete-scene.json")).unwrap();
        let mut region = spec.nodes[0].clone();
        region.node_id = "region-study".into();
        region.parent_id = Some(spec.root_id.clone());
        region.child_ids.clear();
        spec.nodes[0].child_ids.push(region.node_id.clone());
        spec.nodes.push(region);
        let node = spec.nodes.last_mut().unwrap();
        node.kind = "scene_region".into();
        let id = node.node_id.clone();
        let geometry =
            serde_json::to_value((&node.transform, &node.bounds_local, &node.child_ids)).unwrap();
        let parameters=serde_json::from_value(json!({"review_cameras":[{"id":"detail","position":[0,-3,2],"target":[0,0,1],"lens_mm":50,"coverage":"joinery"}]})).unwrap();
        repair_presentation(&mut spec, &id, &parameters, &SceneConfig::default()).unwrap();
        let node = spec.nodes.last().unwrap();
        assert_eq!(
            geometry,
            serde_json::to_value((&node.transform, &node.bounds_local, &node.child_ids)).unwrap()
        );
        let before = serde_json::to_value(&spec).unwrap();
        let forbidden =
            serde_json::from_value(json!({"transform":{"translation":[4,0,0]}})).unwrap();
        assert!(repair_presentation(&mut spec, &id, &forbidden, &SceneConfig::default()).is_err());
        assert_eq!(before, serde_json::to_value(&spec).unwrap());
        let mut invalid = parameters;
        invalid.get_mut("review_cameras").unwrap()[0]["lens_mm"] = json!(0);
        assert!(repair_presentation(&mut spec, &id, &invalid, &SceneConfig::default()).is_err());
        assert_eq!(before, serde_json::to_value(&spec).unwrap());
        spec.nodes[0].generation.parameters.insert("environment".into(),json!({
            "purpose":"afternoon","sky":"physical_sky","background_color":[0.1,0.1,0.1],"strength":0.3,"exposure":2,
            "sun":{"elevation_degrees":35,"azimuth_degrees":120,"energy":2,"color":[1,1,1],"angular_size_degrees":0.53},"lights":[]}));
        spec.nodes.last_mut().unwrap().generation.parameters.get_mut("review_cameras").unwrap().as_array_mut().unwrap()
            .push(json!({"id":"overview","position":[10,-10,8],"target":[0,0,1],"lens_mm":35,"coverage":"whole scene"}));
        let root = spec.root_id.clone();
        let update=serde_json::from_value(json!({"environment":{"exposure":0,"sun":{"energy":1}},"review_cameras":[{"id":"detail","position":[0,3,2],"target":[0,0,1],"lens_mm":50}]})).unwrap();
        let changed =
            repair_presentation(&mut spec, &root, &update, &SceneConfig::default()).unwrap();
        assert_eq!(changed.len(), 2);
        assert_eq!(
            spec.nodes.last().unwrap().generation.parameters["review_cameras"]
                .as_array()
                .unwrap()
                .len(),
            2
        );
        assert_eq!(
            spec.nodes.last().unwrap().generation.parameters["review_cameras"][0]["coverage"],
            "joinery"
        );
        assert!(
            repair_presentation(&mut spec, &root, &update, &SceneConfig::default())
                .unwrap()
                .is_empty()
        );
        assert_eq!(
            spec.nodes[0].generation.parameters["environment"]["sun"]["elevation_degrees"],
            json!(35)
        );
        assert_eq!(
            spec.nodes[0].generation.parameters["environment"]["sun"]["energy"],
            json!(1)
        );
        let before = serde_json::to_value(&spec).unwrap();
        let invalid = serde_json::from_value(json!({"environment":{"sun":{"energy":-5}}})).unwrap();
        assert!(repair_presentation(&mut spec, &root, &invalid, &SceneConfig::default()).is_err());
        assert_eq!(before, serde_json::to_value(&spec).unwrap());
    }
    fn object(value: Value) -> Object {
        serde_json::from_value(value).unwrap()
    }
    fn ground() -> Object {
        object(
            json!({"id":"ground","name":"terrain","semantic_class":"terrain","recipe":"heightfield","size":[2,2,4],"position":[10,20,3],"parameters":{"heights":[[0,2],[1,4]]},"required_features":[]}),
        )
    }
    fn supported() -> Object {
        object(
            json!({"id":"tree","name":"tree","semantic_class":"tree","recipe":"asset","size":[1,1,2],"position":[10.5,19.5,999],"support":"ground","parameters":{"asset_id":"catalog:tree"},"required_features":[]}),
        )
    }
    #[test]
    fn contact_matches_heightfield_triangles_and_yaw() {
        let tree = supported();
        let terrain = ground();
        // (tx,ty)=(.75,.25): a+(b-a)*tx+(d-b)*ty = 2.
        let result = resolve_supports(&[tree.clone(), terrain.clone()]).unwrap();
        assert!((result[0].position[2] - 5.0).abs() < 1e-9);
        let mut rotated = terrain;
        rotated.rotation[2] = 90.0;
        let mut tree = tree;
        tree.position = [10.5, 20.5, 999.0];
        assert!((resolve_supports(&[tree, rotated]).unwrap()[0].position[2] - 5.0).abs() < 1e-9);
    }
    #[test]
    fn contacts_reject_missing_cyclic_and_outside_supports() {
        assert!(resolve_supports(&[supported()]).is_err());
        let mut terrain = ground();
        terrain.support = Some("tree".into());
        assert!(resolve_supports(&[supported(), terrain]).is_err());
        let mut tree = supported();
        tree.position[0] = 50.0;
        assert!(resolve_supports(&[tree, ground()]).is_err());
    }
    #[test]
    fn tilted_contact_accounts_for_rotated_bottom() {
        let mut tree = supported();
        tree.rotation[0] = 90.0;
        let resolved = resolve_supports(&[tree, ground()]).unwrap();
        let t = transform(resolved[0].position, resolved[0].rotation);
        let bottom = bounds(resolved[0].size)
            .corners()
            .iter()
            .map(|p| t.point(*p)[2])
            .fold(f64::INFINITY, f64::min);
        assert!((bottom - 5.0).abs() < 1e-9);
    }
    #[test]
    fn lathe_support_uses_profile_surface_instead_of_envelope_top() {
        let mut terrain = ground();
        terrain.recipe = "lathe".into();
        terrain.parameters =
            serde_json::from_value(json!({"profile":[[0,0],[1,0],[1,1],[0.8,1],[0.8,2],[0,2]]}))
                .unwrap();
        let tree = supported();
        assert!(
            (resolve_supports(&[tree, terrain.clone()]).unwrap()[0].position[2] - 5.0).abs() < 1e-9
        );
        let mut tree = supported();
        tree.position = [10.9, 20.0, 999.0];
        assert!((resolve_supports(&[tree, terrain]).unwrap()[0].position[2] - 4.0).abs() < 1e-9);
    }
    #[test]
    fn malformed_geometry_is_rejected_before_blender() {
        let mut terrain = ground();
        validate_authored_geometry(&terrain).unwrap();
        terrain
            .parameters
            .insert("heights".into(), json!([[0, 1], [5, 2]]));
        assert!(validate_authored_geometry(&terrain).is_err());
        terrain.recipe = "mesh".into();
        terrain.parameters = serde_json::from_value(
            json!({"vertices":[[0,0,0],[1,0,0],[0,1,0]],"faces":[[0,1,99]]}),
        )
        .unwrap();
        assert!(validate_authored_geometry(&terrain).is_err());
        terrain
            .parameters
            .insert("faces".into(), json!([[0, 1, 2]]));
        validate_authored_geometry(&terrain).unwrap();
        assert_eq!(supported().color, [0.5; 3]);
    }
    #[test]
    fn canonical_coordinates_preserve_world_vertices_and_are_idempotent() {
        let mut parameters:BTreeMap<String,Value>=serde_json::from_value(json!({"vertices":[[-4,-3.5,1],[4,-3.5,1],[4,-1,1],[-4,-1,1]],"faces":[[0,1,2],[0,2,3]]})).unwrap();
        let original = parameters["vertices"].as_array().unwrap().clone();
        let old_transform = transform([2.0, 3.0, 4.0], [0.0, 0.0, 37.0]);
        let (size, offset) = canonical_geometry("mesh", &mut parameters, [8.0, 2.5, 0.01])
            .unwrap()
            .unwrap();
        assert_eq!(size, [8.0, 2.5, 0.001]);
        let new_transform = Transform {
            translation: old_transform.point(offset),
            ..old_transform
        };
        for (old, new) in original
            .iter()
            .zip(parameters["vertices"].as_array().unwrap())
        {
            let a = old_transform.point(serde_json::from_value(old.clone()).unwrap());
            let b = new_transform.point(serde_json::from_value(new.clone()).unwrap());
            assert!((0..3).all(|i| (a[i] - b[i]).abs() < 1e-9));
        }
        let (_, offset) = canonical_geometry("mesh", &mut parameters, size)
            .unwrap()
            .unwrap();
        assert!(offset.iter().all(|v| v.abs() < 1e-9));
    }
    #[test]
    fn analytic_sweep_arcs_expand_without_model_authored_vertex_lists() {
        let mut legacy = serde_json::from_value(json!({"path":[],"path_arc_alternative":{"radius":2,"start_degrees":20,"end_degrees":160,"segments":64},"profile":[[-0.05,0],[0.05,0],[0.05,2],[-0.05,2]]})).unwrap();
        canonical_geometry("sweep", &mut legacy, [4.0, 2.0, 2.0]).unwrap();
        assert_eq!(legacy["path"].as_array().unwrap().len(), 65);
        assert!(!legacy.contains_key("path_arc_alternative"));
        let mut p=serde_json::from_value(json!({"path_arc":{"radius":2,"start_degrees":20,"end_degrees":160,"segments":64},"profile":[[-0.05,0],[0.05,0],[0.05,2],[-0.05,2]]})).unwrap();
        let (_, offset) = canonical_geometry("sweep", &mut p, [4.0, 2.0, 2.0])
            .unwrap()
            .unwrap();
        assert!(!p.contains_key("path_arc"));
        let path = p["path"].as_array().unwrap();
        assert_eq!(path.len(), 65);
        for point in path {
            let x = point[0].as_f64().unwrap() + offset[0];
            let y = point[1].as_f64().unwrap() + offset[1];
            assert!((x.hypot(y) - 2.0).abs() < 1e-9);
        }
        p.insert(
            "path_arc".into(),
            json!({"radius":2,"start_degrees":20,"end_degrees":160}),
        );
        assert!(canonical_geometry("sweep", &mut p, [4.0, 2.0, 2.0]).is_err());
    }
    #[test]
    fn sweep_canonical_bounds_include_cross_section_not_only_path() {
        let mut p=serde_json::from_value(json!({"path":[[-2,1,0],[0,2,0],[2,1,0]],"profile":[[-0.05,0],[0.05,0],[0.05,2.1],[-0.05,2.1]]})).unwrap();
        let (size, offset) = canonical_geometry("sweep", &mut p, [4.0, 1.0, 2.1])
            .unwrap()
            .unwrap();
        assert!(size[0] > 4.04 && size[1] > 1.09);
        assert!((size[2] - 2.1).abs() < 1e-9);
        assert!(offset[1] > 1.5 && offset[1] < 1.51);
        let (again, offset) = canonical_geometry("sweep", &mut p, size).unwrap().unwrap();
        assert!((0..3).all(|i| (again[i] - size[i]).abs() < 1e-9 && offset[i].abs() < 1e-9));
    }
    #[test]
    fn regional_patch_preserves_unaffected_objects_and_rejects_conflicts() {
        let terrain = ground();
        let tree = supported();
        let previous = RegionPlan {
            description: "preserve".into(),
            objects: vec![terrain.clone(), tree.clone()],
            unresolved_assets: vec![],
        };
        let mut changed = tree;
        changed.position[0] = 10.7;
        let patch = RegionPatch {
            changed_objects: vec![changed.clone()],
            removed_object_ids: vec![],
            unresolved_assets: vec![],
        };
        let result = RegionRevision::Patch(patch.clone())
            .apply(Some(previous.clone()))
            .unwrap();
        assert_eq!(
            serde_json::to_value(&result.objects[0]).unwrap(),
            serde_json::to_value(terrain).unwrap()
        );
        assert_eq!(result.objects[1].position[0], 10.7);
        assert!(RegionRevision::Patch(patch).apply(None).is_err());
        let conflict = RegionPatch {
            changed_objects: vec![changed],
            removed_object_ids: vec!["tree".into()],
            unresolved_assets: vec![],
        };
        assert!(RegionRevision::Patch(conflict)
            .apply(Some(previous))
            .is_err());
    }
    #[test]
    fn recipe_shorthand_is_normalized_but_conflicts_and_unknowns_fail() {
        let mut value = json!({"objects":[{"id":"ground","name":"ground","semantic_class":"terrain","recipe":"heightfield","size":[2,2,1],"position":[0,0,0],"heights":[[0,1],[1,0]],"required_features":[]}]});
        let parsed: AdaptiveResponse<RegionPlan> = serde_json::from_value(value.clone()).unwrap();
        assert_eq!(
            parsed.0.objects[0].parameters["heights"],
            value["objects"][0]["heights"]
        );
        value["objects"][0]["parameters"] = json!({"heights":[[0,0],[0,0]]});
        assert!(serde_json::from_value::<AdaptiveResponse<RegionPlan>>(value.clone()).is_err());
        value["objects"][0]["parameters"] = json!({});
        value["objects"][0]["secret_path"] = json!("untrusted");
        assert!(serde_json::from_value::<AdaptiveResponse<RegionPlan>>(value).is_err());
        let nested = json!({"objects":[{"id":"ground","name":"ground","semantic_class":"terrain","recipe":"heightfield","size":[2,2,1],"position":[0,0,0],"geometry":{"heights":[[0,1],[1,0]]},"required_features":[]}]});
        let parsed: AdaptiveResponse<RegionRevision> =
            serde_json::from_value(nested.clone()).unwrap();
        let plan = parsed.0.apply(None).unwrap();
        assert!(plan.objects[0].parameters.contains_key("heights"));
        let mut named = nested.clone();
        named["objects"][0]["heightfield"] = named["objects"][0]
            .as_object_mut()
            .unwrap()
            .remove("geometry")
            .unwrap();
        let named_response: AdaptiveResponse<RegionPlan> =
            serde_json::from_value(named.clone()).unwrap();
        assert_eq!(
            named_response.0.objects[0].parameters["heights"],
            json!([[0, 1], [1, 0]])
        );
        named["objects"][0]["parameters"] = json!({"heights":[[0,0],[0,0]]});
        assert!(serde_json::from_value::<AdaptiveResponse<RegionPlan>>(named).is_err());
        let mut source = serde_json::to_value(supported()).unwrap();
        source["recipe"] = json!({"asset_id":"catalog:tree"});
        let normalized: AdaptiveResponse<RegionPlan> =
            serde_json::from_value(json!({"objects":[source.clone()]})).unwrap();
        assert_eq!(normalized.0.objects[0].recipe, "asset");
        source["parameters"]["asset_id"] = json!("other:tree");
        assert!(serde_json::from_value::<AdaptiveResponse<RegionPlan>>(
            json!({"objects":[source]})
        )
        .is_err());
        let mut bad = nested;
        bad["objects"][0]["geometry"]["script_path"] = json!("untrusted");
        assert!(
            serde_json::from_value::<AdaptiveResponse<RegionRevision>>(bad)
                .err()
                .unwrap()
                .to_string()
                .contains("unsupported geometry parameter")
        );
    }
    fn program() -> SceneProgram {
        serde_json::from_value(json!({"description":"open ridge with meandering trail","scene_kind":"landscape","visual_style":"concept","topology":"open asymmetric ridge","required_features":["ridge"],"regions":[{"id":"ridge","name":"Ridge","purpose":"natural terrain","position":[1000,0,-10],"size":[300,220,20],"required_features":["ridge"],"detail_policy":"terrain silhouette","object_budget":4}]})).unwrap()
    }
    #[test]
    fn landscape_can_exceed_legacy_scale_without_rooms() {
        let config = SceneConfig::default();
        let p = program();
        validate_program(&p, &config).unwrap();
        let plan:RegionPlan=serde_json::from_value(json!({"description":"open terrain","objects":[{"id":"terrain","name":"ridge","semantic_class":"terrain","recipe":"heightfield","size":[300,220,20],"position":[0,0,0],"parameters":{"heights":[[0,8],[20,4]]},"color":[0.3,0.4,0.2],"roughness":0.9,"required_features":["ridge"]}]})).unwrap();
        let nodes = compile_region(&p.regions[0], &plan, &p, &config, 0, "test", "v1").unwrap();
        assert_eq!(nodes.len(), 2);
        assert_eq!(nodes[1].generation.recipe, "heightfield");
        assert!(!nodes.iter().any(|n| n.kind == "furnished_room"));
    }
    #[test]
    fn source_distributions_expand_into_real_owned_nodes_with_budgeted_support() {
        let mut tree = supported();
        tree.id = "saplings".into();
        tree.position = [0.0, 0.0, 1.0];
        tree.size = [0.5, 0.5, 1.0];
        tree.distribution=Some(serde_json::from_value(json!({"count":20,"polygon":[[-8,-8],[8,-8],[8,8],[-8,8]],"min_spacing_m":1,"seed":42})).unwrap());
        let expanded = expand_distributions(&[tree.clone()], 20, 3).unwrap();
        assert_eq!(expanded.len(), 20);
        for (i, item) in expanded.iter().enumerate() {
            assert_eq!(item.id, format!("saplings-i{i:06}"));
            assert_eq!(item.recipe, "asset");
            assert_eq!(item.parameters["asset_id"], "catalog:tree");
            assert_eq!(item.size, [0.5, 0.5, 1.0]);
            assert_eq!(item.support, tree.support);
            assert!(item.distribution.is_none());
        }
        assert!(expand_distributions(&[tree.clone()], 19, 3).is_err());
        let mut other_variant = tree.clone();
        other_variant.id = "other-variant".into();
        other_variant.distribution.as_mut().unwrap().min_spacing_m = 2.0;
        // Identical seeds used to place different source variants on top of
        // each other. Enforce the larger pairwise spacing across prototypes.
        let mixed = expand_distributions(&[tree.clone(), other_variant], 40, 3).unwrap();
        for a in &mixed[..20] {
            for b in &mixed[20..] {
                assert!(
                    (a.position[0] - b.position[0]).hypot(a.position[1] - b.position[1]) >= 2.0
                );
            }
        }
        tree.recipe = "slab".into();
        assert!(expand_distributions(&[tree], 20, 3).is_err());
    }
    #[test]
    fn region_budgets_identifiers_and_seams_are_enforced() {
        let config = SceneConfig::default();
        let mut p = program();
        p.regions[0].id = "../escape".into();
        assert!(validate_program(&p, &config).is_err());
        let mut p = program();
        p.regions[0].object_budget = config.budgets.max_nodes;
        assert!(validate_program(&p, &config).is_err());
        let mut p = program();
        let mut second = p.regions[0].clone();
        second.id = "other".into();
        second.position[0] += 500.0;
        p.regions.push(second);
        p.regions[0].connections.push(Connection {
            target: "other".into(),
            at: [0.0; 3],
            target_at: [0.0; 3],
            width_m: 1.0,
        });
        assert!(validate_program(&p, &config).is_err());
    }
    #[test]
    fn missing_assets_are_actionable_and_never_replaced() {
        let error = ensure_assets_available(&[MissingAsset {
            name: "mature oak".into(),
            category: "tree".into(),
            reason: "botanical fidelity".into(),
            asset_type: "models".into(),
        }])
        .unwrap_err();
        assert!(matches!(error, SceneError::WaitingExternal(_)));
        assert!(error.to_string().contains("mature oak"));
    }
    #[tokio::test]
    async fn program_specialists_compile_and_resume_without_new_model_calls() {
        use loop_ai::providers::{faux_provider, FauxResponse, FauxScript};
        use std::sync::Arc;
        let script = FauxScript::new();
        let provider = faux_provider(script.clone());
        let models = Arc::new(loop_ai::Models::new());
        models.set_provider(provider);
        let mut config = SceneConfig::default();
        config.models.provider = "faux".into();
        config.models.default_model = "faux-model".into();
        config.execution.max_agent_tasks = 2;
        let mut p = program();
        let mut second = p.regions[0].clone();
        second.id = "second-ridge".into();
        second.position[0] += 320.0;
        p.regions.push(second);
        let plan = json!({"description":"natural ridge terrain","objects":[{"id":"terrain","name":"ridge","semantic_class":"terrain","recipe":"heightfield","size":[300,220,20],"position":[0,0,0],"parameters":{"heights":[[0,8],[20,4]]},"color":[0.3,0.4,0.2],"roughness":0.9,"required_features":["ridge"]}]});
        for output in [
            json!(p),
            json!({"approved":true,"issues":[]}),
            plan.clone(),
            plan.clone(),
        ] {
            script.push(FauxResponse::Text(output.to_string()));
        }
        let dir = tempfile::tempdir().unwrap();
        let store = RunStore::open(dir.path()).unwrap();
        let task:SceneTask=serde_json::from_value(json!({"id":"landscape","prompt":"Two irregular open landscape ridges, no rooms or buildings."})).unwrap();
        store
            .initialize("test", &task.id, &task.prompt, &json!(config), &json!(task))
            .unwrap();
        let reporter = crate::reporting::Reporter::new(dir.path());
        let run = reporter.begin("run", "test", json!({})).unwrap();
        let invoker = RoleInvoker::new(models, config.models.clone())
            .with_reporter(reporter.clone())
            .with_budget(config.budgets.clone());
        let cancel = CancellationToken::new();
        let first = design_scene(&task, &config, dir.path(), &invoker, &store, &cancel)
            .await
            .unwrap();
        assert_eq!(first.nodes.len(), 5);
        assert_eq!(reporter.model_usage().unwrap().0, 4);
        let second = design_scene(&task, &config, dir.path(), &invoker, &store, &cancel)
            .await
            .unwrap();
        assert_eq!(
            serde_json::to_value(first).unwrap(),
            serde_json::to_value(second).unwrap()
        );
        assert_eq!(reporter.model_usage().unwrap().0, 4);
        let spec = design_scene(&task, &config, dir.path(), &invoker, &store, &cancel)
            .await
            .unwrap();
        let mut repair_plan = plan.clone();
        repair_plan["objects"][0]["id"] = json!("region-ridge-terrain");
        script.push(FauxResponse::Text(repair_plan.to_string()));
        let repaired = repair_region(
            &task,
            &config,
            dir.path(),
            &invoker,
            &store,
            &cancel,
            &spec,
            "region-ridge-terrain",
            &json!({"evidence":"ridge relief needs correction"}),
        )
        .await
        .unwrap();
        assert_eq!(repaired.len(), 2);
        assert_eq!(repaired[1].node_id, "region-ridge-terrain");
        assert!(repaired
            .iter()
            .all(|n| n.node_id.starts_with("region-ridge") && n.revision == 2));
        assert_eq!(reporter.model_usage().unwrap().0, 5);
        assert!(repair_region(
            &task,
            &config,
            dir.path(),
            &invoker,
            &store,
            &cancel,
            &spec,
            "scene",
            &json!({})
        )
        .await
        .is_err());
        run.finish("accepted", json!({})).unwrap();
    }
}
