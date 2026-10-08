//! Mirroring the journal into something you can run queries against.
//!
//! The journal's own storage answers the two questions the *server* asks —
//! "what happened in this column" and "what did this player do" — because
//! those are the two it keeps indexes for. It answers nothing else quickly,
//! and it is not a thing an operator can point a dashboard at.
//!
//! So events are also handed to an [`EventSink`], asynchronously and in
//! batches, for a real database to hold. Three rules shape the design:
//!
//! 1. **The game thread never waits on the database.** A submission is a push
//!    onto a queue and nothing more. A database that is slow, restarting or
//!    absent must not make a player's block placement slow.
//! 2. **The journal is the source of truth, the mirror is derived.** If the
//!    mirror falls behind or loses rows, nothing about the world is at risk
//!    and the gap can be refilled from the journal. That is what makes rule 1
//!    affordable.
//! 3. **A gap must be visible.** Dropping rows silently would turn the mirror
//!    into a thing you cannot trust and therefore cannot use, so drops are
//!    counted and the highest successfully-written sequence is tracked —
//!    together those say exactly what is missing.

use super::Event;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{sync_channel, SyncSender, TrySendError};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// Somewhere batches of events are written.
///
/// Implementations run on the sink's own thread, may block, and may fail.
/// A failure is retried, so `write` **must be idempotent**: the same batch can
/// arrive twice after an ambiguous failure, and the mirror has to survive that
/// without double-counting.
pub trait EventSink: Send + 'static {
    /// Write a batch. Returning `Err` schedules a retry of the same batch.
    fn write(&mut self, batch: &[Event]) -> Result<(), String>;

    /// A name for logs.
    fn describe(&self) -> String {
        "sink".to_string()
    }
}

/// How the batching thread is tuned.
#[derive(Debug, Clone, Copy)]
pub struct BatchConfig {
    /// Write as soon as this many events are waiting.
    pub max_batch: usize,
    /// ...or this long after the first one arrived, whichever comes first. A
    /// quiet server must not leave its last event unwritten indefinitely.
    pub max_delay: Duration,
    /// How many events may wait before submissions start being dropped.
    pub queue_depth: usize,
    /// How long to wait after a failed write before retrying. Doubles up to
    /// eight times this, so a database that is down for an hour is retried
    /// every few seconds rather than every few milliseconds.
    pub retry_delay: Duration,
}

impl Default for BatchConfig {
    fn default() -> Self {
        Self {
            max_batch: 500,
            max_delay: Duration::from_secs(1),
            queue_depth: 100_000,
            retry_delay: Duration::from_millis(250),
        }
    }
}

/// What the mirror has managed to do.
#[derive(Debug, Default)]
pub struct SinkStats {
    /// Events accepted onto the queue.
    pub queued: AtomicU64,
    /// Events refused because the queue was full. Non-zero means the mirror
    /// has a hole; the journal does not.
    pub dropped: AtomicU64,
    /// Events successfully written.
    pub written: AtomicU64,
    /// Batches that failed and were retried.
    pub retries: AtomicU64,
    /// Highest sequence number known to be in the mirror. With `dropped`, this
    /// is enough to say what needs refilling.
    pub high_water: AtomicU64,
}

impl SinkStats {
    /// A one-line summary for `/audit` and the console.
    pub fn summary(&self) -> String {
        format!(
            "queued {}, written {}, dropped {}, retries {}, up to #{}",
            self.queued.load(Ordering::Relaxed),
            self.written.load(Ordering::Relaxed),
            self.dropped.load(Ordering::Relaxed),
            self.retries.load(Ordering::Relaxed),
            self.high_water.load(Ordering::Relaxed),
        )
    }
}

/// A handle onto a background thread that batches events into an
/// [`EventSink`].
pub struct BatchingSink {
    tx: Option<SyncSender<Event>>,
    stats: Arc<SinkStats>,
    joiner: Option<std::thread::JoinHandle<()>>,
}

