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

/// Default base URL of the shield service that sends login SMS codes.
pub const SHIELD_BASE: &str = "https://shield-api-v3.oray.com";
/// Plan (template) alias of the SMS-code client login.
pub const LOGIN_CODE_PLAN: &str = "sl-code-client-login";
/// Aliyun captcha scene the shield service expects in `aliyun_captcha_sceneid`.
pub const CAPTCHA_SCENE: &str = "1sdsal45";
/// Path of the shield send-code endpoint; also part of its `checksum`.
const SECCODE_MOBILE_PATH: &str = "/seccode/mobile";

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
    shield_base: String,
}

/// The `refresh_expires` field of an [`AuthResponse`], typed.
///
/// The service is inconsistent: the value may be absent, `null`, a JSON
/// number, a numeric string — or something unusable. Deserialization is
/// deliberately forgiving: anything that does not yield a number degrades to
/// [`RefreshExpires::Unknown`] instead of failing the whole response, so a
/// login/refresh never breaks over an expiry hint.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub enum RefreshExpires {
    /// Absent / `null` / not a usable number: fall back to `refresh_ttl`.
    #[default]
    Unknown,
    /// An absolute unix timestamp (seconds).
    Timestamp(i64),
}

impl<'de> Deserialize<'de> for RefreshExpires {
    fn deserialize<D>(deserializer: D) -> Result<Self, D::Error>
    where
        D: serde::Deserializer<'de>,
    {
        let raw = serde_json::Value::deserialize(deserializer)?;
        Ok(match raw {
            serde_json::Value::Null => RefreshExpires::Unknown,
            n if n.is_i64() || n.is_u64() => match n.as_i64() {
                Some(ts) => RefreshExpires::Timestamp(ts),
                None => RefreshExpires::Unknown,
            },
            serde_json::Value::String(s) => match s.parse::<i64>() {
                Ok(ts) => RefreshExpires::Timestamp(ts),
                Err(_) => RefreshExpires::Unknown,
            },
            _ => RefreshExpires::Unknown,
        })
    }
}

#[derive(Debug, Deserialize, Clone)]
pub struct AuthResponse {
    pub access_token: String,
    pub refresh_token: String,
    // May be absent, null, a number or a numeric string — see RefreshExpires.
    #[serde(default)]
    pub refresh_expires: RefreshExpires,
    // login response: seconds of TTL (e.g. 2592000);
    // refresh response: an absolute unix timestamp (field is misnamed).
    #[serde(default)]
    pub refresh_ttl: Option<u64>,
}

