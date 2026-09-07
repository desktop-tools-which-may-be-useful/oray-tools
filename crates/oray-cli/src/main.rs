mod auth;
mod config;
mod remote;
mod support;
mod token;
mod wakeup;

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use config::Config;
use reqwest::blocking::Client as HttpClient;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "oray-tools",
    version,
    about = "oray-tools: control Oray (Sunlogin) devices from the cloud API",
    after_help = "Run `oray-tools <COMMAND> --help` for command-specific options (e.g. `oray-tools wakeup --help`)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Path to config file (overrides the platform default location)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Trusted Ex-ClientId (default: machine-generated UUID, persisted to config)
    #[arg(long, global = true)]
    clientid: Option<String>,

    /// Print machine-readable JSON instead of human text
    #[arg(long, global = true)]
    json: bool,

    /// Show raw HTTP requests/responses on stderr
    #[arg(long, global = true)]
    verbose: bool,

    /// With --verbose, show raw (unredacted) request traces; by default
    /// sensitive values (tokens, passwords, ...) are masked
    #[arg(long, global = true)]
    trace_raw: bool,

    /// Timezone offset for plug timers / log windows, e.g. +8h, -5h, +480min,
    /// +08:00, -08:20 (a sign is required). Defaults to config `tz`, else the
    /// machine's local offset (with a warning)
    #[arg(long, global = true, allow_hyphen_values = true)]
    tz: Option<String>,
}

#[derive(Subcommand)]
enum Command {
    /// Manage authentication (persisted locally: account + tokens)
    Auth {
        #[command(subcommand)]
        sub: auth::AuthCmd,
    },
    /// Wakeup devices (smart plugs / power hardware), data fetched live
    Wakeup {
        /// On server-side TOKEN_EXPIRED, refresh the token and retry once
        #[arg(long, global = true)]
        refresh_on_expired: bool,
        #[command(subcommand)]
        sub: wakeup::WakeupCmd,
    },
    /// Remote devices (PCs / phones), data fetched live
    Remote {
        /// On server-side TOKEN_EXPIRED, refresh the token and retry once
        #[arg(long, global = true)]
        refresh_on_expired: bool,
        #[command(subcommand)]
        sub: remote::RemoteCmd,
    },
}

fn main() {
    let mut cmd = Cli::command();
    if let Ok(default) = config::Config::default_path() {
        cmd = cmd.mut_arg("config", |a| {
            a.help(format!(
                "Path to config file (default: {})",
                default.display()
            ))
        });
    }
    let cli = Cli::from_arg_matches(&cmd.get_matches()).unwrap_or_else(|e| e.exit());
    support::set_verbose(cli.verbose);
    support::set_raw_trace(cli.trace_raw);
    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let http = HttpClient::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let (mut cfg, path) = Config::load(cli.config.as_ref())?;
    let json = cli.json;
    let tz = match cli.tz.as_deref() {
        Some(s) => Some(support::parse_tz(s).ok_or_else(|| {
            anyhow::anyhow!(
                "invalid --tz '{s}' (use a signed offset like +8h / -5h / +480min / +08:00 / -08:20)"
            )
        })?),
        None => None,
    };
    match cli.command {
        Command::Auth { sub } => {
            auth::run(&http, &mut cfg, &path, cli.clientid.as_deref(), sub, json)
        }
        Command::Wakeup {
            refresh_on_expired,
            sub,
        } => wakeup::run(&http, &mut cfg, &path, sub, refresh_on_expired, json, tz),
        Command::Remote {
            refresh_on_expired,
            sub,
        } => remote::run(&http, &mut cfg, &path, sub, refresh_on_expired, json),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_on_off_variants() {
        assert!(support::parse_on_off("on").unwrap());
        assert!(support::parse_on_off("1").unwrap());
        assert!(!support::parse_on_off("off").unwrap());
        assert!(!support::parse_on_off("0").unwrap());
        assert!(support::parse_on_off("OFF").is_ok_and(|v| !v));
        assert!(support::parse_on_off("maybe").is_err());
    }

    #[test]
    fn default_config_has_no_device_storage() {
        let cfg = Config::default();
        assert!(cfg.server.is_none());
        assert!(cfg.token.is_none());
    }
}
