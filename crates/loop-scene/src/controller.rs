//! Deterministic durable controller for recursive staged scene execution.

use crate::backends::{BlenderBackend, SceneBackend};
use crate::config::SceneConfig;
use crate::recovery::{action, backoff_ms, classify, fingerprint, FailureClass, RecoveryAction};
use crate::retrieval::{build_embedder, AssetLibrary, AssetMetadata, AssetQuery};
use crate::roles::{FrontierProposal, Role, RoleInvoker};
use crate::spec::*;
use crate::storage::{
    atomic_write, hash_bytes, hash_file, ArtifactRecord, RunLock, RunState, RunStore,
    SceneTaskState,
};
use crate::tasks::{SceneTask, SceneTaskList};
use crate::validation::{self, FinalManifest, ManifestFile};
use crate::{Result, SceneError};
use serde::{Deserialize, Serialize};
use std::collections::{BTreeMap, HashMap, HashSet};
use std::path::{Path, PathBuf};
use std::sync::Arc;
use tokio_util::sync::CancellationToken;

/// User-facing doctor check.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorCheck {
    /// Check name.
    pub name: String,
    /// `pass`, `warning`, or `blocker`.
    pub status: String,
    /// Non-secret evidence.
    pub detail: String,
}
/// Complete preflight report.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DoctorReport {
    /// Whether generation may begin.
    pub runnable: bool,
    /// Whether visual acceptance can complete.
    pub visual_acceptance_available: bool,
    /// Checks.
    pub checks: Vec<DoctorCheck>,
}

/// Concise status/result for one independent queue item.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunSummary {
    /// Run ID.
    pub run_id: String,
    /// Queue task ID.
    pub task_id: String,
    /// Durable state.
    pub state: RunState,
    /// Run directory.
    pub run_dir: PathBuf,
    /// Final manifest when accepted.
    pub manifest: Option<PathBuf>,
    /// Explicit blocker if incomplete.
    pub blocker: Option<String>,
}

/// Scene controller. Creative choices are delegated to roles; only this type
/// validates/commits revisions, invokes deterministic workers, and accepts a run.
pub struct SceneRunController {
    config: SceneConfig,
    models: Option<Arc<loop_ai::Models>>,
    cancel: CancellationToken,
}
impl SceneRunController {
    /// Construct a controller. Models may be absent for status/cancel/doctor's
    /// non-model checks, but a run will durably wait for them.
    pub fn new(config: SceneConfig, models: Option<Arc<loop_ai::Models>>) -> Self {
        Self {
            config,
            models,
            cancel: CancellationToken::new(),
        }
    }
    /// Cancellation token for signal integration.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Run all enabled tasks independently. A blocker is returned per item and
    /// does not prevent subsequent queue items from being attempted.
    pub async fn run_queue(
        &self,
        list: &SceneTaskList,
        task_file: &Path,
    ) -> Vec<Result<RunSummary>> {
        let base = task_file.parent().unwrap_or_else(|| Path::new("."));
        let mut out = Vec::new();
        for task in list.tasks.iter().filter(|t| t.enabled) {
            match self.config.with_override(&task.overrides, base) {
                Ok(cfg) => out.push(self.run_one(task, cfg, None).await),
                Err(e) => out.push(Err(e)),
            }
        }
        out
    }

    /// Run/resume one queue item. When `existing` is set, immutable input
    /// snapshots from that run are authoritative.
    pub async fn run_one(
        &self,
        task: &SceneTask,
        mut config: SceneConfig,
        existing: Option<PathBuf>,
    ) -> Result<RunSummary> {
        let run_id = existing
            .as_ref()
            .and_then(|p| p.file_name())
            .and_then(|s| s.to_str())
            .map(str::to_owned)
            .unwrap_or_else(|| format!("{}-{}", task.id, uuid::Uuid::now_v7()));
        let root = existing.unwrap_or_else(|| config.storage.output_root.join(&run_id));
        std::fs::create_dir_all(&root).map_err(storage)?;
        // Generated objects belong to this individual run. A new run never
        // searches another scene's library, even when tasks share a config.
        // Resuming the same run keeps its local library and accepted work.
        config.storage.asset_library = root.join("asset-library");
        let store = RunStore::open(&root)?;
        let _lock = RunLock::acquire(&root)?;
        let config_json =
            serde_json::to_value(&config).map_err(|e| SceneError::Storage(e.to_string()))?;
        let task_json =
            serde_json::to_value(task).map_err(|e| SceneError::Storage(e.to_string()))?;
        store.initialize(&run_id, &task.id, &task.prompt, &config_json, &task_json)?;
        // The exclusive run lock proves no previous controller still owns tasks.
        // Recover its unfinished claims immediately, not after a 15-minute lease.
        store.recover_orphans(0)?;
        if store.status()?.1==RunState::Accepted && root.join("final/manifest.json").is_file() {
            for entry in std::fs::read_dir(root.join("final/previews")).map_err(storage)? {
                let path=entry.map_err(storage)?.path();
                if path.extension().and_then(|v|v.to_str())==Some("png") {
                    publish_previews(&root,&[path])?;
                }
            }
            return summary(&store,&root,&task.id,Some(root.join("final/manifest.json")));
        }
        let reporter=crate::reporting::Reporter::new(&root);
        let run_span=reporter.begin("run",&task.id,serde_json::json!({"run_id":run_id}))?;
        if root.join("CANCELLED").exists() {
            store.set_state(RunState::Cancelled, Some("explicit user cancellation"))?;
            run_span.finish("cancelled",serde_json::json!({}))?;
            return Ok(summary(&store, &root, &task.id, None)?);
        }
        store.set_state(RunState::Running,None)?;
        let resource_blocker = available_bytes(&root).and_then(|free| {
            (free < config.storage.min_free_bytes).then(|| {
                format!(
                    "only {free} bytes free; configured minimum is {}",
                    config.storage.min_free_bytes
                )
            })
        });
        let result = if let Some(reason) = resource_blocker {
            Err(SceneError::WaitingExternal(reason))
        } else {
            self.progress_run(&store, &root, task, &config).await
        };
        let status=match &result { Ok(_)=>"accepted",Err(SceneError::Cancelled)=>"cancelled",Err(SceneError::WaitingExternal(_))=>"waiting_external",Err(SceneError::Backend(reason)) if classify(reason)==FailureClass::ExternalBlocker=>"waiting_external",Err(_)=>"failed" };
        run_span.finish(status,serde_json::json!({"error":result.as_ref().err().map(ToString::to_string)}))?;
        match result {
            Ok(manifest) => {
                store.set_state(RunState::Accepted, None)?;
                Ok(summary(&store, &root, &task.id, Some(manifest))?)
            }
            Err(SceneError::Cancelled) => {
                store.set_state(RunState::Cancelled, Some("explicit user cancellation"))?;
                Ok(summary(&store, &root, &task.id, None)?)
            }
            Err(SceneError::WaitingExternal(reason)) => {
                store.set_state(RunState::WaitingExternal, Some(&reason))?;
                Ok(summary(&store, &root, &task.id, None)?)
            }
            Err(SceneError::Backend(reason))
                if classify(&reason) == FailureClass::ExternalBlocker =>
            {
                store.set_state(RunState::WaitingExternal, Some(&reason))?;
                Ok(summary(&store, &root, &task.id, None)?)
            }
            Err(e) => {
                store.set_state(RunState::Failed, Some(&e.to_string()))?;
                Err(e)
            }
        }
    }

    async fn progress_run(
        &self,
        store: &RunStore,
        root: &Path,
        task: &SceneTask,
        config: &SceneConfig,
    ) -> Result<PathBuf> {
        self.check_cancel(root)?;
        let models = self.models.clone().ok_or_else(|| {
            SceneError::WaitingExternal("Soket runtime/models unavailable".into())
        })?;
        let auth = models
            .check_auth(&config.models.provider)
            .await
            .ok_or_else(|| SceneError::WaitingExternal("Soket provider is unavailable".into()))?;
        if !auth.configured {
            return Err(SceneError::WaitingExternal(
                "Soket credentials are not configured".into(),
            ));
        }
        let backend = BlenderBackend::new(config.clone())?;
        let capabilities = backend.capabilities().await?;
        let embedder = build_embedder(&config.embedding)?;
        let library = AssetLibrary::open(&config.storage.asset_library, embedder)?;
        let reporter=crate::reporting::Reporter::new(root);
        let invoker = RoleInvoker::new(models, config.models.clone()).with_reporter(reporter.clone()).with_budget(config.budgets.clone());
        let planning=reporter.begin("stage","planning",serde_json::json!({}))?;
        let spec_path = root.join("designs/scene-spec.json");
        let mut spec:SceneSpec = if spec_path.exists() {
            serde_json::from_slice(&std::fs::read(&spec_path).map_err(storage)?)
                .map_err(|e| SceneError::Storage(e.to_string()))?
        } else {
            self.design_scene(store, root, task, config, &invoker)
                .await?
        };
        let accepted=store.accepted_nodes()?;
        for node in &mut spec.nodes {
            if let Some(saved)=accepted.iter().find(|n|n.node_id==node.node_id && n.revision>node.revision) {*node=saved.clone();}
        }
        spec.validate(
            config.quality.containment_tolerance_m,
            config.quality.relative_tolerance,
        )?;
        planning.finish("completed",serde_json::json!({"nodes":spec.nodes.len()}))?;
        write_execution_plan(root, &spec)?;
        eprintln!("Scene {}: plan accepted ({} nodes); generating components", task.id, spec.nodes.len());
        let components=reporter.begin("stage","components",serde_json::json!({}))?;
        self.generate_components(store, root, &mut spec, config, &backend, &library)
            .await?;
        components.finish("completed",serde_json::json!({}))?;
        atomic_write(
            &spec_path,
            &serde_json::to_vec_pretty(&spec).map_err(storage)?,
        )?;
        self.check_cancel(root)?;
        let mut visual_repairs = 0;
        let (assembly, report, parity) = loop {
            self.check_cancel(root)?;
            let repair_round = spec.nodes.iter().map(|n| n.revision).max().unwrap_or(1);
            let assembly_dir = root.join("assemblies").join(format!(
                "{}-attempt-{repair_round}",
                crate::storage::safe_id(&spec.root_id)
            ));
            let rendering=reporter.begin("stage","assembly_and_render",serde_json::json!({"round":repair_round}))?;
            let assembly = self
                .retry_backend(store, "assemble", None, config, || {
                    eprintln!("Scene {}: assembling and rendering preview views", task.id);
                    backend.assemble(&spec, &assembly_dir)
                })
                .await?;
            publish_previews(root, &assembly.previews)?;
            rendering.finish("completed",serde_json::json!({"previews":assembly.previews.len()}))?;
            let validating=reporter.begin("stage","validation",serde_json::json!({}))?;
            let mut report = validation::numerical(&spec, config, &assembly.inspection)?;
            if !report.passed {
                return Err(SceneError::Validation(
                    serde_json::to_string(&report.findings).unwrap_or_default(),
                ));
            }
            validation::render_health(&assembly.previews)?;
            let parity = backend
                .validate_exports(
                    &assembly.blend,
                    &assembly.glb,
                    &root
                        .join("validation")
                        .join(format!("attempt-{repair_round}")),
                )
                .await?;
            eprintln!("Scene {}: native/GLB validation complete", task.id);
            validating.finish("completed",serde_json::json!({"parity":parity}))?;
            report.parity = Some(parity.clone());
            report.previews = assembly.previews.clone();
            if !config.quality.require_visual_review {
                break (assembly, report, parity);
            }
            let labeled: Vec<_> = assembly
                .previews
                .iter()
                .map(|p| {
                    (
                        p.file_stem().and_then(|s| s.to_str()).unwrap_or("view"),
                        p.as_path(),
                    )
                })
                .collect();
            let review = invoker.review_images(&spec, &labeled).await?;
            self.account(store, config, review.tokens)?;
            let hard = review
                .output
                .findings
                .iter()
                .any(|f| matches!(f.severity.as_str(), "error" | "blocker"));
            report.visual_review = Some(review.output.clone());
            if !hard {
                break (assembly, report, parity);
            }
            if visual_repairs >= config.recovery.repair_attempts_per_strategy {
                return Err(SceneError::Validation(
                    "visual repair budget exhausted with unresolved findings".into(),
                ));
            }
            visual_repairs += 1;
            let response = invoker.invoke_json::<crate::roles::RepairDecision>(Role::RepairPlanner, &serde_json::json!({
                "scene_revision": spec,
                "actual_render_findings": review.output,
                "hard_rule": "Change the lowest responsible node without changing its parent transform or bounds. Use only a listed node_id."
            })).await?;
            self.account(store, config, response.tokens)?;
            let decision = response.output;
            if matches!(
                decision.action.as_str(),
                "redesign_subtree" | "revise_parent"
            ) {
                return Err(SceneError::WaitingExternal(format!(
                    "repair requires durable {} at {}; accepted artifacts preserved",
                    decision.action, decision.affected_node_id
                )));
            }
            let node = spec
                .nodes
                .iter_mut()
                .find(|n| n.node_id == decision.affected_node_id)
                .ok_or_else(|| {
                    SceneError::Validation("repair planner referenced unknown node".into())
                })?;
            node.revision += 1;
            node.generation.detail_policy = "high".into();
            node.generation.parameters.remove("component_glb_path");
            node.generation
                .parameters
                .insert("force_regenerate".into(), serde_json::Value::Bool(true));
            node.generation
                .parameters
                .extend(decision.generation_parameters);
            for material in &mut node.materials {
                material.roughness =
                    (material.roughness + decision.material_roughness_delta).clamp(0.0, 1.0);
            }
            node.provenance.model = response.model;
            node.provenance.prompt_version = response.prompt_version;
            node.provenance.validation_status = "repair_proposed".into();
            store.accept_node(
                node,
                &format!("# Repair revision\n\n{}", decision.explanation),
            )?;
            self.generate_components(store, root, &mut spec, config, &backend, &library)
                .await?;
            write_execution_plan(root, &spec)?;
            atomic_write(
                &spec_path,
                &serde_json::to_vec_pretty(&spec).map_err(storage)?,
            )?;
        };
        let validation_path = root.join("validation/final-report.json");
        atomic_write(
            &validation_path,
            &serde_json::to_vec_pretty(&report).map_err(storage)?,
        )?;
        let final_dir = root.join("final");
        let blend = final_dir.join("scene.blend");
        let glb = final_dir.join("scene.glb");
        promote_copy(&assembly.blend, &blend)?;
        promote_copy(&assembly.glb, &glb)?;
        let mut preview_files = Vec::new();
        for p in &assembly.previews {
            let dst = final_dir.join("previews").join(p.file_name().unwrap());
            promote_copy(p, &dst)?;
            preview_files.push(ManifestFile::from_path(dst)?);
        }
        let (_, _, prompt_hash) = read_snapshot_hashes(root)?;
        let config_hash = hash_file(&root.join("input/resolved-config.json"))?;
        let manifest = FinalManifest {
            version: 1,
            run_id: store.status()?.0,
            prompt_hash,
            config_hash,
            scene_revision: format!(
                "{}@{}",
                spec.root_id,
                spec.nodes.iter().map(|n| n.revision).max().unwrap_or(1)
            ),
            backend: serde_json::to_value(capabilities).map_err(storage)?,
            blend: ManifestFile::from_path(blend)?,
            glb: ManifestFile::from_path(glb)?,
            validation: vec![
                ManifestFile::from_path(validation_path.clone())?,
                ManifestFile::from_path(parity)?,
            ],
            previews: preview_files,
            assets: store.artifacts()?,
            outstanding_findings: report.findings.clone(),
        };
        if !manifest.outstanding_findings.is_empty() {
            return Err(SceneError::Validation("outstanding final findings".into()));
        }
        let manifest_path = final_dir.join("manifest.json");
        atomic_write(
            &manifest_path,
            &serde_json::to_vec_pretty(&manifest).map_err(storage)?,
        )?;
        let report_md=format!("# Scene acceptance report\n\nRun: `{}`\n\nCoordinate contract: right-handed meters, +X east, +Y north, +Z up. glTF mapping `(x,y,z) -> (x,z,-y)` is applied once by Blender's verified exporter.\n\n- Nodes: {}\n- Native: `{}`\n- Portable: `{}`\n- Preview views: {}\n- Numerical and cross-format validation: passed\n- Visual review: {}\n",manifest.run_id,spec.nodes.len(),manifest.blend.path.display(),manifest.glb.path.display(),manifest.previews.len(),if report.visual_review.is_some(){"passed actual-image review"}else{"disabled by explicit configuration"});
        atomic_write(&final_dir.join("report.md"), report_md.as_bytes())?;
        Ok(manifest_path)
    }

