//! Durable run state, ownership locking, and atomic artifact promotion.

use crate::{Result, SceneError};
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};
use sha2::{Digest, Sha256};
use std::fs::{self, OpenOptions};
use std::io::Write;
use std::path::{Path, PathBuf};

/// Run lifecycle; completion is only `accepted` after final validation.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum RunState {
    /// Actively progressing.
    Running,
    /// Waiting for credentials/runtime/resource.
    WaitingExternal,
    /// Paused at a durable budget boundary.
    WaitingResource,
    /// Explicitly cancelled.
    Cancelled,
    /// Terminal policy exhaustion.
    Failed,
    /// Final formats and acceptance evidence passed.
    Accepted,
}

/// Detailed task/attempt state used by the scene controller.
#[derive(Debug, Clone, Copy, Serialize, Deserialize, PartialEq, Eq)]
#[serde(rename_all = "snake_case")]
pub enum SceneTaskState {
    /// Awaiting dependencies.
    Pending,
    /// Lease obtained.
    Claimed,
    /// Worker/process started.
    Running,
    /// Output is being checked.
    Validating,
    /// Promoted and accepted.
    Accepted,
    /// Persisted backoff timer.
    RetryWait,
    /// Awaiting repair planning.
    RepairPlanning,
    /// Waiting on external condition.
    WaitingExternal,
    /// Replaced by a newer revision.
    Superseded,
    /// Explicitly cancelled.
    Cancelled,
    /// Recovery policy exhausted.
    Failed,
}

/// One accepted or quarantined artifact record.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ArtifactRecord {
    /// SHA-256 content identity.
    pub content_hash: String,
    /// Producing task ID.
    pub task_id: String,
    /// Artifact kind.
    pub kind: String,
    /// Absolute or run-relative path.
    pub path: PathBuf,
    /// Size in bytes.
    pub bytes: u64,
    /// Whether validation accepted the artifact.
    pub accepted: bool,
    /// JSON validation evidence.
    pub evidence: serde_json::Value,
}

/// Durable run database handle.
pub struct RunStore {
    root: PathBuf,
    conn: Connection,
}

