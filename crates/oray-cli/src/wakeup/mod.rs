//! `wakeup` command group: wakeup devices (smart plugs / power hardware).
//!
//! Device listing/info/rename/memo live here; smart-plug controls live in the
//! [`plug`] submodule.

pub mod plug;

use crate::config::Config;
use crate::prompt;
use crate::support::{emit_json, with_token};
use anyhow::{Context, Result};
use clap::Subcommand;
use oray_core::wakeup::plug::PlugApi;
use oray_core::wakeup::{WakeupApi, WakeupDevice};
use reqwest::blocking::Client as HttpClient;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum WakeupCmd {
    /// List wakeup devices (smart plugs / power hardware)
    List,
    /// Show details for one device (by SN)
    Info {
        /// Device serial number
        sn: Option<String>,
    },
    /// Rename a device (SN)
    Rename {
        /// Device serial number
        sn: Option<String>,
        /// New device name
        new_name: Option<String>,
    },
    /// Set the memo (备注) of a device (SN)
    Memo {
        /// Device serial number
        sn: Option<String>,
        /// New memo text
        new_memo: Option<String>,
    },
    /// Smart-plug specific controls
    Plug {
        #[command(subcommand)]
        sub: plug::PlugCmd,
    },
}

/// The prompt label shared by every `<SN>` argument.
pub(crate) const SN_LABEL: &str = "Device serial number (SN)";

/// `--interactive`: type the arguments this command line left out.
pub fn fill(cmd: &mut WakeupCmd) -> Result<()> {
    match cmd {
        WakeupCmd::List => Ok(()),
        WakeupCmd::Info { sn } => fill_sn(sn, "oray-tools wakeup info <sn>"),
        WakeupCmd::Rename { sn, new_name } => {
            prompt::require_terminal(
                &[("<SN>", sn.is_none()), ("<NEW_NAME>", new_name.is_none())],
                "oray-tools wakeup rename <sn> <new_name>",
            )?;
            prompt::fill_str(sn, SN_LABEL, None)?;
            prompt::fill_str(new_name, "New device name", None)
        }
        WakeupCmd::Memo { sn, new_memo } => {
            prompt::require_terminal(
                &[("<SN>", sn.is_none()), ("<NEW_MEMO>", new_memo.is_none())],
                "oray-tools wakeup memo <sn> <new_memo>",
            )?;
            prompt::fill_str(sn, SN_LABEL, None)?;
            prompt::fill_str(new_memo, "New memo", None)
        }
        WakeupCmd::Plug { sub } => plug::fill(sub),
    }
}

/// Prompt for `<SN>` when the command line left it out.
pub(crate) fn fill_sn(sn: &mut Option<String>, example: &str) -> Result<()> {
    prompt::require_terminal(&[("<SN>", sn.is_none())], example)?;
    prompt::fill_str(sn, SN_LABEL, None)
}

pub fn run(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    sub: WakeupCmd,
    refresh_on_expired: bool,
    json: bool,
    tz: Option<i64>,
) -> Result<()> {
    let server = cfg.server();
    let wakeup = WakeupApi::new(http.clone(), &server.api_base);
    let plug = PlugApi::new(http.clone(), &server.slapi_base);
    match sub {
        WakeupCmd::List => {
            let devices = with_token(http, cfg, path, refresh_on_expired, |tok| {
                wakeup.list(tok, None)
            })?;
            emit_json(json, &devices)?;
            if !json {
                for d in &devices.devices {
                    let enabled = if d.isenable { "enabled" } else { "disabled" };
                    println!(
                        "sn={} name={} type={} outlets={} {}",
                        d.sn, d.name, d.device_type, d.outletcount, enabled
                    );
                }
            }
            Ok(())
        }
        WakeupCmd::Info { sn } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            let device = with_token(http, cfg, path, refresh_on_expired, |tok| {
                wakeup.find(tok, sn)
            })?;
            emit_json(json, &device)?;
            print_wakeup_device(&device, json);
            Ok(())
        }
        WakeupCmd::Rename { sn, new_name } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            let new_name = new_name.as_deref().context("missing <NEW_NAME>")?;
            let device = with_token(http, cfg, path, refresh_on_expired, |tok| {
                wakeup.find(tok, sn)
            })?;
            let description = device.description.as_deref().unwrap_or("");
            with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.rename_device(tok, sn, new_name, description)
            })?;
            if !json {
                println!("renamed {sn} to '{new_name}'");
            }
            Ok(())
        }
        WakeupCmd::Memo { sn, new_memo } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            let new_memo = new_memo.as_deref().context("missing <NEW_MEMO>")?;
            let device = with_token(http, cfg, path, refresh_on_expired, |tok| {
                wakeup.find(tok, sn)
            })?;
            with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.rename_device(tok, sn, &device.name, new_memo)
            })?;
            if !json {
                println!("memo of {sn} set to '{new_memo}'");
            }
            Ok(())
        }
        WakeupCmd::Plug { sub } => plug::run(http, cfg, path, sub, refresh_on_expired, json, tz),
    }
}

fn print_wakeup_device(d: &WakeupDevice, json: bool) {
    if json {
        return;
    }
    println!("sn:          {}", d.sn);
    println!("name:        {}", d.name);
    println!("device_id:   {}", d.device_id);
    println!("mac:         {}", d.mac);
    println!("type:        {} ({})", d.device_type, d.r#type);
    println!("model:       {}", d.model);
    println!("hardware:    {}", d.hardware_type);
    println!("outlets:     {}", d.outletcount);
    println!("enabled:     {}", d.isenable);
    println!("create_time: {}", d.create_time);
    if let Some(desc) = &d.description {
        println!("memo:        {desc}");
    }
    if !d.remote_ids.is_empty() {
        println!("remote_ids:  {:?}", d.remote_ids);
    }
}
