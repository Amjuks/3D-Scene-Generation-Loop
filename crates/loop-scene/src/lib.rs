//! Durable, recursive prompt-to-3D scene generation.

#![deny(missing_docs)]

/// Backend capability discovery and Blender execution.
pub mod backends;
/// Strict scene configuration.
pub mod config;
/// Durable staged scene controller.
pub mod controller;
/// Recovery classification and backoff.
pub mod recovery;
/// Accountable multi-agent category-to-prompt expansion.
pub mod prompt_planning;
mod reporting;
mod endpoint;
/// Persistent vector asset retrieval.
pub mod retrieval;
/// Scene-director and scoped role invocation.
pub mod roles;
/// Canonical scene contracts and spatial validation.
pub mod spec;
/// Run database, locking, and artifact promotion.
pub mod storage;
/// Strict queue/task-list contracts.
pub mod tasks;
/// Numerical and cross-format validation reports.
pub mod validation;

pub use config::SceneConfig;
pub use controller::{RunSummary, SceneRunController};
pub use tasks::SceneTaskList;

/// Scene pipeline error.
#[derive(Debug, thiserror::Error)]
pub enum SceneError {
    /// Invalid input or scene contract.
    #[error("validation: {0}")]
    Validation(String),
    /// Local persistence failure.
    #[error("storage: {0}")]
    Storage(String),
    /// Backend execution or capability failure.
    #[error("backend: {0}")]
    Backend(String),
    /// Model invocation or structured-output failure.
    #[error("model: {0}")]
    Model(String),
    /// Run is durably waiting on an external condition.
    #[error("waiting external: {0}")]
    WaitingExternal(String),
    /// Explicit cancellation.
    #[error("cancelled")]
    Cancelled,
    /// Generic error with context.
    #[error(transparent)]
    Other(#[from] anyhow::Error),
}

/// Result alias for the scene subsystem.
pub type Result<T> = std::result::Result<T, SceneError>;
