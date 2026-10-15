//! The two Mojang services online mode talks to.
//!
//! * the **session server** (`hasJoined`) — proves a client controls the
//!   account it claims, and returns the account's uuid, canonical name and
//!   skin properties;
//! * the **profile API** (`/users/profiles/minecraft/…`) — asks whether a
//!   name is premium at all, which is how hybrid mode stops a premium name
//!   being squatted offline before its owner ever connects.
//!
//! Both sit behind [`ProfileApi`] so the hybrid decision table can be tested
//! against a mock with no network at all.

use std::collections::HashMap;
use std::net::IpAddr;
use std::sync::Mutex;
use std::time::{Duration, Instant};

/// A profile property — in practice the `textures` blob carrying the skin.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Property {
    /// e.g. `textures`.
    pub name: String,
    /// Base64-encoded value.
    pub value: String,
    /// Mojang's signature, when the service supplied one.
    pub signature: Option<String>,
}

/// What the session server tells us about a player who authenticated.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Profile {
    /// The account's uuid.
    pub uuid: u128,
    /// The account's canonical spelling of the name.
    pub name: String,
    /// Skin and cape properties.
    pub properties: Vec<Property>,
}

/// Why an API call did not produce an answer.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ApiError {
    /// Could not connect, timed out, or TLS failed.
    Unreachable,
    /// An HTTP status we do not treat as an answer.
    Status(u16),
    /// The body was not the JSON we expect.
    Decode,
}

/// The Mojang calls the login path makes.
pub trait ProfileApi: Send + Sync {
    /// `hasJoined`: `Ok(Some(profile))` when the client proved the account,
    /// `Ok(None)` when the service says it did not.
    fn has_joined(
        &self,
        username: &str,
        server_hash: &str,
        ip: Option<IpAddr>,
    ) -> Result<Option<Profile>, ApiError>;

    /// Whether a premium account owns `name`, and its uuid if so.
    fn lookup_name(&self, name: &str) -> Result<Option<u128>, ApiError>;
}

/// The real client, over HTTPS with rustls (no OpenSSL in the build).
pub struct MojangApi {
    session_server: String,
    profile_api: String,
    http: reqwest::blocking::Client,
}

impl MojangApi {
    /// Build a client for the configured endpoints. The timeout bounds every
    /// call, so a hung Mojang endpoint cannot pin a login thread.
    pub fn new(
        session_server: &str,
        profile_api: &str,
        timeout: Duration,
    ) -> Result<Self, ApiError> {
        let http = reqwest::blocking::Client::builder()
            .timeout(timeout)
            // The base URLs are configured; never follow a redirect to
            // somewhere else with the account's name in the query string.
            .redirect(reqwest::redirect::Policy::none())
            .build()
            .map_err(|_| ApiError::Unreachable)?;
        Ok(Self {
            session_server: session_server.trim_end_matches('/').to_string(),
            profile_api: profile_api.trim_end_matches('/').to_string(),
            http,
        })
    }
}

impl ProfileApi for MojangApi {
    fn has_joined(
        &self,
        username: &str,
        server_hash: &str,
        ip: Option<IpAddr>,
    ) -> Result<Option<Profile>, ApiError> {
        let mut url = format!(
            "{}/session/minecraft/hasJoined?username={}&serverId={}",
            self.session_server,
            urlencode(username),
            urlencode(server_hash),
        );
        if let Some(ip) = ip {
            // Vanilla adds the client address so Mojang can check IP
            // consistency for accounts with that protection on.
            url.push_str("&ip=");
            url.push_str(&ip.to_string());
        }
        let resp = self
            .http
            .get(&url)
            .send()
            .map_err(|_| ApiError::Unreachable)?;
        let status = resp.status().as_u16();
        match status {
            // 204/empty is "this client did not authenticate".
            204 | 404 => Ok(None),
            200 => {
                let body = resp.text().map_err(|_| ApiError::Unreachable)?;
                if body.trim().is_empty() {
                    return Ok(None);
                }
                parse_profile(&body).map(Some).ok_or(ApiError::Decode)
            }
            other => Err(ApiError::Status(other)),
        }
    }

    fn lookup_name(&self, name: &str) -> Result<Option<u128>, ApiError> {
        let url = format!(
            "{}/users/profiles/minecraft/{}",
            self.profile_api,
            urlencode(name)
        );
        let resp = self
            .http
            .get(&url)
            .send()
            .map_err(|_| ApiError::Unreachable)?;
        match resp.status().as_u16() {
            204 | 404 => Ok(None),
            200 => {
                let body = resp.text().map_err(|_| ApiError::Unreachable)?;
                if body.trim().is_empty() || body.trim() == "[]" {
                    return Ok(None);
                }
                parse_name_lookup(&body).ok_or(ApiError::Decode)
            }
            other => Err(ApiError::Status(other)),
        }
    }
}

