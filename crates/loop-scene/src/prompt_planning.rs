//! Bounded, resumable concept invention and independent diversity review.
use crate::{
    config::ModelsConfig,
    reporting::Reporter,
    roles::{Role, RoleInvoker},
    storage::{atomic_write, hash_bytes, RunLock},
    Result, SceneError,
};
use serde::{Deserialize, Serialize};
use serde_json::{json, Value};
use std::{
    collections::{BTreeMap, BTreeSet},
    path::Path,
    sync::Arc,
};

/// One named scene concept and its diversity ledger.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PlannedPrompt {
    /// Human-readable unique scene name.
    pub name: String,
    /// Complete standalone scene-generation instruction.
    pub prompt: String,
    /// Semantic design axes, retained across planning chunks.
    #[serde(default)]
    pub diversity: BTreeMap<String, String>,
}
/// A small planning chunk; the durable batch scheduler owns global progress.
#[derive(Debug, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PromptRequest {
    /// User category, preserved in all generated concepts.
    pub category: String,
    /// Exact number of concepts, 1 through 5 per invocation.
    pub count: usize,
    /// Stable creative seed (not a promise of deterministic provider sampling).
    #[serde(default)]
    pub seed: u64,
    /// Category-relative position of this chunk.
    #[serde(default)]
    pub offset: usize,
    /// All previously accepted concepts for duplicate detection.
    #[serde(default)]
    pub existing: Vec<PlannedPrompt>,
    /// Optional user creative constraints.
    #[serde(default)]
    pub briefs: Value,
    /// Outer scheduler redesign attempt; changes creative starting points.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub retry_index: u64,
    /// Optional rejected candidates from a previous invocation.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub previous_candidates: Value,
    /// Concrete prior validation or critic feedback to address.
    #[serde(default, skip_serializing_if = "Value::is_null")]
    pub rejection_feedback: Value,
}
fn is_zero(value: &u64) -> bool {
    *value == 0
}
#[derive(Debug, Serialize, Deserialize)]
struct Candidates {
    prompts: Vec<PlannedPrompt>,
}
#[derive(Deserialize)]
struct Critique {
    approved: bool,
    issues: Vec<String>,
}

fn invalid(message: impl Into<String>) -> SceneError {
    SceneError::Validation(message.into())
}
fn normalized(s: &str) -> String {
    s.chars()
        .filter(|c| c.is_alphanumeric())
        .flat_map(char::to_lowercase)
        .collect()
}
fn words(s: &str) -> BTreeSet<String> {
    s.split(|c: char| !c.is_alphanumeric())
        .filter(|s| s.len() > 3)
        .map(str::to_lowercase)
        .collect()
}
fn validate(request: &PromptRequest, candidates: &Candidates) -> Result<()> {
    if candidates.prompts.len() != request.count {
        return Err(invalid(format!(
            "expected {} prompts, received {}",
            request.count,
            candidates.prompts.len()
        )));
    }
    let mut prior = request.existing.iter().collect::<Vec<_>>();
    for item in &candidates.prompts {
        if item.name.trim().len() < 4
            || item.name.len() > 120
            || item.prompt.len() < 180
            || item.prompt.len() > 6000
        {
            return Err(invalid("each concept needs a short distinct name and a complete 180..6000 character prompt"));
        }
        for axis in [
            "layout",
            "setting",
            "form",
            "materials",
            "atmosphere",
            "circulation",
        ] {
            if item.diversity.get(axis).is_none_or(|s| s.trim().is_empty()) {
                return Err(invalid(format!(
                    "missing diversity axis {axis} in {}",
                    item.name
                )));
            }
        }
        let tokens = words(&item.prompt);
        for old in &prior {
            let old_tokens = words(&old.prompt);
            let similarity = tokens.intersection(&old_tokens).count() as f64
                / tokens.union(&old_tokens).count().max(1) as f64;
            let axes = ["layout", "setting", "form"].iter().all(|axis| {
                item.diversity
                    .get(*axis)
                    .zip(old.diversity.get(*axis))
                    .is_some_and(|(a, b)| normalized(a) == normalized(b))
            });
            if normalized(&item.name) == normalized(&old.name)
                || normalized(&item.prompt) == normalized(&old.prompt)
                || similarity > 0.78
                || axes
            {
                return Err(invalid(format!(
                    "concept {} repeats {}: change topology, setting and silhouette substantively",
                    item.name, old.name
                )));
            }
        }
        prior.push(item);
    }
    Ok(())
}

