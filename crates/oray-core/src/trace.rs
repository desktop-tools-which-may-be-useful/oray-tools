//! Pure request/response traces returned by every API call.
//!
//! `oray-core` performs no output of its own and never prints: each HTTP
//! exchange is captured as a [`RequestLog`] and returned together with the
//! parsed data ([`Traced`]) or with the error ([`TracedError`]). A
//! presentation layer (the CLI, a web UI, a TUI, ...) decides how to render a
//! trace — and whether to apply [`RequestLog::redacted`] to mask sensitive
//! values before showing it.

use crate::{Error, Result};
use serde::Serialize;
use std::fmt;
use std::ops::{Deref, DerefMut};

/// One captured HTTP exchange. Values are captured raw; masking is up to the
/// caller via [`RequestLog::redacted`].
#[derive(Debug, Clone, Serialize)]
pub struct RequestLog {
    /// HTTP method (e.g. `GET`, `POST`).
    pub method: String,
    /// Full request URL, including any query string.
    pub url: String,
    /// Request headers exactly as sent, in order. Contains the raw
    /// `Authorization` value when present; redact before displaying.
    pub request_headers: Vec<(String, String)>,
    /// Serialized request body, when the request carried one.
    pub request_body: Option<String>,
    /// Response HTTP status; `None` when no response was received
    /// (e.g. a network error before any bytes came back).
    pub status: Option<u16>,
    /// Full response body.
    pub response_body: String,
}

impl RequestLog {
    /// A copy safe for display or logging: the `Authorization` header value is
    /// shortened, and JSON request/response bodies have string values under
    /// sensitive keys masked. Non-JSON bodies are kept verbatim.
    pub fn redacted(&self) -> RequestLog {
        let request_headers = self
            .request_headers
            .iter()
            .map(|(name, value)| {
                if name.eq_ignore_ascii_case("authorization") {
                    (name.clone(), mask_authorization(value))
                } else {
                    (name.clone(), value.clone())
                }
            })
            .collect();
        RequestLog {
            method: self.method.clone(),
            url: self.url.clone(),
            request_headers,
            request_body: self.request_body.as_deref().map(mask_body),
            status: self.status,
            response_body: mask_body(&self.response_body),
        }
    }
}

/// Parsed result of an API operation plus the raw HTTP exchanges that produced
/// it. An "operation" may span several requests (e.g. `find` lists first), so
/// `calls` is ordered and complete.
#[derive(Debug)]
pub struct Traced<T> {
    pub data: T,
    pub calls: Vec<RequestLog>,
}

impl<T> Deref for Traced<T> {
    type Target = T;
    fn deref(&self) -> &T {
        &self.data
    }
}

impl<T> DerefMut for Traced<T> {
    fn deref_mut(&mut self) -> &mut T {
        &mut self.data
    }
}

/// An [`Error`] from the protocol layer together with the exchanges that led
/// to it, so callers can report exactly what failed.
#[derive(Debug)]
pub struct TracedError {
    pub error: Error,
    pub calls: Vec<RequestLog>,
}

impl TracedError {
    /// Whether the server reported an expired access token
    /// (see [`Error::TokenExpired`]).
    pub fn is_token_expired(&self) -> bool {
        matches!(self.error, Error::TokenExpired(_))
    }
}

impl Deref for TracedError {
    type Target = Error;
    fn deref(&self) -> &Error {
        &self.error
    }
}

impl fmt::Display for TracedError {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        write!(f, "{}", self.error)
    }
}

impl std::error::Error for TracedError {
    // No `source`: the inner `Error` is the same text as `Display`, so
    // exposing it would duplicate every message in a `{:#}` chain. Classify
    // with `Deref` / `is_token_expired` instead.
}

/// Result type of the public API surface: data and errors both carry the
/// request/response exchanges involved.
pub type TracedResult<T> = std::result::Result<Traced<T>, TracedError>;

/// Transport-level result: an executed exchange (not yet parsed) or a traced
/// error. Public API methods convert these into [`TracedResult`] values.
pub type RawResult<T> = std::result::Result<T, TracedError>;

impl From<Error> for TracedError {
    fn from(error: Error) -> Self {
        TracedError {
            error,
            calls: Vec::new(),
        }
    }
}

/// A fully specified HTTP exchange about to be executed.
pub struct Request {
    pub method: &'static str,
    pub url: String,
    pub headers: Vec<(String, String)>,
    pub body: Option<String>,
}

/// Outcome of one executed request that produced a response body.
pub struct Exchange {
    pub log: RequestLog,
    pub status: u16,
    pub text: String,
}

