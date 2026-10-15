//! A simple per-address login rate limit.
//!
//! Each login attempt can cost a Mojang lookup and a connection thread, so
//! unbounded attempts let a scanner drive outbound calls and park threads.
//! `max_connections` bounds *concurrency*, not *rate*; this bounds rate.
//!
//! It is deliberately small: a sliding one-minute window per address, with
//! eviction so it cannot grow without bound. It is a coarse anti-abuse gate,
//! not a precise token bucket.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// The window attempts are counted over.
pub const WINDOW: Duration = Duration::from_secs(60);
/// The most addresses tracked at once.
pub const CAP: usize = 4096;

/// Per-address login attempts in the last [`WINDOW`].
pub struct LoginLimiter {
    /// Attempts allowed per address per window; `0` disables the limit.
    max: u32,
    hits: Mutex<HashMap<IpAddr, Vec<Instant>>>,
}

impl LoginLimiter {
    /// A limiter allowing `max` attempts per address per minute (`0` = off).
    pub fn new(max: u32) -> Self {
        Self {
            max,
            hits: Mutex::new(HashMap::new()),
        }
    }

    /// Whether an attempt from `ip` is allowed, recording it if so.
    ///
    /// An unknown address (the socket had no peer) is always allowed: the
    /// limit is keyed by address, and inventing a shared bucket for "unknown"
    /// would let one such client lock everyone out.
    pub fn allow(&self, ip: Option<IpAddr>) -> bool {
        if self.max == 0 {
            return true;
        }
        let Some(ip) = ip else { return true };
        let mut map = self.hits.lock().unwrap();
        let now = Instant::now();
        let cutoff = now - WINDOW;
        let entry = map.entry(ip).or_default();
        entry.retain(|t| *t > cutoff);
        if entry.len() as u32 >= self.max {
            return false;
        }
        entry.push(now);
        // Bound the number of addresses tracked: drop the emptiest buckets
        // first, which are the addresses that stopped trying.
        if map.len() > CAP {
            map.retain(|_, v| v.iter().any(|t| *t > cutoff));
            while map.len() > CAP {
                if let Some(k) = map.iter().min_by_key(|(_, v)| v.len()).map(|(k, _)| *k) {
                    map.remove(&k);
                } else {
                    break;
                }
            }
        }
        true
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(n: u8) -> Option<IpAddr> {
        Some(IpAddr::from([10, 0, 0, n]))
    }

    #[test]
    fn the_limit_blocks_the_next_attempt_over_the_bound() {
        let limiter = LoginLimiter::new(3);
        assert!(limiter.allow(ip(1)));
        assert!(limiter.allow(ip(1)));
        assert!(limiter.allow(ip(1)));
        assert!(!limiter.allow(ip(1)), "the fourth in the window is refused");
        // A different address is unaffected.
        assert!(limiter.allow(ip(2)));
    }

    #[test]
    fn zero_disables_the_limit() {
        let limiter = LoginLimiter::new(0);
        for _ in 0..1000 {
            assert!(limiter.allow(ip(1)));
        }
    }

    #[test]
    fn an_unknown_address_is_always_allowed() {
        let limiter = LoginLimiter::new(1);
        assert!(limiter.allow(None));
        assert!(limiter.allow(None));
    }

    #[test]
    fn an_attempt_older_than_the_window_is_forgotten() {
        let limiter = LoginLimiter::new(1);
        assert!(limiter.allow(ip(1)));
        assert!(!limiter.allow(ip(1)));
        // Age the recorded hit past the window.
        limiter
            .hits
            .lock()
            .unwrap()
            .get_mut(&ip(1).unwrap())
            .unwrap()[0] = Instant::now() - WINDOW - Duration::from_secs(1);
        assert!(limiter.allow(ip(1)));
    }

    #[test]
    fn the_map_is_bounded() {
        let limiter = LoginLimiter::new(10);
        for i in 0..(CAP as u32 + 50) {
            let ip = IpAddr::from([10, 0, (i >> 8) as u8, i as u8]);
            limiter.allow(Some(ip));
        }
        assert!(limiter.hits.lock().unwrap().len() <= CAP);
    }
}
