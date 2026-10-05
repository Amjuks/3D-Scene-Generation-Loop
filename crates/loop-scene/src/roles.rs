//! Typed Soket role registry and schema-constrained invocation.

use crate::config::{ModelsConfig, RoleModelConfig};
use crate::spec::{SceneNode, SceneSpec};
use crate::{Result, SceneError};
use loop_ai::{
    AssistantContent, Context, ImageContent, Message, Models, SimpleStreamOptions, ThinkingLevel,
    UserContent, UserMessage, UserMessageContent,
};
use serde::{de::DeserializeOwned, Deserialize, Serialize};
use std::collections::BTreeMap;
use std::path::Path;
use std::sync::Arc;
use futures::StreamExt;

/// Stable typed role IDs.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord)]
pub enum Role {
    /// Cross-scene concept and prompt invention.
    PromptPlanner,
    /// Independent cross-scene diversity review.
    DiversityCritic,
    /// Whole-scene interpretation.
    SceneDirector,
    /// Recursive subtree allocation.
    DesignCoordinator,
    /// Envelope and construction.
    StructureDesigner,
    /// Circulation and furnishing envelopes.
    LayoutDesigner,
    /// Reusable object specialist.
    AssetSpecialist,
    /// Coherent physically based appearance.
    MaterialLighting,
    /// Actual-image review.
    VisualReviewer,
    /// Minimal repair strategy.
    RepairPlanner,
}
impl Role {
    fn key(self) -> &'static str {
        match self {
            Self::PromptPlanner => "prompt_planner",
            Self::DiversityCritic => "diversity_critic",
            Self::SceneDirector => "scene_director",
            Self::DesignCoordinator => "design_coordinator",
            Self::StructureDesigner => "structure_designer",
            Self::LayoutDesigner => "layout_designer",
            Self::AssetSpecialist => "asset_specialist",
            Self::MaterialLighting => "material_lighting",
            Self::VisualReviewer => "reviewer",
            Self::RepairPlanner => "repair_planner",
        }
    }
}

/// Versioned role declaration.
pub struct RoleDefinition {
    /// Stable prompt version.
    pub prompt_version: &'static str,
    /// Responsibility/system instruction.
    pub system: &'static str,
    /// Whether images are mandatory.
    pub requires_images: bool,
    /// Tools allowed for this role (empty in v1 direct calls).
    pub permitted_tools: &'static [&'static str],
    /// Compact context policy.
    pub context_policy: &'static str,
}

