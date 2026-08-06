use crate::config::{Config, Server, Token};
use anyhow::{Context, Result, bail};
use base64::Engine;
use reqwest::blocking::{Client, RequestBuilder};
use serde::Deserialize;
use serde_json::json;

pub const ACCOUNT_TYPE: &str = "password";
pub const APP_ID: &str = "kNUC97u86Zr7mt9xeZVl";
/// Salt used to compute the sendcode `checksum`.
pub const CHECKSUM_SALT: &str = "sunlogin.oray.com";

/// Generate a machine-local client id (UUID v4). A fresh id triggers Oray's
/// one-time SMS verification on first login, after which it is trusted.
pub fn generate_client_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

#[derive(Debug, Deserialize, Clone)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    // May be absent; when present it's a string timestamp. Be tolerant of numbers.
    #[serde(default)]
    pub refresh_expires: serde_json::Value,
    // login response: seconds of TTL (e.g. 2592000);
    // refresh response: an absolute unix timestamp (field is misnamed).
    #[serde(default)]
    pub refresh_ttl: Option<u64>,
}

#[derive(Debug, Deserialize, Clone)]
pub struct NewDeviceAlert {
    pub error: String,
    pub code: i64,
    #[serde(default)]
    pub mobile: String,
    #[serde(default)]
    pub email: String,
}

#[derive(Debug)]
pub enum LoginOutcome {
    Tokens(AuthResponse),
    NewDevice(NewDeviceAlert),
}

/// md5 (lowercase hex) of a string.
pub fn md5_hex(input: &str) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}

pub fn standard_client() -> Result<Client> {
    Client::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()
        .context("build http client")
}

fn auth_headers(rb: RequestBuilder, clientid: &str) -> RequestBuilder {
    rb.header("User-Agent", "SLCC/15.5.8.83635 (Android)")
        .header("X-Channel", "OPPO")
        .header("X-AppID", APP_ID)
        .header("EX-ClientId", clientid)
        .header("Country-Region", "zh-Hans_US")
        .header("Accept-Language", "zh-Hans_US")
        .header("Accept", "*/*")
}

pub fn login(client: &Client, server: &Server, clientid: &str, account: &str, password: &str) -> Result<LoginOutcome> {
    let base = server.api_base.trim_end_matches('/');
    let url = format!("{base}/authorization");
    let body = json!({
        "type": ACCOUNT_TYPE,
        "account": account,
        "password": md5_hex(password),
        "ismd5": true,
        "oaid": "",
        "getui": "",
        "umeng": "",
    });
    let resp = auth_headers(client.post(&url), clientid)
        .header("Content-Type", "application/json; charset=utf-8")
        .json(&body)
        .send()
        .context("send login request")?;
    let status = resp.status();
    let text = resp.text().context("read auth response body")?;
    if status.as_u16() == 202 {
        let alert: NewDeviceAlert = serde_json::from_str(&text)
            .with_context(|| format!("parse new-device alert: {text}"))?;
        return Ok(LoginOutcome::NewDevice(alert));
    }
    if !status.is_success() {
        bail!("auth request failed (HTTP {}): {}", status.as_u16(), text);
    }
    let parsed: AuthResponse = serde_json::from_str(&text)
        .with_context(|| format!("parse auth response: {text}"))?;
    Ok(LoginOutcome::Tokens(parsed))
}

/// Request an SMS verification code to register the current client as a
/// trusted device. `checksum = md5(account + method + t + salt)`.
pub fn sendcode(client: &Client, server: &Server, clientid: &str, account: &str) -> Result<()> {
    let base = server.api_base.trim_end_matches('/');
    let url = format!("{base}/login-terminals/sendcode");
    let t = chrono::Utc::now().timestamp_millis().to_string();
    let checksum = md5_hex(&format!("{account}mobile{t}{CHECKSUM_SALT}"));
    let body = json!({
        "account": account,
        "method": "mobile",
        "t": t,
        "checksum": checksum,
    });
    let resp = auth_headers(client.post(&url), clientid)
        .header("Content-Type", "application/json; charset=utf-8")
        .json(&body)
        .send()
        .context("send sendcode request")?;
    check_ok(resp, "sendcode")
}

/// Submit the SMS verification code, registering the clientid as trusted.
pub fn checkcode(
    client: &Client,
    server: &Server,
    clientid: &str,
    account: &str,
    code: &str,
    terminal_name: &str,
) -> Result<()> {
    let base = server.api_base.trim_end_matches('/');
    let url = format!("{base}/login-terminals/checkcode");
    let body = json!({
        "terminal_name": terminal_name,
        "account": account,
        "method": "mobile",
        "code": code,
        "memo": "",
    });
    let resp = auth_headers(client.put(&url), clientid)
        .header("Content-Type", "application/json; charset=utf-8")
        .json(&body)
        .send()
        .context("send checkcode request")?;
    check_ok(resp, "checkcode")
}

fn check_ok(resp: reqwest::blocking::Response, what: &str) -> Result<()> {
    let status = resp.status();
    let text = resp.text().context("read response body")?;
    if !status.is_success() {
        bail!("{what} failed (HTTP {}): {}", status.as_u16(), text);
    }
    Ok(())
}

pub fn refresh(
    client: &Client,
    server: &Server,
    clientid: &str,
    access_token: &str,
    refresh_token: &str,
) -> Result<AuthResponse> {
    let base = server.api_base.trim_end_matches('/');
    let url = format!("{base}/authorize/refreshing");
    let body = json!({ "refresh_token": refresh_token });
    let resp = auth_headers(client.post(&url), clientid)
        .bearer_auth(access_token)
        .header("Content-Type", "application/json; charset=utf-8")
        .json(&body)
        .send()
        .context("send refresh request")?;
    parse_token_resp(resp)
}

fn parse_token_resp(resp: reqwest::blocking::Response) -> Result<AuthResponse> {
    let status = resp.status();
    let text = resp.text().context("read auth response body")?;
    if !status.is_success() {
        bail!("auth request failed (HTTP {}): {}", status.as_u16(), text);
    }
    serde_json::from_str(&text).with_context(|| format!("parse auth response: {text}"))
}

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

fn client_id(cfg: &mut Config) -> String {
    if let Some(cid) = cfg
        .client
        .as_ref()
        .map(|c| c.clientid.clone())
        .filter(|c| !c.is_empty())
    {
        return cid;
    }
    let cid = generate_client_id();
    cfg.client = Some(crate::config::Client { clientid: cid.clone() });
    cid
}

/// Return a usable token, refreshing (and persisting) if needed.
pub fn ensure_token(cfg: &mut Config, path: &std::path::PathBuf) -> Result<Token> {
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
    let client = standard_client()?;
    let cid = client_id(cfg);
    let refreshed =
        refresh(&client, &server, &cid, &current.access_token, &current.refresh_token)
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