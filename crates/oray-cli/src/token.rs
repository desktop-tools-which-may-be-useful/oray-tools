use crate::config::{Client, Config, Token};
use anyhow::{Context, Result, bail};
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
/// `refresh_ttl` is ambiguous between responses: login returns it as a
/// TTL in seconds, refresh returns it as an absolute timestamp. A value
/// larger than 10 years of seconds is treated as an absolute epoch.
pub fn refresh_expiry(resp: &AuthResponse) -> i64 {
    let now = chrono::Utc::now().timestamp();
    if let Some(n) = resp.refresh_expires.as_i64() {
        return n;
    }
    if let Some(s) = resp.refresh_expires.as_str()
        && let Ok(n) = s.parse::<i64>()
    {
        return n;
    }
    match resp.refresh_ttl {
        Some(ttl) if ttl > 10 * 365 * 24 * 3600 => ttl as i64,
        Some(ttl) => now + ttl as i64,
        None => now + 30 * 24 * 3600,
    }
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

/// Resolve the trusted client id, generating and persisting a fresh one if
/// none is configured yet.
fn client_id(cfg: &mut Config) -> String {
    if let Some(cid) = cfg
        .client
        .as_ref()
        .map(|c| c.clientid.clone())
        .filter(|c| !c.is_empty())
    {
        return cid;
    }
    let cid = oray_core::auth::generate_client_id();
    cfg.client = Some(Client { clientid: cid.clone() });
    cid
}

/// Return a usable token, refreshing (and persisting) if needed.
pub fn ensure_token(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
) -> Result<Token> {
    if cfg.account.is_none() {
        bail!("no account configured; run `oray-tools login`");
    }
    let current = cfg.token.clone().unwrap_or_default();
    if !current.access_token.is_empty() && !is_access_expired(&current.access_token) {
        return Ok(current);
    }
    if current.refresh_token.is_empty() {
        bail!("no valid access token and no refresh token; run `oray-tools login`");
    }
    let server = cfg.server();
    let api = AuthApi::new(http.clone(), &server.api_base);
    let cid = client_id(cfg);
    let refreshed = api
        .refresh(&cid, &current.access_token, &current.refresh_token)
        .context("refresh access token")?;
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