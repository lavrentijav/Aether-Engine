//! A tiny HTTP/1.1 server: enough to serve the web admin and nothing more.
//!
//! No framework and no external CDN are allowed at runtime, so this is a
//! hand-rolled, blocking request/response layer over `std::net`. It parses one
//! request at a time (request line + headers + a bounded body), dispatches it
//! through [`route`], and writes one response. The socket handling is in the
//! plugin; everything decision-shaped lives here so it is testable without a
//! listener.

use std::collections::HashMap;

use aether_ops::OpsService;

use crate::auth::{self, Auth};
use crate::pages;

/// The largest request head (request line + headers) accepted, in bytes.
pub const MAX_HEAD: usize = 16 * 1024;
/// The largest request body accepted, in bytes.
pub const MAX_BODY: usize = 64 * 1024;

/// One parsed request.
#[derive(Debug, Clone, Default)]
pub struct Request {
    pub method: String,
    /// The path without the query string, e.g. `/player`.
    pub path: String,
    pub query: HashMap<String, String>,
    pub headers: HashMap<String, String>,
    pub body: String,
}

impl Request {
    /// A header by name, lower-cased.
    pub fn header(&self, name: &str) -> Option<&str> {
        self.headers
            .get(&name.to_ascii_lowercase())
            .map(|s| s.as_str())
    }

    /// A query parameter, or a form field on `POST /login`.
    pub fn param(&self, name: &str) -> Option<&str> {
        self.query.get(name).map(|s| s.as_str())
    }

    /// The bearer token: the cookie, else `?token=`.
    fn token(&self) -> Option<String> {
        if let Some(cookie) = self.header("cookie") {
            if let Some(t) = auth::token_from_cookie(cookie) {
                return Some(t);
            }
        }
        self.query.get("token").cloned()
    }

    /// The token for a login attempt: the posted form, else the query.
    fn login_token(&self) -> Option<String> {
        if self.method == "POST" {
            for pair in self.body.split('&') {
                if let Some(v) = pair.strip_prefix("token=") {
                    return Some(percent_decode(&v.replace('+', " ")));
                }
            }
        }
        self.query.get("token").cloned()
    }
}

/// One response.
#[derive(Debug, Clone)]
pub struct Response {
    pub status: u16,
    pub content_type: String,
    pub extra_headers: Vec<(String, String)>,
    pub body: Vec<u8>,
}

impl Response {
    pub fn html(status: u16, body: String) -> Response {
        Response {
            status,
            content_type: "text/html; charset=utf-8".into(),
            extra_headers: Vec::new(),
            body: body.into_bytes(),
        }
    }

    pub fn json(status: u16, value: &serde_json::Value) -> Response {
        Response {
            status,
            content_type: "application/json; charset=utf-8".into(),
            extra_headers: Vec::new(),
            body: value.to_string().into_bytes(),
        }
    }

    pub fn text(status: u16, body: &str) -> Response {
        Response {
            status,
            content_type: "text/plain; charset=utf-8".into(),
            extra_headers: Vec::new(),
            body: body.as_bytes().to_vec(),
        }
    }

    pub fn not_found() -> Response {
        Response::text(404, "not found")
    }

    pub fn forbidden() -> Response {
        Response::text(403, "forbidden")
    }

    pub fn redirect(to: &str) -> Response {
        let mut r = Response::text(303, "");
        r.extra_headers.push(("Location".into(), to.into()));
        r
    }

    /// Serialize to wire bytes.
    pub fn encode(&self) -> Vec<u8> {
        let mut out = Vec::with_capacity(64 + self.body.len());
        out.extend_from_slice(
            format!("HTTP/1.1 {} {}\r\n", self.status, reason(self.status)).as_bytes(),
        );
        out.extend_from_slice(format!("Content-Type: {}\r\n", self.content_type).as_bytes());
        out.extend_from_slice(format!("Content-Length: {}\r\n", self.body.len()).as_bytes());
        out.extend_from_slice(b"Cache-Control: no-store\r\n");
        out.extend_from_slice(b"Connection: close\r\n");
        for (k, v) in &self.extra_headers {
            out.extend_from_slice(format!("{k}: {v}\r\n").as_bytes());
        }
        out.extend_from_slice(b"\r\n");
        out.extend_from_slice(&self.body);
        out
    }
}

fn reason(status: u16) -> &'static str {
    match status {
        200 => "OK",
        303 => "See Other",
        400 => "Bad Request",
        401 => "Unauthorized",
        403 => "Forbidden",
        404 => "Not Found",
        405 => "Method Not Allowed",
        431 => "Request Header Fields Too Large",
        _ => "OK",
    }
}

/// Parse a request from its raw head bytes (through the blank line) and an
/// optional body. `None` on a malformed or over-long head.
pub fn parse_request(head: &[u8], body: Vec<u8>) -> Option<Request> {
    if head.len() > MAX_HEAD {
        return None;
    }
    let text = String::from_utf8_lossy(head);
    let mut lines = text.split("\r\n");
    let request_line = lines.next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?.to_string();
    let target = parts.next()?;
    if !matches!(method.as_str(), "GET" | "POST") {
        return None;
    }

    let (path, query) = match target.split_once('?') {
        Some((p, q)) => (p.to_string(), parse_query(q)),
        None => (target.to_string(), HashMap::new()),
    };

    let mut headers = HashMap::new();
    for line in lines {
        if line.is_empty() {
            break;
        }
        if let Some((k, v)) = line.split_once(':') {
            headers.insert(k.trim().to_ascii_lowercase(), v.trim().to_string());
        }
    }

    let body = if body.len() > MAX_BODY {
        return None;
    } else {
        String::from_utf8_lossy(&body).to_string()
    };

    Some(Request {
        method,
        path,
        query,
        headers,
        body,
    })
}

