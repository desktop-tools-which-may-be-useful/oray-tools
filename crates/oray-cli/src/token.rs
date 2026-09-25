use crate::config::{Config, Token};
use anyhow::{Result, bail};
use base64::Engine;
use oray_core::auth::{AuthApi, AuthResponse};
use reqwest::blocking::Client as HttpClient;
use std::path::PathBuf;

/// Absolute unix timestamp at which the access token expires,
/// computed per Oray's recommendation as `now + (exp - isa)`.
pub fn access_expiry(token: &str) -> Option<i64> {
    let payload = jwt_payload(token)?;
    let exp = payload.get("exp")?.as_i64()?;
    let isa = payload
        .get("isa")
        .and_then(|v| v.as_i64())
        .or_else(|| payload.get("iat").and_then(|v| v.as_i64()))?;
    let now = chrono::Utc::now().timestamp();
    Some(now + (exp - isa))
}

pub fn is_access_expired(token: &str) -> bool {
    match access_expiry(token) {
        Some(exp) => exp <= chrono::Utc::now().timestamp(),
        None => true,
    }
}

/// Absolute unix timestamp for refresh_token expiry.
///
/// Thin wrapper over [`oray_core::auth::refresh_expires_at`]: the protocol
/// layer owns the interpretation of the ambiguous `refresh_expires` /
/// `refresh_ttl` fields (this layer used to duplicate the heuristic); this
/// side only supplies the current clock.
pub fn refresh_expiry(resp: &AuthResponse) -> i64 {
    oray_core::auth::refresh_expires_at(resp, chrono::Utc::now().timestamp())
}

pub fn jwt_payload(token: &str) -> Option<serde_json::Value> {
    let part = token.split('.').nth(1)?;
    let bytes = base64::engine::general_purpose::URL_SAFE_NO_PAD
        .decode(part)
        .ok()?;
    serde_json::from_slice(&bytes).ok()
}

pub fn human_time(ts: i64) -> String {
    chrono::DateTime::from_timestamp(ts, 0)
        .map(|dt| dt.to_rfc3339())
        .unwrap_or_else(|| ts.to_string())
}

/// Return a usable token, refreshing (and persisting) if needed. When `force`
/// is set the access token is refreshed even if the local expiry check passes
/// (used to recover from a server-side `TOKEN_EXPIRED`).
pub fn ensure_token(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    force: bool,
) -> Result<Token> {
    if cfg.account.is_none() {
        bail!("no account configured; run `oray-tools auth login`");
    }
    let current = cfg.token.clone().unwrap_or_default();
    if !force && !current.access_token.is_empty() && !is_access_expired(&current.access_token) {
        return Ok(current);
    }
    if current.refresh_token.is_empty() {
        bail!("no valid access token and no refresh token; run `oray-tools auth login`");
    }
    let server = cfg.server();
    let api = AuthApi::new(http.clone(), &server.api_base);
    // One shared resolver for every command: `--clientid` is written into
    // `cfg.client` before dispatch (see `main::run`), so the automatic
    // refresh of wakeup/remote uses the same client id as `auth login`.
    let cid = crate::support::resolve_clientid(cfg, None);
    let refreshed = crate::support::traced(
        "refresh access token",
        api.refresh(&cid, &current.access_token, &current.refresh_token),
    )?;
    let expiry = refresh_expiry(&refreshed);
    let token = Token {
        access_token: refreshed.access_token,
        refresh_token: refreshed.refresh_token,
        refresh_expires: expiry,
    };
    cfg.token = Some(token.clone());
    cfg.save(path)?;
    Ok(token)
}

#[cfg(test)]
mod tests {
    use super::*;
    use oray_core::auth::RefreshExpires;

    fn resp(raw: &str) -> AuthResponse {
        serde_json::from_str(raw).unwrap()
    }

    const TEN_YEARS: i64 = 10 * 365 * 24 * 3600;
    const THIRTY_DAYS: i64 = 30 * 24 * 3600;

    /// `refresh_expiry` reads the clock itself, so a relative expectation can
    /// only be bounded: the value must be `before + offset ..= after + offset`
    /// for the two clock reads around the call.
    fn assert_relative(resp: &AuthResponse, offset: i64) {
        let before = chrono::Utc::now().timestamp();
        let got = refresh_expiry(resp);
        let after = chrono::Utc::now().timestamp();
        assert!(
            before + offset <= got && got <= after + offset,
            "expected now {offset}, got {got} (before {before}, after {after})"
        );
    }

    /// A numeric `refresh_expires` is an absolute timestamp, clock-free.
    #[test]
    fn refresh_expiry_number_field_is_absolute() {
        let r = resp(r#"{"access_token":"a","refresh_token":"b","refresh_expires":1790000000}"#);
        assert_eq!(refresh_expiry(&r), 1_790_000_000);
    }

    /// … and so is its numeric-string form.
    #[test]
    fn refresh_expiry_numeric_string_is_absolute() {
        let r = resp(r#"{"access_token":"a","refresh_token":"b","refresh_expires":"1790000000"}"#);
        assert_eq!(refresh_expiry(&r), 1_790_000_000);
    }

    /// A login-shaped `refresh_ttl` is a TTL in seconds.
    #[test]
    fn refresh_expiry_ttl_is_relative_to_now() {
        let r = resp(r#"{"access_token":"a","refresh_token":"b","refresh_ttl":2592000}"#);
        assert_relative(&r, 2_592_000);
    }

    /// Exactly 10 years of seconds is still a TTL; one second more is read
    /// as an absolute timestamp.
    #[test]
    fn refresh_expiry_ttl_threshold_between_relative_and_absolute() {
        let at = resp(&format!(
            r#"{{"access_token":"a","refresh_token":"b","refresh_ttl":{TEN_YEARS}}}"#
        ));
        assert_relative(&at, TEN_YEARS);

        let above = resp(&format!(
            r#"{{"access_token":"a","refresh_token":"b","refresh_ttl":{}}}"#,
            TEN_YEARS + 1
        ));
        assert_eq!(refresh_expiry(&above), TEN_YEARS + 1);
    }

    /// No expiry information at all: 30 days from now.
    #[test]
    fn refresh_expiry_defaults_to_thirty_days() {
        let r = resp(r#"{"access_token":"a","refresh_token":"b"}"#);
        assert_relative(&r, THIRTY_DAYS);
    }

    /// `refresh_expires: null` degrades to "unknown" instead of failing the
    /// response, and the 30-day default applies.
    #[test]
    fn refresh_expiry_null_field_degrades_to_default() {
        let r = resp(r#"{"access_token":"a","refresh_token":"b","refresh_expires":null}"#);
        assert_eq!(r.refresh_expires, RefreshExpires::Unknown);
        assert_relative(&r, THIRTY_DAYS);
    }
}
