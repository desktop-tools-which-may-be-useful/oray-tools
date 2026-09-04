use crate::output::{log_auth_header, log_request, log_response};
use crate::{Error, Result};
use reqwest::blocking::{Client, Response};
use serde::{Deserialize, Serialize};
use std::collections::HashMap;

/// Thin wrapper over the Oray smart-plug HTTP endpoints on `slapi.oray.net`.
/// Stateless: callers own the access token and plug identity.
///
/// The `reqwest` client is injected so callers control timeouts, proxies,
/// connection reuse and test doubles; `clone()` is cheap (shared connection
/// pool), so reuse a single client across all API calls.
pub struct PlugApi {
    client: Client,
    slapi_base: String,
}

fn result_err(what: &str, code: i64, message: Option<&str>) -> Error {
    Error::from_message(format!(
        "{what} failed (code={code}) {}",
        message.unwrap_or("")
    ))
}

/// Build a `GET /plug?...` url from query pairs. Values are percent-encoded.
fn plug_url(slapi_base: &str, params: &[(&str, &str)]) -> Result<String> {
    let base = format!("{slapi_base}/plug");
    let mut url =
        reqwest::Url::parse(&base).map_err(|e| Error::Api(format!("invalid slapi base: {e}")))?;
    url.query_pairs_mut()
        .extend_pairs(params.iter().map(|(k, v)| (*k, *v)));
    Ok(url.to_string())
}

/// Result of a plug state query. `result == 0` means success.
#[derive(Debug, Deserialize, Serialize)]
pub struct PlugStatusResp {
    pub result: i32,
    #[serde(default)]
    pub response: Option<Vec<PlugStatus>>,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub led: Option<u32>,
    #[serde(default)]
    pub def_st: Option<u32>,
}

#[derive(Debug, PartialEq, Eq, Deserialize, Serialize)]
pub struct PlugStatus {
    pub index: i32,
    pub status: i32,
    #[serde(default)]
    pub action: i32,
}

/// Generic acknowledgement; some endpoints also return `timer_id`.
#[derive(Debug, Deserialize, Serialize)]
pub struct SetResp {
    pub result: i32,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub timer_id: Option<u64>,
}

/// Firmware version info.
#[derive(Debug, Deserialize, Serialize)]
pub struct PlugVersionResp {
    pub result: i32,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub version: Option<String>,
    #[serde(default)]
    pub version_num: Option<u32>,
}

/// WiFi info.
#[derive(Debug, Deserialize, Serialize)]
pub struct PlugWifiResp {
    pub result: i32,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub ip: Option<String>,
    #[serde(default)]
    pub mask: Option<String>,
    #[serde(default)]
    pub gw: Option<String>,
    #[serde(default)]
    pub ssid: Option<String>,
}

/// Capability list, e.g. `{"bluetooth":1,"switch":1,"timer":1,...}`.
#[derive(Debug, Deserialize, Serialize)]
pub struct FuncListResp {
    pub result: i32,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub func_list: HashMap<String, u32>,
}