/// All v1 role definitions. Geometry inspection and assembly are deliberately
/// absent because they are deterministic backend operations.
pub fn role_registry() -> BTreeMap<Role, RoleDefinition> {
    use Role::*;
    BTreeMap::from([
    (PromptPlanner,RoleDefinition{prompt_version:"prompt-planner-v2",system:"Invent highly distinct buildable 3D scene concepts. Vary spatial topology, terrain, silhouette, circulation, material language, function, scale and atmosphere rather than merely changing colors or labels. Embrace asymmetry, organic growth, unexpected negative space, fragmented masses and irregular relationships when appropriate. Describe detailed occupied spaces, coherent construction and camera-readable landmarks. Respect the category while challenging its default template. Return JSON only.",requires_images:false,permitted_tools:&[],context_policy:"category, reproducible creative brief and prior concept ledger"}),
    (DiversityCritic,RoleDefinition{prompt_version:"diversity-critic-v2",system:"Independently inspect proposed scene prompts against the prior concept ledger. Reject cosmetic variants of a specific scene, empty-room instructions, vague concepts and contradictory spatial requirements. Judge concrete geometry, silhouette and functional program together. A shared broad layout family, setting, material or circulation label is not itself a duplicate. Do not pool similarities to different prior scenes into a rejection. For duplicates identify one matching scene and the concrete repeated arrangement in both. Replacement directions must remain buildable and respect the category and user constraints. Return explicit indexed replacement feedback in JSON; do not invent approval for missing candidates.",requires_images:false,permitted_tools:&[],context_policy:"candidate concepts, previous concepts and diversity rubric"}),
    (SceneDirector,RoleDefinition{prompt_version:"scene-director-v1",system:"Interpret the complete prompt across any scale. Establish dimensions, hierarchy, major relations, required interiors, style, lighting, construction realism, views, and explicit reasonable assumptions. Allocate immediate children before they are detailed. Return JSON only.",requires_images:false,permitted_tools:&[],context_policy:"original requirements plus fixed global constraints"}),
    (DesignCoordinator,RoleDefinition{prompt_version:"design-coordinator-v1",system:"Design only the supplied subtree. Preserve its accepted parent envelope and interfaces. Allocate immediate child envelopes/exclusions/interfaces before child detail. Stop at meaningful leaves that a named deterministic recipe can generate unambiguously. Return JSON only.",requires_images:false,permitted_tools:&[],context_policy:"node revision, inherited hard constraints, neighbor interfaces, findings, dependency hashes"}),
    (StructureDesigner,RoleDefinition{prompt_version:"structure-v1",system:"Design envelope, slabs, shared boundaries with single ownership, openings, stairs, supports and construction interfaces using explicit metric dimensions. Return JSON only.",requires_images:false,permitted_tools:&[],context_policy:"structural subtree and adjacent boundary interfaces"}),
    (LayoutDesigner,RoleDefinition{prompt_version:"layout-v1",system:"Allocate usable spaces, circulation, door swings, furniture envelopes, adjacency, exclusion and accessibility regions with measured coordinates. Return JSON only.",requires_images:false,permitted_tools:&[],context_policy:"space bounds, openings, circulation and requested contents"}),
    (AssetSpecialist,RoleDefinition{prompt_version:"asset-v1",system:"Specify/retrieve furniture, fixtures, vegetation and props as meaningful assemblies with construction contacts, explicit fronts, bounds, connectors and supported adaptations. Return JSON only.",requires_images:false,permitted_tools:&[],context_policy:"leaf/subtree envelope, style/material references and retrieval candidates"}),
    (MaterialLighting,RoleDefinition{prompt_version:"material-light-v1",system:"Specify coherent PBR values, physical texture scales, photographic lighting and portable glTF-compatible material intent. Return JSON only.",requires_images:false,permitted_tools:&[],context_policy:"scene palette, relevant geometry and camera plan"}),
    (VisualReviewer,RoleDefinition{prompt_version:"visual-review-v1",system:"Review the supplied actual rendered images against requirements. Return structured node-linked findings; never infer that unseen views passed. Return JSON only.",requires_images:true,permitted_tools:&[],context_policy:"requirements, node/camera IDs and bounded actual images"}),
    (RepairPlanner,RoleDefinition{prompt_version:"repair-v1",system:"Given measured failures and prior fingerprints, propose the lowest-owned minimal repair, changing strategy after repeated identical failures. Never weaken hard tolerances. Return JSON only.",requires_images:false,permitted_tools:&[],context_policy:"affected nodes, exact evidence, prior repairs and parent interfaces"}),
])
}

/// Parent plus newly allocated immediate children returned at one frontier.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct FrontierProposal {
    /// Revised parent with complete allocations and child IDs.
    pub parent: SceneNode,
    /// Immediate children only.
    pub children: Vec<SceneNode>,
    /// Readable intent/construction guidance.
    pub markdown: String,
}

/// Structured visual finding.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualFinding {
    /// Severity: info, warning, error, blocker.
    pub severity: String,
    /// Affected stable IDs.
    pub affected_ids: Vec<String>,
    /// Camera/image ID and visible evidence.
    pub evidence: String,
    /// Confidence 0..1.
    pub confidence: f64,
    /// Minimal suggested correction.
    pub suggested_correction: String,
    /// Whether another actual view is needed.
    pub needs_another_view: bool,
}

/// Reviewer output.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct VisualReview {
    /// Findings, empty only when all supplied views pass.
    pub findings: Vec<VisualFinding>,
    /// Whether supplied view coverage is sufficient.
    pub coverage_sufficient: bool,
    /// Short rubric-based summary.
    pub summary: String,
}

/// Bounded repair operation; the controller applies only these typed fields.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct RepairDecision {
    /// Lowest-owned node to change.
    pub affected_node_id: String,
    /// `repair_local`, `replace_strategy`, `redesign_subtree`, or `revise_parent`.
    pub action: String,
    /// Recipe parameter changes that do not alter the parent envelope.
    #[serde(default)]
    pub generation_parameters: BTreeMap<String, serde_json::Value>,
    /// Additive roughness adjustment, clamped to 0..1.
    #[serde(default)]
    pub material_roughness_delta: f64,
    /// Evidence-based rationale stored in design Markdown.
    pub explanation: String,
}

/// Model response plus accountable usage.
pub struct RoleResponse<T> {
    /// Parsed typed output.
    pub output: T,
    /// Provider-reported tokens.
    pub tokens: u64,
    /// Actual model ID.
    pub model: String,
    /// Prompt template version.
    pub prompt_version: String,
}

