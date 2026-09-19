//! Durable execution fields shared with extraction history.

use crate::Result;
use rusqlite::{params, Connection, OptionalExtension};
use serde::{Deserialize, Serialize};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Priority {
    Background = 0,
    Normal = 1,
    High = 2,
}

#[derive(Debug, Clone)]
pub struct NewTaskExecution<'a> {
    pub id: &'a str,
    pub kind: &'a str,
    pub output_path: Option<&'a str>,
    pub started_at: &'a str,
    pub inputs_json: &'a str,
    pub config_snapshot_json: &'a str,
    pub priority: Priority,
    pub queue_position: i64,
    pub recoverable: bool,
    pub owner_epoch: i64,
}

#[derive(Debug, Clone)]
pub struct NewNode<'a> {
    pub id: &'a str,
    pub parent_id: Option<&'a str>,
    pub root_id: &'a str,
    pub input_path: &'a str,
    pub input_ref_json: &'a str,
    pub config_revision: i64,
    pub generation: i64,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PendingDecision {
    pub id: String,
    pub generation: i64,
    pub kind: String,
    pub evidence: String,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct NodeRecord {
    pub task_id: String,
    pub node_id: String,
    pub parent_node_id: Option<String>,
    pub root_node_id: String,
    pub generation: i64,
    pub input_path: String,
    pub input_ref_json: String,
    pub stage: String,
    pub execution_state: String,
    pub decision: Option<PendingDecision>,
    pub artifact_refs_json: Option<String>,
    pub commit_json: Option<String>,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RecoveredTaskRecord {
    pub task_id: String,
    pub output_path: Option<String>,
    pub config_snapshot_json: Option<String>,
    pub priority: Priority,
    pub queue_position: i64,
    pub paused: bool,
    pub committed_output_files: u64,
    pub committed_output_bytes: u64,
    pub nested_candidate_count: usize,
    pub nodes: Vec<NodeRecord>,
}

pub struct TaskExecutionRepository<'a> {
    conn: &'a mut Connection,
}

impl<'a> TaskExecutionRepository<'a> {
    pub fn new(conn: &'a mut Connection) -> Self {
        Self { conn }
    }

    /// Create the task and all initially known roots atomically.
    pub fn submit(&mut self, task: NewTaskExecution<'_>, roots: &[NewNode<'_>]) -> Result<()> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "INSERT INTO tasks(id, kind, status, output_path, started_at, inputs_json, \
             priority, queue_position, config_snapshot_json, recoverable, owner_epoch) \
             VALUES (?1, ?2, 'queued', ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10)",
            params![
                task.id,
                task.kind,
                task.output_path,
                task.started_at,
                task.inputs_json,
                task.priority as i64,
                task.queue_position,
                task.config_snapshot_json,
                task.recoverable as i64,
                task.owner_epoch,
            ],
        )?;
        for root in roots {
            insert_node(&tx, task.id, root)?;
        }
        tx.commit()?;
        Ok(())
    }

    /// Add a child only after its parent output has been committed.
    pub fn enqueue_child(&mut self, task_id: &str, child: NewNode<'_>) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let exists = tx.query_row(
            "SELECT EXISTS(SELECT 1 FROM file_extractions WHERE task_id=?1 \
             AND parent_node_id IS ?2 AND input_ref_json=?3)",
            params![task_id, child.parent_id, child.input_ref_json],
            |row| row.get::<_, bool>(0),
        )?;
        if exists {
            tx.commit()?;
            return Ok(false);
        }
        insert_node(&tx, task_id, &child)?;
        tx.execute(
            "UPDATE tasks SET nested_candidate_count=nested_candidate_count + 1 WHERE id=?1",
            params![task_id],
        )?;
        tx.commit()?;
        Ok(true)
    }

    pub fn child_exists(
        &self,
        task_id: &str,
        parent_id: &str,
        input_ref_json: &str,
    ) -> Result<bool> {
        Ok(self.conn.query_row(
            "SELECT EXISTS(SELECT 1 FROM file_extractions WHERE task_id=?1 \
             AND parent_node_id=?2 AND input_ref_json=?3)",
            params![task_id, parent_id, input_ref_json],
            |row| row.get(0),
        )?)
    }

    pub fn record_artifacts(
        &mut self,
        task_id: &str,
        node_id: &str,
        generation: i64,
        artifact_refs_json: &str,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE file_extractions SET artifact_refs_json=?1 WHERE task_id=?2 \
             AND node_id=?3 AND generation=?4 AND status='pending' \
             AND execution_state='running'",
            params![artifact_refs_json, task_id, node_id, generation],
        )? == 1)
    }

    pub fn clear_artifacts(
        &mut self,
        task_id: &str,
        node_id: &str,
        generation: i64,
    ) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE file_extractions SET artifact_refs_json=NULL WHERE task_id=?1 \
             AND node_id=?2 AND generation=?3 AND status='pending'",
            params![task_id, node_id, generation],
        )? == 1)
    }

    pub fn set_priority(&mut self, task_id: &str, priority: Priority) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE tasks SET priority=?1 WHERE id=?2 AND finished_at IS NULL",
            params![priority as i64, task_id],
        )? == 1)
    }

    pub fn set_paused(&mut self, task_id: &str, paused: bool) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE tasks SET paused=?1 WHERE id=?2 AND finished_at IS NULL",
            params![paused as i64, task_id],
        )? == 1)
    }

    pub fn reorder(&mut self, task_id: &str, queue_position: i64) -> Result<bool> {
        Ok(self.conn.execute(
            "UPDATE tasks SET queue_position=?1 WHERE id=?2 AND finished_at IS NULL",
            params![queue_position, task_id],
        )? == 1)
    }

    /// A transition and its event are one durable fact. Stale workers cannot
    /// change another generation or a node already marked successful.
    pub fn transition(
        &mut self,
        task_id: &str,
        node_id: &str,
        generation: i64,
        from: &str,
        to: &str,
        stage: &str,
        attempt_id: Option<&str>,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE file_extractions SET execution_state=?1, stage=?2, attempt_id=?3 \
             WHERE task_id=?4 AND node_id=?5 AND generation=?6 \
             AND execution_state=?7 AND status='pending'",
            params![to, stage, attempt_id, task_id, node_id, generation, from],
        )?;
        if changed == 1 {
            tx.execute(
                "UPDATE tasks SET status='running' WHERE id=?1 AND finished_at IS NULL",
                params![task_id],
            )?;
            record_stage_event(&tx, task_id, node_id, stage, attempt_id, to)?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    pub fn wait_for_decision(
        &mut self,
        task_id: &str,
        node_id: &str,
        stage: &str,
        decision: &PendingDecision,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let decision_json = serde_json::to_string(decision)?;
        let changed = tx.execute(
            "UPDATE file_extractions SET execution_state='waiting_input', decision_json=?1 \
             WHERE task_id=?2 AND node_id=?3 AND generation=?4 \
             AND stage=?5 AND execution_state='running' AND status='pending'",
            params![decision_json, task_id, node_id, decision.generation, stage],
        )?;
        if changed == 1 {
            record_stage_event(&tx, task_id, node_id, stage, None, "waiting_input")?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    pub fn begin_commit(
        &mut self,
        task_id: &str,
        node_id: &str,
        generation: i64,
        commit_json: &str,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE file_extractions SET execution_state='committing', commit_json=?1 \
             WHERE task_id=?2 AND node_id=?3 AND generation=?4 AND stage='commit' \
             AND execution_state='running' AND status='pending' AND commit_json IS NULL",
            params![commit_json, task_id, node_id, generation],
        )?;
        if changed == 1 {
            record_stage_event(&tx, task_id, node_id, "commit", None, "committing")?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    pub fn commit_published(
        &mut self,
        task_id: &str,
        node_id: &str,
        generation: i64,
        commit_json: &str,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE file_extractions SET execution_state='running', commit_json=?1 \
             WHERE task_id=?2 AND node_id=?3 AND generation=?4 AND stage='commit' \
             AND execution_state='committing' AND status='pending'",
            params![commit_json, task_id, node_id, generation],
        )?;
        if changed == 1 {
            record_stage_event(&tx, task_id, node_id, "commit", None, "published")?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    pub fn abort_commit(&mut self, task_id: &str, node_id: &str, generation: i64) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE file_extractions SET execution_state='running', commit_json=NULL \
             WHERE task_id=?1 AND node_id=?2 AND generation=?3 AND stage='commit' \
             AND execution_state='committing' AND status='pending'",
            params![task_id, node_id, generation],
        )?;
        if changed == 1 {
            record_stage_event(&tx, task_id, node_id, "commit", None, "aborted")?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    pub fn reset_commit_for_retry(
        &mut self,
        task_id: &str,
        node_id: &str,
        generation: i64,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let changed = tx.execute(
            "UPDATE file_extractions SET generation=generation + 1, \
             execution_state='ready', stage='resolve_inputs', attempt_id=NULL, \
             decision_json=NULL, commit_json=NULL WHERE task_id=?1 AND node_id=?2 \
             AND generation=?3 AND status='pending' AND commit_json IS NOT NULL",
            params![task_id, node_id, generation],
        )?;
        if changed == 1 {
            record_stage_event(&tx, task_id, node_id, "recovery", None, "ready")?;
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    pub fn finish_node(
        &mut self,
        task_id: &str,
        node_id: &str,
        generation: i64,
        status: &str,
        reason: Option<&str>,
        output_path: Option<&str>,
        committed: bool,
        sample_hash: Option<&str>,
        file_size: Option<i64>,
        embedded_offset: Option<i64>,
        has_password: bool,
        password_id: Option<i64>,
        encoding: Option<&str>,
        encoding_corrected: bool,
        output_files: u64,
        output_bytes: u64,
    ) -> Result<bool> {
        let output_files = i64::try_from(output_files)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let output_bytes = i64::try_from(output_bytes)
            .map_err(|error| rusqlite::Error::ToSqlConversionFailure(Box::new(error)))?;
        let tx = self.conn.transaction()?;
        let commit_json = committed.then_some("{\"published\":true}");
        let changed = tx.execute(
            "UPDATE file_extractions SET status=?1, reason=?2, output_path=?3, \
             execution_state='terminal', stage='cleanup', attempt_id=NULL, commit_json=?4, \
             artifact_refs_json=NULL, \
             sample_hash=?5, file_size=?6, offset=?7, has_password=?8, \
             password_id=?9, encoding=?10, encoding_corrected=?11 \
             WHERE task_id=?12 AND node_id=?13 AND generation=?14 AND status='pending'",
            params![
                status,
                reason,
                output_path,
                commit_json,
                sample_hash,
                file_size,
                embedded_offset,
                has_password as i64,
                password_id,
                encoding,
                encoding_corrected as i64,
                task_id,
                node_id,
                generation,
            ],
        )?;
        if changed == 1 {
            tx.execute(
                "UPDATE tasks SET committed_output_files=committed_output_files + ?1, \
                 committed_output_bytes=committed_output_bytes + ?2 WHERE id=?3",
                params![output_files, output_bytes, task_id],
            )?;
            record_stage_event(&tx, task_id, node_id, "cleanup", None, status)?;
            tx.execute(
                "UPDATE tasks SET status='running' WHERE id=?1 AND finished_at IS NULL",
                params![task_id],
            )?;
            let pending: i64 = tx.query_row(
                "SELECT COUNT(*) FROM file_extractions WHERE task_id=?1 AND status='pending'",
                params![task_id],
                |row| row.get(0),
            )?;
            if pending == 0 {
                let failed: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM file_extractions WHERE task_id=?1 AND status='failed'",
                    params![task_id],
                    |row| row.get(0),
                )?;
                let succeeded: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM file_extractions WHERE task_id=?1 AND status='extracted'",
                    params![task_id],
                    |row| row.get(0),
                )?;
                let cancelled: i64 = tx.query_row(
                    "SELECT COUNT(*) FROM file_extractions WHERE task_id=?1 AND status='cancelled'",
                    params![task_id],
                    |row| row.get(0),
                )?;
                let task_status = if cancelled > 0 {
                    "cancelled"
                } else if failed == 0 {
                    "completed"
                } else if succeeded == 0 {
                    "failed"
                } else {
                    "partial"
                };
                tx.execute(
                    "UPDATE tasks SET status=?1, finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') \
                     WHERE id=?2 AND finished_at IS NULL",
                    params![task_status, task_id],
                )?;
            }
        }
        tx.commit()?;
        Ok(changed == 1)
    }

    pub fn finish_pending_task(&mut self, task_id: &str, status: &str, reason: &str) -> Result<()> {
        let tx = self.conn.transaction()?;
        let nodes = {
            let mut stmt = tx.prepare(
                "SELECT node_id FROM file_extractions WHERE task_id=?1 AND status='pending' \
                 AND commit_json IS NULL",
            )?;
            let rows = stmt
                .query_map(params![task_id], |row| row.get::<_, String>(0))?
                .collect::<std::result::Result<Vec<_>, _>>()?;
            rows
        };
        tx.execute(
            "UPDATE file_extractions SET status=?1, reason=?2, execution_state='terminal', \
             stage='cleanup', attempt_id=NULL WHERE task_id=?3 AND status='pending' \
             AND commit_json IS NULL",
            params![status, reason, task_id],
        )?;
        for node_id in nodes {
            record_stage_event(&tx, task_id, &node_id, "cleanup", None, status)?;
        }
        let recovery_pending: i64 = tx.query_row(
            "SELECT COUNT(*) FROM file_extractions WHERE task_id=?1 AND status='pending' \
             AND commit_json IS NOT NULL",
            params![task_id],
            |row| row.get(0),
        )?;
        if recovery_pending == 0 {
            tx.execute(
                "UPDATE tasks SET status=?1, finished_at=strftime('%Y-%m-%dT%H:%M:%fZ','now') \
                 WHERE id=?2 AND finished_at IS NULL",
                params![status, task_id],
            )?;
        } else {
            tx.execute(
                "UPDATE tasks SET status='recovering', finished_at=NULL WHERE id=?1",
                params![task_id],
            )?;
        }
        tx.commit()?;
        Ok(())
    }

    pub fn set_terminal_task_status(&mut self, task_id: &str, status: &str) -> Result<()> {
        self.conn.execute(
            "UPDATE tasks SET status=?1, \
             finished_at=COALESCE(finished_at, strftime('%Y-%m-%dT%H:%M:%fZ','now')) \
             WHERE id=?2",
            params![status, task_id],
        )?;
        Ok(())
    }

    /// The reply value is deliberately absent: temporary passwords stay in memory.
    pub fn accept_decision_reply(
        &mut self,
        task_id: &str,
        node_id: &str,
        generation: i64,
        decision_id: &str,
        owner_epoch: i64,
    ) -> Result<bool> {
        let tx = self.conn.transaction()?;
        let stored: Option<String> = tx
            .query_row(
                "SELECT f.decision_json FROM file_extractions f JOIN tasks t ON t.id=f.task_id \
                 WHERE f.task_id=?1 AND f.node_id=?2 AND f.generation=?3 \
                 AND f.execution_state='waiting_input' AND f.status='pending' \
                 AND t.owner_epoch=?4",
                params![task_id, node_id, generation, owner_epoch],
                |row| row.get(0),
            )
            .optional()?
            .flatten();
        let Some(stored) = stored else {
            return Ok(false);
        };
        let decision: PendingDecision = serde_json::from_str(&stored)?;
        if decision.id != decision_id || decision.generation != generation {
            return Ok(false);
        }
        tx.execute(
            "UPDATE file_extractions SET execution_state='ready', decision_json=NULL \
             WHERE task_id=?1 AND node_id=?2 AND generation=?3",
            params![task_id, node_id, generation],
        )?;
        record_stage_event(&tx, task_id, node_id, "decision", None, "ready")?;
        tx.commit()?;
        Ok(true)
    }

    pub fn node(
        &self,
        task_id: &str,
        node_id: &str,
        generation: i64,
    ) -> Result<Option<NodeRecord>> {
        self.conn
            .query_row(
                "SELECT task_id, node_id, parent_node_id, root_node_id, generation, \
                 input_path, input_ref_json, stage, execution_state, decision_json, artifact_refs_json, commit_json \
                 FROM file_extractions WHERE task_id=?1 AND node_id=?2 AND generation=?3",
                params![task_id, node_id, generation],
                |row| {
                    let decision_json: Option<String> = row.get(9)?;
                    let decision = decision_json
                        .map(|json| serde_json::from_str(&json))
                        .transpose()
                        .map_err(|error| rusqlite::Error::FromSqlConversionFailure(
                            9,
                            rusqlite::types::Type::Text,
                            Box::new(error),
                        ))?;
                    Ok(NodeRecord {
                        task_id: row.get(0)?,
                        node_id: row.get(1)?,
                        parent_node_id: row.get(2)?,
                        root_node_id: row.get(3)?,
                        generation: row.get(4)?,
                        input_path: row.get(5)?,
                        input_ref_json: row.get(6)?,
                        stage: row.get(7)?,
                        execution_state: row.get(8)?,
                        decision,
                        artifact_refs_json: row.get(10)?,
                        commit_json: row.get(11)?,
                    })
                },
            )
            .optional()
            .map_err(Into::into)
    }

    pub fn claim_recoverable(&mut self, owner_epoch: i64) -> Result<Vec<RecoveredTaskRecord>> {
        let tx = self.conn.transaction()?;
        tx.execute(
            "UPDATE file_extractions SET execution_state='interrupted', attempt_id=NULL \
             WHERE status='pending' AND execution_state IN \
             ('running', 'waiting_resources', 'persisting', 'committing', 'cancelling') \
             AND task_id IN (SELECT id FROM tasks WHERE recoverable=1 AND finished_at IS NULL)",
            [],
        )?;
        tx.execute(
            "UPDATE file_extractions SET generation=COALESCE(generation, 0) + 1, \
             execution_state='ready', stage='resolve_inputs', attempt_id=NULL, decision_json=NULL \
             WHERE status='pending' AND commit_json IS NULL \
             AND task_id IN (SELECT id FROM tasks WHERE recoverable=1 AND finished_at IS NULL)",
            [],
        )?;
        tx.execute(
            "UPDATE tasks SET owner_epoch=?1, \
             status=CASE WHEN status='running' THEN 'recovering' ELSE status END \
             WHERE recoverable=1 AND finished_at IS NULL",
            params![owner_epoch],
        )?;
        let task_rows = {
            let mut stmt = tx.prepare(
                "SELECT id, output_path, config_snapshot_json, priority, \
                 COALESCE(queue_position, 0), paused, committed_output_files, \
                 committed_output_bytes, nested_candidate_count FROM tasks \
                 WHERE recoverable=1 AND finished_at IS NULL \
                 ORDER BY priority DESC, queue_position, started_at",
            )?;
            let rows = stmt.query_map([], |row| {
                let priority = match row.get::<_, i64>(3)? {
                    0 => Priority::Background,
                    1 => Priority::Normal,
                    2 => Priority::High,
                    value => {
                        return Err(rusqlite::Error::IntegralValueOutOfRange(3, value));
                    }
                };
                let output_files = row.get::<_, i64>(6)?;
                let output_bytes = row.get::<_, i64>(7)?;
                let nested_count = row.get::<_, i64>(8)?;
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    priority,
                    row.get::<_, i64>(4)?,
                    row.get::<_, i64>(5)? != 0,
                    u64::try_from(output_files)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(6, output_files))?,
                    u64::try_from(output_bytes)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(7, output_bytes))?,
                    usize::try_from(nested_count)
                        .map_err(|_| rusqlite::Error::IntegralValueOutOfRange(8, nested_count))?,
                ))
            })?;
            rows.collect::<std::result::Result<Vec<_>, _>>()?
        };
        let mut tasks = Vec::with_capacity(task_rows.len());
        for (
            task_id,
            output_path,
            config_snapshot_json,
            priority,
            queue_position,
            paused,
            committed_output_files,
            committed_output_bytes,
            nested_candidate_count,
        ) in task_rows
        {
            let nodes = {
                let mut stmt = tx.prepare(
                    "SELECT task_id, node_id, parent_node_id, root_node_id, generation, \
                     input_path, input_ref_json, stage, execution_state, decision_json, \
                     artifact_refs_json, commit_json FROM file_extractions \
                     WHERE task_id=?1 AND status='pending' ORDER BY id",
                )?;
                let rows = stmt.query_map(params![task_id], map_node)?;
                rows.collect::<std::result::Result<Vec<_>, _>>()?
            };
            tasks.push(RecoveredTaskRecord {
                task_id,
                output_path,
                config_snapshot_json,
                priority,
                queue_position,
                paused,
                committed_output_files,
                committed_output_bytes,
                nested_candidate_count,
                nodes,
            });
        }
        tx.commit()?;
        Ok(tasks)
    }
}