/// Create an atomic accepted prompt file using separate planner and critic agents.
/// Failed attempts retain raw outputs and provider usage in the adjacent
/// `<output-stem>-planning/run-report.json`; reruns reuse only identical inputs.
pub async fn plan_prompts(
    models: Arc<loop_ai::Models>,
    config: ModelsConfig,
    request: PromptRequest,
    output: &Path,
) -> Result<Value> {
    if request.category.trim().is_empty() || !(1..=5).contains(&request.count) {
        return Err(invalid(
            "category must be nonempty; count must be between 1 and 5",
        ));
    }
    let input_hash = hash_bytes(&serde_json::to_vec(&request).map_err(|e| invalid(e.to_string()))?);
    let stem = output
        .file_stem()
        .and_then(|s| s.to_str())
        .ok_or_else(|| invalid("output needs a filename"))?;
    let root = output.with_file_name(format!("{stem}-planning"));
    std::fs::create_dir_all(&root).map_err(|e| SceneError::Storage(e.to_string()))?;
    let _lock = RunLock::acquire(&root)?;
    if let Ok(bytes) = std::fs::read(output) {
        if let Ok(saved) = serde_json::from_slice::<Value>(&bytes) {
            if saved["request_hash"] == input_hash {
                let candidates: Candidates =
                    serde_json::from_value(saved.clone()).map_err(|e| invalid(e.to_string()))?;
                validate(&request, &candidates)?;
                return Ok(saved);
            }
            return Err(invalid(
                "output exists for a different planning request; choose a new output path",
            ));
        }
    }
    let reporter = Reporter::new(&root);
    let span = reporter.begin(
        "run",
        "prompt_planning",
        json!({"category":request.category,"count":request.count,"request_hash":input_hash}),
    )?;
    let invoker = RoleInvoker::new(models, config).with_reporter(reporter.clone());
    let result = plan_chunk(&invoker, &request, &root).await;
    match result {
        Ok(candidates) => {
            span.finish("accepted", json!({"prompts":candidates.prompts.len()}))?;
            let report: Value = serde_json::from_slice(
                &std::fs::read(root.join("run-report.json"))
                    .map_err(|e| SceneError::Storage(e.to_string()))?,
            )
            .map_err(|e| invalid(e.to_string()))?;
            let value = json!({"version":1,"request_hash":input_hash,"category":request.category,"seed":request.seed,"offset":request.offset,"prompts":candidates.prompts,"telemetry":root.join("run-report.json"),"usage":{"reported_tokens":report["reported_tokens"],"models_used":report["models_used"],"model_attempts":report["model_attempts"],"failed_model_attempts":report["failed_model_attempts"],"elapsed_minutes":report["active_minutes"]}});
            atomic_write(
                output,
                &serde_json::to_vec_pretty(&value).map_err(|e| invalid(e.to_string()))?,
            )?;
            Ok(value)
        }
        Err(error) => {
            span.finish("failed", json!({"error":error.to_string()}))?;
            Err(error)
        }
    }
}

