//! Optional limits for the managed extraction tree. Polling bounds resource use
//! at checkpoints, not to the last byte a subprocess can write between checks.
use smartzip_core::{Result, SmartZipError, TaskExecutionContext};
use std::collections::HashMap;
use std::path::{Path, PathBuf};
use std::sync::{Arc, Mutex};

pub use smartzip_config::ExtractionLimits;
#[derive(Debug, Default, Clone, Copy, PartialEq, Eq)]
pub(crate) struct Usage {
    pub files: u64,
    pub bytes: u64,
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub(crate) struct InventoryFile {
    pub relative_path: PathBuf,
    pub size: u64,
}

#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub(crate) struct OutputInventory {
    pub root: PathBuf,
    pub usage: Usage,
    pub files: Vec<InventoryFile>,
}

impl OutputInventory {
    pub(crate) fn published_files(
        &self,
        plan: &crate::layout::LayoutPlan,
    ) -> Option<Vec<InventoryFile>> {
        use crate::layout::{LayoutPlanKind, PlanSource};

        let source = match (&plan.kind, &plan.source) {
            (LayoutPlanKind::PreserveBothSingleDir | LayoutPlanKind::PreserveBothSingleFile, _)
            | (_, PlanSource::WholeTempDir) => return Some(self.files.clone()),
            (
                _,
                PlanSource::SingleDir(path)
                | PlanSource::SingleDirContents(path)
                | PlanSource::SingleFile(path),
            ) => path.strip_prefix(&self.root).ok()?,
        };

        Some(
            self.files
                .iter()
                .filter_map(|file| {
                    let relative_path = if file.relative_path == source {
                        PathBuf::new()
                    } else {
                        file.relative_path.strip_prefix(source).ok()?.to_path_buf()
                    };
                    Some(InventoryFile {
                        relative_path,
                        size: file.size,
                    })
                })
                .collect(),
        )
    }
}

#[derive(Debug, Default)]
struct TaskBudgetState {
    committed: Usage,
    in_flight: HashMap<u64, Usage>,
    next_attempt: u64,
    nested_candidates: usize,
}

#[derive(Debug, Default)]
pub(crate) struct TaskBudget {
    state: Mutex<TaskBudgetState>,
}

impl TaskBudget {
    pub(crate) fn from_snapshot(snapshot: crate::TaskBudgetSnapshot) -> Self {
        Self {
            state: Mutex::new(TaskBudgetState {
                committed: Usage {
                    files: snapshot.output_files,
                    bytes: snapshot.output_bytes,
                },
                nested_candidates: snapshot.nested_candidates,
                ..TaskBudgetState::default()
            }),
        }
    }

    pub(crate) fn reserve_attempt(self: &Arc<Self>) -> TaskBudgetReservation {
        let mut state = self.state.lock().unwrap();
        let id = state.next_attempt;
        state.next_attempt += 1;
        state.in_flight.insert(id, Usage::default());
        TaskBudgetReservation {
            budget: self.clone(),
            id,
            committed: false,
        }
    }

    pub(crate) fn reserve_nested(&self, limit: usize) -> bool {
        let mut state = self.state.lock().unwrap();
        if state.nested_candidates >= limit {
            return false;
        }
        state.nested_candidates += 1;
        true
    }

    pub(crate) fn release_nested(&self) {
        let mut state = self.state.lock().unwrap();
        state.nested_candidates -= 1;
    }
}

pub(crate) struct TaskBudgetReservation {
    budget: Arc<TaskBudget>,
    id: u64,
    committed: bool,
}

impl TaskBudgetReservation {
    fn update(&self, usage: Usage, limits: &ExtractionLimits) -> Result<()> {
        let mut state = self.budget.state.lock().unwrap();
        let other = state
            .in_flight
            .iter()
            .filter(|(id, _)| **id != self.id)
            .fold(state.committed, |total, (_, usage)| Usage {
                files: total.files.saturating_add(usage.files),
                bytes: total.bytes.saturating_add(usage.bytes),
            });
        let total = Usage {
            files: other.files.saturating_add(usage.files),
            bytes: other.bytes.saturating_add(usage.bytes),
        };
        if limits.max_files != 0 && total.files > limits.max_files {
            return Err(exceeded(format!(
                "output entry limit {} exceeded",
                limits.max_files
            )));
        }
        if limits.max_output_bytes != 0 && total.bytes > limits.max_output_bytes {
            return Err(exceeded(format!(
                "output byte limit {} exceeded",
                limits.max_output_bytes
            )));
        }
        state.in_flight.insert(self.id, usage);
        Ok(())
    }

