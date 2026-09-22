//! Source recycling after the entire extraction succeeds. Only the winning
//! volume set is eligible; failed hypotheses and unrelated siblings are kept.
use crate::{ArchiveRecycleHandler, ExtractWorkflowResult};
use std::collections::{BTreeSet, HashMap};
use std::path::{Path, PathBuf};

pub(crate) type SharedCleanup = Option<std::rc::Rc<std::cell::RefCell<SourceCleanup>>>;

#[derive(Clone)]
struct SourceSnapshot {
    path: PathBuf,
    metadata: std::fs::Metadata,
}
impl SourceSnapshot {
    fn capture(path: &Path) -> std::io::Result<Self> {
        let metadata = std::fs::symlink_metadata(path)?;
        if !metadata.is_file() {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidInput,
                "source is not a regular file",
            ));
        }
        Ok(Self {
            path: path.into(),
            metadata,
        })
    }
    fn unchanged(&self) -> bool {
        std::fs::symlink_metadata(&self.path).is_ok_and(|now| {
            let basic = now.is_file()
                && now.len() == self.metadata.len()
                && now.modified().ok() == self.metadata.modified().ok();
            #[cfg(unix)]
            {
                use std::os::unix::fs::MetadataExt;
                basic
                    && now.dev() == self.metadata.dev()
                    && now.ino() == self.metadata.ino()
                    && now.ctime() == self.metadata.ctime()
                    && now.ctime_nsec() == self.metadata.ctime_nsec()
            }
            #[cfg(not(unix))]
            {
                basic && now.created().ok() == self.metadata.created().ok()
            }
        })
    }
}
fn absolute(path: &Path) -> PathBuf {
    std::path::absolute(path).unwrap_or_else(|_| path.into())
}

