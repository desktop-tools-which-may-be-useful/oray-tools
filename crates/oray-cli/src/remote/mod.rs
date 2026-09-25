//! `remote` command group: remote devices (PCs / phones).
//!
//! Only one module: the remote API has a single client and the commands are
//! thin, so everything lives in this `mod.rs`.
//!
//! # `--json` output contract
//!
//! - With `--json`, every command prints **exactly one** JSON value on
//!   stdout: on success an object (`rename`, `memo`) or one of the shapes
//!   that already existed and stay untouched (`list`, `info`, `status`);
//!   on failure exactly one error object (next bullet).
//! - A failure always `bail!`s: with `--json`, main.rs prints
//!   `{"ok": false, "error": "<message>"}` on **stdout** — one object
//!   carrying the very message the text mode prints — and exits 1, so a
//!   consumer parsing stdout always sees exactly one value; without
//!   `--json` the single `error: ...` line goes to stderr, byte-identical
//!   to before. No branch swallows an error just because `--json` was
//!   passed, and no success path prints an empty stdout in JSON mode.
//! - Text-mode output is unchanged.

use crate::config::Config;
use crate::prompt;
use crate::support::{emit_json, with_token};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use oray_core::remote::{REMOTE_PAGE_LIMIT, RemoteApi, RemoteUpdate, RemotesResponse};
use reqwest::blocking::Client as HttpClient;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum RemoteCmd {
    /// List remote devices (paged server-side: `--offset`/`--limit`)
    List {
        /// First remote of the page to fetch
        #[arg(long, default_value_t = 0, display_order = 0)]
        offset: u64,
        /// Remotes per request (the endpoint's own cap is 10000; a bigger
        /// value is clamped server-side and reported on stderr)
        #[arg(long, default_value_t = REMOTE_PAGE_LIMIT, display_order = 0)]
        limit: u64,
    },
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

/// Post-hoc page warnings for `remote list`, printed on **stderr** so the
/// `--json` contract (exactly one value on stdout) stays intact.
///
/// Both signals come from the response, because that is where they live: the
/// server's own `page_size_limit` — a request cannot be capped before it is
/// sent, the cap arrives with the answer — and `total`, which is what
/// actually tells the caller a page is not the end of the list.
fn page_warnings(offset: u64, limit: u64, resp: &RemotesResponse) -> Vec<String> {
    let mut warnings = Vec::new();
    if let Some(cap) = resp.page_size_limit
        && limit > cap
    {
        warnings.push(format!(
            "warning: --limit {limit} exceeds the server's page size limit {cap}; this \
             response holds at most {cap} remotes"
        ));
    }
    if let Some(total) = resp.total {
        let shown = resp.remotes.len() as u64;
        if offset.saturating_add(shown) < total {
            warnings.push(format!(
                "warning: showing {shown} of {total} remotes (offset {offset}); fetch the rest \
                 with `oray-tools remote list --offset {}`",
                offset + shown
            ));
        }
    }
    warnings
}

/// `--interactive`: type the arguments this command line left out.
///
/// A no-op unless the flag is set (defense in depth on top of clap's strict
/// build in main.rs): prompting must be unreachable without
/// `--interactive`, whatever `strictify` marks as required.
pub fn fill(cmd: &mut RemoteCmd, interactive: bool) -> Result<()> {
    if !interactive {
        return Ok(());
    }
    match cmd {
        // `--offset`/`--limit` both default, so nothing is left to ask for.
        RemoteCmd::List { .. } => Ok(()),
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
        RemoteCmd::List { offset, limit } => {
            // A zero limit is not a page size (the server would answer with
            // the whole list), so refuse it before spending a token round trip.
            if limit == 0 {
                bail!("--limit must be at least 1");
            }
            let resp = with_token(http, cfg, path, refresh_on_expired, |tok| {
                api.list(tok, offset, limit)
            })?;
            // After the fact, because that is when the response knows: server
            // cap, and whether more pages follow. stderr, never stdout.
            for warning in page_warnings(offset, limit, &resp) {
                eprintln!("{warning}");
            }
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
            // Single-item console lookup, not a list+scan: `detail` returns
            // the same remote (`info`, `state`) without downloading up to
            // 10 000 remotes just to resolve one id.
            let remote = with_token(http, cfg, path, refresh_on_expired, |tok| {
                api.detail(tok, id)
            })?;
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
            // `detail` carries `info.description`, so the list+scan `find` is
            // not needed to read one remote's memo.
            let current = with_token(http, cfg, path, refresh_on_expired, |tok| {
                api.detail(tok, id)
            })?;
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
            // Same single-item lookup as rename: `info.name` comes from the
            // console detail, so no device list has to be fetched.
            let current = with_token(http, cfg, path, refresh_on_expired, |tok| {
                api.detail(tok, id)
            })?;
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

    /// A page that stops short of `total`, and a `--limit` above the
    /// server's own cap: the two things the response can prove, and the only
    /// two things worth a stderr line.
    #[test]
    fn page_warnings_report_cap_and_remainder() {
        let resp: RemotesResponse = serde_json::from_str(
            r#"{"remotes":[{"remote_id":800001},{"remote_id":800002}],"total":5,"page_size_limit":10000}"#,
        )
        .unwrap();
        let warnings = page_warnings(0, 20_000, &resp);
        assert_eq!(warnings.len(), 2, "{warnings:?}");
        assert!(
            warnings[0].contains("--limit 20000 exceeds the server's page size limit 10000"),
            "{}",
            warnings[0]
        );
        assert!(
            warnings[1].contains("showing 2 of 5 remotes"),
            "{}",
            warnings[1]
        );
        assert!(warnings[1].contains("--offset 2"), "{}", warnings[1]);
    }

    /// Inside the cap and covering `total` — or no metadata at all — there is
    /// nothing to say, so `--json` consumers never see a stray line.
    #[test]
    fn page_warnings_stay_quiet_on_a_complete_page() {
        let complete: RemotesResponse = serde_json::from_str(
            r#"{"remotes":[{"remote_id":800001}],"total":1,"page_size_limit":10000}"#,
        )
        .unwrap();
        assert!(page_warnings(0, REMOTE_PAGE_LIMIT, &complete).is_empty());

        let bare: RemotesResponse =
            serde_json::from_str(r#"{"remotes":[{"remote_id":800001}]}"#).unwrap();
        assert!(page_warnings(0, REMOTE_PAGE_LIMIT, &bare).is_empty());
    }
}