    pub(crate) fn commit(&mut self) -> Usage {
        let mut state = self.budget.state.lock().unwrap();
        let usage = state
            .in_flight
            .remove(&self.id)
            .expect("budget reservation exists until commit");
        state.committed.files = state.committed.files.saturating_add(usage.files);
        state.committed.bytes = state.committed.bytes.saturating_add(usage.bytes);
        self.committed = true;
        usage
    }
}

impl Drop for TaskBudgetReservation {
    fn drop(&mut self) {
        if !self.committed {
            self.budget.state.lock().unwrap().in_flight.remove(&self.id);
        }
    }
}

pub(crate) fn exceeded(detail: impl Into<String>) -> SmartZipError {
    SmartZipError::ResourceLimit {
        detail: detail.into(),
    }
}

fn inspect_inventory(
    path: &Path,
    limits: &ExtractionLimits,
    previous: Usage,
    collect_files: bool,
) -> Result<OutputInventory> {
    let mut usage = previous;
    let mut files = Vec::new();
    // Streaming traversal with no following symlinks. The final pass retains
    // regular-file paths so nested discovery does not need to walk the tree again.
    for entry in walkdir::WalkDir::new(path).follow_links(false) {
        let entry = match entry {
            Ok(entry) => entry,
            Err(error)
                if error
                    .io_error()
                    .is_some_and(|e| e.kind() == std::io::ErrorKind::NotFound) =>
            {
                continue
            }
            Err(error) => {
                return Err(SmartZipError::io(
                    Some(path.into()),
                    std::io::Error::other(error),
                ))
            }
        };
        if entry.depth() == 0 && entry.file_type().is_dir() {
            continue;
        }
        let metadata = match std::fs::symlink_metadata(entry.path()) {
            Ok(metadata) => metadata,
            Err(error) if error.kind() == std::io::ErrorKind::NotFound => continue,
            Err(error) => return Err(SmartZipError::io(Some(entry.path().into()), error)),
        };
        usage.files = usage.files.saturating_add(1);
        if metadata.is_file() {
            usage.bytes = usage.bytes.saturating_add(metadata.len());
            if collect_files {
                files.push(InventoryFile {
                    relative_path: entry
                        .path()
                        .strip_prefix(path)
                        .unwrap_or(entry.path())
                        .to_path_buf(),
                    size: metadata.len(),
                });
            }
        }
        if limits.max_files != 0 && usage.files > limits.max_files {
            return Err(exceeded(format!(
                "output entry limit {} exceeded",
                limits.max_files
            )));
        }
        if limits.max_output_bytes != 0 && usage.bytes > limits.max_output_bytes {
            return Err(exceeded(format!(
                "output byte limit {} exceeded",
                limits.max_output_bytes
            )));
        }
    }
    check_free_space(path, limits)?;
    Ok(OutputInventory {
        root: path.to_path_buf(),
        usage,
        files,
    })
}

#[cfg(test)]
pub(crate) fn inspect(path: &Path, limits: &ExtractionLimits, previous: Usage) -> Result<Usage> {
    inspect_inventory(path, limits, previous, false).map(|inventory| inventory.usage)
}

fn check_free_space(path: &Path, limits: &ExtractionLimits) -> Result<()> {
    if limits.min_free_bytes == 0 {
        return Ok(());
    }
    if free_bytes(path)? < limits.min_free_bytes {
        return Err(exceeded(format!(
            "free disk space fell below {} bytes",
            limits.min_free_bytes
        )));
    }
    Ok(())
}