fn map_node(row: &rusqlite::Row<'_>) -> rusqlite::Result<NodeRecord> {
    let decision_json: Option<String> = row.get(9)?;
    let decision = decision_json
        .map(|json| serde_json::from_str(&json))
        .transpose()
        .map_err(|error| {
            rusqlite::Error::FromSqlConversionFailure(
                9,
                rusqlite::types::Type::Text,
                Box::new(error),
            )
        })?;
    Ok(NodeRecord {
        task_id: row.get(0)?,
        node_id: row.get(1)?,
        parent_node_id: row.get(2)?,
        root_node_id: row.get(3)?,
        generation: row.get(4)?,
        input_path: row.get(5)?,
        input_ref_json: row.get(6)?,
        stage: row.get(7)?,
        execution_state: row.get(8)?,
        decision,
        artifact_refs_json: row.get(10)?,
        commit_json: row.get(11)?,
    })
}

fn insert_node(tx: &rusqlite::Transaction<'_>, task_id: &str, node: &NewNode<'_>) -> Result<()> {
    tx.execute(
        "INSERT INTO file_extractions(task_id, input_path, status, node_id, \
         parent_node_id, root_node_id, generation, stage, execution_state, \
         input_ref_json, config_revision) \
         VALUES (?1, ?2, 'pending', ?3, ?4, ?5, ?6, 'resolve_inputs', 'ready', ?7, ?8)",
        params![
            task_id,
            node.input_path,
            node.id,
            node.parent_id,
            node.root_id,
            node.generation,
            node.input_ref_json,
            node.config_revision,
        ],
    )?;
    Ok(())
}

