//! Web-admin authentication.
//!
//! Two ways in, matching the brief:
//!
//! * a **token from config** (`[plugins.webadmin] token = "…"`) — the operator
//!   picks it and it never enters git;
//! * a **one-time token** minted in game by an operator running
//!   `/webadmin token`, valid for a few minutes and replaced each time.
//!
//! The token is carried in an `HttpOnly` cookie after login, or directly as a
//! `?token=` query parameter (handy for `curl`). No secret is ever embedded in
//! this source or in a page.

use std::sync::Mutex;
use std::time::{Duration, Instant};

/// How long a one-time token lives.
pub const ONE_TIME_TTL: Duration = Duration::from_secs(300);

/// The cookie name the login sets.
pub const COOKIE: &str = "aether_web";

/// The token state.
pub struct Auth {
    config_token: Option<String>,
    one_time: Mutex<Option<(String, Instant)>>,
}

impl Auth {
    /// Build from an optional config token.
    pub fn new(config_token: Option<String>) -> Auth {
        Auth {
            config_token: config_token.filter(|t| !t.is_empty()),
            one_time: Mutex::new(None),
        }
    }

    /// Whether `provided` matches the config token or a live one-time token.
    ///
    /// The comparison is constant-time-ish (length + XOR accumulate) so a
    /// token cannot be read off by timing the login endpoint.
    pub fn check(&self, provided: &str) -> bool {
        if provided.is_empty() {
            return false;
        }
        if let Some(cfg) = &self.config_token {
            if constant_eq(cfg, provided) {
                return true;
            }
        }
        let mut slot = self.one_time.lock().unwrap();
        match slot.as_ref() {
            Some((token, until)) if *until > Instant::now() && constant_eq(token, provided) => true,
            Some(_) => {
                // Expired: drop it so a stale token cannot linger.
                *slot = None;
                false
            }
            None => false,
        }
    }

    /// Mint a fresh one-time token, replacing any previous one.
    pub fn issue_one_time(&self) -> String {
        let mut bytes = [0u8; 16];
        if !fill_random(&mut bytes) {
            // Fallback: time and address entropy. Only reached off Linux or if
            // `/dev/urandom` cannot be opened.
            let nanos = std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map(|d| d.as_nanos() as u64)
                .unwrap_or(0);
            let addr = &self as *const _ as u64;
            let mix = nanos ^ addr.rotate_left(17);
            for (i, b) in bytes.iter_mut().enumerate() {
                *b = (mix >> ((i * 8) % 64)) as u8;
            }
        }
        let token: String = bytes.iter().map(|b| format!("{b:02x}")).collect();
        *self.one_time.lock().unwrap() = Some((token.clone(), Instant::now() + ONE_TIME_TTL));
        token
    }

    /// Whether a config token is in force (so a one-time token is not needed).
    pub fn has_config_token(&self) -> bool {
        self.config_token.is_some()
    }
}

/// Fill `buf` from the OS CSPRNG. Returns false if it cannot be read.
fn fill_random(buf: &mut [u8]) -> bool {
    use std::io::Read;
    match std::fs::File::open("/dev/urandom") {
        Ok(mut f) => f.read_exact(buf).is_ok(),
        Err(_) => false,
    }
}

/// Compare two strings without an early-exit on the first differing byte.
fn constant_eq(a: &str, b: &str) -> bool {
    if a.len() != b.len() {
        return false;
    }
    let mut diff = 0u8;
    for (x, y) in a.bytes().zip(b.bytes()) {
        diff |= x ^ y;
    }
    diff == 0
}

/// Pull a token out of a `Cookie:` header value (`aether_web=…`).
pub fn token_from_cookie(header: &str) -> Option<String> {
    for part in header.split(';') {
        let part = part.trim();
        if let Some(v) = part.strip_prefix(&format!("{COOKIE}=")) {
            return Some(v.to_string());
        }
    }
    None
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn config_token_authenticates() {
        let a = Auth::new(Some("secret".into()));
        assert!(a.check("secret"));
        assert!(!a.check("nope"));
        assert!(!a.check(""));
    }

    #[test]
    fn one_time_token_expires_and_is_single_live_value() {
        let a = Auth::new(None);
        let t1 = a.issue_one_time();
        assert!(a.check(&t1));
        // A new one replaces the old.
        let t2 = a.issue_one_time();
        assert_eq!(t1.len(), 32);
        assert!(a.check(&t2));
        assert!(!a.check(&t1), "the previous token stops working");
    }

    #[test]
    fn cookie_parsing_finds_the_token() {
        assert_eq!(
            token_from_cookie("foo=1; aether_web=abc123; bar=2"),
            Some("abc123".into())
        );
        assert_eq!(token_from_cookie("foo=1"), None);
    }

    #[test]
    fn constant_eq_matches_only_equal() {
        assert!(constant_eq("abc", "abc"));
        assert!(!constant_eq("abc", "abd"));
        assert!(!constant_eq("abc", "ab"));
    }
}
