mod auth;
mod captcha;
mod config;
mod prompt;
mod remote;
mod support;
mod token;
mod wakeup;

use anyhow::Result;
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use config::Config;
use reqwest::blocking::Client as HttpClient;
use std::ffi::OsString;
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

    /// Prompt for the arguments the command line leaves out (values that were
    /// given are never re-asked; without this flag a missing argument keeps
    /// clap's native error)
    #[arg(long, global = true)]
    interactive: bool,
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

/// The command as it is declared, with the concrete default config path
/// spelled out in `--config`'s help.
fn base_command() -> clap::Command {
    let mut cmd = Cli::command();
    if let Ok(default) = config::Config::default_path() {
        cmd = cmd.mut_arg("config", |a| {
            a.help(format!(
                "Path to config file (default: {})",
                default.display()
            ))
        });
    }
    cmd
}

/// The strict twin of [`base_command`]: every argument that `--interactive`
/// may fill in is required again, so the default parse reports each missing
/// value with clap's own wording — byte for byte the message the command
/// produced before `--interactive` existed.
///
/// [`clap::Command::mut_subcommands`] maps the whole tree in place and keeps
/// the subcommand order the help output depends on
/// ( [`clap::Command::mut_subcommand`] would push the touched command to the
/// end of the list).
fn strictify(cmd: clap::Command) -> clap::Command {
    cmd.mut_args(|arg| {
        if arg.is_positional() || matches!(arg.get_id().as_str(), "time" | "count") {
            arg.required(true)
        } else {
            arg
        }
    })
    .mut_subcommands(strictify)
}

/// Resolve `argv` in two passes (方案 B: lenient first, strict to decide).
///
/// The lenient build parses first: it cannot fail on a missing value, so a
/// successful parse tells us both whether `--interactive` was requested and
/// which values are still missing. Without the flag the same argv is parsed
/// once more with the strict build, which is what produces clap's native
/// missing-argument error — and, for `--help` / `--version` / a bare group,
/// clap's native help rendering (a lenient build would print `[ACCOUNT]`
/// instead of `<ACCOUNT>`).
fn resolve(base: clap::Command, argv: &[OsString]) -> Result<Cli, clap::Error> {
    let strict = strictify(base.clone());
    if let Ok(lenient) = base.try_get_matches_from(argv) {
        let cli = Cli::from_arg_matches(&lenient)?;
        if cli.interactive {
            return Ok(cli);
        }
    }
    Cli::from_arg_matches(&strict.try_get_matches_from(argv)?)
}

