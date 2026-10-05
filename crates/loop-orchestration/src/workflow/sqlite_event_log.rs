//! SQLite-backed event log for durable workflow persistence.

#[cfg(feature = "sqlite")]
mod sqlite_impl {
    use std::sync::Arc;

    use async_trait::async_trait;
    use rusqlite::params;
    use tokio::sync::Mutex;

    use crate::planner::task_graph::TaskGraph;
    use crate::workflow::event_log::EventLog;
    use crate::workflow::types::{SeqNo, WorkflowError, WorkflowEvent, WorkflowState};

    /// SQLite-backed event log.
    pub struct SqliteEventLog {
        conn: Arc<Mutex<rusqlite::Connection>>,
    }

    impl SqliteEventLog {
        /// Create a new SQLite event log at the given path.
        pub fn new(path: &str) -> Result<Self, WorkflowError> {
            let conn = rusqlite::Connection::open(path)
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;

            conn.execute_batch(
                "CREATE TABLE IF NOT EXISTS workflow_events (
                    id INTEGER PRIMARY KEY AUTOINCREMENT,
                    workflow_id TEXT NOT NULL,
                    seq INTEGER NOT NULL,
                    event_json TEXT NOT NULL,
                    created_at INTEGER NOT NULL
                );
                CREATE INDEX IF NOT EXISTS idx_wf_events_workflow
                    ON workflow_events(workflow_id, seq);
                CREATE TABLE IF NOT EXISTS workflow_graphs (
                    workflow_id TEXT PRIMARY KEY,
                    graph_json TEXT NOT NULL
                );",
            )
            .map_err(|e| WorkflowError::EventLog(e.to_string()))?;

            Ok(Self {
                conn: Arc::new(Mutex::new(conn)),
            })
        }

