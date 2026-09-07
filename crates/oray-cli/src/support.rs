//! Shared helpers for the command handlers (token lifecycle, output, parsing).
//!
//! All presentation — the `--verbose`/`--trace-raw` request rendering, the
//! `--json` output and human text — is owned by this CLI layer. The protocol
//! layer only returns request/response traces as data.

use crate::config::Config;
use anyhow::{Result, bail};
use oray_core::trace::{RequestLog, TracedResult};
use reqwest::blocking::Client as HttpClient;
use serde::Serialize;
use std::path::PathBuf;
use std::sync::atomic::{AtomicBool, Ordering};

/// Whether `--verbose` (detailed request rendering on stderr) is on.
static VERBOSE: AtomicBool = AtomicBool::new(false);
/// Whether `--trace-raw` disables masking of sensitive trace values.
static RAW_TRACE: AtomicBool = AtomicBool::new(false);

pub fn set_verbose(enabled: bool) {
    VERBOSE.store(enabled, Ordering::Relaxed);
}

pub fn verbose() -> bool {
    VERBOSE.load(Ordering::Relaxed)
}

pub fn set_raw_trace(enabled: bool) {
    RAW_TRACE.store(enabled, Ordering::Relaxed);
}

pub fn raw_trace() -> bool {
    RAW_TRACE.load(Ordering::Relaxed)
}

/// Truncate a body for display (mirrors the CLI's classic `--verbose` view).
fn display_body(body: &str) -> String {
    let body = body.trim();
    if body.len() > 4096 {
        format!("{}…", &body[..4096])
    } else {
        body.to_string()
    }
}

fn print_request_log(call: &RequestLog) {
    eprintln!("[DEBUG] {} {}", call.method, call.url);
    for (name, value) in &call.request_headers {
        eprintln!("[DEBUG] {name}: {value}");
    }
    if let Some(body) = &call.request_body {
        eprintln!("[DEBUG] Request body: {}", display_body(body));
    }
    match call.status {
        Some(status) => eprintln!("[DEBUG] Response: {status}"),
        None => eprintln!("[DEBUG] Response: <no response>"),
    }
    if !call.response_body.is_empty() {
        eprintln!("[DEBUG] Body: {}", display_body(&call.response_body));
    }
}

/// Render captured request/response exchanges on stderr when `--verbose` is
/// set. Values are masked by default; `--trace-raw` shows them verbatim.
pub fn emit_traces(calls: &[RequestLog]) {
    if !verbose() || calls.is_empty() {
        return;
    }
    for call in calls {
        if raw_trace() {
            print_request_log(call);
        } else {
            print_request_log(&call.redacted());
        }
    }
}

/// Run a `TracedResult` to completion at the presentation layer: print its
/// exchanges when `--verbose` is set, then unwrap the data or the error.
/// Used by command paths that talk to the protocol layer directly (the auth
/// flow), where `with_token` is not involved.
pub fn traced<T>(label: &str, r: TracedResult<T>) -> Result<T> {
    match r {
        Ok(traced) => {
            emit_traces(&traced.calls);
            Ok(traced.data)
        }
        Err(e) => {
            emit_traces(&e.calls);
            Err(anyhow::anyhow!("{label}: {e}"))
        }
    }
}

/// Print a value as pretty JSON when `json` is set.
pub fn emit_json(json: bool, value: &impl Serialize) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

/// Run an authenticated closure, refreshing the token once on `TOKEN_EXPIRED`
/// when `refresh_on_expired` is set. Traces from the refresh and the operation
/// itself are rendered on stderr when `--verbose` is on.
pub fn with_token<T>(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    refresh_on_expired: bool,
    run: impl Fn(&str) -> TracedResult<T>,
) -> Result<T> {
    let mut token = crate::token::ensure_token(http, cfg, path, false)?;
    match run(&token.access_token) {
        Ok(traced) => {
            emit_traces(&traced.calls);
            Ok(traced.data)
        }
        Err(e) if e.is_token_expired() && refresh_on_expired => {
            emit_traces(&e.calls);
            eprintln!("access token expired; refreshing and retrying...");
            token = crate::token::ensure_token(http, cfg, path, true)?;
            match run(&token.access_token) {
                Ok(traced) => {
                    emit_traces(&traced.calls);
                    Ok(traced.data)
                }
                Err(e) => {
                    emit_traces(&e.calls);
                    Err(e.into())
                }
            }
        }
        Err(e) => {
            emit_traces(&e.calls);
            Err(e.into())
        }
    }
}

/// Resolve the trusted client id, generating and persisting a fresh one if
/// none is configured yet.
pub fn resolve_clientid(cfg: &mut Config, cli_clientid: Option<&str>) -> String {
    if let Some(c) = cli_clientid.filter(|c| !c.is_empty()) {
        return c.to_string();
    }
    if let Some(c) = cfg
        .client
        .as_ref()
        .map(|c| c.clientid.clone())
        .filter(|c| !c.is_empty())
    {
        return c;
    }
    let cid = oray_core::auth::generate_client_id();
    cfg.client = Some(crate::config::Client {
        clientid: cid.clone(),
    });
    cid
}