    async fn design_scene(
        &self,
        store: &RunStore,
        root: &Path,
        task: &SceneTask,
        config: &SceneConfig,
        invoker: &RoleInvoker,
    ) -> Result<SceneSpec> {
        let root_task = "design:scene";
        let input_hash = hash_bytes(task.prompt.as_bytes());
        let state = store.upsert_task(root_task, Some("scene"), "director", &input_hash, 100)?;
        let mut nodes = if state == SceneTaskState::Accepted {
            store.accepted_nodes()?
        } else {
            let owner = uuid::Uuid::now_v7().to_string();
            store
                .claim_task(root_task, &owner)?
                .ok_or_else(|| SceneError::Storage("director task could not be claimed".into()))?;
            store.set_task_state(root_task, SceneTaskState::Running, None, None)?;
            let payload = serde_json::json!({"original_prompt":task.prompt,"seed":task.seed.unwrap_or(0),"required_output":"FrontierProposal { parent: complete SceneNode root, children: complete immediate SceneNode shells, markdown: string }","node_contract":"Every SceneNode must include every field shown in node_contract_example. Root id is scene; root parent_id null. child_ids exactly match children. Each child has parent_id scene and placement_region matching one parent child_regions entry. Use generation.strategy=assembly for children needing recursive design, otherwise recipe/retrieve_or_recipe and a supported recipe.","node_contract_example":contract_example_node(),"supported_recipes":["wall","slab","roof","stairs","table","chair","cabinet","shelf","window","door","plant","light","generic_detailed"],"hard_constraints":{"units":"m","axes":"RH_M_ZUP_XEAST_YNORTH","identity_scale":true,"max_nodes":config.budgets.max_nodes,"required_exports":["blend","glb"],"interiors_must_not_be_omitted":true}});
            let response = if config.models.compact_planning {
                let cache_path=root.join("designs/compact-draft.json");
                let cache_hash=compact_draft_input_hash(task,config)?;
                let cached=load_compact_draft(&cache_path,&cache_hash)?;
                let (mut compact, reviewed)=if let Some(cached)=cached {
                    eprintln!("Scene {}: resuming saved {} layout",task.id,if cached.reviewed {"reviewed"} else {"initial"});
                    let reviewed=cached.reviewed;
                    (cached.into_response(),reviewed)
                } else {
                    let compact=invoker.invoke_json::<CompactLayout>(Role::SceneDirector,
                        &compact_design_contract(&task.prompt)).await?;
                    save_compact_draft(&cache_path,&cache_hash,false,&compact)?;
                    self.account(store,config,compact.tokens)?;
                    atomic_write(&root.join("designs/initial-layout.json"),&serde_json::to_vec_pretty(&compact.output).map_err(storage)?)?;
                    (compact,false)
                };
                if !reviewed {
                    eprintln!("Scene {}: reviewing structure, support, circulation and silhouette",task.id);
                    compact=invoker.invoke_json::<CompactLayout>(Role::StructureDesigner,
                        &serde_json::json!({"contract":compact_design_contract(&task.prompt),"draft":compact.output,"instructions":"Return a COMPLETE improved layout in the SAME compact schema. Check actual prompt fidelity, massing hierarchy, readable entrances, structural support, bridge landings and pedestrian circulation. Remove unrelated stylistic elements. Every building/wall/tree on terrain MUST name its support object; bridges MUST name two different terrain/landing objects in connects. Keep main focal structure clearly larger than details. Keep supported objects within 90 percent of the support footprint. Arrange perimeter castle walls to physically join corner towers without sealing the entrance. Minecraft terrain/pyramid/tower/house recipes already add detailed blocks: DO NOT replace them with solid slabs. No narrative outside JSON."})).await?;
                    save_compact_draft(&cache_path,&cache_hash,true,&compact)?;
                    self.account(store,config,compact.tokens)?;
                }
                for pass in 1..=2 {
                    let error=match validate_compact_draft(compact.output.clone(),config) {
                        Ok(_)=>break,
                        Err(error)=>error.to_string(),
                    };
                    eprintln!("Scene {}: correcting layout constraint ({}): {}",task.id,pass,error);
                    let contract = compact_design_contract(&task.prompt);
                    let correction = invoker.invoke_json::<CompactRepair>(Role::StructureDesigner,
                        &serde_json::json!({"original_prompt":task.prompt,"placement_contract":contract["placement_contract"],
                            "object_schema_for_additions":contract["object_schema"],"draft":compact.output,"validation_error":error,
                            "required_output":{"updates":[{"name":"exact existing object name","size":[8,8,2],"support":null}],"add":[]},
                            "instructions":"Return ONLY a small repair object with updates and add arrays, NOT a complete layout. In updates include exact name and ONLY changed fields: size, position, rotation, support, connects, parameters or rooms. Null clears support/connects. Preserve every existing object, recipe, material and occupied room. Correct ALL occurrences of the reported constraint, calculating legal dimensions/contact heights. Do not modify unrelated objects or add decorative detail. Add at most four fully specified objects only if needed as landing/support pads. Check support chains and bridge spacing after the changes. Preserve the requested design and clear circulation. If you return a complete layout instead, only the objects you actually changed are applied; do not rewrite unrelated parts. Return JSON only."})).await?;
                    self.account(store,config,correction.tokens)?;
                    match apply_compact_repair(&compact.output, correction.output) {
                        Ok(updated) => {
                            compact.output = updated;
                            compact.tokens = correction.tokens;
                            compact.model = correction.model;
                            compact.prompt_version = correction.prompt_version;
                            save_compact_draft(&cache_path,&cache_hash,true,&compact)?;
                        }
                        // A malformed, destructive or no-op repair cannot be
                        // applied. Keep the prior layout and spend the remaining
                        // bounded pass instead of aborting the whole scene; the
                        // final full validation still decides acceptance.
                        Err(error) => eprintln!("Scene {}: rejected layout repair ({}): {}",task.id,pass,error),
                    }
                }
                atomic_write(&root.join("designs/model-layout.json"),&serde_json::to_vec_pretty(&compact.output).map_err(storage)?)?;
                crate::roles::RoleResponse {
                    output:validate_compact_draft(compact.output,config)?,
                    // All actual compact calls are charged when received above.
                    tokens:0,model:compact.model,prompt_version:compact.prompt_version,
                }
            } else { invoker
                .invoke_json::<FrontierProposal>(Role::SceneDirector, &payload)
                .await? };
            if !config.models.compact_planning { self.account(store,config,response.tokens)?; }
            let mut proposal = response.output;
            stamp_proposal(
                &mut proposal,
                &response.model,
                &response.prompt_version,
                task.seed.unwrap_or(0),
            );
            let spec = SceneSpec {
                schema_version: 1,
                root_id: proposal.parent.node_id.clone(),
                prompt_summary: task.prompt.clone(),
                coordinate_system: "RH_M_ZUP_XEAST_YNORTH".into(),
                nodes: std::iter::once(proposal.parent.clone())
                    .chain(proposal.children.clone())
                    .collect(),
            };
            if spec.root_id != "scene" {
                return Err(SceneError::Validation(
                    "director root node_id must be scene".into(),
                ));
            }
            spec.validate(
                config.quality.containment_tolerance_m,
                config.quality.relative_tolerance,
            )?;
            store.accept_node(&proposal.parent, &proposal.markdown)?;
            for c in &proposal.children {
                store.accept_node(c, &format!("# {}\n\n{}", c.node_id, c.design))?;
            }
            store.set_task_state(
                root_task,
                SceneTaskState::Accepted,
                None,
                Some(&serde_json::json!({"root_id":spec.root_id})),
            )?;
            spec.nodes
        };
        let mut done: HashSet<String> = HashSet::from(["scene".into()]);
        loop {
            let next = nodes
                .iter()
                .find(|n| n.generation.strategy == "assembly" && n.kind!="furnished_room" && !done.contains(&n.node_id))
                .cloned();
            let Some(current) = next else { break };
            self.check_limits(&nodes, &current, config)?;
            let id = format!("design:{}", current.node_id);
            let hash = hash_bytes(&serde_json::to_vec(&current).map_err(storage)?);
            let state =
                store.upsert_task(&id, Some(&current.node_id), "frontier_design", &hash, 50)?;
            if state == SceneTaskState::Accepted {
                done.insert(current.node_id);
                continue;
            }
            let owner = uuid::Uuid::now_v7().to_string();
            if store.claim_task(&id, &owner)?.is_none() {
                return Err(SceneError::Storage(format!("could not claim {id}")));
            }
            store.set_task_state(&id, SceneTaskState::Running, None, None)?;
            let neighbors:Vec<_>=nodes.iter().filter(|n|n.parent_id==current.parent_id&&n.node_id!=current.node_id).map(|n|serde_json::json!({"node_id":n.node_id,"bounds":n.bounds_local,"transform":n.transform,"interfaces":n.interfaces})).collect();
            let payload = serde_json::json!({"original_requirements_summary":task.prompt,"node_revision":current,"inherited_hard_constraints":{"units":"m","axes":"RH_M_ZUP_XEAST_YNORTH","max_depth":config.budgets.max_depth},"relevant_neighbors":neighbors,"required_output":"FrontierProposal with revised parent and immediate children only; every node includes every field in node_contract_example; preserve parent transform/bounds and its transform-parent contract","node_contract_example":contract_example_node(),"supported_recipes":["wall","slab","roof","stairs","table","chair","cabinet","shelf","window","door","plant","light","generic_detailed"]});
            let response = invoker
                .invoke_json::<FrontierProposal>(role_for(&current), &payload)
                .await?;
            self.account(store, config, response.tokens)?;
            let mut p = response.output;
            stamp_proposal(
                &mut p,
                &response.model,
                &response.prompt_version,
                task.seed.unwrap_or(0),
            );
            if p.parent.node_id != current.node_id
                || p.parent.parent_id != current.parent_id
                || p.parent.bounds_local != current.bounds_local
                || p.parent.transform != current.transform
            {
                return Err(SceneError::Validation(format!(
                    "coordinator tried to change owning parent envelope/transform for {}",
                    current.node_id
                )));
            }
            p.parent.revision = current.revision + 1;
            p.parent.provenance.parent_revision = current.provenance.parent_revision;
            for c in &mut p.children {
                c.parent_id = Some(p.parent.node_id.clone());
                c.provenance.parent_revision = Some(p.parent.revision);
            }
            let mut merged: Vec<_> = nodes
                .iter()
                .filter(|n| n.node_id != current.node_id)
                .cloned()
                .collect();
            merged.push(p.parent.clone());
            merged.extend(p.children.clone());
            if merged.len() > config.budgets.max_nodes {
                return Err(SceneError::WaitingExternal(format!(
                    "node budget {} reached with remaining subtree {}; work was not omitted",
                    config.budgets.max_nodes, current.node_id
                )));
            }
            let candidate = SceneSpec {
                schema_version: 1,
                root_id: "scene".into(),
                prompt_summary: task.prompt.clone(),
                coordinate_system: "RH_M_ZUP_XEAST_YNORTH".into(),
                nodes: merged.clone(),
            };
            candidate.validate(
                config.quality.containment_tolerance_m,
                config.quality.relative_tolerance,
            )?;
            store.accept_node(&p.parent, &p.markdown)?;
            for c in &p.children {
                store.accept_node(c, &format!("# {}\n\n{}", c.node_id, c.design))?;
            }
            store.set_task_state(
                &id,
                SceneTaskState::Accepted,
                None,
                Some(&serde_json::json!({"children":p.parent.child_ids})),
            )?;
            nodes = merged;
            done.insert(current.node_id);
            atomic_write(
                &root.join("designs/scene-spec.partial.json"),
                &serde_json::to_vec_pretty(&candidate).map_err(storage)?,
            )?;
        }
        self.furnish_rooms(store,root,task,config,invoker,&mut nodes).await?;
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
            &root.join("designs/scene-spec.json"),
            &serde_json::to_vec_pretty(&spec).map_err(storage)?,
        )?;
        Ok(spec)
    }

    async fn furnish_rooms(&self, store:&RunStore, root:&Path, task:&SceneTask, config:&SceneConfig, invoker:&RoleInvoker, nodes:&mut Vec<SceneNode>) -> Result<()> {
        let rooms=nodes.iter().filter(|n|n.kind=="furnished_room").cloned().collect::<Vec<_>>();
        write_interior_inventory(root,nodes)?;
        let reporter=crate::reporting::Reporter::new(root);
        for room in rooms {
            if room.generation.strategy=="assembly" && !room.child_ids.is_empty() {continue;}
            self.check_cancel(root)?;
            let span=reporter.begin("stage",&format!("furnish:{}",room.node_id),serde_json::json!({"room":room.design}))?;
            eprintln!("Scene {}: furnishing {} ({})",task.id,room.design,room.node_id);
            let cache=root.join("designs/rooms").join(format!("{}.json",room.node_id));
            let draft=root.join("designs/rooms").join(format!("{}.draft.json",room.node_id));
            let input_hash=hash_bytes(&serde_json::to_vec(&room).map_err(storage)?);
            let cached=std::fs::read(&cache).or_else(|_|std::fs::read(&draft)).ok().and_then(|b|serde_json::from_slice::<serde_json::Value>(&b).ok()).filter(|v|v["input_hash"]==input_hash);
            let mut layout:Option<CompactLayout>=cached.as_ref().and_then(|v|serde_json::from_value(v["layout"].clone()).ok());
            let mut failure=String::new();
            let mut previous_plan=serde_json::Value::Null;
            let mut provenance=(cached.as_ref().and_then(|v|v["model"].as_str()).unwrap_or("cached").to_owned(),cached.as_ref().and_then(|v|v["prompt_version"].as_str()).unwrap_or("room-layout-v1").to_owned());
            let mut accepted=None;
            for attempt in 0..3 {
                if layout.is_none() {
                    let payload=serde_json::json!({"original_scene_prompt":task.prompt,"room":{"name":room.design,"purpose":room.purpose,"bounds_local":room.bounds_local,"size":room.bounds_local.size()},"recipes":FURNITURE_RECIPES,"schema":{"description":"room design intent","objects":[{"name":"unique item name","recipe":"one of the listed furniture recipes","size":[1.2,0.6,0.8],"position":[-2,-2,0],"rotation":[0,0,0],"color":[0.3,0.2,0.1],"roughness":0.55,"finish":"wood","support":null}]},"previous_error":failure,"instructions":"Design ONLY this room as if it were a standalone detailed interior. Output 8-18 useful furniture/decor components matching its purpose, including at least 4 functional furniture items. Use LOCAL room coordinates: X/Y around zero, floor at Z=0. Every rotated object must fit room.bounds_local. Follow circulation_rule, preserving its 0.9m route. Compose intentional functional groups rather than a repeated four-corner furniture template. Centrepieces, angled groups, asymmetric clusters and quiet negative space are welcome when the selected route permits them. Keep enough spacing to use chairs and doors. Rugs may cross circulation. Furniture must not intersect other furniture. Small tabletop items must name their supporting furniture using support. Do not set support for floor-standing furniture. Keep a consistent palette; include bookshelves, upholstered furniture, lighting and plants when appropriate. Recipes add small construction details automatically. No room shells, walls, terrain, exterior objects or nested rooms. No assembly parameters or file paths. Return only complete JSON, not Markdown. On an error repair the whole plan and check every object for the same issue."});
                    let mut payload=payload;
                    payload["circulation_rule"]=serde_json::json!(match room_circulation(&room) {
                        Circulation::Cross=>"Cross: keep abs(X)<0.45 OR abs(Y)<0.45 free of floor furniture.",
                        Circulation::SpineX=>"X-axis spine: keep abs(Y)<0.45 free; group furniture on either side, including across X=0.",
                        Circulation::SpineY=>"Y-axis spine: keep abs(X)<0.45 free; group furniture on either side, including across Y=0.",
                        Circulation::Perimeter=>"Perimeter loop: keep a 0.9m band inside all four room boundaries free; use the central area for asymmetrical clusters or a focal exhibit.",
                    });
                    payload["orientation_rule"]=serde_json::json!("All furniture must stand upright. rotation is THREE Euler degrees [0,0,yaw]. Turn furniture ONLY around Z (the THIRD number), never around Y. For a 90-degree turn use [0,0,90].");
                    payload["previous_plan"]=previous_plan.clone();
                    let response=invoker.invoke_json::<CompactLayout>(Role::LayoutDesigner,&payload).await?;
                    self.account(store,config,response.tokens)?;
                    provenance=(response.model,response.prompt_version);
                    layout=Some(response.output);
                    atomic_write(&draft,&serde_json::to_vec_pretty(&serde_json::json!({"input_hash":input_hash,"layout":layout,"model":provenance.0,"prompt_version":provenance.1})).map_err(storage)?)?;
                }
                let proposed=layout.as_ref().unwrap();
                match room_furniture(&room,proposed.clone()) {
                    Ok(children)=>{accepted=Some(children);break;},
                    Err(e)=>{failure=e.to_string();previous_plan=serde_json::to_value(proposed).map_err(storage)?;eprintln!("Room {}: correction {}/3: {}",room.node_id,attempt+1,failure);layout=None;},
                }
            }
            let mut children=accepted.ok_or_else(||SceneError::Validation(format!("room {} could not be furnished: {}",room.node_id,failure)))?;
            if nodes.len()+children.len()>config.budgets.max_nodes {return Err(SceneError::Validation("node budget exceeded while furnishing; rooms were not omitted".into()));}
            atomic_write(&cache,&serde_json::to_vec_pretty(&serde_json::json!({"input_hash":input_hash,"layout":layout,"model":provenance.0,"prompt_version":provenance.1})).map_err(storage)?)?;
            let mut revised=room.clone();revised.revision+=1;
            revised.generation.strategy="assembly".into();revised.child_ids=children.iter().map(|n|n.node_id.clone()).collect();
            for child in &mut children {child.provenance.model=provenance.0.clone();child.provenance.parent_revision=Some(revised.revision);child.generation.seed=room.generation.seed;}
            // Persist children first; an interrupted partial commit can recover
            // from the cached plan without paying for another model call.
            for child in &children {store.accept_node(child,&format!("# {}\n\nRoom-local furnished component",child.purpose))?;}
            store.accept_node(&revised,&format!("# {}\n\n{} furnished components; local placement checked",revised.design,children.len()))?;
            nodes.retain(|n|n.parent_id.as_deref()!=Some(&room.node_id));
            *nodes.iter_mut().find(|n|n.node_id==room.node_id).unwrap()=revised;
            let count=children.len();nodes.extend(children);
            write_interior_inventory(root,nodes)?;
            span.finish("completed",serde_json::json!({"components":count,"cache_hit":cached.is_some()}))?;
        }
        write_interior_inventory(root,nodes)?;
        Ok(())
    }

    async fn generate_components(
        &self,
        store: &RunStore,
        root: &Path,
        spec: &mut SceneSpec,
        config: &SceneConfig,
        backend: &BlenderBackend,
        library: &AssetLibrary,
    ) -> Result<()> {
        let ids: Vec<_> = spec
            .nodes
            .iter()
            .filter(|n| n.generation.strategy != "assembly" || n.child_ids.is_empty())
            .map(|n| n.node_id.clone())
            .collect();
        for id in ids {
            eprintln!("Generating component {id}");
            self.check_cancel(root)?;
            let idx = spec.nodes.iter().position(|n| n.node_id == id).unwrap();
            let node = spec.nodes[idx].clone();
            let hash = component_input_hash(&node)?;
            let task_id = format!("generate:{id}");
            let state = store.upsert_task(&task_id, Some(&id), "component", &hash, 20)?;
            if state == SceneTaskState::Accepted {
                continue;
            }
            let component_span=crate::reporting::Reporter::new(root).begin("component",&node.node_id,serde_json::json!({"recipe":node.generation.recipe,"parent":node.parent_id}))?;
            let size = node.bounds_local.size();
            if config.assets.reuse
                && !node.generation.parameters.contains_key("force_regenerate")
                && matches!(
                    node.generation.strategy.as_str(),
                    "retrieve_or_recipe" | "recipe"
                )
            {
                let candidates = library.search(AssetQuery {
                    text: &format!(
                        "{} {} {}",
                        node.kind,
                        node.design,
                        node.materials
                            .iter()
                            .map(|m| m.description.as_str())
                            .collect::<Vec<_>>()
                            .join(" ")
                    ),
                    category: Some(&node.kind),
                    max_size: Some(size),
                    interface_types: &node
                        .interfaces
                        .iter()
                        .map(|i| i.interface_type.clone())
                        .collect::<Vec<_>>(),
                    backend: Some("blender"),
                    allowed_licenses: &config.assets.allowed_licenses,
                    limit: 4,
                })?;
                if let Some(c) = candidates.into_iter().find(|c| {
                    let s = c.metadata.bounds.size();
                    (0..3).all(|i| {
                        (s[i] - size[i]).abs()
                            <= config.quality.containment_tolerance_m
                                + config.quality.relative_tolerance * size[i]
                    })
                }) {
                    let n = &mut spec.nodes[idx];
                    n.revision += 1;
                    n.generation.parameters.insert(
                        "component_glb_path".into(),
                        serde_json::json!(c.metadata.glb_path),
                    );
                    n.provenance
                        .dependency_hashes
                        .push(c.metadata.content_hash.clone());
                    n.provenance.validation_status = "accepted_reuse".into();
                    store.accept_node(n,&format!("# {}\n\nReused validated asset `{}` (cosine {:.3}) after exact bounds/interface/backend/license filters.",id,c.metadata.content_hash,c.score))?;
                    store.set_task_state(
                        &task_id,
                        SceneTaskState::Accepted,
                        None,
                        Some(
                            &serde_json::json!({"reused":c.metadata.content_hash,"score":c.score}),
                        ),
                    )?;
                    component_span.finish("reused",serde_json::json!({"source":c.metadata.content_hash}))?;
                    continue;
                }
            }
            let owner = uuid::Uuid::now_v7().to_string();
            let attempt = store
                .claim_task(&task_id, &owner)?
                .ok_or_else(|| SceneError::Storage(format!("could not claim {task_id}")))?;
            store.set_task_state(&task_id, SceneTaskState::Running, None, None)?;
            let attempt_dir = root
                .join("attempts")
                .join(crate::storage::safe_id(&task_id))
                .join(attempt.to_string());
            let output = self
                .retry_backend(store, &task_id, Some(&id), config, || {
                    backend.generate_component(&node, &attempt_dir)
                })
                .await?;
            validate_component(&node, &output.inspection, config)?;
            store.set_task_state(&task_id, SceneTaskState::Validating, None, None)?;
            let glb_hash = hash_file(&output.glb)?;
            let record = ArtifactRecord {
                content_hash: glb_hash.clone(),
                task_id: task_id.clone(),
                kind: "component_glb".into(),
                path: output.glb.clone(),
                bytes: std::fs::metadata(&output.glb).map_err(storage)?.len(),
                accepted: true,
                evidence: serde_json::from_slice(
                    &std::fs::read(&output.inspection).map_err(storage)?,
                )
                .map_err(storage)?,
            };
            store.record_artifact(&record)?;
            let metadata = AssetMetadata {
                content_hash: String::new(),
                description: node.design.clone(),
                category: node.kind.clone(),
                styles: node.assumptions.clone(),
                materials: node
                    .materials
                    .iter()
                    .map(|m| m.description.clone())
                    .collect(),
                construction_summary: node.construction_instructions.clone(),
                units: "m".into(),
                bounds: node.bounds_local,
                origin: node.pivot_local,
                front: node.local_front,
                up: node.local_up,
                interfaces: node.interfaces.clone(),
                parameter_ranges: BTreeMap::new(),
                supported_adaptations: vec!["material_rebind".into()],
                backend_version: "blender-4.5/procedural_pbr_v1".into(),
                recipe_version: node.provenance.recipe_version.clone(),
                native_path: None,
                glb_path: None,
                texture_paths: vec![],
                validation_evidence: record.evidence.clone(),
                source_url: node.provenance.source_url.clone(),
                creator: node.provenance.creator.clone(),
                license: node
                    .provenance
                    .license
                    .clone()
                    .or(Some("generated-local".into())),
                acquisition_date: Some(chrono::Utc::now().date_naive().to_string()),
                permitted_use: Some("locally generated; user controls use".into()),
                quality_status: "accepted".into(),
                tags: node.required_features.clone(),
            };
            match library.add(
                metadata,
                &[
                    ("glb", output.glb.as_path()),
                    ("blend", output.blend.as_path()),
                ],
            ) {
                Ok(indexed) => {
                    let n = &mut spec.nodes[idx];
                    n.revision += 1;
                    n.generation.parameters.insert(
                        "component_glb_path".into(),
                        serde_json::json!(indexed.glb_path),
                    );
                    n.provenance.dependency_hashes.push(indexed.content_hash);
                    n.provenance.validation_status = "accepted".into();
                    store.accept_node(n,&format!("# {}\n\nGenerated and independently inspected component; assembly references the promoted GLB by content hash.",id))?;
                }
                Err(e) => {
                    let class = FailureClass::Index;
                    let fp = fingerprint(class, &task_id, &e.to_string());
                    store.schedule_retry(
                        &format!("index:{id}"),
                        "index",
                        &fp,
                        "rebuild/index valid artifact without regenerating geometry",
                        loop_ai::now_ms() + 1000,
                        &serde_json::json!({"valid_artifact":record.content_hash}),
                    )?;
                }
            }
            store.set_task_state(
                &task_id,
                SceneTaskState::Accepted,
                None,
                Some(&serde_json::json!({"content_hash":glb_hash})),
            )?;
            component_span.finish("completed",serde_json::json!({"glb":output.glb,"blend":output.blend}))?;
        }
        Ok(())
    }

    async fn retry_backend<T, F, Fut>(
        &self,
        store: &RunStore,
        task_id: &str,
        node_id: Option<&str>,
        config: &SceneConfig,
        mut operation: F,
    ) -> Result<T>
    where
        F: FnMut() -> Fut,
        Fut: std::future::Future<Output = Result<T>>,
    {
        let mut last_fp = String::new();
        for attempt in 0..=config.recovery.repair_attempts_per_strategy {
            match operation().await {
                Ok(v) => return Ok(v),
                Err(e) => {
                    let class = classify(&e.to_string());
                    if class == FailureClass::ExternalBlocker {
                        return Err(SceneError::WaitingExternal(e.to_string()));
                    }
                    let fp = fingerprint(class, node_id.unwrap_or(task_id), &e.to_string());
                    let count = store.recovery_count(&fp)? + 1;
                    let act = action(class, count, config.recovery.repair_attempts_per_strategy);
                    let wait = backoff_ms(
                        attempt,
                        config.recovery.external_retry_max_backoff_seconds,
                        &fp,
                    );
                    store.schedule_retry(task_id,&format!("{class:?}"),&fp,&format!("{act:?}"),loop_ai::now_ms()+wait as i64,&serde_json::json!({"error":e.to_string(),"affected_node":node_id,"attempt":attempt}))?;
                    last_fp = fp;
                    if matches!(
                        act,
                        RecoveryAction::WaitExternal | RecoveryAction::ReviseParent
                    ) {
                        return Err(e);
                    }
                    tokio::time::sleep(std::time::Duration::from_millis(wait.min(2_000))).await;
                }
            }
        }
        Err(SceneError::Backend(format!(
            "recovery exhausted for {task_id} ({last_fp})"
        )))
    }
    fn account(&self, store: &RunStore, config: &SceneConfig, tokens: u64) -> Result<()> {
        let (calls, total) = store.add_model_usage(tokens)?;
        if calls > config.budgets.max_model_calls || total > config.budgets.max_total_tokens {
            return Err(SceneError::WaitingExternal(format!("model budget exhausted at {calls} calls/{total} tokens; run paused without weakening scope")));
        }
        Ok(())
    }
    fn check_cancel(&self, root: &Path) -> Result<()> {
        if self.cancel.is_cancelled() || root.join("CANCELLED").exists() {
            Err(SceneError::Cancelled)
        } else {
            Ok(())
        }
    }
    fn check_limits(
        &self,
        nodes: &[SceneNode],
        node: &SceneNode,
        config: &SceneConfig,
    ) -> Result<()> {
        let map: HashMap<_, _> = nodes.iter().map(|n| (n.node_id.as_str(), n)).collect();
        let mut depth = 1;
        let mut p = node.parent_id.as_deref();
        while let Some(id) = p {
            depth += 1;
            p = map.get(id).and_then(|n| n.parent_id.as_deref())
        }
        if depth >= config.budgets.max_depth {
            return Err(SceneError::WaitingExternal(format!(
                "depth budget reached at {}; subtree remains explicitly ungenerated",
                node.node_id
            )));
        }
        Ok(())
    }

    /// Run non-secret preflight checks, including real Blender generation,
    /// native/GLB reopen, local render, and embedding persistence.
    pub async fn doctor(&self) -> DoctorReport {
        let mut checks = Vec::new();
        let mut runnable = true;
        let mut visual = false;
        if self.config.models.provider != "soket" {
            runnable = false;
            checks.push(check("soket_provider", "blocker", "provider must be soket"))
        } else if let Some(models) = &self.models {
            let auth = models.check_auth("soket").await;
            let configured = auth.as_ref().is_some_and(|a| a.configured);
            checks.push(check(
                "soket_credentials",
                if configured { "pass" } else { "blocker" },
                if configured {
                    "configured (value not read or printed)"
                } else {
                    "not configured"
                },
            ));
            runnable &= configured;
            let base = models.get_model("soket", &self.config.models.default_model);
            checks.push(check(
                "default_model",
                if base.is_some() { "pass" } else { "blocker" },
                base.as_ref().map(|m| m.id.as_str()).unwrap_or("configured Soket model not found for default_model"),
            ));
            runnable &= base.is_some();
            if let Some(model) = &base {
                // A cached catalog is not proof the inference host is reachable.
                // GET only: never spend generation tokens during prerequisite checks.
                match crate::endpoint::probe(&model.base_url).await {
                    Ok(detail) => checks.push(check("model_endpoint", "pass", &detail)),
                    Err(detail) => {
                        runnable = false;
                        checks.push(check("model_endpoint", "blocker", &detail));
                    }
                }
            }
            for (role, cfg) in &self.config.models.roles {
                if models.get_model("soket", &cfg.model).is_none() {
                    runnable = false;
                    checks.push(check("role_model", "blocker", &format!("configured Soket model not found for {role}: {}", cfg.model)));
                }
            }
            for fallback in &self.config.models.fallback_models {
                if models.get_model("soket", fallback).is_none() {
                    checks.push(check("fallback_model", "warning", &format!("{fallback} absent from registered catalog; skipped during retries")));
                }
            }
            let review_cfg = self.config.models.roles.get("reviewer");
            visual = review_cfg
                .and_then(|r| models.get_model("soket", &r.model))
                .is_some_and(|m| m.supports_images());
            checks.push(check(
                "visual_model",
                if visual { "pass" } else if self.config.quality.require_visual_review { "blocker" } else { "warning" },
                if visual {
                    "verified image modality"
                } else {
                    "configure a Soket reviewer model whose catalog declares image input"
                },
            ));
        } else {
            runnable = false;
            checks.push(check(
                "soket_runtime",
                "blocker",
                "models runtime unavailable",
            ));
        }
        // Do not regenerate/render the doctor fixture on every endpoint retry.
        if !runnable {
            return DoctorReport { runnable, visual_acceptance_available: visual, checks };
        }
        match build_embedder(&self.config.embedding).and_then(|e| {
            let v = e.embed("oak chair seating")?;
            if v.len() != e.dimensions() || !v.iter().all(|x| x.is_finite()) {
                Err(SceneError::Validation("embedding probe invalid".into()))
            } else {
                Ok(())
            }
        }) {
            Ok(_) => checks.push(check(
                "embedding",
                "pass",
                &format!(
                    "{} dimensions={}",
                    self.config.embedding.model, self.config.embedding.dimensions
                ),
            )),
            Err(e) => {
                runnable = false;
                checks.push(check("embedding", "blocker", &e.to_string()))
            }
        }
        if let Err(e) = std::fs::create_dir_all(&self.config.storage.output_root)
            .and_then(|_| std::fs::create_dir_all(&self.config.storage.asset_library))
        {
            runnable = false;
            checks.push(check("storage", "blocker", &e.to_string()))
        } else {
            checks.push(check(
                "storage",
                "pass",
                &format!(
                    "writable roots: {}, {}",
                    self.config.storage.output_root.display(),
                    self.config.storage.asset_library.display()
                ),
            ))
        }
        match BlenderBackend::new(self.config.clone()) {
            Err(e) => {
                runnable = false;
                checks.push(check("blender", "blocker", &e.to_string()))
            }
            Ok(backend) => match backend.capabilities().await {
                Err(e) => {
                    runnable = false;
                    checks.push(check("blender", "blocker", &e.to_string()))
                }
                Ok(caps) => {
                    checks.push(check(
                        "blender",
                        "pass",
                        &format!("{} at {}", caps.version, caps.executable.display()),
                    ));
                    let d = tempfile::tempdir();
                    if let Ok(d) = d {
                        let fixture = fixture_node();
                        let probe = match backend.generate_component(&fixture, d.path()).await {
                            Ok(o) => backend.validate_exports(&o.blend, &o.glb, d.path()).await,
                            Err(e) => Err(e),
                        };
                        match probe{Ok(_)=>checks.push(check("blender_export_render","pass","generated geometry, saved .blend, exported/reimported .glb, and rendered GLB inspection")),Err(e)=>{runnable=false;checks.push(check("blender_export_render","blocker",&e.to_string()))}}
                    }
                }
            },
        }
        if self.config.quality.require_visual_review && !visual {
            runnable = false;
        }
        if let Some(free) = available_bytes(&self.config.storage.output_root) {
            let ok = free >= self.config.storage.min_free_bytes;
            checks.push(check(
                "disk_space",
                if ok { "pass" } else { "blocker" },
                &format!(
                    "{free} bytes free; minimum {}",
                    self.config.storage.min_free_bytes
                ),
            ));
            runnable &= ok;
        }
        DoctorReport {
            runnable,
            visual_acceptance_available: visual,
            checks,
        }
    }

    /// Read-only status from a run directory.
    pub fn status(run_dir: &Path) -> Result<RunSummary> {
        let store = RunStore::open(run_dir)?;
        let (id, state, blocker) = store.status()?;
        let task: serde_json::Value = serde_json::from_slice(
            &std::fs::read(run_dir.join("input/task.json")).map_err(storage)?,
        )
        .map_err(storage)?;
        Ok(RunSummary {
            run_id: id,
            task_id: task["id"].as_str().unwrap_or("unknown").into(),
            state,
            run_dir: run_dir.into(),
            manifest: run_dir
                .join("final/manifest.json")
                .exists()
                .then(|| run_dir.join("final/manifest.json")),
            blocker,
        })
    }
    /// Mark a run explicitly cancelled without starting recovery.
    pub fn cancel_run(run_dir: &Path) -> Result<RunSummary> {
        let store = RunStore::open(run_dir)?;
        atomic_write(&run_dir.join("CANCELLED"), b"explicit user cancellation\n")?;
        store.set_state(RunState::Cancelled, Some("explicit user cancellation"))?;
        Self::status(run_dir)
    }
}

