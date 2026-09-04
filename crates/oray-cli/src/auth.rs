//! `auth` command group: authentication management (locally stored).

use crate::config::Config;
use crate::support::{emit_json, hostname, print_tokens, resolve_clientid};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use oray_core::auth::{AuthApi, LoginOutcome};
use reqwest::blocking::Client as HttpClient;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum AuthCmd {
    /// Log in with an Oray account (a fresh device may prompt for an SMS code)
    Login {
        /// Oray account (mobile number or email)
        account: String,
        /// Account password (stored locally as md5)
        password: String,
    },
    /// Renew tokens with the saved refresh_token
    Refresh,
    /// Show current token info and expiry
    Status,
    /// Clear saved tokens and account
    Logout,
}

pub fn run(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    clientid: Option<&str>,
    sub: AuthCmd,
    json: bool,
) -> Result<()> {
    match sub {
        AuthCmd::Login { account, password } => {
            do_login(http, cfg, path, clientid, &account, &password, json)
        }
        AuthCmd::Refresh => do_refresh(http, cfg, path, clientid, json),
        AuthCmd::Status => do_status(cfg, json),
        AuthCmd::Logout => do_logout(cfg, path, json),
    }
}

fn do_login(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    clientid: Option<&str>,
    account: &str,
    password: &str,
    json: bool,
) -> Result<()> {
    let cid = resolve_clientid(cfg, clientid);
    let server = cfg.server();
    let api = AuthApi::new(http.clone(), &server.api_base);
    let terminal_name = hostname();
    let password_md5 = oray_core::auth::md5_hex(password);
    let resp = match api.login(&cid, account, &password_md5)? {
        LoginOutcome::Tokens(resp) => resp,
        LoginOutcome::NewDevice(alert) => {
            let target = if !alert.mobile.is_empty() {
                &alert.mobile
            } else {
                &alert.email
            };
            eprintln!(
                "New device detected ({}), code={}: {target} requires SMS verification. A code has been sent.",
                alert.error, alert.code
            );
            api.sendcode(&cid, account)
                .context("failed to send verification code")?;
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
                LoginOutcome::Tokens(resp) => resp,
                other => bail!("re-login did not return tokens: {other:?}"),
            }
        }
    };

    cfg.account = Some(crate::config::Account {
        account: account.to_string(),
        password_md5,
    });
    cfg.client = Some(crate::config::Client { clientid: cid });
    let expiry = crate::token::refresh_expiry(&resp);
    cfg.token = Some(crate::config::Token {
        access_token: resp.access_token,
        refresh_token: resp.refresh_token,
        refresh_expires: expiry,
    });
    cfg.save(path)?;
    if json {
        emit_json(true, &serde_json::json!({ "ok": true, "account": account }))?;
    } else {
        println!("logged in as {account}");
    }
    Ok(())
}

fn do_refresh(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    clientid: Option<&str>,
    json: bool,
) -> Result<()> {
    let (access, refresh) = {
        let token = cfg
            .token
            .as_ref()
            .context("no token saved; run `oray-tools auth login` first")?;
        if token.refresh_token.is_empty() {
            bail!("no refresh token saved; run `oray-tools auth login` first");
        }
        (token.access_token.clone(), token.refresh_token.clone())
    };
    let cid = resolve_clientid(cfg, clientid);
    let server = cfg.server();
    let api = AuthApi::new(http.clone(), &server.api_base);
    let resp = api.refresh(&cid, &access, &refresh)?;
    let expiry = crate::token::refresh_expiry(&resp);
    cfg.token = Some(crate::config::Token {
        access_token: resp.access_token,
        refresh_token: resp.refresh_token,
        refresh_expires: expiry,
    });
    cfg.save(path)?;
    if json {
        emit_json(
            true,
            &serde_json::json!({ "ok": true, "account": cfg.account.as_ref().map(|a| a.account.clone()) }),
        )?;
    } else {
        println!("tokens refreshed");
    }
    Ok(())
}

fn do_status(cfg: &Config, json: bool) -> Result<()> {
    if json {
        let access_expiry = cfg
            .token
            .as_ref()
            .and_then(|t| crate::token::access_expiry(&t.access_token));
        let v = serde_json::json!({
            "logged_in": cfg.token.is_some() && cfg.account.is_some(),
            "account": cfg.account.as_ref().map(|a| a.account.clone()),
            "access_expires": access_expiry,
            "refresh_expires": cfg.token.as_ref().map(|t| t.refresh_expires),
        });
        emit_json(true, &v)?;
        return Ok(());
    }
    print_tokens(cfg);
    Ok(())
}

fn do_logout(cfg: &mut Config, path: &PathBuf, json: bool) -> Result<()> {
    cfg.account = None;
    cfg.token = None;
    cfg.save(path)?;
    if json {
        emit_json(true, &serde_json::json!({ "ok": true }))?;
    } else {
        println!("logged out");
    }
    Ok(())
}
