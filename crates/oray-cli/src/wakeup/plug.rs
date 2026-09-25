//! `wakeup plug` command group: smart-plug specific controls.
//!
//! Outlet switching/status, logs, timers, countdown, LED and power-restore.
//!
//! # `--json` output contract
//!
//! - With `--json`, every command prints **exactly one** JSON value on stdout
//!   and only when it succeeds: one object (`plug on/off`, `led`,
//!   `power-on-restore`, `countdown start/stop`, `timer remove`,
//!   `timer enable/disable`) or one of the shapes that already existed and
//!   stay untouched (`plug status`, `plug logs` array, `timer list` array,
//!   `timer add` SetResp, `countdown status`).
//! - A failure always `bail!`s: main.rs prints `error: ...` on stderr and
//!   exits 1, identically in JSON and text mode. No branch swallows an error
//!   just because `--json` was passed, and no success path prints an empty
//!   stdout in JSON mode.
//! - Text-mode output is unchanged, except that an empty `logs` result now
//!   says so (`no matching events`) instead of printing nothing.
//!
//! Timer scheduling: the plug API stores schedule times in UTC minutes of the
//! day and weekday bits over UTC days, while the user-facing (App) semantics
//! are local time with weekdays starting Monday. The helpers below convert
//! between the two representations given a timezone offset in minutes.

use super::{SN_LABEL, fill_sn};
use crate::config::Config;
use crate::prompt;
use crate::support::{
    emit_json, parse_ago_secs, parse_on_off, render_ts, resolve_time_bound_tz, resolve_tz,
    tz_label, with_token,
};
use anyhow::{Context, Result, bail};
use chrono::Utc;
use clap::Subcommand;
use oray_core::trace::TracedResult;
use oray_core::wakeup::plug::{PlugApi, PlugTimer, StatusLog, StatusLogsData};
use reqwest::blocking::Client as HttpClient;
use std::path::PathBuf;

/// Everything the `wakeup plug` handlers need besides their own arguments.
///
/// The helpers used to receive `(http, cfg, path, refresh_on_expired, ...)`
/// one parameter at a time, which pushed `load_logs_page`, `do_timer` and
/// `set_timer_enabled` past clippy's argument limit; bundling the plumbing
/// here keeps every helper at five parameters or fewer. `json` (the `--json`
/// output mode) and `tz` (the `--tz` / config timezone argument) ride along
/// because they are properties of the current invocation, not of one command.
struct Ctx<'a> {
    http: &'a HttpClient,
    cfg: &'a mut Config,
    path: &'a PathBuf,
    plug: &'a PlugApi,
    refresh_on_expired: bool,
    json: bool,
    tz: Option<i64>,
}

impl Ctx<'_> {
    /// Run an authenticated plug call with this context's token lifecycle
    /// (refresh-on-expire plus `--verbose` trace rendering).
    fn with_tok<T>(&mut self, run: impl Fn(&str) -> TracedResult<T>) -> Result<T> {
        with_token(self.http, self.cfg, self.path, self.refresh_on_expired, run)
    }
}

/// Fetch one page of status-change logs (page 1 is the newest).
fn fetch_logs_page(ctx: &mut Ctx, sn: &str, page: u32) -> Result<StatusLogsData> {
    let plug = ctx.plug;
    ctx.with_tok(|tok| plug.status_logs(tok, sn, page))
}

/// Return page `p`, (re)using a cache indexed by page number.
fn load_logs_page<'a>(
    ctx: &mut Ctx,
    sn: &str,
    page: u32,
    cached: &'a mut [Option<StatusLogsData>],
) -> Result<&'a StatusLogsData> {
    let i = page as usize;
    if cached[i].is_none() {
        cached[i] = Some(fetch_logs_page(ctx, sn, page)?);
    }
    Ok(cached[i].as_ref().expect("loaded page"))
}

/// Client-side port filter behind `logs --index`.
///
/// `--index` is optional: without it nothing is dropped (the API already
/// returns the whole device's log, which is the historical behaviour), with
/// it only the entries the plug reported for that port stay. Pure so it can
/// be tested without the network.
fn filter_logs_by_index(logs: Vec<StatusLog>, index: Option<usize>) -> Vec<StatusLog> {
    match index {
        None => logs,
        Some(port) => logs
            .into_iter()
            .filter(|l| i64::from(l.index) == port as i64)
            .collect(),
    }
}

/// The text-mode lines of a `logs` query. An empty result gets an explicit
/// notice instead of printing nothing (which looked like a hang); `--json`
/// always prints an array (`[]` when empty) and is unaffected.
fn logs_text_lines(logs: &[StatusLog], tz: Option<i64>) -> Vec<String> {
    if logs.is_empty() {
        return vec!["no matching events".to_string()];
    }
    logs.iter()
        .map(|l| {
            let state = if l.status == 1 { "ON" } else { "OFF" };
            format!(
                "{} index={} {} {}",
                render_ts(l.createtime, tz),
                l.index,
                state,
                l.event
            )
        })
        .collect()
}

/// The `(newest, oldest)` event timestamp pair seen on a page.
fn logs_page_span(data: &StatusLogsData) -> Option<(i64, i64)> {
    let newest = data.logs.first()?.createtime;
    let oldest = data.logs.last()?.createtime;
    Some((newest, oldest))
}

