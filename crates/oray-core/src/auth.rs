use crate::Error;
use crate::trace::{self, Exchange, RawResult, Traced, TracedError, TracedResult};
use reqwest::blocking::Client;
use serde::Deserialize;
use serde_json::json;

pub const ACCOUNT_TYPE: &str = "password";
pub const APP_ID: &str = "kNUC97u86Zr7mt9xeZVl";
/// Salt used to compute the sendcode `checksum`.
const CHECKSUM_SALT: &str = "sunlogin.oray.com";
const USER_AGENT: &str = "SLCC/15.5.8.83635 (Android)";

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

    fn send(
        &self,
        clientid: &str,
        method: &'static str,
        url: &str,
        token: Option<&str>,
        body: Option<String>,
    ) -> RawResult<Exchange> {
        let mut headers = vec![
            ("User-Agent".into(), USER_AGENT.into()),
            ("X-Channel".into(), "OPPO".into()),
            ("X-AppID".into(), APP_ID.into()),
            ("EX-ClientId".into(), clientid.into()),
            ("Country-Region".into(), "zh-Hans_US".into()),
            ("Accept-Language".into(), "zh-Hans_US".into()),
            ("Accept".into(), "*/*".into()),
        ];
        if let Some(tok) = token {
            headers.push(("Authorization".into(), format!("Bearer {tok}")));
        }
        if body.is_some() {
            headers.push((
                "Content-Type".into(),
                "application/json; charset=utf-8".into(),
            ));
        }
        trace::execute(
            &self.client,
            trace::Request {
                method,
                url: url.to_string(),
                headers,
                body,
            },
        )
    }

    /// Check a response that should be `2xx` with no payload, carrying its
    /// exchange on both paths.
    fn expect_ok(ex: Exchange, what: &'static str) -> TracedResult<()> {
        if !(200..300).contains(&ex.status) {
            return Err(TracedError {
                error: Error::HttpStatus {
                    what,
                    status: ex.status,
                    body: ex.text,
                },
                calls: vec![ex.log],
            });
        }
        Ok(Traced {
            data: (),
            calls: vec![ex.log],
        })
    }

    /// Authenticate with `password` already md5-hashed (hex, lowercase).
    pub fn login(
        &self,
        clientid: &str,
        account: &str,
        password_md5: &str,
    ) -> TracedResult<LoginOutcome> {
        let url = format!("{}/authorization", self.api_base);
        let body = json!({
            "type": ACCOUNT_TYPE,
            "account": account,
            "password": password_md5,
            "ismd5": true,
            "oaid": "",
            "getui": "",
            "umeng": "",
        })
        .to_string();
        let ex = self.send(clientid, "POST", &url, None, Some(body))?;
        if ex.status == 202 {
            let alert: NewDeviceAlert = match serde_json::from_str(&ex.text)
                .map_err(|e| Error::bad_body(ex.text.clone(), e))
            {
                Ok(a) => a,
                Err(error) => {
                    return Err(TracedError {
                        error,
                        calls: vec![ex.log],
                    });
                }
            };
            return Ok(Traced {
                data: LoginOutcome::NewDevice(alert),
                calls: vec![ex.log],
            });
        }
        if !(200..300).contains(&ex.status) {
            return Err(TracedError {
                error: Error::HttpStatus {
                    what: "login",
                    status: ex.status,
                    body: ex.text,
                },
                calls: vec![ex.log],
            });
        }
        match serde_json::from_str::<AuthResponse>(&ex.text)
            .map_err(|e| Error::bad_body(ex.text, e))
        {
            Ok(tokens) => Ok(Traced {
                data: LoginOutcome::Tokens(tokens),
                calls: vec![ex.log],
            }),
            Err(error) => Err(TracedError {
                error,
                calls: vec![ex.log],
            }),
        }
    }

    /// Request an SMS verification code to register the current client as a
    /// trusted device. `checksum = md5(account + method + t + salt)`.
    pub fn sendcode(&self, clientid: &str, account: &str) -> TracedResult<()> {
        let url = format!("{}/login-terminals/sendcode", self.api_base);
        let t = chrono::Utc::now().timestamp_millis().to_string();
        let checksum = md5_hex(&format!("{account}mobile{t}{CHECKSUM_SALT}"));
        let body = json!({
            "account": account,
            "method": "mobile",
            "t": t,
            "checksum": checksum,
        })
        .to_string();
        let ex = self.send(clientid, "POST", &url, None, Some(body))?;
        Self::expect_ok(ex, "sendcode")
    }

    /// Submit the SMS verification code, registering the clientid as trusted.
    pub fn checkcode(
        &self,
        clientid: &str,
        account: &str,
        code: &str,
        terminal_name: &str,
    ) -> TracedResult<()> {
        let url = format!("{}/login-terminals/checkcode", self.api_base);
        let body = json!({
            "terminal_name": terminal_name,
            "account": account,
            "method": "mobile",
            "code": code,
            "memo": "",
        })
        .to_string();
        let ex = self.send(clientid, "PUT", &url, None, Some(body))?;
        Self::expect_ok(ex, "checkcode")
    }

    /// Exchange refresh_token (+ access_token) for fresh tokens.
    pub fn refresh(
        &self,
        clientid: &str,
        access_token: &str,
        refresh_token: &str,
    ) -> TracedResult<AuthResponse> {
        let url = format!("{}/authorize/refreshing", self.api_base);
        let body = json!({ "refresh_token": refresh_token }).to_string();
        let ex = self.send(clientid, "POST", &url, Some(access_token), Some(body))?;
        if !(200..300).contains(&ex.status) {
            return Err(TracedError {
                error: Error::HttpStatus {
                    what: "refresh",
                    status: ex.status,
                    body: ex.text,
                },
                calls: vec![ex.log],
            });
        }
        match serde_json::from_str::<AuthResponse>(&ex.text)
            .map_err(|e| Error::bad_body(ex.text, e))
        {
            Ok(tokens) => Ok(Traced {
                data: tokens,
                calls: vec![ex.log],
            }),
            Err(error) => Err(TracedError {
                error,
                calls: vec![ex.log],
            }),
        }
    }
}

/// md5 (lowercase hex) of a string.
pub fn md5_hex(input: &str) -> String {
    use md5::{Digest, Md5};
    let mut hasher = Md5::new();
    hasher.update(input.as_bytes());
    hex::encode(hasher.finalize())
}
