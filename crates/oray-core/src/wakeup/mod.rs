//! 开机设备 (wakeup devices): smart plugs and other power hardware.
//!
//! Device listing lives here (`WakeupApi`); plug-specific controls live in the
//! [`plug`] submodule.

pub mod plug;

use crate::output::{log_auth_header, log_request, log_response};
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

impl WakeupApi {
    /// Wrap an injected client for the API base
    /// (e.g. `https://api-std.sunlogin.oray.com`).
    pub fn new(client: Client, api_base: &str) -> Self {
        Self {
            client,
            api_base: api_base.trim_end_matches('/').to_string(),
        }
    }

    /// List all wakeup-capable devices. Optionally filter to one SN
    /// (`/wakeup/devices?sn=<sn>`).
    pub fn list(&self, token: &str, sn: Option<&str>) -> Result<WakeupDevicesResponse> {
        let mut url = format!("{}/wakeup/devices?offset=0&limit=100", self.api_base);
        if let Some(sn) = sn {
            url = format!("{url}&sn={sn}");
        }
        log_request("GET", &url);
        let resp = self
            .client
            .get(&url)
            .bearer_auth(token)
            .header("Accept", "application/json")
            .header("User-Agent", crate::USER_AGENT)
            .header("X-Channel", "OPPO")
            .header("Country-Region", "CN")
            .send()?;
        log_auth_header("", token);
        let status = resp.status();
        let text = resp.text()?;
        log_response(status.as_u16(), &text);
        if !status.is_success() {
            return Err(Error::HttpStatus {
                what: "list wakeup devices",
                status: status.as_u16(),
                body: text,
            });
        }
        serde_json::from_str(&text).map_err(|e| Error::bad_body(text, e))
    }

    /// Look up a single device by SN.
    pub fn find(&self, token: &str, sn: &str) -> Result<WakeupDevice> {
        let all = self.list(token, None)?;
        all.devices
            .into_iter()
            .find(|d| d.sn == sn)
            .ok_or_else(|| Error::Api(format!("wakeup device sn={sn} not found")))
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
}
