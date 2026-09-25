//! Interactive terminal input — the "human in the loop" half of the CLI.
//!
//! Everything a person has to *do* lives here: choosing a login method,
//! typing an account, a password or a code. The protocol layer
//! (`oray-core`) never reads stdin and never prints: it only exchanges data
//! with the API endpoints (see `oray-core/tests/purity.rs`, which fails the
//! build if that ever stops being true).
//!
//! Prompts are written to **stderr** so that `--json` output on stdout stays
//! machine-readable even when a command asks questions. The prompts take the
//! input/output handles as arguments so the flows can be tested without a
//! terminal; the interactive callers pass `stdin().lock()` and `stderr()`.

use anyhow::{Result, bail};
use std::io::{BufRead, Write};

/// Read one line, showing `label` (plus `default` in brackets) on `out`.
///
/// A non-empty line is returned trimmed. An empty line yields `default` when
/// one is given, and is asked again otherwise. End of input is an error
/// instead of a hang, so a redirected stdin reports a usable message.
pub fn ask_from<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    label: &str,
    default: Option<&str>,
) -> Result<String> {
    loop {
        match default {
            Some(d) => write!(out, "{label} [{d}]: ")?,
            None => write!(out, "{label}: ")?,
        }
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            bail!(
                "no input for `{label}` (stdin ended); pass the values as command-line \
                 arguments instead, see `oray-tools auth --help`"
            );
        }
        let value = line.trim().to_string();
        if !value.is_empty() {
            return Ok(value);
        }
        if let Some(d) = default {
            return Ok(d.to_string());
        }
        // required but empty: ask again.
    }
}

/// Read a required, non-empty value (repeats on an empty line).
pub fn ask<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    label: &str,
    default: Option<&str>,
) -> Result<String> {
    ask_from(input, out, label, default)
}

/// Read a line without echoing it back to the terminal.
///
/// Echo is disabled with `stty -echo` when stdin is a real terminal (unix);
/// where that is unavailable the value is simply read normally, so the flow
/// still works — only less privately.
pub fn ask_secret(label: &str) -> Result<String> {
    eprint!("{label}: ");
    let _ = std::io::stderr().flush();
    let hidden = hide_echo();
    let mut line = String::new();
    let read = std::io::stdin().read_line(&mut line);
    if hidden {
        show_echo();
        // The typed characters and the final newline were not echoed.
        eprintln!();
    }
    let read = read?;
    if read == 0 {
        bail!(
            "no input for `{label}` (stdin ended); pass the values as command-line \
             arguments instead, see `oray-tools auth --help`"
        );
    }
    // Only the line terminator is removed: a password may contain spaces.
    while line.ends_with('\n') || line.ends_with('\r') {
        line.pop();
    }
    if line.is_empty() {
        bail!("no {label} entered");
    }
    Ok(line)
}

/// Present a numbered menu on `out` and return the chosen index (0-based).
///
/// Anything that is not a number inside the menu is rejected with a hint and
/// asked again; end of input is an error.
pub fn choose_from<R: BufRead, W: Write>(
    input: &mut R,
    out: &mut W,
    label: &str,
    options: &[&str],
) -> Result<usize> {
    debug_assert!(!options.is_empty());
    writeln!(out, "{label}:")?;
    for (i, option) in options.iter().enumerate() {
        writeln!(out, "  {}) {option}", i + 1)?;
    }
    loop {
        write!(out, "Choose 1-{}: ", options.len())?;
        out.flush()?;
        let mut line = String::new();
        if input.read_line(&mut line)? == 0 {
            bail!("no choice for `{label}` (stdin ended); pass the values as arguments instead");
        }
        if let Ok(n) = line.trim().parse::<usize>()
            && (1..=options.len()).contains(&n)
        {
            return Ok(n - 1);
        }
        writeln!(out, "please enter a number between 1 and {}", options.len())?;
    }
}

/// Turn terminal echo off; `false` when that was not possible (the caller
/// then treats the input as echoed).
#[cfg(unix)]
fn hide_echo() -> bool {
    stty(&["-echo"])
}

#[cfg(unix)]
fn show_echo() {
    stty(&["echo"]);
}

#[cfg(unix)]
fn stty(args: &[&str]) -> bool {
    use std::process::{Command, Stdio};
    Command::new("stty")
        .args(args)
        .stdin(Stdio::inherit())
        .stdout(Stdio::null())
        .stderr(Stdio::null())
        .status()
        .is_ok_and(|s| s.success())
}

#[cfg(not(unix))]
fn hide_echo() -> bool {
    false
}

#[cfg(not(unix))]
fn show_echo() {}

#[cfg(test)]
mod tests {
    use super::*;
    use std::io::Cursor;

    fn ask_ok(input: &str, label: &str, default: Option<&str>) -> (String, String) {
        let mut input = Cursor::new(input.to_string());
        let mut out = Vec::new();
        let value = ask(&mut input, &mut out, label, default).unwrap();
        (value, String::from_utf8(out).unwrap())
    }

    #[test]
    fn typed_value_is_trimmed() {
        let (value, prompt) = ask_ok("  alice@example.com \n", "Account", None);
        assert_eq!(value, "alice@example.com");
        assert_eq!(prompt, "Account: ");
    }

    #[test]
    fn empty_line_falls_back_to_default() {
        let (value, prompt) = ask_ok("\n", "Account", Some("saved@example.com"));
        assert_eq!(value, "saved@example.com");
        assert_eq!(prompt, "Account [saved@example.com]: ");
    }

    #[test]
    fn required_value_reasks_until_filled() {
        let mut input = Cursor::new("\n\n42\n");
        let mut out = Vec::new();
        let value = ask(&mut input, &mut out, "Code", None).unwrap();
        assert_eq!(value, "42");
        assert_eq!(String::from_utf8(out).unwrap().matches("Code: ").count(), 3);
    }

    #[test]
    fn end_of_input_is_an_error_not_a_hang() {
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let err = ask(&mut input, &mut out, "Account", None).unwrap_err();
        assert!(err.to_string().contains("`Account`"), "{err}");
        assert!(err.to_string().contains("--help"), "{err}");
    }

    #[test]
    fn choose_parses_a_valid_number() {
        let mut input = Cursor::new("2\n");
        let mut out = Vec::new();
        let index =
            choose_from(&mut input, &mut out, "Login method", &["password", "sms"]).unwrap();
        assert_eq!(index, 1);
        assert!(String::from_utf8(out).unwrap().contains("1) password"));
    }

    #[test]
    fn choose_reasks_on_out_of_range_input() {
        let mut input = Cursor::new("9\nx\n1\n");
        let mut out = Vec::new();
        let index =
            choose_from(&mut input, &mut out, "Login method", &["password", "sms"]).unwrap();
        assert_eq!(index, 0);
        let text = String::from_utf8(out).unwrap();
        assert_eq!(text.matches("between 1 and 2").count(), 2);
    }

    #[test]
    fn choose_reports_end_of_input() {
        let mut input = Cursor::new(String::new());
        let mut out = Vec::new();
        let err = choose_from(&mut input, &mut out, "Login method", &["password"]).unwrap_err();
        assert!(err.to_string().contains("stdin ended"), "{err}");
    }
}
