pub mod auth;
pub mod remote;
pub mod trace;
pub mod wakeup;

/// User-Agent accepted by the api-std endpoints (they reject unknown formats).
pub const USER_AGENT: &str = "SLCC/15.5.8.83635 (Android)";

/// Errors returned by the protocol layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying HTTP transport error (connect/timeout/TLS...).
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    /// The endpoint returned a non-success HTTP status.
    #[error("{what} failed (HTTP {status}): {body}")]
    HttpStatus {
        what: &'static str,
        status: u16,
        body: String,
    },
    /// The endpoint returned a success status but an unparseable body.
    #[error("unexpected response body: {body}")]
    BadBody {
        body: String,
        #[source]
        source: serde_json::Error,
    },
    /// The endpoint rejected the request (business `result != 0`).
    #[error("{0}")]
    Api(String),
    /// The server rejected the request because the access token has expired.
    #[error("access token expired: {0}")]
    TokenExpired(String),
}

impl Error {
    /// Wrap a server-side message, recognizing Oray's token-expired responses
    /// as [`Error::TokenExpired`] and classifying everything else as
    /// [`Error::Api`].
    ///
    /// Recognition is deliberately exact: a bare `contains("1010")` also
    /// matched unrelated business errors such as `sn=101000000001 not found`
    /// or `plug get failed (code=10100)`, which sent callers into a pointless
    /// refresh/retry and showed them a bogus "access token expired". The
    /// shapes recognized here (case-insensitive; nothing else becomes
    /// `TokenExpired`) are:
    ///
    /// * `token_expired` — the literal message, in any case,
    /// * the XML fragment `<code>1010</code>`,
    /// * [`oray_xml_error`]'s rendering `Oray API error 1010: ...`,
    /// * JSON `"code":1010` / `"code": 1010`,
    /// * `code=1010`, whose digits must start on a boundary too: the
    ///   character before `code` has to be a non-alphanumeric (or the
    ///   marker the start of the message), and the digits must end on a
    ///   non-alphanumeric one — so `{"errorCode":1010}`, `devicecode=1010`,
    ///   `code=10100` and `sn=101000000001` do not match.
    pub fn from_message(desc: String) -> Self {
        if is_token_expired_message(&desc) {
            Error::TokenExpired(desc)
        } else {
            Error::Api(desc)
        }
    }

    /// Build a `BadBody` error, but recognize Oray's XML error responses
    /// (e.g. `TOKEN_EXPIRED`) and surface their code/message instead of a
    /// raw JSON parse failure.
    pub fn bad_body(body: String, source: serde_json::Error) -> Self {
        match oray_xml_error(&body) {
            Some(desc) => Error::from_message(desc),
            None => Error::BadBody { body, source },
        }
    }
}

/// Whether `desc` is one of the exact token-expired shapes documented on
/// [`Error::from_message`]. Hand-rolled scanning (case-insensitive), so the
/// protocol layer keeps its zero-extra-dependency surface.
fn is_token_expired_message(desc: &str) -> bool {
    let lower = desc.to_lowercase();

    // The literal message Oray returns, e.g. `TOKEN_EXPIRED`.
    if lower.contains("token_expired") {
        return true;
    }
    // The XML error document itself: `<code>1010</code>`.
    if lower.contains("<code>1010</code>") {
        return true;
    }
    // Markers that must stand on a proper left boundary (not the tail of a
    // longer key) and be followed by the digits `1010` on a proper right
    // boundary: JSON `"code":1010` (`"code": 1010`), the `code=1010` query
    // style, and `oray_xml_error`'s `Oray API error 1010: ...` rendering.
    ["code=", "code\":", "oray api error "]
        .iter()
        .any(|marker| marker_is_1010(&lower, marker))
}

/// True when `lower` contains `marker` on a proper **left** boundary
/// (start of the message, or preceded by a non-alphanumeric character) and
/// then optional whitespace, the exact digits `1010`, and a non-alphanumeric
/// right boundary — so `{"code":1010}`, `code=1010` and
/// `Oray API error 1010: x` match while `{"errorCode":1010}`,
/// `devicecode=1010`, `code=10100` and `Oray API error 10100: x` do not.
///
/// The left check matters because the markers are *suffixes* of longer keys:
/// without it any field merely ending in `code` (`errorCode`, `devicecode`)
/// matched and sent callers into the pointless refresh/retry this matcher
/// exists to avoid.
fn marker_is_1010(lower: &str, marker: &str) -> bool {
    let mut from = 0usize;
    while let Some(rel) = lower[from..].find(marker) {
        let pos = from + rel;
        let left_ok = lower[..pos]
            .chars()
            .next_back()
            .is_none_or(|c| !c.is_alphanumeric());
        let after = &lower[pos + marker.len()..];
        let rest = after.trim_start();
        let digits_end = rest
            .find(|c: char| !c.is_ascii_digit())
            .unwrap_or(rest.len());
        let next_is_word = rest[digits_end..]
            .chars()
            .next()
            .is_some_and(|c| c.is_alphanumeric());
        if left_ok && &rest[..digits_end] == "1010" && !next_is_word {
            return true;
        }
        // Continue after this occurrence: an embedded (non-boundary) hit
        // must not hide a later one that is on a boundary.
        from = pos + marker.len();
    }
    false
}

