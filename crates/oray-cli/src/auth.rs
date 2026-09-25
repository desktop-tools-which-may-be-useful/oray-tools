//! `auth` command group: authentication management (locally stored).

use crate::config::Config;
use crate::prompt;
use crate::support::{emit_json, hostname, print_tokens, resolve_clientid, traced};
use anyhow::{Context, Result, bail};
use clap::Subcommand;
use oray_core::auth::{AuthApi, LoginOutcome};
use reqwest::blocking::Client as HttpClient;
use std::io::IsTerminal;
use std::path::PathBuf;

#[derive(Subcommand)]
pub enum AuthCmd {
    /// Interactive sign-in: choose a login method and type the parameters
    ///
    /// This is the only command that prompts. `login`/`login-sms` keep their
    /// required arguments and fail when one is missing.
    Interactive,
    /// Log in with an Oray account (a fresh device may prompt for an SMS code)
    Login {
        /// Oray account (mobile number or email)
        account: String,
        /// Account password (stored locally as md5)
        password: String,
    },
    /// Log in with an SMS code instead of a password (`auth login-sms`)
    LoginSms {
        /// Mobile number bound to the account; it receives the login code
        mobile: String,
        /// Use a code you already received (e.g. from the official app):
        /// skips the captcha step and the SMS request
        #[arg(long)]
        code: Option<String>,
        /// Reuse a captcha token obtained elsewhere instead of the browser
        /// step (advanced / scripted use)
        #[arg(long)]
        captcha: Option<String>,
        /// Print the captcha URL instead of opening a browser
        #[arg(long)]
        no_browser: bool,
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
        // Interactive prompting is opt-in: only this subcommand asks.
        AuthCmd::Interactive => interactive(http, cfg, path, clientid, json),
        AuthCmd::Login { account, password } => {
            do_login(http, cfg, path, clientid, &account, &password, json)
        }
        AuthCmd::LoginSms {
            mobile,
            code,
            captcha,
            no_browser,
        } => do_login_sms(
            http,
            cfg,
            path,
            clientid,
            &mobile,
            SmsLoginOpts {
                code: code.as_deref(),
                captcha: captcha.as_deref(),
                no_browser,
            },
            json,
        ),
        AuthCmd::Refresh => do_refresh(http, cfg, path, clientid, json),
        AuthCmd::Status => do_status(cfg, json),
        AuthCmd::Logout => do_logout(cfg, path, json),
    }
}

/// Interactive session behind `oray-tools auth interactive`: pick a login
/// method, then enter the parameters it needs. The protocol layer is
/// untouched by this — it only receives the finished values.
fn interactive(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    clientid: Option<&str>,
    json: bool,
) -> Result<()> {
    if !std::io::stdin().is_terminal() {
        bail!(
            "the interactive session needs a terminal; run a concrete command instead, \
             e.g. `oray-tools auth login <account> <password>` or \
             `oray-tools auth login-sms <mobile>`"
        );
    }
    // The stdin lock is held only for the menu: `Stdin::lock` is not
    // reentrant, and the helpers below read stdin themselves.
    eprintln!("oray-tools: interactive sign-in");
    let method = {
        let stdin = std::io::stdin();
        let mut input = stdin.lock();
        prompt::choose_from(
            &mut input,
            &mut std::io::stderr(),
            "Login method",
            &[
                "password (account + password)",
                "SMS code (mobile, no password)",
                "quit",
            ],
        )?
    };
    match method {
        0 => {
            let (account, password) = ask_password_login(cfg)?;
            do_login(http, cfg, path, clientid, &account, &password, json)
        }
        1 => {
            let mobile = ask_mobile(cfg)?;
            do_login_sms(
                http,
                cfg,
                path,
                clientid,
                &mobile,
                SmsLoginOpts::default(),
                json,
            )
        }
        _ => {
            eprintln!("cancelled");
            Ok(())
        }
    }
}

/// Collect the account/password pair. The password comes back from a
/// no-echo prompt.
fn ask_password_login(cfg: &Config) -> Result<(String, String)> {
    let saved = cfg.account.as_ref().map(|a| a.account.clone());
    let account = prompt::ask(
        &mut std::io::stdin().lock(),
        &mut std::io::stderr(),
        "Account (mobile or email)",
        saved.as_deref(),
    )?;
    let password = prompt::ask_secret("Password")?;
    Ok((account, password))
}

/// Collect the mobile number for the SMS flow, offering the saved account
/// when it is a plausible mobile number.
fn ask_mobile(cfg: &Config) -> Result<String> {
    let saved = cfg
        .account
        .as_ref()
        .map(|a| a.account.clone())
        .filter(|a| check_mobile(a).is_ok());
    let mobile = prompt::ask(
        &mut std::io::stdin().lock(),
        &mut std::io::stderr(),
        "Mobile number",
        saved.as_deref(),
    )?;
    check_mobile(&mobile)?;
    Ok(mobile)
}

