//! "Bounced" online-auth attempts: a client that dropped during the
//! encryption handshake.
//!
//! A cracked or offline client that is offered encryption but has no Mojang
//! session does one of two things:
//!
//! * answers the Encryption Request and then fails `hasJoined` — a modified
//!   client; or
//! * disconnects on the spot, which is what the vanilla client does when its
//!   session is invalid.
//!
//! The second case is indistinguishable from a network blip, so the next
//! login attempt from the **same address and name** within [`TTL`] is
//! admitted offline with no encryption offer, rather than being bounced
//! again. This is the hybrid spec's step 2, third bullet.
//!
//! The map is bounded so a scanner cycling names cannot grow it without
//! bound, and entries expire on their own after [`TTL`].

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a drop suppresses the next encryption offer.
pub const TTL: Duration = Duration::from_secs(300);
/// The most entries kept at once.
pub const CAP: usize = 4096;

/// Recent drops, keyed by `(address, name_lower)`.
#[derive(Default)]
pub struct BounceCache {
    entries: Mutex<HashMap<(IpAddr, String), Instant>>,
}

impl BounceCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self::default()
    }

    /// Whether `(ip, name)` dropped during online auth within [`TTL`].
    ///
    /// `None` ip (a socket whose peer address could not be read) is never a
    /// match: the spec pins the fallback to the same address.
    pub fn bounced(&self, ip: Option<IpAddr>, name: &str) -> bool {
        let Some(ip) = ip else { return false };
        let key = (ip, name.to_ascii_lowercase());
        let map = self.entries.lock().unwrap();
        map.get(&key).is_some_and(|at| at.elapsed() < TTL)
    }

    /// Record a drop.
    pub fn record(&self, ip: Option<IpAddr>, name: &str) {
        let Some(ip) = ip else { return };
        let key = (ip, name.to_ascii_lowercase());
        let mut map = self.entries.lock().unwrap();
        if map.len() >= CAP && !map.contains_key(&key) {
            let now = Instant::now();
            map.retain(|_, at| now.duration_since(*at) < TTL);
            if map.len() >= CAP {
                if let Some(k) = map.keys().next().cloned() {
                    map.remove(&k);
                }
            }
        }
        map.insert(key, Instant::now());
    }

    /// How many entries are held. For tests.
    pub fn len(&self) -> usize {
        self.entries.lock().unwrap().len()
    }

    /// Whether the cache is empty. For tests.
    pub fn is_empty(&self) -> bool {
        self.len() == 0
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn ip(s: &str) -> Option<IpAddr> {
        Some(s.parse().unwrap())
    }

    #[test]
    fn a_drop_is_remembered_for_the_same_address_and_name_only() {
        let cache = BounceCache::new();
        cache.record(ip("10.0.0.1"), "Notch");
        assert!(cache.bounced(ip("10.0.0.1"), "notch"), "case-insensitive");
        assert!(!cache.bounced(ip("10.0.0.2"), "notch"), "another address");
        assert!(!cache.bounced(ip("10.0.0.1"), "Alice"), "another name");
        assert!(!cache.bounced(None, "notch"), "no address is never a match");
    }

    #[test]
    fn an_entry_older_than_the_ttl_does_not_match() {
        // The TTL is five minutes; we cannot wait it out, so the clock is
        // nudged by writing an old instant directly.
        let cache = BounceCache::new();
        cache.entries.lock().unwrap().insert(
            (ip("10.0.0.1").unwrap(), "notch".into()),
            Instant::now() - TTL - Duration::from_secs(1),
        );
        assert!(!cache.bounced(ip("10.0.0.1"), "notch"));
    }

    #[test]
    fn the_cache_evicts_rather_than_growing_without_bound() {
        let cache = BounceCache::new();
        for i in 0..(CAP + 10) {
            cache.record(ip("10.0.0.1"), &format!("name{i}"));
        }
        assert!(cache.len() <= CAP, "len {}", cache.len());
    }

    #[test]
    fn a_record_with_no_address_is_ignored() {
        let cache = BounceCache::new();
        cache.record(None, "Notch");
        assert!(cache.is_empty());
    }
}