pub(crate) struct SourceCleanup {
    requested: Vec<PathBuf>,
    captured: HashMap<PathBuf, Option<SourceSnapshot>>,
    successful: BTreeSet<PathBuf>,
    outputs: Vec<PathBuf>,
}
impl SourceCleanup {
    pub(crate) fn new(inputs: &[PathBuf]) -> Self {
        let requested: Vec<_> = inputs.iter().map(|p| absolute(p)).collect();
        let mut cleanup = Self {
            requested: requested.clone(),
            captured: HashMap::new(),
            successful: BTreeSet::new(),
            outputs: Vec::new(),
        };
        cleanup.capture(&requested);
        cleanup
    }
    pub(crate) fn capture(&mut self, paths: &[PathBuf]) {
        for path in paths {
            let path = absolute(path);
            self.captured
                .entry(path.clone())
                .or_insert_with(|| SourceSnapshot::capture(&path).ok());
        }
    }
    pub(crate) fn committed(&mut self, paths: &[PathBuf], output: &Path, entries: u64) {
        if entries != 0 {
            self.successful.extend(paths.iter().map(|p| absolute(p)));
            self.outputs.push(output.into());
        }
    }
    fn plan(&self, result: &ExtractWorkflowResult) -> Option<Vec<SourceSnapshot>> {
        if result.status != crate::history::TaskCompletionStatus::Completed
            || result.failed_count != 0
            || self.successful.is_empty()
            || !self.requested.iter().all(|p| self.successful.contains(p))
            || !result
                .skipped
                .iter()
                .all(|c| c.depth == 0 && self.successful.contains(&absolute(&c.path)))
            || !self
                .outputs
                .iter()
                .all(|p| std::fs::symlink_metadata(p).is_ok())
        {
            return None;
        }
        self.successful
            .iter()
            .map(|p| self.captured.get(p)?.clone())
            .collect::<Option<Vec<_>>>()
            .filter(|sources| sources.iter().all(SourceSnapshot::unchanged))
    }
    pub(crate) async fn finish(
        cleanup: SharedCleanup,
        result: &mut ExtractWorkflowResult,
        cancellation: &tokio_util::sync::CancellationToken,
        recycler: &ArchiveRecycleHandler,
        events: &crate::events::EventSink,
    ) {
        let Some(cleanup) = cleanup else {
            return;
        };
        let plan = cleanup.borrow().plan(result);
        let mut warnings = Vec::new();
        if cancellation.is_cancelled() || plan.is_none() {
            warnings.push(
                "原包已保留：任务未全部成功、源文件变化、输出为空或缺少源归档提交记录".into(),
            );
        } else if let Some(sources) = plan {
            let cancellation = cancellation.clone();
            let recycler = recycler.clone();
            let recycled = tokio::task::spawn_blocking(move || {
                let mut warnings = Vec::new();
                for source in sources {
                    if cancellation.is_cancelled() || !source.unchanged() {
                        warnings.push(format!("原包已保留：{}", source.path.display()));
                    } else if let Err(error) = recycler(source.path.clone()) {
                        warnings.push(format!("原包回收失败 {}：{error}", source.path.display()));
                    }
                }
                warnings
            })
            .await;
            match recycled {
                Ok(messages) => warnings.extend(messages),
                Err(error) => warnings.push(format!("原包回收未完成：{error}")),
            }
        }
        for message in warnings {
            let event = smartzip_core::TaskEvent {
                task_id: result.task_id.clone(),
                kind: smartzip_core::TaskEventKind::Warning { message },
            };
            events.push(event);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    fn completed() -> ExtractWorkflowResult {
        ExtractWorkflowResult {
            status: crate::history::TaskCompletionStatus::Completed,
            failed_count: 0,
            task_id: smartzip_core::TaskId::new(),
            processed: vec![],
            skipped: vec![],
            enqueued: vec![],
            events: vec![],
        }
    }
    #[test]
    fn empty_success_never_recycles_input() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("archive.zip");
        std::fs::write(&path, b"data").unwrap();
        let mut cleanup = SourceCleanup::new(&[path.clone()]);
        assert!(cleanup.plan(&completed()).is_none());
        let output = temp.path().join("empty");
        std::fs::create_dir(&output).unwrap();
        cleanup.committed(&[path.clone()], &output, 0);
        assert!(cleanup.plan(&completed()).is_none());
        assert!(path.exists());
    }
    #[test]
    fn source_replacement_is_detected_even_with_same_size() {
        let temp = tempfile::tempdir().unwrap();
        let path = temp.path().join("archive.zip");
        std::fs::write(&path, b"same").unwrap();
        let source = SourceSnapshot::capture(&path).unwrap();
        let replacement = temp.path().join("replacement.zip");
        std::fs::write(&replacement, b"same").unwrap();
        std::fs::rename(replacement, &path).unwrap();
        assert!(!source.unchanged());
    }
    #[test]
    fn cleanup_uses_only_winning_members_and_accepts_duplicate_inputs() {
        let temp = tempfile::tempdir().unwrap();
        let paths: Vec<_> = ["first", "second", "unused"]
            .map(|name| temp.path().join(name))
            .into();
        for path in &paths {
            std::fs::write(path, b"volume").unwrap();
        }
        let output = temp.path().join("output");
        std::fs::write(&output, b"data").unwrap();
        let mut cleanup = SourceCleanup::new(&paths[..2]);
        cleanup.capture(&paths);
        cleanup.committed(&paths[..2], &output, 1);
        let mut result = completed();
        result
            .skipped
            .push(crate::ExtractionCandidate::root(paths[1].clone()));
        assert_eq!(cleanup.plan(&result).unwrap().len(), 2);
        result.failed_count = 1;
        assert!(cleanup.plan(&result).is_none());
        result.failed_count = 0;
        result.status = crate::history::TaskCompletionStatus::Cancelled;
        assert!(cleanup.plan(&result).is_none());
        result.status = crate::history::TaskCompletionStatus::Completed;
        std::fs::write(&paths[1], b"changed volume").unwrap();
        assert!(cleanup.plan(&result).is_none());
    }
}