fn compact_ledger(request: &PromptRequest) -> (Vec<Value>, Vec<(String, usize)>) {
    // Full prior prompts are checked locally, but a compact complete concept ledger
    // keeps provider context bounded enough for large category batches.
    let mut selected = BTreeSet::new();
    for i in request.existing.len().saturating_sub(64)..request.existing.len() {
        selected.insert(i);
    }
    for i in 0..64 {
        if !request.existing.is_empty() {
            selected.insert(i * request.existing.len() / 64);
        }
    }
    // Bound descriptive context while giving the critic actual geometry/program,
    // rather than asking it to infer duplication from broad axis labels alone.
    let excerpt_chars = 48_000 / selected.len().max(1);
    let ledger = selected
        .into_iter()
        .map(|i| {
            let p = &request.existing[i];
            json!({"name":p.name,"diversity":p.diversity,
                "prompt_excerpt":p.prompt.chars().take(excerpt_chars).collect::<String>(),
                "excerpt_truncated":p.prompt.chars().count()>excerpt_chars})
        })
        .collect::<Vec<_>>();
    let mut axis_counts: BTreeMap<String, usize> = BTreeMap::new();
    for p in &request.existing {
        for (axis, value) in &p.diversity {
            *axis_counts.entry(format!("{axis}: {value}")).or_default() += 1;
        }
    }
    let mut repeated = axis_counts
        .into_iter()
        .filter(|(_, count)| *count > 1)
        .collect::<Vec<_>>();
    repeated.sort_by(|a, b| b.1.cmp(&a.1).then(a.0.cmp(&b.0)));
    repeated.truncate(40);
    (ledger, repeated)
}

fn creative_stimuli(request: &PromptRequest) -> Vec<Value> {
    // A finite seed menu is useful only for an empty collection. Repeating it
    // across categories anchors invention to concepts the critic already rejected.
    if !request.existing.is_empty() || !request.rejection_feedback.is_null() {
        return Vec::new();
    }
    let layouts = [
        "branching inhabited spine",
        "loose constellation of unequal clusters",
        "descending terraces around a void",
        "winding ribbon with pocket spaces",
        "off-centre courtyard and broken perimeter",
        "radial fan with uneven wings",
        "stacked interlocking volumes",
        "landscape-following crescent",
        "fragmented archipelago linked by paths",
        "canyon-like passage with side chambers",
        "compact sculpted monolith with carved pockets",
        "multi-level loop around an open landmark",
    ];
    let settings = [
        "weathered coastal edge",
        "lush valley clearing",
        "dense urban infill",
        "arid rock basin",
        "steep wooded hillside",
        "floodplain on raised supports",
        "snow-lit highland",
        "abandoned industrial landscape",
        "subterranean rock chamber",
        "floating island garden",
        "riverside escarpment",
    ];
    let scales = [
        "intimate craft and close detail",
        "medium public gathering spaces",
        "one monumental landmark with small supporting spaces",
        "compact vertical exploration",
        "landscape-spread low structures",
    ];
    (0..request.count).map(|i| {
        let category_hash = request.category.trim().to_lowercase().bytes().fold(14695981039346656037u64, |h,b| h.wrapping_mul(1099511628211) ^ u64::from(b));
        let n=category_hash.wrapping_add(request.seed).wrapping_add(request.offset as u64).wrapping_add(i as u64).wrapping_add(request.retry_index.wrapping_mul(7919)) as usize;
        json!({"scene_index":request.offset.wrapping_add(i),"spatial_stimulus":layouts[n%layouts.len()],"context_stimulus":settings[(n/layouts.len()).wrapping_add(n.wrapping_mul(7))%settings.len()],"scale_stimulus":scales[(n/3).wrapping_add(n)%scales.len()]})
    }).collect::<Vec<_>>()
}

fn continuity_hash(request: &PromptRequest) -> Result<String> {
    let mut value = serde_json::to_value(request).map_err(|e| invalid(e.to_string()))?;
    let fields = value.as_object_mut().unwrap();
    for key in ["retry_index", "previous_candidates", "rejection_feedback"] {
        fields.remove(key);
    }
    Ok(hash_bytes(
        &serde_json::to_vec(&value).map_err(|e| invalid(e.to_string()))?,
    ))
}

fn original_request_hash(request: &PromptRequest) -> Result<String> {
    // Preserve the original struct field order for pre-upgrade checkpoints.
    let mut value = serde_json::to_value(request).map_err(|e| invalid(e.to_string()))?;
    for key in ["retry_index", "previous_candidates", "rejection_feedback"] {
        value.as_object_mut().unwrap().remove(key);
    }
    let original: PromptRequest =
        serde_json::from_value(value).map_err(|e| invalid(e.to_string()))?;
    Ok(hash_bytes(
        &serde_json::to_vec(&original).map_err(|e| invalid(e.to_string()))?,
    ))
}

