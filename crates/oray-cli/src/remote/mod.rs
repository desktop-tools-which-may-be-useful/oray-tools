//! `remote` command group: remote devices (PCs / phones).
//!
//! Only one module: the remote API has a single client and the commands are
//! thin, so everything lives in this `mod.rs`.

use crate::config::Config;
use crate::support::{emit_json, with_token};
use anyhow::Result;
use clap::Subcommand;
use oray_core::remote::{RemoteApi, RemoteUpdate};
use reqwest::blocking::Client as HttpClient;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum RemoteCmd {
    /// List remote devices
    List,
    /// Show extended detail for one remote (by remote id)
    Info {
        /// Remote device id
        id: u64,
    },
    /// Show runtime status of one remote
    Status {
        /// Remote device id
        id: u64,
    },
    /// Rename a remote device
    Rename {
        /// Remote device id
        id: u64,
        /// New device name
        new_name: String,
    },
    /// Set the memo of a remote device
    Memo {
        /// Remote device id
        id: u64,
        /// New memo text
        new_memo: String,
    },
}

pub fn run(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    sub: RemoteCmd,
    refresh_on_expired: bool,
    json: bool,
) -> Result<()> {
    let server = cfg.server();
    let api = RemoteApi::new(http.clone(), &server.api_base);
    match sub {
        RemoteCmd::List => {
            let resp = with_token(http, cfg, path, refresh_on_expired, |tok| {
                api.list(tok, 0, 10_000)
            })?;
            emit_json(json, &resp)?;
            if !json {
                for r in &resp.remotes {
                    let online = if r.state.as_ref().is_some_and(|s| s.is_online()) {
                        "online"
                    } else {
                        "offline"
                    };
                    let memo = if r.info.description.is_empty() {
                        String::new()
                    } else {
                        format!(" memo={}", r.info.description)
                    };
                    println!(
                        "id={} name={} os={} client={} {online}{memo}",
                        r.remote_id, r.info.name, r.info.os_name, r.client
                    );
                }
            }
            Ok(())
        }
        RemoteCmd::Info { id } => {
            let detail = with_token(http, cfg, path, refresh_on_expired, |tok| {
                api.detail(tok, id)
            })?;
            emit_json(json, &detail)?;
            if !json {
                println!("id:           {}", detail.remote_id);
                println!("name:         {}", detail.info.name);
                println!("mac:          {}", detail.mac);
                println!("client:       {} {}", detail.client, detail.info.version);
                println!("os:           {}", detail.info.os_name);
                println!("cpu:          {}", detail.info.cpu);
                println!("memory:       {}", detail.info.memory);
                if !detail.info.screen_size.is_empty() {
                    println!("screen:       {}", detail.info.screen_size);
                }
                if !detail.info.description.is_empty() {
                    println!("memo:         {}", detail.info.description);
                }
                if let Some(s) = &detail.state {
                    let online = if s.is_online() { "online" } else { "offline" };
                    println!("state:        {online}");
                    if !s.ip.is_empty() {
                        println!("ip:           {}", s.ip);
                    }
                    if s.login_time > 0 {
                        println!(
                            "login_time:   {}",
                            crate::token::human_time(s.login_time as i64)
                        );
                    }
                } else {
                    println!("state:        offline");
                }
            }
            Ok(())
        }
        RemoteCmd::Status { id } => {
            let remote = with_token(http, cfg, path, refresh_on_expired, |tok| api.find(tok, id))?;
            emit_json(json, &remote)?;
            if !json {
                let online = if remote.state.as_ref().is_some_and(|s| s.is_online()) {
                    "online"
                } else {
                    "offline"
                };
                println!(
                    "id={} name={} status={online}",
                    remote.remote_id, remote.info.name
                );
                if let Some(s) = &remote.state {
                    if !s.ip.is_empty() {
                        println!("ip={}", s.ip);
                    }
                    if s.login_time > 0 {
                        println!(
                            "login_time={}",
                            crate::token::human_time(s.login_time as i64)
                        );
                    }
                    if !s.fastcode.is_empty() {
                        println!("fastcode={}", s.fastcode);
                    }
                }
                println!("client={} os={}", remote.client, remote.info.os_name);
            }
            Ok(())
        }
        RemoteCmd::Rename { id, new_name } => {
            // Preserve the memo: fetch the current description first, then send
            // both fields together (the PATCH endpoint always updates both).
            let current = with_token(http, cfg, path, refresh_on_expired, |tok| api.find(tok, id))?;
            with_token(http, cfg, path, refresh_on_expired, |tok| {
                api.update(
                    tok,
                    id,
                    &RemoteUpdate::new(&new_name, &current.info.description),
                )
            })?;
            if !json {
                println!("renamed remote {id} to '{new_name}'");
            }
            Ok(())
        }
        RemoteCmd::Memo { id, new_memo } => {
            let current = with_token(http, cfg, path, refresh_on_expired, |tok| api.find(tok, id))?;
            with_token(http, cfg, path, refresh_on_expired, |tok| {
                api.update(tok, id, &RemoteUpdate::new(&current.info.name, &new_memo))
            })?;
            if !json {
                println!("memo of remote {id} set to '{new_memo}'");
            }
            Ok(())
        }
    }
}
