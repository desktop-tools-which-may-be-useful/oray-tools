use crate::config::{Config, Token};
use anyhow::{Result, bail};
use base64::Engine;
use oray_core::auth::{AuthApi, AuthResponse};
use reqwest::blocking::Client as HttpClient;
use std::path::PathBuf;

/// Safety skew applied by [`is_access_expired`]: a token is treated as
/// expired this many seconds *before* its `exp`, so a request launched right
/// at the boundary (or a slightly fast local clock) does not go out with an
/// already-dead token.
const EXPIRY_SKEW_SECS: i64 = 45;

/// Absolute unix timestamp at which the access token expires, read verbatim
/// from the JWT `exp` claim — standard JWT semantics: `exp` *is* the
/// absolute expiry.
///
/// The previous rule was `now + (exp - isa)`, which re-anchored the expiry
/// to the moment of the check: `is_access_expired` then reduced to
/// `exp <= isa` (false for every well-formed token, so a refresh was never
/// proactively triggered) and `auth status` displayed an expiry counted from
/// *now* instead of from the token. The `isa`/`iat` claims are no longer
/// consulted at all; a token without `exp` yields `None`, which
/// [`is_access_expired`] treats as expired so a refresh is attempted.
pub fn access_expiry(token: &str) -> Option<i64> {
    jwt_payload(token)?.get("exp")?.as_i64()
}

/// Whether the access token should be refreshed before use: its absolute
/// `exp` (from [`access_expiry`]) is at most [`EXPIRY_SKEW_SECS`] away from
/// now, or the token carries no readable expiry at all (`None => true`, so
/// an unparseable token still goes through the refresh path instead of being
/// sent to the server and failing there).
pub fn is_access_expired(token: &str) -> bool {
    match access_expiry(token) {
        Some(exp) => exp <= chrono::Utc::now().timestamp() + EXPIRY_SKEW_SECS,
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

    /// Build a JWT-shaped string (`header.payload.signature`) whose payload
    /// is `payload` JSON. Only the middle segment is ever decoded, so the
    /// header/signature contents are irrelevant — these tests are offline.
    fn jwt(payload: &str) -> String {
        let encoded = base64::engine::general_purpose::URL_SAFE_NO_PAD.encode(payload);
        format!("eyJhbGciOiJIUzI1NiJ9.{encoded}.c2lnbmF0dXJl")
    }

    /// `access_expiry` is the token's absolute `exp`, not a value re-anchored
    /// to the clock: this token really expired at 1970-01-01 00:16:40 UTC.
    #[test]
    fn access_expiry_returns_the_absolute_exp() {
        // The token from the bug report: exp=1000, isa=500. The old rule
        // displayed `now + 500` and never considered the token expired.
        assert_eq!(access_expiry(&jwt(r#"{"exp":1000,"isa":500}"#)), Some(1000));
        assert!(is_access_expired(&jwt(r#"{"exp":1000,"isa":500}"#)));
    }

    #[test]
    fn is_access_expired_false_for_far_future_exp() {
        let far = chrono::Utc::now().timestamp() + 3600;
        let token = jwt(&format!(r#"{{"exp":{far},"isa":0}}"#));
        assert_eq!(access_expiry(&token), Some(far));
        assert!(!is_access_expired(&token));
    }

    /// Inside the skew window the token counts as expired so the next request
    /// does not start with a token that dies in flight.
    #[test]
    fn is_access_expired_true_inside_the_skew_window() {
        let almost = chrono::Utc::now().timestamp() + EXPIRY_SKEW_SECS - 5;
        assert!(is_access_expired(&jwt(&format!(r#"{{"exp":{almost}}}"#))));
    }

    /// A token that cannot be parsed at all, or carries no `exp`, counts as
    /// expired (`None => true`): the refresh path must still be attempted
    /// instead of shipping a token the server will reject.
    #[test]
    fn is_access_expired_true_without_readable_expiry() {
        // No `.` segments: not a JWT.
        assert_eq!(access_expiry("not-a-jwt"), None);
        assert!(is_access_expired("not-a-jwt"));
        // Parses, but has no `exp` claim (the old code demanded `isa`/`iat`
        // too and failed the same way).
        assert_eq!(access_expiry(&jwt(r#"{"isa":500,"iat":500}"#)), None);
        assert!(is_access_expired(&jwt(r#"{"isa":500,"iat":500}"#)));
    }
}