/// Render an Oray XML error document as `"Oray API error <code>: <message>"`.
/// Returns `None` when `body` is not such a document.
///
/// This is intentionally a flat text scan, not an XML parser: pulling in an
/// XML dependency for two fields is not worth it, and Oray's error documents
/// are always a single `<response>` root with plain `<code>` / `<message>`
/// children. Consequences worth knowing before changing it:
///
/// * only flat `<code>...</code>` / `<message>...</message>` elements are
///   recognized; nested elements would be read at the wrong level,
/// * elements carrying attributes (`<code type="int">`) are **not** matched —
///   the scan requires the exact `<name>` opening tag,
/// * the leading `<?xml ...?>` prolog (or `<response ...>` root) is detected
///   case-insensitively and tolerates leading whitespace/BOM, but nothing
///   beyond the root element is interpreted.
///
/// The returned string format is part of the error contract: see
/// [`Error::from_message`], which recognizes `Oray API error 1010: ...` as an
/// expired token.
pub fn oray_xml_error(body: &str) -> Option<String> {
    let body = body.trim_start_matches(|c: char| c.is_whitespace() || c == '\u{feff}');
    let prolog = body.starts_with("<?xml");
    let root = body
        .get(..9)
        .is_some_and(|head| head.eq_ignore_ascii_case("<response"));
    if !prolog && !root {
        return None;
    }
    let field = |name: &str| {
        body.split_once(&format!("<{name}>"))
            .and_then(|(_, rest)| rest.split_once(&format!("</{name}>")))
            .map(|(v, _)| v.trim().to_string())
    };
    let code = field("code");
    let message = field("message");
    Some(match (code, message) {
        (Some(code), Some(message)) => format!("Oray API error {code}: {message}"),
        (Some(code), None) => format!("Oray API error {code}"),
        (None, Some(message)) => format!("Oray API error: {message}"),
        (None, None) => "Oray API error (unparseable XML response)".to_string(),
    })
}

pub type Result<T> = std::result::Result<T, Error>;

#[cfg(test)]
mod tests {
    use super::*;

    fn is_expired(desc: &str) -> bool {
        matches!(
            Error::from_message(desc.to_string()),
            Error::TokenExpired(_)
        )
    }

    /// A `1010` that is part of some other identifier must not be mistaken
    /// for the token-expired code (these used to trigger a pointless refresh
    /// and an "access token expired" message for a plain business error).
    #[test]
    fn from_message_ignores_unrelated_1010_occurrences() {
        for desc in [
            "sn=101000000001 not found",
            "plug get failed (code=10100)",
            "code=10100",
            "code=10101: rate limited",
            "Oray API error 10100: device offline",
            "order 1010 rejected",
            "remote 101000000001 is offline",
            // Keys that merely *end* in `code`: the marker without a left
            // boundary classified both as TokenExpired and sent callers into
            // a pointless refresh/retry.
            r#"{"errorCode":1010}"#,
            "devicecode=1010",
        ] {
            assert!(
                matches!(Error::from_message(desc.to_string()), Error::Api(_)),
                "{desc} must stay an Api error"
            );
        }
    }

    #[test]
    fn from_message_recognizes_token_expired_shapes() {
        for desc in [
            // literal message, any case
            "token_expired",
            "TOKEN_EXPIRED",
            "ToKeN_ExPiReD",
            "login failed: token_expired (code=1010)",
            // XML fragment and `oray_xml_error` rendering
            "<code>1010</code>",
            "<response><code>1010</code><message>whatever</message></response>",
            "Oray API error 1010: TOKEN_EXPIRED",
            "Oray API error 1010",
            // JSON forms
            r#"{"code":1010,"message":"expired"}"#,
            r#"{ "code": 1010 }"#,
            // query/inline form on a proper boundary
            "code=1010",
            "login failed (code=1010) ignored message",
            "code=1010, retry later",
        ] {
            assert!(is_expired(desc), "{desc} must be TokenExpired");
        }
    }

    #[test]
    fn from_message_plain_error_stays_api() {
        assert!(matches!(
            Error::from_message("device not found".to_string()),
            Error::Api(_)
        ));
        assert!(matches!(
            Error::from_message("result != 0: rate limited".to_string()),
            Error::Api(_)
        ));
    }

    /// `bad_body` must surface an XML token-expired document as
    /// `TokenExpired` instead of a JSON parse failure.
    #[test]
    fn bad_body_recognizes_xml_token_expired() {
        let source = serde_json::from_str::<serde_json::Value>("{").unwrap_err();
        let body = "<?xml version=\"1.0\" encoding=\"utf-8\"?><response><code>1010</code><message>TOKEN_EXPIRED</message></response>";
        assert!(matches!(
            Error::bad_body(body.to_string(), source),
            Error::TokenExpired(_)
        ));
    }

    #[test]
    fn oray_xml_error_flat_document_format() {
        assert_eq!(
            oray_xml_error(
                "<response><code>1010</code><message>TOKEN_EXPIRED</message></response>"
            ),
            Some("Oray API error 1010: TOKEN_EXPIRED".to_string())
        );
        assert_eq!(
            oray_xml_error("\n  <?xml version=\"1.0\"?><response><code>42</code></response>"),
            Some("Oray API error 42".to_string())
        );
        // `<response >` root, message only, and unparseable-but-recognizable.
        assert_eq!(
            oray_xml_error("<response ><message>boom</message></response>"),
            Some("Oray API error: boom".to_string())
        );
        assert_eq!(
            oray_xml_error("<response></response>"),
            Some("Oray API error (unparseable XML response)".to_string())
        );
        assert_eq!(oray_xml_error("<html>404 not found</html>"), None);
        assert_eq!(oray_xml_error("{\"code\":1010}"), None);
    }
}