fn save_draft(
    cache: &Path,
    request: &PromptRequest,
    hash: &str,
    draft: &Value,
    feedback: &Value,
) -> Result<()> {
    atomic_write(
        cache,
        &serde_json::to_vec_pretty(&json!({
            "request_hash":hash,"continuity_hash":continuity_hash(request)?,
            "candidates":draft,"rejection_feedback":feedback
        }))
        .map_err(|e| invalid(e.to_string()))?,
    )
}

async fn plan_chunk(
    invoker: &RoleInvoker,
    request: &PromptRequest,
    root: &Path,
) -> Result<Candidates> {
    let (ledger, repeated) = compact_ledger(request);
    let mut feedback = request.rejection_feedback.clone();
    let mut draft = request.previous_candidates.clone();
    let cache = root.join("draft.json");
    let hash = hash_bytes(&serde_json::to_vec(request).map_err(|e| invalid(e.to_string()))?);
    if let Ok(bytes) = std::fs::read(&cache) {
        if let Ok(v) = serde_json::from_slice::<Value>(&bytes) {
            if v["request_hash"] == hash
                || v["continuity_hash"] == continuity_hash(request)?
                || (v.get("continuity_hash").is_none()
                    && v["request_hash"] == original_request_hash(request)?)
            {
                draft = v["candidates"].clone();
                if feedback.is_null() {
                    feedback = v["rejection_feedback"].clone();
                }
                // Older checkpoints saved critiques separately, without feedback in draft.
                if feedback.is_null() && v.get("rejection_feedback").is_none() {
                    if let Ok(bytes) = std::fs::read(root.join("latest-critique.json")) {
                        if let Ok(critique) = serde_json::from_slice::<Value>(&bytes) {
                            if critique["approved"] == false {
                                feedback = critique["issues"].clone();
                            }
                        }
                    }
                }
            }
        }
    }
    for attempt in 0..3 {
        let candidates: Candidates = if attempt == 0 && !draft.is_null() && feedback.is_null() {
            serde_json::from_value(draft.clone()).map_err(|e| invalid(e.to_string()))?
        } else {
            invoker.invoke_json::<Candidates>(Role::PromptPlanner,&json!({
                "category":request.category,"count":request.count,"seed":request.seed,"offset":request.offset,"briefs":request.briefs,"creative_stimuli":if feedback.is_null() {creative_stimuli(request)} else {vec![]},"stimulus_policy":"These are optional inspiration only, never required layouts or settings. Required corrections take precedence over every stimulus. When the list is empty, invent new spatial arrangements from the category, user briefs and corrections; there is no required menu. For each rejected candidate, implement the concrete replacement direction when buildable and consistent with the category, rather than rewording the rejected geometry. Preserve candidates not mentioned in the feedback unless necessary to resolve a conflict. If a stimulus resembles an existing concept or was rejected, discard it entirely and invent a different spatial organization and context. Do not copy stimulus labels into the diversity ledger. Invent distinct actual geometry, room functions, landmarks and circulation beyond this finite list.",
                "existing_concept_ledger":ledger,"total_prior_concepts":request.existing.len(),"most_repeated_prior_axes":repeated,"previous_candidates":draft,"required_corrections":feedback,
                "contract":"Return {prompts:[{name:string,prompt:string,diversity:{layout:string,setting:string,form:string,materials:string,atmosphere:string,circulation:string}}]}. EXACTLY count concepts, unique names <=120 characters. Each prompt 120-240 words. No numbering variants of one template. Specify distinctive spatial organization, memorable silhouette, setting, program and detailed occupied spaces. Vary organic/asymmetric/terraced/radial/linear/courtyard/fragmented/elevated/subterranean spatial arrangements as appropriate, not every feature in every scene. Furnish each usable room according to its function. Some concepts can be open-air. Do not default every building to rectangular paired boxes, a central cross corridor, symmetric blocks or a roof hiding all detail. Describe physical structure and practical assembly, not impossible abstract metaphors. Limit scope to a strong camera-readable detailed scene rather than an entire empty city. Achieve variation through layout, form, context and program; palette alone is insufficient. Each prompt is self-contained and respects the user's category and constraints. Seed is a creative identifier, not a scene style."
            })).await?.output
        };
        draft = serde_json::to_value(&candidates).map_err(|e| invalid(e.to_string()))?;
        // This new candidate has not been reviewed yet. Persist before critic calls
        // so an outage resumes review without paying for another invention call.
        feedback = Value::Null;
        save_draft(&cache, request, &hash, &draft, &feedback)?;
        if let Err(error) = validate(request, &candidates) {
            feedback = json!([error.to_string()]);
            save_draft(&cache, request, &hash, &draft, &feedback)?;
            continue;
        }
        let critique=invoker.invoke_json::<Critique>(Role::DiversityCritic,&json!({"category":request.category,"existing_concept_ledger":ledger,"candidates":draft,"contract":"Return {approved:boolean,issues:[string]}. Review every candidate against every other candidate and prior concepts using their actual geometry and functional program. Each duplicate rejection must name ONE specific prior or peer concept and describe the same concrete spatial arrangement AND silhouette/program in both. Do not combine a layout match to one scene with a setting match to a different scene to manufacture a duplicate. Broad labels alone are insufficient evidence; do not infer missing details from a truncated excerpt. Approve substantively different spatial layout/silhouette/context/program combinations, not decorative variants. Evaluate the combination: sharing a broad setting or layout family alone is not grounds for rejection when actual geometry, silhouette and functional program differ. This is a large multi-category batch, so individual axes can recur. Prompts must include detailed functional occupied spaces when appropriate, coherent spatial design, and respect category. If rejected, give candidate names and concrete replacement directions. Return issues:[] only if approved."})).await?;
        atomic_write(
            &root.join("latest-critique.json"),
            &serde_json::to_vec_pretty(
                &json!({"approved":critique.output.approved,"issues":critique.output.issues}),
            )
            .map_err(|e| invalid(e.to_string()))?,
        )?;
        if critique.output.approved && critique.output.issues.is_empty() {
            return Ok(candidates);
        }
        feedback = json!(critique.output.issues);
        if feedback.as_array().is_some_and(Vec::is_empty) {
            feedback = json!(["Diversity critic rejected the concepts without details; redesign substantive layout, silhouette, setting and program differences."]);
        }
        save_draft(&cache, request, &hash, &draft, &feedback)?;
    }
    Err(invalid(format!(
        "prompt diversity review unresolved after 3 revisions: {feedback}"
    )))
}

