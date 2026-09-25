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
    /// Log in with an Oray account (a fresh device may prompt for an SMS code)
    Login {
        /// Oray account (mobile number or email)
        account: Option<String>,
        /// Account password (stored locally as md5)
        ///
        /// SECURITY: a password given as an argument is visible to other
        /// local processes (`ps`, `/proc/*/cmdline`) and lands in your shell
        /// history. Prefer `oray-tools auth login --interactive`, which
        /// reads the password without echoing it.
        password: Option<String>,
    },
    /// Log in with an SMS code instead of a password (`auth login-sms`)
    LoginSms {
        /// Mobile number bound to the account; it receives the login code
        mobile: Option<String>,
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
    /// Show current token info and expiry (tokens masked by default)
    Status {
        /// Print the access/refresh tokens in full instead of masking them
        #[arg(long)]
        show: bool,
    },
    /// Clear saved tokens and account
    Logout,
}

/// `--interactive`: type the arguments the command line left out.
///
/// Without the flag clap rejects a missing argument before this runs, so
/// every `None` below is one the user explicitly asked to be prompted for —
/// and `interactive` gates that explicitly too: this is a no-op unless the
/// flag is set, so no code path can reach a prompt without `--interactive`
/// even if `strictify`'s fillable-argument list (main.rs) ever falls out of
/// sync. The login method is never a choice here — it is the subcommand
/// itself (`login` = account + password, `login-sms` = mobile number).
pub fn fill(cmd: &mut AuthCmd, cfg: &Config, interactive: bool) -> Result<()> {
    if !interactive {
        return Ok(());
    }
    match cmd {
        AuthCmd::Login { account, password } => {
            prompt::require_terminal(
                &[
                    ("<ACCOUNT>", account.is_none()),
                    ("<PASSWORD>", password.is_none()),
                ],
                "oray-tools auth login <account> <password>",
            )?;
            prompt::fill_str(
                account,
                "Account (mobile or email)",
                saved_account(cfg).as_deref(),
            )?;
            if password.is_none() {
                // Read without echoing: `ask_stdin` would print it back.
                *password = Some(prompt::ask_secret("Password")?);
            }
            Ok(())
        }
        AuthCmd::LoginSms { mobile, .. } => {
            prompt::require_terminal(
                &[("<MOBILE>", mobile.is_none())],
                "oray-tools auth login-sms <mobile>",
            )?;
            prompt::fill_checked(
                mobile,
                "Mobile number",
                saved_account(cfg)
                    .filter(|a| check_mobile(a).is_ok())
                    .as_deref(),
                check_mobile,
            )
        }
        // Nothing to complete: no arguments at all (`--show` is a flag).
        AuthCmd::Refresh | AuthCmd::Status { .. } | AuthCmd::Logout => Ok(()),
    }
}

/// The account stored by an earlier login, offered as a prompt default.
fn saved_account(cfg: &Config) -> Option<String> {
    cfg.account.as_ref().map(|a| a.account.clone())
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
            let account = account.as_deref().context("missing <ACCOUNT>")?;
            let password = password.as_deref().context("missing <PASSWORD>")?;
            do_login(http, cfg, path, clientid, account, password, json)
        }
        AuthCmd::LoginSms {
            mobile,
            code,
            captcha,
            no_browser,
        } => {
            let mobile = mobile.as_deref().context("missing <MOBILE>")?;
            do_login_sms(
                http,
                cfg,
                path,
                clientid,
                mobile,
                SmsLoginOpts {
                    code: code.as_deref(),
                    captcha: captcha.as_deref(),
                    no_browser,
                },
                json,
            )
        }
        AuthCmd::Refresh => do_refresh(http, cfg, path, clientid, json),
        AuthCmd::Status { show } => do_status(cfg, json, show),
        AuthCmd::Logout => do_logout(cfg, path, json),
    }
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

/// Whether a flow that is about to *request* an SMS code cannot possibly
/// collect it: no code was supplied up front and stdin is not a terminal,
/// so the read at the end of the flow would fail anyway. Pure (both inputs
/// are decided by the caller) so the fail-fast decision is testable without
/// a TTY.
///
/// The point is *when* the failure happens: without this guard a scripted
/// run sits through the whole captcha wait (up to 300 s) and consumes a
/// daily SMS request (`daily request #n`) before `prompt_code` reports that
/// stdin ended — the side effects have already happened for nothing.
fn code_unreachable(has_code: bool, is_terminal: bool) -> bool {
    !has_code && !is_terminal
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
            // Fail before the SMS request below: without a terminal nobody
            // can read the code `prompt_code` is about to ask for, and
            // `sendcode` would consume a daily SMS slot for nothing. (This
            // flow has no `--code` escape hatch — hence `has_code = false`.)
            if code_unreachable(false, std::io::stdin().is_terminal()) {
                bail!(
                    "new-device SMS verification is required ({}), but stdin is not a terminal \
                     so the code could not be read: run `oray-tools auth login <account> \
                     <password>` in a terminal to complete it",
                    alert.error
                );
            }
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
    // Fail fast, before anything waits or sends: without `--code` this flow
    // ends by reading the SMS code from stdin, which is impossible when
    // stdin is not a terminal. Waiting out the captcha (up to 300 s) and
    // burning a daily SMS request first would only defer an error that is
    // certain — so check before `obtain_token` / `send_login_code`.
    if code_unreachable(opts.code.is_some(), std::io::stdin().is_terminal()) {
        bail!(
            "stdin is not a terminal, so the SMS code could not be read: pass `--code <CODE>` \
             with a code you already have (it skips the captcha and the SMS request), or run \
             `oray-tools auth login-sms <mobile> --interactive` in a terminal"
        );
    }
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

fn do_status(cfg: &Config, json: bool, show: bool) -> Result<()> {
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
    print_tokens(cfg, show);
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The fail-fast decision shared by both login flows (see
    /// [`code_unreachable`]): it only fires when a code will certainly be
    /// requested *and* cannot be collected.
    #[test]
    fn sms_code_guard_fails_only_without_code_and_terminal() {
        // `echo | oray-tools auth login-sms <mobile>`: bail immediately,
        // before the captcha wait and before any SMS-consuming request.
        assert!(code_unreachable(false, false));
        // `--code 123456` skips the captcha + SMS and proceeds (it does not
        // read the code from stdin).
        assert!(!code_unreachable(true, false));
        // A real terminal keeps behaving exactly as before.
        assert!(!code_unreachable(false, true));
        assert!(!code_unreachable(true, true));
    }
}
