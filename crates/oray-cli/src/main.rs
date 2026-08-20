mod config;
mod token;

use anyhow::{Context, Result, bail};
use clap::{CommandFactory, FromArgMatches, Parser, Subcommand};
use config::Config;
use oray_core::auth::AuthApi;
use oray_core::plug::PlugApi;
use reqwest::blocking::Client as HttpClient;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "oray-tools",
    version,
    about = "oray-tools: control Oray smart plugs",
    after_help = "Run `oray-tools <COMMAND> --help` for command-specific options (e.g. `oray-tools login --help`)."
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
}

#[derive(Subcommand)]
enum Command {
    /// Authenticate and persist tokens
    Login {
        account: String,
        password: String,
    },
    /// Renew tokens with refresh_token and persist them
    Refresh,
    /// Show current token info and expiry
    Tokens,
    /// Clear saved tokens and account
    Logout,
    /// Manage and control plugs
    Plug {
        #[command(subcommand)]
        sub: PlugCmd,
    },
}

#[derive(Subcommand)]
enum PlugCmd {
    /// List configured plugs
    List,
    /// Register a plug (name maps to a device SN)
    Add {
        name: String,
        sn: String,
    },
    /// Remove a configured plug
    Remove { name: String },
    /// Query plug status
    Status {
        /// Plug name (defaults to `default` or the only configured plug)
        name: Option<String>,
        /// Port index (default: 0)
        #[arg(long)]
        index: Option<usize>,
        /// On server-side TOKEN_EXPIRED, refresh the token and retry once
        #[arg(long)]
        refresh_on_expired: bool,
    },
    /// Turn the plug on
    On {
        name: Option<String>,
        #[arg(long)]
        index: Option<usize>,
        /// On server-side TOKEN_EXPIRED, refresh the token and retry once
        #[arg(long)]
        refresh_on_expired: bool,
    },
    /// Turn the plug off
    Off {
        name: Option<String>,
        #[arg(long)]
        index: Option<usize>,
        /// On server-side TOKEN_EXPIRED, refresh the token and retry once
        #[arg(long)]
        refresh_on_expired: bool,
    },
}

#[derive(Clone, Copy)]
enum PlugAction {
    Status,
    On,
    Off,
}

fn main() {
    let mut cmd = Cli::command();
    if let Ok(default) = config::Config::default_path() {
        cmd = cmd.mut_arg(
            "config",
            |a| a.help(format!("Path to config file (default: {})", default.display())),
        );
    }
    let cli = Cli::from_arg_matches(&cmd.get_matches()).unwrap_or_else(|e| e.exit());
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
    match cli.command {
        Command::Login { account, password } => do_login(&http, &mut cfg, &path, cli.clientid.as_deref(), &account, &password),
        Command::Refresh => do_refresh(&http, &mut cfg, &path, cli.clientid.as_deref()),
        Command::Tokens => do_tokens(&cfg),
        Command::Logout => do_logout(&mut cfg, &path),
        Command::Plug { sub } => do_plug(&http, &mut cfg, &path, sub),
    }
}

fn resolve_clientid(cfg: &mut Config, cli_clientid: Option<&str>) -> String {
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
    cfg.client = Some(config::Client { clientid: cid.clone() });
    cid
}

fn do_login(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    clientid: Option<&str>,
    account: &str,
    password: &str,
) -> Result<()> {
    let cid = resolve_clientid(cfg, clientid);
    let server = cfg.server();
    let api = AuthApi::new(http.clone(), &server.api_base);
    let terminal_name = hostname();
    let password_md5 = oray_core::auth::md5_hex(password);
    let resp = match api.login(&cid, account, &password_md5)? {
        oray_core::auth::LoginOutcome::Tokens(resp) => resp,
        oray_core::auth::LoginOutcome::NewDevice(alert) => {
            let target = if !alert.mobile.is_empty() { &alert.mobile } else { &alert.email };
            eprintln!(
                "New device detected ({}), code={}: {target} requires SMS verification. A code has been sent.",
                alert.error, alert.code
            );
            api.sendcode(&cid, account).context("failed to send verification code")?;
            eprint!("Enter the SMS code: ");
            use std::io::Write;
            std::io::stdout().flush().ok();
            let mut code = String::new();
            std::io::stdin()
                .read_line(&mut code)
                .context("failed to read code input")?;
            let code = code.trim().to_string();
            if code.is_empty() {
                bail!("no code entered");
            }
            api.checkcode(&cid, account, &code, &terminal_name)
                .context("failed to verify code")?;
            eprintln!("Device trusted, logging in again...");
            match api.login(&cid, account, &password_md5)? {
                oray_core::auth::LoginOutcome::Tokens(resp) => resp,
                other => bail!("re-login did not return tokens: {other:?}"),
            }
        }
    };

    cfg.account = Some(config::Account {
        account: account.to_string(),
        password_md5,
    });
    cfg.client = Some(config::Client { clientid: cid });
    let expiry = token::refresh_expiry(&resp);
    cfg.token = Some(config::Token {
        access_token: resp.access_token,
        refresh_token: resp.refresh_token,
        refresh_expires: expiry,
    });
    cfg.save(path)?;
    print_tokens(cfg);
    Ok(())
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "oray-tools".to_string())
}

