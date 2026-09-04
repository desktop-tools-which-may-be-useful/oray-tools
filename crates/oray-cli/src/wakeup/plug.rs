//! `wakeup plug` command group: smart-plug specific controls.
//!
//! Outlet switching/status, logs, timers, countdown, LED and power-restore.
//!
//! Timer scheduling: the plug API stores schedule times in UTC minutes of the
//! day and weekday bits over UTC days, while the user-facing (App) semantics
//! are local time with weekdays starting Monday. The helpers below convert
//! between the two representations given a timezone offset in minutes.

use crate::config::Config;
use crate::support::{emit_json, parse_duration, parse_on_off, resolve_tz, with_token};
use anyhow::{Result, bail};
use chrono::Utc;
use clap::Subcommand;
use oray_core::wakeup::plug::{PlugApi, PlugTimer};
use reqwest::blocking::Client as HttpClient;
use std::path::PathBuf;

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
        sn: String,
        /// Port index (default: 0, the master switch)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Turn the plug on
    On {
        /// Device serial number
        sn: String,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Turn the plug off
    Off {
        /// Device serial number
        sn: String,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Fetch status-change logs
    Logs {
        /// Device serial number
        sn: String,
        /// Port index
        #[arg(long, default_value_t = 0)]
        index: usize,
        /// Only include events newer than this (e.g. 30m, 2h, 1d); also fetches
        /// all pages up to the newest matching page
        #[arg(long)]
        since: Option<String>,
        /// Fetch a single specific page instead of all pages
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
        sn: String,
        /// on or off
        state: String,
    },
    /// Set the state after a power loss: 0 = off, 2 = keep last state
    PowerOnRestore {
        /// Device serial number
        sn: String,
        /// 0 (off) or 2 (keep last state)
        state: u32,
    },
}

#[derive(Subcommand)]
pub enum TimerCmd {
    /// List timers for an outlet
    List {
        /// Device serial number
        sn: String,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Add a timer that fires at a local clock time on the matching days
    Add {
        /// Device serial number
        sn: String,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
        /// Local minutes of the day (0-1439) when the timer fires, e.g. 480 = 08:00
        #[arg(long)]
        time: u64,
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
        sn: String,
        /// Timer id (see `timer list`)
        id: u64,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Enable a timer by its timer id
    Enable {
        /// Device serial number
        sn: String,
        /// Timer id (see `timer list`)
        id: u64,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Disable a timer by its timer id (keeps it configured but inactive)
    Disable {
        /// Device serial number
        sn: String,
        /// Timer id (see `timer list`)
        id: u64,
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
        sn: String,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Start a countdown that flips the outlet after `count` seconds
    Start {
        /// Device serial number
        sn: String,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
        /// Seconds until the outlet switches
        #[arg(long)]
        count: u64,
        /// Resulting state when the countdown ends: 0 = off, 1 = on (default: 0)
        #[arg(long, default_value_t = 0)]
        action: u8,
    },
    /// Stop any running countdown
    Stop {
        /// Device serial number
        sn: String,
        /// Port index (default: 0)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
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
    match sub {
        PlugCmd::Status { sn, index } => {
            let resp = with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.get_status(tok, &sn, index)
            })?;
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
            with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.set_status(tok, &sn, index, true)
            })?;
            if !json {
                println!("sn={sn} index={index} ON");
            }
            Ok(())
        }
        PlugCmd::Off { sn, index } => {
            with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.set_status(tok, &sn, index, false)
            })?;
            if !json {
                println!("sn={sn} index={index} OFF");
            }
            Ok(())
        }
        PlugCmd::Logs {
            sn,
            index: _index,
            since,
            page,
        } => {
            let pages = match page {
                Some(p) => vec![p],
                None => {
                    // read all pages (newest first, page 1)
                    let first = with_token(http, cfg, path, refresh_on_expired, |tok| {
                        plug.status_logs(tok, &sn, 1)
                    })?;
                    let mut pages = vec![1];
                    let total = first.totalpage;
                    if total > 1 {
                        pages.extend(2..=total);
                    }
                    pages
                }
            };
            let cutoff = since.as_deref().map(parse_duration);
            let mut all = Vec::new();
            for p in pages {
                let data = with_token(http, cfg, path, refresh_on_expired, |tok| {
                    plug.status_logs(tok, &sn, p)
                })?;
                for log in data.logs {
                    let keep = match cutoff {
                        Some(secs) => log.createtime >= Utc::now().timestamp() - secs,
                        None => true,
                    };
                    if keep {
                        all.push(log);
                    }
                }
            }
            emit_json(json, &all)?;
            if !json {
                for l in all {
                    let state = if l.status == 1 { "ON" } else { "OFF" };
                    println!(
                        "{} index={} {} {}",
                        l.createtime_format, l.index, state, l.event
                    );
                }
            }
            Ok(())
        }
        PlugCmd::Timer { sub } => {
            do_timer(http, cfg, path, &plug, sub, refresh_on_expired, json, tz)
        }
        PlugCmd::Countdown { sub } => {
            do_countdown(http, cfg, path, &plug, sub, refresh_on_expired, json)
        }
        PlugCmd::Led { sn, state } => {
            let enabled = parse_on_off(&state)?;
            with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.set_led(tok, &sn, enabled)
            })?;
            if !json {
                println!("sn={sn} led {}", if enabled { "ON" } else { "OFF" });
            }
            Ok(())
        }
        PlugCmd::PowerOnRestore { sn, state } => {
            if state != 0 && state != 2 {
                bail!("power-on-restore state must be 0 (off) or 2 (keep last state), got {state}");
            }
            with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.set_dfltstat(tok, &sn, state)
            })?;
            if !json {
                println!("sn={sn} power-on-restore={state}");
            }
            Ok(())
        }
    }
}