pub fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "oray-tools".to_string())
}

pub fn parse_on_off(state: &str) -> Result<bool> {
    match state.to_ascii_lowercase().as_str() {
        "on" | "1" | "true" => Ok(true),
        "off" | "0" | "false" => Ok(false),
        other => bail!("expected on or off, got '{other}'"),
    }
}

/// Seconds for an "ago" bound like `30s`, `5m`, `2h`, `1d`; `None` when the
/// string is not a plain duration (e.g. an absolute date/time instead).
pub fn parse_ago_secs(s: &str) -> Option<i64> {
    let s = s.trim();
    if s.len() < 2 {
        return None;
    }
    let (num, unit) = s.split_at(s.len() - 1);
    if !num.bytes().all(|b| b.is_ascii_digit()) {
        return None;
    }
    let n: i64 = num.parse().ok()?;
    match unit {
        "s" => Some(n),
        "m" => Some(n * 60),
        "h" => Some(n * 3600),
        "d" => Some(n * 86400),
        _ => None,
    }
}

/// Resolve a time bound (a `--since`/`--until` value) to a unix timestamp.
///
/// Two forms are accepted:
/// - an "ago" duration (`30m`, `2h`, `1d`) → `now - duration`;
/// - an absolute local time: `YYYY-MM-DD`, `YYYY-MM-DD HH:MM[:SS]` or the
///   `T`-separated equivalent. A bare date means the start of the day for a
///   lower bound (`00:00:00`) or the end of the day for an upper bound
///   (`23:59:59`), so `--since 2026-09-01 --until 2026-09-02` spans two days.
pub fn resolve_time_bound(value: &str, lower: bool, now: i64) -> Result<i64> {
    use chrono::{Local, NaiveDate, NaiveDateTime, TimeZone};

    let v = value.trim();
    if let Some(secs) = parse_ago_secs(v) {
        return Ok(now - secs);
    }
    let local_epoch = |dt: NaiveDateTime| -> i64 {
        Local
            .from_local_datetime(&dt)
            .earliest()
            .map(|d| d.timestamp())
            .unwrap_or_else(|| dt.and_utc().timestamp())
    };
    const DATETIME_FORMATS: &[&str] = &[
        "%Y-%m-%d %H:%M:%S",
        "%Y-%m-%d %H:%M",
        "%Y-%m-%dT%H:%M:%S",
        "%Y-%m-%dT%H:%M",
    ];
    if let Some(dt) = DATETIME_FORMATS
        .iter()
        .find_map(|f| NaiveDateTime::parse_from_str(v, f).ok())
    {
        return Ok(local_epoch(dt));
    }
    if let Ok(date) = NaiveDate::parse_from_str(v, "%Y-%m-%d") {
        let dt = if lower {
            date.and_hms_opt(0, 0, 0).expect("valid time")
        } else {
            date.and_hms_opt(23, 59, 59).expect("valid time")
        };
        return Ok(local_epoch(dt));
    }
    bail!(
        "invalid time bound '{value}': use an ago duration like 30m/2h/1d, or an absolute \
         local time like 2026-09-01 or 2026-09-01 08:30[:00]"
    )
}

/// Parse a user-supplied timezone value into minutes east of UTC.
///
/// Accepted forms: plain minutes (`480`, `-300`) or signed hours with an
/// optional minute part (`+8`, `+08:00`, `-5`, `-05:30`).
pub fn parse_tz(input: &str) -> Option<i64> {
    let s = input.trim();
    let first = s.as_bytes().first().copied();
    if first == Some(b'+') || first == Some(b'-') {
        let neg = first == Some(b'-');
        let rest = &s[1..];
        let (h, m) = match rest.split_once(':') {
            Some((h, m)) => (h.parse::<i64>().ok()?, m.parse::<i64>().ok()?),
            None => (rest.parse::<i64>().ok()?, 0),
        };
        if !(0..=23).contains(&h) || !(0..=59).contains(&m) {
            return None;
        }
        let total = h * 60 + m;
        return Some(if neg { -total } else { total });
    }
    // Plain integer: minutes east of UTC.
    s.parse::<i64>().ok()
}

/// The machine's current UTC offset in minutes east of UTC.
fn machine_offset_min() -> i64 {
    let secs = chrono::Local::now().offset().local_minus_utc();
    (secs / 60).into()
}

fn format_offset(min: i64) -> String {
    let sign = if min < 0 { '-' } else { '+' };
    let abs = min.abs();
    format!("UTC{}{:02}:{:02}", sign, abs / 60, abs % 60)
}

