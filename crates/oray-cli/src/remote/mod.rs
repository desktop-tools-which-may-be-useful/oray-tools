//! `remote` command group: remote devices (PCs / phones).
//!
//! Only one module: the remote API has a single client and the commands are
//! thin, so everything lives in this `mod.rs`.
//!
//! # `--json` output contract
//!
//! - With `--json`, every command prints **exactly one** JSON value on stdout
//!   and only when it succeeds: an object (`rename`, `memo`) or one of the
//!   shapes that already existed and stay untouched (`list`, `info`,
//!   `status`).
//! - A failure always `bail!`s: main.rs prints `error: ...` on stderr and
//!   exits 1, identically in JSON and text mode. No branch swallows an error
//!   just because `--json` was passed, and no success path prints an empty
//!   stdout in JSON mode.
//! - Text-mode output is unchanged.

use crate::config::Config;
use crate::prompt;
use crate::support::{emit_json, with_token};
use anyhow::{Context, Result};
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
        id: Option<u64>,
    },
    /// Show runtime status of one remote
    Status {
        /// Remote device id
        id: Option<u64>,
    },
    /// Rename a remote device
    Rename {
        /// Remote device id
        id: Option<u64>,
        /// New device name
        new_name: Option<String>,
    },
    /// Set the memo of a remote device
    Memo {
        /// Remote device id
        id: Option<u64>,
        /// New memo text
        new_memo: Option<String>,
    },
}

/// `--json` body of `remote rename` (module docs carry the contract). Kept as
/// a builder so the shape is unit-testable without a network round trip.
fn json_renamed(id: u64, name: &str) -> serde_json::Value {
    serde_json::json!({ "ok": true, "id": id, "name": name })
}

/// `--json` body of `remote memo`.
fn json_memo(id: u64, memo: &str) -> serde_json::Value {
    serde_json::json!({ "ok": true, "id": id, "memo": memo })
}

/// `--interactive`: type the arguments this command line left out.
pub fn fill(cmd: &mut RemoteCmd) -> Result<()> {
    match cmd {
        RemoteCmd::List => Ok(()),
        RemoteCmd::Info { id } => {
            prompt::require_terminal(&[("<ID>", id.is_none())], "oray-tools remote info <id>")?;
            prompt::fill_parsed(id, "Remote id")
        }
        RemoteCmd::Status { id } => {
            prompt::require_terminal(&[("<ID>", id.is_none())], "oray-tools remote status <id>")?;
            prompt::fill_parsed(id, "Remote id")
        }
        RemoteCmd::Rename { id, new_name } => {
            prompt::require_terminal(
                &[("<ID>", id.is_none()), ("<NEW_NAME>", new_name.is_none())],
                "oray-tools remote rename <id> <new_name>",
            )?;
            prompt::fill_parsed(id, "Remote id")?;
            prompt::fill_str(new_name, "New device name", None)
        }
        RemoteCmd::Memo { id, new_memo } => {
            prompt::require_terminal(
                &[("<ID>", id.is_none()), ("<NEW_MEMO>", new_memo.is_none())],
                "oray-tools remote memo <id> <new_memo>",
            )?;
            prompt::fill_parsed(id, "Remote id")?;
            prompt::fill_str(new_memo, "New memo", None)
        }
    }
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
            let id = id.context("missing <ID>")?;
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
            let id = id.context("missing <ID>")?;
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
            let id = id.context("missing <ID>")?;
            let new_name = new_name.as_deref().context("missing <NEW_NAME>")?;
            // Preserve the memo: fetch the current description first, then send
            // both fields together (the PATCH endpoint always updates both).
            let current = with_token(http, cfg, path, refresh_on_expired, |tok| api.find(tok, id))?;
            with_token(http, cfg, path, refresh_on_expired, |tok| {
                api.update(
                    tok,
                    id,
                    &RemoteUpdate::new(new_name, &current.info.description),
                )
            })?;
            emit_json(json, &json_renamed(id, new_name))?;
            if !json {
                println!("renamed remote {id} to '{new_name}'");
            }
            Ok(())
        }
        RemoteCmd::Memo { id, new_memo } => {
            let id = id.context("missing <ID>")?;
            let new_memo = new_memo.as_deref().context("missing <NEW_MEMO>")?;
            let current = with_token(http, cfg, path, refresh_on_expired, |tok| api.find(tok, id))?;
            with_token(http, cfg, path, refresh_on_expired, |tok| {
                api.update(tok, id, &RemoteUpdate::new(&current.info.name, new_memo))
            })?;
            emit_json(json, &json_memo(id, new_memo))?;
            if !json {
                println!("memo of remote {id} set to '{new_memo}'");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The serialized body (the exact value `emit_json` prints) of a
    /// constructed JSON value.
    fn body(v: serde_json::Value) -> serde_json::Value {
        serde_json::to_value(&v).expect("JSON body serializes")
    }

    #[test]
    fn json_shape_rename() {
        assert_eq!(
            body(json_renamed(42, "workstation")),
            serde_json::json!({ "ok": true, "id": 42, "name": "workstation" })
        );
    }

    #[test]
    fn json_shape_memo() {
        assert_eq!(
            body(json_memo(42, "办公桌")),
            serde_json::json!({ "ok": true, "id": 42, "memo": "办公桌" })
        );
    }
}
