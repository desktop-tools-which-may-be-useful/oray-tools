use crate::{Error, Result};
use reqwest::blocking::Client;
use serde::Deserialize;

/// Thin wrapper over the Oray smart-plug HTTP endpoints. Stateless: callers
/// own the access token and plug identity.
///
/// The `reqwest` client is injected so callers control timeouts, proxies,
/// connection reuse and test doubles; `clone()` is cheap (shared connection
/// pool), so reuse a single client across all API calls.
pub struct PlugApi {
    client: Client,
    slapi_base: String,
}

/// Result of a status query. `result == 0` means success.
#[derive(Debug, Deserialize)]
pub struct PlugStatusResp {
    pub result: i32,
    #[serde(default)]
    pub response: Option<Vec<PlugStatus>>,
    #[serde(default)]
    pub message: Option<String>,
}

#[derive(Debug, Deserialize)]
pub struct PlugStatus {
    pub index: i32,
    pub status: i32,
}

#[derive(Debug, Deserialize)]
pub struct SetResp {
    pub result: i32,
    #[serde(default)]
    pub message: Option<String>,
}

impl PlugApi {
    /// Wrap an injected client for the SLAPI base (e.g. `https://slapi.oray.net`).
    pub fn new(client: Client, slapi_base: &str) -> Self {
        Self {
            client,
            slapi_base: slapi_base.trim_end_matches('/').to_string(),
        }
    }

    pub fn get_status(
        &self,
        access_token: &str,
        sn: &str,
        index: usize,
    ) -> Result<PlugStatusResp> {
        let url = format!(
            "{}/plug?sn={sn}&_api=get_plug_status&index={index}",
            self.slapi_base
        );
        let resp = self
            .client
            .get(&url)
            .bearer_auth(access_token)
            .send()?;
        let text = resp.text()?;
        let parsed: PlugStatusResp = serde_json::from_str(&text)
            .map_err(|_| Error::BadBody { body: text.clone() })?;
        if parsed.result != 0 {
            return Err(Error::Api(format!(
                "get_plug_status failed (result={}) {}",
                parsed.result,
                parsed.message.as_deref().unwrap_or("")
            )));
        }
        Ok(parsed)
    }

    pub fn set_status(
        &self,
        access_token: &str,
        sn: &str,
        index: usize,
        on: bool,
    ) -> Result<SetResp> {
        let st = if on { 1 } else { 0 };
        let url = format!(
            "{}/plug?sn={sn}&index={index}&status={st}&_api=set_plug_status",
            self.slapi_base
        );
        let resp = self
            .client
            .get(&url)
            .bearer_auth(access_token)
            .send()?;
        let text = resp.text()?;
        let parsed: SetResp = serde_json::from_str(&text)
            .map_err(|_| Error::BadBody { body: text.clone() })?;
        if parsed.result != 0 {
            return Err(Error::Api(format!(
                "set_plug_status failed (result={}) {}",
                parsed.result,
                parsed.message.as_deref().unwrap_or("")
            )));
        }
        Ok(parsed)
    }
}