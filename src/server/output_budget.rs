//! PTY readers retain their byte reservation through the journal write.
//! Slow storage therefore slows the producing pane, never the event loop.

use std::sync::{Arc, Condvar, Mutex};

const OUTPUT_BYTES: usize = 2 * 1024 * 1024;

#[derive(Clone, Default)]
pub(super) struct OutputBudget(Arc<(Mutex<usize>, Condvar)>);

pub(super) struct OutputPermit {
    budget: OutputBudget,
    bytes: usize,
}

impl OutputBudget {
    /// Called only by a pane's PTY reader, whose chunks are at most 32 KiB.
    pub(super) fn acquire(&self, bytes: usize) -> OutputPermit {
        assert!(bytes <= OUTPUT_BYTES);
        let (used, available) = &*self.0;
        let mut used = used.lock().unwrap();
        while *used + bytes > OUTPUT_BYTES {
            used = available.wait(used).unwrap();
        }
        *used += bytes;
        OutputPermit {
            budget: self.clone(),
            bytes,
        }
    }
}

impl Drop for OutputPermit {
    fn drop(&mut self) {
        let (used, available) = &*self.budget.0;
        *used.lock().unwrap() -= self.bytes;
        available.notify_one();
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::{sync::mpsc, thread, time::Duration};

    #[test]
    fn budget_is_released_when_the_last_pipeline_stage_drops_the_permit() {
        let budget = OutputBudget::default();
        let held = budget.acquire(OUTPUT_BYTES);
        let (acquired, ready) = mpsc::channel();
        let worker = thread::spawn(move || {
            let _permit = budget.acquire(1);
            acquired.send(()).unwrap();
        });
        assert!(ready.recv_timeout(Duration::from_millis(50)).is_err());
        drop(held);
        ready.recv_timeout(Duration::from_secs(2)).unwrap();
        worker.join().unwrap();
    }
}