fn check(name: &str, status: &str, detail: &str) -> DoctorCheck {
    DoctorCheck {
        name: name.into(),
        status: status.into(),
        detail: detail.into(),
    }
}
fn summary(
    store: &RunStore,
    root: &Path,
    task_id: &str,
    manifest: Option<PathBuf>,
) -> Result<RunSummary> {
    let (id, state, blocker) = store.status()?;
    Ok(RunSummary {
        run_id: id,
        task_id: task_id.into(),
        state,
        run_dir: root.into(),
        manifest,
        blocker,
    })
}
fn storage(e: impl std::fmt::Display) -> SceneError {
    SceneError::Storage(e.to_string())
}
// Publish diagnostic images before acceptance, too: failed validation must not
// leave the user-facing preview directory empty after successful rendering.
fn publish_previews(root:&Path, paths:&[PathBuf])->Result<()> {
    for path in paths {
        let name=path.file_name().ok_or_else(||SceneError::Storage("preview has no filename".into()))?;
        promote_copy(path,&root.join("previews").join(name))?;
    }
    Ok(())
}

fn component_input_hash(node:&SceneNode)->Result<String> {
    let mut generation=node.generation.clone();
    generation.parameters.remove("component_glb_path");
    Ok(hash_bytes(&serde_json::to_vec(&serde_json::json!({"node_id":node.node_id,"kind":node.kind,"bounds":node.bounds_local,"materials":node.materials,"construction":node.construction,"generation":generation})).map_err(storage)?))
}
fn role_for(node: &SceneNode) -> Role {
    let k = node.kind.to_lowercase();
    if ["building", "floor", "wall", "roof", "stairs", "structure"]
        .iter()
        .any(|x| k.contains(x))
    {
        Role::StructureDesigner
    } else if ["room", "space", "layout", "garden", "site"]
        .iter()
        .any(|x| k.contains(x))
    {
        Role::LayoutDesigner
    } else {
        Role::DesignCoordinator
    }
}
fn stamp_proposal(p: &mut FrontierProposal, model: &str, prompt: &str, seed: u64) {
    p.parent.provenance.model = model.into();
    p.parent.provenance.prompt_version = prompt.into();
    p.parent.generation.seed ^= seed;
    for c in &mut p.children {
        c.provenance.model = model.into();
        c.provenance.prompt_version = prompt.into();
        c.generation.seed ^= seed;
    }
}
fn promote_copy(src: &Path, dst: &Path) -> Result<()> {
    let bytes = std::fs::read(src).map_err(storage)?;
    atomic_write(dst, &bytes)
}
fn read_snapshot_hashes(root: &Path) -> Result<(String, String, String)> {
    let p = root.join("input/prompt.txt");
    let c = root.join("input/resolved-config.json");
    Ok((
        p.display().to_string(),
        c.display().to_string(),
        hash_file(&p)?,
    ))
}
const VOXEL_RECIPES: &[&str] = &["voxel_terrain", "voxel_tower", "voxel_wall", "voxel_gatehouse", "voxel_house", "voxel_pyramid", "voxel_tree", "voxel_bridge", "voxel_waterfall", "voxel_stairs"];
const COMPACT_RECIPES: &[&str] = &["terrain", "room_shell", "sofa", "bed", "rug", "bookshelf", "desk", "bench", "art", "kitchen_counter", "wall", "slab", "table", "chair", "cabinet", "shelf", "window", "door", "plant", "light", "ring", "gallery", "bridge", "arcade", "garden", "waterfall", "hologram", "light_beam"];
const FURNITURE_RECIPES: &[&str] = &["sofa","bed","rug","bookshelf","desk","bench","art","kitchen_counter","table","chair","cabinet","shelf","plant","light"];

