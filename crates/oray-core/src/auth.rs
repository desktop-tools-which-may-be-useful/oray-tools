use crate::{Error, Result};
use reqwest::blocking::{Client, RequestBuilder};
use serde::Deserialize;
use serde_json::json;

pub const ACCOUNT_TYPE: &str = "password";
pub const APP_ID: &str = "kNUC97u86Zr7mt9xeZVl";
/// Salt used to compute the sendcode `checksum`.
const CHECKSUM_SALT: &str = "sunlogin.oray.com";

/// Generate a machine-local client id (UUID v4). A fresh id triggers Oray's
/// one-time SMS verification on first login, after which it is trusted.
pub fn generate_client_id() -> String {
    uuid::Uuid::new_v4().to_string()
}

/// Thin wrapper over the Oray auth HTTP endpoints. Stateless: callers own
/// credentials, client id and token lifecycle.
///
/// The `reqwest` client is injected so callers control timeouts, proxies,
/// connection reuse and test doubles; `clone()` is cheap (shared connection
/// pool), so reuse a single client across all API calls.
pub struct AuthApi {
    client: Client,
    api_base: String,
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

impl AuthApi {
    /// Wrap an injected client for the API base
    /// (e.g. `https://api-std.sunlogin.oray.com`).
    pub fn new(client: Client, api_base: &str) -> Self {
        Self {
            client,
            api_base: api_base.trim_end_matches('/').to_string(),
        }
    }

    fn auth_headers(&self, rb: RequestBuilder, clientid: &str) -> RequestBuilder {
        rb.header("User-Agent", "SLCC/15.5.8.83635 (Android)")
            .header("X-Channel", "OPPO")
            .header("X-AppID", APP_ID)
            .header("EX-ClientId", clientid)
            .header("Country-Region", "zh-Hans_US")
            .header("Accept-Language", "zh-Hans_US")
            .header("Accept", "*/*")
    }

    /// Authenticate with `password` already md5-hashed (hex, lowercase).
    pub fn login(&self, clientid: &str, account: &str, password_md5: &str) -> Result<LoginOutcome> {
        let url = format!("{}/authorization", self.api_base);
        let body = json!({
            "type": ACCOUNT_TYPE,
            "account": account,
            "password": password_md5,
            "ismd5": true,
            "oaid": "",
            "getui": "",
            "umeng": "",
        });
        let resp = self
            .auth_headers(self.client.post(&url), clientid)
            .header("Content-Type", "application/json; charset=utf-8")
            .json(&body)
            .send()?;
        let status = resp.status();
        let text = resp.text()?;
        if status.as_u16() == 202 {
            let alert: NewDeviceAlert =
                serde_json::from_str(&text).map_err(|e| Error::bad_body(text.clone(), e))?;
            return Ok(LoginOutcome::NewDevice(alert));
        }
        if !status.is_success() {
            return Err(Error::HttpStatus {
                what: "login",
                status: status.as_u16(),
                body: text,
            });
        }
        let parsed: AuthResponse =
            serde_json::from_str(&text).map_err(|e| Error::bad_body(text.clone(), e))?;
        Ok(LoginOutcome::Tokens(parsed))
    }

    /// Request an SMS verification code to register the current client as a
    /// trusted device. `checksum = md5(account + method + t + salt)`.
    pub fn sendcode(&self, clientid: &str, account: &str) -> Result<()> {
        let url = format!("{}/login-terminals/sendcode", self.api_base);
        let t = chrono::Utc::now().timestamp_millis().to_string();
        let checksum = md5_hex(&format!("{account}mobile{t}{CHECKSUM_SALT}"));
        let body = json!({
            "account": account,
            "method": "mobile",
            "t": t,
            "checksum": checksum,
        });
        let resp = self
            .auth_headers(self.client.post(&url), clientid)
            .header("Content-Type", "application/json; charset=utf-8")
            .json(&body)
            .send()?;
        check_ok(resp, "sendcode")
    }

    /// Submit the SMS verification code, registering the clientid as trusted.
    pub fn checkcode(
        &self,
        clientid: &str,
        account: &str,
        code: &str,
        terminal_name: &str,
    ) -> Result<()> {
        let url = format!("{}/login-terminals/checkcode", self.api_base);
        let body = json!({
            "terminal_name": terminal_name,
            "account": account,
            "method": "mobile",
            "code": code,
            "memo": "",
        });
        let resp = self
            .auth_headers(self.client.put(&url), clientid)
            .header("Content-Type", "application/json; charset=utf-8")
            .json(&body)
            .send()?;
        check_ok(resp, "checkcode")
    }

    /// Exchange refresh_token (+ access_token) for fresh tokens.
    pub fn refresh(
        &self,
        clientid: &str,
        access_token: &str,
        refresh_token: &str,
    ) -> Result<AuthResponse> {
        let url = format!("{}/authorize/refreshing", self.api_base);
        let body = json!({ "refresh_token": refresh_token });
        let resp = self
            .auth_headers(self.client.post(&url), clientid)
            .bearer_auth(access_token)
            .header("Content-Type", "application/json; charset=utf-8")
            .json(&body)
            .send()?;
        parse_token_resp(resp)
    }
}

fn check_ok(resp: reqwest::blocking::Response, what: &'static str) -> Result<()> {
    let status = resp.status();
    let text = resp.text()?;
    if !status.is_success() {
        return Err(Error::HttpStatus {
            what,
            status: status.as_u16(),
            body: text,
        });
    }
    Ok(())
}

fn parse_token_resp(resp: reqwest::blocking::Response) -> Result<AuthResponse> {
    let status = resp.status();
    let text = resp.text()?;
    if !status.is_success() {
        return Err(Error::HttpStatus {
            what: "refresh",
            status: status.as_u16(),
            body: text,
        });
    }
    serde_json::from_str(&text).map_err(|e| Error::bad_body(text.clone(), e))
}

/// md5 (lowercase hex) of a string.
pub fn md5_hex(input: &str) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}
