//! Scene generation backend abstraction.

mod blender;

use crate::spec::{SceneNode, SceneSpec};
use crate::Result;
use async_trait::async_trait;
use serde::{Deserialize, Serialize};
use std::path::{Path, PathBuf};

pub use blender::BlenderBackend;

/// Discovered backend capabilities and pinned versions.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct BackendCapabilities {
    /// Adapter name.
    pub backend: String,
    /// Resolved executable path.
    pub executable: PathBuf,
    /// Reported backend version.
    pub version: String,
    /// Supported named features.
    pub features: Vec<String>,
    /// Render engines available in this installation.
    pub render_engines: Vec<String>,
}

/// Validated component attempt outputs.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ComponentOutput {
    /// Native component asset.
    pub blend: PathBuf,
    /// Portable component asset.
    pub glb: PathBuf,
    /// Evaluated geometry inspection JSON.
    pub inspection: PathBuf,
    /// Captured stdout.
    pub stdout: PathBuf,
    /// Captured stderr.
    pub stderr: PathBuf,
}

/// Final native/interchange scene and rendered evidence.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct AssemblyOutput {
    /// Editable Blender file.
    pub blend: PathBuf,
    /// Self-contained portable GLB.
    pub glb: PathBuf,
    /// Backend inspection report.
    pub inspection: PathBuf,
    /// Multiple actual renderer previews.
    pub previews: Vec<PathBuf>,
    /// Camera plan and render metadata.
    pub camera_manifest: PathBuf,
}

/// Generation/inspection/assembly/export adapter boundary.
#[async_trait]
pub trait SceneBackend: Send + Sync {
    /// Discover and verify backend features.
    async fn capabilities(&self) -> Result<BackendCapabilities>;
    /// Generate a single component in an isolated process/directory.
    async fn generate_component(
        &self,
        node: &SceneNode,
        attempt_dir: &Path,
    ) -> Result<ComponentOutput>;
    /// Assemble accepted nodes bottom-up, render, and export both final formats.
    async fn assemble(&self, spec: &SceneSpec, output_dir: &Path) -> Result<AssemblyOutput>;
    /// Reopen both formats independently and compare their canonical landmarks.
    async fn validate_exports(
        &self,
        blend: &Path,
        glb: &Path,
        output_dir: &Path,
    ) -> Result<PathBuf>;
}
