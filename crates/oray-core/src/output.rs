//! Output and debug helpers shared by the CLI.
//!
//! The protocol layer reports verbose HTTP request/response details to stderr
//! through a process-wide flag (set by the CLI from `--verbose`). Human/JSON
//! presentation itself is left to the CLI; this module only centralizes the
//! verbose switch and a few formatting helpers.

use std::sync::atomic::{AtomicBool, Ordering};

static VERBOSE: AtomicBool = AtomicBool::new(false);

/// Enable or disable verbose (request/response) logging.
pub fn set_verbose(enabled: bool) {
    VERBOSE.store(enabled, Ordering::Relaxed);
}

/// Whether verbose logging is currently enabled.
pub fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

/// Print a redacted HTTP request line to stderr when verbose logging is on.
pub fn log_request(method: &str, url: &str) {
    if verbose() {
        eprintln!("[DEBUG] {method} {url}");
    }
}

/// Print a redacted authorization header to stderr when verbose logging is on.
pub fn log_auth_header(prefix: &str, token: &str) {
    if verbose() {
        let shown = if token.is_empty() {
            "<none>".to_string()
        } else {
            format!("{}***", &token[..token.len().min(6)])
        };
        eprintln!("[DEBUG] {prefix}Authorization: Bearer {shown}");
    }
}

/// Print a response status line to stderr when verbose logging is on.
pub fn log_response(status: u16, body: &str) {
    if verbose() {
        eprintln!("[DEBUG] Response: {status}");
        let body = body.trim();
        if !body.is_empty() {
            let max = 4096;
            if body.len() > max {
                eprintln!("[DEBUG] Body: {}…", &body[..max]);
            } else {
                eprintln!("[DEBUG] Body: {body}");
            }
        }
    }
}
