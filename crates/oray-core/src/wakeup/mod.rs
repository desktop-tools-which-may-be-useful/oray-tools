//! 开机设备 (wakeup devices): smart plugs and other power hardware.
//!
//! Device listing lives here (`WakeupApi`); plug-specific controls live in the
//! [`plug`] submodule.

pub mod plug;

use crate::trace::{self, RawResult, RequestLog, Traced, TracedError, TracedResult};
use crate::{Error, Result};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

/// Thin wrapper over the Oray wakeup-device HTTP endpoints on
/// `api-std.sunlogin.oray.com`. These are "开机设备": smart plugs and other
/// power hardware. Stateless: callers own the access token.
///
/// The `reqwest` client is injected so callers control timeouts, proxies,
/// connection reuse and test doubles; `clone()` is cheap (shared connection
/// pool), so reuse a single client across all API calls.
pub struct WakeupApi {
    client: Client,
    api_base: String,
}

/// A wakeup device (smart plug / power hardware).
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WakeupDevice {
    pub device_id: u64,
    pub sn: String,
    #[serde(default)]
    pub mac: String,
    #[serde(default)]
    pub owner_id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(rename = "type", default)]
    pub r#type: u32,
    #[serde(default)]
    pub model: String,
    #[serde(default)]
    pub service_type: u32,
    #[serde(default)]
    pub create_time: String,
    #[serde(default)]
    pub isenable: bool,
    #[serde(default)]
    pub device_type: String,
    #[serde(default)]
    pub hardwareid: u64,
    /// Free-form memo/备注 set through `wakeup memo`.
    #[serde(default)]
    pub description: Option<String>,
    #[serde(default)]
    pub remote_ids: Vec<u64>,
    #[serde(default)]
    pub hardware_type: String,
    /// Remote devices this hardware is bound to.
    #[serde(default)]
    pub remotes: Vec<WakeupRemoteRef>,
    /// Number of switchable outlets.
    #[serde(default)]
    pub outletcount: u32,
    #[serde(default)]
    pub delays: Vec<Delay>,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct WakeupRemoteRef {
    pub remote_id: u64,
}

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Delay {
    #[serde(default)]
    pub delay: u32,
}

#[derive(Debug, Deserialize, Serialize)]
pub struct WakeupDevicesResponse {
    pub devices: Vec<WakeupDevice>,
}

/// The page cap of `/wakeup/devices`: `list` only ever asks for the first
/// page (`offset=0&limit=<here>`), so this is also the number of devices
/// [`WakeupApi::find`] can scan — both the URL and the "not found" message
/// are derived from it so they cannot drift apart.
const DEVICE_LIST_LIMIT: usize = 100;

/// Build `GET {api_base}/wakeup/devices?offset=0&limit=100[&sn=...]`, with
/// every query value percent-encoded the way
/// `application/x-www-form-urlencoded` requires (space -> `+`; `&`, `+` and
/// non-ASCII escaped). A plain `format!` splice would let an `sn` containing
/// `&` inject extra query parameters — and would disagree with the encoded
/// URLs the plug endpoints already build.
fn device_list_url(api_base: &str, sn: Option<&str>) -> Result<String> {
    let base = format!("{api_base}/wakeup/devices");
    let mut url =
        reqwest::Url::parse(&base).map_err(|e| Error::Api(format!("invalid api base: {e}")))?;
    {
        let mut query = url.query_pairs_mut();
        query
            .append_pair("offset", "0")
            .append_pair("limit", &DEVICE_LIST_LIMIT.to_string());
        if let Some(sn) = sn {
            query.append_pair("sn", sn);
        }
    }
    Ok(url.to_string())
}

/// The URL [`WakeupApi::find`] lists with: the server-side `sn` filter
/// always present, so a find never downloads an unrelated page. Kept as the
/// single place `find` builds its query so tests can pin the `sn=` pair.
fn find_list_url(api_base: &str, sn: &str) -> Result<String> {
    device_list_url(api_base, Some(sn))
}

