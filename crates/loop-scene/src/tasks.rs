//! Strict versioned scene task-list schema.

use crate::{Result, SceneError};
use serde::{Deserialize, Serialize};
use std::path::Path;

/// A queue of independent scene requests.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneTaskList {
    /// Schema version.
    pub version: u32,
    /// Ordered independent requests.
    pub tasks: Vec<SceneTask>,
}

/// One scene queue item.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct SceneTask {
    /// Stable filesystem-safe task ID.
    pub id: String,
    /// Whether it should be processed.
    #[serde(default = "enabled")]
    pub enabled: bool,
    /// Natural-language scene request.
    pub prompt: String,
    /// Reproducible procedural seed.
    #[serde(default)]
    pub seed: Option<u64>,
    /// Strict partial scene-config overlay.
    #[serde(default)]
    pub overrides: serde_yaml::Value,
}
fn enabled() -> bool {
    true
}

impl SceneTaskList {
    /// Load and validate a task file.
    pub fn load(path: &Path) -> Result<Self> {
        let raw = std::fs::read_to_string(path)
            .map_err(|e| SceneError::Validation(format!("read {}: {e}", path.display())))?;
        let list: Self = serde_yaml::from_str(&raw)
            .map_err(|e| SceneError::Validation(format!("task-list schema: {e}")))?;
        list.validate()?;
        Ok(list)
    }
    /// Validate queue IDs and prompts.
    pub fn validate(&self) -> Result<()> {
        if self.version != 1 {
            return Err(SceneError::Validation(format!(
                "unsupported task-list version {}",
                self.version
            )));
        }
        let mut ids = std::collections::HashSet::new();
        for task in &self.tasks {
            if task.id.is_empty()
                || !task
                    .id
                    .bytes()
                    .all(|b| b.is_ascii_alphanumeric() || b == b'_' || b == b'-')
            {
                return Err(SceneError::Validation(format!(
                    "unsafe task id: {}",
                    task.id
                )));
            }
            if !ids.insert(&task.id) {
                return Err(SceneError::Validation(format!(
                    "duplicate task id: {}",
                    task.id
                )));
            }
            if task.prompt.trim().is_empty() {
                return Err(SceneError::Validation(format!(
                    "empty prompt for {}",
                    task.id
                )));
            }
        }
        Ok(())
    }
}