/// Execute a request, capturing its full details into a [`RequestLog`].
///
/// The body must be passed pre-serialized together with the matching
/// `Content-Type` header, because a `reqwest` body cannot be read back once
/// attached. A non-2xx response is *not* an error here: callers inspect
/// [`Exchange::status`] so they can branch (e.g. Oray's `202` login flow).
pub fn execute(client: &reqwest::blocking::Client, req: Request) -> RawResult<Exchange> {
    let mut rb = client.request(
        reqwest::Method::from_bytes(req.method.as_bytes()).unwrap_or(reqwest::Method::GET),
        &req.url,
    );
    for (name, value) in &req.headers {
        rb = rb.header(name, value);
    }
    if let Some(body) = &req.body {
        rb = rb.body(body.clone());
    }
    let partial = |status: Option<u16>| RequestLog {
        method: req.method.to_string(),
        url: req.url.clone(),
        request_headers: req.headers.clone(),
        request_body: req.body.clone(),
        status,
        response_body: String::new(),
    };
    match rb.send() {
        Err(e) => Err(TracedError {
            error: Error::Http(e),
            calls: vec![partial(None)],
        }),
        Ok(resp) => {
            let status = resp.status().as_u16();
            match resp.text() {
                Err(e) => Err(TracedError {
                    error: Error::Http(e),
                    calls: vec![partial(Some(status))],
                }),
                Ok(text) => Ok(Exchange {
                    log: RequestLog {
                        method: req.method.to_string(),
                        url: req.url,
                        request_headers: req.headers,
                        request_body: req.body,
                        status: Some(status),
                        response_body: text.clone(),
                    },
                    status,
                    text,
                }),
            }
        }
    }
}

/// Wrap a parsed/checked plain result with the exchanges that produced it.
pub fn finish<T>(calls: Vec<RequestLog>, parsed: Result<T>) -> TracedResult<T> {
    match parsed {
        Ok(data) => Ok(Traced { data, calls }),
        Err(error) => Err(TracedError { error, calls }),
    }
}

fn sensitive_key(key: &str) -> bool {
    const KEYS: &[&str] = &[
        "token",
        "password",
        "passwd",
        "checksum",
        "authorization",
        "cookie",
        "secret",
        "api_key",
        "apikey",
        "key",
    ];
    let k = key.to_ascii_lowercase();
    KEYS.iter().any(|s| k.contains(s))
}

fn mask_str(s: &str) -> String {
    let shown: String = s.chars().take(4).collect();
    format!("{shown}***")
}

fn mask_authorization(s: &str) -> String {
    if let Some(rest) = s.strip_prefix("Bearer ") {
        let shown: String = rest.chars().take(6).collect();
        format!("Bearer {shown}***")
    } else {
        mask_str(s)
    }
}

fn mask_body(body: &str) -> String {
    let Ok(mut value) = serde_json::from_str::<serde_json::Value>(body) else {
        return body.to_string();
    };
    mask_value(&mut value);
    serde_json::to_string_pretty(&value).unwrap_or_else(|_| body.to_string())
}

fn mask_value(value: &mut serde_json::Value) {
    match value {
        serde_json::Value::Object(map) => {
            for (key, val) in map.iter_mut() {
                if sensitive_key(key)
                    && let serde_json::Value::String(s) = val
                {
                    *val = serde_json::Value::String(mask_str(s));
                    continue;
                }
                mask_value(val);
            }
        }
        serde_json::Value::Array(items) => {
            for item in items.iter_mut() {
                mask_value(item);
            }
        }
        _ => {}
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn redact_authorization_header() {
        let log = RequestLog {
            method: "GET".into(),
            url: "https://api.example/x".into(),
            request_headers: vec![
                ("Accept".into(), "application/json".into()),
                ("Authorization".into(), "Bearer abcdef012345".into()),
            ],
            request_body: None,
            status: Some(200),
            response_body: String::new(),
        };
        let r = log.redacted();
        assert_eq!(r.request_headers[1].1, "Bearer abcdef***");
        assert_eq!(r.request_headers[0].1, "application/json");
    }

    #[test]
    fn redact_nested_json_sensitive_keys() {
        let log = RequestLog {
            method: "POST".into(),
            url: "https://api.example/login".into(),
            request_headers: Vec::new(),
            request_body: Some(r#"{"account":"user","password":"d41d8cd98f00b204e9800998ecf8427e","extra":{"refresh_token":"rt123","note":"hi"}}"#.into()),
            status: Some(200),
            response_body: r#"{"access_token":"at789","refresh_token":"rt456","ok":true}"#.into(),
        };
        let r = log.redacted();
        let body: serde_json::Value =
            serde_json::from_str(r.request_body.as_deref().unwrap()).unwrap();
        assert_eq!(body["account"], "user");
        assert!(body["password"].as_str().unwrap().contains("***"));
        assert!(body["password"].as_str().unwrap().starts_with("d41d"));
        assert!(
            body["extra"]["refresh_token"]
                .as_str()
                .unwrap()
                .ends_with("***")
        );
        assert_eq!(body["extra"]["note"], "hi");
        let resp: serde_json::Value = serde_json::from_str(&r.response_body).unwrap();
        assert!(resp["access_token"].as_str().unwrap().ends_with("***"));
        assert!(resp["refresh_token"].as_str().unwrap().ends_with("***"));
        assert_eq!(resp["ok"], true);
    }

    #[test]
    fn redact_leaves_non_json_bodies_alone() {
        let log = RequestLog {
            method: "GET".into(),
            url: "https://api.example/x".into(),
            request_headers: Vec::new(),
            request_body: None,
            status: Some(200),
            response_body: "plain text body".into(),
        };
        assert_eq!(log.redacted().response_body, "plain text body");
    }

    #[test]
    fn redacted_short_authorization_is_all_masked() {
        assert_eq!(mask_authorization("abc"), "abc***");
        assert_eq!(mask_authorization("Bearer ab"), "Bearer ab***");
        assert!(mask_authorization("Bearer abcdef012345").ends_with("***"));
    }
}
