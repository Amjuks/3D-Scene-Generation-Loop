//! Durable, non-secret timing and model-attempt evidence.
use crate::{Result, SceneError};
use crate::storage::atomic_write;
use serde_json::{json, Value};
use std::path::{Path, PathBuf};

#[derive(Clone)]
pub(crate) struct Reporter { root: PathBuf }
pub(crate) struct Span { reporter: Reporter, path: PathBuf, event: Value, finished: bool }
fn err(e: impl std::fmt::Display) -> SceneError { SceneError::Storage(e.to_string()) }
impl Reporter {
    pub(crate) fn new(root: &Path) -> Self { Self { root: root.into() } }
    pub(crate) fn save_response(&self,role:&str,text:&str)->Result<()> {
        atomic_write(&self.root.join("model-responses").join(format!("{}-{}.txt",uuid::Uuid::now_v7(),role)),text.as_bytes())
    }
    pub(crate) fn begin(&self, kind: &str, name: &str, details: Value) -> Result<Span> {
        let folder=self.root.join("telemetry");
        std::fs::create_dir_all(&folder).map_err(err)?;
        if kind=="run" {
            // Exclusive run ownership makes previously running spans orphaned.
            // Do not count time spent offline as active generation time.
            let mut old=Vec::new();
            for file in std::fs::read_dir(&folder).map_err(err)? {
                let path=file.map_err(err)?.path();
                if path.extension().is_some_and(|e|e=="json") {
                    let event:Value=serde_json::from_slice(&std::fs::read(&path).map_err(err)?).map_err(err)?;
                    old.push((path,event));
                }
            }
            let last=old.iter().flat_map(|(_,e)|[e["started_at_ms"].as_i64(),e["finished_at_ms"].as_i64()]).flatten().max().unwrap_or(loop_ai::now_ms());
            for (path,mut event) in old {
                if event["status"]=="running" {
                    event["status"]=json!("interrupted");event["finished_at_ms"]=json!(last);
                    event["duration_seconds"]=json!((last-event["started_at_ms"].as_i64().unwrap_or(last)).max(0) as f64/1000.0);
                    event["details"]["duration_is_lower_bound"]=json!(true);
                    atomic_write(&path,&serde_json::to_vec_pretty(&event).map_err(err)?)?;
                }
            }
        }
        let path=folder.join(format!("{}.json",uuid::Uuid::now_v7()));
        let event=json!({"kind":kind,"name":name,"status":"running","started_at_ms":loop_ai::now_ms(),"finished_at_ms":null,"duration_seconds":null,"details":details});
        atomic_write(&path,&serde_json::to_vec_pretty(&event).map_err(err)?)?;
        self.refresh()?;
        Ok(Span { reporter:self.clone(),path,event,finished:false })
    }
    pub(crate) fn check_model_budget(&self, budget: &crate::config::BudgetConfig) -> Result<()> {
        // The report includes failed calls and schema corrections, unlike the
        // old controller counter, which was updated only after parsed success.
        let report: Value = serde_json::from_slice(&std::fs::read(self.root.join("run-report.json")).map_err(err)?).map_err(err)?;
        let calls = report["model_attempts"].as_u64().unwrap_or(0);
        let tokens = report["reported_tokens"].as_u64().unwrap_or(0);
        if calls >= u64::from(budget.max_model_calls) || tokens >= budget.max_total_tokens {
            return Err(SceneError::WaitingExternal(format!("model budget exhausted at {calls} calls/{tokens} reported tokens; review saved budget before retrying")));
        }
        Ok(())
    }
    pub(crate) fn refresh(&self) -> Result<()> {
        let mut paths=std::fs::read_dir(self.root.join("telemetry")).map_err(err)?
            .filter_map(|p|p.ok().map(|p|p.path())).filter(|p|p.extension().is_some_and(|e|e=="json")).collect::<Vec<_>>();
        paths.sort();
        let events=paths.iter().map(|p|std::fs::read(p).map_err(err).and_then(|b|serde_json::from_slice::<Value>(&b).map_err(err))).collect::<Result<Vec<_>>>()?;
        let runs=events.iter().filter(|e|e["kind"]=="run").collect::<Vec<_>>();
        let now=loop_ai::now_ms();
        let first=runs.first().and_then(|e|e["started_at_ms"].as_i64()).unwrap_or(now);
        let last=runs.last();
        let end=last.and_then(|e|e["finished_at_ms"].as_i64()).unwrap_or(now);
        let duration=|e: &&Value| (e["finished_at_ms"].as_i64().unwrap_or(now)-e["started_at_ms"].as_i64().unwrap_or(now)).max(0) as f64/1000.0;
        let active=runs.iter().map(duration).sum::<f64>();
        let calls=events.iter().filter(|e|e["kind"]=="model").collect::<Vec<_>>();
        let models=calls.iter().filter_map(|e|e["details"]["model"].as_str()).collect::<std::collections::BTreeSet<_>>();
        let inventory=std::fs::read(self.root.join("designs/interior-inventory.json")).ok().and_then(|b|serde_json::from_slice::<Value>(&b).ok());
        let report=json!({"version":1,"run_dir":self.root,"status":last.map(|e|e["status"].clone()).unwrap_or(json!("running")),"started_at_ms":first,"finished_at_ms":last.map(|e|e["finished_at_ms"].clone()),"wall_minutes":(end-first).max(0) as f64/60000.0,"active_minutes":active/60.0,"models_used":models,"model_attempts":calls.len(),"failed_model_attempts":calls.iter().filter(|e|e["status"]=="failed").count(),"reported_tokens":calls.iter().filter_map(|e|e["details"]["tokens"].as_u64()).sum::<u64>(),"cost":null,"notes":["Wall time includes pauses between resumes; active time sums controller invocations.","Tokens are provider-reported; unavailable usage on failed calls is not estimated.","Cost is unknown; no provider pricing has been assumed."],"interiors":inventory,"events":events});
        let mut markdown=format!("# Scene execution report\n\n- Status: {}\n- Wall time: {:.2} minutes\n- Active time: {:.2} minutes\n- Model attempts: {} (failed: {})\n- Provider-reported tokens: {}\n- Models: {}\n- Cost: not available\n\n## Stages and model calls\n\n| Type | Name | Status | Minutes | Model |\n|---|---|---|---:|---|\n",report["status"].as_str().unwrap_or("unknown"),report["wall_minutes"].as_f64().unwrap_or(0.0),active/60.0,calls.len(),report["failed_model_attempts"],report["reported_tokens"],report["models_used"]);
        let mut report=report;
        report["artifacts"]=json!(["scene.blend","scene.glb","manifest.json"].iter().filter_map(|name| {
            let p=self.root.join("final").join(name);
            std::fs::metadata(&p).ok().map(|m|json!({"path":p,"bytes":m.len()}))
        }).collect::<Vec<_>>());
        report["started_at_utc"]=json!(chrono::DateTime::from_timestamp_millis(first).map(|t|t.to_rfc3339()));
        report["duration_is_lower_bound"]=json!(events.iter().any(|e|e["details"]["duration_is_lower_bound"]==true));
        for e in &events {
            let minutes=duration(&e)/60.0;
            markdown.push_str(&format!("| {} | {} | {} | {:.2} | {} |\n",e["kind"].as_str().unwrap_or(""),e["name"].as_str().unwrap_or("").replace('|',"/"),e["status"].as_str().unwrap_or(""),minutes,e["details"]["model"].as_str().unwrap_or("—")));
        }
        markdown.push_str("\nSee run-report.json for per-attempt timestamps, errors, room coverage and details. Active stages include their nested calls; do not sum all table rows. Image-model visual approval is separate from numerical/export checks.\n");
        for folder in [self.root.clone(),self.root.join("final")].into_iter().filter(|p|p.is_dir()) {
            atomic_write(&folder.join("run-report.json"),&serde_json::to_vec_pretty(&report).map_err(err)?)?;
            atomic_write(&folder.join("run-report.md"),markdown.as_bytes())?;
        }
        Ok(())
    }
}
impl Span {
    pub(crate) fn finish(mut self, status: &str, details: Value) -> Result<()> {
        self.close(status,details)?;self.finished=true;Ok(())
    }
    fn close(&mut self,status:&str,details:Value)->Result<()> {
        self.event["status"]=json!(status);
        let end=loop_ai::now_ms();
        self.event["finished_at_ms"]=json!(end);
        self.event["duration_seconds"]=json!((end-self.event["started_at_ms"].as_i64().unwrap_or(end)).max(0) as f64/1000.0);
        if let (Some(dst),Some(src))=(self.event["details"].as_object_mut(),details.as_object()) {dst.extend(src.clone());}
        atomic_write(&self.path,&serde_json::to_vec_pretty(&self.event).map_err(err)?)?;
        self.reporter.refresh()
    }
}
impl Drop for Span {
    fn drop(&mut self) { if !self.finished { let _=self.close("not_completed",json!({})); } }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn failed_calls_and_reported_usage_trip_budget_before_another_request() {
        let dir = tempfile::tempdir().unwrap();
        let r = Reporter::new(dir.path());
        let run = r.begin("run", "test", json!({})).unwrap();
        let mut budget = crate::config::BudgetConfig::default();
        budget.max_model_calls = 1;
        budget.max_total_tokens = 100;
        r.check_model_budget(&budget).unwrap();
        r.begin("model", "director", json!({"model":"qwen"})).unwrap()
            .finish("failed", json!({"tokens":8192,"error":"no JSON text"})).unwrap();
        assert!(r.check_model_budget(&budget).unwrap_err().to_string().contains("model budget exhausted"));
        budget.max_model_calls = 100;
        assert!(r.check_model_budget(&budget).is_err());
        run.finish("failed", json!({})).unwrap();
        // Resume preserves the circuit; starting a new controller is not a reset.
        let resumed = r.begin("run", "resume", json!({})).unwrap();
        assert!(r.check_model_budget(&budget).is_err());
        resumed.finish("failed", json!({})).unwrap();
    }
    #[test]
    fn reports_actual_attempts_tokens_and_final_copies() {
        let dir=tempfile::tempdir().unwrap();std::fs::create_dir(dir.path().join("final")).unwrap();
        let r=Reporter::new(dir.path());let run=r.begin("run","test",json!({})).unwrap();
        r.begin("model","layout_designer",json!({"model":"first","tokens":null})).unwrap().finish("failed",json!({"error":"transport"})).unwrap();
        r.begin("model","layout_designer",json!({"model":"fallback"})).unwrap().finish("completed",json!({"tokens":42})).unwrap();
        run.finish("accepted",json!({})).unwrap();
        let read=|p:&Path|serde_json::from_slice::<Value>(&std::fs::read(p).unwrap()).unwrap();
        let report=read(&dir.path().join("run-report.json"));
        assert_eq!(report["status"],"accepted");assert_eq!(report["reported_tokens"],42);
        assert_eq!(report["model_attempts"],2);assert_eq!(report["failed_model_attempts"],1);
        assert_eq!(report,read(&dir.path().join("final/run-report.json")));
    }
    #[test]
    fn orphaned_spans_are_not_left_running_on_resume() {
        let dir=tempfile::tempdir().unwrap();let r=Reporter::new(dir.path());
        let mut lost=r.begin("run","old",json!({})).unwrap();lost.finished=true;drop(lost);
        r.begin("run","resume",json!({})).unwrap().finish("failed",json!({})).unwrap();
        let report:Value=serde_json::from_slice(&std::fs::read(dir.path().join("run-report.json")).unwrap()).unwrap();
        assert_eq!(report["events"][0]["status"],"interrupted");
        assert_eq!(report["status"],"failed");assert_eq!(report["duration_is_lower_bound"],true);
    }
}
