//! `loop scene` command implementation.

use anyhow::{bail, Context};
use loop_app_core::config::auth::FileCredentialStore;
use loop_app_core::config::paths::{auth_path, get_agent_dir};
use loop_scene::{SceneConfig, SceneRunController, SceneTaskList};
use std::path::{Path, PathBuf};
use std::sync::Arc;

/// Run the concept planner and independent diversity critic with durable telemetry.
pub async fn plan_prompts(config_path: &Path, request_path: &Path, output: &Path) -> anyhow::Result<()> {
    let config = SceneConfig::load(config_path).map_err(anyhow::Error::msg)?;
    let request = serde_json::from_slice(&std::fs::read(request_path)?).context("prompt planning request")?;
    let models = load_models()?;
    let _ = models.refresh(loop_ai::ModelsRefreshOptions {
        provider_id: Some(config.models.provider.clone()), ..Default::default()
    }).await;
    let result = loop_scene::prompt_planning::plan_prompts(models, config.models, request, output).await?;
    println!("{}", serde_json::to_string(&result)?);
    Ok(())
}

/// Run the doctor preflight and print JSON evidence.
pub async fn doctor(config_path: &Path) -> anyhow::Result<()> {
    let config = SceneConfig::load(config_path).map_err(anyhow::Error::msg)?;
    let models = load_models().context("load Loop/Soket models")?;
    let _ = models
        .refresh(loop_ai::ModelsRefreshOptions {
            provider_id: Some("soket".into()),
            ..Default::default()
        })
        .await;
    let report = SceneRunController::new(config, Some(models)).doctor().await;
    println!("{}", serde_json::to_string_pretty(&report)?);
    if !report.runnable {
        bail!("scene doctor found blockers")
    };
    Ok(())
}

/// Start all enabled independent queue items.
pub async fn run(config_path: &Path, tasks_path: &Path) -> anyhow::Result<()> {
    let config = SceneConfig::load(config_path).map_err(anyhow::Error::msg)?;
    let tasks = SceneTaskList::load(tasks_path).map_err(anyhow::Error::msg)?;
    let models = load_models()?;
    let _ = models
        .refresh(loop_ai::ModelsRefreshOptions {
            provider_id: Some("soket".into()),
            ..Default::default()
        })
        .await;
    let ctl = SceneRunController::new(config, Some(models));
    let results = ctl.run_queue(&tasks, tasks_path).await;
    let mut accepted = true;
    for r in results {
        match r {
            Ok(s) => {
                accepted &= s.state == loop_scene::storage::RunState::Accepted;
                println!("{}", serde_json::to_string(&s)?)
            }
            Err(e) => {
                accepted = false;
                eprintln!("scene task failed: {e}")
            }
        }
    }
    if !accepted {
        bail!("one or more scenes are incomplete; use `loop scene status` and `resume`")
    }
    Ok(())
}

/// Print durable state.
pub fn status(run: &Path, cwd: &Path) -> anyhow::Result<()> {
    let root = resolve_run(run, cwd)?;
    let value = SceneRunController::status(&root).map_err(anyhow::Error::msg)?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

/// Resume using immutable saved input snapshots.
pub async fn resume(run: &Path, cwd: &Path) -> anyhow::Result<()> {
    let root = resolve_run(run, cwd)?.canonicalize().context("resolve saved run directory")?;
    let config: SceneConfig =
        serde_json::from_slice(&std::fs::read(root.join("input/resolved-config.json"))?)
            .context("saved resolved config")?;
    let task: loop_scene::tasks::SceneTask =
        serde_json::from_slice(&std::fs::read(root.join("input/task.json"))?)
            .context("saved task")?;
    let models = load_models()?;
    let _ = models
        .refresh(loop_ai::ModelsRefreshOptions {
            provider_id: Some("soket".into()),
            ..Default::default()
        })
        .await;
    let result = SceneRunController::new(config.clone(), Some(models))
        .run_one(&task, config, Some(root))
        .await
        .map_err(anyhow::Error::msg)?;
    println!("{}", serde_json::to_string_pretty(&result)?);
    if result.state != loop_scene::storage::RunState::Accepted {
        bail!("run remains incomplete")
    };
    Ok(())
}

/// Print inspectable design/evidence locations and the final manifest when present.
pub fn inspect(run: &Path, cwd: &Path) -> anyhow::Result<()> {
    let root = resolve_run(run, cwd)?;
    let status = SceneRunController::status(&root).map_err(anyhow::Error::msg)?;
    let spec = root.join("designs/scene-spec.json");
    let manifest = root.join("final/manifest.json");
    let report = root.join("final/report.md");
    let viewer = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../packages/scene-viewer");
    println!(
        "{}",
        serde_json::to_string_pretty(
            &serde_json::json!({"status":status,"scene_spec":spec.exists().then_some(spec),"manifest":manifest.exists().then_some(manifest),"report":report.exists().then_some(report),"viewer":viewer,"viewer_command":"npm install && npm run dev -- --scene /absolute/path/to/scene.glb"})
        )?
    );
    Ok(())
}

/// Persist explicit cancellation. Recovery will not restart the run.
pub fn cancel(run: &Path, cwd: &Path) -> anyhow::Result<()> {
    let root = resolve_run(run, cwd)?;
    let value = SceneRunController::cancel_run(&root).map_err(anyhow::Error::msg)?;
    println!("{}", serde_json::to_string_pretty(&value)?);
    Ok(())
}

fn load_models() -> anyhow::Result<Arc<loop_ai::Models>> {
    let agent = get_agent_dir();
    let credentials = Arc::new(FileCredentialStore::open(auth_path(&agent))?);
    loop_app_core::build_models(&agent, credentials)
}
fn resolve_run(run: &Path, cwd: &Path) -> anyhow::Result<PathBuf> {
    if run.is_dir() {
        return Ok(run.to_path_buf());
    }
    let p = cwd.join("scene-runs").join(run);
    if p.is_dir() {
        return Ok(p);
    }
    bail!(
        "run not found: {} (pass its directory when output_root is custom)",
        run.display()
    )
}
