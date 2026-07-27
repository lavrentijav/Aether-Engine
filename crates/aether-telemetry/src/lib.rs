//! # aether-telemetry
//!
//! Minimal, dependency-free telemetry primitives wired into the (still empty)
//! main loop during Phase 0:
//!
//! * atomic [`Counter`] and [`Gauge`] metrics registered in a [`Registry`],
//! * a scoped [`Timer`] / [`span`] for measuring tick-phase durations,
//! * a [`Registry::render_prometheus`] exporter that emits the standard
//!   text exposition format a Prometheus scrape or Grafana agent can read.
//!
//! The Tracy zone hooks live behind the `tracy` feature; until that client is
//! wired in they compile to no-ops so instrumentation call-sites can be added
//! now and lit up later.

use std::collections::BTreeMap;
use std::sync::atomic::{AtomicI64, AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Instant;

/// A monotonically increasing counter (e.g. ticks processed, chunks saved).
#[derive(Debug, Default)]
pub struct Counter(AtomicU64);

impl Counter {
    /// Add `n` to the counter.
    #[inline]
    pub fn add(&self, n: u64) {
        self.0.fetch_add(n, Ordering::Relaxed);
    }
    /// Increment by one.
    #[inline]
    pub fn inc(&self) {
        self.add(1);
    }
    /// Current value.
    #[inline]
    pub fn get(&self) -> u64 {
        self.0.load(Ordering::Relaxed)
    }
}

/// A value that can go up or down (e.g. loaded chunks, entities alive).
#[derive(Debug, Default)]
pub struct Gauge(AtomicI64);

impl Gauge {
    /// Set the gauge to `v`.
    #[inline]
    pub fn set(&self, v: i64) {
        self.0.store(v, Ordering::Relaxed);
    }
    /// Add `delta` (may be negative).
    #[inline]
    pub fn add(&self, delta: i64) {
        self.0.fetch_add(delta, Ordering::Relaxed);
    }
    /// Current value.
    #[inline]
    pub fn get(&self) -> i64 {
        self.0.load(Ordering::Relaxed)
    }
}

enum Metric {
    Counter(Arc<Counter>),
    Gauge(Arc<Gauge>),
}

/// A named collection of metrics that can be exported for Prometheus.
///
/// Cheap to clone (`Arc` inside); share one registry across subsystems.
#[derive(Clone, Default)]
pub struct Registry {
    inner: Arc<Mutex<BTreeMap<String, Metric>>>,
}

impl Registry {
    /// Create an empty registry.
    pub fn new() -> Self {
        Self::default()
    }

    /// Get or create a counter named `name`.
    pub fn counter(&self, name: &str) -> Arc<Counter> {
        let mut map = self.inner.lock().expect("telemetry registry poisoned");
        match map.get(name) {
            Some(Metric::Counter(c)) => Arc::clone(c),
            Some(Metric::Gauge(_)) => panic!("metric `{name}` already registered as a gauge"),
            None => {
                let c = Arc::new(Counter::default());
                map.insert(name.to_owned(), Metric::Counter(Arc::clone(&c)));
                c
            }
        }
    }

    /// Get or create a gauge named `name`.
    pub fn gauge(&self, name: &str) -> Arc<Gauge> {
        let mut map = self.inner.lock().expect("telemetry registry poisoned");
        match map.get(name) {
            Some(Metric::Gauge(g)) => Arc::clone(g),
            Some(Metric::Counter(_)) => panic!("metric `{name}` already registered as a counter"),
            None => {
                let g = Arc::new(Gauge::default());
                map.insert(name.to_owned(), Metric::Gauge(Arc::clone(&g)));
                g
            }
        }
    }

    /// Render every metric in the Prometheus text exposition format.
    ///
    /// Metric names are emitted in sorted order for deterministic output.
    pub fn render_prometheus(&self) -> String {
        let map = self.inner.lock().expect("telemetry registry poisoned");
        let mut out = String::new();
        for (name, metric) in map.iter() {
            match metric {
                Metric::Counter(c) => {
                    out.push_str(&format!("# TYPE {name} counter\n{name} {}\n", c.get()));
                }
                Metric::Gauge(g) => {
                    out.push_str(&format!("# TYPE {name} gauge\n{name} {}\n", g.get()));
                }
            }
        }
        out
    }
}

/// A scoped stopwatch. Dropping it (or calling [`Timer::stop`]) reports the
/// elapsed nanoseconds to the provided sink.
///
/// Prefer the [`span!`](crate::span) macro at call sites.
pub struct Timer<F: FnMut(u64)> {
    start: Instant,
    sink: Option<F>,
}

impl<F: FnMut(u64)> Timer<F> {
    /// Start a timer that reports elapsed nanoseconds to `sink` on drop.
    pub fn new(sink: F) -> Self {
        Self {
            start: Instant::now(),
            sink: Some(sink),
        }
    }
    /// Nanoseconds elapsed so far.
    pub fn elapsed_ns(&self) -> u64 {
        self.start.elapsed().as_nanos() as u64
    }
    /// Stop early and report now (idempotent-ish: further drop is a no-op).
    pub fn stop(mut self) {
        if let Some(mut sink) = self.sink.take() {
            sink(self.start.elapsed().as_nanos() as u64);
        }
    }
}

impl<F: FnMut(u64)> Drop for Timer<F> {
    fn drop(&mut self) {
        if let Some(sink) = self.sink.as_mut() {
            sink(self.start.elapsed().as_nanos() as u64);
        }
    }
}

/// Emit a Tracy zone for the current scope.
///
/// A no-op until the `tracy` feature wires in the real client; call sites can
/// be added now.
#[macro_export]
macro_rules! zone {
    ($name:expr) => {{
        #[cfg(feature = "tracy")]
        {
            // Real Tracy client integration lands with the profiler work.
            let _ = $name;
        }
        #[cfg(not(feature = "tracy"))]
        {
            let _ = $name;
        }
    }};
}

/// Time the enclosing block, adding the elapsed nanoseconds to `counter`.
///
/// ```
/// use aether_telemetry::Registry;
/// let reg = Registry::new();
/// let ns = reg.counter("phase_physics_ns");
/// let _t = aether_telemetry::span!(ns);
/// // ... work ...
/// ```
#[macro_export]
macro_rules! span {
    ($counter:expr) => {{
        let c = ::std::sync::Arc::clone(&$counter);
        $crate::Timer::new(move |ns| c.add(ns))
    }};
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn counters_and_gauges_track_values() {
        let reg = Registry::new();
        let ticks = reg.counter("ticks_total");
        ticks.inc();
        ticks.add(4);
        assert_eq!(ticks.get(), 5);

        let loaded = reg.gauge("chunks_loaded");
        loaded.set(10);
        loaded.add(-3);
        assert_eq!(loaded.get(), 7);
    }

    #[test]
    fn get_or_create_is_idempotent() {
        let reg = Registry::new();
        reg.counter("c").add(2);
        reg.counter("c").add(3);
        assert_eq!(reg.counter("c").get(), 5);
    }

    #[test]
    fn prometheus_output_is_sorted_and_typed() {
        let reg = Registry::new();
        reg.counter("b_total").add(2);
        reg.gauge("a_gauge").set(-1);
        let text = reg.render_prometheus();
        assert_eq!(
            text,
            "# TYPE a_gauge gauge\na_gauge -1\n# TYPE b_total counter\nb_total 2\n"
        );
    }

    #[test]
    fn span_accumulates_time() {
        let reg = Registry::new();
        let ns = reg.counter("work_ns");
        {
            let _t = span!(ns);
            std::hint::black_box((0..1000).sum::<u64>());
        }
        assert!(ns.get() > 0, "span recorded no elapsed time");
    }

    #[test]
    #[should_panic(expected = "already registered as a gauge")]
    fn type_conflict_panics() {
        let reg = Registry::new();
        reg.gauge("x");
        reg.counter("x");
    }
}