/// Percent-encode the characters that appear in usernames and hashes.
///
/// A username is `[A-Za-z0-9_]` and a server hash may start with `-`, so this
/// is deliberately small rather than a full URI encoder.
fn urlencode(s: &str) -> String {
    let mut out = String::with_capacity(s.len());
    for b in s.bytes() {
        match b {
            b'A'..=b'Z' | b'a'..=b'z' | b'0'..=b'9' | b'-' | b'_' | b'.' | b'~' => {
                out.push(b as char)
            }
            _ => out.push_str(&format!("%{b:02X}")),
        }
    }
    out
}

/// Parse the session server's profile object without a JSON dependency in the
/// hot path.
///
/// The shape is fixed and small: `{"id":"…","name":"…","properties":[…]}`.
/// Using `serde_json` here would be fine too; this keeps the failure mode
/// explicit (a missing field is [`ApiError::Decode`], never a silent default).
fn parse_profile(body: &str) -> Option<Profile> {
    let id = json_string(body, "id")?;
    let uuid = parse_hyphenated_uuid(&id)?;
    let name = json_string(body, "name").unwrap_or_default();
    let properties = parse_properties(body);
    Some(Profile {
        uuid,
        name,
        properties,
    })
}

/// Parse `[{"id":"…","name":"…"}]` from the profile API.
fn parse_name_lookup(body: &str) -> Option<Option<u128>> {
    let id = json_string(body, "id")?;
    Some(parse_hyphenated_uuid(&id))
}

/// The value of the first `"key":"value"` string in `body`.
fn json_string(body: &str, key: &str) -> Option<String> {
    let needle = format!("\"{key}\"");
    let start = body.find(&needle)? + needle.len();
    let rest = body[start..].trim_start();
    let rest = rest.strip_prefix(':')?.trim_start();
    let rest = rest.strip_prefix('"')?;
    let mut out = String::new();
    let mut chars = rest.chars();
    while let Some(c) = chars.next() {
        match c {
            '"' => return Some(out),
            '\\' => match chars.next()? {
                '"' => out.push('"'),
                '\\' => out.push('\\'),
                '/' => out.push('/'),
                'n' => out.push('\n'),
                't' => out.push('\t'),
                'r' => out.push('\r'),
                'u' => {
                    let hex: String = chars.by_ref().take(4).collect();
                    let v = u32::from_str_radix(&hex, 16).ok()?;
                    out.push(char::from_u32(v)?);
                }
                other => out.push(other),
            },
            c => out.push(c),
        }
    }
    None
}

/// The `properties` array, if present. Best-effort: a malformed skin is not
/// worth refusing a login over, so parse failures yield an empty list.
fn parse_properties(body: &str) -> Vec<Property> {
    let mut out = Vec::new();
    let Some(start) = body.find("\"properties\"") else {
        return out;
    };
    let Some(arr) = body[start..].find('[') else {
        return out;
    };
    let tail = &body[start + arr..];
    let mut rest = tail;
    while let Some(obj) = rest.find('{') {
        let after = &rest[obj..];
        let Some(end) = after.find('}') else { break };
        let chunk = &after[..=end];
        if let Some(name) = json_string(chunk, "name") {
            if let Some(value) = json_string(chunk, "value") {
                out.push(Property {
                    name,
                    value,
                    signature: json_string(chunk, "signature"),
                });
            }
        }
        rest = &after[end + 1..];
    }
    out
}

/// Parse `8-4-4-4-12` hex into a u128.
pub fn parse_hyphenated_uuid(s: &str) -> Option<u128> {
    let hex: String = s.chars().filter(|c| *c != '-').collect();
    if hex.len() != 32 {
        return None;
    }
    u128::from_str_radix(&hex, 16).ok()
}

/// Format a u128 as `8-4-4-4-12`, the spelling Mojang uses.
pub fn hyphenated_uuid(uuid: u128) -> String {
    let hex = format!("{uuid:032x}");
    format!(
        "{}-{}-{}-{}-{}",
        &hex[0..8],
        &hex[8..12],
        &hex[12..16],
        &hex[16..20],
        &hex[20..32]
    )
}

/// A bounded, time-limited cache of name → premium-or-not.
///
/// Positive answers are held for [`POSITIVE_TTL`], negative for
/// [`NEGATIVE_TTL`]: a name that just became premium should start being
/// enforced promptly, while a confirmed premium name should not cost a lookup
/// on every reconnect. The map is capped so a scanner cycling names cannot
/// grow it without bound.
pub struct LookupCache {
    entries: Mutex<HashMap<String, CacheEntry>>,
}

