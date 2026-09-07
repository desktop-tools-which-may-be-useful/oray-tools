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

/// Interpret a wall-clock time in a timezone: an explicit offset in minutes
/// east of UTC, or the machine's local timezone when `tz_min` is `None`.
fn naive_to_epoch(dt: chrono::NaiveDateTime, tz_min: Option<i64>) -> i64 {
    use chrono::{FixedOffset, Local, TimeZone};
    match tz_min {
        None => Local
            .from_local_datetime(&dt)
            .earliest()
            .map(|d| d.timestamp())
            .unwrap_or_else(|| dt.and_utc().timestamp()),
        Some(min) => {
            let secs = i32::try_from(min * 60).unwrap_or(0);
            let offset =
                FixedOffset::east_opt(secs).unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
            offset
                .from_local_datetime(&dt)
                .single()
                .map(|d| d.timestamp())
                .unwrap_or_else(|| dt.and_utc().timestamp())
        }
    }
}

/// Resolve a time bound (a `--since`/`--until` value) to a unix timestamp.
///
/// Two forms are accepted:
/// - an "ago" duration (`30m`, `2h`, `1d`) → `now - duration`;
/// - an absolute wall-clock time: `YYYY-MM-DD`, `YYYY-MM-DD HH:MM[:SS]` or the
///   `T`-separated equivalent. A bare date means the start of the day for a
///   lower bound (`00:00:00`) or the end of the day for an upper bound
///   (`23:59:59`), so `--since 2026-09-01 --until 2026-09-02` spans two days.
///
/// Absolute times are converted to a unix timestamp using the timezone offset
/// `tz_min` (minutes east of UTC) when given, or the machine's local timezone
/// otherwise. The timestamp is compared against the events' `createtime`
/// (epoch seconds), which the plug stores in UTC.
pub fn resolve_time_bound_tz(
    value: &str,
    lower: bool,
    now: i64,
    tz_min: Option<i64>,
) -> Result<i64> {
    use chrono::{NaiveDate, NaiveDateTime};

    let v = value.trim();
    if let Some(secs) = parse_ago_secs(v) {
        return Ok(now - secs);
    }
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
        return Ok(naive_to_epoch(dt, tz_min));
    }
    if let Ok(date) = NaiveDate::parse_from_str(v, "%Y-%m-%d") {
        let dt = if lower {
            date.and_hms_opt(0, 0, 0).expect("valid time")
        } else {
            date.and_hms_opt(23, 59, 59).expect("valid time")
        };
        return Ok(naive_to_epoch(dt, tz_min));
    }
    bail!(
        "invalid time bound '{value}': use an ago duration like 30m/2h/1d, or an absolute \
         time like 2026-09-01 or 2026-09-01 08:30[:00]"
    )
}

/// Parse a local clock time into minutes of the day (0-1439).
///
/// Accepted forms: a clock time like `19:25` / `8:05`, or plain minutes of
/// the day like `480` (= 08:00). `None` on anything else.
pub fn parse_local_time(input: &str) -> Option<u64> {
    let s = input.trim();
    if let Some((h, m)) = s.split_once(':') {
        let h: i64 = h.trim().parse().ok()?;
        let m: i64 = m.trim().parse().ok()?;
        if !(0..=23).contains(&h) || !(0..=59).contains(&m) {
            return None;
        }
        return Some((h * 60 + m) as u64);
    }
    let n: i64 = s.parse().ok()?;
    if !(0..=1439).contains(&n) {
        return None;
    }
    Some(n as u64)
}

/// Parse a user-supplied timezone value into minutes east of UTC.
///
/// A sign is mandatory and only these unambiguous forms are accepted:
/// - signed hours with a unit: `+8h`, `-5h`;
/// - signed minutes with a unit: `+480min`, `-300min`;
/// - a signed `HH:MM` offset: `+08:00`, `-08:20`, `+8:00`.
///
/// Bare numbers like `8` or `480` are rejected: they are ambiguous.
pub fn parse_tz(input: &str) -> Option<i64> {
    let s = input.trim();
    let first = s.as_bytes().first().copied();
    let neg = match first {
        Some(b'+') => false,
        Some(b'-') => true,
        _ => return None,
    };
    let rest = &s[1..];
    if rest.is_empty() {
        return None;
    }
    let minutes = if let Some((h, m)) = rest.split_once(':') {
        let h: i64 = h.parse().ok()?;
        let m: i64 = m.parse().ok()?;
        if !(0..=23).contains(&h) || !(0..=59).contains(&m) {
            return None;
        }
        h * 60 + m
    } else if let Some(num) = rest.strip_suffix("min") {
        let n: i64 = num.parse().ok()?;
        if !(0..=1439).contains(&n) {
            return None;
        }
        n
    } else {
        let num = rest.strip_suffix('h').or_else(|| rest.strip_suffix('H'))?;
        let n: i64 = num.parse().ok()?;
        if !(0..=23).contains(&n) {
            return None;
        }
        n * 60
    };
    Some(if neg { -minutes } else { minutes })
}

