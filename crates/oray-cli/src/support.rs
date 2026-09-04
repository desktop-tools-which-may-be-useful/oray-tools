//! Shared helpers for the command handlers (token lifecycle, output, parsing).

use crate::config::Config;
use anyhow::{Result, bail};
use reqwest::blocking::Client as HttpClient;
use serde::Serialize;
use std::path::PathBuf;

/// Print a value as pretty JSON when `json` is set.
pub fn emit_json(json: bool, value: &impl Serialize) -> Result<()> {
    if json {
        println!("{}", serde_json::to_string_pretty(value)?);
    }
    Ok(())
}

/// Run an authenticated closure, refreshing the token once on `TOKEN_EXPIRED`
/// when `refresh_on_expired` is set.
pub fn with_token<T>(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    refresh_on_expired: bool,
    run: impl Fn(&str) -> oray_core::Result<T>,
) -> Result<T> {
    let mut token = crate::token::ensure_token(http, cfg, path, false)?;
    match run(&token.access_token) {
        Ok(v) => Ok(v),
        Err(oray_core::Error::TokenExpired(_)) if refresh_on_expired => {
            eprintln!("access token expired; refreshing and retrying...");
            token = crate::token::ensure_token(http, cfg, path, true)?;
            run(&token.access_token).map_err(Into::into)
        }
        Err(e) => Err(e.into()),
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

/// Parse a short duration like `30s`, `5m`, `2h`, `1d` into seconds.
pub fn parse_duration(s: &str) -> i64 {
    let s = s.trim();
    let (num, unit) = s.split_at(s.len().saturating_sub(1));
    let n: i64 = num.parse().unwrap_or(0);
    match unit {
        "s" => n,
        "m" => n * 60,
        "h" => n * 3600,
        "d" => n * 86400,
        _ => n,
    }
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
        return parse_tz(s)
            .ok_or_else(|| anyhow::anyhow!("invalid `tz` in config: '{s}'"));
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
}
