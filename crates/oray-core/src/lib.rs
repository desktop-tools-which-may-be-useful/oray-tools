pub mod auth;
pub mod plug;

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
    /// (XML code `1010` or message `TOKEN_EXPIRED`) as `TokenExpired`.
    pub fn from_message(desc: String) -> Self {
        let lower = desc.to_lowercase();
        if lower.contains("1010") || lower.contains("token_expired") {
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

/// Render an Oray XML error document as `"Oray API error <code>: <message>"`.
/// Returns `None` when `body` is not such a document.
pub fn oray_xml_error(body: &str) -> Option<String> {
    let body = body.trim_start();
    if !body.starts_with("<?xml") && !body.starts_with("<response") {
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