impl RunStore {
    /// Create/open a run directory and migrate its database.
    pub fn open(root: impl Into<PathBuf>) -> Result<Self> {
        let root = root.into();
        fs::create_dir_all(&root).map_err(storage)?;
        for sub in [
            "input",
            "designs",
            "attempts",
            "artifacts",
            "assemblies",
            "previews",
            "validation",
            "final",
            "logs",
        ] {
            fs::create_dir_all(root.join(sub)).map_err(storage)?;
        }
        let conn = Connection::open(root.join("run.db")).map_err(storage)?;
        conn.pragma_update(None, "journal_mode", "WAL")
            .map_err(storage)?;
        conn.execute_batch("PRAGMA foreign_keys=ON;
          CREATE TABLE IF NOT EXISTS run (id TEXT PRIMARY KEY, task_id TEXT NOT NULL, state TEXT NOT NULL, prompt_hash TEXT NOT NULL, config_hash TEXT NOT NULL, created_at INTEGER NOT NULL, updated_at INTEGER NOT NULL, blocker TEXT, model_calls INTEGER NOT NULL DEFAULT 0, tokens INTEGER NOT NULL DEFAULT 0);
          CREATE TABLE IF NOT EXISTS node_revisions (node_id TEXT NOT NULL, revision INTEGER NOT NULL, parent_revision INTEGER, spec_json TEXT NOT NULL, spec_hash TEXT NOT NULL, accepted INTEGER NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY(node_id,revision));
          CREATE TABLE IF NOT EXISTS scene_tasks (task_id TEXT PRIMARY KEY, node_id TEXT, stage TEXT NOT NULL, input_hash TEXT NOT NULL, state TEXT NOT NULL, priority INTEGER NOT NULL DEFAULT 0, attempt INTEGER NOT NULL DEFAULT 0, lease_owner TEXT, heartbeat_at INTEGER, next_retry_at INTEGER, failure_class TEXT, failure_fingerprint TEXT, last_error TEXT, output_json TEXT);
          CREATE TABLE IF NOT EXISTS dependencies (task_id TEXT NOT NULL, depends_on TEXT NOT NULL, binding TEXT, PRIMARY KEY(task_id,depends_on,binding));
          CREATE TABLE IF NOT EXISTS attempts (attempt_id TEXT PRIMARY KEY, task_id TEXT NOT NULL, number INTEGER NOT NULL, state TEXT NOT NULL, directory TEXT NOT NULL, started_at INTEGER NOT NULL, heartbeat_at INTEGER NOT NULL, ended_at INTEGER, exit_code INTEGER, failure_class TEXT, fingerprint TEXT, repair_json TEXT, stdout_path TEXT, stderr_path TEXT);
          CREATE TABLE IF NOT EXISTS artifacts (content_hash TEXT NOT NULL, task_id TEXT NOT NULL, kind TEXT NOT NULL, path TEXT NOT NULL, bytes INTEGER NOT NULL, accepted INTEGER NOT NULL, evidence_json TEXT NOT NULL, created_at INTEGER NOT NULL, PRIMARY KEY(content_hash,kind));
          CREATE TABLE IF NOT EXISTS findings (id TEXT PRIMARY KEY, node_id TEXT, severity TEXT NOT NULL, status TEXT NOT NULL, evidence_json TEXT NOT NULL, created_at INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS recovery (id INTEGER PRIMARY KEY AUTOINCREMENT, task_id TEXT NOT NULL, failure_class TEXT NOT NULL, fingerprint TEXT NOT NULL, action TEXT NOT NULL, next_retry_at INTEGER, evidence_json TEXT NOT NULL, created_at INTEGER NOT NULL);
          CREATE TABLE IF NOT EXISTS commits (id TEXT PRIMARY KEY, source_path TEXT NOT NULL, destination_path TEXT NOT NULL, content_hash TEXT NOT NULL, state TEXT NOT NULL, created_at INTEGER NOT NULL);
        ").map_err(storage)?;
        Ok(Self { root, conn })
    }
    /// Run directory.
    pub fn root(&self) -> &Path {
        &self.root
    }
    /// Initialize immutable run identity and input hashes.
    pub fn initialize(
        &self,
        run_id: &str,
        task_id: &str,
        prompt: &str,
        config: &serde_json::Value,
        task: &serde_json::Value,
    ) -> Result<()> {
        let now = loop_ai::now_ms();
        let prompt_hash = hash_bytes(prompt.as_bytes());
        let config_bytes = serde_json::to_vec(config).map_err(storage)?;
        let config_hash = hash_bytes(&config_bytes);
        self.conn.execute("INSERT OR IGNORE INTO run(id,task_id,state,prompt_hash,config_hash,created_at,updated_at) VALUES(?1,?2,'running',?3,?4,?5,?5)",params![run_id,task_id,prompt_hash,config_hash,now]).map_err(storage)?;
        atomic_write(&self.root.join("input/prompt.txt"), prompt.as_bytes())?;
        atomic_write(&self.root.join("input/resolved-config.json"), &config_bytes)?;
        atomic_write(
            &self.root.join("input/task.json"),
            &serde_json::to_vec_pretty(task).map_err(storage)?,
        )?;
        Ok(())
    }
    /// Current state and optional blocker.
    pub fn status(&self) -> Result<(String, RunState, Option<String>)> {
        self.conn
            .query_row("SELECT id,state,blocker FROM run LIMIT 1", [], |r| {
                let s: String = r.get(1)?;
                Ok((r.get(0)?, parse_run_state(&s), r.get(2)?))
            })
            .map_err(storage)
    }
    /// Set run state and blocker atomically.
    pub fn set_state(&self, state: RunState, blocker: Option<&str>) -> Result<()> {
        self.conn
            .execute(
                "UPDATE run SET state=?1,blocker=?2,updated_at=?3",
                params![state_name(state), blocker, loop_ai::now_ms()],
            )
            .map_err(storage)?;
        Ok(())
    }
    /// Add provider-reported usage and return cumulative calls/tokens.
    pub fn add_model_usage(&self, tokens: u64) -> Result<(u32, u64)> {
        self.conn
            .execute(
                "UPDATE run SET model_calls=model_calls+1,tokens=tokens+?1,updated_at=?2",
                params![tokens, loop_ai::now_ms()],
            )
            .map_err(storage)?;
        self.conn
            .query_row("SELECT model_calls,tokens FROM run LIMIT 1", [], |r| {
                Ok((r.get(0)?, r.get(1)?))
            })
            .map_err(storage)
    }
    /// Count prior recovery records with the same fingerprint.
    pub fn recovery_count(&self, fingerprint: &str) -> Result<u32> {
        self.conn
            .query_row(
                "SELECT COUNT(*) FROM recovery WHERE fingerprint=?1",
                params![fingerprint],
                |r| r.get(0),
            )
            .map_err(storage)
    }
    /// Persist an accepted node revision and readable Markdown alongside it.
    pub fn accept_node(&self, node: &crate::spec::SceneNode, markdown: &str) -> Result<String> {
        let json = serde_json::to_vec_pretty(node).map_err(storage)?;
        let hash = hash_bytes(&json);
        let latest: Option<(u32, String)> = self
            .conn
            .query_row(
                "SELECT revision,spec_hash FROM node_revisions WHERE node_id=?1 AND accepted=1 ORDER BY revision DESC LIMIT 1",
                params![node.node_id],
                |r| Ok((r.get(0)?, r.get(1)?)),
            )
            .optional()
            .map_err(storage)?;
        if let Some((revision, old_hash)) = latest {
            if revision == node.revision && old_hash == hash {
                return Ok(hash);
            }
            if node.revision <= revision {
                return Err(SceneError::Validation(format!(
                    "stale/conflicting revision {} for {}; latest accepted is {}",
                    node.revision, node.node_id, revision
                )));
            }
        }
        let tx = self.conn.unchecked_transaction().map_err(storage)?;
        tx.execute("INSERT OR REPLACE INTO node_revisions(node_id,revision,parent_revision,spec_json,spec_hash,accepted,created_at) VALUES(?1,?2,?3,?4,?5,1,?6)",params![node.node_id,node.revision,node.provenance.parent_revision, String::from_utf8_lossy(&json),hash,loop_ai::now_ms()]).map_err(storage)?;
        let dir = self
            .root
            .join("designs")
            .join(safe_id(&node.node_id))
            .join(node.revision.to_string());
        fs::create_dir_all(&dir).map_err(storage)?;
        atomic_write(&dir.join("spec.json"), &json)?;
        atomic_write(&dir.join("design.md"), markdown.as_bytes())?;
        tx.commit().map_err(storage)?;
        Ok(hash)
    }
    /// Load the newest accepted scene nodes.
    pub fn accepted_nodes(&self) -> Result<Vec<crate::spec::SceneNode>> {
        let mut st=self.conn.prepare("SELECT n.spec_json FROM node_revisions n JOIN (SELECT node_id,MAX(revision) rev FROM node_revisions WHERE accepted=1 GROUP BY node_id) x ON n.node_id=x.node_id AND n.revision=x.rev ORDER BY n.node_id").map_err(storage)?;
        let rows = st
            .query_map([], |r| r.get::<_, String>(0))
            .map_err(storage)?;
        let mut out = Vec::new();
        for r in rows {
            out.push(serde_json::from_str(&r.map_err(storage)?).map_err(storage)?);
        }
        Ok(out)
    }
    /// Create/update a deterministic scene task.
    pub fn upsert_task(
        &self,
        id: &str,
        node_id: Option<&str>,
        stage: &str,
        input_hash: &str,
        priority: i32,
    ) -> Result<SceneTaskState> {
        self.conn.execute("INSERT INTO scene_tasks(task_id,node_id,stage,input_hash,state,priority) VALUES(?1,?2,?3,?4,'pending',?5)
            ON CONFLICT(task_id) DO UPDATE SET node_id=excluded.node_id,stage=excluded.stage,input_hash=excluded.input_hash,state='pending',attempt=0,lease_owner=NULL,next_retry_at=NULL,last_error='input identity changed; prior result superseded'
            WHERE scene_tasks.input_hash <> excluded.input_hash",params![id,node_id,stage,input_hash,priority]).map_err(storage)?;
        let state: String = self
            .conn
            .query_row(
                "SELECT state FROM scene_tasks WHERE task_id=?1",
                params![id],
                |r| r.get(0),
            )
            .map_err(storage)?;
        Ok(parse_task_state(&state))
    }
    /// Atomically lease a pending/eligible task.
    pub fn claim_task(&self, id: &str, owner: &str) -> Result<Option<u32>> {
        let tx = self.conn.unchecked_transaction().map_err(storage)?;
        let row: Option<(String, u32, Option<i64>)> = tx
            .query_row(
                "SELECT state,attempt,next_retry_at FROM scene_tasks WHERE task_id=?1",
                params![id],
                |r| Ok((r.get(0)?, r.get(1)?, r.get(2)?)),
            )
            .optional()
            .map_err(storage)?;
        let Some((state, attempt, next)) = row else {
            return Ok(None);
        };
        let now = loop_ai::now_ms();
        if state != "pending" && !(state == "retry_wait" && next.unwrap_or(i64::MAX) <= now) {
            return Ok(None);
        };
        let n=tx.execute("UPDATE scene_tasks SET state='claimed',lease_owner=?2,heartbeat_at=?3,attempt=attempt+1 WHERE task_id=?1 AND state=?4",params![id,owner,now,state]).map_err(storage)?;
        if n == 0 {
            return Ok(None);
        };
        tx.commit().map_err(storage)?;
        Ok(Some(attempt + 1))
    }
    /// Update task state and structured output/error.
    pub fn set_task_state(
        &self,
        id: &str,
        state: SceneTaskState,
        error: Option<&str>,
        output: Option<&serde_json::Value>,
    ) -> Result<()> {
        let output = output
            .map(serde_json::to_string)
            .transpose()
            .map_err(storage)?;
        self.conn.execute("UPDATE scene_tasks SET state=?2,last_error=?3,output_json=?4,heartbeat_at=?5 WHERE task_id=?1",params![id,task_state_name(state),error,output,loop_ai::now_ms()]).map_err(storage)?;
        Ok(())
    }
    /// Persist a retry/backoff recovery decision.
    pub fn schedule_retry(
        &self,
        id: &str,
        class: &str,
        fingerprint: &str,
        action: &str,
        ready_at: i64,
        evidence: &serde_json::Value,
    ) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(storage)?;
        tx.execute("UPDATE scene_tasks SET state='retry_wait',next_retry_at=?2,failure_class=?3,failure_fingerprint=?4,last_error=?5 WHERE task_id=?1",params![id,ready_at,class,fingerprint,action]).map_err(storage)?;
        tx.execute("INSERT INTO recovery(task_id,failure_class,fingerprint,action,next_retry_at,evidence_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7)",params![id,class,fingerprint,action,ready_at,serde_json::to_string(evidence).map_err(storage)?,loop_ai::now_ms()]).map_err(storage)?;
        tx.commit().map_err(storage)?;
        Ok(())
    }
    /// Register an artifact after promotion.
    pub fn record_artifact(&self, record: &ArtifactRecord) -> Result<()> {
        self.conn.execute("INSERT OR REPLACE INTO artifacts(content_hash,task_id,kind,path,bytes,accepted,evidence_json,created_at) VALUES(?1,?2,?3,?4,?5,?6,?7,?8)",params![record.content_hash,record.task_id,record.kind,record.path.to_string_lossy(),record.bytes,record.accepted as i32,serde_json::to_string(&record.evidence).map_err(storage)?,loop_ai::now_ms()]).map_err(storage)?;
        Ok(())
    }
    /// List accepted artifacts.
    pub fn artifacts(&self) -> Result<Vec<ArtifactRecord>> {
        let mut st=self.conn.prepare("SELECT content_hash,task_id,kind,path,bytes,accepted,evidence_json FROM artifacts ORDER BY kind,content_hash").map_err(storage)?;
        let rows = st
            .query_map([], |r| {
                Ok((
                    r.get::<_, String>(0)?,
                    r.get::<_, String>(1)?,
                    r.get::<_, String>(2)?,
                    r.get::<_, String>(3)?,
                    r.get::<_, u64>(4)?,
                    r.get::<_, bool>(5)?,
                    r.get::<_, String>(6)?,
                ))
            })
            .map_err(storage)?;
        let mut out = Vec::new();
        for r in rows {
            let (h, t, k, p, b, a, e) = r.map_err(storage)?;
            out.push(ArtifactRecord {
                content_hash: h,
                task_id: t,
                kind: k,
                path: p.into(),
                bytes: b,
                accepted: a,
                evidence: serde_json::from_str(&e).map_err(storage)?,
            });
        }
        Ok(out)
    }
    /// Supersede accepted descendants after an owning ancestor/interface revision.
    pub fn invalidate_nodes(&self, node_ids: &[String], reason: &str) -> Result<()> {
        let tx = self.conn.unchecked_transaction().map_err(storage)?;
        for id in node_ids {
            tx.execute(
                "UPDATE node_revisions SET accepted=0 WHERE node_id=?1",
                params![id],
            )
            .map_err(storage)?;
            tx.execute("UPDATE scene_tasks SET state='superseded',last_error=?2 WHERE node_id=?1 AND state='accepted'",params![id,reason]).map_err(storage)?;
        }
        tx.commit().map_err(storage)?;
        Ok(())
    }
    /// Reconcile stale claims after restart.
    pub fn recover_orphans(&self, lease_seconds: u64) -> Result<usize> {
        let cutoff = loop_ai::now_ms() - lease_seconds as i64 * 1000;
        let n=self.conn.execute("UPDATE scene_tasks SET state='retry_wait',next_retry_at=?1,last_error='orphaned lease recovered',lease_owner=NULL WHERE state IN ('claimed','running','validating') AND COALESCE(heartbeat_at,0)<?1",params![cutoff]).map_err(storage)?;
        Ok(n)
    }
}

/// Exclusive run ownership lock. The file is removed on clean drop; stale PID
/// locks are recoverable if the recorded local process no longer exists.
pub struct RunLock {
    path: PathBuf,
}
impl RunLock {
    /// Acquire exclusive ownership.
    pub fn acquire(run_root: &Path) -> Result<Self> {
        let path = run_root.join(".run.lock");
        let create = || OpenOptions::new().write(true).create_new(true).open(&path);
        let mut f = match create() {
            Ok(f) => f,
            Err(e) if e.kind() == std::io::ErrorKind::AlreadyExists => {
                let pid = fs::read_to_string(&path)
                    .ok()
                    .and_then(|s| s.trim().parse::<u32>().ok());
                if pid.is_some_and(|p| Path::new(&format!("/proc/{p}")).exists()) {
                    return Err(SceneError::Storage(format!(
                        "run is owned by process {}",
                        pid.unwrap()
                    )));
                }
                fs::remove_file(&path).map_err(storage)?;
                create().map_err(storage)?
            }
            Err(e) => return Err(storage(e)),
        };
        writeln!(f, "{}", std::process::id()).map_err(storage)?;
        f.sync_all().map_err(storage)?;
        Ok(Self { path })
    }
}
impl Drop for RunLock {
    fn drop(&mut self) {
        let _ = fs::remove_file(&self.path);
    }
}

/// Write a file through a sibling temporary path and atomic rename.
pub fn atomic_write(path: &Path, data: &[u8]) -> Result<()> {
    let parent = path
        .parent()
        .ok_or_else(|| SceneError::Storage("artifact has no parent".into()))?;
    fs::create_dir_all(parent).map_err(storage)?;
    let temp = parent.join(format!(
        ".{}.{}.tmp",
        path.file_name().unwrap_or_default().to_string_lossy(),
        uuid::Uuid::now_v7()
    ));
    let mut f = OpenOptions::new()
        .create_new(true)
        .write(true)
        .open(&temp)
        .map_err(storage)?;
    f.write_all(data).map_err(storage)?;
    f.sync_all().map_err(storage)?;
    fs::rename(&temp, path).map_err(storage)?;
    Ok(())
}
/// SHA-256 file content identity.
pub fn hash_file(path: &Path) -> Result<String> {
    let bytes = fs::read(path).map_err(storage)?;
    Ok(hash_bytes(&bytes))
}
/// SHA-256 byte content identity.
pub fn hash_bytes(bytes: &[u8]) -> String {
    hex::encode(Sha256::digest(bytes))
}
/// Filesystem-safe reversible-enough encoding for stable IDs.
pub fn safe_id(id: &str) -> String {
    id.bytes()
        .map(|b| {
            if b.is_ascii_alphanumeric() || matches!(b, b'-' | b'_') {
                (b as char).to_string()
            } else {
                format!("%{b:02X}")
            }
        })
        .collect()
}
fn storage(e: impl std::fmt::Display) -> SceneError {
    SceneError::Storage(e.to_string())
}
fn state_name(s: RunState) -> &'static str {
    match s {
        RunState::Running => "running",
        RunState::WaitingExternal => "waiting_external",
        RunState::WaitingResource => "waiting_resource",
        RunState::Cancelled => "cancelled",
        RunState::Failed => "failed",
        RunState::Accepted => "accepted",
    }
}
fn parse_run_state(s: &str) -> RunState {
    match s {
        "running" => RunState::Running,
        "waiting_external" => RunState::WaitingExternal,
        "waiting_resource" => RunState::WaitingResource,
        "cancelled" => RunState::Cancelled,
        "accepted" => RunState::Accepted,
        _ => RunState::Failed,
    }
}
fn task_state_name(s: SceneTaskState) -> &'static str {
    match s {
        SceneTaskState::Pending => "pending",
        SceneTaskState::Claimed => "claimed",
        SceneTaskState::Running => "running",
        SceneTaskState::Validating => "validating",
        SceneTaskState::Accepted => "accepted",
        SceneTaskState::RetryWait => "retry_wait",
        SceneTaskState::RepairPlanning => "repair_planning",
        SceneTaskState::WaitingExternal => "waiting_external",
        SceneTaskState::Superseded => "superseded",
        SceneTaskState::Cancelled => "cancelled",
        SceneTaskState::Failed => "failed",
    }
}
fn parse_task_state(s: &str) -> SceneTaskState {
    match s {
        "pending" => SceneTaskState::Pending,
        "claimed" => SceneTaskState::Claimed,
        "running" => SceneTaskState::Running,
        "validating" => SceneTaskState::Validating,
        "accepted" => SceneTaskState::Accepted,
        "retry_wait" => SceneTaskState::RetryWait,
        "repair_planning" => SceneTaskState::RepairPlanning,
        "waiting_external" => SceneTaskState::WaitingExternal,
        "superseded" => SceneTaskState::Superseded,
        "cancelled" => SceneTaskState::Cancelled,
        _ => SceneTaskState::Failed,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn persistence_and_claim_are_idempotent() {
        let d = tempfile::tempdir().unwrap();
        let s = RunStore::open(d.path()).unwrap();
        s.initialize(
            "r",
            "t",
            "p",
            &serde_json::json!({}),
            &serde_json::json!({}),
        )
        .unwrap();
        s.upsert_task("a", None, "x", "h", 0).unwrap();
        assert_eq!(s.claim_task("a", "w").unwrap(), Some(1));
        assert_eq!(s.claim_task("a", "w2").unwrap(), None);
        drop(s);
        assert_eq!(RunStore::open(d.path()).unwrap().status().unwrap().0, "r");
    }

    #[test]
    fn conflicting_node_revision_is_rejected() {
        let d = tempfile::tempdir().unwrap();
        let s = RunStore::open(d.path()).unwrap();
        let mut node = serde_json::from_str::<crate::spec::SceneSpec>(include_str!(
            "../examples/complete-scene.json"
        ))
        .unwrap()
        .nodes
        .remove(0);
        s.accept_node(&node, "first").unwrap();
        node.design = "conflicting rewrite".into();
        assert!(s.accept_node(&node, "stale").is_err());
    }
}
