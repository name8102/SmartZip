//! Task event sink and listener types.

use smartzip_core::{TaskEvent, TaskEventKind, TaskEventSink};
use std::{
    collections::VecDeque,
    sync::{Arc, Mutex},
};

pub type TaskEventListener = Arc<dyn Fn(&TaskEvent) + Send + Sync>;
const RETAINED_EVENTS: usize = 4096;

#[derive(Default)]
struct RetainedEvents {
    events: VecDeque<TaskEvent>,
    dropped: usize,
}

#[derive(Clone)]
pub(crate) struct EventSink {
    events: Arc<Mutex<RetainedEvents>>,
    listener: Option<TaskEventListener>,
}

impl TaskEventSink for EventSink {
    fn push(&self, event: TaskEvent) {
        Self::push(self, event);
    }
}

impl EventSink {
    pub(crate) fn new(listener: Option<TaskEventListener>) -> Self {
        Self {
            events: Arc::new(Mutex::new(RetainedEvents::default())),
            listener,
        }
    }

    pub(crate) fn push(&self, event: TaskEvent) {
        // Live listeners receive every event; only retained diagnostics are bounded.
        if let Some(listener) = &self.listener {
            listener(&event);
        }
        let mut retained = self.events.lock().unwrap_or_else(|p| p.into_inner());
        if retained.events.len() == RETAINED_EVENTS - 1 {
            // Keep task start/policy context and the most recent diagnostics,
            // reserving one slot for an explicit truncation notice in snapshots.
            retained.events.remove(2);
            retained.dropped += 1;
        }
        retained.events.push_back(event);
    }

    pub(crate) fn snapshot(&self) -> Vec<TaskEvent> {
        let retained = self.events.lock().unwrap_or_else(|p| p.into_inner());
        let mut events: Vec<_> = retained.events.iter().cloned().collect();
        if retained.dropped > 0 {
            let task_id = events[0].task_id.clone();
            events.insert(2, TaskEvent { task_id, kind: TaskEventKind::Warning {
                message: format!("{} earlier diagnostic events omitted; per-file outcomes are tracked separately", retained.dropped),
            } });
        }
        events
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    #[test]
    fn non_progress_flood_is_bounded_and_terminal_and_live_events_survive() {
        let seen = Arc::new(std::sync::atomic::AtomicUsize::new(0));
        let count = seen.clone();
        let sink = EventSink::new(Some(Arc::new(move |_| {
            count.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
        })));
        let id = smartzip_core::TaskId::new();
        sink.push(TaskEvent::started(id.clone()));
        for _ in 0..10_000 {
            sink.push(TaskEvent {
                task_id: id.clone(),
                kind: TaskEventKind::PasswordTried { candidate_id: None },
            });
        }
        sink.push(TaskEvent {
            task_id: id,
            kind: TaskEventKind::Finished {
                status: "failed".into(),
            },
        });
        let events = sink.snapshot();
        assert_eq!(events.len(), RETAINED_EVENTS);
        assert!(matches!(events[0].kind, TaskEventKind::Started));
        assert!(matches!(events[2].kind, TaskEventKind::Warning { .. }));
        assert!(matches!(
            events.last().unwrap().kind,
            TaskEventKind::Finished { .. }
        ));
        assert_eq!(seen.load(std::sync::atomic::Ordering::Relaxed), 10_002);
    }
}