fn record_stage_event(
    tx: &rusqlite::Transaction<'_>,
    task_id: &str,
    node_id: &str,
    stage: &str,
    attempt_id: Option<&str>,
    state: &str,
) -> Result<()> {
    tx.execute(
        "INSERT INTO task_events(task_id, level, event_type, message, node_id, stage, \
         attempt_id, sequence) VALUES (?1, 'info', 'stage', ?2, ?3, ?4, ?5, \
         (SELECT COALESCE(MAX(sequence), 0) + 1 FROM task_events WHERE task_id=?1))",
        params![task_id, state, node_id, stage, attempt_id],
    )?;
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::SmartZipDb;

    fn submit(repo: &mut TaskExecutionRepository<'_>) {
        repo.submit(
            NewTaskExecution {
                id: "task",
                kind: "extract",
                output_path: Some("/out"),
                started_at: "2026-09-19T00:00:00Z",
                inputs_json: "[\"/in.zip\"]",
                config_snapshot_json: "{}",
                priority: Priority::Normal,
                queue_position: 0,
                recoverable: true,
                owner_epoch: 0,
            },
            &[NewNode {
                id: "root",
                parent_id: None,
                root_id: "root",
                input_path: "/in.zip",
                input_ref_json: "{\"path\":\"/in.zip\"}",
                config_revision: 0,
                generation: 0,
            }],
        )
        .unwrap();
    }

    #[test]
    fn transition_and_reply_require_current_generation_decision_and_owner() {
        let mut db = SmartZipDb::in_memory().unwrap();
        let mut repo = TaskExecutionRepository::new(db.connection_mut());
        submit(&mut repo);
        assert!(repo
            .transition(
                "task",
                "root",
                0,
                "ready",
                "running",
                "extract_attempt",
                Some("a1")
            )
            .unwrap());
        assert!(!repo
            .transition(
                "task",
                "root",
                0,
                "ready",
                "running",
                "extract_attempt",
                Some("a2")
            )
            .unwrap());
        let decision = PendingDecision {
            id: "d1".into(),
            generation: 0,
            kind: "password".into(),
            evidence: "password required".into(),
        };
        assert!(repo
            .wait_for_decision("task", "root", "extract_attempt", &decision)
            .unwrap());
        assert!(!repo
            .accept_decision_reply("task", "root", 0, "other", 0)
            .unwrap());
        assert!(!repo
            .accept_decision_reply("task", "root", 1, "d1", 0)
            .unwrap());
        assert!(!repo
            .accept_decision_reply("task", "root", 0, "d1", 1)
            .unwrap());
        assert!(repo
            .accept_decision_reply("task", "root", 0, "d1", 0)
            .unwrap());
        assert!(!repo
            .accept_decision_reply("task", "root", 0, "d1", 0)
            .unwrap());
        let node = repo.node("task", "root", 0).unwrap().unwrap();
        assert_eq!(node.execution_state, "ready");
        assert!(node.decision.is_none());
    }

    #[test]
    fn task_and_roots_roll_back_together() {
        let mut db = SmartZipDb::in_memory().unwrap();
        let mut repo = TaskExecutionRepository::new(db.connection_mut());
        let duplicate_roots = [
            NewNode {
                id: "same",
                parent_id: None,
                root_id: "same",
                input_path: "/one.zip",
                input_ref_json: "{}",
                config_revision: 0,
                generation: 0,
            },
            NewNode {
                id: "same",
                parent_id: None,
                root_id: "same",
                input_path: "/two.zip",
                input_ref_json: "{}",
                config_revision: 0,
                generation: 0,
            },
        ];
        assert!(repo
            .submit(
                NewTaskExecution {
                    id: "bad",
                    kind: "extract",
                    output_path: None,
                    started_at: "2026-09-19T00:00:00Z",
                    inputs_json: "[]",
                    config_snapshot_json: "{}",
                    priority: Priority::Normal,
                    queue_position: 0,
                    recoverable: true,
                    owner_epoch: 0,
                },
                &duplicate_roots,
            )
            .is_err());
        assert!(repo.node("bad", "same", 0).unwrap().is_none());
        drop(repo);
        let task_count: i64 = db
            .connection()
            .query_row("SELECT COUNT(*) FROM tasks WHERE id='bad'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(task_count, 0);
    }

    #[test]
    fn terminal_node_is_the_single_file_history_and_finishes_task() {
        let mut db = SmartZipDb::in_memory().unwrap();
        let mut repo = TaskExecutionRepository::new(db.connection_mut());
        submit(&mut repo);
        assert!(repo
            .finish_node(
                "task",
                "root",
                0,
                "extracted",
                None,
                Some("/out/in"),
                true,
                Some("hash"),
                Some(42),
                None,
                true,
                None,
                Some("utf-8"),
                true,
                2,
                42,
            )
            .unwrap());
        drop(repo);
        let row: (i64, String, String, String, i64, i64) = db
            .connection()
            .query_row(
                "SELECT COUNT(*), f.status, t.status, f.commit_json, \
                 t.committed_output_files, t.committed_output_bytes \
                 FROM file_extractions f JOIN tasks t ON t.id=f.task_id WHERE t.id='task'",
                [],
                |row| {
                    Ok((
                        row.get(0)?,
                        row.get(1)?,
                        row.get(2)?,
                        row.get(3)?,
                        row.get(4)?,
                        row.get(5)?,
                    ))
                },
            )
            .unwrap();
        assert_eq!(row.0, 1);
        assert_eq!(row.1, "extracted");
        assert_eq!(row.2, "completed");
        assert_eq!(row.3, "{\"published\":true}");
        assert_eq!((row.4, row.5), (2, 42));
    }

    #[test]
    fn cancelled_node_finishes_task_as_cancelled() {
        let mut db = SmartZipDb::in_memory().unwrap();
        let mut repo = TaskExecutionRepository::new(db.connection_mut());
        submit(&mut repo);
        assert!(repo
            .finish_node(
                "task",
                "root",
                0,
                "cancelled",
                Some("cancelled"),
                None,
                false,
                None,
                None,
                None,
                false,
                None,
                None,
                false,
                0,
                0,
            )
            .unwrap());
        drop(repo);
        let status: String = db
            .connection()
            .query_row("SELECT status FROM tasks WHERE id='task'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "cancelled");
    }

    #[test]
    fn completed_node_followed_by_cancelled_node_finishes_task_as_cancelled() {
        let mut db = SmartZipDb::in_memory().unwrap();
        let mut repo = TaskExecutionRepository::new(db.connection_mut());
        submit(&mut repo);
        assert!(repo
            .enqueue_child(
                "task",
                NewNode {
                    id: "second",
                    parent_id: Some("root"),
                    root_id: "second",
                    input_path: "/second.zip",
                    input_ref_json: "{\"path\":\"/second.zip\"}",
                    config_revision: 0,
                    generation: 0,
                },
            )
            .unwrap());
        assert!(repo
            .finish_node(
                "task",
                "root",
                0,
                "extracted",
                None,
                Some("/out/in"),
                true,
                None,
                None,
                None,
                false,
                None,
                None,
                false,
                0,
                0,
            )
            .unwrap());
        assert!(repo
            .finish_node(
                "task",
                "second",
                0,
                "cancelled",
                Some("cancelled"),
                None,
                false,
                None,
                None,
                None,
                false,
                None,
                None,
                false,
                0,
                0,
            )
            .unwrap());
        drop(repo);
        let status: String = db
            .connection()
            .query_row("SELECT status FROM tasks WHERE id='task'", [], |row| {
                row.get(0)
            })
            .unwrap();
        assert_eq!(status, "cancelled");
    }

    #[test]
    fn recovery_snapshot_preserves_paused_state() {
        let mut db = SmartZipDb::in_memory().unwrap();
        let mut repo = TaskExecutionRepository::new(db.connection_mut());
        submit(&mut repo);
        assert!(repo.set_paused("task", true).unwrap());
        let recovered = repo.claim_recoverable(1).unwrap();
        assert_eq!(recovered.len(), 1);
        assert!(recovered[0].paused);
    }

    #[test]
    fn stopping_task_preserves_node_with_commit_intent_for_recovery() {
        let mut db = SmartZipDb::in_memory().unwrap();
        let mut repo = TaskExecutionRepository::new(db.connection_mut());
        submit(&mut repo);
        assert!(repo
            .transition("task", "root", 0, "ready", "running", "commit", None)
            .unwrap());
        assert!(repo
            .begin_commit("task", "root", 0, "{\"phase\":\"prepared\"}")
            .unwrap());
        repo.finish_pending_task("task", "failed", "worker_failed")
            .unwrap();
        let node = repo.node("task", "root", 0).unwrap().unwrap();
        assert_eq!(node.execution_state, "committing");
        assert_eq!(
            node.commit_json.as_deref(),
            Some("{\"phase\":\"prepared\"}")
        );
        drop(repo);
        let task: (String, Option<String>) = db
            .connection()
            .query_row(
                "SELECT status, finished_at FROM tasks WHERE id='task'",
                [],
                |row| Ok((row.get(0)?, row.get(1)?)),
            )
            .unwrap();
        assert_eq!(task, ("recovering".into(), None));
    }
}