impl BatchingSink {
    /// Start the background thread.
    pub fn start<S: EventSink>(mut sink: S, cfg: BatchConfig) -> BatchingSink {
        let (tx, rx) = sync_channel::<Event>(cfg.queue_depth);
        let stats = Arc::new(SinkStats::default());
        let st = Arc::clone(&stats);
        let joiner = std::thread::Builder::new()
            .name("journal-sink".into())
            .spawn(move || {
                let mut batch: Vec<Event> = Vec::with_capacity(cfg.max_batch);
                let mut opened: Option<Instant> = None;
                loop {
                    // Wait only as long as the oldest waiting event still has
                    // before its deadline, so `max_delay` is a bound on
                    // latency and not merely a poll interval.
                    let wait = match opened {
                        Some(t) => cfg.max_delay.saturating_sub(t.elapsed()),
                        None => cfg.max_delay,
                    };
                    match rx.recv_timeout(wait) {
                        Ok(e) => {
                            if batch.is_empty() {
                                opened = Some(Instant::now());
                            }
                            batch.push(e);
                            if batch.len() < cfg.max_batch {
                                continue;
                            }
                        }
                        Err(std::sync::mpsc::RecvTimeoutError::Timeout) => {}
                        // Every sender is gone: flush what is left and stop.
                        Err(std::sync::mpsc::RecvTimeoutError::Disconnected) => {
                            if !batch.is_empty() {
                                flush(&mut sink, &batch, &st, cfg);
                            }
                            return;
                        }
                    }
                    if !batch.is_empty() {
                        flush(&mut sink, &batch, &st, cfg);
                        batch.clear();
                        opened = None;
                    }
                }
            })
            .expect("failed to start the journal sink thread");
        BatchingSink {
            tx: Some(tx),
            stats,
            joiner: Some(joiner),
        }
    }

