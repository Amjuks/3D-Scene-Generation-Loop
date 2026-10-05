//! Diagnose the exact scene-role model path without generating geometry.
use std::sync::Arc;
use loop_app_core::config::{auth::FileCredentialStore, paths::{auth_path, get_agent_dir}};

#[tokio::main]
async fn main() -> anyhow::Result<()> {
    let agent = get_agent_dir();
    let credentials = Arc::new(FileCredentialStore::open(auth_path(&agent))?);
    let models = loop_app_core::build_models(&agent, credentials)?;
    let mut config = loop_scene::config::ModelsConfig::default();
    config.default_model = std::env::args().nth(1).unwrap_or("gpt".into());
    let invoker = loop_scene::roles::RoleInvoker::new(models, config);
    let result = invoker.invoke_json::<serde_json::Value>(loop_scene::roles::Role::SceneDirector,
        &serde_json::json!({"required_output":{"ok":true},"instruction":"Return exactly this JSON, no other text."})).await?;
    println!("{}", result.output);
    Ok(())
}
