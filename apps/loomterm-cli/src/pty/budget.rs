//! Admission remains owned until the last queued or active operation is dropped.
use std::sync::{Arc, Mutex};

#[derive(Debug)]
pub(crate) struct Budget {
    limit: (usize, usize),
    used: Mutex<(usize, usize)>,
}

impl Budget {
    pub(crate) fn new(count: usize, bytes: usize) -> Arc<Self> {
        Arc::new(Self {
            limit: (count, bytes),
            used: Mutex::new((0, 0)),
        })
    }

    pub(crate) fn acquire(self: &Arc<Self>, bytes: usize) -> Option<Permit> {
        let mut used = self.used.lock().unwrap_or_else(|p| p.into_inner());
        let next = used.1.checked_add(bytes)?;
        if used.0 >= self.limit.0 || next > self.limit.1 {
            return None;
        }
        used.0 += 1;
        used.1 = next;
        Some(Permit {
            budget: self.clone(),
            bytes,
        })
    }
}

#[derive(Debug)]
pub(crate) struct Permit {
    budget: Arc<Budget>,
    bytes: usize,
}

impl Permit {
    pub(crate) fn shrink_to(&mut self, bytes: usize) {
        assert!(
            bytes <= self.bytes,
            "admission cannot grow after allocation"
        );
        let mut used = self.budget.used.lock().unwrap_or_else(|p| p.into_inner());
        used.1 -= self.bytes - bytes;
        self.bytes = bytes;
    }
}

impl Drop for Permit {
    fn drop(&mut self) {
        let mut used = self.budget.used.lock().unwrap_or_else(|p| p.into_inner());
        used.0 -= 1;
        used.1 -= self.bytes;
    }
}

pub(crate) fn input_budget() -> Arc<Budget> {
    Budget::new(128, 2 * 1024 * 1024)
}
pub(crate) fn reply_budget() -> Arc<Budget> {
    Budget::new(64, 256 * 1024)
}

#[derive(Clone, Default)]
pub(crate) struct IoFailure(Arc<Mutex<Option<String>>>);

impl IoFailure {
    pub(crate) fn fail(&self, reason: impl Into<String>) {
        self.0
            .lock()
            .unwrap_or_else(|p| p.into_inner())
            .get_or_insert_with(|| reason.into());
    }
    pub(crate) fn reason(&self) -> Option<String> {
        self.0.lock().unwrap_or_else(|p| p.into_inner()).clone()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn admission_counts_active_work_and_recovers_on_drop() {
        let budget = Budget::new(2, 8);
        let first = budget.acquire(5).unwrap();
        assert!(budget.acquire(4).is_none());
        let second = budget.acquire(3).unwrap();
        assert!(budget.acquire(0).is_none());
        drop(first);
        assert!(budget.acquire(5).is_some());
        drop(second);
        assert_eq!(*budget.used.lock().unwrap(), (0, 0));
        assert!(budget.acquire(usize::MAX).is_none());
    }

    #[test]
    fn input_pressure_cannot_consume_reply_reserve() {
        let input = input_budget();
        let _input = input.acquire(2 * 1024 * 1024).unwrap();
        assert!(input.acquire(1).is_none());
        assert!(reply_budget().acquire(64 * 1024).is_some());
        let failure = IoFailure::default();
        failure.fail("first");
        failure.fail("second");
        assert_eq!(failure.reason().as_deref(), Some("first"));
    }
}