#[derive(Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
struct CompactDraft {
    version: u32,
    input_hash: String,
    reviewed: bool,
    layout: CompactLayout,
    model: String,
    prompt_version: String,
}

impl CompactDraft {
    fn into_response(self) -> crate::roles::RoleResponse<CompactLayout> {
        crate::roles::RoleResponse {output:self.layout,tokens:0,model:self.model,prompt_version:self.prompt_version}
    }
}

fn compact_draft_input_hash(task: &SceneTask, config: &SceneConfig) -> Result<String> {
    // Identity includes the scene seed and effective configuration, not only its
    // prose. Deliberately independent of implementation changes so a new resolver
    // can recover a previously rejected draft without paying for another draft.
    Ok(hash_bytes(&serde_json::to_vec(&serde_json::json!({
        "format":"compact-draft-v1","task":task,"config":config,
    })).map_err(storage)?))
}

fn load_compact_draft(path: &Path, input_hash: &str) -> Result<Option<CompactDraft>> {
    let bytes=match std::fs::read(path) {
        Ok(bytes)=>bytes,
        Err(error) if error.kind()==std::io::ErrorKind::NotFound=>return Ok(None),
        Err(error)=>return Err(storage(error)),
    };
    let draft=match serde_json::from_slice::<CompactDraft>(&bytes) {
        Ok(draft)=>draft,
        Err(error)=>{ eprintln!("Ignoring unreadable compact checkpoint {}: {}",path.display(),error); return Ok(None); }
    };
    Ok((draft.version==1 && draft.input_hash==input_hash && !draft.model.is_empty()
        && !draft.prompt_version.is_empty()).then_some(draft))
}

fn save_compact_draft(path: &Path, input_hash: &str, reviewed: bool, response: &crate::roles::RoleResponse<CompactLayout>) -> Result<()> {
    let draft=CompactDraft {version:1,input_hash:input_hash.into(),reviewed,layout:response.output.clone(),
        model:response.model.clone(),prompt_version:response.prompt_version.clone()};
    atomic_write(path,&serde_json::to_vec_pretty(&draft).map_err(storage)?)
}