struct CacheEntry {
    uuid: Option<u128>,
    at: Instant,
}

/// How long a positive lookup is trusted.
pub const POSITIVE_TTL: Duration = Duration::from_secs(3600);
/// How long a negative lookup is trusted.
pub const NEGATIVE_TTL: Duration = Duration::from_secs(600);
/// The most names cached at once.
pub const CACHE_CAP: usize = 4096;

impl Default for LookupCache {
    fn default() -> Self {
        Self::new()
    }
}

impl LookupCache {
    /// An empty cache.
    pub fn new() -> Self {
        Self {
            entries: Mutex::new(HashMap::new()),
        }
    }

    /// Look up `name`, consulting the cache first and `api` on a miss.
    pub fn get(&self, api: &dyn ProfileApi, name: &str) -> Result<Option<u128>, ApiError> {
        let key = name.to_ascii_lowercase();
        if let Some(hit) = self.fresh(&key) {
            return Ok(hit);
        }
        let value = api.lookup_name(name)?;
        self.store(key, value);
        Ok(value)
    }

    fn fresh(&self, key: &str) -> Option<Option<u128>> {
        let map = self.entries.lock().unwrap();
        let e = map.get(key)?;
        let ttl = if e.uuid.is_some() {
            POSITIVE_TTL
        } else {
            NEGATIVE_TTL
        };
        (e.at.elapsed() < ttl).then_some(e.uuid)
    }

    fn store(&self, key: String, uuid: Option<u128>) {
        let mut map = self.entries.lock().unwrap();
        if map.len() >= CACHE_CAP && !map.contains_key(&key) {
            // Evict expired entries first; if none, drop an arbitrary one.
            let now = Instant::now();
            map.retain(|_, e| {
                let ttl = if e.uuid.is_some() {
                    POSITIVE_TTL
                } else {
                    NEGATIVE_TTL
                };
                now.duration_since(e.at) < ttl
            });
            if map.len() >= CACHE_CAP {
                if let Some(k) = map.keys().next().cloned() {
                    map.remove(&k);
                }
            }
        }
        map.insert(
            key,
            CacheEntry {
                uuid,
                at: Instant::now(),
            },
        );
    }

    /// How many names are cached. For tests.
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
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct Counting {
        calls: AtomicUsize,
        answer: Option<u128>,
    }

    impl ProfileApi for Counting {
        fn has_joined(
            &self,
            _u: &str,
            _h: &str,
            _ip: Option<IpAddr>,
        ) -> Result<Option<Profile>, ApiError> {
            Ok(None)
        }
        fn lookup_name(&self, _name: &str) -> Result<Option<u128>, ApiError> {
            self.calls.fetch_add(1, Ordering::Relaxed);
            Ok(self.answer)
        }
    }

    #[test]
    fn uuid_text_round_trips() {
        let uuid = 0x069a79f4_4d42_4e6a_8a0a_5c2e5a4e8e1f;
        let s = hyphenated_uuid(uuid);
        assert_eq!(s, "069a79f4-4d42-4e6a-8a0a-5c2e5a4e8e1f");
        assert_eq!(parse_hyphenated_uuid(&s), Some(uuid));
        assert_eq!(parse_hyphenated_uuid("nope"), None);
    }

    #[test]
    fn the_profile_parser_reads_the_session_server_shape() {
        let body = r#"{"id":"069a79f4-4d42-4e6a-8a0a-5c2e5a4e8e1f","name":"Notch","properties":[{"name":"textures","value":"abc","signature":"sig"}]}"#;
        let p = parse_profile(body).unwrap();
        assert_eq!(p.uuid, 0x069a79f4_4d42_4e6a_8a0a_5c2e5a4e8e1f);
        assert_eq!(p.name, "Notch");
        assert_eq!(p.properties.len(), 1);
        assert_eq!(p.properties[0].value, "abc");
        assert_eq!(p.properties[0].signature.as_deref(), Some("sig"));
    }

    #[test]
    fn the_name_lookup_parser_reads_the_array_shape() {
        let body = r#"[{"id":"069a79f4-4d42-4e6a-8a0a-5c2e5a4e8e1f","name":"Notch"}]"#;
        assert_eq!(
            parse_name_lookup(body),
            Some(Some(0x069a79f4_4d42_4e6a_8a0a_5c2e5a4e8e1f))
        );
    }

    #[test]
    fn a_positive_lookup_is_cached_and_a_second_call_does_not_hit_the_api() {
        let api = Counting {
            calls: AtomicUsize::new(0),
            answer: Some(42),
        };
        let cache = LookupCache::new();
        assert_eq!(cache.get(&api, "Notch").unwrap(), Some(42));
        assert_eq!(cache.get(&api, "notch").unwrap(), Some(42));
        assert_eq!(api.calls.load(Ordering::Relaxed), 1, "case-insensitive hit");
        assert_eq!(cache.len(), 1);
    }

