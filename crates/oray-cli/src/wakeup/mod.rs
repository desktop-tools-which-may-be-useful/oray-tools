//! `wakeup` command group: wakeup devices (smart plugs / power hardware).
//!
//! Device listing/info/rename/memo live here; smart-plug controls live in the
//! [`plug`] submodule.
//!
//! # `--json` output contract
//!
//! - With `--json`, every command prints **exactly one** JSON value on
//!   stdout: on success an object (`rename`, `memo`) or one of the shapes
//!   that already existed and stay untouched (`list`, `info`, plus
//!   everything the [`plug`] submodule emits); on failure exactly one error
//!   object (next bullet).
//! - A failure always `bail!`s: with `--json`, main.rs prints
//!   `{"ok": false, "error": "<message>"}` on **stdout** — one object
//!   carrying the very message the text mode prints — and exits 1, so a
//!   consumer parsing stdout always sees exactly one value; without
//!   `--json` the single `error: ...` line goes to stderr, byte-identical
//!   to before. No branch swallows an error just because `--json` was
//!   passed, and no success path prints an empty stdout in JSON mode.
//! - Text-mode output is unchanged.

pub mod plug;

use crate::config::Config;
use crate::prompt;
use crate::support::{emit_json, with_token};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use oray_core::wakeup::plug::PlugApi;
use oray_core::wakeup::{DEVICE_LIST_LIMIT, WakeupApi, WakeupDevice, effective_device_limit};
use reqwest::blocking::Client as HttpClient;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum WakeupCmd {
    /// List wakeup devices (smart plugs / power hardware; paged)
    List {
        /// First device of the page to fetch
        #[arg(long, default_value_t = 0, display_order = 0)]
        offset: u64,
        /// Devices per request (default 100, capped at 10000)
        #[arg(long, default_value_t = DEVICE_LIST_LIMIT, display_order = 0)]
        limit: usize,
    },
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

/// `--json` body of `wakeup rename` (module docs carry the contract). Kept as
/// a builder so the shape is unit-testable without a network round trip.
fn json_renamed(sn: &str, name: &str) -> serde_json::Value {
    serde_json::json!({ "ok": true, "sn": sn, "name": name })
}

/// `--json` body of `wakeup memo`.
fn json_memo(sn: &str, memo: &str) -> serde_json::Value {
    serde_json::json!({ "ok": true, "sn": sn, "memo": memo })
}

/// Post-hoc page warnings for `wakeup list`, printed on **stderr** so the
/// `--json` contract (exactly one value on stdout) stays intact.
///
/// This endpoint answers neither `total` nor a page-size limit, so the only
/// honest signals are local ones: a `--limit` the client did not send as
/// asked, and a page that came back exactly full — which may be the whole
/// account or the first of several, and the response cannot tell them apart.
fn page_warnings(offset: u64, limit: usize, shown: usize) -> Vec<String> {
    let effective = effective_device_limit(limit);
    let mut warnings = Vec::new();
    if effective != limit {
        warnings.push(format!(
            "warning: --limit {limit} requested, {effective} sent — the endpoint publishes no \
             page size, so this client bounds its own request"
        ));
    }
    if shown == effective {
        warnings.push(format!(
            "warning: a full page ({shown} fetched at --limit {effective}) and the endpoint \
             reports no total, so more may exist — fetch the next one with \
             `oray-tools wakeup list --offset {}`",
            offset + shown as u64
        ));
    }
    warnings
}

/// `--interactive`: type the arguments this command line left out.
///
/// A no-op unless the flag is set (defense in depth on top of clap's strict
/// build in main.rs): prompting must be unreachable without
/// `--interactive`, whatever `strictify` marks as required.
pub fn fill(cmd: &mut WakeupCmd, interactive: bool) -> Result<()> {
    if !interactive {
        return Ok(());
    }
    match cmd {
        // `--offset`/`--limit` both default, so nothing is left to ask for.
        WakeupCmd::List { .. } => Ok(()),
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
        WakeupCmd::Plug { sub } => plug::fill(sub, interactive),
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
        WakeupCmd::List { offset, limit } => {
            // A zero limit is not a page size, so refuse it before spending a
            // token round trip (`effective_device_limit` maps 0 to the
            // default for direct API callers, but the flag should be explicit).
            if limit == 0 {
                bail!("--limit must be at least 1");
            }
            let devices = with_token(http, cfg, path, refresh_on_expired, |tok| {
                wakeup.list(tok, offset, limit, None)
            })?;
            // A full page or a capped `--limit` cannot be seen from the JSON
            // alone; both go to stderr, never stdout.
            for warning in page_warnings(offset, limit, devices.devices.len()) {
                eprintln!("{warning}");
            }
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
            emit_json(json, &json_renamed(sn, new_name))?;
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
            emit_json(json, &json_memo(sn, new_memo))?;
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
            body(json_renamed("100000000001", "desk plug")),
            serde_json::json!({ "ok": true, "sn": "100000000001", "name": "desk plug" })
        );
    }

    #[test]
    fn json_shape_memo() {
        assert_eq!(
            body(json_memo("100000000001", "阳台")),
            serde_json::json!({ "ok": true, "sn": "100000000001", "memo": "阳台" })
        );
    }

    /// A full page is the endpoint's only hint that more devices may exist,
    /// so it has to be reported — together with any capped `--limit`.
    #[test]
    fn page_warnings_flag_a_full_page_and_a_capped_limit() {
        let full = page_warnings(0, DEVICE_LIST_LIMIT, DEVICE_LIST_LIMIT);
        assert_eq!(full.len(), 1, "{full:?}");
        assert!(
            full[0].contains("a full page (100 fetched at --limit 100)"),
            "{}",
            full[0]
        );
        assert!(full[0].contains("--offset 100"), "{}", full[0]);

        let capped = page_warnings(0, 20_000, 10_000);
        assert_eq!(capped.len(), 2, "{capped:?}");
        assert!(
            capped[0].contains("--limit 20000 requested, 10000 sent"),
            "{}",
            capped[0]
        );
        assert!(capped[1].contains("--offset 10000"), "{}", capped[1]);
    }

    /// A short page is provably the last one: nothing to warn about, and the
    /// next-page offset advances by what was actually fetched.
    #[test]
    fn page_warnings_stay_quiet_on_a_short_page() {
        assert!(page_warnings(0, DEVICE_LIST_LIMIT, 3).is_empty());
        let next = page_warnings(150, 50, 50);
        assert_eq!(next.len(), 1, "{next:?}");
        assert!(next[0].contains("--offset 200"), "{}", next[0]);
    }
}