/// Resolve the effective timezone offset for plug timers (minutes east of UTC).
///
/// Precedence: `--tz` argument, then the `config.tz` string, then the machine's
/// local offset. When the machine offset is used as a fallback a warning is
/// printed so the user knows which value was assumed (the plug's own timezone
/// is the one that matters).
pub fn resolve_tz(cfg: &Config, arg: Option<i64>) -> Result<i64> {
    if let Some(min) = arg {
        return Ok(min);
    }
    if let Some(s) = &cfg.tz {
        return parse_tz(s).ok_or_else(|| anyhow::anyhow!("invalid `tz` in config: '{s}'"));
    }
    let min = machine_offset_min();
    eprintln!(
        "warning: no --tz or config `tz` set; defaulting to the machine's local offset {min} min ({}) for timer scheduling — use --tz or set `tz` in the config if the plug is in another timezone",
        format_offset(min)
    );
    Ok(min)
}

pub fn print_tokens(cfg: &Config) {
    match &cfg.token {
        Some(t) => {
            println!("access_token:    {}", t.access_token);
            println!("refresh_token:   {}", t.refresh_token);
            println!(
                "access_expiry:   {}",
                crate::token::access_expiry(&t.access_token)
                    .map(crate::token::human_time)
                    .unwrap_or_else(|| "unknown".to_string())
            );
            println!(
                "refresh_expiry:  {}",
                crate::token::human_time(t.refresh_expires)
            );
        }
        None => println!("no tokens saved"),
    }
    if let Some(a) = &cfg.account {
        println!("account:         {}", a.account);
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_tz_minutes() {
        assert_eq!(parse_tz("480"), Some(480));
        assert_eq!(parse_tz("330"), Some(330));
    }

    #[test]
    fn parse_tz_hours() {
        assert_eq!(parse_tz("+8"), Some(480));
        assert_eq!(parse_tz("8"), Some(8)); // bare integer = minutes
        assert_eq!(parse_tz("+08:00"), Some(480));
        assert_eq!(parse_tz("-5"), Some(-300));
        assert_eq!(parse_tz("-05:30"), Some(-330));
    }

    #[test]
    fn parse_tz_invalid() {
        assert!(parse_tz("bogus").is_none());
        assert!(parse_tz("+25").is_none());
        assert!(parse_tz("-300").is_none()); // negative offsets must be ±HH[:MM]
        assert!(parse_tz("+08:99").is_none());
    }

    #[test]
    fn format_offsets() {
        assert_eq!(format_offset(480), "UTC+08:00");
        assert_eq!(format_offset(-330), "UTC-05:30");
        assert_eq!(format_offset(0), "UTC+00:00");
    }

    #[test]
    fn resolve_prefers_arg_over_config() {
        let mut cfg = Config::default();
        cfg.tz = Some("-05:00".to_string());
        assert_eq!(resolve_tz(&cfg, Some(480)).unwrap(), 480);
        assert_eq!(resolve_tz(&cfg, None).unwrap(), -300);
    }

    #[test]
    fn resolve_invalid_config_tz_errors() {
        let mut cfg = Config::default();
        cfg.tz = Some("bogus".to_string());
        assert!(resolve_tz(&cfg, None).is_err());
    }

    #[test]
    fn ago_bounds() {
        assert_eq!(parse_ago_secs("30s"), Some(30));
        assert_eq!(parse_ago_secs("5m"), Some(300));
        assert_eq!(parse_ago_secs("2h"), Some(7200));
        assert_eq!(parse_ago_secs("1d"), Some(86400));
        assert_eq!(parse_ago_secs("bogus"), None);
        assert_eq!(parse_ago_secs("1x"), None);
        assert_eq!(parse_ago_secs("2026-09-01"), None);
        assert_eq!(parse_ago_secs("0"), None);
    }

    #[test]
    fn ago_resolution_is_relative_to_now() {
        let now = 1_800_000_000;
        assert_eq!(resolve_time_bound("2h", true, now).unwrap(), now - 7200);
        assert_eq!(resolve_time_bound("1d", false, now).unwrap(), now - 86400);
    }

    #[test]
    fn absolute_time_equivalence() {
        let now = 1_800_000_000;
        let with_secs = resolve_time_bound("2026-09-01 08:30:00", true, now).unwrap();
        let no_secs = resolve_time_bound("2026-09-01 08:30", true, now).unwrap();
        let t_form = resolve_time_bound("2026-09-01T08:30:00", true, now).unwrap();
        assert_eq!(with_secs, no_secs);
        assert_eq!(with_secs, t_form);
    }

    #[test]
    fn date_only_expands_to_full_day() {
        let now = 1_800_000_000;
        let start = resolve_time_bound("2026-09-01", true, now).unwrap();
        let midnight = resolve_time_bound("2026-09-01 00:00:00", true, now).unwrap();
        assert_eq!(start, midnight);
        let end = resolve_time_bound("2026-09-01", false, now).unwrap();
        let end_of_day = resolve_time_bound("2026-09-01 23:59:59", false, now).unwrap();
        assert_eq!(end, end_of_day);
    }

    #[test]
    fn invalid_time_bound_is_an_error() {
        let now = 1_800_000_000;
        assert!(resolve_time_bound("not-a-time", true, now).is_err());
        assert!(resolve_time_bound("2026-13-01", true, now).is_err());
        assert!(resolve_time_bound("", true, now).is_err());
    }
}