/// The UTC minutes-of-the-day for a local time in `tz`.
fn local_to_cloud_time(local_min: u64, tz_min: i64) -> u64 {
    ((local_min as i64 - tz_min).rem_euclid(1440)) as u64
}

/// The local minutes-of-the-day for a UTC time stored by the plug API.
fn cloud_to_local_time(cloud_min: u64, tz_min: i64) -> u64 {
    ((cloud_min as i64 + tz_min).rem_euclid(1440)) as u64
}

/// Calendar-day offset between the local firing date and its UTC instant:
/// -1 when the UTC moment falls on the previous UTC day (east of UTC and local
/// time early), +1 when it falls on the next day (west of UTC and local time
/// late), otherwise 0.
fn date_delta(local_min: u64, tz_min: i64) -> i64 {
    let raw = local_min as i64 - tz_min;
    if raw < 0 {
        -1
    } else if raw >= 1440 {
        1
    } else {
        0
    }
}

/// Convert a local weekday mask (bit0=Mon ... bit6=Sun) plus a local time into
/// the UTC weekday mask the plug API stores (bit0=Sun ... bit6=Sat over the
/// UTC day of the firing instant).
fn local_to_cloud_repeat(local_mask: u8, local_min: u64, tz_min: i64) -> u8 {
    let delta = date_delta(local_min, tz_min);
    let mut out = 0u8;
    for local_bit in 0..7u8 {
        if local_mask & (1 << local_bit) == 0 {
            continue;
        }
        // local weekday Mon=0..Sun=6 -> cloud weekday Sun=0..Sat=6
        let cloud_weekday = (local_bit as i64 + 1).rem_euclid(7);
        // apply the UTC calendar-day shift, then set the bit
        let shifted = (cloud_weekday + delta).rem_euclid(7);
        out |= 1 << shifted;
    }
    out
}

/// Inverse of [`local_to_cloud_repeat`]: cloud weekday mask (Sun=bit0 ...
/// Sat=bit6) plus the local time -> local weekday mask (Mon=bit0 ... Sun=bit6).
fn cloud_to_local_mask(cloud_mask: u8, local_min: u64, tz_min: i64) -> u8 {
    let delta = date_delta(local_min, tz_min);
    let mut out = 0u8;
    for cloud_bit in 0..7u8 {
        if cloud_mask & (1 << cloud_bit) == 0 {
            continue;
        }
        // cloud weekday Sun=0..Sat=6 -> local weekday Mon=0..Sun=6 of the
        // local firing date (UTC date minus the calendar shift)
        let same_local = ((cloud_bit as i64 + 6) - delta).rem_euclid(7);
        out |= 1 << same_local;
    }
    out
}

const WEEKDAY_NAMES: [&str; 7] = ["周一", "周二", "周三", "周四", "周五", "周六", "周日"];

/// Human label for a local weekday mask (Mon=bit0..Sun=bit6), e.g. `周一三五`.
fn mask_days(mask: u8) -> String {
    let mut names = Vec::new();
    for bit in 0..7u8 {
        if mask & (1 << bit) != 0 {
            names.push(WEEKDAY_NAMES[bit as usize]);
        }
    }
    if names.is_empty() {
        "只一次".to_string()
    } else {
        names.join("")
    }
}