#[cfg(test)]
mod tests {
    use super::*;
    fn request(count: usize) -> PromptRequest {
        PromptRequest {
            category: "museum".into(),
            count,
            seed: 17,
            offset: 0,
            existing: vec![],
            briefs: Value::Null,
            retry_index: 0,
            previous_candidates: Value::Null,
            rejection_feedback: Value::Null,
        }
    }
    #[test]
    fn established_batches_and_rejected_drafts_do_not_reuse_seed_menu() {
        let mut r = request(5);
        assert_eq!(creative_stimuli(&r).len(), 5);
        r.existing.push(concept("Existing showroom", "branching spine"));
        assert!(creative_stimuli(&r).is_empty());
        r.existing.clear();
        r.rejection_feedback = json!(["Replace the branching spine with a column hall"]);
        assert!(creative_stimuli(&r).is_empty());
    }

    #[test]
    fn critic_gets_real_prompt_context_with_bounded_unicode_excerpts() {
        let mut r = request(1);
        r.existing.push(concept("Existing showroom", "branching spine"));
        let (ledger, _) = compact_ledger(&r);
        assert_eq!(ledger[0]["prompt_excerpt"], r.existing[0].prompt);
        assert_eq!(ledger[0]["excerpt_truncated"], false);
        for i in 0..150 {
            let mut p = concept(&format!("Showroom {i}"), "courtyard");
            p.prompt = "景".repeat(6000);
            r.existing.push(p);
        }
        let (ledger, _) = compact_ledger(&r);
        assert!(ledger.iter().map(|p| p["prompt_excerpt"].as_str().unwrap().chars().count()).sum::<usize>() <= 48_000);
        assert!(ledger.iter().any(|p| p["excerpt_truncated"] == true));
    }