fn parse_query(q: &str) -> HashMap<String, String> {
    let mut out = HashMap::new();
    for pair in q.split('&') {
        if pair.is_empty() {
            continue;
        }
        let (k, v) = pair.split_once('=').unwrap_or((pair, ""));
        out.insert(percent_decode(k), percent_decode(&v.replace('+', " ")));
    }
    out
}

/// Minimal application/x-www-form-urlencoded percent decoding (`%XX`).
fn percent_decode(s: &str) -> String {
    let bytes = s.as_bytes();
    let mut out = Vec::with_capacity(bytes.len());
    let mut i = 0;
    while i < bytes.len() {
        if bytes[i] == b'%' && i + 2 < bytes.len() {
            let hi = (bytes[i + 1] as char).to_digit(16);
            let lo = (bytes[i + 2] as char).to_digit(16);
            if let (Some(hi), Some(lo)) = (hi, lo) {
                out.push((hi * 16 + lo) as u8);
                i += 3;
                continue;
            }
        }
        out.push(bytes[i]);
        i += 1;
    }
    String::from_utf8_lossy(&out).to_string()
}

/// Dispatch a parsed request. `ops` is the live-ops service and `auth` the
/// token state.
pub fn route(req: &Request, ops: &dyn OpsService, auth: &Auth) -> Response {
    // Login is the only unauthenticated write, and the only route that may set
    // a cookie.
    if req.path == "/login" {
        return match req.method.as_str() {
            "POST" => {
                let provided = req.login_token().unwrap_or_default();
                if auth.check(&provided) {
                    let mut r = Response::redirect("/");
                    r.extra_headers.push((
                        "Set-Cookie".into(),
                        format!(
                            "{}={}; Path=/; HttpOnly; SameSite=Strict",
                            auth::COOKIE,
                            provided
                        ),
                    ));
                    r
                } else {
                    Response::html(403, pages::login("wrong token"))
                }
            }
            _ => Response::html(200, pages::login("")),
        };
    }

    let authed = match req.token() {
        Some(t) => auth.check(&t),
        None => false,
    };
    if !authed {
        return match req.method.as_str() {
            "GET" => Response::html(401, pages::login("")),
            _ => Response::forbidden(),
        };
    }

    if req.method != "GET" {
        return Response::text(405, "read-only");
    }

    match req.path.as_str() {
        "/" | "/index.html" => Response::html(200, pages::dashboard(ops)),
        "/regions" => Response::html(200, pages::regions(ops)),
        "/players" => Response::html(200, pages::players(ops)),
        "/player" => match req.param("id") {
            Some(id) => match ops.profile(id) {
                Some(p) => Response::html(200, pages::profile(&p)),
                None => Response::not_found(),
            },
            None => Response::text(400, "id required"),
        },
        "/inventory" => match req.param("id") {
            Some(id) => match ops.inventory(id) {
                Some(v) => Response::html(200, pages::inventory(&v)),
                None => Response::not_found(),
            },
            None => Response::text(400, "id required"),
        },
        "/api/dashboard" => Response::json(200, &serde_json::to_value(ops.dashboard()).unwrap()),
        "/api/regions" => Response::json(200, &serde_json::to_value(ops.regions(100)).unwrap()),
        "/api/players" => Response::json(200, &serde_json::to_value(ops.players()).unwrap()),
        "/api/player" => match req.param("id").and_then(|id| ops.profile(id)) {
            Some(p) => Response::json(200, &serde_json::to_value(p).unwrap()),
            None => Response::not_found(),
        },
        "/api/inventory" => match req.param("id").and_then(|id| ops.inventory(id)) {
            Some(v) => Response::json(200, &serde_json::to_value(v).unwrap()),
            None => Response::not_found(),
        },
        _ => Response::not_found(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn req(method: &str, target: &str) -> Request {
        let head = format!("{method} {target} HTTP/1.1\r\nHost: x\r\n\r\n");
        parse_request(head.as_bytes(), Vec::new()).unwrap()
    }

    #[test]
    fn parses_method_path_and_query() {
        let r = req("GET", "/player?id=abc&x=1");
        assert_eq!(r.method, "GET");
        assert_eq!(r.path, "/player");
        assert_eq!(r.param("id"), Some("abc"));
        assert_eq!(r.param("x"), Some("1"));
    }

    #[test]
    fn parses_headers_case_insensitively() {
        let head = b"GET / HTTP/1.1\r\nCookie: aether_web=tok\r\n\r\n";
        let r = parse_request(head, Vec::new()).unwrap();
        assert_eq!(r.header("cookie"), Some("aether_web=tok"));
    }

    #[test]
    fn rejects_unknown_methods() {
        let head = b"DELETE / HTTP/1.1\r\n\r\n";
        assert!(parse_request(head, Vec::new()).is_none());
    }

    #[test]
    fn percent_decoding_round_trips() {
        assert_eq!(percent_decode("a%20b%2Fc"), "a b/c");
        // A malformed escape is passed through, not dropped.
        assert_eq!(percent_decode("100%"), "100%");
    }

    #[test]
    fn encodes_a_complete_response() {
        let r = Response::text(200, "hi");
        let bytes = r.encode();
        let text = String::from_utf8(bytes).unwrap();
        assert!(text.starts_with("HTTP/1.1 200 OK\r\n"));
        assert!(text.contains("Content-Length: 2\r\n"));
        assert!(text.ends_with("\r\n\r\nhi"));
    }
}