fn main() {
    let argv: Vec<OsString> = std::env::args_os().collect();
    let cli = resolve(base_command(), &argv).unwrap_or_else(|e| e.exit());
    support::set_verbose(cli.verbose);
    support::set_raw_trace(cli.trace_raw);
    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(mut cli: Cli) -> Result<()> {
    let http = HttpClient::builder()
        .timeout(std::time::Duration::from_secs(20))
        .build()?;
    let (mut cfg, path) = Config::load(cli.config.as_ref())?;
    // --interactive: complete the missing arguments before anything is
    // fetched or sent (and before anything can block on stdin unnoticed).
    // Without the flag the parse above already rejected every missing value,
    // so nothing here has a `None` left to ask about.
    match &mut cli.command {
        Command::Auth { sub } => auth::fill(sub, &cfg)?,
        Command::Wakeup { sub, .. } => wakeup::fill(sub)?,
        Command::Remote { sub, .. } => remote::fill(sub)?,
    }
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
    use clap::error::ErrorKind;

    fn argv(args: &[&str]) -> Vec<OsString> {
        std::iter::once("oray-tools")
            .chain(args.iter().copied())
            .map(OsString::from)
            .collect()
    }

    /// Resolve exactly like `main` does; `Err` is the message clap prints.
    fn parse(args: &[&str]) -> Result<Cli, clap::Error> {
        resolve(base_command(), &argv(args))
    }

    /// `parse`, expecting clap to reject the command line.
    fn parse_err(args: &[&str]) -> clap::Error {
        match parse(args) {
            Err(e) => e,
            Ok(_) => panic!("`oray-tools {}` was accepted", args.join(" ")),
        }
    }

    /// The error as a user sees it, without any ANSI escapes.
    fn error_text(err: &clap::Error) -> String {
        let rendered = err.render().to_string();
        let mut out = String::new();
        let mut chars = rendered.chars();
        while let Some(c) = chars.next() {
            if c == '\u{1b}' {
                for c in chars.by_ref() {
                    if c == 'm' {
                        break;
                    }
                }
            } else {
                out.push(c);
            }
        }
        out
    }

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

    #[test]
    fn missing_arguments_without_the_flag_keep_the_native_error() {
        let err = parse_err(&["auth", "login"]);
        assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument);
        assert_eq!(err.exit_code(), 2);
        let text = error_text(&err);
        assert!(
            text.contains(
                "error: the following required arguments were not provided:\n  <ACCOUNT>\n  <PASSWORD>"
            ),
            "{text}"
        );
        assert!(
            text.contains("Usage: oray-tools auth login <ACCOUNT> <PASSWORD>"),
            "{text}"
        );

        let err = parse_err(&["auth", "login", "alice"]);
        let text = error_text(&err);
        // Only the argument list may mention the missing value: <ACCOUNT>
        // was given, so it belongs to the usage line alone.
        let listed = text.split("Usage:").next().unwrap_or(&text);
        assert!(
            listed.contains("<PASSWORD>") && !listed.contains("<ACCOUNT>"),
            "{text}"
        );
    }

    #[test]
    fn every_group_keeps_its_native_missing_argument_error() {
        for args in [
            vec!["wakeup", "info"],
            vec!["wakeup", "rename", "SN123"],
            vec!["wakeup", "memo", "SN123"],
            vec!["wakeup", "plug", "status"],
            vec!["wakeup", "plug", "led", "SN1"],
            vec!["wakeup", "plug", "timer", "add", "SN1"], // --time is missing
            vec!["wakeup", "plug", "timer", "remove"],
            vec!["wakeup", "plug", "countdown", "start"],
            vec!["remote", "rename", "42"],
        ] {
            let err = parse(&args)
                .err()
                .unwrap_or_else(|| panic!("{args:?} parsed without its required arguments"));
            assert_eq!(err.kind(), ErrorKind::MissingRequiredArgument, "{args:?}");
            assert_eq!(err.exit_code(), 2, "{args:?}");
        }
    }

    #[test]
    fn required_long_options_stay_required() {
        let text = error_text(&parse_err(&["wakeup", "plug", "timer", "add", "SN1"]));
        assert!(text.contains("  --time <TIME>"), "{text}");

        let text = error_text(&parse_err(&["wakeup", "plug", "countdown", "start"]));
        assert!(
            text.contains("  --count <COUNT>") && text.contains("  <SN>"),
            "{text}"
        );
    }

    #[test]
    fn help_shows_required_positionals_and_the_new_flag() {
        let err = parse_err(&["auth", "login", "--help"]);
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        let text = error_text(&err);
        assert!(
            text.contains("Usage: oray-tools auth login [OPTIONS] <ACCOUNT> <PASSWORD>"),
            "{text}"
        );
        assert!(!text.contains("[ACCOUNT]"), "{text}");
        assert!(text.contains("--interactive"), "{text}");

        // `help <SUBCOMMAND>` renders through the same strict build.
        let err = parse_err(&["help", "auth", "login"]);
        assert_eq!(err.kind(), ErrorKind::DisplayHelp);
        let text = error_text(&err);
        assert!(text.contains("<ACCOUNT> <PASSWORD>"), "{text}");
    }

    #[test]
    fn invalid_values_keep_the_native_error() {
        let err = parse_err(&["remote", "info", "notanumber"]);
        assert_eq!(err.kind(), ErrorKind::ValueValidation);
        let text = error_text(&err);
        assert!(
            text.contains("error: invalid value 'notanumber' for '<ID>'"),
            "{text}"
        );
    }

    #[test]
    fn interactive_flag_is_read_from_any_position() {
        for args in [
            vec!["auth", "login", "alice", "--interactive"],
            vec!["auth", "--interactive", "login", "alice"],
            vec!["--interactive", "auth", "login", "alice"],
        ] {
            let cli = parse(&args).unwrap_or_else(|e| panic!("{args:?}: {e}"));
            assert!(cli.interactive, "{args:?}");
            let (account, password) = match cli.command {
                Command::Auth {
                    sub: auth::AuthCmd::Login { account, password },
                } => (account, password),
                _ => panic!("{args:?}: not a login command"),
            };
            assert_eq!(account.as_deref(), Some("alice"), "{args:?}");
            assert!(password.is_none(), "{args:?}");
        }
    }
}
