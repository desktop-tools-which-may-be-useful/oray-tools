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
    about = "oray-tools: control Oray smart plugs",
    after_help = "Run `oray-tools <COMMAND> --help` for command-specific options (e.g. `oray-tools login --help`)."
)]
struct Cli {
    #[command(subcommand)]
    command: Command,

    /// Path to config file (default: $XDG_CONFIG_HOME/oray-tools/config.toml)
    #[arg(long, global = true)]
    config: Option<PathBuf>,

    /// Trusted Ex-ClientId (default: built-in trusted id)
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
        /// Default port index for this plug
        #[arg(long, default_value_t = 0)]
        index: usize,
    },
    /// Remove a configured plug
    Remove { name: String },
    /// Query plug status
    Status {
        /// Plug name (defaults to `default` or the only configured plug)
        name: Option<String>,
        /// Port index (defaults to the plug's configured index)
        #[arg(long)]
        index: Option<usize>,
    },
    /// Turn the plug on
    On {
        name: Option<String>,
        #[arg(long)]
        index: Option<usize>,
    },
    /// Turn the plug off
    Off {
        name: Option<String>,
        #[arg(long)]
        index: Option<usize>,
    },
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
        Command::Login { account, password } => do_login(&mut cfg, &path, cli.clientid.as_deref(), &account, &password),
        Command::Refresh => do_refresh(&mut cfg, &path, cli.clientid.as_deref()),
        Command::Tokens => do_tokens(&cfg),
        Command::Logout => do_logout(&mut cfg, &path),
        Command::Plug { sub } => do_plug(&mut cfg, &path, sub),
    }
}

fn resolve_clientid(cfg: &Config, cli_clientid: Option<&str>) -> String {
    cli_clientid
        .filter(|c| !c.is_empty())
        .or_else(|| cfg.client.as_ref().map(|c| c.clientid.as_str()))
        .filter(|c| !c.is_empty())
        .map(|s| s.to_string())
        .unwrap_or_else(|| auth::DEFAULT_CLIENT_ID.to_string())
}

fn do_login(cfg: &mut Config, path: &PathBuf, clientid: Option<&str>, account: &str, password: &str) -> Result<()> {
    let cid = resolve_clientid(cfg, clientid);
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
    cfg.save(path)?;
    print_tokens(cfg);
    Ok(())
}

fn hostname() -> String {
    std::env::var("HOSTNAME")
        .or_else(|_| std::env::var("COMPUTERNAME"))
        .unwrap_or_else(|_| "oray-tools".to_string())
}

fn do_refresh(cfg: &mut Config, path: &PathBuf, clientid: Option<&str>) -> Result<()> {
    let token = cfg
        .token
        .as_ref()
        .context("no token saved; run `oray-tools login` first")?;
    if token.refresh_token.is_empty() {
        bail!("no refresh token saved; run `oray-tools login` first");
    }
    let cid = resolve_clientid(cfg, clientid);
    let server = cfg.server();
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

fn do_logout(cfg: &mut Config, path: &PathBuf) -> Result<()> {
    cfg.account = None;
    cfg.token = None;
    cfg.save(path)?;
    println!("logged out");
    Ok(())
}

fn do_plug(cfg: &mut Config, path: &PathBuf, sub: PlugCmd) -> Result<()> {
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
                println!("{name}{mark}: sn={} default_index={}", d.sn, d.index);
            }
        }
        PlugCmd::Add { name, sn, index } => {
            if sn.trim().is_empty() {
                bail!("SN must not be empty");
            }
            cfg.plugs.insert(name.clone(), config::Device { sn: sn.clone(), index });
            cfg.save(path)?;
            println!("plug '{name}' registered: sn={sn} index={index}");
        }
        PlugCmd::Remove { name } => {
            if cfg.plugs.remove(&name).is_none() {
                bail!("no plug named '{name}'");
            }
            cfg.save(path)?;
            println!("plug '{name}' removed");
        }
        PlugCmd::Status { name, index } => {
            do_plug_action(cfg, path, name.as_deref(), index, plug::PlugAction::Status)?
        }
        PlugCmd::On { name, index } => {
            do_plug_action(cfg, path, name.as_deref(), index, plug::PlugAction::On)?
        }
        PlugCmd::Off { name, index } => {
            do_plug_action(cfg, path, name.as_deref(), index, plug::PlugAction::Off)?
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

fn do_plug_action(cfg: &mut Config, path: &PathBuf, name: Option<&str>, index: Option<usize>, action: plug::PlugAction) -> Result<()> {
    let (name, dev) = resolve_plug(cfg, name)?;
    let index = index.unwrap_or(dev.index);
    let server = cfg.server();
    let token = auth::ensure_token(cfg, path)?;
    let client = auth::standard_client()?;

    match action {
        plug::PlugAction::Status => {
            let r = plug::get_status(&client, &server.slapi_base, &token.access_token, &dev.sn, index)?;
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
        plug::PlugAction::On => {
            plug::set_status(&client, &server.slapi_base, &token.access_token, &dev.sn, index, true)?;
            println!("plug={name} sn={} port={index} ON", dev.sn);
        }
        plug::PlugAction::Off => {
            plug::set_status(&client, &server.slapi_base, &token.access_token, &dev.sn, index, false)?;
            println!("plug={name} sn={} port={index} OFF", dev.sn);
        }
    }
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