/// Absolute unix timestamp (seconds) at which `refresh_token` expires.
///
/// This is the single place that knows how ambiguous the two expiry fields
/// are: `refresh_expires` may be an absolute timestamp (number or numeric
/// string), while the misnamed `refresh_ttl` is a TTL in seconds on login
/// responses but an absolute timestamp on refresh responses. The rules, in
/// order:
///
/// 1. `refresh_expires` parses to a number → that absolute timestamp;
/// 2. `refresh_ttl` greater than 10 years of seconds → treated as absolute;
/// 3. `refresh_ttl` otherwise → `now + ttl`;
/// 4. nothing usable → `now + 30 days`.
///
/// Pure: `now` comes from the caller, so the heuristics are testable.
pub fn refresh_expires_at(resp: &AuthResponse, now: i64) -> i64 {
    if let RefreshExpires::Timestamp(ts) = resp.refresh_expires {
        return ts;
    }
    const TEN_YEARS: u64 = 10 * 365 * 24 * 3600;
    match resp.refresh_ttl {
        Some(ttl) if ttl > TEN_YEARS => ttl as i64,
        Some(ttl) => now + ttl as i64,
        None => now + 30 * 24 * 3600,
    }
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

/// Response of the shield send-code endpoint: how many codes were requested
/// for the target today and which medium delivered the last one.
#[derive(Debug, Deserialize, Clone)]
pub struct SendCodeResponse {
    #[serde(default)]
    pub request_num: Option<i64>,
    #[serde(default)]
    pub medium: Option<String>,
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
            shield_base: SHIELD_BASE.to_string(),
        }
    }

    /// Override the shield base URL used to send login SMS codes
    /// (defaults to [`SHIELD_BASE`]).
    pub fn shield_base(mut self, base: &str) -> Self {
        self.shield_base = base.trim_end_matches('/').to_string();
        self
    }

    /// Run one traced request, adding `Content-Type` when a body is present.
    fn request(
        &self,
        headers: Vec<(String, String)>,
        method: &'static str,
        url: &str,
        body: Option<String>,
    ) -> RawResult<Exchange> {
        let mut headers = headers;
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
        self.request(headers, method, url, body)
    }

    /// Shield send-code request. The shield service receives no app headers
    /// (no `X-AppID`/`EX-ClientId`) and reports `CN` as the country region —
    /// this mirrors the Sunlogin client's own request.
    fn send_shield(
        &self,
        method: &'static str,
        url: &str,
        body: Option<String>,
    ) -> RawResult<Exchange> {
        let headers: Vec<(String, String)> = [
            ("User-Agent", USER_AGENT),
            ("X-Channel", "OPPO"),
            ("Country-Region", "CN"),
            ("Accept", "*/*"),
        ]
        .into_iter()
        .map(|(name, value)| (name.to_string(), value.to_string()))
        .collect();
        self.request(headers, method, url, body)
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
        Self::auth_outcome(ex, "login")
    }

    /// Turn an `/authorization` exchange into tokens (2xx) or the new-device
    /// alert (202); every other status is an error. The exchange is carried
    /// on both paths.
    fn auth_outcome(ex: Exchange, what: &'static str) -> TracedResult<LoginOutcome> {
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
                    what,
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

    /// Send the SMS login code for `mobile` through the shield service.
    ///
    /// The request mirrors the Sunlogin client: plan `sl-code-client-login`,
    /// a 6-digit code and `checksum = md5(plan_alias + target +
    /// "/seccode/mobile" + timestamp)` (seconds). `captcha_token` is the
    /// Aliyun captcha result for scene [`CAPTCHA_SCENE`]; the shield service
    /// rejects the request without one.
    pub fn send_login_code(
        &self,
        mobile: &str,
        captcha_token: &str,
    ) -> TracedResult<SendCodeResponse> {
        let timestamp = chrono::Utc::now().timestamp();
        let body = send_login_code_body(mobile, timestamp, captcha_token);
        let url = format!("{}{SECCODE_MOBILE_PATH}", self.shield_base);
        let ex = self.send_shield("POST", &url, Some(body))?;
        if !(200..300).contains(&ex.status) {
            return Err(TracedError {
                error: Error::HttpStatus {
                    what: "send sms code",
                    status: ex.status,
                    body: ex.text,
                },
                calls: vec![ex.log],
            });
        }
        match serde_json::from_str::<SendCodeResponse>(&ex.text)
            .map_err(|e| Error::bad_body(ex.text, e))
        {
            Ok(sent) => Ok(Traced {
                data: sent,
                calls: vec![ex.log],
            }),
            Err(error) => Err(TracedError {
                error,
                calls: vec![ex.log],
            }),
        }
    }

    /// Exchange an SMS login code for tokens (`type: securecode`).
    pub fn login_with_code(
        &self,
        clientid: &str,
        account: &str,
        code: &str,
    ) -> TracedResult<LoginOutcome> {
        let url = format!("{}/authorization", self.api_base);
        let body = login_with_code_body(account, code);
        let ex = self.send(clientid, "POST", &url, None, Some(body))?;
        Self::auth_outcome(ex, "sms login")
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

/// `checksum` of the shield send-code request:
/// `md5(plan_alias + target + "/seccode/mobile" + timestamp)`, with the
/// timestamp in seconds (the same value sent in the body).
pub fn seccode_checksum(plan_alias: &str, target: &str, timestamp: i64) -> String {
    md5_hex(&format!(
        "{plan_alias}{target}{SECCODE_MOBILE_PATH}{timestamp}"
    ))
}

/// Body of `POST {shield}/seccode/mobile`: request a 6-digit SMS login code
/// for `mobile`, bound to the Aliyun captcha result of scene [`CAPTCHA_SCENE`].
pub fn send_login_code_body(mobile: &str, timestamp: i64, captcha_token: &str) -> String {
    json!({
        "target": mobile,
        "plan_alias": LOGIN_CODE_PLAN,
        "length": 6,
        "send_voice_confirm": 3,
        "brand_id": 3,
        "timestamp": timestamp,
        "checksum": seccode_checksum(LOGIN_CODE_PLAN, mobile, timestamp),
        "aliyun_captcha_response": captcha_token,
        "aliyun_captcha_sceneid": CAPTCHA_SCENE,
    })
    .to_string()
}

/// Body of `POST {api_base}/authorization` for SMS-code login: exchanges the
/// code issued for [`LOGIN_CODE_PLAN`] for access/refresh tokens.
pub fn login_with_code_body(account: &str, code: &str) -> String {
    json!({
        "type": "securecode",
        "account": account,
        "code": code,
        "medium": "sms",
        "code-type": LOGIN_CODE_PLAN,
        "oaid": "",
        "getui": "",
        "umeng": "",
    })
    .to_string()
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The formula and the timestamps come from a capture of the Sunlogin
    /// client's own request; the target is a dummy number — real phone
    /// numbers never appear in this repository.
    #[test]
    fn seccode_checksum_matches_captured_client() {
        assert_eq!(
            seccode_checksum("sl-code-client-login", "12345678901", 1790321487),
            "1e2a5b17983b451d932bd1a935362f30"
        );
        assert_eq!(
            seccode_checksum("sl-code-client-login", "12345678901", 1790321424),
            "e7338c50fe7ce28363ba40c903420d6f"
        );
    }

    #[test]
    fn send_login_code_body_fields() {
        let body = send_login_code_body("12345678901", 1790321487, "TOKEN");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["target"], "12345678901");
        assert_eq!(v["plan_alias"], LOGIN_CODE_PLAN);
        assert_eq!(v["length"], 6);
        assert_eq!(v["send_voice_confirm"], 3);
        assert_eq!(v["brand_id"], 3);
        assert_eq!(v["timestamp"], 1790321487);
        assert_eq!(v["aliyun_captcha_response"], "TOKEN");
        assert_eq!(v["aliyun_captcha_sceneid"], CAPTCHA_SCENE);
        assert_eq!(v["checksum"], "1e2a5b17983b451d932bd1a935362f30");
    }

    #[test]
    fn login_with_code_body_fields() {
        let body = login_with_code_body("12345678901", "123456");
        let v: serde_json::Value = serde_json::from_str(&body).unwrap();
        assert_eq!(v["type"], "securecode");
        assert_eq!(v["account"], "12345678901");
        assert_eq!(v["code"], "123456");
        assert_eq!(v["medium"], "sms");
        assert_eq!(v["code-type"], LOGIN_CODE_PLAN);
        assert_eq!(v["oaid"], "");
    }

    /// The securecode answer carries the same token fields as a password
    /// login (captured response shape).
    #[test]
    fn auth_response_from_securecode() {
        let raw = r#"{"access_token":"a","refresh_token":"b","refresh_ttl":1792913498,"is_recalled_user":false}"#;
        let parsed: AuthResponse = serde_json::from_str(raw).unwrap();
        assert_eq!(parsed.access_token, "a");
        assert_eq!(parsed.refresh_token, "b");
        assert_eq!(parsed.refresh_ttl, Some(1792913498));
    }

    /// A response parsed exactly like the HTTP layer parses it.
    fn resp(raw: &str) -> AuthResponse {
        serde_json::from_str(raw).unwrap()
    }

    /// Fixed clock for the pure `refresh_expires_at` heuristic.
    const NOW: i64 = 1_800_000_000;
    const TEN_YEARS: i64 = 10 * 365 * 24 * 3600;
    const THIRTY_DAYS: i64 = 30 * 24 * 3600;

    /// A numeric `refresh_expires` is an absolute timestamp.
    #[test]
    fn refresh_expires_number_is_absolute() {
        let r = resp(r#"{"access_token":"a","refresh_token":"b","refresh_expires":1790000000}"#);
        assert_eq!(r.refresh_expires, RefreshExpires::Timestamp(1_790_000_000));
        assert_eq!(refresh_expires_at(&r, NOW), 1_790_000_000);
    }

    /// So is a numeric string (`"1790000000"`).
    #[test]
    fn refresh_expires_numeric_string_is_absolute() {
        let r = resp(r#"{"access_token":"a","refresh_token":"b","refresh_expires":"1790000000"}"#);
        assert_eq!(r.refresh_expires, RefreshExpires::Timestamp(1_790_000_000));
        assert_eq!(refresh_expires_at(&r, NOW), 1_790_000_000);
    }

    /// `refresh_expires` wins over `refresh_ttl`.
    #[test]
    fn refresh_expires_beats_refresh_ttl() {
        let r = resp(
            r#"{"access_token":"a","refresh_token":"b","refresh_expires":1790000000,"refresh_ttl":60}"#,
        );
        assert_eq!(refresh_expires_at(&r, NOW), 1_790_000_000);
    }

    /// A login-shaped `refresh_ttl` is a TTL in seconds: `now + ttl`.
    #[test]
    fn refresh_ttl_is_relative() {
        let r = resp(r#"{"access_token":"a","refresh_token":"b","refresh_ttl":2592000}"#);
        assert_eq!(refresh_expires_at(&r, NOW), NOW + 2_592_000);
    }

    /// The 10-year threshold: exactly at it the value is still a TTL, one
    /// second above it it is treated as an absolute timestamp.
    #[test]
    fn refresh_ttl_threshold_between_relative_and_absolute() {
        let at = resp(&format!(
            r#"{{"access_token":"a","refresh_token":"b","refresh_ttl":{TEN_YEARS}}}"#
        ));
        assert_eq!(refresh_expires_at(&at, NOW), NOW + TEN_YEARS);

        let above = resp(&format!(
            r#"{{"access_token":"a","refresh_token":"b","refresh_ttl":{}}}"#,
            TEN_YEARS + 1
        ));
        assert_eq!(refresh_expires_at(&above, NOW), TEN_YEARS + 1);
    }

    /// Nothing usable at all: assume 30 days.
    #[test]
    fn missing_expiry_defaults_to_thirty_days() {
        let r = resp(r#"{"access_token":"a","refresh_token":"b"}"#);
        assert_eq!(r.refresh_expires, RefreshExpires::Unknown);
        assert_eq!(refresh_expires_at(&r, NOW), NOW + THIRTY_DAYS);
    }

    /// `refresh_expires: null` degrades to "unknown" and the TTL (or the
    /// 30-day default) is used instead — deserialization never fails.
    #[test]
    fn refresh_expires_null_degrades_to_ttl_fallback() {
        let with_ttl = resp(
            r#"{"access_token":"a","refresh_token":"b","refresh_expires":null,"refresh_ttl":60}"#,
        );
        assert_eq!(with_ttl.refresh_expires, RefreshExpires::Unknown);
        assert_eq!(refresh_expires_at(&with_ttl, NOW), NOW + 60);

        let without = resp(r#"{"access_token":"a","refresh_token":"b","refresh_expires":null}"#);
        assert_eq!(refresh_expires_at(&without, NOW), NOW + THIRTY_DAYS);
    }

    /// Unusable shapes (non-numeric string, object) are tolerated by the
    /// deserializer and fall back like a missing field.
    #[test]
    fn refresh_expires_garbage_is_tolerated() {
        for raw in [
            r#"{"access_token":"a","refresh_token":"b","refresh_expires":"soon"}"#,
            r#"{"access_token":"a","refresh_token":"b","refresh_expires":{"odd":true}}"#,
            r#"{"access_token":"a","refresh_token":"b","refresh_expires":true}"#,
            r#"{"access_token":"a","refresh_token":"b","refresh_expires":1.5}"#,
        ] {
            let r = resp(raw);
            assert_eq!(r.refresh_expires, RefreshExpires::Unknown, "{raw}");
            assert_eq!(refresh_expires_at(&r, NOW), NOW + THIRTY_DAYS, "{raw}");
        }
    }
}