fn validate_compact_draft(layout: CompactLayout, config: &SceneConfig) -> Result<FrontierProposal> {
    let proposal=expand_compact_layout(layout)?;
    let spec=SceneSpec {schema_version:1,root_id:proposal.parent.node_id.clone(),
        prompt_summary:proposal.markdown.clone(),coordinate_system:"RH_M_ZUP_XEAST_YNORTH".into(),
        nodes:std::iter::once(proposal.parent.clone()).chain(proposal.children.clone()).collect()};
    spec.validate(config.quality.containment_tolerance_m,config.quality.relative_tolerance)?;
    Ok(proposal)
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct RoomBrief {
    name: String,
    purpose: String,
    size: [f64;3],
    position: [f64;3],
    #[serde(default)]
    circulation: Circulation,
}

#[derive(Clone, Copy, Default, Deserialize, Serialize)]
#[serde(rename_all="snake_case")]
enum Circulation {
    #[default]
    Cross,
    SpineX,
    SpineY,
    Perimeter,
}

fn room_circulation(room: &SceneNode) -> Circulation {
    room.generation.parameters.get("circulation").cloned().and_then(|v|serde_json::from_value(v).ok()).unwrap_or_default()
}

fn circulation_blocked(room:&SceneNode,b:&Bounds)->bool {
    if b.min[2]>=1.9 || b.max[2]<=0.1 {return false;}
    let crosses=|axis:usize| b.min[axis]<0.45 && b.max[axis]>-0.45;
    match room_circulation(room) {
        Circulation::Cross=>crosses(0)||crosses(1),
        Circulation::SpineX=>crosses(1),
        Circulation::SpineY=>crosses(0),
        Circulation::Perimeter=>(0..2).any(|a|b.min[a]<room.bounds_local.min[a]+0.9 || b.max[a]>room.bounds_local.max[a]-0.9),
    }
}

fn allocated_rooms(object: &CompactObject) -> Result<Vec<RoomBrief>> {
    let mut rooms=object.rooms.clone();
    if rooms.is_empty() && object.habitable!=Some(false)
        && ["room_shell","voxel_house","voxel_tower","gallery"].contains(&object.recipe.as_str())
        && object.size[0]>=3.5 && object.size[1]>=3.5 && object.size[2]>=2.6 {
        let margin=if object.recipe.starts_with("voxel") {0.8} else {0.3};
        let height=if object.recipe=="voxel_house" {object.size[2]*0.58} else {object.size[2]-0.25};
        let floors=if object.recipe=="voxel_tower" {
            object.parameters.get("floors").and_then(|v|v.as_u64()).unwrap_or((object.size[2]/3.0).round() as u64).clamp(1,6) as usize
        } else {1};
        for i in 0..floors {
            let floor_height=if object.recipe=="voxel_tower" {(object.size[2]-0.65)/floors as f64} else {height};
            if floor_height<2.2 {return Err(SceneError::Validation(format!("{} floors are too short to furnish",object.name)));}
            rooms.push(RoomBrief { name:format!("{} room {}",object.name,i+1),purpose:object.name.clone(),
                size:[object.size[0]-2.0*margin,object.size[1]-2.0*margin,floor_height-0.3],
                position:[0.0,0.0,i as f64*floor_height+0.18], circulation:object.parameters.get("circulation").cloned().map(serde_json::from_value).transpose().map_err(storage)?.unwrap_or_default() });
        }
    }
    if rooms.len()>16 {return Err(SceneError::Validation("at most 16 rooms per component; split the building".into()));}
    let bounds=Bounds {min:[-object.size[0]/2.0,-object.size[1]/2.0,0.0],max:[object.size[0]/2.0,object.size[1]/2.0,object.size[2]]};
    let mut used=Vec::<Bounds>::new();
    for room in &rooms {
        if room.size.iter().any(|v|!v.is_finite()||*v<1.8) || room.position.iter().any(|v|!v.is_finite()) {return Err(SceneError::Validation(format!("invalid room {}",room.name)));}
        let r=Bounds {min:[room.position[0]-room.size[0]/2.0,room.position[1]-room.size[1]/2.0,room.position[2]],max:[room.position[0]+room.size[0]/2.0,room.position[1]+room.size[1]/2.0,room.position[2]+room.size[2]]};
        if !r.corners().iter().all(|p|bounds.contains(*p,0.001,0.0)) {return Err(SceneError::Validation(format!("room {} exceeds building {}",room.name,object.name)));}
        if used.iter().any(|b|(0..3).all(|i|r.max[i].min(b.max[i])-r.min[i].max(b.min[i])>0.02)) {return Err(SceneError::Validation(format!("room {} overlaps another room",room.name)));}
        used.push(r);
    }
    Ok(rooms)
}

fn write_interior_inventory(root:&Path,nodes:&[SceneNode])->Result<()> {
    let rooms=nodes.iter().filter(|n|n.kind=="furnished_room").map(|n|serde_json::json!({"id":n.node_id,"name":n.design,"purpose":n.purpose,"parent":n.parent_id,"furniture_count":n.child_ids.len(),"status":if n.child_ids.is_empty(){"pending"}else{"furnished"}})).collect::<Vec<_>>();
    atomic_write(&root.join("designs/interior-inventory.json"),&serde_json::to_vec_pretty(&serde_json::json!({"room_count":rooms.len(),"furnished_count":rooms.iter().filter(|r|r["status"]=="furnished").count(),"rooms":rooms})).map_err(storage)?)
}

fn room_furniture(room:&SceneNode, mut layout:CompactLayout)->Result<Vec<SceneNode>> {
    if layout.objects.len()<6 || layout.objects.len()>30 {return Err(SceneError::Validation("room needs 6-30 components, not an empty or token-filled interior".into()));}
    if layout.objects.iter().any(|o|!FURNITURE_RECIPES.contains(&o.recipe.as_str())||!o.rooms.is_empty()) {return Err(SceneError::Validation("room can contain only supported furniture recipes".into()));}
    if layout.objects.iter().filter(|o|!["rug","art","plant","light"].contains(&o.recipe.as_str())).count()<4 {return Err(SceneError::Validation("room needs at least four functional furniture items".into()));}
    resolve_layout_connections(&mut layout)?;
    let original_positions=layout.objects.iter().map(|o|o.position).collect::<Vec<_>>();
    let original_rotations=layout.objects.iter().map(|o|o.rotation).collect::<Vec<_>>();
    // These recipes are upright floor/wall furniture, not arbitrary posed
    // meshes. Prevent accidental Y-up model conventions from tipping a sofa
    // through the floor; retain the explicit Z heading and record corrections.
    for object in &mut layout.objects {object.rotation[0]=0.0;object.rotation[1]=0.0;}
    fit_room_layout(room,&mut layout)?;
    fit_supported_props(room, &mut layout)?;
    resolve_layout_connections(&mut layout)?;
    let objects=layout.objects.clone();
    let mut children=expand_compact_layout(layout)?.children;
    let mut boxes=Vec::new();
    for (i,n) in children.iter_mut().enumerate() {
        n.node_id=format!("{}-item-{:02}",room.node_id,i+1);n.parent_id=Some(room.node_id.clone());
        n.materials[0].material_id=format!("{}-material",n.node_id);
        let offset=[objects[i].position[0]-original_positions[i][0],objects[i].position[1]-original_positions[i][1],objects[i].position[2]-original_positions[i][2]];
        if offset.iter().any(|v|v.abs()>0.001) {n.generation.parameters.insert("room_fit_offset_m".into(),serde_json::json!(offset));}
        if original_rotations[i][0].abs()>0.001 || original_rotations[i][1].abs()>0.001 {
            n.generation.parameters.insert("upright_correction_degrees".into(),serde_json::json!([-original_rotations[i][0],-original_rotations[i][1],0]));
        }
        n.child_regions.clear();n.placement_region=None;
        let corners=n.bounds_local.corners().map(|p|n.transform.point(p));
        if !corners.iter().all(|p|room.bounds_local.contains(*p,0.005,0.0)) {return Err(SceneError::Validation(format!("{} exceeds room bounds {:?}; move/resize it",n.purpose,room.bounds_local)));}
        let mut b=Bounds {min:[f64::INFINITY;3],max:[f64::NEG_INFINITY;3]};
        for p in corners {for axis in 0..3 {b.min[axis]=b.min[axis].min(p[axis]);b.max[axis]=b.max[axis].max(p[axis]);}}
        if n.kind!="rug" && circulation_blocked(room,&b) {
            return Err(SceneError::Validation(format!("{} blocks the selected 0.9m circulation route; move it outside that route",n.purpose)));
        }
        for (j,other) in boxes.iter().enumerate() {
            let other:&Bounds=other;
            let supported=objects[i].support.as_deref()==Some(&objects[j].name)||objects[j].support.as_deref()==Some(&objects[i].name);
            if !supported && objects[i].recipe!="rug" && objects[j].recipe!="rug" && (0..3).all(|a|b.max[a].min(other.max[a])-b.min[a].max(other.min[a])>0.04) {
                return Err(SceneError::Validation(format!("{} overlaps {}; separate their occupied volumes",objects[i].name,objects[j].name)));
            }
        }
        boxes.push(b);
    }
    Ok(children)
}

fn fit_supported_props(room: &SceneNode, layout: &mut CompactLayout) -> Result<()> {
    let mut done: HashSet<usize> = layout.objects.iter().enumerate()
        .filter_map(|(i, o)| o.support.is_none().then_some(i)).collect();
    for _ in 0..layout.objects.len() {
        resolve_layout_connections(layout)?;
        for i in 0..layout.objects.len() {
            if done.contains(&i) { continue; }
            let object = layout.objects[i].clone();
            let parent = layout.objects.iter().position(|o| Some(&o.name) == object.support.as_ref()).unwrap();
            if !done.contains(&parent) { continue; }
            // Keep rotated support cases for the full geometric validator.
            if layout.objects[parent].support.is_some() || layout.objects[parent].rotation != [0.0; 3] || object.rotation != [0.0; 3] {
                done.insert(i);
                continue;
            }
            let support = &layout.objects[parent];
            let half = [object.size[0] / 2.0, object.size[1] / 2.0];
            let lo = [support.position[0] - support.size[0]*0.45 + half[0], support.position[1] - support.size[1]*0.45 + half[1]];
            let hi = [support.position[0] + support.size[0]*0.45 - half[0], support.position[1] + support.size[1]*0.45 - half[1]];
            let mut candidates = vec![[object.position[0], object.position[1]]];
            for x in 0..=24 { for y in 0..=24 {
                candidates.push([lo[0] + (hi[0]-lo[0])*x as f64/24.0, lo[1] + (hi[1]-lo[1])*y as f64/24.0]);
            }}
            candidates.sort_by(|a,b| ((a[0]-object.position[0]).powi(2)+(a[1]-object.position[1]).powi(2))
                .total_cmp(&((b[0]-object.position[0]).powi(2)+(b[1]-object.position[1]).powi(2))));
            let found = candidates.into_iter().find(|p| {
                let b = Bounds { min:[p[0]-half[0],p[1]-half[1],object.position[2]],
                    max:[p[0]+half[0],p[1]+half[1],object.position[2]+object.size[2]] };
                b.corners().iter().all(|p| room.bounds_local.contains(*p,0.005,0.0)) && done.iter().all(|j| {
                    if *j == parent || layout.objects[*j].recipe == "rug" { return true; }
                    let o = &layout.objects[*j];
                    let yaw = o.rotation[2].to_radians();
                    let hx = (o.size[0]*yaw.cos().abs()+o.size[1]*yaw.sin().abs())/2.0;
                    let hy = (o.size[0]*yaw.sin().abs()+o.size[1]*yaw.cos().abs())/2.0;
                    let other = Bounds {min:[o.position[0]-hx,o.position[1]-hy,o.position[2]],max:[o.position[0]+hx,o.position[1]+hy,o.position[2]+o.size[2]]};
                    !(0..3).all(|a| b.max[a].min(other.max[a])-b.min[a].max(other.min[a])>0.0)
                })
            });
            if let Some(p) = found {
                layout.objects[i].position[0] = p[0];
                layout.objects[i].position[1] = p[1];
            }
            // If no placement exists, the unchanged plan still fails normal
            // bounds/overlap validation; never drop props or loosen tolerances.
            done.insert(i);
        }
        if done.len() == layout.objects.len() { break; }
    }
    Ok(())
}

fn fit_room_layout(room:&SceneNode,layout:&mut CompactLayout)->Result<()> {
    // Keep model-selected furniture, sizes, rotations and materials. Fit their
    // positions into legal zones instead of spending a new LLM call on every
    // small overflow. Supported items move only through support resolution.
    let proposal=expand_compact_layout(layout.clone())?;
    let mut order=(0..layout.objects.len()).collect::<Vec<_>>();
    order.sort_by(|a,b|(layout.objects[*b].size[0]*layout.objects[*b].size[1]).total_cmp(&(layout.objects[*a].size[0]*layout.objects[*a].size[1])));
    let mut placed=Vec::<Bounds>::new();
    for i in order {
        let object=layout.objects[i].clone();
        if object.support.is_some() {continue;}
        let node=&proposal.children[i];
        let mut bbox=Bounds {min:[f64::INFINITY;3],max:[f64::NEG_INFINITY;3]};
        for p in node.bounds_local.corners().map(|p|node.transform.point(p)) {for a in 0..3 {bbox.min[a]=bbox.min[a].min(p[a]);bbox.max[a]=bbox.max[a].max(p[a]);}}
        // Use transformed bounds rather than assuming an unrotated height.
        // Clamp the bottom, preserving dimensions and upright/yaw orientation.
        let height=bbox.max[2]-bbox.min[2];
        let low=room.bounds_local.min[2];
        let high=room.bounds_local.max[2]-height;
        if !height.is_finite() || high<low {
            return Err(SceneError::Validation(format!("{} is taller than its room; resize it",object.name)));
        }
        let dz=bbox.min[2].clamp(low,high)-bbox.min[2];
        layout.objects[i].position[2]+=dz;
        bbox.min[2]+=dz; bbox.max[2]+=dz;
        if object.recipe=="rug" {continue;}
        let half=[(bbox.max[0]-bbox.min[0])/2.0,(bbox.max[1]-bbox.min[1])/2.0];
        let center=[(bbox.max[0]+bbox.min[0])/2.0,(bbox.max[1]+bbox.min[1])/2.0];
        let mut candidates=Vec::new();
        // Search the whole floor, keeping the proposed position first. The selected
        // circulation policy filters candidates without imposing four quadrants.
        let margin=if matches!(room_circulation(room),Circulation::Perimeter) {0.92} else {0.02};
        let lo=[room.bounds_local.min[0]+margin+half[0],room.bounds_local.min[1]+margin+half[1]];
        let hi=[room.bounds_local.max[0]-margin-half[0],room.bounds_local.max[1]-margin-half[1]];
        if (0..2).all(|a|lo[a]<=hi[a]) {
            candidates.push([center[0].clamp(lo[0],hi[0]),center[1].clamp(lo[1],hi[1])]);
            for x in 0..=32 {for y in 0..=32 {candidates.push([lo[0]+(hi[0]-lo[0])*x as f64/32.0,lo[1]+(hi[1]-lo[1])*y as f64/32.0]);}}
        }
        candidates.sort_by(|a,b|((a[0]-center[0]).powi(2)+(a[1]-center[1]).powi(2)).total_cmp(&((b[0]-center[0]).powi(2)+(b[1]-center[1]).powi(2))));
        let found=candidates.into_iter().find_map(|candidate| {
            let mut b=bbox;for a in 0..2 {b.min[a]=candidate[a]-half[a];b.max[a]=candidate[a]+half[a];}
            if circulation_blocked(room,&b) || placed.iter().any(|p|(0..3).all(|a|b.max[a].min(p.max[a])-b.min[a].max(p.min[a])> -0.06)) {None} else {Some((candidate,b))}
        });
        let Some((candidate,b))=found else {return Err(SceneError::Validation(format!("{} cannot fit with other furniture while keeping circulation clear; reduce its size or use fewer/lighter pieces",object.name)));};
        for a in 0..2 {layout.objects[i].position[a]+=candidate[a]-center[a];}
        placed.push(b);
    }
    Ok(())
}

fn compact_design_contract(prompt: &str) -> serde_json::Value {
    let mut contract=base_compact_design_contract(prompt);
    contract["placement_contract"]=serde_json::json!({
        "support":"Means RESTING ON TOP, not inside, around, attached to a side, or sharing a foundation. It overrides Z to support.position.z + support.size.z, and clamps XY. Except room_shell, child X/Y dimensions must each be <= 90% of support X/Y dimensions (room_shell <=100%). Give surrounding rings, enclosing ramps, and overhanging roofs their own explicit position with no support reference to the enclosed building; preserve physical contact through dimensions/placement.",
        "connects":"Only for two SEPARATE, non-overlapping landings with distinct XY centres. Never connect concentric rings or nested structures by their centres. Use separate slab landing pads at actual entrances for these connections. Bridge slope must be <=0.5 after subtracting both landing footprints. connects and support are mutually exclusive.",
        "recipe_shapes":"gallery is a straight rectangular glazed pavilion, NOT a spiral ramp or annular cloister. arcade is a straight row of columns. ring is an annular glazed deck/roof with a central void. slab is rectangular, NOT a conical roof. Assemble supported parts to express the requested shape; a name alone never changes geometry."
    });
    contract["terrain_capability"]=serde_json::json!("terrain is an alias for the bounded voxel_terrain block-cliff generator with a flat usable top; it is not smooth geological mesh synthesis. Use slab for a plain architectural foundation.");
    contract["object_schema"]["habitable"]=serde_json::json!("optional boolean; false ONLY for explicitly exterior-only/non-habitable props");
    contract["object_schema"]["rooms"]=serde_json::json!([{"name":"unique room name","purpose":"room function, required furniture and style","size":[6,6,2.8],"position":[0,0,0.18],"circulation":"cross | spine_x | spine_y | perimeter (choose by intended visitor route)"}]);
    contract["interior_requirements"]=serde_json::json!("Every habitable building needs furnished rooms. Allocate rooms now, not furniture: a dedicated room designer fills each one later. rooms sizes are CLEAR interior dimensions; positions are building-LOCAL centre X/Y and floor Z. Keep rooms non-overlapping and inside shell with .3m wall clearance. Room heights >=2.2m. Vary room aspect ratios and sizes by function; rooms can be elongated, intimate or monumental. Select circulation per room: cross joins four centred doors, spine_x joins X-side doors, spine_y joins Y-side doors, perimeter keeps a 0.9m loop around a central exhibit cluster. Do not default every room to the same circulation. For ordinary multi-room buildings, room_shell objects are supported rectangular envelopes; compose them as staggered wings, L/U courtyards or rotated pavilions where access and support allow, not always a row of equal squares; each includes floor, four walls with centred doors, windows and roof, and defaults to one furnished room. Align adjacent door centres. Do NOT substitute solid slabs for buildings. For existing voxel houses, reserve below the roof eave (58% of full height). For towers match floor elevations to floors parameter. Do not allocate furniture in the macro plan for these rooms. Use 2-6 rooms for a moderate building, never silently omit requested rooms. Room interiors receive dedicated renders. No room is needed for terrain, bridges, ornaments or explicitly exterior-only structures.");
    contract["creative_direction"]=serde_json::json!("Let the prompt's design intention determine topology, silhouette, asymmetry, scale rhythm, palette, landscape and focal spaces. Avoid repeatedly using identical rooms, evenly spaced props, matching towers, compulsory symmetry or a standard platform. Use supported ring/arcade/bridge/garden recipes for curved visual rhythms, irregular massing and landscape-led layouts when appropriate. Furnishable room_shell envelopes remain rectangular: do not pretend they can make arbitrary organic occupied volumes. Architectural precision is needed at support/contact/access points, not everywhere in the composition.");
    contract
}

fn base_compact_design_contract(prompt: &str) -> serde_json::Value {
    let voxel = prompt.to_lowercase().contains("minecraft") || prompt.to_lowercase().contains("voxel");
    serde_json::json!({"prompt":prompt,"object_schema":{"name":"unique semantic name","recipe":"one supported recipe","size":"[positive X width, Y depth, Z height] in metres <=100","position":"[world X centre, world Y centre, bottom Z]","rotation":"optional [X,Y,Z] degrees; keep zero for Minecraft","color":"[R,G,B] numbers 0..1","roughness":"number 0..1","finish":"stone/wood/matte/glass/metal/water/emissive","support":"optional exact name of supporting object; controller computes contact height","connects":"for bridge or voxel_bridge: [exact first landing name, exact second landing name]; controller computes endpoints, span and slope. gallery/room_shell landings use floor elevation; terrain/slab landings use their top","parameters":{"biome":"snow/desert/jungle","block_size":"optional number .3..1.2","roof_color":"optional [R,G,B]","roof_style":"gable or flat","tree_type":"pine, palm or broadleaf","snow":"optional boolean","floors":"optional integer 1..6","levels":"optional integer 3..12"}},"required_output":{"description":"specific design intent based only on this prompt","objects":[]},"recipes":if voxel {VOXEL_RECIPES} else {COMPACT_RECIPES},"rules":"Design a coherent, distinctive scene, NOT a catalogue of all recipes. No fixed motif or preset scene layout. Choose 12-28 functional assemblies with a clear focal structure, secondary masses, approach and terrain. Each assembly recipe builds its own small details. Put most detail where visible from negative Y. Unique names. Support references must exist and be acyclic. Children supported by terrain must fit inside 90% of its footprint and must not overlap each other except intended wall/tower joints. Keep openings and access paths clear. All connected island tops must differ by less than half their horizontal gap. Bridges connect actual island edges, never arbitrary points. For voxel castles use terrain, gatehouse, towers, walls, houses and trees, with joined walls and an accessible courtyard. For desert scenes use a stepped pyramid with built-in central stairs and shrine, flat-roofed houses, palms and canyon terrain. For floating villages use separate terrain islands with houses and trees on top, connected bridges, falling water. A waterfall position is its BOTTOM, not its source height. Minecraft shapes are axis aligned and solid matte block construction, never glazed museum recipes. Terrain recipe produces a rugged block cliff with a flat usable upper platform. terrain size Z is cliff depth. Supply a biome parameter for every voxel object. The same scene's objects must have consistent metre/block scale. Return only complete JSON, no Markdown."})
}

#[derive(Clone, Deserialize, Serialize)]
#[serde(deny_unknown_fields)]
struct CompactLayout {
    #[serde(default)]
    description: String,
    objects: Vec<CompactObject>,
}

#[derive(Deserialize)]
struct CompactRepair {
    #[serde(default)]
    updates: Vec<BTreeMap<String, serde_json::Value>>,
    #[serde(default)]
    add: Vec<CompactObject>,
    // Some models ignore the diff contract and resend the whole layout. Accept
    // it only as a bounded local diff so an unrelated rewrite cannot silently
    // double the scene, drop authored parts or reintroduce removed errors.
    #[serde(default)]
    objects: Option<Vec<CompactObject>>,
    #[serde(default)]
    #[allow(dead_code)]
    description: Option<String>,
}

/// Reduce a model's whole-layout response to the minimal set of targeted field
/// changes. Unchanged objects are preserved verbatim; deletions are refused.
fn diff_full_layout(draft: &CompactLayout, replacement: &[CompactObject]) -> Result<CompactRepair> {
    let before: HashMap<&str, &CompactObject> = draft.objects.iter().map(|o| (o.name.as_str(), o)).collect();
    let mut updates = Vec::new();
    let mut add = Vec::new();
    let mut seen = HashSet::new();
    for object in replacement {
        if !seen.insert(object.name.clone()) {
            return Err(SceneError::Validation(format!("repair returns duplicate object {}", object.name)));
        }
        match before.get(object.name.as_str()) {
            Some(previous) => {
                let old = serde_json::to_value(previous).map_err(storage)?;
                let new = serde_json::to_value(object).map_err(storage)?;
                let mut changed = BTreeMap::new();
                for field in ["size", "position", "rotation", "support", "connects", "parameters", "rooms"] {
                    if old.get(field) != new.get(field) {
                        changed.insert(field.to_string(), new.get(field).cloned().unwrap_or(serde_json::Value::Null));
                    }
                }
                if !changed.is_empty() {
                    changed.insert("name".into(), serde_json::json!(object.name));
                    updates.push(changed);
                }
            }
            None => add.push(object.clone()),
        }
    }
    for previous in &draft.objects {
        if !seen.contains(&previous.name) {
            return Err(SceneError::Validation(format!(
                "repair would drop {}; corrections must preserve every existing object",
                previous.name
            )));
        }
    }
    Ok(CompactRepair { updates, add, objects: None, description: None })
}

fn apply_compact_repair(layout: &CompactLayout, mut repair: CompactRepair) -> Result<CompactLayout> {
    if let Some(objects) = repair.objects.take() {
        if !repair.updates.is_empty() || !repair.add.is_empty() {
            return Err(SceneError::Validation("layout repair must not mix a full layout with targeted updates".into()));
        }
        repair = diff_full_layout(layout, &objects)?;
    }
    if repair.add.len() > 4 {
        return Err(SceneError::Validation("layout repair may add at most four necessary parts".into()));
    }
    let mut result = layout.clone();
    let mut updated = HashSet::new();
    for mut fields in repair.updates {
        let name = fields.remove("name").and_then(|v| v.as_str().map(str::to_owned))
            .ok_or_else(|| SceneError::Validation("repair update requires an exact object name".into()))?;
        if !updated.insert(name.clone()) {
            return Err(SceneError::Validation(format!("duplicate repair update: {name}")));
        }
        let object = result.objects.iter_mut().find(|o| o.name == name)
            .ok_or_else(|| SceneError::Validation(format!("unknown repair object: {name}")))?;
        let mut value = serde_json::to_value(&*object).map_err(storage)?;
        for (key, val) in fields {
            if !["size", "position", "rotation", "support", "connects", "parameters", "rooms"].contains(&key.as_str()) {
                return Err(SceneError::Validation(format!("repair cannot change {key} on {name}")));
            }
            value[&key] = val;
        }
        let replacement: CompactObject = serde_json::from_value(value)
            .map_err(|e| SceneError::Validation(format!("repair schema for {name}: {e}")))?;
        if object.rooms.iter().any(|room| !replacement.rooms.iter().any(|r| r.name == room.name)) {
            return Err(SceneError::Validation(format!("repair cannot omit occupied rooms in {name}")));
        }
        *object = replacement;
    }
    let mut names = result.objects.iter().map(|o| o.name.clone()).collect::<HashSet<_>>();
    for object in repair.add {
        if !names.insert(object.name.clone()) {
            return Err(SceneError::Validation(format!("repair adds duplicate object {}", object.name)));
        }
        result.objects.push(object);
    }
    if serde_json::to_value(&result).map_err(storage)? == serde_json::to_value(layout).map_err(storage)? {
        return Err(SceneError::Validation("layout repair made no changes; refusing an identical correction".into()));
    }
    // The caller validates the entire result, including unchanged neighbors,
    // before any component generation. Patching does not bypass acceptance.
    Ok(result)
}

#[derive(Clone, Deserialize, Serialize, Default)]
#[serde(deny_unknown_fields)]
struct CompactObject {
    #[serde(default)]
    habitable: Option<bool>,
    #[serde(default)]
    rooms: Vec<RoomBrief>,
    #[serde(default)]
    support: Option<String>,
    #[serde(default)]
    connects: Option<[String; 2]>,
    #[serde(default)]
    parameters: BTreeMap<String, serde_json::Value>,
    #[serde(default, deserialize_with="deserialize_compact_rotation")]
    rotation: [f64; 3],
    #[serde(default)]
    finish: String,
    name: String,
    recipe: String,
    size: [f64; 3],
    position: [f64; 3],
    #[serde(default="compact_default_color")]
    color: [f64; 3],
    #[serde(default="compact_default_roughness")]
    roughness: f64,
}

fn compact_default_color() -> [f64;3] { [0.5;3] }
fn compact_default_roughness() -> f64 { 0.6 }

fn deserialize_compact_rotation<'de,D:serde::Deserializer<'de>>(d:D)->std::result::Result<[f64;3],D::Error> {
    let values=Vec::<f64>::deserialize(d)?;
    if values.iter().any(|v|!v.is_finite()) {return Err(serde::de::Error::custom("rotation must be finite"));}
    match values.as_slice() {
        [x,y,z]=>Ok([*x,*y,*z]),
        [x,y,z,w] if (x*x+y*y+z*z+w*w-1.0).abs()<1e-4=>Ok([
            (2.0*(w*x+y*z)).atan2(1.0-2.0*(x*x+y*y)).to_degrees(),
            (2.0*(w*y-z*x)).clamp(-1.0,1.0).asin().to_degrees(),
            (2.0*(w*z+x*y)).atan2(1.0-2.0*(y*y+z*z)).to_degrees()]),
        _=>Err(serde::de::Error::custom("rotation requires three Euler degrees or a normalized [x,y,z,w] quaternion")),
    }
}

fn resolve_layout_connections(layout: &mut CompactLayout) -> Result<()> {
    let mut done = HashSet::new();
    let mut names = HashSet::new();
    for object in &layout.objects {
        if object.size.iter().any(|v| !v.is_finite() || *v <= 0.0 || *v > 100.0)
            || object.position.iter().chain(object.rotation.iter()).any(|v| !v.is_finite()) {
            return Err(SceneError::Validation(format!("invalid layout envelope: {}", object.name)));
        }
        if !names.insert(object.name.clone()) {
            return Err(SceneError::Validation(format!("duplicate layout name: {}", object.name)));
        }
        if object.parameters.keys().any(|key| !["biome","block_size","roof_color","roof_style","tree_type","snow","floors","levels"].contains(&key.as_str())) {
            return Err(SceneError::Validation(format!("unsupported construction parameter on {}", object.name)));
        }
    }
    // Dimensions are independent of contact-height resolution. Report every
    // invalid support at once so a correction does not just uncover the next.
    let mut errors = Vec::new();
    for object in &layout.objects {
        if let Some(name) = &object.support {
            if let Some(parent) = layout.objects.iter().find(|p| &p.name == name) {
                if parent.connects.is_some() { continue; } // bridge size is resolved below
                let factor = if object.recipe == "room_shell" { 1.0 } else { 0.9 };
                for axis in 0..2 {
                    if object.size[axis] > parent.size[axis] * factor {
                        errors.push(format!("{} is larger than its support {} on axis {}: child {:.2}m, maximum {:.2}m", object.name, name, axis, object.size[axis], parent.size[axis] * factor));
                    }
                }
            }
        }
    }
    if !errors.is_empty() {
        return Err(SceneError::Validation(format!("{}. support means wholly ON TOP, not enclosing/side-attached. Resize the parts or explicitly position surrounding/overhanging parts without this support reference", errors.join("; "))));
    }
    for _ in 0..=layout.objects.len() {
        for i in 0..layout.objects.len() {
            if done.contains(&i) { continue; }
            if let Some(support) = layout.objects[i].support.clone() {
                let j = layout.objects.iter().position(|o| o.name == support)
                    .ok_or_else(|| SceneError::Validation(format!("missing support {support}")))?;
                if !done.contains(&j) { continue; }
                let parent = &layout.objects[j];
                let top = parent.position[2] + parent.size[2];
                let center = parent.position;
                let size = parent.size;
                let object = &mut layout.objects[i];
                for axis in 0..2 {
                    // Attached room modules may share an edge. Shrinking their
                    // placements independently would make neighbouring rooms overlap.
                    let available = size[axis] * if object.recipe=="room_shell" {0.5} else {0.45} - object.size[axis] * 0.5;
                    if available < 0.0 { return Err(SceneError::Validation(format!("{} is larger than its support {support} on axis {axis}: child {:.2}m, maximum {:.2}m. support means wholly ON TOP and overrides Z, not enclosing or side-attached. Resize/move the child or use explicit placement without this support for a surrounding/overhanging part", object.name, object.size[axis], size[axis] * if object.recipe=="room_shell" {1.0} else {0.9}))); }
                    object.position[axis] = object.position[axis].clamp(center[axis]-available, center[axis]+available);
                }
                object.position[2] = top;
                if object.recipe == "voxel_waterfall" && object.size[2] > 1.5 {
                    // Water hangs DOWN from its named source, unlike a house.
                    object.position[2] = top-object.size[2];
                    let dx=(object.position[0]-center[0])/size[0];
                    let dy=(object.position[1]-center[1])/size[1];
                    let axis=if dx.abs()>dy.abs() {0} else {1};
                    let direction=if object.position[axis]>center[axis] {1.0} else {-1.0};
                    object.position[axis]=center[axis]+direction*(size[axis]*0.5+object.size[axis]*0.4);
                }
            }
            if let Some(ends) = layout.objects[i].connects.clone() {
                if !["voxel_bridge", "bridge"].contains(&layout.objects[i].recipe.as_str()) || ends[0] == ends[1] {
                    return Err(SceneError::Validation("connects requires a bridge and two different supports".into()));
                }
                if layout.objects[i].support.is_some() {
                    return Err(SceneError::Validation("a connected bridge uses connects, not a separate support".into()));
                }
                let indices: Vec<_> = ends.iter().map(|name| layout.objects.iter().position(|o| &o.name == name)
                    .ok_or_else(|| SceneError::Validation(format!("missing bridge landing {name}")))).collect::<Result<_>>()?;
                if indices.iter().any(|j| !done.contains(j)) { continue; }
                let first = &layout.objects[indices[0]]; let second = &layout.objects[indices[1]];
                let dx = second.position[0]-first.position[0]; let dy = second.position[1]-first.position[1];
                let distance = dx.hypot(dy);
                if distance < 0.1 { return Err(SceneError::Validation(format!("bridge landings coincide: {} and {}. connects cannot span concentric/nested structures; allocate separate entrance landing pads", first.name, second.name))); }
                let ux = dx/distance; let uy = dy/distance;
                if [first, second].iter().any(|o| o.rotation[0].abs()>0.001 || o.rotation[1].abs()>0.001) {
                    return Err(SceneError::Validation("bridge landings must be horizontal (yaw rotation is supported)".into()));
                }
                let edge = |o: &CompactObject| {
                    // Intersect in the landing's local frame with deck overlap.
                    let yaw=o.rotation[2].to_radians();
                    let x=ux*yaw.cos()+uy*yaw.sin();
                    let y=-ux*yaw.sin()+uy*yaw.cos();
                    (if x.abs()>1e-9 {o.size[0]*0.43/x.abs()} else {f64::INFINITY})
                        .min(if y.abs()>1e-9 {o.size[1]*0.43/y.abs()} else {f64::INFINITY})
                };
                let e1=edge(first);let e2=edge(second);
                let length=distance-e1-e2;
                if length < 0.3 { return Err(SceneError::Validation(format!("bridge {} supports overlap: {} and {} have centre distance {distance:.2}m but projected landing extents sum to {:.2}m. Separate their centres by at least {:.2}m or use smaller dedicated entrance pads; do not connect enclosing/nested platforms", layout.objects[i].name, first.name, second.name, e1+e2, e1+e2+0.3))); }
                let landing_z = |o: &CompactObject| o.position[2] + match o.recipe.as_str() {
                    "gallery" | "room_shell" => 0.18_f64.min(o.size[2]*0.12),
                    _ => o.size[2],
                };
                let z1=landing_z(first); let z2=landing_z(second);
                let rise=z2-z1;
                if rise.abs()>length*0.5 {
                    return Err(SceneError::Validation(format!("bridge {} is too steep: rise {rise:.1}m over {length:.1}m. Reduce island height differences or increase horizontal separation", layout.objects[i].name)));
                }
                let center=[(first.position[0]+ux*e1+second.position[0]-ux*e2)/2.0,
                            (first.position[1]+uy*e1+second.position[1]-uy*e2)/2.0,z1.min(z2)];
                let bridge=&mut layout.objects[i];
                bridge.position=center; bridge.size=[length, bridge.size[1].clamp(1.5,4.0),rise.abs()+2.2];
                if bridge.size.iter().any(|v| !v.is_finite() || *v>100.0) || center.iter().any(|v| !v.is_finite()) {
                    return Err(SceneError::Validation("resolved bridge exceeds the finite 100m envelope limit".into()));
                }
                bridge.rotation=[0.0,0.0,dy.atan2(dx).to_degrees()];
                bridge.parameters.insert("rise".into(),serde_json::json!(rise));
                bridge.parameters.insert("start_height".into(),serde_json::json!(z1-z1.min(z2)));
            }
            done.insert(i);
        }
        if done.len()==layout.objects.len() { return Ok(()); }
    }
    Err(SceneError::Validation("support/bridge references form a cycle".into()))
}

fn expand_compact_layout(mut layout: CompactLayout) -> Result<FrontierProposal> {
    resolve_layout_connections(&mut layout)?;
    if layout.objects.is_empty() || layout.objects.len() > 64 {
        return Err(SceneError::Validation("compact layout needs 1-64 objects".into()));
    }
    let mut root: SceneNode = serde_json::from_value(contract_example_node()).map_err(storage)?;
    root.kind = if layout.objects.iter().any(|o| o.recipe.starts_with("voxel_")) { "voxel_scene" } else { "building_scene" }.into();
    root.purpose = layout.description.clone();
    root.design = layout.description.clone();
    root.construction_instructions = "Assemble the model-authored recipe layout".into();
    root.required_features.clear();
    root.assumptions = vec!["Compact recipe layout; no automatic image review implied".into()];
    root.construction.clear();
    root.acceptance.numeric_rules.clear();
    root.acceptance.visual_requirements = vec!["coherent furnished scene".into()];
    root.generation.strategy = "assembly".into();
    root.generation.recipe = "room".into();
    root.bounds_local = Bounds { min: [f64::INFINITY; 3], max: [f64::NEG_INFINITY; 3] };
    let mut children = Vec::new();
    for (i, object) in layout.objects.into_iter().enumerate() {
        let rooms=allocated_rooms(&object)?;
        if !(COMPACT_RECIPES.contains(&object.recipe.as_str()) || VOXEL_RECIPES.contains(&object.recipe.as_str()))
            || object.size.iter().any(|v| !v.is_finite() || *v <= 0.0 || *v > 100.0)
            || object.position.iter().any(|v| !v.is_finite())
            || object.rotation.iter().any(|v| !v.is_finite())
            || object.color.iter().any(|v| !(0.0..=1.0).contains(v))
            || !(0.0..=1.0).contains(&object.roughness) {
            return Err(SceneError::Validation(format!("invalid compact object {}", object.name)));
        }
        let mut node = root.clone();
        node.node_id = format!("object-{:03}", i + 1);
        node.parent_id = Some("scene".into());
        node.kind = object.recipe.clone();
        node.purpose = object.name.clone();
        node.design = object.name;
        node.construction_instructions = format!("Build {} to the supplied metric bounds", object.recipe);
        node.transform.translation = object.position;
        let [rx, ry, rz] = object.rotation.map(|v| v.to_radians() / 2.0);
        let (sx,cx) = rx.sin_cos(); let (sy,cy) = ry.sin_cos(); let (sz,cz) = rz.sin_cos();
        node.transform.rotation_xyzw = [sx*cy*cz-cx*sy*sz, cx*sy*cz+sx*cy*sz, cx*cy*sz-sx*sy*cz, cx*cy*cz+sx*sy*sz];
        node.bounds_local = Bounds { min: [-object.size[0]/2.0, -object.size[1]/2.0, 0.0], max: [object.size[0]/2.0, object.size[1]/2.0, object.size[2]] };
        node.generation.strategy = "recipe".into();
        // The generic terrain alias currently uses the bounded block-cliff
        // generator; it does not promise smooth or photorealistic geology.
        node.generation.recipe = if object.recipe == "terrain" { "voxel_terrain".into() } else { object.recipe };
        node.generation.parameters = object.parameters;
        if !rooms.is_empty() { node.generation.parameters.insert("rooms".into(),serde_json::to_value(&rooms).map_err(storage)?); }
        // Different assemblies need their own construction/material decisions.
        // Repeated instances may still be reused explicitly within this run.
        node.generation.parameters.insert("force_regenerate".into(), serde_json::json!(true));
        node.materials[0].material_id = format!("material-{:03}", i + 1);
        node.materials[0].description = format!("{} [finish:{}]", node.design, object.finish);
        node.materials[0].metallic = if object.finish == "metal" { 0.85 } else { 0.0 };
        node.materials[0].base_color = [object.color[0], object.color[1], object.color[2], 1.0];
        node.materials[0].roughness = object.roughness;
        for corner in node.bounds_local.corners().map(|p| node.transform.point(p)) {
          for axis in 0..3 {
            root.bounds_local.min[axis] = root.bounds_local.min[axis].min(corner[axis]);
            root.bounds_local.max[axis] = root.bounds_local.max[axis].max(corner[axis]);
          }
        }
        // Each child owns no descendants; root ownership is filled after expansion.
        node.child_ids.clear();
        for (j,brief) in rooms.into_iter().enumerate() {
            let mut room=node.clone();
            room.node_id=format!("{}-room-{:02}",node.node_id,j+1);
            room.parent_id=Some(node.node_id.clone());
            room.kind="furnished_room".into();room.purpose=brief.purpose.clone();room.design=brief.name.clone();
            room.transform=Transform {translation:brief.position,..Default::default()};
            room.bounds_local=Bounds {min:[-brief.size[0]/2.0,-brief.size[1]/2.0,0.0],max:[brief.size[0]/2.0,brief.size[1]/2.0,brief.size[2]]};
            room.generation.strategy="interior".into();room.generation.recipe="room".into();room.generation.parameters.clear();
            room.generation.parameters.insert("circulation".into(),serde_json::to_value(brief.circulation).map_err(storage)?);
            room.child_ids.clear();room.child_regions.clear();room.placement_region=None;
            node.child_ids.push(room.node_id.clone());children.push(room);
        }
        children.push(node);
    }
    root.child_ids = children.iter().filter(|n|n.parent_id.as_deref()==Some("scene")).map(|n| n.node_id.clone()).collect();
    Ok(FrontierProposal { parent: root, children, markdown: layout.description })
}

#[cfg(test)]
mod compact_tests {
    use super::*;

    #[test]
    fn compact_repairs_preserve_unaffected_objects_and_reject_destructive_edits() {
        let layout = CompactLayout {description:"keep design".into(),objects:vec![
            CompactObject {name:"core".into(),recipe:"room_shell".into(),size:[10.0,10.0,4.0],..Default::default()},
            CompactObject {name:"roof".into(),recipe:"slab".into(),size:[12.0,12.0,1.0],support:Some("core".into()),..Default::default()},
        ]};
        let repair = serde_json::from_value(serde_json::json!({"updates":[{"name":"roof","support":null,"position":[0,0,4]}]})).unwrap();
        let fixed = apply_compact_repair(&layout, repair).unwrap();
        assert_eq!(serde_json::to_value(&fixed.objects[0]).unwrap(),serde_json::to_value(&layout.objects[0]).unwrap());
        assert_eq!(fixed.objects[1].size,layout.objects[1].size);
        assert!(fixed.objects[1].support.is_none());
        validate_compact_draft(fixed, &SceneConfig::default()).unwrap();
        for update in [serde_json::json!({"name":"missing","size":[1,1,1]}),
                       serde_json::json!({"name":"core","recipe":"slab"}),
                       serde_json::json!({"name":"core","habitable":false}),
                       serde_json::json!({"name":"core"})] {
            let repair = serde_json::from_value(serde_json::json!({"updates":[update]})).unwrap();
            assert!(apply_compact_repair(&layout, repair).is_err());
        }
    }

    #[test]
    fn whole_layout_repairs_are_reduced_to_bounded_targeted_updates() {
        let layout = CompactLayout {description:"citadel".into(),objects:vec![
            CompactObject {name:"core".into(),recipe:"room_shell".into(),size:[10.0,10.0,4.0],position:[0.0,0.0,0.0],..Default::default()},
            CompactObject {name:"roof".into(),recipe:"slab".into(),size:[12.0,12.0,1.0],position:[0.0,0.0,4.0],support:Some("core".into()),..Default::default()},
            CompactObject {name:"gallery".into(),recipe:"gallery".into(),size:[8.0,8.0,3.0],position:[0.0,0.0,0.0],..Default::default()},
        ]};
        // A model that ignores the diff contract and resends the whole layout,
        // changing only the roof, must not replace or resize the untouched parts.
        let full = serde_json::json!({"description":"rewritten","objects":[
            {"name":"core","recipe":"room_shell","size":[10.0,10.0,4.0],"position":[0.0,0.0,0.0]},
            {"name":"roof","recipe":"slab","size":[8.0,8.0,1.0],"position":[0.0,0.0,4.0],"support":"core"},
            {"name":"gallery","recipe":"gallery","size":[8.0,8.0,3.0],"position":[0.0,0.0,0.0]}
        ]});
        let repair: CompactRepair = serde_json::from_value(full).unwrap();
        let fixed = apply_compact_repair(&layout, repair).unwrap();
        assert_eq!(fixed.objects.len(), layout.objects.len());
        assert_eq!(serde_json::to_value(&fixed.objects[0]).unwrap(), serde_json::to_value(&layout.objects[0]).unwrap());
        assert_eq!(fixed.objects[1].size, [8.0,8.0,1.0]);
        assert_eq!(fixed.objects[1].support, Some("core".into()));
        assert_eq!(serde_json::to_value(&fixed.objects[2]).unwrap(), serde_json::to_value(&layout.objects[2]).unwrap());
        // Dropping an authored object is refused, as is mixing both shapes.
        let dropped = serde_json::json!({"objects":[
            {"name":"core","recipe":"room_shell","size":[10.0,10.0,4.0],"position":[0.0,0.0,0.0]},
            {"name":"roof","recipe":"slab","size":[8.0,8.0,1.0],"position":[0.0,0.0,4.0]}
        ]});
        assert!(apply_compact_repair(&layout, serde_json::from_value(dropped).unwrap()).is_err());
        let mixed = serde_json::json!({"updates":[{"name":"roof","size":[8.0,8.0,1.0]}],
            "objects":[{"name":"roof","recipe":"slab","size":[8.0,8.0,1.0],"position":[0.0,0.0,4.0]}]});
        assert!(apply_compact_repair(&layout, serde_json::from_value(mixed).unwrap()).is_err());
        // A whole layout that grows by more than four parts is still bounded.
        let mut grown = serde_json::json!({"objects":[]});
        for i in 0..6 {
            grown["objects"].as_array_mut().unwrap().push(serde_json::json!({"name":format!("pad{i}"),"recipe":"slab","size":[1.0,1.0,0.2],"position":[i as f64,0.0,0.0]}));
        }
        grown["objects"].as_array_mut().unwrap().push(serde_json::json!({"name":"core","recipe":"room_shell","size":[10.0,10.0,4.0],"position":[0.0,0.0,0.0]}));
        grown["objects"].as_array_mut().unwrap().push(serde_json::json!({"name":"roof","recipe":"slab","size":[12.0,12.0,1.0],"position":[0.0,0.0,4.0],"support":"core"}));
        grown["objects"].as_array_mut().unwrap().push(serde_json::json!({"name":"gallery","recipe":"gallery","size":[8.0,8.0,3.0],"position":[0.0,0.0,0.0]}));
        assert!(apply_compact_repair(&layout, serde_json::from_value(grown).unwrap()).is_err());
    }

    #[test]
    fn support_diagnostics_report_all_oversized_parts_without_relaxing_fit() {
        let mut plan = CompactLayout { description:"citadel".into(), objects:vec![
            CompactObject { name:"core".into(), recipe:"slab".into(), size:[10.0,10.0,4.0], ..Default::default() },
            CompactObject { name:"roof".into(), recipe:"slab".into(), size:[12.0,12.0,1.0], support:Some("core".into()), ..Default::default() },
            CompactObject { name:"surrounding_ramp".into(), recipe:"gallery".into(), size:[14.0,14.0,3.0], support:Some("core".into()), ..Default::default() },
        ]};
        let error = resolve_layout_connections(&mut plan).unwrap_err().to_string();
        assert!(error.contains("roof") && error.contains("surrounding_ramp") && error.contains("maximum 9.00m"));
    }

    #[test]
    fn tabletop_props_are_separated_without_removing_or_resizing_them() {
        let proposal = expand_compact_layout(CompactLayout {description:"room".into(), objects:vec![
            CompactObject {name:"room".into(),recipe:"room_shell".into(),size:[10.0,10.0,4.0],..Default::default()}
        ]}).unwrap();
        let room = proposal.children.iter().find(|n|n.kind=="furnished_room").unwrap();
        let mut objects = vec![CompactObject {name:"table".into(),recipe:"table".into(),size:[2.0,2.0,0.8],position:[-2.0,-2.0,0.0],..Default::default()}];
        for i in 0..3 {
            objects.push(CompactObject {name:format!("chair{i}"),recipe:"chair".into(),size:[0.5,0.5,0.8],position:[2.0,-2.0+i as f64,0.0],..Default::default()});
        }
        for i in 0..2 {
            objects.push(CompactObject {name:format!("offering{i}"),recipe:"light".into(),size:[0.3,0.3,0.3],position:[-2.0,-2.0,0.8],support:Some("table".into()),..Default::default()});
        }
        let parts = room_furniture(room, CompactLayout {description:"offerings".into(),objects}).unwrap();
        assert_eq!(parts.len(),6);
        assert_ne!(parts[4].transform.translation,parts[5].transform.translation);
        assert_eq!(parts[4].bounds_local.size(),parts[5].bounds_local.size());
    }

    #[test]
    fn circulation_options_allow_distinct_floor_compositions() {
        let proposal=expand_compact_layout(CompactLayout {description:"room".into(),objects:vec![CompactObject {
            name:"exhibit".into(),recipe:"room_shell".into(),size:[8.0,8.0,3.6],..Default::default()
        }]}).unwrap();
        let mut room=proposal.children.iter().find(|n|n.kind=="furnished_room").unwrap().clone();
        let centre=Bounds {min:[-0.5,-0.5,0.0],max:[0.5,0.5,1.0]};
        let side=Bounds {min:[-0.5,1.0,0.0],max:[0.5,2.0,1.0]};
        assert!(circulation_blocked(&room,&centre));
        assert!(circulation_blocked(&room,&side));
        room.generation.parameters.insert("circulation".into(),serde_json::json!("spine_x"));
        assert!(circulation_blocked(&room,&centre));
        assert!(!circulation_blocked(&room,&side));
        room.generation.parameters.insert("circulation".into(),serde_json::json!("perimeter"));
        assert!(!circulation_blocked(&room,&centre));
        assert!(circulation_blocked(&room,&Bounds {min:[3.0,0.0,0.0],max:[3.5,0.5,1.0]}));
        let mut layout=CompactLayout {description:"centre feature".into(),objects:vec![CompactObject {
            name:"central table".into(),recipe:"table".into(),size:[2.5,1.8,1.0],..Default::default()
        }]};
        fit_room_layout(&room,&mut layout).unwrap();
        assert_eq!(layout.objects[0].position,[0.0,0.0,0.0]);
        let legacy:RoomBrief=serde_json::from_value(serde_json::json!({"name":"old","purpose":"legacy","size":[5,5,3],"position":[0,0,0]})).unwrap();
        assert!(matches!(legacy.circulation,Circulation::Cross));
        assert!(serde_json::from_value::<RoomBrief>(serde_json::json!({"name":"bad","purpose":"bad","size":[5,5,3],"position":[0,0,0],"circulation":"anything"})).is_err());
    }

    #[test]
    fn previews_publish_before_acceptance_and_can_be_backfilled() {
        let dir=tempfile::tempdir().unwrap();
        let source=dir.path().join("interior.png");
        std::fs::write(&source,b"render fixture").unwrap();
        publish_previews(dir.path(),&[source.clone()]).unwrap();
        publish_previews(dir.path(),&[source]).unwrap();
        assert_eq!(std::fs::read(dir.path().join("previews/interior.png")).unwrap(),b"render fixture");
    }

    #[test]
    fn component_cache_ignores_output_metadata_but_not_geometry() {
        let proposal=expand_compact_layout(CompactLayout {
            description:"cache fixture".into(),
            objects:vec![CompactObject {name:"Desk".into(),recipe:"desk".into(),size:[1.5,0.8,1.0],..Default::default()}],
        }).unwrap();
        let mut node=proposal.children[0].clone();
        let hash=component_input_hash(&node).unwrap();
        node.revision+=1;
        node.generation.parameters.insert("component_glb_path".into(),serde_json::json!("output.glb"));
        assert_eq!(hash,component_input_hash(&node).unwrap());
        node.generation.parameters.insert("detail".into(),serde_json::json!("changed"));
        assert_ne!(hash,component_input_hash(&node).unwrap());
    }

    #[test]
    fn compact_appearance_defaults_do_not_relax_geometry_schema() {
        let mut value=serde_json::json!({"name":"slab","recipe":"slab","size":[3,3,0.2],"position":[0,0,0]});
        let object:CompactObject=serde_json::from_value(value.clone()).unwrap();
        assert_eq!(object.color,[0.5;3]); assert_eq!(object.roughness,0.6);
        value.as_object_mut().unwrap().remove("size");
        assert!(serde_json::from_value::<CompactObject>(value).is_err());
    }

    #[test]
    fn compact_checkpoint_keys_inputs_preserves_provenance_and_does_not_recharge() {
        let dir=tempfile::tempdir().unwrap(); let path=dir.path().join("draft.json");
        let mut task:SceneTask=serde_json::from_value(serde_json::json!({"id":"museum","prompt":"A museum","seed":7})).unwrap();
        let config=SceneConfig::default();
        let hash=compact_draft_input_hash(&task,&config).unwrap();
        task.seed=Some(8); assert_ne!(hash,compact_draft_input_hash(&task,&config).unwrap());
        task.seed=Some(7);task.prompt.push_str(" different");
        assert_ne!(hash,compact_draft_input_hash(&task,&config).unwrap());
        let mut changed=config.clone(); changed.budgets.max_nodes+=1;
        task.prompt="A museum".into();
        assert_ne!(hash,compact_draft_input_hash(&task,&changed).unwrap());
        assert!(load_compact_draft(&path,&hash).unwrap().is_none());
        let response=crate::roles::RoleResponse {output:CompactLayout {description:"invalid draft still recoverable".into(),objects:vec![]},
            tokens:123,model:"actual-fallback-model".into(),prompt_version:"structure-v1".into()};
        for reviewed in [false,true] {
            save_compact_draft(&path,&hash,reviewed,&response).unwrap();
            assert!(load_compact_draft(&path,"unrelated").unwrap().is_none());
            let cached=load_compact_draft(&path,&hash).unwrap().unwrap();
            assert_eq!(cached.reviewed,reviewed);
            let recovered=cached.into_response();
            assert_eq!(recovered.tokens,0);assert_eq!(recovered.model,response.model);
            assert_eq!(recovered.prompt_version,response.prompt_version);
            assert!(expand_compact_layout(recovered.output).is_err());
        }
        let valid=crate::roles::RoleResponse {output:CompactLayout {description:"valid saved foundation".into(),objects:vec![
            serde_json::from_value(serde_json::json!({"name":"base","recipe":"slab","size":[3,3,0.2],"position":[0,0,0]})).unwrap(),
        ]},tokens:90,model:"original-model".into(),prompt_version:"structure-original".into()};
        save_compact_draft(&path,&hash,true,&valid).unwrap();
        let recovered=load_compact_draft(&path,&hash).unwrap().unwrap().into_response();
        let mut proposal=validate_compact_draft(recovered.output,&config).unwrap();
        stamp_proposal(&mut proposal,&recovered.model,&recovered.prompt_version,7);
        assert_eq!(proposal.parent.provenance.model,"original-model");
        assert_eq!(proposal.children[0].provenance.prompt_version,"structure-original");
        let mut unsupported:serde_json::Value=serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        unsupported["version"]=serde_json::json!(99);
        atomic_write(&path,&serde_json::to_vec(&unsupported).unwrap()).unwrap();
        assert!(load_compact_draft(&path,&hash).unwrap().is_none());
        atomic_write(&path,b"broken checkpoint").unwrap();
        assert!(load_compact_draft(&path,&hash).unwrap().is_none());
    }

    #[tokio::test]
    async fn new_runs_isolate_libraries_and_resume_preserves_scope() {
        let dir = tempfile::tempdir().unwrap();
        let mut config = SceneConfig::default();
        config.storage.output_root = dir.path().join("runs");
        config.storage.asset_library = dir.path().join("shared-library");
        config.storage.min_free_bytes = 0;
        let controller = SceneRunController::new(config.clone(), None);
        let task = SceneTask { id: "same-prompt".into(), enabled: true,
            prompt: "A museum".into(), seed: Some(1), overrides: serde_yaml::Value::Null };
        let first = controller.run_one(&task, config.clone(), None).await.unwrap();
        let second = controller.run_one(&task, config, None).await.unwrap();
        let saved = |root: &Path| -> SceneConfig {
            serde_json::from_slice(&std::fs::read(root.join("input/resolved-config.json")).unwrap()).unwrap()
        };
        let first_config = saved(&first.run_dir);
        let second_config = saved(&second.run_dir);
        assert_ne!(first_config.storage.asset_library, second_config.storage.asset_library);
        assert_eq!(first_config.storage.asset_library, first.run_dir.join("asset-library"));
        let resumed = controller.run_one(&task, first_config.clone(), Some(first.run_dir.clone())).await.unwrap();
        assert_eq!(resumed.run_dir, first.run_dir);
        assert_eq!(saved(&resumed.run_dir).storage.asset_library, first_config.storage.asset_library);
    }

    #[test]
    fn compact_layout_expands_into_valid_owned_scene() {
        let proposal = expand_compact_layout(CompactLayout {
            description: "Table on floor".into(),
            objects: vec![
                CompactObject { rotation: [0.0;3], finish: "stone".into(), name: "Floor".into(), recipe: "slab".into(), size: [5.0,5.0,0.1], position: [0.0;3], color: [0.4;3], roughness: 0.6, ..Default::default() },
                CompactObject { rotation: [0.0,0.0,30.0], finish: "wood".into(), name: "Table".into(), recipe: "table".into(), size: [1.2,0.7,0.75], position: [0.0,0.0,0.1], color: [0.3;3], roughness: 0.4, ..Default::default() },
            ],
        }).unwrap();
        assert_eq!(proposal.parent.child_ids.len(), 2);
        assert!(proposal.children.iter().all(|n| n.child_ids.is_empty()));
        let spec = SceneSpec { schema_version: 1, root_id: "scene".into(), prompt_summary: "test".into(), coordinate_system: "RH_M_ZUP_XEAST_YNORTH".into(), nodes: std::iter::once(proposal.parent).chain(proposal.children).collect() };
        spec.validate(0.005, 0.00001).unwrap();
    }

    #[test]
    fn compact_support_and_bridge_landings_are_resolved() {
        let mut layout = CompactLayout { description: "islands".into(), objects: vec![
            CompactObject { name: "west".into(), recipe: "voxel_terrain".into(), size: [10.0,10.0,4.0], ..Default::default() },
            CompactObject { name: "east".into(), recipe: "voxel_terrain".into(), size: [10.0,10.0,6.0], position: [20.0,0.0,0.0], ..Default::default() },
            CompactObject { name: "house".into(), recipe: "voxel_house".into(), size: [4.0,4.0,5.0], position: [100.0,0.0,99.0], support: Some("west".into()), ..Default::default() },
            CompactObject { name: "bridge".into(), recipe: "voxel_bridge".into(), size: [2.0,2.0,2.0], connects: Some(["west".into(),"east".into()]), ..Default::default() },
        ]};
        resolve_layout_connections(&mut layout).unwrap();
        assert_eq!(layout.objects[2].position, [2.5,0.0,4.0]);
        let bridge = &layout.objects[3];
        assert_eq!(bridge.position, [10.0,0.0,4.0]);
        assert!((bridge.size[0]-11.4).abs()<1e-8);
        assert_eq!(bridge.parameters["rise"], serde_json::json!(2.0));
        layout.objects[0].support = Some("house".into());
        layout.objects.pop();
        assert!(resolve_layout_connections(&mut layout).is_err());
    }

    #[test]
    fn modern_bridges_connect_rotated_landing_floors_and_enforce_bounds() {
        let layout = || CompactLayout {description:"museum bridge".into(), objects:vec![
            CompactObject {name:"a".into(),recipe:"gallery".into(),size:[8.0,4.0,5.0],rotation:[0.0,0.0,90.0],..Default::default()},
            CompactObject {name:"b".into(),recipe:"room_shell".into(),size:[8.0,4.0,8.0],position:[20.0,0.0,2.0],..Default::default()},
            CompactObject {name:"walkway".into(),recipe:"bridge".into(),size:[2.0,2.0,2.0],connects:Some(["a".into(),"b".into()]),..Default::default()},
        ]};
        let mut plan=layout();
        resolve_layout_connections(&mut plan).unwrap();
        let bridge=&plan.objects[2];
        assert!((bridge.size[0]-14.84).abs()<1e-8);
        assert!((bridge.position[0]-9.14).abs()<1e-8);
        assert_eq!(bridge.position[2],0.18);
        assert_eq!(bridge.parameters["rise"],serde_json::json!(2.0));
        let mut plan=layout(); plan.objects[1].position[0]=200.0;
        assert!(resolve_layout_connections(&mut plan).unwrap_err().to_string().contains("100m"));
        let mut plan=layout(); plan.objects[0].rotation[0]=10.0;
        assert!(resolve_layout_connections(&mut plan).unwrap_err().to_string().contains("horizontal"));
    }

    #[test]
    fn generic_terrain_maps_to_bounded_cliff_recipe() {
        let proposal=expand_compact_layout(CompactLayout {description:"cliff".into(),objects:vec![
            CompactObject {name:"cliff".into(),recipe:"terrain".into(),size:[10.0,8.0,3.0],color:[0.5;3],roughness:0.8,..Default::default()},
        ]}).unwrap();
        assert_eq!(proposal.children[0].generation.recipe,"voxel_terrain");
        assert_eq!(proposal.parent.kind,"building_scene");
    }

    #[test]
    fn waterfall_hangs_from_support_and_steep_bridges_fail() {
        let mut layout = CompactLayout { description: "contact rules".into(), objects: vec![
            CompactObject { name: "island".into(), recipe: "voxel_terrain".into(), size: [20.0,20.0,6.0], position: [0.0,0.0,10.0], ..Default::default() },
            CompactObject { name: "water".into(), recipe: "voxel_waterfall".into(), size: [2.0,2.0,12.0], position: [-7.0,0.0,100.0], support: Some("island".into()), ..Default::default() },
        ]};
        resolve_layout_connections(&mut layout).unwrap();
        assert_eq!(layout.objects[1].position, [-10.8,0.0,4.0]);
        layout.objects.push(CompactObject { name: "low".into(), recipe: "voxel_terrain".into(), size: [10.0,10.0,2.0], position: [20.0,0.0,0.0], ..Default::default() });
        layout.objects.push(CompactObject { name: "bridge".into(), recipe: "voxel_bridge".into(), size: [2.0,2.0,2.0], connects: Some(["island".into(),"low".into()]), ..Default::default() });
        assert!(resolve_layout_connections(&mut layout).unwrap_err().to_string().contains("too steep"));
    }

    #[test]
    fn rooms_are_owned_and_furniture_stays_local_under_rotation() {
        let mut proposal=expand_compact_layout(CompactLayout {description:"building".into(),objects:vec![CompactObject {
            name:"library".into(),recipe:"room_shell".into(),size:[8.0,8.0,3.6],position:[10.0,20.0,1.0],rotation:[0.0,0.0,90.0],color:[0.5;3],roughness:0.5,..Default::default()
        }]}).unwrap();
        let room=proposal.children.iter().find(|n|n.kind=="furnished_room").unwrap().clone();
        let furniture=CompactLayout {description:"local furnishing".into(),objects:(0..6).map(|i|CompactObject {
            name:format!("chair{i}"),recipe:"chair".into(),size:[0.5,0.5,0.8],position:[if i<3 {-2.0} else {2.0},1.0+(i%3) as f64,0.0],color:[0.4;3],roughness:0.5,..Default::default()
        }).collect()};
        let parts=room_furniture(&room,furniture.clone()).unwrap();
        let room_node=proposal.children.iter_mut().find(|n|n.node_id==room.node_id).unwrap();
        room_node.generation.strategy="assembly".into();room_node.child_ids=parts.iter().map(|n|n.node_id.clone()).collect();
        let first=parts[0].node_id.clone();proposal.children.extend(parts);
        let spec=SceneSpec {schema_version:1,root_id:"scene".into(),prompt_summary:"test".into(),coordinate_system:"RH_M_ZUP_XEAST_YNORTH".into(),nodes:std::iter::once(proposal.parent).chain(proposal.children).collect()};
        spec.validate(0.005,0.00001).unwrap();
        let world=spec.world_transforms().unwrap()[&first].translation;
        assert!((world[0]-9.0).abs()<1e-6);assert!((world[1]-18.0).abs()<1e-6);
        let mut blocked=furniture.clone();blocked.objects[0].position=[0.0,0.0,0.0];assert!(room_furniture(&room,blocked).unwrap()[0].generation.parameters.contains_key("room_fit_offset_m"));
        let mut overlap=furniture.clone();overlap.objects[1].position=overlap.objects[0].position;assert!(room_furniture(&room,overlap).is_ok());
        let mut oversized=furniture;oversized.objects[0].size=[9.0,9.0,3.0];assert!(room_furniture(&room,oversized).is_err());
        assert!(room_furniture(&room,CompactLayout {description:"empty".into(),objects:vec![]}).is_err());
    }

    #[test]
    fn explicit_rooms_cannot_overlap_or_escape_the_building() {
        let mut object=CompactObject {name:"building".into(),recipe:"room_shell".into(),size:[8.0,8.0,4.0],..Default::default()};
        object.rooms=vec![RoomBrief {name:"a".into(),purpose:"study".into(),size:[5.0,5.0,3.0],position:[0.0,0.0,0.18],circulation:Circulation::Cross}];
        assert!(allocated_rooms(&object).is_ok());
        object.rooms.push(object.rooms[0].clone());assert!(allocated_rooms(&object).is_err());
        object.rooms.pop();object.rooms[0].position[0]=10.0;assert!(allocated_rooms(&object).is_err());
    }

    #[test]
    fn room_height_projection_repairs_offsets_but_not_oversized_support_groups() {
        let mut room:SceneNode=serde_json::from_value(contract_example_node()).unwrap();
        room.bounds_local=Bounds {min:[-5.0,-5.0,1.0],max:[5.0,5.0,6.5]};
        let layout=CompactLayout {description:"height fitting".into(),objects:(0..6).map(|i|CompactObject {
            name:format!("chair{i}"),recipe:"chair".into(),size:[0.5,0.5,0.8],
            position:[if i<3 {-2.0} else {2.0},1.0+(i%3) as f64,1.0],
            rotation:[0.0,0.0,30.0],color:[0.4;3],roughness:0.5,..Default::default()
        }).collect()};
        let mut misplaced=layout.clone();
        misplaced.objects[0].recipe="light".into();misplaced.objects[0].position[2]=6.0;
        misplaced.objects[1].position[2]=-1.0;
        let parts=room_furniture(&room,misplaced).unwrap();
        assert!((parts[0].transform.translation[2]-5.7).abs()<1e-8);
        assert_eq!(parts[1].transform.translation[2],1.0);
        assert!((parts[0].generation.parameters["room_fit_offset_m"][2].as_f64().unwrap()+0.3).abs()<1e-8);
        let mut oversized=layout.clone();oversized.objects[0].size[2]=6.0;
        assert!(room_furniture(&room,oversized).unwrap_err().to_string().contains("taller"));
        let mut supported=layout.clone();
        supported.objects[0].size[2]=2.9;
        supported.objects[1].support=Some("chair0".into());
        supported.objects[1].recipe="light".into();supported.objects[1].size=[0.2,0.2,3.0];
        assert!(room_furniture(&room,supported.clone()).unwrap_err().to_string().contains("exceeds room bounds"));
        // A supported child follows its parent's corrected height; it is never
        // independently dropped off its support to satisfy the ceiling check.
        supported.objects[0].position[2]=-3.0;
        supported.objects[1].size[2]=0.5;
        let parts=room_furniture(&room,supported).unwrap();
        assert_eq!(parts[0].transform.translation[2],1.0);
        assert!((parts[1].transform.translation[2]-3.9).abs()<1e-8);
    }

    #[test]
    fn compact_layout_rejects_empty_output() {
        assert!(expand_compact_layout(CompactLayout { description: "empty".into(), objects: vec![] }).is_err());
    }
}

fn contract_example_node() -> serde_json::Value {
    let spec: SceneSpec = serde_json::from_str(include_str!("../examples/complete-scene.json"))
        .expect("shipped scene contract example is tested");
    serde_json::to_value(&spec.nodes[0]).expect("scene node serializes")
}
fn available_bytes(path: &Path) -> Option<u64> {
    let out = std::process::Command::new("df")
        .args(["-Pk", path.to_str()?])
        .output()
        .ok()?;
    let line = String::from_utf8(out.stdout)
        .ok()?
        .lines()
        .last()?
        .to_string();
    line.split_whitespace()
        .nth(3)?
        .parse::<u64>()
        .ok()
        .map(|k| k * 1024)
}
fn write_execution_plan(root: &Path, spec: &SceneSpec) -> Result<()> {
    use loop_orchestration::planner::{TaskGraph, TaskKind, TaskNode};
    let mut graph = TaskGraph::new();
    for node in &spec.nodes {
        let design = format!("design:{}", node.node_id);
        graph.add_task(TaskNode::new(
            &design,
            TaskKind::Custom {
                worker_type: "scene_design_commit".into(),
                params: serde_json::json!({"node_id":node.node_id,"revision":node.revision}),
            },
            format!("commit design {}@{}", node.node_id, node.revision),
        ));
        if let Some(parent) = &node.parent_id {
            graph.add_dependency(&design, format!("design:{parent}"));
        }
        if node.generation.strategy != "assembly" || node.child_ids.is_empty() {
            let generate = format!("generate:{}", node.node_id);
            graph.add_task(TaskNode::new(
                &generate,
                TaskKind::Custom {
                    worker_type: "scene_component".into(),
                    params: serde_json::json!({"node_id":node.node_id,"revision":node.revision}),
                },
                format!("generate component {}", node.node_id),
            ));
            graph.add_dependency(generate, design);
        }
    }
    graph.add_task(TaskNode::new(
        "assemble:scene",
        TaskKind::Custom {
            worker_type: "scene_assembly".into(),
            params: serde_json::json!({"root_id":spec.root_id}),
        },
        "bottom-up scene assembly",
    ));
    for node in spec
        .nodes
        .iter()
        .filter(|n| n.generation.strategy != "assembly" || n.child_ids.is_empty())
    {
        graph.add_dependency("assemble:scene", format!("generate:{}", node.node_id));
    }
    graph.add_task(TaskNode::new(
        "validate:scene",
        TaskKind::Custom {
            worker_type: "scene_validation".into(),
            params: serde_json::Value::Null,
        },
        "numerical, visual, and cross-format validation",
    ));
    graph.add_dependency("validate:scene", "assemble:scene");
    graph.add_task(TaskNode::new(
        "export:scene",
        TaskKind::Custom {
            worker_type: "scene_export".into(),
            params: serde_json::json!({"formats":["blend","glb"]}),
        },
        "atomic final artifact promotion",
    ));
    graph.add_dependency("export:scene", "validate:scene");
    graph.validate().map_err(SceneError::Validation)?;
    atomic_write(
        &root.join("input/execution-plan.json"),
        &serde_json::to_vec_pretty(&graph).map_err(storage)?,
    )
}
fn validate_component(node: &SceneNode, path: &Path, config: &SceneConfig) -> Result<()> {
    let v: serde_json::Value =
        serde_json::from_slice(&std::fs::read(path).map_err(storage)?).map_err(storage)?;
    if !v["nonempty"].as_bool().unwrap_or(false)
        || v["materials"].as_array().is_none_or(|x| x.is_empty())
    {
        return Err(SceneError::Validation(format!(
            "{} component lacks geometry/materials",
            node.node_id
        )));
    }
    let b = &v["bounds_canonical"];
    let min = b["min"]
        .as_array()
        .ok_or_else(|| SceneError::Validation("inspection bounds missing".into()))?;
    let max = b["max"]
        .as_array()
        .ok_or_else(|| SceneError::Validation("inspection bounds missing".into()))?;
    for i in 0..3 {
        let lo = min[i].as_f64().unwrap_or(f64::NAN);
        let hi = max[i].as_f64().unwrap_or(f64::NAN);
        let tol = config.quality.containment_tolerance_m
            + config.quality.relative_tolerance * node.bounds_local.size()[i];
        if lo < node.bounds_local.min[i] - tol || hi > node.bounds_local.max[i] + tol {
            return Err(SceneError::Validation(format!(
                "{} measured geometry exceeds allocated bounds on axis {i}",
                node.node_id
            )));
        }
    }
    Ok(())
}
fn fixture_node() -> SceneNode {
    SceneNode {
        schema_version: 1,
        node_id: "axis_fixture".into(),
        revision: 1,
        kind: "table".into(),
        parent_id: None,
        child_ids: vec![],
        purpose: "asymmetric doctor fixture".into(),
        required_features: vec!["top".into(), "four legs".into()],
        assumptions: vec![],
        design: "asymmetric 1.2 by 0.7 table".into(),
        construction_instructions: "beveled top and joined legs".into(),
        units: "m".into(),
        transform: Transform::default(),
        pivot_local: [0.0; 3],
        local_front: [0.0, -1.0, 0.0],
        local_up: [0.0, 0.0, 1.0],
        bounds_local: Bounds {
            min: [-0.6, -0.35, 0.0],
            max: [0.6, 0.35, 0.76],
        },
        usable_volume_local: None,
        child_regions: vec![],
        placement_region: None,
        interfaces: vec![],
        relationships: vec![],
        construction: BTreeMap::new(),
        materials: vec![MaterialRef {
            material_id: "oak".into(),
            description: "warm oak".into(),
            base_color: [0.36, 0.18, 0.07, 1.0],
            metallic: 0.0,
            roughness: 0.42,
            texture_scale_m: 0.7,
            texture_ref: None,
        }],
        generation: GenerationStrategy {
            strategy: "recipe".into(),
            recipe: "table".into(),
            parameters: BTreeMap::new(),
            seed: 1,
            detail_policy: "standard".into(),
            required_capabilities: vec!["pbr_materials".into()],
        },
        acceptance: Acceptance {
            numeric_rules: BTreeMap::new(),
            visual_requirements: vec!["credible contact".into()],
            review_views: vec!["three_quarter".into()],
            evidence_refs: vec![],
        },
        provenance: Provenance {
            owning_task: "doctor".into(),
            parent_revision: None,
            dependency_hashes: vec![],
            model: "deterministic".into(),
            prompt_version: "doctor-v1".into(),
            recipe_version: "table-v1".into(),
            source_url: None,
            creator: None,
            license: Some("generated-local".into()),
            validation_status: "proposed".into(),
        },
    }
}