    /// Hand one event to the mirror. Never blocks.
    pub fn submit(&self, e: &Event) {
        let Some(tx) = &self.tx else { return };
        match tx.try_send(e.clone()) {
            Ok(()) => {
                self.stats.queued.fetch_add(1, Ordering::Relaxed);
            }
            Err(TrySendError::Full(_)) | Err(TrySendError::Disconnected(_)) => {
                // Counted, never waited on. See rule 1 in the module docs.
                self.stats.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }

    /// What the mirror has managed to do.
    pub fn stats(&self) -> &SinkStats {
        &self.stats
    }
}

impl Drop for BatchingSink {
    /// Close the queue and wait for the last batch.
    ///
    /// Joining on drop is what makes a clean shutdown lose nothing. It can
    /// wait, which is acceptable here and nowhere else: this runs once, at
    /// exit, not on the tick.
    fn drop(&mut self) {
        self.tx.take();
        if let Some(j) = self.joiner.take() {
            let _ = j.join();
        }
    }
}

/// Write one batch, retrying with a doubling delay until it lands.
fn flush<S: EventSink>(sink: &mut S, batch: &[Event], stats: &SinkStats, cfg: BatchConfig) {
    let mut delay = cfg.retry_delay;
    for attempt in 0..8 {
        match sink.write(batch) {
            Ok(()) => {
                stats
                    .written
                    .fetch_add(batch.len() as u64, Ordering::Relaxed);
                if let Some(top) = batch.iter().map(|e| e.seq).max() {
                    stats.high_water.fetch_max(top, Ordering::Relaxed);
                }
                return;
            }
            Err(e) => {
                stats.retries.fetch_add(1, Ordering::Relaxed);
                eprintln!(
                    "[journal-sink] {} write failed (attempt {}): {e}",
                    sink.describe(),
                    attempt + 1
                );
                std::thread::sleep(delay);
                delay = (delay * 2).min(cfg.retry_delay * 8);
            }
        }
    }
    // Out of attempts. The batch is lost to the mirror only — the journal
    // still has every one of these events, and `dropped` says how many to
    // refill.
    stats
        .dropped
        .fetch_add(batch.len() as u64, Ordering::Relaxed);
}

/// A sink that records everything in memory. For tests, and for a dry run.
#[derive(Default, Clone)]
pub struct MemorySink {
    pub written: Arc<Mutex<Vec<Event>>>,
    /// Fail this many times before succeeding, to exercise the retry path.
    pub fail_first: Arc<AtomicU64>,
}

impl EventSink for MemorySink {
    fn write(&mut self, batch: &[Event]) -> Result<(), String> {
        if self.fail_first.load(Ordering::SeqCst) > 0 {
            self.fail_first.fetch_sub(1, Ordering::SeqCst);
            return Err("injected failure".into());
        }
        // Idempotent, as the trait requires: a retried batch must not double
        // up. Real sinks get this from a primary key on `seq`.
        let mut w = self.written.lock().unwrap();
        for e in batch {
            if !w.iter().any(|x| x.seq == e.seq) {
                w.push(e.clone());
            }
        }
        Ok(())
    }
    fn describe(&self) -> String {
        "memory".into()
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::journal::{ActorId, EventBody};
    use crate::BlockStateId;

    fn event(seq: u64) -> Event {
        Event {
            seq,
            at_ms: 1000 + seq,
            actor: ActorId(1),
            body: EventBody::BlockSet {
                x: seq as i32,
                y: 64,
                z: 0,
                from: BlockStateId::AIR,
                to: BlockStateId(1),
            },
        }
    }

    fn quick() -> BatchConfig {
        BatchConfig {
            max_batch: 10,
            max_delay: Duration::from_millis(50),
            queue_depth: 1000,
            retry_delay: Duration::from_millis(1),
        }
    }

    #[test]
    fn a_full_batch_is_written_without_waiting_for_the_timer() {
        let mem = MemorySink::default();
        let seen = Arc::clone(&mem.written);
        let sink = BatchingSink::start(mem, quick());
        for i in 1..=10 {
            sink.submit(&event(i));
        }
        drop(sink); // joins
        assert_eq!(seen.lock().unwrap().len(), 10);
    }

    #[test]
    fn a_partial_batch_is_written_when_the_delay_expires() {
        // Without this a quiet server would hold its last few events forever,
        // and the mirror would always be one incident behind.
        let mem = MemorySink::default();
        let seen = Arc::clone(&mem.written);
        let sink = BatchingSink::start(mem, quick());
        sink.submit(&event(1));
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(seen.lock().unwrap().len(), 1, "flushed on the timer");
        drop(sink);
    }

    #[test]
    fn everything_queued_before_shutdown_is_written() {
        let mem = MemorySink::default();
        let seen = Arc::clone(&mem.written);
        let sink = BatchingSink::start(mem, quick());
        for i in 1..=137 {
            sink.submit(&event(i));
        }
        drop(sink); // must flush the trailing partial batch
        let w = seen.lock().unwrap();
        assert_eq!(w.len(), 137);
        let mut seqs: Vec<u64> = w.iter().map(|e| e.seq).collect();
        seqs.sort_unstable();
        assert_eq!(seqs, (1..=137).collect::<Vec<_>>());
    }

    #[test]
    fn a_failing_write_is_retried_and_the_batch_is_not_lost() {
        let mem = MemorySink::default();
        mem.fail_first.store(3, Ordering::SeqCst);
        let seen = Arc::clone(&mem.written);
        let stats_holder = BatchingSink::start(mem, quick());
        for i in 1..=10 {
            stats_holder.submit(&event(i));
        }
        std::thread::sleep(Duration::from_millis(300));
        assert!(stats_holder.stats().retries.load(Ordering::Relaxed) >= 3);
        drop(stats_holder);
        assert_eq!(seen.lock().unwrap().len(), 10, "the batch still landed");
    }

    #[test]
    fn a_retried_batch_is_not_written_twice() {
        // The trait requires idempotence and this is what it buys: an
        // ambiguous failure must not double every row in the mirror.
        let mut mem = MemorySink::default();
        let batch: Vec<Event> = (1..=5).map(event).collect();
        mem.write(&batch).unwrap();
        mem.write(&batch).unwrap();
        assert_eq!(mem.written.lock().unwrap().len(), 5);
    }

    #[test]
    fn a_full_queue_drops_and_counts_instead_of_blocking() {
        // The property that matters is that `submit` returns promptly even
        // when the sink cannot keep up — a block placement must never wait on
        // a database.
        struct Slow;
        impl EventSink for Slow {
            fn write(&mut self, _: &[Event]) -> Result<(), String> {
                std::thread::sleep(Duration::from_millis(50));
                Ok(())
            }
        }
        let cfg = BatchConfig {
            max_batch: 1,
            max_delay: Duration::from_millis(5),
            queue_depth: 2,
            retry_delay: Duration::from_millis(1),
        };
        let sink = BatchingSink::start(Slow, cfg);
        let start = Instant::now();
        for i in 1..=500 {
            sink.submit(&event(i));
        }
        assert!(
            start.elapsed() < Duration::from_secs(2),
            "submit blocked: took {:?}",
            start.elapsed()
        );
        assert!(
            sink.stats().dropped.load(Ordering::Relaxed) > 0,
            "a full queue must drop visibly, not silently succeed"
        );
    }

    #[test]
    fn the_high_water_mark_tracks_what_actually_landed() {
        let mem = MemorySink::default();
        let sink = BatchingSink::start(mem, quick());
        for i in 1..=10 {
            sink.submit(&event(i));
        }
        std::thread::sleep(Duration::from_millis(200));
        assert_eq!(sink.stats().high_water.load(Ordering::Relaxed), 10);
        drop(sink);
    }
}