/// One timer entry as returned by `plug_timer_get` / accepted by add/set.
#[derive(Debug, Clone, Default, PartialEq, Eq, Deserialize, Serialize)]
pub struct PlugTimer {
    #[serde(default)]
    pub timer_id: Option<u64>,
    #[serde(default)]
    pub time: Option<u64>,
    #[serde(default)]
    pub action: Option<u8>,
    #[serde(default)]
    pub repeat: Option<u8>,
    #[serde(default)]
    pub enabled: Option<u8>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct TimerListResp {
    pub result: i32,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub timer: Vec<PlugTimer>,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct CntdownResp {
    pub result: i32,
    #[serde(default)]
    pub message: Option<String>,
    #[serde(default)]
    pub index: Option<u32>,
    #[serde(default)]
    pub action: Option<u8>,
    #[serde(default)]
    pub count: Option<u64>,
    #[serde(default)]
    pub remain: Option<u64>,
}

/// One status-log entry.
#[derive(Debug, Deserialize, Serialize)]
pub struct StatusLog {
    pub event: String,
    #[serde(default)]
    pub status: i32,
    #[serde(default)]
    pub index: i32,
    #[serde(default)]
    pub createtime: i64,
    #[serde(default)]
    pub createtime_format: String,
}

/// `data` payload of `POST /smart-plug/get-status-logs`.
#[derive(Debug, Deserialize, Serialize)]
pub struct StatusLogsData {
    #[serde(default)]
    pub logs: Vec<StatusLog>,
    #[serde(default)]
    pub currentpage: u32,
    #[serde(default)]
    pub totalpage: u32,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub sn: String,
    #[serde(default)]
    pub count: u32,
}

/// Wrapper for the `/smart-plug/*` JSON endpoints
/// (`code`/`message`/`data`/`category`/`action`).
#[derive(Debug, Deserialize)]
pub struct SlResp<T> {
    pub code: i32,
    #[serde(default)]
    pub message: Option<String>,
    pub data: Option<T>,
}

/// Parse an expected-JSON response body, recognizing Oray XML error documents
/// (e.g. `TOKEN_EXPIRED`) as their proper `Error` variant.
fn parse_json<T: serde::de::DeserializeOwned>(text: &str, _what: &str) -> Result<T> {
    serde_json::from_str(text).map_err(|e| Error::bad_body(text.to_string(), e))
}

impl PlugApi {
    /// Wrap an injected client for the SLAPI base (e.g. `https://slapi.oray.net`).
    pub fn new(client: Client, slapi_base: &str) -> Self {
        Self {
            client,
            slapi_base: slapi_base.trim_end_matches('/').to_string(),
        }
    }

    fn send_checked(&self, token: &str, method: &str, url: &str) -> Result<(u16, String)> {
        log_request(method, url);
        let rb = self
            .client
            .request(
                reqwest::Method::from_bytes(method.as_bytes()).unwrap_or(reqwest::Method::GET),
                url,
            )
            .bearer_auth(token)
            .header("Accept", "application/json")
            .header("User-Agent", crate::USER_AGENT)
            .header("X-Channel", "OPPO")
            .header("Country-Region", "CN");
        log_auth_header("", token);
        let resp: Response = rb.send()?;
        let status = resp.status().as_u16();
        let text = resp.text()?;
        log_response(status, &text);
        if status < 200 || status >= 300 {
            return Err(Error::HttpStatus {
                what: "plug request",
                status,
                body: text,
            });
        }
        Ok((status, text))
    }

    fn post_form_checked(
        &self,
        token: &str,
        url: &str,
        form: &[(&str, &str)],
    ) -> Result<(u16, String)> {
        log_request("POST", url);
        let rb = self
            .client
            .post(url)
            .bearer_auth(token)
            .header("Accept", "application/json")
            .header("User-Agent", crate::USER_AGENT)
            .header("X-Channel", "OPPO")
            .header("Country-Region", "CN");
        log_auth_header("", token);
        let resp = rb.form(form).send()?;
        let status = resp.status().as_u16();
        let text = resp.text()?;
        log_response(status, &text);
        if status < 200 || status >= 300 {
            return Err(Error::HttpStatus {
                what: "plug request",
                status,
                body: text,
            });
        }
        Ok((status, text))
    }

    fn check_result(what: &'static str, result: i32, message: Option<&str>) -> Result<()> {
        if result == 0 || (what == "plug_cntdown_del" && result == 11) {
            Ok(())
        } else {
            Err(result_err(what, result.into(), message))
        }
    }

    /// Query the current status of one outlet (`index`, default 0 = master).
    pub fn get_status(&self, token: &str, sn: &str, index: usize) -> Result<PlugStatusResp> {
        let url = plug_url(
            &self.slapi_base,
            &[
                ("sn", sn),
                ("_api", "get_plug_status"),
                ("index", &index.to_string()),
            ],
        )?;
        let (_, text) = self.send_checked(token, "GET", &url)?;
        let parsed: PlugStatusResp = parse_json(&text, "get_plug_status")?;
        Self::check_result("get_plug_status", parsed.result, parsed.message.as_deref())?;
        Ok(parsed)
    }

    /// Turn an outlet on/off (`on`). `index` 0 = master switch.
    pub fn set_status(&self, token: &str, sn: &str, index: usize, on: bool) -> Result<SetResp> {
        let st = if on { "1" } else { "0" };
        let url = plug_url(
            &self.slapi_base,
            &[
                ("sn", sn),
                ("index", &index.to_string()),
                ("status", st),
                ("_api", "set_plug_status"),
            ],
        )?;
        let (_, text) = self.send_checked(token, "GET", &url)?;
        let parsed: SetResp = parse_json(&text, "set_plug_status")?;
        Self::check_result("set_plug_status", parsed.result, parsed.message.as_deref())?;
        Ok(parsed)
    }

    /// Query the firmware version.
    pub fn get_version(&self, token: &str, sn: &str) -> Result<PlugVersionResp> {
        let url = plug_url(
            &self.slapi_base,
            &[("sn", sn), ("_api", "get_plug_version")],
        )?;
        let (_, text) = self.send_checked(token, "GET", &url)?;
        let parsed: PlugVersionResp = parse_json(&text, "get_plug_version")?;
        Self::check_result("get_plug_version", parsed.result, parsed.message.as_deref())?;
        Ok(parsed)
    }

    /// Query WiFi info.
    pub fn get_wifi(&self, token: &str, sn: &str) -> Result<PlugWifiResp> {
        let url = plug_url(&self.slapi_base, &[("sn", sn), ("_api", "get_plug_wifi")])?;
        let (_, text) = self.send_checked(token, "GET", &url)?;
        let parsed: PlugWifiResp = parse_json(&text, "get_plug_wifi")?;
        Self::check_result("get_plug_wifi", parsed.result, parsed.message.as_deref())?;
        Ok(parsed)
    }

    /// Query the supported feature set.
    pub fn get_func_list(&self, token: &str, sn: &str) -> Result<FuncListResp> {
        let url = plug_url(&self.slapi_base, &[("sn", sn), ("_api", "get_func_list")])?;
        let (_, text) = self.send_checked(token, "GET", &url)?;
        let parsed: FuncListResp = parse_json(&text, "get_func_list")?;
        Self::check_result("get_func_list", parsed.result, parsed.message.as_deref())?;
        Ok(parsed)
    }

    /// List the timers configured for one outlet.
    pub fn timer_list(&self, token: &str, sn: &str, index: usize) -> Result<TimerListResp> {
        let url = plug_url(
            &self.slapi_base,
            &[
                ("sn", sn),
                ("_api", "plug_timer_get"),
                ("index", &index.to_string()),
            ],
        )?;
        let (_, text) = self.send_checked(token, "GET", &url)?;
        let parsed: TimerListResp = parse_json(&text, "plug_timer_get")?;
        Self::check_result("plug_timer_get", parsed.result, parsed.message.as_deref())?;
        Ok(parsed)
    }

    /// Add a timer. `time` is a duration (minutes), `action` the resulting
    /// state (0 = off, 1 = on), `repeat` a bitmask of weekdays (0 = once),
    /// `enabled` whether the timer starts active.
    pub fn timer_add(
        &self,
        token: &str,
        sn: &str,
        index: usize,
        timer: &PlugTimer,
    ) -> Result<SetResp> {
        let timer_json = serde_json::json!({
            "time": timer.time.unwrap_or(0),
            "action": timer.action.unwrap_or(1),
            "repeat": timer.repeat.unwrap_or(0),
            "enabled": timer.enabled.unwrap_or(1),
        })
        .to_string();
        let url = plug_url(
            &self.slapi_base,
            &[
                ("sn", sn),
                ("_api", "plug_timer_add"),
                ("index", &index.to_string()),
                ("timer", &timer_json),
            ],
        )?;
        let (_, text) = self.send_checked(token, "GET", &url)?;
        let parsed: SetResp = parse_json(&text, "plug_timer_add")?;
        Self::check_result("plug_timer_add", parsed.result, parsed.message.as_deref())?;
        Ok(parsed)
    }

    /// Enable or disable a timer, preserving its other settings. `action` is
    /// the timer's stored resulting state (0 = off, 1 = on); it must be passed
    /// through so a toggle does not rewrite the action.
    pub fn timer_set(
        &self,
        token: &str,
        sn: &str,
        index: usize,
        timer_id: u64,
        enabled: bool,
        action: u8,
        repeat: u8,
        time: u64,
    ) -> Result<SetResp> {
        let url = plug_url(
            &self.slapi_base,
            &[
                ("sn", sn),
                ("timer_id", &timer_id.to_string()),
                ("enabled", if enabled { "1" } else { "0" }),
                ("action", &action.to_string()),
                ("repeat", &repeat.to_string()),
                ("index", &index.to_string()),
                ("timer", &time.to_string()),
                ("_api", "plug_timer_set"),
            ],
        )?;
        let (_, text) = self.send_checked(token, "GET", &url)?;
        let parsed: SetResp = parse_json(&text, "plug_timer_set")?;
        Self::check_result("plug_timer_set", parsed.result, parsed.message.as_deref())?;
        Ok(parsed)
    }

    /// Delete a timer. `repeat`/`time` identify the timer alongside `timer_id`.
    pub fn timer_del(
        &self,
        token: &str,
        sn: &str,
        index: usize,
        timer_id: u64,
        repeat: u8,
        time: u64,
    ) -> Result<SetResp> {
        let url = plug_url(
            &self.slapi_base,
            &[
                ("sn", sn),
                ("timer_id", &timer_id.to_string()),
                ("repeat", &repeat.to_string()),
                ("index", &index.to_string()),
                ("timer", &time.to_string()),
                ("_api", "plug_timer_del"),
            ],
        )?;
        let (_, text) = self.send_checked(token, "GET", &url)?;
        let parsed: SetResp = parse_json(&text, "plug_timer_del")?;
        Self::check_result("plug_timer_del", parsed.result, parsed.message.as_deref())?;
        Ok(parsed)
    }

    /// Query the running countdown for one outlet.
    pub fn cntdown_get(&self, token: &str, sn: &str, index: usize) -> Result<CntdownResp> {
        let url = plug_url(
            &self.slapi_base,
            &[
                ("sn", sn),
                ("_api", "plug_cntdown_get"),
                ("index", &index.to_string()),
            ],
        )?;
        let (_, text) = self.send_checked(token, "GET", &url)?;
        let parsed: CntdownResp = parse_json(&text, "plug_cntdown_get")?;
        Self::check_result("plug_cntdown_get", parsed.result, parsed.message.as_deref())?;
        Ok(parsed)
    }

    /// Start a countdown that flips the outlet after `count` seconds. `action`
    /// is the resulting state (0 = off, 1 = on).
    pub fn cntdown_start(
        &self,
        token: &str,
        sn: &str,
        index: usize,
        action: u8,
        count: u64,
    ) -> Result<SetResp> {
        let url = plug_url(
            &self.slapi_base,
            &[
                ("sn", sn),
                ("_api", "plug_cntdown_add"),
                ("action", &action.to_string()),
                ("count", &count.to_string()),
                ("index", &index.to_string()),
            ],
        )?;
        let (_, text) = self.send_checked(token, "GET", &url)?;
        let parsed: SetResp = parse_json(&text, "plug_cntdown_add")?;
        Self::check_result("plug_cntdown_add", parsed.result, parsed.message.as_deref())?;
        Ok(parsed)
    }

    /// Stop any running countdown for one outlet.
    pub fn cntdown_stop(&self, token: &str, sn: &str, index: usize) -> Result<SetResp> {
        let url = plug_url(
            &self.slapi_base,
            &[
                ("sn", sn),
                ("_api", "plug_cntdown_del"),
                ("index", &index.to_string()),
            ],
        )?;
        let (_, text) = self.send_checked(token, "GET", &url)?;
        let parsed: SetResp = parse_json(&text, "plug_cntdown_del")?;
        Self::check_result("plug_cntdown_del", parsed.result, parsed.message.as_deref())?;
        Ok(parsed)
    }

    /// Turn the LED indicator on/off.
    pub fn set_led(&self, token: &str, sn: &str, enabled: bool) -> Result<SetResp> {
        let url = plug_url(
            &self.slapi_base,
            &[
                ("sn", sn),
                ("_api", "set_plug_led"),
                ("enabled", if enabled { "1" } else { "0" }),
            ],
        )?;
        let (_, text) = self.send_checked(token, "GET", &url)?;
        let parsed: SetResp = parse_json(&text, "set_plug_led")?;
        Self::check_result("set_plug_led", parsed.result, parsed.message.as_deref())?;
        Ok(parsed)
    }

    /// Set the outlet state after a power loss (`default`). Meaning of values
    /// follows the plug firmware (e.g. 0 = off, 2 = keep last state).
    pub fn set_dfltstat(&self, token: &str, sn: &str, default: u32) -> Result<SetResp> {
        let url = plug_url(
            &self.slapi_base,
            &[
                ("sn", sn),
                ("_api", "set_plug_dfltstat"),
                ("default", &default.to_string()),
            ],
        )?;
        let (_, text) = self.send_checked(token, "GET", &url)?;
        let parsed: SetResp = parse_json(&text, "set_plug_dfltstat")?;
        Self::check_result(
            "set_plug_dfltstat",
            parsed.result,
            parsed.message.as_deref(),
        )?;
        Ok(parsed)
    }

    /// Fetch a page of status-change logs.
    pub fn status_logs(&self, token: &str, sn: &str, page: u32) -> Result<StatusLogsData> {
        let url = format!("{}/smart-plug/get-status-logs", self.slapi_base);
        let (_, text) = self.post_form_checked(
            token,
            &url,
            &[("sn", sn), ("page", &page.to_string()), ("_format", "json")],
        )?;
        let parsed: SlResp<StatusLogsData> = parse_json(&text, "get-status-logs")?;
        if parsed.code != 0 {
            return Err(result_err(
                "get-status-logs",
                parsed.code.into(),
                parsed.message.as_deref(),
            ));
        }
        parsed
            .data
            .ok_or_else(|| Error::Api("get-status-logs returned no data".into()))
    }

    /// Rename a plug and/or update its memo (`description`). The endpoint
    /// always updates both, so callers should supply the current values for
    /// the field they are not changing. The `data` payload may be an empty
    /// array, so only the wrapper `code` is checked.
    pub fn rename_device(
        &self,
        token: &str,
        sn: &str,
        name: &str,
        description: &str,
    ) -> Result<()> {
        let url = format!("{}/smart-plug/rename", self.slapi_base);
        let (_, text) = self.post_form_checked(
            token,
            &url,
            &[
                ("sn", sn),
                ("name", name),
                ("description", description),
                ("_format", "json"),
            ],
        )?;
        let parsed: SlResp<serde_json::Value> = parse_json(&text, "rename")?;
        if parsed.code != 0 {
            return Err(result_err(
                "rename",
                parsed.code.into(),
                parsed.message.as_deref(),
            ));
        }
        Ok(())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_plug_status() {
        let json =
            r#"{"response":[{"action":0,"index":0,"status":1}],"led":1,"def_st":2,"result":0}"#;
        let parsed: PlugStatusResp = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.result, 0);
        assert_eq!(parsed.led, Some(1));
        assert_eq!(parsed.def_st, Some(2));
        assert_eq!(
            parsed.response.as_ref().unwrap()[0],
            PlugStatus {
                index: 0,
                status: 1,
                action: 0
            }
        );
    }

    #[test]
    fn parse_plug_version() {
        let json = r#"{"version":"1.0.3","version_num":0,"develop_num":"0","result":0}"#;
        let parsed: PlugVersionResp = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.version.as_deref(), Some("1.0.3"));
    }

    #[test]
    fn parse_func_list() {
        let json = r#"{"func_list":{"bluetooth":1,"delayer":1,"led_set":1,"switch":1,"timer":1,"wifi":1},"result":0}"#;
        let parsed: FuncListResp = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.func_list.get("timer"), Some(&1));
    }

