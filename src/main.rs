mod auth;
mod config;
mod plug;

use anyhow::{Context, Result, bail};
use clap::{Parser, Subcommand};
use config::Config;
use std::path::PathBuf;

#[derive(Parser)]
#[command(
    name = "oray-tools",
    version,
    about = "oray-tools: control Oray smart plugs (login / refresh / on / off)"
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Path to config file (default: $XDG_CONFIG_HOME/oray-tools/config.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,
}

#[derive(Subcommand)]
enum Command {
    /// Authenticate and persist tokens
    Login {
        account: String,
        password: String,
        /// Trusted Ex-ClientId (default: built-in trusted id)
        #[arg(long)]
        clientid: Option<String>,
        /// Default device SN (optional, written to config)
        #[arg(long)]
        sn: Option<String>,
        /// Default port index (optional, written to config)
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Renew tokens with refresh_token and persist them
    Refresh,
    /// Show current token info and expiry
    Tokens,
    /// Query plug status
    Status {
        #[arg(long)]
        sn: Option<String>,
        #[arg(long)]
        index: Option<usize>,
    },
    /// Turn the plug on
    On {
        #[arg(long)]
        sn: Option<String>,
        #[arg(long)]
        index: Option<usize>,
    },
    /// Turn the plug off
    Off {
        #[arg(long)]
        sn: Option<String>,
        #[arg(long)]
        index: Option<usize>,
    },
    /// Clear saved tokens and account
    Logout,
}

fn main() {
    let cli = Cli::parse();
    if let Err(e) = run(cli) {
        eprintln!("error: {e:#}");
        std::process::exit(1);
    }
}

fn run(cli: Cli) -> Result<()> {
    let (mut cfg, path) = Config::load(cli.config.as_ref())?;
    match cli.command {
        Command::Login { account, password, clientid, sn, index } => {
            do_login(&mut cfg, &path, &account, &password, clientid, sn, index)
        }
        Command::Refresh => do_refresh(&mut cfg, &path),
        Command::Tokens => do_tokens(&cfg),
        Command::Status { sn, index } => do_plug(&mut cfg, &path, sn, index, plug::PlugAction::Status),
        Command::On { sn, index } => do_plug(&mut cfg, &path, sn, index, plug::PlugAction::On),
        Command::Off { sn, index } => do_plug(&mut cfg, &path, sn, index, plug::PlugAction::Off),
        Command::Logout => do_logout(&mut cfg, &path),
    }
}

fn do_login(
    cfg: &mut Config,
    path: &PathBuf,
    account: &str,
    password: &str,
    clientid: Option<String>,
    sn: Option<String>,
    index: usize,
) -> Result<()> {
    let cid = match clientid.or_else(|| cfg.client.as_ref().map(|c| c.clientid.clone())) {
        Some(c) if !c.is_empty() => c,
        _ => auth::DEFAULT_CLIENT_ID.to_string(),
    };
    let server = cfg.server();
    let client = auth::standard_client()?;
    let terminal_name = hostname();
    let resp = match auth::login(&client, &server, &cid, account, password)? {
        auth::LoginOutcome::Tokens(resp) => resp,
        auth::LoginOutcome::NewDevice(alert) => {
            let target = if !alert.mobile.is_empty() { &alert.mobile } else { &alert.email };
            eprintln!(
                "New device detected ({}), code={}: {target} requires SMS verification. A code has been sent.",
                alert.error, alert.code
            );
            auth::sendcode(&client, &server, &cid, account).context("failed to send verification code")?;
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
            auth::checkcode(&client, &server, &cid, account, &code, &terminal_name)
                .context("failed to verify code")?;
            eprintln!("Device trusted, logging in again...");
            match auth::login(&client, &server, &cid, account, password)? {
                auth::LoginOutcome::Tokens(resp) => resp,
                other => bail!("re-login did not return tokens: {other:?}"),
            }
        }
    };

    cfg.account = Some(config::Account {
        account: account.to_string(),
        password_md5: auth::md5_hex(password),
    });
    cfg.client = Some(config::Client { clientid: cid });
    let expiry = auth::refresh_expiry(&resp);
    cfg.token = Some(config::Token {
        access_token: resp.access_token,
        refresh_token: resp.refresh_token,
        refresh_expires: expiry,
    });
    if let Some(sn) = sn {
        cfg.device = Some(config::Device { sn, index });
    }
    cfg.save(path)?;
    print_tokens(cfg);
    Ok(())
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "oray-tools".to_string())
}

fn do_refresh(cfg: &mut Config, path: &PathBuf) -> Result<()> {
    let token = cfg
        .token
        .as_ref()
        .context("no token saved; run `oray-tools login` first")?;
    if token.refresh_token.is_empty() {
        bail!("no refresh token saved; run `oray-tools login` first");
    }
    let server = cfg.server();
    let cid = cfg
        .client
        .as_ref()
        .map(|c| c.clientid.clone())
        .filter(|c| !c.is_empty())
        .unwrap_or_else(|| auth::DEFAULT_CLIENT_ID.to_string());
    let client = auth::standard_client()?;
    let resp = auth::refresh(&client, &server, &cid, &token.access_token, &token.refresh_token)?;
    let expiry = auth::refresh_expiry(&resp);
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

fn do_plug(cfg: &mut Config, path: &PathBuf, sn: Option<String>, index: Option<usize>, action: plug::PlugAction) -> Result<()> {
    let default_index = cfg.device.as_ref().map(|d| d.index).unwrap_or(0);
    let index = index.unwrap_or(default_index);
    let sn = match sn.or_else(|| cfg.device.as_ref().map(|d| d.sn.clone())) {
        Some(s) if !s.is_empty() => s,
        _ => bail!("no SN; pass --sn or set a default device in config"),
    };
    let server = cfg.server();
    let token = auth::ensure_token(cfg, path)?;
    let client = auth::standard_client()?;

    match action {
        plug::PlugAction::Status => {
            let r = plug::get_status(&client, &server.slapi_base, &token.access_token, &sn, index)?;
            let mut found = false;
            if let Some(ports) = &r.response {
                for p in ports {
                    if p.index as usize == index {
                        let state = if p.status == 1 { "ON" } else { "OFF" };
                        println!("sn={sn} index={} status={state}", p.index);
                        found = true;
                    }
                }
            }
            if !found {
                println!("sn={sn} index={index} status=<<unknown>>");
            }
        }
        plug::PlugAction::On => {
            plug::set_status(&client, &server.slapi_base, &token.access_token, &sn, index, true)?;
            println!("sn={sn} port={index} ON");
        }
        plug::PlugAction::Off => {
            plug::set_status(&client, &server.slapi_base, &token.access_token, &sn, index, false)?;
            println!("sn={sn} port={index} OFF");
        }
    }
    Ok(())
}

fn do_logout(cfg: &mut Config, path: &PathBuf) -> Result<()> {
    cfg.account = None;
    cfg.token = None;
    cfg.save(path)?;
    println!("logged out");
    Ok(())
}

fn print_tokens(cfg: &Config) {
    match &cfg.token {
        Some(t) => {
            println!("access_token:    {}", t.access_token);
            println!("refresh_token:   {}", t.refresh_token);
            println!(
                "access_expiry:   {}",
                auth::access_expiry(&t.access_token)
                    .map(auth::human_time)
                    .unwrap_or_else(|| "unknown".to_string())
            );
            println!(
                "refresh_expiry:  {}",
                auth::human_time(t.refresh_expires)
            );
        }
        None => println!("no tokens saved"),
    }
    if let Some(a) = &cfg.account {
        println!("account:         {}", a.account);
    }
}