/// `123****8901` — recognizable, but not usable to harvest the number.
fn mask_mobile(mobile: &str) -> String {
    let chars: Vec<char> = mobile.chars().collect();
    if chars.len() >= 7 {
        let head: String = chars[..3].iter().collect();
        let tail: String = chars[chars.len() - 4..].iter().collect();
        format!("{head}****{tail}")
    } else {
        "*".repeat(chars.len().max(1))
    }
}

/// A login target for the SMS flow: digits (an optional leading `+` for
/// international numbers), 6-15 characters.
fn check_mobile(mobile: &str) -> Result<()> {
    let digits = mobile.strip_prefix('+').unwrap_or(mobile);
    if digits.is_empty() || digits.len() > 15 || !digits.bytes().all(|b| b.is_ascii_digit()) {
        bail!("expected a mobile number like 12345678901, got '{mobile}'");
    }
    Ok(())
}

/// Read a verification code from stdin (shared by both login flows).
fn prompt_code() -> Result<String> {
    prompt::ask(
        &mut std::io::stdin().lock(),
        &mut std::io::stderr(),
        "SMS code",
        None,
    )
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
    let resp = match traced("login", api.login(&cid, account, &password_md5))? {
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
            traced("send verification code", api.sendcode(&cid, account))?;
            let code = prompt_code()?;
            traced(
                "verify code",
                api.checkcode(&cid, account, &code, &terminal_name),
            )?;
            eprintln!("Device trusted, logging in again...");
            match traced("login", api.login(&cid, account, &password_md5))? {
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

/// Optional inputs of the SMS login flow.
#[derive(Default)]
struct SmsLoginOpts<'a> {
    /// A code the user already holds (e.g. requested in the official app):
    /// skips the captcha step and the SMS request.
    code: Option<&'a str>,
    /// Captcha token supplied instead of running the browser step.
    captcha: Option<&'a str>,
    /// Print the captcha URL instead of opening a browser.
    no_browser: bool,
}

/// Passwordless login: solve the captcha in a browser, receive the SMS code
/// and exchange it for tokens.
fn do_login_sms(
    http: &HttpClient,
    cfg: &mut Config,
    path: &PathBuf,
    clientid: Option<&str>,
    mobile: &str,
    opts: SmsLoginOpts<'_>,
    json: bool,
) -> Result<()> {
    check_mobile(mobile)?;
    let cid = resolve_clientid(cfg, clientid);
    let server = cfg.server();
    let api = AuthApi::new(http.clone(), &server.api_base).shield_base(&server.shield_base);

    // The shield endpoint only accepts a request carrying an Aliyun captcha
    // result, so one has to be produced in a browser first — unless a code
    // was obtained elsewhere and nothing has to be sent.
    let code = match opts.code {
        Some(code) => {
            eprintln!("using the supplied code; skipping the captcha and the SMS request");
            code.to_string()
        }
        None => {
            let captcha_token = match opts.captcha {
                Some(token) => token.to_string(),
                None => crate::captcha::obtain_token(
                    &mask_mobile(mobile),
                    !opts.no_browser,
                    crate::captcha::DEFAULT_TIMEOUT,
                )?,
            };
            let sent = traced("send sms code", api.send_login_code(mobile, &captcha_token))?;
            if let Some(n) = sent.request_num {
                eprintln!(
                    "verification code requested for {} (daily request #{n})",
                    mask_mobile(mobile)
                );
            } else {
                eprintln!("verification code requested for {}", mask_mobile(mobile));
            }
            prompt_code()?
        }
    };
    let resp = match traced("sms login", api.login_with_code(&cid, mobile, &code))? {
        LoginOutcome::Tokens(resp) => resp,
        LoginOutcome::NewDevice(alert) => bail!(
            "server asked for extra device verification ({}); a password login is required for \
             this device — run `oray-tools auth login <account> <password>`",
            alert.error
        ),
    };

    // No password backs an SMS login: keep a stored md5 only when it belongs
    // to this same account, otherwise clear it.
    let password_md5 = cfg
        .account
        .as_ref()
        .filter(|a| a.account == mobile)
        .map(|a| a.password_md5.clone())
        .unwrap_or_default();
    cfg.account = Some(crate::config::Account {
        account: mobile.to_string(),
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
        emit_json(true, &serde_json::json!({ "ok": true, "account": mobile }))?;
    } else {
        println!("logged in as {mobile}");
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
    let resp = traced("refresh", api.refresh(&cid, &access, &refresh))?;
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