fn free_bytes(path: &Path) -> Result<u64> {
    #[cfg(unix)]
    {
        use std::os::unix::ffi::OsStrExt;
        let path = std::ffi::CString::new(path.as_os_str().as_bytes())
            .map_err(|e| SmartZipError::io(Some(path.into()), std::io::Error::other(e)))?;
        let mut stat = std::mem::MaybeUninit::<libc::statvfs>::uninit();
        // SAFETY: path is a live terminated string and stat is writable.
        if unsafe { libc::statvfs(path.as_ptr(), stat.as_mut_ptr()) } != 0 {
            return Err(SmartZipError::io(None, std::io::Error::last_os_error()));
        }
        // SAFETY: successful statvfs initialized the structure.
        let stat = unsafe { stat.assume_init() };
        Ok((stat.f_bavail as u64).saturating_mul(stat.f_frsize as u64))
    }
    #[cfg(not(unix))]
    {
        let _ = path;
        Err(exceeded(
            "disk budget monitoring is unsupported on this platform",
        ))
    }
}

// Both tree traversal and statvfs can block on a slow filesystem. Keep the
// handle owned until completion, including when the backend finishes first.
fn scan(
    path: &Path,
    limits: &ExtractionLimits,
    previous: Usage,
    full: bool,
    collect_files: bool,
) -> tokio::task::JoinHandle<Result<OutputInventory>> {
    let path = path.to_owned();
    let limits = limits.clone();
    tokio::task::spawn_blocking(move || {
        if full {
            inspect_inventory(&path, &limits, previous, collect_files)
        } else {
            check_free_space(&path, &limits).map(|()| OutputInventory {
                root: path,
                usage: previous,
                files: Vec::new(),
            })
        }
    })
}

fn scan_result(
    result: std::result::Result<Result<OutputInventory>, tokio::task::JoinError>,
) -> Result<OutputInventory> {
    result.map_err(|error| SmartZipError::io(None, std::io::Error::other(error)))?
}

#[cfg(test)]
pub(crate) async fn monitor<T>(
    path: &Path,
    limits: &ExtractionLimits,
    previous: Usage,
    context: Arc<TaskExecutionContext>,
    operation: impl std::future::Future<Output = Result<T>>,
) -> Result<(T, Usage)> {
    let budget = Arc::new(TaskBudget::from_snapshot(crate::TaskBudgetSnapshot {
        output_files: previous.files,
        output_bytes: previous.bytes,
        ..Default::default()
    }));
    let reservation = budget.reserve_attempt();
    let (value, inventory) = monitor_task(path, limits, &reservation, context, operation).await?;
    let usage = inventory.usage;
    Ok((
        value,
        Usage {
            files: previous.files.saturating_add(usage.files),
            bytes: previous.bytes.saturating_add(usage.bytes),
        },
    ))
}