fn do_refresh(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    clientid: Option<&str>,
) -> Result<()> {
    let (access, refresh) = {
        let token = cfg
            .token
            .as_ref()
            .context("no token saved; run `oray-tools login` first")?;
        if token.refresh_token.is_empty() {
            bail!("no refresh token saved; run `oray-tools login` first");
        }
        (token.access_token.clone(), token.refresh_token.clone())
    };
    let cid = resolve_clientid(cfg, clientid);
    let server = cfg.server();
    let api = AuthApi::new(http.clone(), &server.api_base);
    let resp = api.refresh(&cid, &access, &refresh)?;
    let expiry = token::refresh_expiry(&resp);
    cfg.token = Some(config::Token {
        access_token: resp.access_token,
        refresh_token: resp.refresh_token,
        refresh_expires: expiry,
    });
    cfg.save(path)?;
    print_tokens(cfg);
    Ok(())
}

fn do_tokens(cfg: &Config) -> Result<()> {
    print_tokens(cfg);
    Ok(())
}

fn do_logout(cfg: &mut Config, path: &PathBuf) -> Result<()> {
    cfg.account = None;
    cfg.token = None;
    cfg.save(path)?;
    println!("logged out");
    Ok(())
}

fn do_plug(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    sub: PlugCmd,
) -> Result<()> {
    match sub {
        PlugCmd::List => {
            if cfg.plugs.is_empty() {
                println!("no plugs configured; use `oray-tools plug add <name> <sn>`");
                return Ok(());
            }
            let mut names: Vec<_> = cfg.plugs.keys().collect();
            names.sort();
            for name in names {
                let d = &cfg.plugs[name];
                let mark = if name == "default" { " (default)" } else { "" };
                println!("{name}{mark}: sn={}", d.sn);
            }
        }
        PlugCmd::Add { name, sn } => {
            if sn.trim().is_empty() {
                bail!("SN must not be empty");
            }
            cfg.plugs.insert(name.clone(), config::Device { sn: sn.clone() });
            cfg.save(path)?;
            println!("plug '{name}' registered: sn={sn}");
        }
        PlugCmd::Remove { name } => {
            if cfg.plugs.remove(&name).is_none() {
                bail!("no plug named '{name}'");
            }
            cfg.save(path)?;
            println!("plug '{name}' removed");
        }
        PlugCmd::Status { name, index, refresh_on_expired } => {
            do_plug_action(http, cfg, path, name.as_deref(), index, PlugAction::Status, refresh_on_expired)?
        }
        PlugCmd::On { name, index, refresh_on_expired } => {
            do_plug_action(http, cfg, path, name.as_deref(), index, PlugAction::On, refresh_on_expired)?
        }
        PlugCmd::Off { name, index, refresh_on_expired } => {
            do_plug_action(http, cfg, path, name.as_deref(), index, PlugAction::Off, refresh_on_expired)?
        }
    }
    Ok(())
}

/// Resolve a plug by name; falls back to `default` or the only plug.
fn resolve_plug(cfg: &Config, name: Option<&str>) -> Result<(String, config::Device)> {
    if let Some(n) = name {
        let d = cfg
            .plugs
            .get(n)
            .with_context(|| format!("no plug named '{n}' (see `oray-tools plug list`)"))?;
        return Ok((n.to_string(), d.clone()));
    }
    if let Some(d) = cfg.plugs.get("default") {
        return Ok(("default".to_string(), d.clone()));
    }
    if cfg.plugs.len() == 1 {
        let (k, v) = cfg.plugs.iter().next().unwrap();
        return Ok((k.clone(), v.clone()));
    }
    bail!("no plug selected; pass a plug name (see `oray-tools plug list`)")
}

fn do_plug_action(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    name: Option<&str>,
    index: Option<usize>,
    action: PlugAction,
    refresh_on_expired: bool,
) -> Result<()> {
    let (name, dev) = resolve_plug(cfg, name)?;
    let index = index.unwrap_or(0);
    let server = cfg.server();
    let mut token = token::ensure_token(http, cfg, path, false)?;
    let api = PlugApi::new(http.clone(), &server.slapi_base);

    let run = |token: &str| -> oray_core::Result<()> {
        match action {
            PlugAction::Status => {
                let r = api.get_status(token, &dev.sn, index)?;
                let mut found = false;
                if let Some(ports) = &r.response {
                    for p in ports {
                        if p.index as usize == index {
                            let state = if p.status == 1 { "ON" } else { "OFF" };
                            println!("plug={name} sn={} index={} status={state}", dev.sn, p.index);
                            found = true;
                        }
                    }
                }
                if !found {
                    println!("plug={name} sn={} index={index} status=<<unknown>>", dev.sn);
                }
            }
            PlugAction::On => {
                api.set_status(token, &dev.sn, index, true)?;
                println!("plug={name} sn={} port={index} ON", dev.sn);
            }
            PlugAction::Off => {
                api.set_status(token, &dev.sn, index, false)?;
                println!("plug={name} sn={} port={index} OFF", dev.sn);
            }
        }
        Ok(())
    };

    match run(&token.access_token) {
        Ok(()) => Ok(()),
        Err(oray_core::Error::TokenExpired(_)) if refresh_on_expired => {
            eprintln!("access token expired; refreshing and retrying...");
            token = token::ensure_token(http, cfg, path, true)?;
            run(&token.access_token)?;
            Ok(())
        }
        Err(e) => Err(e.into()),
    }
}

fn print_tokens(cfg: &Config) {
    match &cfg.token {
        Some(t) => {
            println!("access_token:    {}", t.access_token);
            println!("refresh_token:   {}", t.refresh_token);
            println!(
                "access_expiry:   {}",
                token::access_expiry(&t.access_token)
                    .map(token::human_time)
                    .unwrap_or_else(|| "unknown".to_string())
            );
            println!(
                "refresh_expiry:  {}",
                token::human_time(t.refresh_expires)
            );
        }
        None => println!("no tokens saved"),
    }
    if let Some(a) = &cfg.account {
        println!("account:         {}", a.account);
    }
}