#[derive(Subcommand)]
pub enum PlugCmd {
    /// Query plug status
    Status {
        /// Device serial number
        sn: Option<String>,
        /// Port index (default: 0, the master switch)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Turn the plug on
    On {
        /// Device serial number
        sn: Option<String>,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Turn the plug off
    Off {
        /// Device serial number
        sn: Option<String>,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Fetch status-change logs
    Logs {
        /// Device serial number
        sn: Option<String>,
        /// Only keep events the plug reported for this port (filtered
        /// client-side; omit to keep every port)
        #[arg(long)]
        index: Option<usize>,
        /// Lower bound: only events at or after this. Either an ago duration
        /// (30m, 2h, 1d) or an absolute time (2026-09-01, or 2026-09-01
        /// 08:30[:00]); a bare date means the start of that day. Absolute
        /// times use the timezone from --tz / config tz, else machine local.
        #[arg(long)]
        since: Option<String>,
        /// Upper bound: only events at or before this. Same forms as --since;
        /// a bare date means the end of that day (23:59:59). Together with
        /// --since this selects the window between them.
        #[arg(long)]
        until: Option<String>,
        /// Fetch a single specific page instead of locating the window
        /// (page 1 is the newest)
        #[arg(long)]
        page: Option<u32>,
    },
    /// Timer management
    Timer {
        #[command(subcommand)]
        sub: TimerCmd,
    },
    /// Countdown management
    Countdown {
        #[command(subcommand)]
        sub: CountdownCmd,
    },
    /// Control the LED indicator
    Led {
        /// Device serial number
        sn: Option<String>,
        /// on or off
        state: Option<String>,
    },
    /// Set the state after a power loss: 0 = off, 2 = keep last state
    PowerOnRestore {
        /// Device serial number
        sn: Option<String>,
        /// 0 (off) or 2 (keep last state)
        state: Option<u32>,
    },
}

#[derive(Subcommand)]
pub enum TimerCmd {
    /// List timers for an outlet
    List {
        /// Device serial number
        sn: Option<String>,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Add a timer that fires at a local clock time on the matching days
    Add {
        /// Device serial number
        sn: Option<String>,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
        /// Local time when the timer fires: a clock time like 19:25 (or
        /// 8:05), or minutes of the day 0-1439 (e.g. 480 = 08:00)
        #[arg(long)]
        time: Option<String>,
        /// Resulting state: 0 = off, 1 = on (default: 1)
        #[arg(long, default_value_t = 1)]
        action: u8,
        /// Local weekday bitmask (bit0=Mon ... bit6=Sun; 0 = run once)
        #[arg(long, default_value_t = 0)]
        repeat: u8,
        /// Create the timer disabled (kept but inactive until enabled)
        #[arg(long)]
        disabled: bool,
    },
    /// Remove a timer by its timer id
    Remove {
        /// Device serial number
        sn: Option<String>,
        /// Timer id (see `timer list`)
        id: Option<u64>,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Enable a timer by its timer id
    Enable {
        /// Device serial number
        sn: Option<String>,
        /// Timer id (see `timer list`)
        id: Option<u64>,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Disable a timer by its timer id (keeps it configured but inactive)
    Disable {
        /// Device serial number
        sn: Option<String>,
        /// Timer id (see `timer list`)
        id: Option<u64>,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
}

#[derive(Subcommand)]
pub enum CountdownCmd {
    /// Show the running countdown for an outlet
    Status {
        /// Device serial number
        sn: Option<String>,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Start a countdown that flips the outlet after `count` seconds
    Start {
        /// Device serial number
        sn: Option<String>,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
        /// Seconds until the outlet switches
        #[arg(long)]
        count: Option<u64>,
        /// Resulting state when the countdown ends: 0 = off, 1 = on (default: 0)
        #[arg(long, default_value_t = 0)]
        action: u8,
    },
    /// Stop any running countdown
    Stop {
        /// Device serial number
        sn: Option<String>,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
}

/// `--interactive`: type the arguments this command line left out.
pub fn fill(cmd: &mut PlugCmd) -> Result<()> {
    match cmd {
        PlugCmd::Status { sn, .. } => fill_sn(sn, "oray-tools wakeup plug status <sn>"),
        PlugCmd::On { sn, .. } => fill_sn(sn, "oray-tools wakeup plug on <sn>"),
        PlugCmd::Off { sn, .. } => fill_sn(sn, "oray-tools wakeup plug off <sn>"),
        PlugCmd::Logs { sn, .. } => fill_sn(sn, "oray-tools wakeup plug logs <sn>"),
        PlugCmd::Led { sn, state } => {
            prompt::require_terminal(
                &[("<SN>", sn.is_none()), ("<STATE>", state.is_none())],
                "oray-tools wakeup plug led <sn> <state>",
            )?;
            prompt::fill_str(sn, SN_LABEL, None)?;
            prompt::fill_checked(state, "LED state (on/off)", None, |value| {
                crate::support::parse_on_off(value).map(|_| ())
            })
        }
        PlugCmd::PowerOnRestore { sn, state } => {
            prompt::require_terminal(
                &[("<SN>", sn.is_none()), ("<STATE>", state.is_none())],
                "oray-tools wakeup plug power-on-restore <sn> <state>",
            )?;
            prompt::fill_str(sn, SN_LABEL, None)?;
            prompt::fill_parsed(state, "Power-on state (0 = off, 2 = keep last)")
        }
        PlugCmd::Timer { sub } => fill_timer(sub),
        PlugCmd::Countdown { sub } => fill_countdown(sub),
    }
}

/// `--interactive` for `wakeup plug timer <SUB>`.
fn fill_timer(cmd: &mut TimerCmd) -> Result<()> {
    match cmd {
        TimerCmd::List { sn, .. } => fill_sn(sn, "oray-tools wakeup plug timer list <sn>"),
        TimerCmd::Add { sn, time, .. } => {
            prompt::require_terminal(
                &[("<SN>", sn.is_none()), ("<TIME>", time.is_none())],
                "oray-tools wakeup plug timer add <sn> --time <TIME>",
            )?;
            prompt::fill_str(sn, SN_LABEL, None)?;
            prompt::fill_checked(time, "Timer time (e.g. 19:25)", None, |value| {
                if crate::support::parse_local_time(value).is_some() {
                    Ok(())
                } else {
                    bail!(
                        "use a local clock time like 19:25, or minutes of the day 0-1439 \
                         (e.g. 480 = 08:00), got '{value}'"
                    )
                }
            })
        }
        TimerCmd::Remove { sn, id, .. } => {
            fill_sn_id(sn, id, "oray-tools wakeup plug timer remove <sn> <id>")
        }
        TimerCmd::Enable { sn, id, .. } => {
            fill_sn_id(sn, id, "oray-tools wakeup plug timer enable <sn> <id>")
        }
        TimerCmd::Disable { sn, id, .. } => {
            fill_sn_id(sn, id, "oray-tools wakeup plug timer disable <sn> <id>")
        }
    }
}

/// Prompt for `<SN>` and the `<ID>` of a timer.
fn fill_sn_id(sn: &mut Option<String>, id: &mut Option<u64>, example: &str) -> Result<()> {
    prompt::require_terminal(&[("<SN>", sn.is_none()), ("<ID>", id.is_none())], example)?;
    prompt::fill_str(sn, SN_LABEL, None)?;
    prompt::fill_parsed(id, "Timer id (see `timer list`)")
}

/// `--interactive` for `wakeup plug countdown <SUB>`.
fn fill_countdown(cmd: &mut CountdownCmd) -> Result<()> {
    match cmd {
        CountdownCmd::Status { sn, .. } => {
            fill_sn(sn, "oray-tools wakeup plug countdown status <sn>")
        }
        CountdownCmd::Start { sn, count, .. } => {
            prompt::require_terminal(
                &[("<SN>", sn.is_none()), ("<COUNT>", count.is_none())],
                "oray-tools wakeup plug countdown start <sn> --count <COUNT>",
            )?;
            prompt::fill_str(sn, SN_LABEL, None)?;
            prompt::fill_parsed(count, "Countdown seconds")
        }
        CountdownCmd::Stop { sn, .. } => fill_sn(sn, "oray-tools wakeup plug countdown stop <sn>"),
    }
}
/// `--json` bodies of the mutating plug commands (module docs carry the
/// contract). Kept as tiny builders so the shapes are unit-testable without
/// a network round trip.
fn json_plug_switch(sn: &str, index: usize, on: bool) -> serde_json::Value {
    let status = if on { "on" } else { "off" };
    serde_json::json!({ "ok": true, "sn": sn, "index": index, "status": status })
}

fn json_led(sn: &str, on: bool) -> serde_json::Value {
    let led = if on { "on" } else { "off" };
    serde_json::json!({ "ok": true, "sn": sn, "led": led })
}

fn json_power_on_restore(sn: &str, state: u32) -> serde_json::Value {
    serde_json::json!({ "ok": true, "sn": sn, "state": state })
}

fn json_countdown_start(sn: &str, index: usize, count: u64, action: u8) -> serde_json::Value {
    serde_json::json!({ "ok": true, "sn": sn, "index": index, "count": count, "action": action })
}

fn json_countdown_stop(sn: &str, index: usize) -> serde_json::Value {
    serde_json::json!({ "ok": true, "sn": sn, "index": index })
}

fn json_timer_removed(sn: &str, index: usize, id: u64) -> serde_json::Value {
    serde_json::json!({ "ok": true, "sn": sn, "index": index, "timer_id": id })
}

/// `timer enable|disable` succeeded. Carries the uniform `ok` marker like
/// every other mutating command — the one deliberate, documented exception to
/// "an existing shape stays byte-identical": the added key is additive (only
/// a `deny_unknown_fields` decoder could notice) and makes the success
/// contract uniform for consumers.
fn json_timer_enabled(sn: &str, index: usize, id: u64, enabled: bool) -> serde_json::Value {
    serde_json::json!({ "ok": true, "sn": sn, "index": index, "timer_id": id, "enabled": enabled })
}

pub fn run(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    sub: PlugCmd,
    refresh_on_expired: bool,
    json: bool,
    tz: Option<i64>,
) -> Result<()> {
    let server = cfg.server();
    let plug = PlugApi::new(http.clone(), &server.slapi_base);
    // The plumbing every helper below needs, threaded once instead of per call.
    let mut ctx = Ctx {
        http,
        cfg,
        path,
        plug: &plug,
        refresh_on_expired,
        json,
        tz,
    };
    match sub {
        PlugCmd::Status { sn, index } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            let resp = ctx.with_tok(|tok| plug.get_status(tok, sn, index))?;
            emit_json(json, &resp)?;
            if !json {
                if let Some(ports) = &resp.response {
                    for p in ports {
                        let state = if p.status == 1 { "ON" } else { "OFF" };
                        println!("sn={sn} index={} status={state}", p.index);
                    }
                } else {
                    println!("sn={sn} index={index} status=<<unknown>>");
                }
            }
            Ok(())
        }
        PlugCmd::On { sn, index } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            ctx.with_tok(|tok| plug.set_status(tok, sn, index, true))?;
            emit_json(json, &json_plug_switch(sn, index, true))?;
            if !json {
                println!("sn={sn} index={index} ON");
            }
            Ok(())
        }
        PlugCmd::Off { sn, index } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            ctx.with_tok(|tok| plug.set_status(tok, sn, index, false))?;
            emit_json(json, &json_plug_switch(sn, index, false))?;
            if !json {
                println!("sn={sn} index={index} OFF");
            }
            Ok(())
        }
        PlugCmd::Logs {
            sn,
            index,
            since,
            until,
            page,
        } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            let now = Utc::now().timestamp();
            // Absolute wall-clock bounds are interpreted in the plug's
            // timezone (--tz > config tz > machine local); ago bounds are
            // duration math on `now` and need no timezone.
            let absolute = since
                .as_deref()
                .is_some_and(|s| parse_ago_secs(s).is_none())
                || until
                    .as_deref()
                    .is_some_and(|u| parse_ago_secs(u).is_none());
            let tz_min = absolute.then(|| resolve_tz(ctx.cfg, ctx.tz)).transpose()?;
            // Rendering timezone: an explicit --tz/config tz always wins;
            // otherwise reuse the offset already resolved for an absolute
            // window (so displayed times match the bounds), else machine local.
            let display_tz = if tz.is_some() || ctx.cfg.tz.is_some() {
                Some(resolve_tz(ctx.cfg, ctx.tz)?)
            } else {
                tz_min
            };
            let start = match since {
                Some(ref s) => Some(resolve_time_bound_tz(s, true, now, tz_min)?),
                None => None,
            };
            let end = match until {
                Some(ref u) => Some(resolve_time_bound_tz(u, false, now, tz_min)?),
                None => None,
            };
            if let (Some(start), Some(end)) = (start, end)
                && start > end
            {
                bail!(
                    "invalid window: --since starts after --until ends (since={since:?}, until={until:?})"
                );
            }
            let keep = |log: &StatusLog| {
                let t = log.createtime;
                start.is_none_or(|s| t >= s) && end.is_none_or(|e| t <= e)
            };
            let mut all: Vec<StatusLog> = Vec::new();
            match page {
                Some(p) => {
                    let data = fetch_logs_page(&mut ctx, sn, p)?;
                    all.extend(data.logs.into_iter().filter(&keep));
                }
                None => {
                    // Pages are newest-first. Page 1 reports the total, then we
                    // binary-search the first/last page that can intersect the
                    // window (probing ~log2(pages)) and read only that slice,
                    // instead of walking from page 1.
                    let first = fetch_logs_page(&mut ctx, sn, 1)?;
                    let total = first.totalpage.max(1);
                    let mut cached: Vec<Option<StatusLogsData>> =
                        (0..=total).map(|_| None).collect();
                    cached[1] = Some(first);

                    // p_low: first page whose oldest event is at or before the
                    // upper bound (nothing on earlier pages can match).
                    let p_low = match end {
                        None => Some(1),
                        Some(e) => {
                            let (mut lo, mut hi) = (1u32, total);
                            let mut found = None;
                            while lo <= hi {
                                let mid = lo + (hi - lo) / 2;
                                let data = load_logs_page(&mut ctx, sn, mid, &mut cached)?;
                                let ok =
                                    logs_page_span(data).is_some_and(|(_, oldest)| oldest <= e);
                                if ok {
                                    found = Some(mid);
                                    hi = mid.saturating_sub(1);
                                } else {
                                    lo = mid + 1;
                                }
                            }
                            found
                        }
                    };

                    // p_high: last page whose newest event is at or after the
                    // lower bound (nothing on later pages can match).
                    let p_high = match start {
                        None => Some(total),
                        Some(s) => {
                            let (mut lo, mut hi) = (1u32, total);
                            let mut found = None;
                            while lo <= hi {
                                let mid = lo + (hi - lo) / 2;
                                let data = load_logs_page(&mut ctx, sn, mid, &mut cached)?;
                                let ok =
                                    logs_page_span(data).is_some_and(|(newest, _)| newest >= s);
                                if ok {
                                    found = Some(mid);
                                    lo = mid + 1;
                                } else {
                                    hi = mid.saturating_sub(1);
                                }
                            }
                            found
                        }
                    };

                    if let (Some(lo), Some(hi)) = (p_low, p_high)
                        && lo <= hi
                    {
                        for p in lo..=hi {
                            let i = p as usize;
                            if cached[i].is_none() {
                                cached[i] = Some(fetch_logs_page(&mut ctx, sn, p)?);
                            }
                            if let Some(data) = cached[i].take() {
                                all.extend(data.logs.into_iter().filter(|l| keep(l)));
                            }
                        }
                    }
                }
            }
            // `--index` narrows the result client-side; without it the query
            // returns everything the API sent (the historical behaviour).
            let all = filter_logs_by_index(all, index);
            if json {
                let arr: Vec<serde_json::Value> = all
                    .iter()
                    .map(|l| {
                        let mut value = serde_json::to_value(l).unwrap_or(serde_json::Value::Null);
                        if let Some(obj) = value.as_object_mut() {
                            obj.insert(
                                "createtime_tz".to_string(),
                                serde_json::Value::String(render_ts(l.createtime, display_tz)),
                            );
                        }
                        value
                    })
                    .collect();
                emit_json(true, &arr)?;
            } else {
                for line in logs_text_lines(&all, display_tz) {
                    println!("{line}");
                }
            }
            Ok(())
        }
        PlugCmd::Timer { sub } => do_timer(&mut ctx, sub),
        PlugCmd::Countdown { sub } => do_countdown(&mut ctx, sub),
        PlugCmd::Led { sn, state } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            let state = state.as_deref().context("missing <STATE>")?;
            let enabled = parse_on_off(state)?;
            ctx.with_tok(|tok| plug.set_led(tok, sn, enabled))?;
            emit_json(json, &json_led(sn, enabled))?;
            if !json {
                println!("sn={sn} led {}", if enabled { "ON" } else { "OFF" });
            }
            Ok(())
        }
        PlugCmd::PowerOnRestore { sn, state } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            let state = state.context("missing <STATE>")?;
            if state != 0 && state != 2 {
                bail!("power-on-restore state must be 0 (off) or 2 (keep last state), got {state}");
            }
            ctx.with_tok(|tok| plug.set_dfltstat(tok, sn, state))?;
            emit_json(json, &json_power_on_restore(sn, state))?;
            if !json {
                println!("sn={sn} power-on-restore={state}");
            }
            Ok(())
        }
    }
}

fn do_timer(ctx: &mut Ctx, sub: TimerCmd) -> Result<()> {
    let plug = ctx.plug;
    let json = ctx.json;
    match sub {
        TimerCmd::List { sn, index } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            let tz = resolve_tz(ctx.cfg, ctx.tz)?;
            let resp = ctx.with_tok(|tok| plug.timer_list(tok, sn, index))?;
            // The plug stores UTC times/weekday bits; present them in local
            // time with a Monday-first weekday mask.
            let rows: Vec<_> = resp
                .timer
                .iter()
                .map(|t| {
                    let time = cloud_to_local_time(t.time.unwrap_or(0), tz);
                    let repeat = cloud_to_local_mask(t.repeat.unwrap_or(0), time, tz);
                    (t, time, repeat)
                })
                .collect();
            if json {
                let tz_name = tz_label(tz);
                let arr: Vec<serde_json::Value> = rows
                    .iter()
                    .map(|(t, time, repeat)| {
                        serde_json::json!({
                            "timer_id": t.timer_id,
                            "time": time,
                            "time_local": format!("{:02}:{:02}", time / 60, time % 60),
                            "action": t.action,
                            "repeat": repeat,
                            "days": mask_days(*repeat),
                            "enabled": t.enabled,
                            "tz": tz_name,
                        })
                    })
                    .collect();
                emit_json(true, &arr)?;
                return Ok(());
            }
            if resp.timer.is_empty() {
                println!("sn={sn} index={index} no timers");
            }
            for (t, time, repeat) in &rows {
                let id = t
                    .timer_id
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into());
                let state = match t.enabled {
                    Some(1) => "enabled",
                    _ => "disabled",
                };
                println!(
                    "sn={sn} index={index} timer_id={id} time={:02}:{:02} days={} action={} {state} ({})",
                    time / 60,
                    time % 60,
                    mask_days(*repeat),
                    t.action.unwrap_or(0),
                    tz_label(tz)
                );
            }
            Ok(())
        }
        TimerCmd::Add {
            sn,
            index,
            time,
            action,
            repeat,
            disabled,
        } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            let time = time.as_deref().context("missing <TIME>")?;
            if action > 1 {
                bail!("timer action must be 0 (off) or 1 (on)");
            }
            let time = crate::support::parse_local_time(time).ok_or_else(|| {
                anyhow::anyhow!(
                    "invalid --time '{time}': use a local clock time like 19:25, or minutes of the day 0-1439 (e.g. 480 = 08:00)"
                )
            })?;
            let tz = resolve_tz(ctx.cfg, ctx.tz)?;
            let timer = PlugTimer {
                timer_id: None,
                time: Some(local_to_cloud_time(time, tz)),
                action: Some(action),
                repeat: Some(local_to_cloud_repeat(repeat, time, tz)),
                enabled: Some(if disabled { 0 } else { 1 }),
            };
            let resp = ctx.with_tok(|tok| plug.timer_add(tok, sn, index, &timer))?;
            emit_json(json, &resp)?;
            if !json {
                let id = resp
                    .timer_id
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into());
                println!(
                    "sn={sn} index={index} timer added: id={id} time={:02}:{:02} days={} action={} {} ({})",
                    time / 60,
                    time % 60,
                    mask_days(repeat),
                    action,
                    if disabled { "disabled" } else { "enabled" },
                    tz_label(tz)
                );
            }
            Ok(())
        }
        TimerCmd::Remove { sn, id, index } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            let id = id.context("missing <ID>")?;
            let resp = ctx.with_tok(|tok| plug.timer_list(tok, sn, index))?;
            let found = resp.timer.into_iter().find(|t| t.timer_id == Some(id));
            match found {
                Some(t) => {
                    ctx.with_tok(|tok| {
                        plug.timer_del(
                            tok,
                            sn,
                            index,
                            id,
                            t.repeat.unwrap_or(0),
                            t.time.unwrap_or(0),
                        )
                    })?;
                    emit_json(json, &json_timer_removed(sn, index, id))?;
                    if !json {
                        println!("sn={sn} index={index} timer {id} removed");
                    }
                    Ok(())
                }
                // Both modes fail the same way: `error: ...` on stderr, exit 1.
                None => bail!("timer {id} not found on sn={sn} index={index}"),
            }
        }
        TimerCmd::Enable { sn, id, index } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            let id = id.context("missing <ID>")?;
            set_timer_enabled(ctx, sn, index, id, true)
        }
        TimerCmd::Disable { sn, id, index } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            let id = id.context("missing <ID>")?;
            set_timer_enabled(ctx, sn, index, id, false)
        }
    }
}

