use anyhow::{Context, Result, bail};
use reqwest::blocking::Client;
use serde::Deserialize;

#[derive(Clone, Copy)]
pub enum PlugAction {
    Status,
    On,
    Off,
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

pub fn get_status(
    client: &Client,
    slapi_base: &str,
    access_token: &str,
    sn: &str,
    index: usize,
) -> Result<PlugStatusResp> {
    let base = slapi_base.trim_end_matches('/');
    let url = format!(
        "{base}/plug?sn={sn}&_api=get_plug_status&index={index}"
    );
    let resp = client
        .get(&url)
        .bearer_auth(access_token)
        .send()
        .context("send get_plug_status request")?;
    let text = resp.text().context("read get_plug_status body")?;
    let parsed: PlugStatusResp =
        serde_json::from_str(&text).with_context(|| format!("parse get_plug_status: {text}"))?;
    if parsed.result != 0 {
        bail!(
            "get_plug_status failed (result={}) {}",
            parsed.result,
            parsed.message.as_deref().unwrap_or("")
        );
    }
    Ok(parsed)
}

pub fn set_status(
    client: &Client,
    slapi_base: &str,
    access_token: &str,
    sn: &str,
    index: usize,
    on: bool,
) -> Result<SetResp> {
    let base = slapi_base.trim_end_matches('/');
    let st = if on { 1 } else { 0 };
    let url = format!("{base}/plug?sn={sn}&index={index}&status={st}&_api=set_plug_status");
    let resp = client
        .get(&url)
        .bearer_auth(access_token)
        .send()
        .context("send set_plug_status request")?;
    let text = resp.text().context("read set_plug_status body")?;
    let parsed: SetResp = serde_json::from_str(&text).with_context(|| format!("parse set_plug_status: {text}"))?;
    if parsed.result != 0 {
        bail!(
            "set_plug_status failed (result={}) {}",
            parsed.result,
            parsed.message.as_deref().unwrap_or("")
        );
    }
    Ok(parsed)
}