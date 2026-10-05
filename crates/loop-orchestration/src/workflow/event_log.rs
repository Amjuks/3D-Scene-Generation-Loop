//! Event log: append-only durable record of workflow events.

use std::collections::HashMap;
use std::sync::Arc;

use async_trait::async_trait;
use tokio::sync::{Mutex, Notify};

use super::types::{SeqNo, WorkflowError, WorkflowEvent, WorkflowId, WorkflowState};
use crate::planner::task_graph::TaskGraph;

/// Durable event log for workflow history and replay.
#[async_trait]
pub trait EventLog: Send + Sync {
    /// Append an event and return its sequence number.
    async fn append(&self, workflow_id: &str, event: WorkflowEvent)
        -> Result<SeqNo, WorkflowError>;

    /// Read events after a given sequence number.
    async fn read(
        &self,
        workflow_id: &str,
        after_seq: SeqNo,
    ) -> Result<Vec<(SeqNo, WorkflowEvent)>, WorkflowError>;

    /// Replay the full event log to reconstruct workflow state.
    async fn replay(&self, workflow_id: &str) -> Result<WorkflowState, WorkflowError>;

    /// Atomically claim a pending or eligible retry task. Returns the retry
    /// number when this caller won the claim and `None` otherwise.
    async fn claim_task(
        &self,
        workflow_id: &str,
        task_id: &str,
        worker_id: &str,
        timestamp: i64,
    ) -> Result<Option<u32>, WorkflowError>;
}

/// In-memory event log for development and testing.
pub struct MemoryEventLog {
    logs: Mutex<HashMap<WorkflowId, Vec<(SeqNo, WorkflowEvent)>>>,
    graphs: Mutex<HashMap<WorkflowId, TaskGraph>>,
    notify: Arc<Notify>,
}

impl MemoryEventLog {
    /// Create a new empty in-memory event log.
    pub fn new() -> Self {
        Self {
            logs: Mutex::new(HashMap::new()),
            graphs: Mutex::new(HashMap::new()),
            notify: Arc::new(Notify::new()),
        }
    }

    /// Get a Notify handle to await new events.
    pub fn notifier(&self) -> Arc<Notify> {
        Arc::clone(&self.notify)
    }
}

impl Default for MemoryEventLog {
    fn default() -> Self {
        Self::new()
    }
}

#[async_trait]
impl EventLog for MemoryEventLog {
    async fn append(
        &self,
        workflow_id: &str,
        event: WorkflowEvent,
    ) -> Result<SeqNo, WorkflowError> {
        let mut logs = self.logs.lock().await;
        let entries = logs.entry(workflow_id.to_string()).or_default();
        let seq = entries.len() as SeqNo + 1;

        if let WorkflowEvent::WorkflowStarted { plan, .. } = &event {
            self.graphs
                .lock()
                .await
                .insert(workflow_id.to_string(), plan.clone());
        }

        entries.push((seq, event));
        self.notify.notify_waiters();
        Ok(seq)
    }

    async fn read(
        &self,
        workflow_id: &str,
        after_seq: SeqNo,
    ) -> Result<Vec<(SeqNo, WorkflowEvent)>, WorkflowError> {
        let logs = self.logs.lock().await;
        let entries = logs
            .get(workflow_id)
            .ok_or_else(|| WorkflowError::NotFound(workflow_id.to_string()))?;
        Ok(entries
            .iter()
            .filter(|(seq, _)| *seq > after_seq)
            .cloned()
            .collect())
    }

    async fn replay(&self, workflow_id: &str) -> Result<WorkflowState, WorkflowError> {
        let logs = self.logs.lock().await;
        let entries = logs
            .get(workflow_id)
            .ok_or_else(|| WorkflowError::NotFound(workflow_id.to_string()))?;

        let graph = self
            .graphs
            .lock()
            .await
            .get(workflow_id)
            .cloned()
            .ok_or_else(|| {
                WorkflowError::InvalidState("no graph found for workflow".to_string())
            })?;

        let mut state = WorkflowState::new(workflow_id.to_string(), graph);
        for (seq, event) in entries {
            state.apply(*seq, event);
        }
        Ok(state)
    }

    async fn claim_task(
        &self,
        workflow_id: &str,
        task_id: &str,
        worker_id: &str,
        timestamp: i64,
    ) -> Result<Option<u32>, WorkflowError> {
        // The log mutex is the transaction boundary for the memory backend.
        let mut logs = self.logs.lock().await;
        let entries = logs
            .get_mut(workflow_id)
            .ok_or_else(|| WorkflowError::NotFound(workflow_id.to_string()))?;
        let graph = self
            .graphs
            .lock()
            .await
            .get(workflow_id)
            .cloned()
            .ok_or_else(|| WorkflowError::InvalidState("no graph found for workflow".into()))?;
        let mut state = WorkflowState::new(workflow_id.to_string(), graph);
        for (seq, event) in entries.iter() {
            state.apply(*seq, event);
        }
        let retry_count = match state.task_statuses.get(task_id) {
            Some(crate::planner::task_graph::TaskStatus::Pending) => 0,
            Some(crate::planner::task_graph::TaskStatus::RetryWait {
                retry_count,
                ready_at_ms,
                ..
            }) if *ready_at_ms <= timestamp => *retry_count,
            _ => return Ok(None),
        };
        let seq = entries.len() as SeqNo + 1;
        entries.push((
            seq,
            WorkflowEvent::TaskClaimed {
                task_id: task_id.to_string(),
                worker_id: worker_id.to_string(),
                retry_count,
                timestamp,
            },
        ));
        self.notify.notify_waiters();
        Ok(Some(retry_count))
    }
}