    #[test]
    fn category_and_retry_change_stimuli_without_overflow() {
        let mut r = request(5);
        let original = creative_stimuli(&r);
        r.category = "Temples & Religious Architecture".into();
        assert_ne!(original, creative_stimuli(&r));
        r.category = "museum".into();
        r.retry_index = 1;
        assert_ne!(original, creative_stimuli(&r));
        r.retry_index = u64::MAX;
        r.seed = u64::MAX;
        r.offset = usize::MAX;
        assert_eq!(creative_stimuli(&r), creative_stimuli(&r));
    }
    #[test]
    fn old_request_defaults_and_feedback_checkpoint_roundtrip() {
        let mut r: PromptRequest = serde_json::from_value(
            json!({"category":"museum","count":1,"seed":17,"offset":0,"existing":[],"briefs":null}),
        )
        .unwrap();
        assert_eq!(r.retry_index, 0);
        let value = serde_json::to_value(&r).unwrap();
        assert!(value.get("retry_index").is_none());
        assert!(value.get("rejection_feedback").is_none());
        let first = continuity_hash(&r).unwrap();
        let legacy_hash = hash_bytes(&serde_json::to_vec(&r).unwrap());
        r.retry_index = 3;
        r.rejection_feedback = json!(["Replace the repeated spiral court"]);
        r.previous_candidates = json!({"prompts":[]});
        assert_eq!(first, continuity_hash(&r).unwrap());
        assert_eq!(legacy_hash, original_request_hash(&r).unwrap());
        let root =
            std::env::temp_dir().join(format!("loop-prompt-feedback-{}", uuid::Uuid::now_v7()));
        std::fs::create_dir_all(&root).unwrap();
        let path = root.join("draft.json");
        save_draft(
            &path,
            &r,
            "hash",
            &r.previous_candidates,
            &r.rejection_feedback,
        )
        .unwrap();
        let saved: Value = serde_json::from_slice(&std::fs::read(&path).unwrap()).unwrap();
        assert_eq!(saved["rejection_feedback"], r.rejection_feedback);
        assert_eq!(saved["candidates"], r.previous_candidates);
        assert_eq!(saved["continuity_hash"], first);
        std::fs::remove_file(path).unwrap();
        std::fs::remove_dir(root).unwrap();
    }
    #[test]
    fn model_ledger_is_bounded_but_local_validation_checks_omitted_history() {
        let mut r = request(1);
        r.existing = (0..2000)
            .map(|i| {
                let mut p = concept(&format!("Prior scene {i}"), &format!("layout {i}"));
                p.prompt = (0..30)
                    .map(|j| format!("prior{i}word{j}"))
                    .collect::<Vec<_>>()
                    .join(" ");
                p
            })
            .collect();
        let (ledger, repeated) = compact_ledger(&r);
        assert!(ledger.len() <= 128);
        assert!(repeated.len() <= 40);
        for item in &ledger {
            assert!(item.get("prompt").is_none());
        }
        assert!(ledger.iter().any(|v| v["name"] == "Prior scene 1999"));
        let omitted = r
            .existing
            .iter()
            .find(|p| !ledger.iter().any(|v| v["name"] == p.name))
            .unwrap();
        // Even an item outside the bounded model context remains a hard local duplicate.
        let duplicate_error = validate(
            &r,
            &Candidates {
                prompts: vec![omitted.clone()],
            },
        )
        .unwrap_err()
        .to_string();
        assert!(duplicate_error.contains(&format!("repeats {}", omitted.name)));
    }
    #[test]
    fn stimuli_are_stable_across_resume_and_change_with_offset_and_seed() {
        let mut r = request(5);
        let first = creative_stimuli(&r);
        assert_eq!(first, creative_stimuli(&r));
        assert_eq!(first.len(), 5);
        let layouts = first
            .iter()
            .map(|v| v["spatial_stimulus"].as_str().unwrap())
            .collect::<BTreeSet<_>>();
        assert_eq!(layouts.len(), 5);
        r.offset = 5;
        assert_ne!(first, creative_stimuli(&r));
        r.offset = 0;
        r.seed += 1;
        assert_ne!(
            first[0]["spatial_stimulus"],
            creative_stimuli(&r)[0]["spatial_stimulus"]
        );
        r.seed = u64::MAX;
        r.offset = usize::MAX;
        assert_eq!(creative_stimuli(&r).len(), 5);
    }
    #[test]
    fn accepts_five_distinct_concepts_and_rejects_oversized_prompt_without_truncation() {
        let r = request(5);
        let mut candidates = Candidates {
            prompts: (0..5)
                .map(|i| {
                    let mut p = concept(&format!("Unique museum {i}"), &format!("layout {i}"));
                    p.prompt = (0..50)
                        .map(|j| format!("concept{i}word{j}"))
                        .collect::<Vec<_>>()
                        .join(" ");
                    p
                })
                .collect(),
        };
        assert!(validate(&r, &candidates).is_ok());
        candidates.prompts[0].prompt = "a".repeat(6001);
        assert!(validate(&r, &candidates).is_err());
        assert_eq!(candidates.prompts[0].prompt.len(), 6001);
    }
    fn concept(name: &str, layout: &str) -> PlannedPrompt {
        PlannedPrompt {name:name.into(),prompt:format!("{name} {}", "detailed spatial program with occupied galleries furnishing physical structure circulation ".repeat(4)),diversity:[("layout",layout),("setting","forest"),("form",layout),("materials","wood"),("atmosphere","morning"),("circulation","winding")].into_iter().map(|(a,b)|(a.into(),b.into())).collect()}
    }
    #[test]
    fn rejects_duplicate_names_counts_and_layouts() {
        let a = concept("Forest museum", "spiral");
        let r = PromptRequest {
            category: "museum".into(),
            count: 1,
            seed: 0,
            offset: 0,
            existing: vec![a.clone()],
            briefs: Value::Null,
            retry_index: 0,
            previous_candidates: Value::Null,
            rejection_feedback: Value::Null,
        };
        assert!(validate(&r, &Candidates { prompts: vec![] }).is_err());
        assert!(validate(&r, &Candidates { prompts: vec![a] }).is_err());
        assert!(validate(
            &r,
            &Candidates {
                prompts: vec![concept("New name", "spiral")]
            }
        )
        .is_err());
    }
    #[test]
    fn accepts_complete_standalone_concept() {
        let r = PromptRequest {
            category: "museum".into(),
            count: 1,
            seed: 0,
            offset: 0,
            existing: vec![],
            briefs: Value::Null,
            retry_index: 0,
            previous_candidates: Value::Null,
            rejection_feedback: Value::Null,
        };
        assert!(validate(
            &r,
            &Candidates {
                prompts: vec![concept("Forest museum", "spiral")]
            }
        )
        .is_ok());
    }
    #[test]
    fn rejects_cosmetic_rewording_but_accepts_distinct_designs() {
        let a = concept("Forest museum", "spiral");
        let r = PromptRequest {
            category: "museum".into(),
            count: 1,
            seed: 0,
            offset: 0,
            existing: vec![a],
            briefs: Value::Null,
            retry_index: 0,
            previous_candidates: Value::Null,
            rejection_feedback: Value::Null,
        };
        let mut cosmetic = concept("Island gallery", "linear");
        cosmetic.prompt = r.existing[0].prompt.clone() + " blue";
        assert!(validate(
            &r,
            &Candidates {
                prompts: vec![cosmetic]
            }
        )
        .is_err());
        let mut b = concept("Cliffside archive", "terraced");
        b.prompt="A weathered cliffside archive descends through staggered sandstone terraces connected by narrow shaded ramps. Cantilevered observation decks overlook a tidal basin. Carved alcoves house specimen cases, pale linen seating, study desks and delicate suspended instruments. Copper screens filter sunset over sheltered excavation courtyards.".into();
        assert!(validate(
            &r,
            &Candidates {
                prompts: vec![b.clone()]
            }
        )
        .is_ok());
        let mut pair = r;
        pair.count = 2;
        let a = pair.existing.remove(0);
        assert!(validate(
            &pair,
            &Candidates {
                prompts: vec![a, b]
            }
        )
        .is_ok());
    }
}
