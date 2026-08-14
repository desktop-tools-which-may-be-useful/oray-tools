pub mod auth;
pub mod plug;

/// Errors returned by the protocol layer.
#[derive(Debug, thiserror::Error)]
pub enum Error {
    /// Underlying HTTP transport error (connect/timeout/TLS...).
    #[error("network error: {0}")]
    Http(#[from] reqwest::Error),
    /// The endpoint returned a non-success HTTP status.
    #[error("HTTP {status}: {body}")]
    HttpStatus { status: u16, body: String },
    /// The endpoint returned a success status but an unparseable body.
    #[error("unexpected response body: {body}")]
    BadBody { body: String },
    /// The endpoint rejected the request (business `result != 0`).
    #[error("{0}")]
    Api(String),
}

pub type Result<T> = std::result::Result<T, Error>;