/// Direct scene role invoker backed by Loop's existing Models/Soket auth path.
pub struct RoleInvoker {
    models: Arc<Models>,
    config: ModelsConfig,
    reporter: Option<crate::reporting::Reporter>,
    budget: Option<crate::config::BudgetConfig>,
}
impl RoleInvoker {
    /// Construct a role invoker.
    pub fn new(models: Arc<Models>, config: ModelsConfig) -> Self {
        Self { models, config, reporter: None, budget: None }
    }
    pub(crate) fn with_reporter(mut self, reporter: crate::reporting::Reporter) -> Self {
        self.reporter=Some(reporter);self
    }
    pub(crate) fn with_budget(mut self, budget: crate::config::BudgetConfig) -> Self {
        self.budget = Some(budget);
        self
    }
    fn role_config(&self, role: Role) -> RoleModelConfig {
        self.config
            .roles
            .get(role.key())
            .cloned()
            .unwrap_or(RoleModelConfig {
                model: self.config.default_model.clone(),
                reasoning: "off".into(),
                stream: true,
                requires_images: role_registry()[&role].requires_images,
                timeout_seconds: crate::config::model_timeout(),
                max_tokens: 8192,
            })
    }
    fn resolve(&self, role: Role) -> Result<(loop_ai::Model, RoleModelConfig)> {
        let cfg = self.role_config(role);
        let model = self
            .models
            .get_model(&self.config.provider, &cfg.model)
            .ok_or_else(|| {
                SceneError::Model(format!(
                    "configured Soket model not found for {}: {}",
                    role.key(),
                    cfg.model
                ))
            })?;
        if (cfg.requires_images || role_registry()[&role].requires_images)
            && !model.supports_images()
        {
            return Err(SceneError::WaitingExternal(format!(
                "role {} requires image input but {}/{} is text-only",
                role.key(),
                model.provider,
                model.id
            )));
        }
        Ok((model, cfg))
    }
    /// Invoke a text-only role and require valid typed JSON. One bounded
    /// correction is attempted with the precise local schema error.
    pub async fn invoke_json<T: DeserializeOwned>(
        &self,
        role: Role,
        payload: &serde_json::Value,
    ) -> Result<RoleResponse<T>> {
        let def = &role_registry()[&role];
        let prompt=format!("ROLE OUTPUT CONTRACT: Return one JSON value matching the requested fields exactly; no Markdown fence and no commentary. All positions are meters in RH +X east/+Y north/+Z up. Quaternion order is [x,y,z,w]. Do not use non-identity scale.\n\nRelevant scoped context:\n{}",serde_json::to_string_pretty(payload).map_err(|e|SceneError::Model(e.to_string()))?);
        let prompt=format!("{prompt}\nIMPORTANT: rotation_xyzw fields use quaternion [x,y,z,w]. Compact object rotation fields instead use THREE Euler angles [X,Y,Z] in DEGREES; use [0,0,0] for no rotation. Follow the exact requested schema, including description. JSON numbers must be computed numeric literals: no arithmetic expressions, comments, units or ellipses in JSON. Return exactly one object.");
        let first = self.call(role, vec![Message::user_text(prompt.clone())]).await?;
        if let Some(r)=&self.reporter {r.save_response(role.key(),&first.0)?;}
        match parse_json::<T>(&first.0) {
            Ok(output) => Ok(RoleResponse {
                output,
                tokens: first.1,
                model: first.2,
                prompt_version: def.prompt_version.into(),
            }),
            Err(error) => {
                eprintln!("Scene role {}: correcting JSON schema: {}",role.key(),error);
                let correction=format!("Your previous output failed local validation: {error}. Return a complete corrected JSON value only. Do not discuss the error.\nOriginal required contract:\n{prompt}\nPrevious output:\n{}",truncate(&first.0,24000));
                let second = self
                    .call(role, vec![Message::user_text(correction)])
                    .await?;
                if let Some(r)=&self.reporter {r.save_response(role.key(),&second.0)?;}
                let output = parse_json(&second.0).map_err(|e| {
                    SceneError::Model(format!("{} output remained invalid: {e}", role.key()))
                })?;
                Ok(RoleResponse {
                    output,
                    tokens: first.1 + second.1,
                    model: second.2,
                    prompt_version: def.prompt_version.into(),
                })
            }
        }
    }
    /// Review actual PNG/JPEG renderer outputs using a verified image-capable model.
    pub async fn review_images(
        &self,
        spec: &SceneSpec,
        images: &[(&str, &Path)],
    ) -> Result<RoleResponse<VisualReview>> {
        let mut blocks=vec![UserContent::Text(loop_ai::TextContent{text:format!("Review these actual camera renders. Return JSON {{\"findings\":[{{\"severity\":...,\"affected_ids\":[...],\"evidence\":...,\"confidence\":0..1,\"suggested_correction\":...,\"needs_another_view\":false}}],\"coverage_sufficient\":bool,\"summary\":string}}. Requirements and nodes: {}",serde_json::to_string(spec).map_err(|e|SceneError::Model(e.to_string()))?),text_signature:None})];
        for (label, path) in images {
            let data = std::fs::read(path)
                .map_err(|e| SceneError::Model(format!("read render {}: {e}", path.display())))?;
            blocks.push(UserContent::Text(loop_ai::TextContent {
                text: format!("Camera: {label}"),
                text_signature: None,
            }));
            blocks.push(UserContent::Image(ImageContent {
                data: base64::Engine::encode(&base64::engine::general_purpose::STANDARD, data),
                mime_type: "image/png".into(),
            }));
        }
        let (text, tokens, model) = self
            .call(
                Role::VisualReviewer,
                vec![Message::User(UserMessage {
                    content: UserMessageContent::Blocks(blocks),
                    timestamp: loop_ai::now_ms(),
                })],
            )
            .await?;
        let output = parse_json(&text)
            .map_err(|e| SceneError::Model(format!("visual review schema: {e}")))?;
        Ok(RoleResponse {
            output,
            tokens,
            model,
            prompt_version: role_registry()[&Role::VisualReviewer].prompt_version.into(),
        })
    }
    async fn call(&self, role: Role, messages: Vec<Message>) -> Result<(String, u64, String)> {
        // Resolve before spending tokens. A missing fallback must neither abort
        // a usable chain nor cause repeated paid calls to the primary model.
        self.resolve(role)?;
        let primary = self.role_config(role).model;
        let mut candidates = vec![primary.clone()];
        for model in &self.config.fallback_models {
            if candidates.contains(model) { continue; }
            if self.models.get_model(&self.config.provider, model).is_some() {
                candidates.push(model.clone());
            } else {
                eprintln!("Scene role {}: skipping fallback absent from registered model catalog: {model}", role.key());
            }
        }
        for attempt in 1..=3 {
            if let (Some(reporter), Some(budget)) = (&self.reporter, &self.budget) {
                reporter.check_model_budget(budget)?;
            }
            let mut invocation = Self::new(Arc::clone(&self.models), self.config.clone());
            invocation.reporter = self.reporter.clone();
            let mut config = self.role_config(role);
            config.model = candidates[((attempt - 1) as usize).min(candidates.len() - 1)].clone();
            invocation.config.roles.insert(role.key().into(), config);
            eprintln!("Scene role {}: model {}, attempt {attempt}/3", role.key(), invocation.role_config(role).model);
            let span=self.reporter.as_ref().map(|r|r.begin("model",role.key(),serde_json::json!({"model":invocation.role_config(role).model,"provider":self.config.provider,"attempt":attempt,"tokens":null}))).transpose()?;
            let mut usage = serde_json::json!({});
            let result=invocation.call_once(role, messages.clone(), &mut usage).await;
            if let Some(span)=span {
                let (status,mut details)=match &result {
                    Ok((_,tokens,model))=>("completed",serde_json::json!({"tokens":tokens,"model":model})),
                    Err(e)=>("failed",serde_json::json!({"error":e.to_string()})),
                };
                details.as_object_mut().unwrap().extend(usage.as_object().unwrap().clone());
                span.finish(status,details)?;
            }
            match result {
                Ok(result) => return Ok(result),
                Err(error) => {
                    let transient = retryable_model_error(&error);
                    if !transient || attempt == 3 || (error.to_string().contains("stop=Length") && attempt as usize >= candidates.len()) {
                        return Err(SceneError::Model(format!(
                            "role {} failed after {attempt} attempt(s): {error}",
                            role.key()
                        )));
                    }
                    let delay = 5 * attempt;
                    eprintln!(
                        "Scene role {}: {error}; retry {}/3 in {delay}s",
                        role.key(),
                        attempt + 1
                    );
                    tokio::time::sleep(std::time::Duration::from_secs(delay)).await;
                }
            }
        }
        unreachable!("bounded retry loop returns on its final attempt")
    }

