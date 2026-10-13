//! Admission control for TCP connections.
//!
//! The accept loop spawns one OS thread per socket, so without a cap anyone
//! can open thousands of idle connections — each parked until the login read
//! timeout — and exhaust threads and memory. [`Limiter`] keeps a global count
//! and a per-address count, and hands out a [`Guard`] that gives the count
//! back on drop, so every way a connection can end releases its slot without
//! the call sites having to remember to.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

/// The shortest gap between two "refused" warnings, so a flood cannot fill the
/// log.
const WARN_INTERVAL: Duration = Duration::from_secs(1);

/// Connection admission control, shared across the accept loop with an [`Arc`].
pub struct Limiter {
    /// Total concurrent connections allowed. `0` disables the total cap.
    max_total: usize,
    /// Concurrent connections allowed from one address. `0` disables it.
    max_per_ip: usize,
    /// Live connections, all addresses together.
    total: AtomicUsize,
    /// Live connections per address. An entry is removed when it falls to zero
    /// so that a scan from many source addresses cannot grow this map without
    /// bound.
    per_ip: Mutex<HashMap<IpAddr, usize>>,
    /// When the last refusal was logged, for [`Self::warn_refused`].
    last_warn: Mutex<Option<Instant>>,
}

impl Limiter {
    /// A limiter allowing `max_total` connections in total (`0` = no total
    /// cap) and `max_per_ip` per address (`0` = no per-address cap).
    pub fn new(max_total: usize, max_per_ip: usize) -> Arc<Self> {
        Arc::new(Self {
            max_total,
            max_per_ip,
            total: AtomicUsize::new(0),
            per_ip: Mutex::new(HashMap::new()),
            last_warn: Mutex::new(None),
        })
    }

    /// Reserve a connection slot for `ip`, or return `None` when the total or
    /// per-address cap is already reached. The returned [`Guard`] releases the
    /// slot when it is dropped.
    pub fn try_acquire(self: &Arc<Self>, ip: Option<IpAddr>) -> Option<Guard> {
        // Admission is serialised on `per_ip` so both counters are checked and
        // raised together: two threads racing for the last slot must not both
        // be admitted. A `None` address (a failed `peer_addr`) is not tracked
        // per address, but still counts towards the total.
        let mut per_ip = self.per_ip.lock().unwrap();
        if self.max_total != 0 && self.total.load(Ordering::Relaxed) >= self.max_total {
            return None;
        }
        if let Some(ip) = ip {
            let used = per_ip.get(&ip).copied().unwrap_or(0);
            if self.max_per_ip != 0 && used >= self.max_per_ip {
                return None;
            }
            *per_ip.entry(ip).or_insert(0) += 1;
        }
        self.total.fetch_add(1, Ordering::Relaxed);
        Some(Guard {
            limiter: Arc::clone(self),
            ip,
        })
    }

    /// Log a refused connection, at most once per [`WARN_INTERVAL`].
    pub fn warn_refused(&self, ip: Option<IpAddr>) {
        let mut last = self.last_warn.lock().unwrap();
        let now = Instant::now();
        if last.map_or(true, |t| now.duration_since(t) >= WARN_INTERVAL) {
            *last = Some(now);
            crate::log::warn(&format!(
                "refused connection from {}: too many connections",
                ip.map(|i| i.to_string()).unwrap_or_default()
            ));
        }
    }
}

/// A held connection slot. Dropping it releases the slot, so the end of a
/// connection thread, an early return and a panic all give it back the same
/// way.
pub struct Guard {
    limiter: Arc<Limiter>,
    ip: Option<IpAddr>,
}

impl Drop for Guard {
    fn drop(&mut self) {
        self.limiter.total.fetch_sub(1, Ordering::Relaxed);
        let Some(ip) = self.ip else {
            return;
        };
        let mut per_ip = self.limiter.per_ip.lock().unwrap();
        if let Some(used) = per_ip.get_mut(&ip) {
            *used -= 1;
            if *used == 0 {
                per_ip.remove(&ip);
            }
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> IpAddr {
        s.parse().unwrap()
    }

    #[test]
    fn a_guard_takes_a_slot_and_gives_it_back_on_drop() {
        let limiter = Limiter::new(4, 0);
        assert_eq!(limiter.total.load(Ordering::Relaxed), 0);
        let a = limiter.try_acquire(Some(ip("10.0.0.1"))).unwrap();
        assert_eq!(limiter.total.load(Ordering::Relaxed), 1);
        let b = limiter.try_acquire(Some(ip("10.0.0.1"))).unwrap();
        assert_eq!(limiter.total.load(Ordering::Relaxed), 2);
        drop(a);
        assert_eq!(limiter.total.load(Ordering::Relaxed), 1);
        drop(b);
        assert_eq!(limiter.total.load(Ordering::Relaxed), 0);
        assert!(
            limiter.per_ip.lock().unwrap().is_empty(),
            "an address's entry is swept once it falls to zero"
        );
    }

    #[test]
    fn an_early_return_still_releases_the_slot() {
        // A guard taken and then abandoned by an early return must release on
        // the way out, exactly like one dropped at the end of the scope.
        fn dial(limiter: &Arc<Limiter>) -> Option<()> {
            let _guard = limiter.try_acquire(None)?;
            if limiter.total.load(Ordering::Relaxed) == 1 {
                return None; // fails while still holding the slot
            }
            Some(())
        }
        let limiter = Limiter::new(1, 0);
        assert!(dial(&limiter).is_none());
        assert_eq!(
            limiter.total.load(Ordering::Relaxed),
            0,
            "the slot came back on the early return"
        );
        assert!(
            limiter.try_acquire(None).is_some(),
            "and the slot can be taken again"
        );
    }

    #[test]
    fn the_total_cap_refuses_once_full_and_zero_disables_it() {
        let limiter = Limiter::new(2, 0);
        let _first = limiter.try_acquire(None).unwrap();
        let _second = limiter.try_acquire(None).unwrap();
        assert!(
            limiter.try_acquire(None).is_none(),
            "the third is over the cap of two"
        );

        let unlimited = Limiter::new(0, 0);
        for _ in 0..1000 {
            assert!(unlimited.try_acquire(None).is_some());
        }
    }

    #[test]
    fn the_per_ip_cap_refuses_only_the_address_that_filled_it() {
        let limiter = Limiter::new(0, 2);
        let a = ip("10.0.0.1");
        let b = ip("10.0.0.2");
        let _first = limiter.try_acquire(Some(a)).unwrap();
        let _second = limiter.try_acquire(Some(a)).unwrap();
        assert!(
            limiter.try_acquire(Some(a)).is_none(),
            "the third from one address is over its cap of two"
        );
        assert!(
            limiter.try_acquire(Some(b)).is_some(),
            "another address is unaffected by the first address's flood"
        );
    }
}
