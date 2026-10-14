//! A queue of scheduled block updates, shared by the systems that need one.
//!
//! Vanilla resolves much block behaviour not the instant a block changes but a
//! few ticks later: a fluid flows, a leaf recomputes its `distance`, sand
//! notices it is unsupported. Each of those systems wants the same shape of
//! queue — a position, a tick to run at, and deduplication so a cell is only
//! pending once — but a *different* one: the same position may be scheduled by
//! two systems at different delays, and the shorter delay must not be swallowed
//! by the longer. So the queue is a type here and each system keeps its own.
//!
//! This is the scheduler [`super::fluids`] used to hold inline.

use std::collections::{BTreeMap, HashSet};

/// Positions due for an update, keyed by the tick they are due at.
pub struct Scheduler<T> {
    due: BTreeMap<u64, Vec<T>>,
    pending: HashSet<T>,
}

impl<T> Default for Scheduler<T> {
    fn default() -> Self {
        Self {
            due: BTreeMap::new(),
            pending: HashSet::new(),
        }
    }
}

impl<T: Copy + Eq + std::hash::Hash> Scheduler<T> {
    /// A fresh, empty queue.
    pub fn new() -> Self {
        Self::default()
    }

    /// Schedule `key` to run at tick `at`, unless it is already pending.
    ///
    /// The first schedule wins: a key already in the queue keeps the tick it
    /// was given, so a flurry of changes in one tick does not keep pushing an
    /// update further into the future.
    pub fn schedule(&mut self, key: T, at: u64) {
        if self.pending.insert(key) {
            self.due.entry(at).or_default().push(key);
        }
    }

    /// Every pending key with the tick it is due at, in due-tick order.
    ///
    /// This is the state that must survive a restart (see
    /// [`super::blockticks::save`]): the keys not yet taken by [`Self::due`],
    /// each with the tick it was scheduled for.
    pub fn entries(&self) -> Vec<(u64, T)> {
        self.due
            .iter()
            .flat_map(|(&at, list)| list.iter().map(move |&k| (at, k)))
            .collect()
    }

    /// Take up to `budget` keys due at or before `now`, oldest first.
    ///
    /// What does not fit stays queued for the next call, so a flood of due
    /// updates is spread over ticks rather than run in one.
    pub fn due(&mut self, now: u64, budget: usize) -> Vec<T> {
        let mut batch = Vec::new();
        while batch.len() < budget {
            let Some((&t, _)) = self.due.iter().next() else {
                break;
            };
            if t > now {
                break;
            }
            let mut list = self.due.remove(&t).unwrap_or_default();
            let room = budget - batch.len();
            if list.len() > room {
                let rest = list.split_off(room);
                self.due.insert(t, rest);
            }
            for k in &list {
                self.pending.remove(k);
            }
            batch.extend(list);
        }
        batch
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_update_runs_at_its_tick_and_not_before() {
        let mut q = Scheduler::new();
        q.schedule(1u32, 10);
        assert!(q.due(9, 16).is_empty(), "not due yet");
        assert_eq!(q.due(10, 16), vec![1]);
    }

    #[test]
    fn a_key_is_only_pending_once() {
        let mut q = Scheduler::new();
        q.schedule(1u32, 10);
        q.schedule(1u32, 20); // ignored: already pending
        assert_eq!(q.due(10, 16), vec![1]);
        assert!(q.due(20, 16).is_empty());
    }

    #[test]
    fn a_budget_defers_the_rest_to_the_next_call() {
        let mut q = Scheduler::new();
        for i in 0..5u32 {
            q.schedule(i, 0);
        }
        assert_eq!(q.due(0, 2).len(), 2);
        assert_eq!(q.due(0, 2).len(), 2);
        assert_eq!(q.due(0, 2).len(), 1);
        assert!(q.due(0, 2).is_empty());
    }
}