    async fn call_once(&self, role: Role, messages: Vec<Message>, usage: &mut serde_json::Value) -> Result<(String, u64, String)> {
        let (model, cfg) = self.resolve(role)?;
        let def = &role_registry()[&role];
        let context = Context {
            system_prompt: Some(format!(
                "{}\nPrompt template: {}. Context policy: {}. Permitted tools: {:?}.",
                def.system, def.prompt_version, def.context_policy, def.permitted_tools
            )),
            messages,
            tools: None,
        };
        let mut options = SimpleStreamOptions::default();
        options.base.max_tokens = Some(cfg.max_tokens);
        options.base.timeout_ms = Some(cfg.timeout_seconds * 1000);
        options.reasoning = match cfg.reasoning.as_str() {
            "minimal" => Some(ThinkingLevel::Minimal),
            "low" => Some(ThinkingLevel::Low),
            "medium" => Some(ThinkingLevel::Medium),
            "high" => Some(ThinkingLevel::High),
            "xhigh" => Some(ThinkingLevel::XHigh),
            "max" => Some(ThinkingLevel::Max),
            _ => None,
        };
        if cfg.reasoning == "off" && self.config.provider == "soket" {
            options.base.on_payload = Some(Arc::new(disable_qwen_thinking));
        }
        let mut stream = self.models.stream_simple(&model, &context, options);
        let started = std::time::Instant::now();
        let mut last_log = started;
        let mut text_bytes = 0;
        let mut thinking_bytes = 0;
        while let Some(event) = stream.next().await {
            match &event {
                loop_ai::AssistantMessageEvent::TextDelta { delta, .. } => text_bytes += delta.len(),
                loop_ai::AssistantMessageEvent::ThinkingDelta { delta, .. } => thinking_bytes += delta.len(),
                _ => (),
            }
            if last_log.elapsed().as_secs() >= 20 || event.is_terminal() {
                eprintln!("Scene role {}: {}s elapsed, {} text bytes, {} reasoning bytes", role.key(), started.elapsed().as_secs(), text_bytes, thinking_bytes);
                last_log = std::time::Instant::now();
                if let Some(r)=&self.reporter {r.refresh()?;}
            }
            if event.is_terminal() { break; }
        }
        let msg = stream.result().await;
        if msg.usage.total_tokens > 0 {
            *usage = serde_json::json!({"tokens":msg.usage.total_tokens,
                "input_tokens":msg.usage.input,"output_tokens":msg.usage.output});
        }
        if msg.stop_reason.is_error() {
            return Err(SceneError::Model(
                msg.error_message
                    .unwrap_or_else(|| "Soket stream failed".into()),
            ));
        }
        let text = msg
            .content
            .iter()
            .filter_map(|c| match c {
                AssistantContent::Text(t) => Some(t.text.as_str()),
                _ => None,
            })
            .collect::<Vec<_>>()
            .join("");
        if text.trim().is_empty() {
            return Err(SceneError::Model(format!("model returned no JSON text (model={}, stop={:?}, output_tokens={}, content_blocks={})", model.id, msg.stop_reason, msg.usage.output, msg.content.len())));
        }
        Ok((text, msg.usage.total_tokens, model.id))
    }
}

