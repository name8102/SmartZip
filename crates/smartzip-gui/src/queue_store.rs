//! Desktop drafts share the tasks table. Engine submission atomically adopts their stable IDs.
//! Credentials never enter the serialized request. A queued task starts only after its latest
//! snapshot is acknowledged; a stale save cannot overwrite an executing task.
use crate::{
    model::{Phase, Queue},
    runtime::{JobRequest, TaskOperation},
};
use smartzip_config::{ResolvedConfig, StateMode};
use std::{collections::HashMap, path::PathBuf, sync::mpsc};

#[derive(Clone, serde::Serialize, serde::Deserialize)]
struct Draft {
    version: u32,
    request: JobRequest,
}
#[derive(Clone)]
struct Write {
    id: String,
    path: PathBuf,
    snapshot: String,
    position: i64,
    status: String,
}
impl Write {
    fn signature(&self) -> String {
        format!("{}:{}:{}", self.status, self.position, self.snapshot)
    }
}
struct Tracked {
    write: Write,
    sent: String,
    saved: Option<String>,
}
pub struct QueueStore {
    tx: mpsc::Sender<Write>,
    rx: mpsc::Receiver<(String, String, Result<(), String>)>,
    tracked: HashMap<String, Tracked>,
    pending: usize,
    pub error: Option<String>,
}
impl Default for QueueStore {
    fn default() -> Self {
        let (tx, receiver) = mpsc::channel::<Write>();
        let (sender, rx) = mpsc::channel();
        std::thread::spawn(move || {
            while let Ok(write) = receiver.recv() {
                let result = save(&write);
                let _ = sender.send((write.id.clone(), write.signature(), result));
            }
        });
        Self {
            tx,
            rx,
            tracked: HashMap::new(),
            pending: 0,
            error: None,
        }
    }
}
impl QueueStore {
    pub fn idle(&self) -> bool {
        self.pending == 0
    }
    pub fn settled(&self) -> bool {
        self.pending == 0 && self.error.is_none()
    }
    pub fn retry(&mut self) {
        self.error = None;
        for t in self.tracked.values_mut() {
            if t.saved.as_ref() != Some(&t.sent) {
                t.sent.clear();
            }
        }
    }
    pub fn sync(&mut self, queue: &mut Queue) {
        while let Ok((id, signature, result)) = self.rx.try_recv() {
            self.pending = self.pending.saturating_sub(1);
            match result {
                Ok(()) => {
                    if let Some(t) = self.tracked.get_mut(&id) {
                        t.saved = Some(signature);
                    }
                }
                Err(e) => self.error = Some(format!("等待队列未能保存，相关任务尚未启动：{e}")),
            }
        }
        let mut present = std::collections::HashSet::new();
        for (position, job) in queue.jobs.iter_mut().enumerate() {
            if job.request.operation != TaskOperation::Extract {
                continue;
            }
            let Some(id) = job.request.settings.queued_task_id.clone() else {
                continue;
            };
            present.insert(id.clone());
            if job.phase.active() {
                continue;
            }
            let Some(config) = job.request.resolved.as_ref() else {
                continue;
            };
            let path = match database_path(config) {
                Ok(Some(p)) => p,
                Ok(None) => continue,
                Err(e) => {
                    self.error = Some(e);
                    job.persistence_pending = true;
                    continue;
                }
            };
            let status = match job.phase {
                Phase::Queued => "queued_gui",
                Phase::Completed => "completed",
                Phase::Partial => "partial",
                Phase::Failed => "failed",
                _ => "cancelled",
            };
            let snapshot = match serde_json::to_string(&Draft {
                version: 1,
                request: job.request.clone(),
            }) {
                Ok(s) => s,
                Err(e) => {
                    self.error = Some(e.to_string());
                    job.persistence_pending = true;
                    continue;
                }
            };
            let write = Write {
                id: id.clone(),
                path,
                snapshot,
                position: position as i64,
                status: status.into(),
            };
            let signature = write.signature();
            let t = self.tracked.entry(id).or_insert_with(|| Tracked {
                write: write.clone(),
                sent: String::new(),
                saved: None,
            });
            t.write = write;
            job.persistence_pending =
                job.phase == Phase::Queued && t.saved.as_ref() != Some(&signature);
        }
        for (id, t) in &mut self.tracked {
            if !present.contains(id) && t.write.status == "queued_gui" {
                t.write.status = "cancelled".into();
            }
            let signature = t.write.signature();
            if t.sent != signature {
                if self.tx.send(t.write.clone()).is_ok() {
                    self.pending += 1;
                    t.sent = signature;
                } else {
                    self.error = Some("等待队列保存线程已退出".into());
                }
            }
        }
    }
}
pub fn database_path(config: &ResolvedConfig) -> Result<Option<PathBuf>, String> {
    let s = &config.values.state;
    if s.mode != StateMode::ReadWrite || !s.history {
        return Ok(None);
    }
    if let Some(path) = &s.database {
        return Ok(Some(path.clone()));
    }
    let paths = smartzip_platform::PlatformPaths::try_new().map_err(|e| e.to_string())?;
    let legacy = smartzip_platform::PlatformPaths::legacy().map_err(|e| e.to_string())?;
    paths
        .select_database(&legacy)
        .map(|p| Some(p.0))
        .map_err(|e| e.to_string())
}
fn save(w: &Write) -> Result<(), String> {
    let result = (|| -> Result<(), Box<dyn std::error::Error>> {
        if let Some(parent) = w.path.parent() {
            std::fs::create_dir_all(parent)?;
        }
        let db = smartzip_db::SmartZipDb::open(&w.path)?;
        if w.status == "queued_gui" {
            let draft: Draft = serde_json::from_str(&w.snapshot)?;
            let inputs = serde_json::to_string(&draft.request.paths)?;
            db.connection().execute("INSERT INTO tasks(id,kind,status,started_at,inputs_json,config_snapshot_json,queue_position,recoverable,paused) VALUES (?1,'extract','queued_gui',?2,?3,?4,?5,0,1)
                ON CONFLICT(id) DO UPDATE SET inputs_json=excluded.inputs_json,config_snapshot_json=excluded.config_snapshot_json,queue_position=excluded.queue_position WHERE tasks.status='queued_gui' AND tasks.recoverable=0 AND tasks.finished_at IS NULL",
                (&w.id,smartzip_db::timestamp::now_utc_iso8601(),inputs,&w.snapshot,w.position))?;
        } else {
            db.connection().execute("UPDATE tasks SET status=?1,finished_at=?2 WHERE id=?3 AND status='queued_gui' AND recoverable=0",(&w.status,smartzip_db::timestamp::now_utc_iso8601(),&w.id))?;
        }
        Ok(())
    })();
    result.map_err(|e| e.to_string())
}
pub fn load(config: &ResolvedConfig) -> Result<Vec<JobRequest>, String> {
    let Some(path) = database_path(config)? else {
        return Ok(vec![]);
    };
    if !path.try_exists().map_err(|e| e.to_string())? {
        return Ok(vec![]);
    }
    let db = smartzip_db::SmartZipDb::open_read_only(path).map_err(|e| e.to_string())?;
    let mut stmt=db.connection().prepare("SELECT id,config_snapshot_json FROM tasks WHERE status='queued_gui' AND recoverable=0 AND finished_at IS NULL ORDER BY queue_position,started_at").map_err(|e|e.to_string())?;
    let rows = stmt
        .query_map([], |r| Ok((r.get::<_, String>(0)?, r.get::<_, String>(1)?)))
        .map_err(|e| e.to_string())?;
    let mut requests = Vec::new();
    for row in rows {
        let (id, snapshot) = row.map_err(|e| e.to_string())?;
        let draft: Draft = serde_json::from_str(&snapshot)
            .map_err(|_| format!("保存的队列 {id} 无法读取，原记录未改动"))?;
        if draft.version != 1
            || draft.request.operation != TaskOperation::Extract
            || draft.request.settings.queued_task_id.as_ref() != Some(&id)
        {
            return Err(format!("保存的队列 {id} 版本或身份无效"));
        }
        requests.push(draft.request);
    }
    Ok(requests)
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn draft_roundtrip_omits_passwords_and_late_save_preserves_engine_state() {
        let dir = tempfile::tempdir().unwrap();
        let mut resolved = ResolvedConfig::load(None).unwrap();
        resolved.values.state.database = Some(dir.path().join("state.db"));
        let request = JobRequest {
            operation: TaskOperation::Extract,
            paths: vec!["归档 space.zip".into()],
            settings: crate::runtime::TaskSettings {
                queued_task_id: Some("stable-task".into()),
                passwords: vec!["never-persist-this".into()],
                temporary_passwords: true,
                ..Default::default()
            },
            resolved: Some(resolved.clone()),
        };
        let w = Write {
            id: "stable-task".into(),
            path: database_path(&resolved).unwrap().unwrap(),
            snapshot: serde_json::to_string(&Draft {
                version: 1,
                request,
            })
            .unwrap(),
            position: 7,
            status: "queued_gui".into(),
        };
        assert!(!w.snapshot.contains("never-persist-this"));
        save(&w).unwrap();
        let restored = load(&resolved).unwrap();
        assert_eq!(restored.len(), 1);
        assert!(restored[0].settings.passwords.is_empty());
        assert!(restored[0].settings.temporary_passwords);
        let db = smartzip_db::SmartZipDb::open(&w.path).unwrap();
        db.connection().execute("UPDATE tasks SET status='running',recoverable=1,config_snapshot_json='engine' WHERE id='stable-task'",[]).unwrap();
        save(&w).unwrap();
        let mut cancelled = w.clone();
        cancelled.status = "cancelled".into();
        save(&cancelled).unwrap();
        assert_eq!(
            db.connection()
                .query_row(
                    "SELECT config_snapshot_json FROM tasks WHERE id='stable-task'",
                    [],
                    |r| r.get::<_, String>(0)
                )
                .unwrap(),
            "engine"
        );
        assert!(load(&resolved).unwrap().is_empty());
    }
}
