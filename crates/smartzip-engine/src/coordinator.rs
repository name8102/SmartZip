//! In-process stage scheduler and atomic resource admission.

use smartzip_core::{AttemptId, NodeId, TaskId};
use std::collections::{BTreeMap, HashMap, VecDeque};
use std::path::{Path, PathBuf};

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum TaskPriority {
    Background,
    Normal,
    High,
}

impl TaskPriority {
    fn weight(self) -> u64 {
        match self {
            Self::Background => 1,
            Self::Normal => 2,
            Self::High => 4,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Stage {
    ResolveInputs,
    Fingerprint,
    ScanEmbedded,
    ReadMetadata,
    AnalyzeEncoding,
    PrepareAccess,
    ExtractAttempt,
    InspectAndPlan,
    Commit,
    DiscoverChildren,
    ReadMember,
    DecodePreview,
    Cleanup,
}

impl Stage {
    pub fn as_str(self) -> &'static str {
        match self {
            Self::ResolveInputs => "resolve_inputs",
            Self::Fingerprint => "fingerprint",
            Self::ScanEmbedded => "scan_embedded",
            Self::ReadMetadata => "read_metadata",
            Self::AnalyzeEncoding => "analyze_encoding",
            Self::PrepareAccess => "prepare_access",
            Self::ExtractAttempt => "extract_attempt",
            Self::InspectAndPlan => "inspect_and_plan",
            Self::Commit => "commit",
            Self::DiscoverChildren => "discover_children",
            Self::ReadMember => "read_member",
            Self::DecodePreview => "decode_preview",
            Self::Cleanup => "cleanup",
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq)]
pub struct StageRun {
    pub task_id: TaskId,
    pub node_id: NodeId,
    pub root_id: NodeId,
    pub stage: Stage,
    pub generation: u64,
    pub attempt_id: AttemptId,
    pub resources: ResourceRequest,
    pub estimated_cost: u64,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceRequest {
    pub cpu_units: u32,
    pub memory_bytes: u64,
    pub backend_processes: u32,
    pub temporary_bytes: u64,
    pub io_domains: BTreeMap<String, u32>,
    pub artifact_reads: Vec<PathBuf>,
    pub target_paths: Vec<PathBuf>,
}

#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct ResourceCapacity {
    pub cpu_units: u32,
    pub memory_bytes: u64,
    pub backend_processes: u32,
    pub temporary_bytes: u64,
    pub io_domains: BTreeMap<String, u32>,
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum AdmissionFailure {
    TemporarilyUnavailable,
    ExceedsCapacity,
}

#[derive(Debug)]
pub struct ResourceLease {
    id: u64,
    request: ResourceRequest,
}

#[derive(Debug)]
pub struct ResourceBroker {
    capacity: ResourceCapacity,
    used: ResourceRequest,
    next_lease: u64,
    active_paths: HashMap<u64, (Vec<PathBuf>, Vec<PathBuf>)>,
}

impl ResourceBroker {
    pub fn new(capacity: ResourceCapacity) -> Self {
        Self {
            capacity,
            used: ResourceRequest::default(),
            next_lease: 0,
            active_paths: HashMap::new(),
        }
    }

    pub fn try_acquire(
        &mut self,
        request: &ResourceRequest,
    ) -> Result<ResourceLease, AdmissionFailure> {
        if !fits_capacity(request, &self.capacity) {
            return Err(AdmissionFailure::ExceedsCapacity);
        }
        let path_conflict = self.active_paths.values().any(|(reads, writes)| {
            request.target_paths.iter().any(|target| {
                reads
                    .iter()
                    .chain(writes)
                    .any(|path| paths_overlap(target, path))
            }) || request
                .artifact_reads
                .iter()
                .any(|read| writes.iter().any(|target| paths_overlap(read, target)))
        });
        if path_conflict || !fits_available(request, &self.used, &self.capacity) {
            return Err(AdmissionFailure::TemporarilyUnavailable);
        }
        add_request(&mut self.used, request);
        self.next_lease += 1;
        let id = self.next_lease;
        self.active_paths.insert(
            id,
            (request.artifact_reads.clone(), request.target_paths.clone()),
        );
        Ok(ResourceLease {
            id,
            request: request.clone(),
        })
    }

    pub fn release(&mut self, lease: ResourceLease) {
        if self.active_paths.remove(&lease.id).is_some() {
            subtract_request(&mut self.used, &lease.request);
        }
    }

    pub fn used(&self) -> &ResourceRequest {
        &self.used
    }

    pub fn set_io_domain_capacity(&mut self, domain: String, units: u32) {
        self.capacity.io_domains.insert(domain, units);
    }
}

#[derive(Debug)]
struct ScheduledTask {
    priority: TaskPriority,
    queue_position: u64,
    paused: bool,
    service: u64,
    roots: VecDeque<NodeId>,
    ready: BTreeMap<NodeId, VecDeque<StageRun>>,
}

#[derive(Debug)]
pub struct Dispatch {
    pub stage: StageRun,
    pub lease: ResourceLease,
}

#[derive(Debug)]
pub struct TaskCoordinator {
    tasks: BTreeMap<TaskId, ScheduledTask>,
    broker: ResourceBroker,
    next_position: u64,
}

impl TaskCoordinator {
    pub fn new(capacity: ResourceCapacity) -> Self {
        Self {
            tasks: BTreeMap::new(),
            broker: ResourceBroker::new(capacity),
            next_position: 0,
        }
    }

    pub fn submit(&mut self, task_id: TaskId, priority: TaskPriority) {
        let position = self.next_position;
        self.next_position += 1;
        self.tasks.entry(task_id).or_insert(ScheduledTask {
            priority,
            queue_position: position,
            paused: false,
            service: 0,
            roots: VecDeque::new(),
            ready: BTreeMap::new(),
        });
    }

    pub fn enqueue(&mut self, stage: StageRun) -> bool {
        let Some(task) = self.tasks.get_mut(&stage.task_id) else {
            return false;
        };
        if !task.ready.contains_key(&stage.root_id) {
            task.roots.push_back(stage.root_id.clone());
        }
        task.ready
            .entry(stage.root_id.clone())
            .or_default()
            .push_back(stage);
        true
    }

    pub fn set_priority(&mut self, task_id: &TaskId, priority: TaskPriority) -> bool {
        self.tasks.get_mut(task_id).is_some_and(|task| {
            task.priority = priority;
            true
        })
    }

    pub fn set_paused(&mut self, task_id: &TaskId, paused: bool) -> bool {
        self.tasks.get_mut(task_id).is_some_and(|task| {
            task.paused = paused;
            true
        })
    }

    pub fn reorder(&mut self, task_id: &TaskId, position: u64) -> bool {
        self.tasks.get_mut(task_id).is_some_and(|task| {
            task.queue_position = position;
            true
        })
    }

    pub fn remove(&mut self, task_id: &TaskId) {
        self.tasks.remove(task_id);
    }

    pub fn remove_queued_stage(
        &mut self,
        task_id: &TaskId,
        node_id: &NodeId,
        generation: u64,
        attempt_id: &AttemptId,
    ) -> bool {
        let Some(task) = self.tasks.get_mut(task_id) else {
            return false;
        };
        let mut removed = false;
        for queue in task.ready.values_mut() {
            let before = queue.len();
            queue.retain(|stage| {
                stage.node_id != *node_id
                    || stage.generation != generation
                    || stage.attempt_id != *attempt_id
            });
            removed |= queue.len() != before;
        }
        task.ready.retain(|_, queue| !queue.is_empty());
        task.roots.retain(|root| task.ready.contains_key(root));
        removed
    }

    /// Examine every task and every root head. A blocked head cannot stop an
    /// unrelated stage from using otherwise idle resources.
    pub fn dispatch_next(&mut self) -> Option<Dispatch> {
        let mut candidates: Vec<_> = self
            .tasks
            .iter()
            .filter(|(_, task)| !task.paused)
            .flat_map(|(task_id, task)| {
                task.roots.iter().filter_map(move |root_id| {
                    task.ready
                        .get(root_id)
                        .and_then(|queue| queue.front())
                        .map(|stage| {
                            (
                                task_id.clone(),
                                root_id.clone(),
                                task.service / task.priority.weight(),
                                task.queue_position,
                                stage.clone(),
                            )
                        })
                })
            })
            .collect();
        candidates.sort_by_key(|(_, _, service, position, _)| (*service, *position));

        for (task_id, root_id, _, _, stage) in candidates {
            let Ok(lease) = self.broker.try_acquire(&stage.resources) else {
                continue;
            };
            let task = self.tasks.get_mut(&task_id).expect("candidate task exists");
            let queue = task.ready.get_mut(&root_id).expect("candidate root exists");
            let dispatched = queue.pop_front().expect("candidate stage exists");
            if queue.is_empty() {
                task.ready.remove(&root_id);
                task.roots.retain(|root| root != &root_id);
            } else {
                task.roots.retain(|root| root != &root_id);
                task.roots.push_back(root_id);
            }
            task.service = task
                .service
                .saturating_add(dispatched.estimated_cost.max(1));
            return Some(Dispatch {
                stage: dispatched,
                lease,
            });
        }
        None
    }

    pub fn complete(&mut self, dispatch: Dispatch, actual_cost: u64) {
        self.broker.release(dispatch.lease);
        if let Some(task) = self.tasks.get_mut(&dispatch.stage.task_id) {
            let estimated = dispatch.stage.estimated_cost.max(1);
            if actual_cost > estimated {
                task.service = task.service.saturating_add(actual_cost - estimated);
            } else {
                task.service = task.service.saturating_sub(estimated - actual_cost.max(1));
            }
        }
    }

    pub fn resources(&self) -> &ResourceBroker {
        &self.broker
    }

    pub fn set_io_domain_capacity(&mut self, domain: String, units: u32) {
        self.broker.set_io_domain_capacity(domain, units);
    }
}

fn fits_capacity(request: &ResourceRequest, capacity: &ResourceCapacity) -> bool {
    request.cpu_units <= capacity.cpu_units
        && request.memory_bytes <= capacity.memory_bytes
        && request.backend_processes <= capacity.backend_processes
        && request.temporary_bytes <= capacity.temporary_bytes
        && request.io_domains.iter().all(|(domain, amount)| {
            *amount <= capacity.io_domains.get(domain).copied().unwrap_or(0)
        })
}

fn fits_available(
    request: &ResourceRequest,
    used: &ResourceRequest,
    capacity: &ResourceCapacity,
) -> bool {
    request.cpu_units <= capacity.cpu_units.saturating_sub(used.cpu_units)
        && request.memory_bytes <= capacity.memory_bytes.saturating_sub(used.memory_bytes)
        && request.backend_processes
            <= capacity
                .backend_processes
                .saturating_sub(used.backend_processes)
        && request.temporary_bytes
            <= capacity
                .temporary_bytes
                .saturating_sub(used.temporary_bytes)
        && request.io_domains.iter().all(|(domain, amount)| {
            *amount
                <= capacity
                    .io_domains
                    .get(domain)
                    .copied()
                    .unwrap_or(0)
                    .saturating_sub(used.io_domains.get(domain).copied().unwrap_or(0))
        })
}

fn add_request(used: &mut ResourceRequest, request: &ResourceRequest) {
    used.cpu_units += request.cpu_units;
    used.memory_bytes += request.memory_bytes;
    used.backend_processes += request.backend_processes;
    used.temporary_bytes += request.temporary_bytes;
    for (domain, amount) in &request.io_domains {
        *used.io_domains.entry(domain.clone()).or_default() += amount;
    }
}

fn subtract_request(used: &mut ResourceRequest, request: &ResourceRequest) {
    used.cpu_units -= request.cpu_units;
    used.memory_bytes -= request.memory_bytes;
    used.backend_processes -= request.backend_processes;
    used.temporary_bytes -= request.temporary_bytes;
    for (domain, amount) in &request.io_domains {
        let remove = {
            let entry = used
                .io_domains
                .get_mut(domain)
                .expect("leased domain exists");
            *entry -= amount;
            *entry == 0
        };
        if remove {
            used.io_domains.remove(domain);
        }
    }
}

fn paths_overlap(left: &Path, right: &Path) -> bool {
    left == right || left.starts_with(right) || right.starts_with(left)
}

#[cfg(test)]
mod tests {
    use super::*;

    fn stage(task: &TaskId, root: &NodeId, cost: u64, cpu: u32, domain: &str) -> StageRun {
        StageRun {
            task_id: task.clone(),
            node_id: root.clone(),
            root_id: root.clone(),
            stage: Stage::ExtractAttempt,
            generation: 0,
            attempt_id: AttemptId::new(),
            resources: ResourceRequest {
                cpu_units: cpu,
                io_domains: BTreeMap::from([(domain.into(), 1)]),
                ..Default::default()
            },
            estimated_cost: cost,
        }
    }

    fn coordinator() -> TaskCoordinator {
        TaskCoordinator::new(ResourceCapacity {
            cpu_units: 2,
            backend_processes: 2,
            io_domains: BTreeMap::from([("a".into(), 1), ("b".into(), 1)]),
            ..Default::default()
        })
    }

    #[test]
    fn admission_is_atomic_and_release_restores_all_dimensions() {
        let mut broker = ResourceBroker::new(ResourceCapacity {
            cpu_units: 2,
            memory_bytes: 20,
            backend_processes: 1,
            temporary_bytes: 30,
            io_domains: BTreeMap::from([("disk".into(), 1)]),
        });
        let request = ResourceRequest {
            cpu_units: 2,
            memory_bytes: 20,
            backend_processes: 1,
            temporary_bytes: 30,
            io_domains: BTreeMap::from([("disk".into(), 1)]),
            ..Default::default()
        };
        let lease = broker.try_acquire(&request).unwrap();
        assert_eq!(
            broker.try_acquire(&request).unwrap_err(),
            AdmissionFailure::TemporarilyUnavailable
        );
        broker.release(lease);
        assert_eq!(broker.used(), &ResourceRequest::default());
    }

    #[test]
    fn blocked_head_does_not_stop_fitting_work() {
        let mut coordinator = coordinator();
        let first = TaskId::new();
        let second = TaskId::new();
        let root_a = NodeId::new();
        let root_b = NodeId::new();
        coordinator.submit(first.clone(), TaskPriority::Normal);
        coordinator.submit(second.clone(), TaskPriority::Normal);
        coordinator.enqueue(stage(&first, &root_a, 1, 3, "a"));
        coordinator.enqueue(stage(&second, &root_b, 1, 1, "b"));
        let dispatch = coordinator.dispatch_next().unwrap();
        assert_eq!(dispatch.stage.task_id, second);
    }

    #[test]
    fn weighted_service_still_dispatches_background_task() {
        let mut coordinator = coordinator();
        let high = TaskId::new();
        let background = TaskId::new();
        let high_root = NodeId::new();
        let background_root = NodeId::new();
        coordinator.submit(high.clone(), TaskPriority::High);
        coordinator.submit(background.clone(), TaskPriority::Background);
        for _ in 0..8 {
            coordinator.enqueue(stage(&high, &high_root, 1, 1, "a"));
        }
        coordinator.enqueue(stage(&background, &background_root, 1, 1, "a"));
        let mut order = Vec::new();
        for _ in 0..5 {
            let dispatch = coordinator.dispatch_next().unwrap();
            order.push(dispatch.stage.task_id.clone());
            coordinator.complete(dispatch, 1);
        }
        assert!(order.contains(&background));
    }

    #[test]
    fn write_lease_conflicts_with_ancestor_read() {
        let mut broker = ResourceBroker::new(ResourceCapacity::default());
        let read = ResourceRequest {
            artifact_reads: vec!["/root/tree/file".into()],
            ..Default::default()
        };
        let lease = broker.try_acquire(&read).unwrap();
        let write = ResourceRequest {
            target_paths: vec!["/root/tree".into()],
            ..Default::default()
        };
        assert_eq!(
            broker.try_acquire(&write).unwrap_err(),
            AdmissionFailure::TemporarilyUnavailable
        );
        broker.release(lease);
        assert!(broker.try_acquire(&write).is_ok());
    }
}