fn do_timer(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    plug: &PlugApi,
    sub: TimerCmd,
    refresh_on_expired: bool,
    json: bool,
    tz_arg: Option<i64>,
) -> Result<()> {
    match sub {
        TimerCmd::List { sn, index } => {
            let tz = resolve_tz(cfg, tz_arg)?;
            let resp = with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.timer_list(tok, &sn, index)
            })?;
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
                    "sn={sn} index={index} timer_id={id} time={:02}:{:02} days={} action={} {state}",
                    time / 60,
                    time % 60,
                    mask_days(*repeat),
                    t.action.unwrap_or(0)
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
            if action > 1 {
                bail!("timer action must be 0 (off) or 1 (on)");
            }
            if time > 1439 {
                bail!("timer time must be local minutes of the day (0-1439), got {time}");
            }
            let tz = resolve_tz(cfg, tz_arg)?;
            let timer = PlugTimer {
                timer_id: None,
                time: Some(local_to_cloud_time(time, tz)),
                action: Some(action),
                repeat: Some(local_to_cloud_repeat(repeat, time, tz)),
                enabled: Some(if disabled { 0 } else { 1 }),
            };
            let resp = with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.timer_add(tok, &sn, index, &timer)
            })?;
            emit_json(json, &resp)?;
            if !json {
                let id = resp
                    .timer_id
                    .map(|v| v.to_string())
                    .unwrap_or_else(|| "-".into());
                println!(
                    "sn={sn} index={index} timer added: id={id} time={:02}:{:02} days={} action={} {}",
                    time / 60,
                    time % 60,
                    mask_days(repeat),
                    action,
                    if disabled { "disabled" } else { "enabled" }
                );
            }
            Ok(())
        }
        TimerCmd::Remove { sn, id, index } => {
            let resp = with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.timer_list(tok, &sn, index)
            })?;
            let found = resp.timer.into_iter().find(|t| t.timer_id == Some(id));
            match found {
                Some(t) => {
                    with_token(http, cfg, path, refresh_on_expired, |tok| {
                        plug.timer_del(
                            tok,
                            &sn,
                            index,
                            id,
                            t.repeat.unwrap_or(0),
                            t.time.unwrap_or(0),
                        )
                    })?;
                    if !json {
                        println!("sn={sn} index={index} timer {id} removed");
                    }
                    Ok(())
                }
                None => {
                    emit_json(
                        json,
                        &serde_json::json!({ "removed": false, "sn": sn, "timer_id": id }),
                    )?;
                    if !json {
                        bail!("timer {id} not found on sn={sn} index={index}");
                    }
                    Ok(())
                }
            }
        }
        TimerCmd::Enable { sn, id, index } => set_timer_enabled(
            http,
            cfg,
            path,
            plug,
            &sn,
            index,
            id,
            true,
            refresh_on_expired,
            json,
        ),
        TimerCmd::Disable { sn, id, index } => set_timer_enabled(
            http,
            cfg,
            path,
            plug,
            &sn,
            index,
            id,
            false,
            refresh_on_expired,
            json,
        ),
    }
}

/// Toggle a timer's enabled state, keeping its other settings intact.
#[allow(clippy::too_many_arguments)]
fn set_timer_enabled(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    plug: &PlugApi,
    sn: &str,
    index: usize,
    id: u64,
    enabled: bool,
    refresh_on_expired: bool,
    json: bool,
) -> Result<()> {
    let resp = with_token(http, cfg, path, refresh_on_expired, |tok| {
        plug.timer_list(tok, sn, index)
    })?;
    let found = resp.timer.into_iter().find(|t| t.timer_id == Some(id));
    match found {
        Some(t) => {
            with_token(http, cfg, path, refresh_on_expired, |tok| {
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
            emit_json(
                json,
                &serde_json::json!({ "sn": sn, "index": index, "timer_id": id, "enabled": enabled }),
            )?;
            if !json {
                println!(
                    "sn={sn} index={index} timer {id} {}",
                    if enabled { "enabled" } else { "disabled" }
                );
            }
            Ok(())
        }
        None => {
            if !json {
                bail!("timer {id} not found on sn={sn} index={index}");
            }
            Ok(())
        }
    }
}

fn do_countdown(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    plug: &PlugApi,
    sub: CountdownCmd,
    refresh_on_expired: bool,
    json: bool,
) -> Result<()> {
    match sub {
        CountdownCmd::Status { sn, index } => {
            let resp = with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.cntdown_get(tok, &sn, index)
            })?;
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
            if action > 1 {
                bail!("countdown action must be 0 (off) or 1 (on)");
            }
            if count == 0 {
                bail!("countdown count must be > 0 seconds");
            }
            with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.cntdown_start(tok, &sn, index, action, count)
            })?;
            if !json {
                println!(
                    "sn={sn} index={index} countdown started: {count}s -> {}",
                    if action == 1 { "ON" } else { "OFF" }
                );
            }
            Ok(())
        }
        CountdownCmd::Stop { sn, index } => {
            with_token(http, cfg, path, refresh_on_expired, |tok| {
                plug.cntdown_stop(tok, &sn, index)
            })?;
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
}