    #[test]
    fn parse_empty_timer_list() {
        let json = r#"{"result":0,"timer":[]}"#;
        let parsed: TimerListResp = serde_json::from_str(json).unwrap();
        assert!(parsed.timer.is_empty());
    }

    #[test]
    fn parse_timer_add() {
        let json = r#"{"result":0,"timer_id":5555000001}"#;
        let parsed: SetResp = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.timer_id, Some(5555000001));
    }

    #[test]
    fn parse_cntdown_get_active() {
        let json = r#"{"index":0,"count":600,"remain":570,"action":0,"result":0}"#;
        let parsed: CntdownResp = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.count, Some(600));
        assert_eq!(parsed.remain, Some(570));
    }

    #[test]
    fn parse_cntdown_get_idle() {
        let json = r#"{"result":0,"count":0}"#;
        let parsed: CntdownResp = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.count, Some(0));
        assert!(parsed.remain.is_none());
    }

    #[test]
    fn parse_status_logs() {
        let json = r#"{"code":0,"message":"SUCCESS","stdcode":0,"data":{"logs":[{"event":"on","status":1,"createtime":1788314204,"createtime_format":"2026-09-02 09:56:44","index":0}],"currentpage":1,"totalpage":3,"name":"demo plug","sn":"100000000001","count":50},"category":"smartplug","action":"getstatuslogs"}"#;
        let parsed: SlResp<StatusLogsData> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.code, 0);
        let data = parsed.data.unwrap();
        assert_eq!(data.logs.len(), 1);
        assert_eq!(data.totalpage, 3);
        assert_eq!(data.logs[0].event, "on");
    }

    #[test]
    fn parse_rename_empty_data_array() {
        // `/smart-plug/rename` returns `data: []` on success.
        let json = r#"{"code":0,"message":"SUCCESS","stdcode":0,"data":[],"category":"smartplug","action":"rename"}"#;
        let parsed: SlResp<serde_json::Value> = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.code, 0);
        assert_eq!(parsed.data, Some(serde_json::Value::Array(vec![])));
    }

    #[test]
    fn url_encodes_timer_json() {
        let url = plug_url(
            "https://slapi.oray.net",
            &[
                ("sn", "100000000001"),
                ("_api", "plug_timer_add"),
                ("index", "0"),
                ("timer", r#"{"time":300,"action":1,"repeat":0,"enabled":1}"#),
            ],
        )
        .unwrap();
        assert!(url.starts_with("https://slapi.oray.net/plug?"));
        assert!(url.contains("_api=plug_timer_add"));
        assert!(url.contains("%7B%22time%22%3A300"));
    }

    #[test]
    fn token_expired_xml_is_recognized() {
        let xml = r#"<?xml version="1.0" encoding="utf-8"?>
<response><category>error</category><action>error</action><code>1010</code><message>TOKEN_EXPIRED</message><datas></datas></response>"#;
        let err = serde_json::from_str::<PlugStatusResp>(xml).unwrap_err();
        let e = Error::bad_body(xml.to_string(), err);
        assert!(matches!(e, Error::TokenExpired(_)), "got {e:?}");
    }
}
