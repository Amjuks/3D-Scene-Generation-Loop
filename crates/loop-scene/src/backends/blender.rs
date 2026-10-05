//! Blender 4.x subprocess adapter. Every attempt owns its files and process.

use crate::backends::{AssemblyOutput, BackendCapabilities, ComponentOutput, SceneBackend};
use crate::config::SceneConfig;
use crate::spec::{SceneNode, SceneSpec};
use crate::storage::atomic_write;
use crate::{Result, SceneError};
use async_trait::async_trait;
use serde_json::json;
use std::path::{Path, PathBuf};
use std::process::Stdio;

/// Blender procedural PBR v1 adapter.
pub struct BlenderBackend {
    config: SceneConfig,
    executable: PathBuf,
}

impl BlenderBackend {
    /// Resolve configured/PATH/known local Blender installations.
    pub fn new(config: SceneConfig) -> Result<Self> {
        let executable =
            discover_blender(config.backend.blender_path.as_deref()).ok_or_else(|| {
                SceneError::Backend("Blender not found; set backend.blender_path".into())
            })?;
        Ok(Self { config, executable })
    }
    async fn run_job(&self, job: &serde_json::Value, dir: &Path) -> Result<(PathBuf, PathBuf)> {
        std::fs::create_dir_all(dir).map_err(|e| SceneError::Backend(e.to_string()))?;
        let job_path = dir.join("job.json");
        atomic_write(
            &job_path,
            &serde_json::to_vec_pretty(job).map_err(|e| SceneError::Backend(e.to_string()))?,
        )?;
        let script =
            Path::new(env!("CARGO_MANIFEST_DIR")).join("backends/blender/scene_backend.py");
        let mut cmd = tokio::process::Command::new(&self.executable);
        cmd.args([
            "--background",
            "--factory-startup",
            "--disable-autoexec",
            "--python-exit-code", "31",
            "--python",
        ])
        .arg(script)
        .arg("--")
        .arg(&job_path)
        .current_dir(dir)
        .stdin(Stdio::null())
        .stdout(Stdio::piped())
        .stderr(Stdio::piped())
        .kill_on_drop(true);
        for key in [
            "SOKET_API_KEY",
            "TENSORSTUDIO_API_KEY",
            "LOOP_API_KEY",
            "OPENAI_API_KEY",
        ] {
            cmd.env_remove(key);
        }
        let child = cmd
            .spawn()
            .map_err(|e| SceneError::Backend(format!("start Blender: {e}")))?;
        let result = tokio::time::timeout(
            std::time::Duration::from_secs(self.config.execution.task_timeout_seconds),
            child.wait_with_output(),
        )
        .await
        .map_err(|_| SceneError::Backend("Blender attempt timed out and was terminated".into()))?
        .map_err(|e| SceneError::Backend(e.to_string()))?;
        let stdout = dir.join("stdout.log");
        let stderr = dir.join("stderr.log");
        atomic_write(&stdout, &result.stdout)?;
        atomic_write(&stderr, &result.stderr)?;
        if !result.status.success() {
            return Err(SceneError::Backend(format!(
                "Blender exited {}: {}",
                result.status,
                String::from_utf8_lossy(&result.stderr)
                    .chars()
                    .rev()
                    .take(1200)
                    .collect::<String>()
                    .chars()
                    .rev()
                    .collect::<String>()
            )));
        }
        Ok((stdout, stderr))
    }
    fn check_file(path: &Path, min: u64) -> Result<()> {
        let m = std::fs::metadata(path)
            .map_err(|e| SceneError::Backend(format!("missing {}: {e}", path.display())))?;
        if m.len() < min {
            return Err(SceneError::Backend(format!(
                "{} is unexpectedly small ({} bytes)",
                path.display(),
                m.len()
            )));
        }
        Ok(())
    }
}