pub(crate) async fn monitor_task<T>(
    path: &Path,
    limits: &ExtractionLimits,
    reservation: &TaskBudgetReservation,
    context: Arc<TaskExecutionContext>,
    operation: impl std::future::Future<Output = Result<T>>,
) -> Result<(T, OutputInventory)> {
    use std::time::Duration;
    use tokio::time::{Instant, MissedTickBehavior};

    let tree_limits = limits.max_files != 0 || limits.max_output_bytes != 0;
    if !tree_limits && limits.min_free_bytes == 0 {
        let value = operation.await?;
        if context.is_cancelled() {
            return Err(SmartZipError::Cancelled);
        }
        // One final inventory for history/recovery; no polling of the live tree.
        let inventory = scan_result(scan(path, limits, Usage::default(), true, true).await)?;
        reservation.update(inventory.usage, limits)?;
        if context.is_cancelled() {
            return Err(SmartZipError::Cancelled);
        }
        return Ok((value, inventory));
    }
    let initial = scan_result(scan(path, limits, Usage::default(), tree_limits, false).await)?;
    reservation.update(initial.usage, limits)?;
    let mut operation = std::pin::pin!(operation);
    let period = Duration::from_secs(1);
    let mut interval = tokio::time::interval_at(Instant::now() + period, period);
    interval.set_missed_tick_behavior(MissedTickBehavior::Delay);
    let mut next_tree = Instant::now() + period;
    let value = loop {
        tokio::select! {
            result = &mut operation => break result?,
            _ = interval.tick() => {
                let started = Instant::now();
                let full = tree_limits && started >= next_tree;
                let mut pending = scan(path, limits, Usage::default(), full, false);
                let result = tokio::select! {
                    result = &mut operation => {
                        let checked = scan_result(pending.await);
                        let value = result?;
                        let inventory = checked?;
                        if full {
                            reservation.update(inventory.usage, limits)?;
                        }
                        break value;
                    }
                    result = &mut pending => scan_result(result),
                };
                let checked = result.and_then(|inventory| {
                    if full {
                        reservation.update(inventory.usage, limits)
                    } else {
                        Ok(())
                    }
                });
                if let Err(error) = checked {
                        context.cancel();
                        let _ = operation.await;
                        return Err(error);
                }
                if full {
                    let delay = (started.elapsed() * 4).max(period);
                    next_tree = Instant::now() + delay;
                }
            }
        }
    };
    if context.is_cancelled() {
        return Err(SmartZipError::Cancelled);
    }
    let inventory = scan_result(scan(path, limits, Usage::default(), true, true).await)?;
    reservation.update(inventory.usage, limits)?;
    if context.is_cancelled() {
        return Err(SmartZipError::Cancelled);
    }
    Ok((value, inventory))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn disabled_disk_limit_does_not_inspect_the_filesystem() {
        let root = tempfile::tempdir().unwrap();
        check_free_space(&root.path().join("missing"), &ExtractionLimits::default()).unwrap();
    }

    #[test]
    fn default_limits_accept_large_outputs() {
        let budget = Arc::new(TaskBudget::default());
        budget
            .reserve_attempt()
            .update(
                Usage {
                    files: 200_000,
                    bytes: 100 * 1024 * 1024 * 1024,
                },
                &ExtractionLimits::default(),
            )
            .unwrap();
    }

    #[cfg(unix)]
    #[test]
    fn output_inventory_accepts_links_without_following_them() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file"), b"hello").unwrap();
        std::fs::hard_link(root.path().join("file"), root.path().join("hard")).unwrap();
        std::os::unix::fs::symlink("file", root.path().join("link")).unwrap();
        std::os::unix::fs::symlink("missing", root.path().join("dangling")).unwrap();
        std::os::unix::fs::symlink(".", root.path().join("loop")).unwrap();
        let usage = inspect(root.path(), &ExtractionLimits::default(), Usage::default()).unwrap();
        assert_eq!(
            usage,
            Usage {
                files: 5,
                bytes: 10
            }
        );
    }

    #[test]
    fn published_inventory_follows_the_selected_layout_source() {
        let staging = PathBuf::from("/tmp/smartzip-staging");
        let inventory = OutputInventory {
            root: staging.clone(),
            usage: Usage::default(),
            files: vec![
                InventoryFile {
                    relative_path: PathBuf::from("wrapper/nested.zip"),
                    size: 123,
                },
                InventoryFile {
                    relative_path: PathBuf::from("ignored.txt"),
                    size: 7,
                },
            ],
        };
        let plan = crate::layout::LayoutPlan {
            source: crate::layout::PlanSource::SingleDir(staging.join("wrapper")),
            kind: crate::layout::LayoutPlanKind::CommitSingleDirAsInnerName,
            target: PathBuf::from("/published/wrapper"),
            reason: crate::layout::LayoutDecisionReason::SingleDirGoodName,
            warnings: Vec::new(),
        };

        assert_eq!(
            inventory.published_files(&plan).unwrap(),
            vec![InventoryFile {
                relative_path: PathBuf::from("nested.zip"),
                size: 123,
            }]
        );
    }

    #[test]
    fn task_budget_counts_concurrent_roots_and_nested_candidates_once() {
        let budget = Arc::new(TaskBudget::default());
        let limits = ExtractionLimits {
            max_files: 10,
            max_output_bytes: 20,
            min_free_bytes: 0,
            max_nested_candidates: 2,
        };
        let mut first = budget.reserve_attempt();
        let mut second = budget.reserve_attempt();
        first
            .update(
                Usage {
                    files: 1,
                    bytes: 15,
                },
                &limits,
            )
            .unwrap();
        assert!(second
            .update(Usage { files: 1, bytes: 6 }, &limits)
            .is_err());
        second
            .update(Usage { files: 1, bytes: 5 }, &limits)
            .unwrap();
        first.commit();
        second.commit();

        assert!(budget.reserve_nested(limits.max_nested_candidates));
        assert!(budget.reserve_nested(limits.max_nested_candidates));
        assert!(!budget.reserve_nested(limits.max_nested_candidates));
    }

    #[tokio::test(flavor = "current_thread")]
    async fn final_usage_includes_last_backend_write_and_prior_outputs() {
        let root = tempfile::tempdir().unwrap();
        let limits = ExtractionLimits {
            min_free_bytes: 0,
            ..Default::default()
        };
        let (_, usage) = monitor(
            root.path(),
            &limits,
            Usage { files: 2, bytes: 7 },
            Arc::new(TaskExecutionContext::detached()),
            async {
                tokio::task::yield_now().await;
                std::fs::write(root.path().join("last"), [0; 5]).unwrap();
                Ok(())
            },
        )
        .await
        .unwrap();
        assert_eq!((usage.files, usage.bytes), (3, 12));
    }

    #[test]
    fn waiting_for_filesystem_worker_keeps_runtime_responsive() {
        let runtime = tokio::runtime::Builder::new_current_thread()
            .enable_all()
            .max_blocking_threads(1)
            .build()
            .unwrap();
        runtime.block_on(async {
            let (release, wait) = std::sync::mpsc::channel();
            let occupied = tokio::task::spawn_blocking(move || wait.recv().unwrap());
            let ticked = Arc::new(std::sync::atomic::AtomicBool::new(false));
            let heartbeat_ticked = ticked.clone();
            let heartbeat = tokio::spawn(async move {
                tokio::task::yield_now().await;
                heartbeat_ticked.store(true, std::sync::atomic::Ordering::SeqCst);
                release.send(()).unwrap();
            });
            let root = tempfile::tempdir().unwrap();
            let limits = ExtractionLimits {
                min_free_bytes: 0,
                ..Default::default()
            };
            monitor(
                root.path(),
                &limits,
                Usage::default(),
                Arc::new(TaskExecutionContext::detached()),
                async { Ok(()) },
            )
            .await
            .unwrap();
            assert!(ticked.load(std::sync::atomic::Ordering::SeqCst));
            heartbeat.await.unwrap();
            occupied.await.unwrap();
        });
    }

    #[tokio::test]
    async fn growing_output_stops_backend_and_fails_budget() {
        let root = tempfile::tempdir().unwrap();
        let context = Arc::new(TaskExecutionContext::detached());
        let token = context.cancellation_token();
        let limits = ExtractionLimits {
            max_output_bytes: 8,
            min_free_bytes: 0,
            ..Default::default()
        };
        let stopped = std::cell::Cell::new(false);
        let result = monitor(root.path(), &limits, Usage::default(), context, async {
            std::fs::write(root.path().join("bomb"), [0; 9]).unwrap();
            token.cancelled().await;
            stopped.set(true);
            Err::<(), _>(SmartZipError::Cancelled)
        })
        .await;
        assert!(matches!(result, Err(SmartZipError::ResourceLimit { .. })));
        assert!(stopped.get());
    }
    #[test]
    fn cumulative_count_and_bytes_are_enforced() {
        let root = tempfile::tempdir().unwrap();
        std::fs::write(root.path().join("file"), [0; 5]).unwrap();
        let limits = ExtractionLimits {
            max_files: 1,
            max_output_bytes: 8,
            min_free_bytes: 0,
            ..Default::default()
        };
        assert!(inspect(root.path(), &limits, Usage { files: 1, bytes: 0 }).is_err());
        assert!(inspect(root.path(), &limits, Usage { files: 0, bytes: 4 }).is_err());
    }
}