// Soket can expose a Qwen chat template through an OpenAI-compatible proxy.
// None means "server default", not off; thinking-only models cannot disable it.
fn disable_qwen_thinking(payload: &serde_json::Value, model: &loop_ai::Model) -> Option<serde_json::Value> {
    let id = model.id.to_ascii_lowercase();
    if !id.starts_with("qwen3") || id.contains("thinking") { return None; }
    let mut payload = payload.clone();
    payload["chat_template_kwargs"]["enable_thinking"] = serde_json::json!(false);
    Some(payload)
}

fn retryable_model_error(error: &SceneError) -> bool {
    let SceneError::Model(message) = error else {
        return false;
    };
    let message = message.to_ascii_lowercase();
    [
        "timeout",
        "timed out",
        "transport error",
        "error sending request",
        "model returned no json text",
        "rate limit",
        "overloaded",
        "temporarily unavailable",
        "server stopped responding",
        "connection reset",
        "connection closed",
        "http 408:",
        "http 429:",
        "http 500:",
        "http 502:",
        "http 503:",
        "http 504:",
    ]
    .iter()
    .any(|needle| message.contains(needle))
}

#[cfg(test)]
mod retry_tests {
    use super::*;

    #[tokio::test]
    async fn missing_fallback_is_skipped_and_failed_length_usage_is_retained() {
        use loop_ai::providers::{faux_provider, FauxResponse, FauxScript};
        let script = FauxScript::new();
        let provider = faux_provider(script.clone());
        let model = provider.get_models().remove(0);
        let mut alternate = model.clone();
        alternate.id = "alternate".into();
        provider.set_models(vec![model.clone(), alternate]);
        let models = Arc::new(Models::new());
        models.set_provider(provider);
        let mut message = loop_ai::AssistantMessage::pending(&model);
        message.stop_reason = loop_ai::StopReason::Length;
        message.usage.output = 8192;
        message.usage.total_tokens = 8200;
        script.push(FauxResponse::Events(vec![loop_ai::AssistantMessageEvent::Done {
            reason: loop_ai::StopReason::Length, message,
        }]));
        script.push(FauxResponse::Text("{\"ok\":true}".into()));
        let dir = tempfile::tempdir().unwrap();
        let reporter = crate::reporting::Reporter::new(dir.path());
        let run = reporter.begin("run", "test", serde_json::json!({})).unwrap();
        let mut config = ModelsConfig::default();
        config.provider = "faux".into();
        config.default_model = "faux-model".into();
        config.fallback_models = vec!["missing".into(), "alternate".into()];
        let invoker = RoleInvoker::new(models, config).with_reporter(reporter);
        let response = invoker.invoke_json::<serde_json::Value>(Role::SceneDirector, &serde_json::json!({})).await.unwrap();
        assert_eq!(response.model, "alternate");
        run.finish("accepted", serde_json::json!({})).unwrap();
        let report: serde_json::Value = serde_json::from_slice(&std::fs::read(dir.path().join("run-report.json")).unwrap()).unwrap();
        assert_eq!(report["model_attempts"], 2);
        assert_eq!(report["reported_tokens"], 8201);
        assert_eq!(report["failed_model_attempts"], 1);
    }