/// The machine's current UTC offset in minutes east of UTC.
fn machine_offset_min() -> i64 {
    let secs = chrono::Local::now().offset().local_minus_utc();
    (secs / 60).into()
}

/// A human-readable timezone name for an offset in minutes east of UTC, e.g.
/// `UTC+08:00`, `UTC-05:30`; zero is written `UTC+00:00`.
pub fn tz_label(min: i64) -> String {
    let sign = if min < 0 { '-' } else { '+' };
    let abs = min.abs();
    format!("UTC{}{:02}:{:02}", sign, abs / 60, abs % 60)
}

/// Render an epoch timestamp as `YYYY-MM-DD HH:MM:SS <tz name>`. `tz` is the
/// offset in minutes east of UTC; `None` renders in the machine's local zone,
/// annotating the instant with its actual offset.
pub fn render_ts(epoch: i64, tz: Option<i64>) -> String {
    use chrono::{DateTime, FixedOffset, Local, Utc};
    let Some(utc) = DateTime::<Utc>::from_timestamp(epoch, 0) else {
        return format!("{epoch} (epoch)");
    };
    match tz {
        Some(min) => {
            let secs = i32::try_from(min * 60).unwrap_or(0);
            let offset =
                FixedOffset::east_opt(secs).unwrap_or_else(|| FixedOffset::east_opt(0).unwrap());
            format!(
                "{} {}",
                utc.with_timezone(&offset).format("%Y-%m-%d %H:%M:%S"),
                tz_label(min)
            )
        }
        None => {
            let local = utc.with_timezone(&Local);
            let min = i64::from(local.offset().local_minus_utc() / 60);
            format!("{} {}", local.format("%Y-%m-%d %H:%M:%S"), tz_label(min))
        }
    }
}

