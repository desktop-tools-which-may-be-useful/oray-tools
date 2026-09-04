//! `wakeup` command group: wakeup devices (smart plugs / power hardware).
//!
//! Device listing/info/rename/memo live here; smart-plug controls live in the
//! [`plug`] submodule.

pub mod plug;

use crate::config::Config;
use crate::support::{emit_json, with_token};
use anyhow::Result;
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
        sn: String,
    },
    /// Rename a device (SN)
    Rename {
        /// Device serial number
        sn: String,
        /// New device name
        new_name: String,
    },
    /// Set the memo (备注) of a device (SN)
    Memo {
        /// Device serial number
        sn: String,
        /// New memo text
        new_memo: String,
    },
    /// Smart-plug specific controls
    Plug {
        #[command(subcommand)]
        sub: plug::PlugCmd,
    },
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
            let device = with_token(http, cfg, path, refresh_on_expired, |tok| {
                wakeup.find(tok, &sn)
            })?;
            emit_json(json, &device)?;
            print_wakeup_device(&device, json);
            Ok(())
        }
        WakeupCmd::Rename { sn, new_name } => {
            let device = with_token(http, cfg, path, refresh_on_expired, |tok| {
                wakeup.find(tok, &sn)
            })?;
            let description = device.description.as_deref().unwrap_or("");
            with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.rename_device(tok, &sn, &new_name, description)
            })?;
            if !json {
                println!("renamed {sn} to '{new_name}'");
            }
            Ok(())
        }
        WakeupCmd::Memo { sn, new_memo } => {
            let device = with_token(http, cfg, path, refresh_on_expired, |tok| {
                wakeup.find(tok, &sn)
            })?;
            with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.rename_device(tok, &sn, &device.name, &new_memo)
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