        /// Create an in-memory SQLite event log (useful for testing).
        pub fn in_memory() -> Result<Self, WorkflowError> {
            Self::new(":memory:")
        }
    }

    #[async_trait]
    impl EventLog for SqliteEventLog {
        async fn append(
            &self,
            workflow_id: &str,
            event: WorkflowEvent,
        ) -> Result<SeqNo, WorkflowError> {
            let mut conn = self.conn.lock().await;
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;

            if let WorkflowEvent::WorkflowStarted { plan, .. } = &event {
                let graph_json = serde_json::to_string(plan)
                    .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
                tx.execute(
                    "INSERT OR REPLACE INTO workflow_graphs (workflow_id, graph_json) VALUES (?1, ?2)",
                    params![workflow_id, graph_json],
                )
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
            }
            if let WorkflowEvent::TaskScheduled {
                task: Some(task),
                dependencies,
                ..
            } = &event
            {
                let current: String = tx
                    .query_row(
                        "SELECT graph_json FROM workflow_graphs WHERE workflow_id = ?1",
                        params![workflow_id],
                        |row| row.get(0),
                    )
                    .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
                let mut graph: TaskGraph = serde_json::from_str(&current)
                    .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
                if graph.tasks.contains_key(&task.id) {
                    return Err(WorkflowError::InvalidState(format!(
                        "task already exists: {}",
                        task.id
                    )));
                }
                graph.add_task(task.clone());
                for dep in dependencies {
                    graph.add_dependency(task.id.clone(), dep.clone());
                }
                graph.validate().map_err(WorkflowError::InvalidState)?;
                let graph_json = serde_json::to_string(&graph)
                    .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
                tx.execute(
                    "UPDATE workflow_graphs SET graph_json = ?2 WHERE workflow_id = ?1",
                    params![workflow_id, graph_json],
                )
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
            }

            let event_json = serde_json::to_string(&event)
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;

            let seq: SeqNo = tx
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) + 1 FROM workflow_events WHERE workflow_id = ?1",
                    params![workflow_id],
                    |row| row.get(0),
                )
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;

            let now = loop_ai::now_ms();
            tx.execute(
                "INSERT INTO workflow_events (workflow_id, seq, event_json, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![workflow_id, seq as i64, event_json, now],
            )
            .map_err(|e| WorkflowError::EventLog(e.to_string()))?;

            tx.commit()
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
            Ok(seq)
        }

        async fn read(
            &self,
            workflow_id: &str,
            after_seq: SeqNo,
        ) -> Result<Vec<(SeqNo, WorkflowEvent)>, WorkflowError> {
            let conn = self.conn.lock().await;

            let mut stmt = conn
                .prepare(
                    "SELECT seq, event_json FROM workflow_events WHERE workflow_id = ?1 AND seq > ?2 ORDER BY seq",
                )
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;

            let rows = stmt
                .query_map(params![workflow_id, after_seq as i64], |row| {
                    let seq: i64 = row.get(0)?;
                    let json: String = row.get(1)?;
                    Ok((seq as SeqNo, json))
                })
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;

            let mut results = Vec::new();
            for row in rows {
                let (seq, json) = row.map_err(|e| WorkflowError::EventLog(e.to_string()))?;
                let event: WorkflowEvent = serde_json::from_str(&json)
                    .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
                results.push((seq, event));
            }
            Ok(results)
        }

        async fn replay(&self, workflow_id: &str) -> Result<WorkflowState, WorkflowError> {
            let conn = self.conn.lock().await;

            let graph_json: String = conn
                .query_row(
                    "SELECT graph_json FROM workflow_graphs WHERE workflow_id = ?1",
                    params![workflow_id],
                    |row| row.get(0),
                )
                .map_err(|_| WorkflowError::NotFound(workflow_id.to_string()))?;

            let graph: TaskGraph = serde_json::from_str(&graph_json)
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;

            let mut stmt = conn
                .prepare(
                    "SELECT seq, event_json FROM workflow_events WHERE workflow_id = ?1 ORDER BY seq",
                )
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;

            let rows = stmt
                .query_map(params![workflow_id], |row| {
                    let seq: i64 = row.get(0)?;
                    let json: String = row.get(1)?;
                    Ok((seq as SeqNo, json))
                })
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;

            let mut state = WorkflowState::new(workflow_id.to_string(), graph);
            for row in rows {
                let (seq, json) = row.map_err(|e| WorkflowError::EventLog(e.to_string()))?;
                let event: WorkflowEvent = serde_json::from_str(&json)
                    .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
                state.apply(seq, &event);
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
            let mut conn = self.conn.lock().await;
            let tx = conn
                .transaction_with_behavior(rusqlite::TransactionBehavior::Immediate)
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
            let graph_json: String = tx
                .query_row(
                    "SELECT graph_json FROM workflow_graphs WHERE workflow_id = ?1",
                    params![workflow_id],
                    |row| row.get(0),
                )
                .map_err(|_| WorkflowError::NotFound(workflow_id.to_string()))?;
            let graph: TaskGraph = serde_json::from_str(&graph_json)
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
            let mut stmt = tx.prepare(
                "SELECT seq, event_json FROM workflow_events WHERE workflow_id = ?1 ORDER BY seq"
            ).map_err(|e| WorkflowError::EventLog(e.to_string()))?;
            let rows = stmt
                .query_map(params![workflow_id], |row| {
                    Ok((row.get::<_, i64>(0)? as SeqNo, row.get::<_, String>(1)?))
                })
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
            let mut state = WorkflowState::new(workflow_id.to_string(), graph);
            for row in rows {
                let (seq, json) = row.map_err(|e| WorkflowError::EventLog(e.to_string()))?;
                let event: WorkflowEvent = serde_json::from_str(&json)
                    .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
                state.apply(seq, &event);
            }
            drop(stmt);
            let retry_count = match state.task_statuses.get(task_id) {
                Some(crate::planner::task_graph::TaskStatus::Pending) => 0,
                Some(crate::planner::task_graph::TaskStatus::RetryWait {
                    retry_count,
                    ready_at_ms,
                    ..
                }) if *ready_at_ms <= timestamp => *retry_count,
                _ => return Ok(None),
            };
            let seq: i64 = tx
                .query_row(
                    "SELECT COALESCE(MAX(seq), 0) + 1 FROM workflow_events WHERE workflow_id = ?1",
                    params![workflow_id],
                    |row| row.get(0),
                )
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
            let event = WorkflowEvent::TaskClaimed {
                task_id: task_id.to_string(),
                worker_id: worker_id.to_string(),
                retry_count,
                timestamp,
            };
            let json = serde_json::to_string(&event)
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
            tx.execute(
                "INSERT INTO workflow_events (workflow_id, seq, event_json, created_at) VALUES (?1, ?2, ?3, ?4)",
                params![workflow_id, seq, json, timestamp],
            ).map_err(|e| WorkflowError::EventLog(e.to_string()))?;
            tx.commit()
                .map_err(|e| WorkflowError::EventLog(e.to_string()))?;
            Ok(Some(retry_count))
        }
    }
}

#[cfg(feature = "sqlite")]
pub use sqlite_impl::SqliteEventLog;