#[async_trait]
impl SceneBackend for BlenderBackend {
    async fn capabilities(&self) -> Result<BackendCapabilities> {
        let out = tokio::process::Command::new(&self.executable)
            .arg("--version")
            .output()
            .await
            .map_err(|e| SceneError::Backend(e.to_string()))?;
        if !out.status.success() {
            return Err(SceneError::Backend("Blender --version failed".into()));
        }
        let text = String::from_utf8_lossy(&out.stdout);
        let version = text
            .lines()
            .next()
            .unwrap_or("unknown")
            .trim_start_matches("Blender ")
            .to_string();
        if let Some(required) = &self.config.backend.required_blender_version {
            if !version.starts_with(required) {
                return Err(SceneError::Backend(format!(
                    "Blender {version} does not satisfy pinned prefix {required}"
                )));
            }
        }
        let d = tempfile::tempdir().map_err(|e| SceneError::Backend(e.to_string()))?;
        let report = d.path().join("caps.json");
        let expr=format!("import bpy,json;json.dump({{'engines':[e.identifier for e in bpy.types.RenderSettings.bl_rna.properties['engine'].enum_items]}},open(r'{}','w'))",report.display());
        let out = tokio::process::Command::new(&self.executable)
            .args(["--background", "--factory-startup", "--python-expr", &expr])
            .output()
            .await
            .map_err(|e| SceneError::Backend(e.to_string()))?;
        if !out.status.success() {
            return Err(SceneError::Backend(
                "Blender capability probe failed".into(),
            ));
        }
        let v: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&report).map_err(|e| SceneError::Backend(e.to_string()))?,
        )
        .map_err(|e| SceneError::Backend(e.to_string()))?;
        let engines = v["engines"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|x| x.as_str().map(str::to_owned))
            .collect();
        Ok(BackendCapabilities {
            backend: "blender/procedural_pbr_v1".into(),
            executable: self.executable.clone(),
            version,
            features: vec![
                "native_blend".into(),
                "glb_export_yup".into(),
                "evaluated_mesh_inspection".into(),
                "pbr_materials".into(),
                "multi_view_render".into(),
            ],
            render_engines: engines,
        })
    }
    async fn generate_component(
        &self,
        node: &SceneNode,
        attempt_dir: &Path,
    ) -> Result<ComponentOutput> {
        let (stdout,stderr)=self.run_job(&json!({"mode":"component","node":node,"output_dir":attempt_dir,"renderer":self.config.backend.preview_renderer}),attempt_dir).await?;
        let blend = attempt_dir.join("component.blend");
        let glb = attempt_dir.join("component.glb");
        let inspection = attempt_dir.join("inspection.json");
        Self::check_file(&blend, 1024)?;
        Self::check_file(&glb, 512)?;
        Self::check_file(&inspection, 10)?;
        let report: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&inspection).map_err(|e| SceneError::Backend(e.to_string()))?,
        )
        .map_err(|e| SceneError::Backend(format!("inspection JSON: {e}")))?;
        if report["mesh_count"].as_u64().unwrap_or(0) == 0 {
            return Err(SceneError::Backend(
                "successful process produced no evaluated meshes".into(),
            ));
        }
        Ok(ComponentOutput {
            blend,
            glb,
            inspection,
            stdout,
            stderr,
        })
    }
    async fn assemble(&self, spec: &SceneSpec, output_dir: &Path) -> Result<AssemblyOutput> {
        let (w, h) = (
            self.config.quality.preview_resolution[0],
            self.config.quality.preview_resolution[1],
        );
        self.run_job(&json!({"mode":"assembly","scene":spec,"output_dir":output_dir,"renderer":self.config.backend.preview_renderer,"resolution":[w,h],"samples":self.config.quality.preview_samples}),output_dir).await?;
        let blend = output_dir.join("scene.blend");
        let glb = output_dir.join("scene.glb");
        let inspection = output_dir.join("inspection.json");
        let camera_manifest = output_dir.join("cameras.json");
        for p in [&blend, &glb, &inspection, &camera_manifest] {
            Self::check_file(p, if p == &camera_manifest { 10 } else { 512 })?
        }
        let cams: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&camera_manifest).map_err(|e| SceneError::Backend(e.to_string()))?,
        )
        .map_err(|e| SceneError::Backend(e.to_string()))?;
        let previews = cams["cameras"]
            .as_array()
            .into_iter()
            .flatten()
            .filter_map(|c| c["path"].as_str().map(PathBuf::from))
            .filter(|p| p.exists())
            .collect();
        Ok(AssemblyOutput {
            blend,
            glb,
            inspection,
            previews,
            camera_manifest,
        })
    }
    async fn validate_exports(
        &self,
        blend: &Path,
        glb: &Path,
        output_dir: &Path,
    ) -> Result<PathBuf> {
        let dir = output_dir.join("export-validation");
        let _=self.run_job(&json!({"mode":"validate_exports","blend":blend,"glb":glb,"output_dir":dir,"translation_tolerance_m":self.config.quality.connector_tolerance_m}),&dir).await?;
        let report = dir.join("parity.json");
        Self::check_file(&report, 20)?;
        let v: serde_json::Value = serde_json::from_slice(
            &std::fs::read(&report).map_err(|e| SceneError::Backend(e.to_string()))?,
        )
        .map_err(|e| SceneError::Backend(e.to_string()))?;
        if v["passed"] != true {
            return Err(SceneError::Backend(format!(
                "cross-format parity failed: {}",
                v
            )));
        }
        Ok(report)
    }
}

fn discover_blender(configured: Option<&Path>) -> Option<PathBuf> {
    if let Some(p) = configured {
        if p.is_file() {
            return Some(p.to_path_buf());
        }
    }
    if let Some(paths) = std::env::var_os("PATH") {
        for root in std::env::split_paths(&paths) {
            let p = root.join("blender");
            if p.is_file() {
                return Some(p);
            }
        }
    }
    if let Some(p) = std::env::var_os("BLENDER_PATH").map(PathBuf::from) {
        if p.is_file() {
            return Some(p);
        }
    }
    for p in [
        "/opt/blender/blender",
        "/usr/local/bin/blender",
        "/home/aman/3d_scene_generation/.cache/tools/blender-4.5.1-linux-x64/blender",
    ] {
        let p = PathBuf::from(p);
        if p.is_file() {
            return Some(p);
        }
    }
    None
}
