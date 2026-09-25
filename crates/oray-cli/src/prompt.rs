//! Interactive terminal input — the "human in the loop" half of the CLI.
//!
//! Everything a person has to *do* lives here: typing the argument the
//! command line left out (only when `--interactive` asked for it), a
//! password or a code. The protocol layer (`oray-core`) never reads stdin
//! and never prints: it only exchanges data with the API endpoints (see
//! `oray-core/tests/purity.rs`, which fails the build if that ever stops
//! being true).
//!
//! Prompts are written to **stderr** so that `--json` output on stdout stays
//! machine-readable even when a command asks questions. The prompts take the
//! input/output handles as arguments so the flows can be tested without a
//! terminal; the interactive callers go through [`ask_stdin`] / [`ask_secret`].

use anyhow::{Result, bail};
use std::fmt::Display;
use std::io::{BufRead, IsTerminal, Write};
use std::str::FromStr;

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

/// Fail when `--interactive` would have to prompt for an argument but stdin
/// is not a terminal (a pipe or CI would hang otherwise).
///
/// `args` lists every argument of the command as a `(name, is_missing)` pair
/// so the error can name *all* the values that are still missing, together
/// with the concrete command that supplies them — nothing has to be guessed.
/// When nothing is missing this is a no-op and stdin is never touched.
pub fn require_terminal(args: &[(&str, bool)], example: &str) -> Result<()> {
    let missing: Vec<&str> = args
        .iter()
        .filter(|(_, is_missing)| *is_missing)
        .map(|(name, _)| *name)
        .collect();
    if missing.is_empty() {
        return Ok(());
    }
    if !std::io::stdin().is_terminal() {
        bail!(
            "`--interactive` needs an interactive terminal, but stdin is not one; missing {} — \
             pass the value{} directly instead, e.g. `{example}`",
            missing.join(" "),
            if missing.len() == 1 { "" } else { "s" }
        );
    }
    Ok(())
}

/// [`ask`] against the process's own stdin/stderr for a single value.
///
/// The stdin lock is taken and released inside the call, so [`ask_secret`]
/// (which reads stdin itself) never runs while it is held — `Stdin::lock` is
/// not reentrant.
pub fn ask_stdin(label: &str, default: Option<&str>) -> Result<String> {
    ask(
        &mut std::io::stdin().lock(),
        &mut std::io::stderr(),
        label,
        default,
    )
}

/// Fill `slot` with [`ask_stdin`] when the command line left it empty.
///
/// Callers run [`require_terminal`] for the whole command first, so this
/// only ever prompts on a real terminal.
pub fn fill_str(slot: &mut Option<String>, label: &str, default: Option<&str>) -> Result<()> {
    if slot.is_none() {
        *slot = Some(ask_stdin(label, default)?);
    }
    Ok(())
}

/// Like [`fill_str`], but keeps asking until the line parses as `T`
/// (a numeric argument: id, count, ...).
pub fn fill_parsed<T>(slot: &mut Option<T>, label: &str) -> Result<()>
where
    T: FromStr,
    T::Err: Display,
{
    if slot.is_some() {
        return Ok(());
    }
    loop {
        let line = ask_stdin(label, None)?;
        match line.parse::<T>() {
            Ok(value) => {
                *slot = Some(value);
                return Ok(());
            }
            Err(e) => eprintln!("error: expected a number for `{label}`, got '{line}' ({e})"),
        }
    }
}

/// Like [`fill_str`], but keeps asking until `ok` accepts the line (an
/// argument with a known format: mobile number, on/off, clock time, ...).
pub fn fill_checked<F>(
    slot: &mut Option<String>,
    label: &str,
    default: Option<&str>,
    ok: F,
) -> Result<()>
where
    F: Fn(&str) -> Result<()>,
{
    if slot.is_some() {
        return Ok(());
    }
    loop {
        let line = ask_stdin(label, default)?;
        match ok(&line) {
            Ok(()) => {
                *slot = Some(line);
                return Ok(());
            }
            Err(e) => eprintln!("error: {e}"),
        }
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
    fn require_terminal_nothing_missing_never_touches_stdin() {
        // Nothing missing: never reads stdin, so it cannot block.
        require_terminal(&[], "oray-tools wakeup info <sn>").unwrap();
    }
}