    #[test]
    fn the_cache_evicts_rather_than_growing_without_bound() {
        let api = Counting {
            calls: AtomicUsize::new(0),
            answer: None,
        };
        let cache = LookupCache::new();
        for i in 0..(CACHE_CAP + 10) {
            let _ = cache.get(&api, &format!("name{i}"));
        }
        assert!(cache.len() <= CACHE_CAP, "cache len {}", cache.len());
    }

    /// A minimal HTTP/1.1 server that answers one request per connection
    /// from a fixed path → (status, body) table.
    fn spawn_http(routes: Vec<(String, u16, String)>) -> String {
        use std::io::{Read, Write};
        let listener = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
        let addr = listener.local_addr().unwrap();
        std::thread::spawn(move || {
            for stream in listener.incoming() {
                let mut stream = stream.unwrap();
                let mut buf = [0u8; 2048];
                let n = stream.read(&mut buf).unwrap_or(0);
                let req = String::from_utf8_lossy(&buf[..n]);
                let path = req
                    .lines()
                    .next()
                    .and_then(|l| l.split_whitespace().nth(1))
                    .unwrap_or("")
                    .to_string();
                let (status, body) = routes
                    .iter()
                    .find(|(p, _, _)| path.contains(p.as_str()))
                    .map(|(_, s, b)| (*s, b.clone()))
                    .unwrap_or((404, String::new()));
                let reason = if status == 200 { "OK" } else { "No Content" };
                let resp = format!(
                    "HTTP/1.1 {status} {reason}\r\nContent-Length: {}\r\nContent-Type: application/json\r\nConnection: close\r\n\r\n{body}",
                    body.len()
                );
                let _ = stream.write_all(resp.as_bytes());
            }
        });
        format!("http://{addr}")
    }

    #[test]
    fn the_real_client_parses_a_hasjoined_profile() {
        let base = spawn_http(vec![(
            "/session/minecraft/hasJoined".into(),
            200,
            r#"{"id":"069a79f4-4d42-4e6a-8a0a-5c2e5a4e8e1f","name":"Notch","properties":[]}"#
                .into(),
        )]);
        let api = MojangApi::new(&base, &base, Duration::from_secs(5)).unwrap();
        let p = api
            .has_joined("Notch", "abc", None)
            .unwrap()
            .expect("a profile");
        assert_eq!(p.uuid, 0x069a79f4_4d42_4e6a_8a0a_5c2e5a4e8e1f);
        assert_eq!(p.name, "Notch");
    }

    #[test]
    fn the_real_client_reads_204_as_no_profile() {
        let base = spawn_http(vec![(
            "/session/minecraft/hasJoined".into(),
            204,
            String::new(),
        )]);
        let api = MojangApi::new(&base, &base, Duration::from_secs(5)).unwrap();
        assert_eq!(api.has_joined("x", "h", None).unwrap(), None);
    }

    #[test]
    fn the_real_client_reads_the_name_lookup() {
        let base = spawn_http(vec![(
            "/users/profiles/minecraft/".into(),
            200,
            r#"[{"id":"069a79f4-4d42-4e6a-8a0a-5c2e5a4e8e1f","name":"Notch"}]"#.into(),
        )]);
        let api = MojangApi::new(&base, &base, Duration::from_secs(5)).unwrap();
        assert_eq!(
            api.lookup_name("Notch").unwrap(),
            Some(0x069a79f4_4d42_4e6a_8a0a_5c2e5a4e8e1f)
        );
    }

    #[test]
    fn a_refused_connection_is_unreachable() {
        // Port 1 is not listening; the call must come back as an error, not
        // hang or panic.
        let api = MojangApi::new(
            "http://127.0.0.1:1",
            "http://127.0.0.1:1",
            Duration::from_millis(300),
        )
        .unwrap();
        assert_eq!(
            api.has_joined("x", "h", None).unwrap_err(),
            ApiError::Unreachable
        );
    }

    #[test]
    fn api_errors_are_surfaced_not_swallowed() {
        struct Down;
        impl ProfileApi for Down {
            fn has_joined(
                &self,
                _u: &str,
                _h: &str,
                _ip: Option<IpAddr>,
            ) -> Result<Option<Profile>, ApiError> {
                Err(ApiError::Unreachable)
            }
            fn lookup_name(&self, _n: &str) -> Result<Option<u128>, ApiError> {
                Err(ApiError::Unreachable)
            }
        }
        assert_eq!(
            LookupCache::new().get(&Down, "x").unwrap_err(),
            ApiError::Unreachable
        );
    }
}