/// Toggle a timer's enabled state, keeping its other settings intact.
fn set_timer_enabled(ctx: &mut Ctx, sn: &str, index: usize, id: u64, enabled: bool) -> Result<()> {
    let json = ctx.json;
    let plug = ctx.plug;
    let resp = ctx.with_tok(|tok| plug.timer_list(tok, sn, index))?;
    let found = resp.timer.into_iter().find(|t| t.timer_id == Some(id));
    let Some(t) = found else {
        // Both modes fail the same way: `error: ...` on stderr, exit 1.
        bail!("timer {id} not found on sn={sn} index={index}");
    };
    ctx.with_tok(|tok| {
        plug.timer_set(
            tok,
            sn,
            index,
            id,
            enabled,
            t.action.unwrap_or(1),
            t.repeat.unwrap_or(0),
            t.time.unwrap_or(0),
        )
    })?;
    emit_json(json, &json_timer_enabled(sn, index, id, enabled))?;
    if !json {
        println!(
            "sn={sn} index={index} timer {id} {}",
            if enabled { "enabled" } else { "disabled" }
        );
    }
    Ok(())
}

fn do_countdown(ctx: &mut Ctx, sub: CountdownCmd) -> Result<()> {
    let plug = ctx.plug;
    let json = ctx.json;
    match sub {
        CountdownCmd::Status { sn, index } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            let resp = ctx.with_tok(|tok| plug.cntdown_get(tok, sn, index))?;
            emit_json(json, &resp)?;
            if !json {
                match resp.remain {
                    Some(remain) if remain > 0 => println!(
                        "sn={sn} index={index} countdown running: remain={remain}s total={}s action={}",
                        resp.count.unwrap_or(0),
                        resp.action.unwrap_or(0)
                    ),
                    _ => println!("sn={sn} index={index} no countdown running"),
                }
            }
            Ok(())
        }
        CountdownCmd::Start {
            sn,
            index,
            count,
            action,
        } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            let count = count.context("missing <COUNT>")?;
            if action > 1 {
                bail!("countdown action must be 0 (off) or 1 (on)");
            }
            if count == 0 {
                bail!("countdown count must be > 0 seconds");
            }
            ctx.with_tok(|tok| plug.cntdown_start(tok, sn, index, action, count))?;
            emit_json(json, &json_countdown_start(sn, index, count, action))?;
            if !json {
                println!(
                    "sn={sn} index={index} countdown started: {count}s -> {}",
                    if action == 1 { "ON" } else { "OFF" }
                );
            }
            Ok(())
        }
        CountdownCmd::Stop { sn, index } => {
            let sn = sn.as_deref().context("missing <SN>")?;
            ctx.with_tok(|tok| plug.cntdown_stop(tok, sn, index))?;
            emit_json(json, &json_countdown_stop(sn, index))?;
            if !json {
                println!("sn={sn} index={index} countdown stopped");
            }
            Ok(())
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn time_local_to_cloud() {
        let tz = 480;
        // 16:00 local = 08:00 UTC
        assert_eq!(local_to_cloud_time(960, tz), 480);
        // 02:00 local = 18:00 UTC on the previous day
        assert_eq!(local_to_cloud_time(120, tz), 1080);
        // 08:00 local = 00:00 UTC
        assert_eq!(local_to_cloud_time(480, tz), 0);
        // 18:00 local = 10:00 UTC
        assert_eq!(local_to_cloud_time(1080, tz), 600);
        // west of UTC: 02:00 local (UTC-5) = 07:00 UTC same day
        assert_eq!(local_to_cloud_time(120, -300), 420);
    }

    #[test]
    fn time_cloud_to_local() {
        let tz = 480;
        assert_eq!(cloud_to_local_time(480, tz), 960);
        assert_eq!(cloud_to_local_time(1080, tz), 120);
        assert_eq!(cloud_to_local_time(0, tz), 480);
    }

    #[test]
    fn repeat_vectors_matching_app() {
        let tz = 480;
        // 02:00 local Mon-Fri -> UTC 18:00 Sun-Thu
        assert_eq!(local_to_cloud_repeat(31, 120, tz), 31);
        // 16:00 local Sun-Thu (79) -> UTC 08:00 Sun-Thu
        assert_eq!(local_to_cloud_repeat(79, 960, tz), 31);
        // 08:00 local Mon-Fri -> UTC 00:00 Mon-Fri (62)
        assert_eq!(local_to_cloud_repeat(31, 480, tz), 62);
        // 18:00 local Mon-Fri -> UTC 10:00 Mon-Fri (62)
        assert_eq!(local_to_cloud_repeat(31, 1080, tz), 62);
        // west of UTC (UTC-5): local 23:00 fires 04:00 UTC next day -> shift +1
        // local Mon-Fri (31) at 23:00 -> UTC Tue-Sat (bits 2..6 = 124)
        assert_eq!(local_to_cloud_repeat(31, 23 * 60, -300), 124);
    }

    #[test]
    fn repeat_roundtrip() {
        let roundtrip = |mask: u8, local_min: u64, tz: i64| {
            let cloud = local_to_cloud_repeat(mask, local_min, tz);
            let cloud_min = local_to_cloud_time(local_min, tz);
            let local_back = cloud_to_local_time(cloud_min, tz);
            assert_eq!(cloud_to_local_mask(cloud, local_back, tz), mask);
        };
        roundtrip(79, 960, 480);
        roundtrip(31, 120, 480);
        roundtrip(31, 480, 480);
        roundtrip(31, 1380, -300);
        roundtrip(21, 120, -300);
    }

    #[test]
    fn mask_days_labels() {
        assert_eq!(mask_days(0), "只一次");
        assert_eq!(mask_days(31), "周一周二周三周四周五");
        assert_eq!(mask_days(79), "周一周二周三周四周日");
    }

    /// A synthetic log entry for the `--index` filter / text-render tests.
    fn log(index: i32) -> StatusLog {
        StatusLog {
            event: "on".to_string(),
            status: 1,
            index,
            // 2026-09-02 01:56:44 UTC
            createtime: 1_788_314_204,
            createtime_format: "2026-09-02 09:56:44".to_string(),
        }
    }

    #[test]
    fn logs_index_filter_without_index_keeps_everything() {
        let out = filter_logs_by_index(vec![log(0), log(1), log(0)], None);
        let kept: Vec<i32> = out.iter().map(|l| l.index).collect();
        assert_eq!(kept, vec![0, 1, 0]);
    }

    #[test]
    fn logs_index_filter_selects_one_port() {
        let out = filter_logs_by_index(vec![log(0), log(1), log(0)], Some(1));
        let kept: Vec<i32> = out.iter().map(|l| l.index).collect();
        assert_eq!(kept, vec![1]);
    }

    #[test]
    fn logs_index_filter_without_match_is_empty() {
        assert!(filter_logs_by_index(vec![log(0)], Some(2)).is_empty());
        assert!(filter_logs_by_index(Vec::<StatusLog>::new(), Some(0)).is_empty());
    }

    #[test]
    fn logs_text_empty_result_gets_a_notice() {
        // The empty branch of the filtering: what a query that matched
        // nothing must print instead of staying silent.
        let empty = filter_logs_by_index(vec![log(0)], Some(9));
        assert_eq!(logs_text_lines(&empty, Some(0)), vec!["no matching events"]);
        assert_eq!(logs_text_lines(&[], None), vec!["no matching events"]);
    }

    #[test]
    fn logs_text_renders_each_entry() {
        let lines = logs_text_lines(&[log(0), log(1)], Some(0));
        assert_eq!(
            lines,
            vec![
                "2026-09-02 01:56:44 UTC+00:00 index=0 ON on",
                "2026-09-02 01:56:44 UTC+00:00 index=1 ON on",
            ]
        );
    }

    #[test]
    fn json_shape_plug_switch_led_and_restore() {
        assert_eq!(
            serde_json::to_value(json_plug_switch("SN1", 2, true)).unwrap(),
            serde_json::json!({ "ok": true, "sn": "SN1", "index": 2, "status": "on" })
        );
        assert_eq!(
            serde_json::to_value(json_plug_switch("SN1", 2, false)).unwrap(),
            serde_json::json!({ "ok": true, "sn": "SN1", "index": 2, "status": "off" })
        );
        assert_eq!(
            serde_json::to_value(json_led("SN1", true)).unwrap(),
            serde_json::json!({ "ok": true, "sn": "SN1", "led": "on" })
        );
        assert_eq!(
            serde_json::to_value(json_led("SN1", false)).unwrap(),
            serde_json::json!({ "ok": true, "sn": "SN1", "led": "off" })
        );
        assert_eq!(
            serde_json::to_value(json_power_on_restore("SN1", 0)).unwrap(),
            serde_json::json!({ "ok": true, "sn": "SN1", "state": 0 })
        );
        assert_eq!(
            serde_json::to_value(json_power_on_restore("SN1", 2)).unwrap(),
            serde_json::json!({ "ok": true, "sn": "SN1", "state": 2 })
        );
    }

    #[test]
    fn json_shape_countdown_start_and_stop() {
        assert_eq!(
            serde_json::to_value(json_countdown_start("SN1", 0, 600, 1)).unwrap(),
            serde_json::json!({
                "ok": true, "sn": "SN1", "index": 0, "count": 600, "action": 1
            })
        );
        assert_eq!(
            serde_json::to_value(json_countdown_start("SN1", 0, 30, 0)).unwrap(),
            serde_json::json!({
                "ok": true, "sn": "SN1", "index": 0, "count": 30, "action": 0
            })
        );
        assert_eq!(
            serde_json::to_value(json_countdown_stop("SN1", 0)).unwrap(),
            serde_json::json!({ "ok": true, "sn": "SN1", "index": 0 })
        );
    }

    #[test]
    fn json_shape_timer_remove_and_enable_disable() {
        assert_eq!(
            serde_json::to_value(json_timer_removed("SN1", 0, 7)).unwrap(),
            serde_json::json!({ "ok": true, "sn": "SN1", "index": 0, "timer_id": 7 })
        );
        assert_eq!(
            serde_json::to_value(json_timer_enabled("SN1", 0, 7, true)).unwrap(),
            serde_json::json!({
                "ok": true, "sn": "SN1", "index": 0, "timer_id": 7, "enabled": true
            })
        );
        assert_eq!(
            serde_json::to_value(json_timer_enabled("SN1", 0, 7, false)).unwrap(),
            serde_json::json!({
                "ok": true, "sn": "SN1", "index": 0, "timer_id": 7, "enabled": false
            })
        );
    }
}
