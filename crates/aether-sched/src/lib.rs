//! # aether-sched
//!
//! A small background scheduler for the "scalability without global locks"
//! principle (spec §5.4): the heavy, tick-independent work — chunk generation,
//! async saves, lighting — is fanned out across a worker pool instead of
//! running on the main tick thread.
//!
//! This is a **dynamic shared-queue** executor: workers pull the next job from a
//! single atomic cursor, so a worker that finishes early immediately grabs more
//! instead of idling behind a slow neighbour (greedy load balancing — the
//! practical benefit of work-stealing, without per-worker deques; true
//! per-worker stealing deques are a later refinement).
//!
//! Jobs in a batch must be **independent** — the scheduler exploits exactly the
//! independence the engine's Safe-Point model already requires, so a parallel
//! [`Scheduler::map`] returns the *same* results as a sequential one, just
//! faster. Output order always matches input order.
//!
//! ```
//! use aether_sched::Scheduler;
//!
//! let pool = Scheduler::new(4);
//! let squares = pool.map(&[1, 2, 3, 4, 5], |&n| n * n);
//! assert_eq!(squares, vec![1, 4, 9, 16, 25]);
//! ```

use std::sync::atomic::{AtomicUsize, Ordering};

/// A fixed-size worker pool that runs independent job batches in parallel.
///
/// The pool is stateless between calls: each [`Scheduler::map`] /
/// [`Scheduler::for_each`] spins up scoped threads for that batch and joins them
/// before returning, so there are no long-lived threads to manage and results
/// can borrow freely from the caller's stack.
#[derive(Debug, Clone, Copy)]
pub struct Scheduler {
    workers: usize,
}

impl Scheduler {
    /// A pool with `workers` worker threads (clamped to at least 1).
    pub fn new(workers: usize) -> Self {
        Self {
            workers: workers.max(1),
        }
    }

    /// A pool sized to the machine's available parallelism (falling back to 1).
    pub fn with_available_parallelism() -> Self {
        let n = std::thread::available_parallelism()
            .map(|n| n.get())
            .unwrap_or(1);
        Self::new(n)
    }

    /// Number of worker threads.
    #[inline]
    pub fn workers(&self) -> usize {
        self.workers
    }

    /// Map `f` over `items` in parallel, returning the results **in input
    /// order**.
    ///
    /// Falls back to a plain sequential map for a single worker or a batch small
    /// enough that spawning threads would not pay off.
    pub fn map<T, R, F>(&self, items: &[T], f: F) -> Vec<R>
    where
        T: Sync,
        R: Send,
        F: Fn(&T) -> R + Sync,
    {
        let n = items.len();
        if self.workers == 1 || n <= 1 {
            return items.iter().map(&f).collect();
        }

        let cursor = AtomicUsize::new(0);
        let threads = self.workers.min(n);
        let f = &f;

        // Each worker drains the shared cursor and keeps (index, result) pairs;
        // after the join we scatter them back into input order. No unsafe, no
        // shared mutable output.
        let partials: Vec<Vec<(usize, R)>> = std::thread::scope(|scope| {
            let handles: Vec<_> = (0..threads)
                .map(|_| {
                    let cursor = &cursor;
                    scope.spawn(move || {
                        let mut local: Vec<(usize, R)> = Vec::new();
                        loop {
                            let i = cursor.fetch_add(1, Ordering::Relaxed);
                            if i >= n {
                                break;
                            }
                            local.push((i, f(&items[i])));
                        }
                        local
                    })
                })
                .collect();
            handles.into_iter().map(|h| h.join().unwrap()).collect()
        });

        // Reassemble in input order.
        let mut out: Vec<Option<R>> = (0..n).map(|_| None).collect();
        for chunk in partials {
            for (i, r) in chunk {
                out[i] = Some(r);
            }
        }
        out.into_iter()
            .map(|slot| slot.expect("every index filled"))
            .collect()
    }

    /// Run `f` on every item in parallel for its side effects (no results).
    pub fn for_each<T, F>(&self, items: &[T], f: F)
    where
        T: Sync,
        F: Fn(&T) + Sync,
    {
        let n = items.len();
        if self.workers == 1 || n <= 1 {
            items.iter().for_each(&f);
            return;
        }
        let cursor = AtomicUsize::new(0);
        let threads = self.workers.min(n);
        let f = &f;
        std::thread::scope(|scope| {
            for _ in 0..threads {
                let cursor = &cursor;
                scope.spawn(move || loop {
                    let i = cursor.fetch_add(1, Ordering::Relaxed);
                    if i >= n {
                        break;
                    }
                    f(&items[i]);
                });
            }
        });
    }
}

impl Default for Scheduler {
    fn default() -> Self {
        Self::with_available_parallelism()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use std::sync::atomic::AtomicU64;

    #[test]
    fn map_preserves_input_order() {
        let pool = Scheduler::new(4);
        let out = pool.map(&[0, 1, 2, 3, 4, 5, 6, 7, 8, 9], |&n| n * 10);
        assert_eq!(out, vec![0, 10, 20, 30, 40, 50, 60, 70, 80, 90]);
    }

    #[test]
    fn map_matches_sequential_on_many_items() {
        let items: Vec<u64> = (0..10_000).collect();
        let seq: Vec<u64> = items.iter().map(|&n| n * n + 1).collect();
        let par = Scheduler::new(8).map(&items, |&n| n * n + 1);
        assert_eq!(par, seq);
    }

    #[test]
    fn single_worker_is_sequential_fallback() {
        let pool = Scheduler::new(1);
        assert_eq!(pool.workers(), 1);
        assert_eq!(pool.map(&[1, 2, 3], |&n| n + 1), vec![2, 3, 4]);
    }

    #[test]
    fn empty_and_singleton_batches() {
        let pool = Scheduler::new(4);
        let empty: Vec<i32> = pool.map::<i32, i32, _>(&[], |&n| n);
        assert!(empty.is_empty());
        assert_eq!(pool.map(&[42], |&n| n), vec![42]);
    }

    #[test]
    fn for_each_visits_every_item_once() {
        let counter = AtomicU64::new(0);
        let items: Vec<u64> = (0..5_000).collect();
        Scheduler::new(8).for_each(&items, |&n| {
            counter.fetch_add(n, Ordering::Relaxed);
        });
        let expected: u64 = (0..5_000).sum();
        assert_eq!(counter.load(Ordering::Relaxed), expected);
    }

    #[test]
    fn workers_clamped_to_at_least_one() {
        assert_eq!(Scheduler::new(0).workers(), 1);
        assert!(Scheduler::with_available_parallelism().workers() >= 1);
    }

    #[test]
    fn uneven_workloads_still_complete() {
        // Jobs with wildly different costs must all finish and stay ordered —
        // the point of the dynamic cursor.
        let items: Vec<u64> = (0..200).collect();
        let out = Scheduler::new(4).map(&items, |&n| {
            // Simulate uneven work by summing a variable-length range.
            (0..=n).sum::<u64>()
        });
        for (i, &v) in out.iter().enumerate() {
            let n = i as u64;
            assert_eq!(v, n * (n + 1) / 2);
        }
    }
}