/// The "not found" message of [`WakeupApi::find`], stating how many devices
/// were actually looked at and that the scan stops at the page cap — the
/// same honesty as `RemoteApi::find`'s "not among the first N remotes
/// listed", so an account holding more devices than the cap is not left
/// guessing why a real SN was missed.
fn find_not_found_message(sn: &str, scanned: usize) -> String {
    format!(
        "wakeup device sn={sn} not found among {scanned} devices listed (only the first \
         {DEVICE_LIST_LIMIT} are scanned)"
    )
}

impl WakeupApi {
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
        token: &str,
        what: &'static str,
        url: &str,
    ) -> RawResult<(Vec<RequestLog>, String)> {
        let ex = trace::execute(
            &self.client,
            trace::Request {
                method: "GET",
                url: url.to_string(),
                headers: trace::api_headers(token),
                body: None,
            },
        )?;
        trace::expect_2xx(ex, what)
    }

    /// List wakeup devices. Optionally filter to one SN
    /// (`/wakeup/devices?sn=<sn>`); the SN is percent-encoded.
    ///
    /// Only the **first page** is fetched (`offset=0`, `limit` being the
    /// [`DEVICE_LIST_LIMIT`] constant, 100): accounts with more devices see
    /// just that first hundred, which is also all [`find`](Self::find) can
    /// scan.
    pub fn list(&self, token: &str, sn: Option<&str>) -> TracedResult<WakeupDevicesResponse> {
        let url = device_list_url(&self.api_base, sn)?;
        self.list_at(token, &url)
    }

    /// Issue the device-list request for an already built URL (shared by
    /// [`list`](Self::list) and [`find`](Self::find), which always carries
    /// the `sn=` filter).
    fn list_at(&self, token: &str, url: &str) -> TracedResult<WakeupDevicesResponse> {
        let (calls, text) = self.send(token, "list wakeup devices", url)?;
        trace::finish(
            calls,
            serde_json::from_str(&text).map_err(|e| Error::bad_body(text, e)),
        )
    }

    /// Look up a single device by SN.
    ///
    /// The request carries the server-side `sn` filter (`list` has always
    /// supported it, but `find` used to pass `None`, which both downloaded an
    /// unrelated page and left that filter dead in production); the result is
    /// still scanned client-side below, so behaviour stays correct even if a
    /// server ignores the parameter.
    ///
    /// Like [`list`](Self::list), at most [`DEVICE_LIST_LIMIT`] devices are
    /// ever seen — a miss therefore reports how many devices were listed and
    /// that only the first 100 are scanned, mirroring `RemoteApi::find`'s
    /// "not among the first N listed" wording, instead of a bare
    /// "not found".
    pub fn find(&self, token: &str, sn: &str) -> TracedResult<WakeupDevice> {
        let url = find_list_url(&self.api_base, sn)?;
        let all = self.list_at(token, &url)?;
        let calls = all.calls;
        let devices = all.data.devices;
        let scanned = devices.len();
        match devices.into_iter().find(|d| d.sn == sn) {
            Some(device) => Ok(Traced {
                data: device,
                calls,
            }),
            None => Err(TracedError {
                error: Error::Api(find_not_found_message(sn, scanned)),
                calls,
            }),
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_wakeup_devices() {
        let json = r#"{"devices":[{"device_id":900001,"sn":"100000000001","mac":"aa:bb:cc:dd:ee:01","owner_id":600001,"name":"Demo Smart Plug","type":5,"model":"C1Pro-BLE-V3","service_type":5,"create_time":"2026-09-01 19:41:03","isenable":true,"device_type":"sl_smartplug","hardwareid":700001,"description":"demo memo","remote_ids":[800001],"hardware_type":"DEMO-PLUG","remotes":[{"remote_id":800001}],"outletcount":1,"delays":[{"delay":120}]}]}"#;
        let parsed: WakeupDevicesResponse = serde_json::from_str(json).unwrap();
        let d = &parsed.devices[0];
        assert_eq!(d.sn, "100000000001");
        assert_eq!(d.r#type, 5);
        assert_eq!(d.name, "Demo Smart Plug");
        assert_eq!(d.description.as_deref(), Some("demo memo"));
        assert_eq!(d.outletcount, 1);
        assert_eq!(d.remote_ids, vec![800001]);
    }

    #[test]
    fn parse_wakeup_devices_missing_optional() {
        // The prompt's example omits `description`; must still parse.
        let json = r#"{"devices":[{"device_id":900001,"sn":"100000000001","mac":"aa:bb:cc:dd:ee:01","owner_id":600001,"name":"x","type":5,"model":"C1Pro-BLE-V3","service_type":5,"create_time":"2026-09-01 19:41:03","isenable":true,"device_type":"sl_smartplug","hardwareid":700001,"remote_ids":[800001],"hardware_type":"DEMO-PLUG","remotes":[{"remote_id":800001}],"outletcount":1,"delays":[{"delay":120}]}]}"#;
        let parsed: WakeupDevicesResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.devices[0].description.is_none());
    }

    /// The list URL must stay byte-identical to the old `format!` output for
    /// a normal SN, and must percent-encode everything else.
    #[test]
    fn device_list_url_plain_sn_matches_legacy_format() {
        assert_eq!(
            device_list_url("https://api-std.sunlogin.oray.com", None).unwrap(),
            "https://api-std.sunlogin.oray.com/wakeup/devices?offset=0&limit=100"
        );
        assert_eq!(
            device_list_url("https://api-std.sunlogin.oray.com", Some("100000000001")).unwrap(),
            "https://api-std.sunlogin.oray.com/wakeup/devices?offset=0&limit=100&sn=100000000001"
        );
        // Trailing slash on the base is trimmed by `WakeupApi::new` already;
        // a URL-safe sn with dashes/underscores/dots also stays verbatim.
        assert_eq!(
            device_list_url("https://example.invalid", Some("AB_cd-1.2")).unwrap(),
            "https://example.invalid/wakeup/devices?offset=0&limit=100&sn=AB_cd-1.2"
        );
    }

    #[test]
    fn device_list_url_encodes_special_sn() {
        let space = device_list_url("https://example.invalid", Some("a b")).unwrap();
        assert_eq!(
            space,
            "https://example.invalid/wakeup/devices?offset=0&limit=100&sn=a+b"
        );

        // `&` must not split the query, `+` must not decode to a space,
        // non-ASCII must be UTF-8 percent-encoded.
        let weird = device_list_url("https://example.invalid", Some("a&b+c中文")).unwrap();
        assert!(!weird.contains(' '));
        assert!(weird.ends_with("&sn=a%26b%2Bc%E4%B8%AD%E6%96%87"));
        // Exactly one `sn=` pair, so nothing got injected as a new parameter.
        assert_eq!(weird.matches("sn=").count(), 1);
        // `?offset=0&limit=100&sn=…` — the only two separators in the URL.
        assert_eq!(weird.matches('&').count(), 2);
    }

    /// `find` must issue the server-side `sn=` filter (it used to call
    /// `list(token, None)`, so the filter branch was dead in production and
    /// a find always downloaded a whole unrelated page).
    #[test]
    fn find_url_carries_the_sn_filter() {
        let url = find_list_url("https://api-std.sunlogin.oray.com", "100000000001").unwrap();
        assert_eq!(
            url,
            "https://api-std.sunlogin.oray.com/wakeup/devices?offset=0&limit=100&sn=100000000001"
        );
        // The page cap rides the same constant the message below quotes.
        assert_eq!(DEVICE_LIST_LIMIT, 100);
    }

    /// A miss has to say how many devices were listed and that the scan
    /// stops at the page cap — an account with more than 100 devices would
    /// otherwise get a bare "not found" for a device the tool never fetched.
    #[test]
    fn find_not_found_message_reports_the_scanned_count_and_cap() {
        assert_eq!(
            find_not_found_message("100000000001", 100),
            "wakeup device sn=100000000001 not found among 100 devices listed (only the first \
             100 are scanned)"
        );
        // Fewer devices than the cap: the count is what was actually listed.
        let msg = find_not_found_message("42", 3);
        assert!(msg.contains("among 3 devices listed"), "{msg}");
        assert!(msg.contains("only the first 100 are scanned"), "{msg}");
        assert!(msg.starts_with("wakeup device sn=42 not found"), "{msg}");
    }
}
