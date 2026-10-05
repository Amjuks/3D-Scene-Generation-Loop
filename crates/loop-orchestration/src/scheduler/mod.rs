//! Scheduler: dispatch loop coordinating workers on a task graph.
//!
//! Inspired by Orloj's approach to execution scheduling, worker coordination,
//! and artifact management.

pub mod artifacts;
pub mod pool;
pub mod worker;

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use futures::FutureExt;
use tokio::sync::RwLock;
use tokio_util::sync::CancellationToken;

pub use artifacts::{ArtifactStore, ScopedArtifactAccess};
pub use pool::WorkerPool;
pub use worker::{
    ArtifactAccess, SharedMemoryAccess, TaskMemoryAccess, Worker, WorkerContext, WorkerError,
};

use crate::planner::task_graph::{TaskId, TaskKind, TaskNode, TaskStatus};
use crate::workflow::engine::WorkflowEngine;
use crate::workflow::types::{TaskResult, WorkflowError, WorkflowResult};

/// Error from the scheduler.
#[derive(Debug, thiserror::Error)]
pub enum SchedulerError {
    /// Underlying workflow error.
    #[error("workflow error: {0}")]
    Workflow(#[from] WorkflowError),
    /// Worker execution error.
    #[error("worker error: {0}")]
    Worker(#[from] WorkerError),
    /// Scheduler was cancelled.
    #[error("cancelled")]
    Cancelled,
    /// Other error.
    #[error("{0}")]
    Other(String),
}

/// Configuration for the scheduler.
#[derive(Debug, Clone)]
pub struct SchedulerConfig {
    /// Maximum concurrent tasks.
    pub max_concurrency: usize,
    /// Whether to fail the workflow on first task failure.
    pub fail_fast: bool,
}

impl Default for SchedulerConfig {
    fn default() -> Self {
        Self {
            max_concurrency: 4,
            fail_fast: false,
        }
    }
}

/// The scheduler drives task execution across the worker pool.
pub struct Scheduler {
    workflow_engine: Arc<WorkflowEngine>,
    worker_pool: Arc<RwLock<WorkerPool>>,
    artifact_store: Arc<ArtifactStore>,
    shared_memory: Arc<dyn SharedMemoryAccess>,
    cancel: CancellationToken,
    config: SchedulerConfig,
}

impl Scheduler {
    /// Create a new scheduler.
    pub fn new(
        workflow_engine: Arc<WorkflowEngine>,
        worker_pool: WorkerPool,
        shared_memory: Arc<dyn SharedMemoryAccess>,
        config: SchedulerConfig,
    ) -> Self {
        Self {
            workflow_engine,
            worker_pool: Arc::new(RwLock::new(worker_pool)),
            artifact_store: Arc::new(ArtifactStore::new()),
            shared_memory,
            cancel: CancellationToken::new(),
            config,
        }
    }

    /// Get the cancellation token for this scheduler.
    pub fn cancel_token(&self) -> CancellationToken {
        self.cancel.clone()
    }

    /// Access the artifact store.
    pub fn artifact_store(&self) -> &Arc<ArtifactStore> {
        &self.artifact_store
    }

    /// Run the scheduler loop for a workflow until completion or failure.
    pub async fn run(&self, workflow_id: &str) -> Result<WorkflowResult, SchedulerError> {
        let progress = self.workflow_engine.progress_notify();
        let mut running = tokio::task::JoinSet::new();
        let mut dispatched = HashSet::new();

        loop {
            if self.cancel.is_cancelled() {
                running.abort_all();
                while running.join_next().await.is_some() {}
                self.cancel_unfinished(workflow_id).await?;
                return Err(SchedulerError::Cancelled);
            }

            let state = self.workflow_engine.state(workflow_id).await?;

            if state.is_complete() {
                let result = self.workflow_engine.result(workflow_id).await?;
                self.workflow_engine
                    .complete_workflow(workflow_id, result.clone())
                    .await?;
                return Ok(result);
            }

            if self.config.fail_fast && state.has_failures() {
                let result = WorkflowResult {
                    success: false,
                    output: serde_json::json!({ "error": "task failure in fail-fast mode" }),
                    task_results: Vec::new(),
                };
                self.workflow_engine
                    .complete_workflow(workflow_id, result.clone())
                    .await?;
                return Ok(result);
            }

            // Pending descendants of terminally failed prerequisites cannot ever
            // become ready. Make that state explicit instead of hanging forever.
            let mut propagated = false;
            for (task_id, status) in &state.task_statuses {
                if !matches!(status, TaskStatus::Pending | TaskStatus::RetryWait { .. }) {
                    continue;
                }
                let failed: Vec<_> = state
                    .graph
                    .dependencies_of(task_id)
                    .into_iter()
                    .filter(|dep| {
                        matches!(
                            state.task_statuses.get(dep),
                            Some(TaskStatus::Failed(_)) | Some(TaskStatus::Cancelled(_))
                        )
                    })
                    .collect();
                if !failed.is_empty() {
                    self.workflow_engine
                        .task_cancelled(
                            workflow_id,
                            task_id,
                            format!("failed dependency: {}", failed.join(", ")),
                        )
                        .await?;
                    propagated = true;
                }
            }
            if propagated {
                continue;
            }

            let mut ready = if state.status == crate::workflow::types::WorkflowStatus::Paused {
                Vec::new()
            } else {
                state.ready_tasks()
            };
            ready.sort_by(|a, b| {
                let pa = state
                    .graph
                    .tasks
                    .get(a)
                    .map(|t| t.config.priority)
                    .unwrap_or_default();
                let pb = state
                    .graph
                    .tasks
                    .get(b)
                    .map(|t| t.config.priority)
                    .unwrap_or_default();
                pb.cmp(&pa).then_with(|| a.cmp(b))
            });

            for task_id in ready {
                if !dispatched.insert(task_id.clone()) {
                    continue;
                }
                let task_node = state.graph.tasks.get(&task_id).cloned();
                let Some(task_node) = task_node else {
                    dispatched.remove(&task_id);
                    continue;
                };

                let engine = Arc::clone(&self.workflow_engine);
                let pool = Arc::clone(&self.worker_pool);
                let artifact_store = Arc::clone(&self.artifact_store);
                let shared_memory = Arc::clone(&self.shared_memory);
                let cancel = self.cancel.clone();
                let wf_id = workflow_id.to_string();
                let (dep_results, inputs) = self.collect_dependency_results(&state, &task_id);

                running.spawn(async move {
                    Self::execute_task(
                        engine,
                        pool,
                        artifact_store,
                        shared_memory,
                        cancel,
                        wf_id,
                        task_node,
                        dep_results,
                        inputs,
                    )
                    .await
                });
            }

            let retry_sleep = state.next_retry_at_ms().map(|at| {
                let wait = (at - loop_ai::now_ms()).max(1) as u64;
                tokio::time::sleep(std::time::Duration::from_millis(wait))
            });
            tokio::pin!(retry_sleep);
            tokio::select! {
                _ = progress.notified() => {},
                joined = running.join_next(), if !running.is_empty() => {
                    if let Some(Ok(task_id)) = joined { dispatched.remove(&task_id); }
                },
                _ = async { if let Some(s) = retry_sleep.as_mut().as_pin_mut() { s.await } }, if retry_sleep.is_some() => {},
                _ = self.cancel.cancelled() => {
                    running.abort_all();
                    while running.join_next().await.is_some() {}
                    self.cancel_unfinished(workflow_id).await?;
                    return Err(SchedulerError::Cancelled);
                },
            }
        }
    }

    async fn cancel_unfinished(&self, workflow_id: &str) -> Result<(), SchedulerError> {
        let state = self.workflow_engine.state(workflow_id).await?;
        for (id, status) in state.task_statuses {
            if matches!(
                status,
                TaskStatus::Pending
                    | TaskStatus::Ready
                    | TaskStatus::Claimed { .. }
                    | TaskStatus::Running
                    | TaskStatus::RetryWait { .. }
            ) {
                self.workflow_engine
                    .task_cancelled(workflow_id, &id, "workflow cancelled".into())
                    .await?;
            }
        }
        Ok(())
    }

    fn collect_dependency_results(
        &self,
        state: &crate::workflow::types::WorkflowState,
        task_id: &str,
    ) -> (HashMap<TaskId, TaskResult>, HashMap<String, TaskResult>) {
        let deps = state.graph.dependencies_of(task_id);
        let mut results = HashMap::new();
        for dep_id in deps {
            if let Some(TaskStatus::Completed(result)) = state.task_statuses.get(&dep_id) {
                results.insert(dep_id, result.clone());
            }
        }
        let mut inputs = HashMap::new();
        for edge in &state.graph.edges {
            if let crate::planner::task_graph::TaskEdge::DataFlow { from, to, key } = edge {
                if to == task_id {
                    if let Some(TaskStatus::Completed(result)) = state.task_statuses.get(from) {
                        inputs.insert(key.clone(), result.clone());
                    }
                }
            }
        }
        (results, inputs)
    }

    async fn execute_task(
        engine: Arc<WorkflowEngine>,
        pool: Arc<RwLock<WorkerPool>>,
        artifact_store: Arc<ArtifactStore>,
        shared_memory: Arc<dyn SharedMemoryAccess>,
        cancel: CancellationToken,
        workflow_id: String,
        task_node: TaskNode,
        dependency_results: HashMap<TaskId, TaskResult>,
        inputs: HashMap<String, TaskResult>,
    ) -> TaskId {
        let task_id = task_node.id.clone();
        let worker_id = format!("worker_{}", uuid::Uuid::now_v7());

        let _permit = {
            let guard = pool.read().await;
            match guard.acquire_permit().await {
                Ok(permit) => permit,
                Err(_) => return task_id,
            }
        };

        let retry_count = match engine.claim_task(&workflow_id, &task_id, &worker_id).await {
            Ok(Some(count)) => count,
            Ok(None) | Err(_) => return task_id,
        };

        if let Err(e) = engine
            .task_started(&workflow_id, &task_id, &worker_id)
            .await
        {
            tracing::error!("Failed to mark task started: {e}");
            return task_id;
        }

        let kind_str = match &task_node.kind {
            TaskKind::AgentTurn { .. } => "agent_turn",
            TaskKind::ShellCommand { .. } => "shell_command",
            TaskKind::SubWorkflow { .. } => "sub_workflow",
            TaskKind::Barrier => "barrier",
            TaskKind::Custom { worker_type, .. } => worker_type.as_str(),
        };

        if matches!(task_node.kind, TaskKind::Barrier) {
            let _ = engine
                .task_completed(&workflow_id, &task_id, TaskResult::empty())
                .await;
            return task_id;
        }

        let worker = {
            let pool_guard = pool.read().await;
            match pool_guard.find_worker(kind_str) {
                Ok(w) => w,
                Err(e) => {
                    let _ = engine
                        .task_failed(&workflow_id, &task_id, e.to_string(), retry_count)
                        .await;
                    return task_id;
                }
            }
        };

        let signal_rx = engine.signal_router().register_task(&task_id).await;
        let task_cancel = cancel.child_token();

        let task_memory = Arc::new(InMemoryTaskMemory::new(task_id.clone()));

        let context = WorkerContext {
            shared_memory,
            task_memory,
            signal_rx,
            cancel: task_cancel.clone(),
            artifact_store: Arc::new(ScopedArtifactAccess::new(artifact_store, task_id.clone())),
            dependency_results,
            inputs,
        };

        let timeout_ms = task_node.config.timeout_ms;
        let execution =
            std::panic::AssertUnwindSafe(worker.execute(&task_node, context)).catch_unwind();
        let result = if timeout_ms > 0 {
            let duration = std::time::Duration::from_millis(timeout_ms);
            match tokio::time::timeout(duration, execution).await {
                Ok(Ok(result)) => result,
                Ok(Err(_)) => Err(WorkerError::ExecutionFailed(
                    "worker panicked at recovery boundary".into(),
                )),
                Err(_) => {
                    task_cancel.cancel();
                    Err(WorkerError::TimedOut)
                }
            }
        } else {
            match execution.await {
                Ok(result) => result,
                Err(_) => Err(WorkerError::ExecutionFailed(
                    "worker panicked at recovery boundary".into(),
                )),
            }
        };

        match result {
            Ok(task_result) => {
                let _ = engine
                    .task_completed(&workflow_id, &task_id, task_result)
                    .await;
            }
            Err(WorkerError::Cancelled) => {
                let _ = engine
                    .task_cancelled(&workflow_id, &task_id, "cancelled".to_string())
                    .await;
            }
            Err(e) => {
                if retry_count < task_node.config.max_retries {
                    let next_retry = retry_count + 1;
                    let base_ms = 250u64.saturating_mul(1u64 << next_retry.min(10));
                    let jitter = (uuid::Uuid::now_v7().as_u128() % 101) as u64;
                    let ready_at = loop_ai::now_ms() + (base_ms + jitter) as i64;
                    let _ = engine
                        .task_retry_scheduled(
                            &workflow_id,
                            &task_id,
                            e.to_string(),
                            next_retry,
                            ready_at,
                        )
                        .await;
                } else {
                    let _ = engine
                        .task_failed(&workflow_id, &task_id, e.to_string(), retry_count)
                        .await;
                }
            }
        }
        task_id
    }

    /// Cancel the workflow execution.
    pub fn cancel(&self) {
        self.cancel.cancel();
    }
}

/// Simple in-memory task memory implementation.
struct InMemoryTaskMemory {
    #[allow(dead_code)]
    task_id: TaskId,
    store: tokio::sync::RwLock<HashMap<String, serde_json::Value>>,
}

impl InMemoryTaskMemory {
    fn new(task_id: TaskId) -> Self {
        Self {
            task_id,
            store: tokio::sync::RwLock::new(HashMap::new()),
        }
    }
}

#[async_trait::async_trait]
impl TaskMemoryAccess for InMemoryTaskMemory {
    async fn get(&self, key: &str) -> Option<serde_json::Value> {
        self.store.read().await.get(key).cloned()
    }

    async fn set(&self, key: &str, value: serde_json::Value) {
        self.store.write().await.insert(key.to_string(), value);
    }

    async fn list_keys(&self) -> Vec<String> {
        self.store.read().await.keys().cloned().collect()
    }

    async fn delete(&self, key: &str) -> bool {
        self.store.write().await.remove(key).is_some()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::memory::bus::create_memory_bus;
    use crate::memory::SharedMemory;
    use crate::planner::{TaskGraph, TaskKind};
    use crate::workflow::{EventLog, MemoryEventLog};
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct ProbeWorker {
        active: AtomicUsize,
        max_active: AtomicUsize,
        executions: tokio::sync::Mutex<HashMap<String, usize>>,
        fail_first: bool,
        fail_always: HashSet<String>,
    }

    #[async_trait::async_trait]
    impl Worker for ProbeWorker {
        fn supported_task_kinds(&self) -> &[&str] {
            &["probe"]
        }
        async fn execute(
            &self,
            task: &TaskNode,
            ctx: WorkerContext,
        ) -> Result<TaskResult, WorkerError> {
            let now = self.active.fetch_add(1, Ordering::SeqCst) + 1;
            self.max_active.fetch_max(now, Ordering::SeqCst);
            tokio::time::sleep(std::time::Duration::from_millis(20)).await;
            self.active.fetch_sub(1, Ordering::SeqCst);
            let mut executions = self.executions.lock().await;
            let count = executions.entry(task.id.clone()).or_default();
            *count += 1;
            if self.fail_always.contains(&task.id) || (self.fail_first && *count == 1) {
                return Err(WorkerError::ExecutionFailed("injected transient".into()));
            }
            if task.id == "sink" && !ctx.inputs.contains_key("named_input") {
                return Err(WorkerError::ExecutionFailed("missing named binding".into()));
            }
            Ok(TaskResult::with_output(
                serde_json::json!({"id": task.id, "attempt": *count}),
            ))
        }
    }

    async fn setup(
        graph: TaskGraph,
        worker: Arc<ProbeWorker>,
        concurrency: usize,
    ) -> (Arc<WorkflowEngine>, Scheduler) {
        let log: Arc<dyn EventLog> = Arc::new(MemoryEventLog::new());
        let engine = Arc::new(WorkflowEngine::new(log));
        engine.start_workflow("wf".into(), graph).await.unwrap();
        let mut pool = WorkerPool::new(concurrency);
        pool.register(worker);
        let memory = Arc::new(SharedMemory::new(create_memory_bus()));
        let scheduler = Scheduler::new(
            engine.clone(),
            pool,
            memory,
            SchedulerConfig {
                max_concurrency: concurrency,
                fail_fast: false,
            },
        );
        (engine, scheduler)
    }

    fn task(id: &str, retries: u32) -> TaskNode {
        TaskNode::new(
            id,
            TaskKind::Custom {
                worker_type: "probe".into(),
                params: serde_json::Value::Null,
            },
            id,
        )
        .with_config(crate::planner::TaskConfig {
            max_retries: retries,
            timeout_ms: 2_000,
            priority: 0,
        })
    }

    #[tokio::test]
    async fn permits_atomic_claims_and_retries_are_enforced() {
        let mut graph = TaskGraph::new();
        for i in 0..6 {
            graph.add_task(task(&format!("t{i}"), 1));
        }
        let worker = Arc::new(ProbeWorker {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            executions: tokio::sync::Mutex::new(HashMap::new()),
            fail_first: true,
            fail_always: HashSet::new(),
        });
        let (_engine, scheduler) = setup(graph, worker.clone(), 2).await;
        let result = scheduler.run("wf").await.unwrap();
        assert!(result.success);
        assert!(worker.max_active.load(Ordering::SeqCst) <= 2);
        assert!(worker.executions.lock().await.values().all(|n| *n == 2));
    }

    #[tokio::test]
    async fn failed_dependency_is_explicit_and_dataflow_is_named() {
        let mut graph = TaskGraph::new();
        graph.add_task(task("source", 0));
        graph.add_task(task("sink", 0));
        graph.add_data_flow("source", "sink", "named_input");
        let worker = Arc::new(ProbeWorker {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            executions: tokio::sync::Mutex::new(HashMap::new()),
            fail_first: false,
            fail_always: HashSet::new(),
        });
        let (_engine, scheduler) = setup(graph, worker, 1).await;
        let result = scheduler.run("wf").await.unwrap();
        assert!(result.success);
        assert_eq!(result.output["id"], "sink");

        let mut graph = TaskGraph::new();
        graph.add_task(task("bad", 0));
        graph.add_task(task("child", 0));
        graph.add_dependency("child", "bad");
        let worker = Arc::new(ProbeWorker {
            active: AtomicUsize::new(0),
            max_active: AtomicUsize::new(0),
            executions: tokio::sync::Mutex::new(HashMap::new()),
            fail_first: false,
            fail_always: HashSet::from(["bad".into()]),
        });
        let (engine, scheduler) = setup(graph, worker, 1).await;
        let result = scheduler.run("wf").await.unwrap();
        assert!(!result.success);
        let state = engine.state("wf").await.unwrap();
        assert!(matches!(
            state.task_statuses["child"],
            TaskStatus::Cancelled(_)
        ));
    }
}