    #[test]
    fn explicit_off_disables_switchable_qwen_without_touching_other_models() {
        let mut model = loop_ai::providers::soket::soket_seed_models().remove(0);
        model.id = "qwen3-8-27b".into();
        let payload = serde_json::json!({"messages":[],"max_tokens":8192,"chat_template_kwargs":{"other":true}});
        let off = disable_qwen_thinking(&payload, &model).unwrap();
        assert_eq!(off["chat_template_kwargs"]["enable_thinking"], false);
        assert_eq!(off["chat_template_kwargs"]["other"], true);
        for id in ["gemma-4-31b", "qwen3-30b-thinking-2507"] {
            model.id = id.into();
            assert!(disable_qwen_thinking(&payload, &model).is_none());
        }
    }

    #[test]
    fn role_timeout_defaults_and_overrides() {
        let default: RoleModelConfig =
            serde_json::from_value(serde_json::json!({"model": "test-model"})).unwrap();
        assert_eq!(default.timeout_seconds, 600);
        let custom: RoleModelConfig = serde_json::from_value(
            serde_json::json!({"model": "test-model", "timeout_seconds": 42}),
        )
        .unwrap();
        assert_eq!(custom.timeout_seconds, 42);
    }

    #[test]
    fn retries_only_transient_failures() {
        for message in [
            "request timed out",
            "sse error: Transport error: error decoding response body",
            "HTTP 429: busy",
            "HTTP 503: unavailable",
            "error sending request for url (http://example.invalid)",
            "model returned no JSON text (stop=Length)",
        ] {
            assert!(retryable_model_error(&SceneError::Model(message.into())));
        }
        for message in [
            "HTTP 401: unauthorized",
            "HTTP 400: bad request",
            "aborted",
        ] {
            assert!(!retryable_model_error(&SceneError::Model(message.into())));
        }
        assert!(!retryable_model_error(&SceneError::Validation(
            "timeout".into()
        )));
    }
}

fn parse_json<T: DeserializeOwned>(text: &str) -> std::result::Result<T, String> {
    let trimmed = text.trim();
    let value = if trimmed.starts_with("```") {
        let mut lines = trimmed.lines();
        lines.next();
        let body = lines
            .take_while(|l| !l.trim_start().starts_with("```"))
            .collect::<Vec<_>>()
            .join("\n");
        body
    } else {
        trimmed.to_string()
    };
    serde_json::from_str(&value).map_err(|e| e.to_string())
}
fn truncate(s: &str, max: usize) -> String {
    s.chars().take(max).collect()
}