/// Resolve the effective timezone offset for plug-local wall-clock times
/// (minutes east of UTC), used for timer scheduling and for absolute
/// `--since`/`--until` log windows.
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
        return parse_tz(s).ok_or_else(|| {
            anyhow::anyhow!(
                "invalid `tz` in config: '{s}' (use a signed offset like +8h / +480min / +08:00)"
            )
        });
    }
    let min = machine_offset_min();
    eprintln!(
        "warning: no --tz or config `tz` set; assuming the machine's local offset {min} min ({}) for plug-local times (timers and absolute log windows) — use --tz or set `tz` in the config if the plug is in another timezone",
        tz_label(min)
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
    fn parse_local_time_clock() {
        assert_eq!(parse_local_time("19:25"), Some(19 * 60 + 25));
        assert_eq!(parse_local_time("8:05"), Some(8 * 60 + 5));
        assert_eq!(parse_local_time("08:00"), Some(480));
        assert_eq!(parse_local_time("0:00"), Some(0));
        assert_eq!(parse_local_time("23:59"), Some(1439));
    }

    #[test]
    fn parse_local_time_minutes() {
        assert_eq!(parse_local_time("480"), Some(480));
        assert_eq!(parse_local_time("0"), Some(0));
        assert_eq!(parse_local_time("1439"), Some(1439));
    }

    #[test]
    fn parse_local_time_invalid() {
        assert!(parse_local_time("25:00").is_none());
        assert!(parse_local_time("12:60").is_none());
        assert!(parse_local_time("1440").is_none());
        assert!(parse_local_time("19:25:00").is_none());
        assert!(parse_local_time("abc").is_none());
        assert!(parse_local_time("").is_none());
        assert!(parse_local_time("-1").is_none());
    }

    #[test]
    fn parse_tz_units() {
        assert_eq!(parse_tz("+8h"), Some(480));
        assert_eq!(parse_tz("-5h"), Some(-300));
        assert_eq!(parse_tz("+8H"), Some(480));
        assert_eq!(parse_tz("+480min"), Some(480));
        assert_eq!(parse_tz("-300min"), Some(-300));
        assert_eq!(parse_tz("+8min"), Some(8));
    }

    #[test]
    fn parse_tz_colon_form() {
        assert_eq!(parse_tz("+08:00"), Some(480));
        assert_eq!(parse_tz("-05:30"), Some(-330));
        assert_eq!(parse_tz("-08:20"), Some(-500));
        assert_eq!(parse_tz("+8:00"), Some(480));
    }

    #[test]
    fn parse_tz_invalid() {
        // Bare numbers are rejected: ambiguous between minutes and hours.
        assert!(parse_tz("8").is_none());
        assert!(parse_tz("480").is_none());
        assert!(parse_tz("330").is_none());
        assert!(parse_tz("+8").is_none()); // unit required unless HH:MM
        assert!(parse_tz("-5").is_none());
        assert!(parse_tz("bogus").is_none());
        assert!(parse_tz("+25h").is_none());
        assert!(parse_tz("+08:99").is_none());
        assert!(parse_tz("-300").is_none());
        assert!(parse_tz("+1500min").is_none());
        assert!(parse_tz("+1440min").is_none());
        assert!(parse_tz("+24h").is_none());
        assert!(parse_tz("-").is_none());
    }

    #[test]
    fn format_offsets() {
        assert_eq!(tz_label(480), "UTC+08:00");
        assert_eq!(tz_label(-330), "UTC-05:30");
        assert_eq!(tz_label(0), "UTC+00:00");
    }

    #[test]
    fn render_ts_uses_requested_offset() {
        // 1788314270 == 2026-09-02 01:57:50 UTC (09:57:50 in UTC+8).
        assert_eq!(
            render_ts(1788314270, Some(0)),
            "2026-09-02 01:57:50 UTC+00:00"
        );
        assert_eq!(
            render_ts(1788314270, Some(480)),
            "2026-09-02 09:57:50 UTC+08:00"
        );
        assert_eq!(
            render_ts(1788314270, Some(-300)),
            "2026-09-01 20:57:50 UTC-05:00"
        );
    }

    #[test]
    fn render_ts_negative_epoch_falls_back_to_raw() {
        assert!(render_ts(i64::MIN, Some(0)).ends_with("(epoch)"));
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
        assert_eq!(
            resolve_time_bound_tz("2h", true, now, None).unwrap(),
            now - 7200
        );
        assert_eq!(
            resolve_time_bound_tz("1d", false, now, None).unwrap(),
            now - 86400
        );
    }

    #[test]
    fn absolute_time_equivalence() {
        let now = 1_800_000_000;
        let with_secs = resolve_time_bound_tz("2026-09-01 08:30:00", true, now, None).unwrap();
        let no_secs = resolve_time_bound_tz("2026-09-01 08:30", true, now, None).unwrap();
        let t_form = resolve_time_bound_tz("2026-09-01T08:30:00", true, now, None).unwrap();
        assert_eq!(with_secs, no_secs);
        assert_eq!(with_secs, t_form);
    }

    #[test]
    fn date_only_expands_to_full_day() {
        let now = 1_800_000_000;
        let start = resolve_time_bound_tz("2026-09-01", true, now, None).unwrap();
        let midnight = resolve_time_bound_tz("2026-09-01 00:00:00", true, now, None).unwrap();
        assert_eq!(start, midnight);
        let end = resolve_time_bound_tz("2026-09-01", false, now, None).unwrap();
        let end_of_day = resolve_time_bound_tz("2026-09-01 23:59:59", false, now, None).unwrap();
        assert_eq!(end, end_of_day);
    }

    #[test]
    fn invalid_time_bound_is_an_error() {
        let now = 1_800_000_000;
        assert!(resolve_time_bound_tz("not-a-time", true, now, None).is_err());
        assert!(resolve_time_bound_tz("2026-13-01", true, now, None).is_err());
        assert!(resolve_time_bound_tz("", true, now, None).is_err());
    }

    #[test]
    fn tz_offset_moves_absolute_bound() {
        let now = 1_800_000_000;
        let east8 = resolve_time_bound_tz("2026-09-01 08:00:00", true, now, Some(480)).unwrap();
        let utc = resolve_time_bound_tz("2026-09-01 08:00:00", true, now, Some(0)).unwrap();
        // 08:00 UTC+8 is 00:00 UTC.
        assert_eq!(east8 - utc, -480 * 60);
        // A bare date in UTC+8 starts 16:00 UTC the previous day.
        let day = resolve_time_bound_tz("2026-09-01", true, now, Some(480)).unwrap();
        let day_utc = resolve_time_bound_tz("2026-09-01 00:00:00", true, now, Some(0)).unwrap();
        assert_eq!(day - day_utc, -480 * 60);
    }
}
