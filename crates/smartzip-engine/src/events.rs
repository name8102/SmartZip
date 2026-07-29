//! Task event sink and listener types.

use smartzip_core::{TaskEvent, TaskEventSink};
use std::sync::{Arc, Mutex};

pub type TaskEventListener = Arc<dyn Fn(&TaskEvent) + Send + Sync>;

#[derive(Clone)]
pub(crate) struct EventSink {
    events: Arc<Mutex<Vec<TaskEvent>>>,
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
            events: Arc::new(Mutex::new(Vec::new())),
            listener,
        }
    }

    pub(crate) fn push(&self, event: TaskEvent) {
        if let Some(listener) = &self.listener {
            listener(&event);
        }
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .push(event);
    }

    pub(crate) fn snapshot(&self) -> Vec<TaskEvent> {
        self.events
            .lock()
            .unwrap_or_else(|poisoned| poisoned.into_inner())
            .clone()
    }
}
