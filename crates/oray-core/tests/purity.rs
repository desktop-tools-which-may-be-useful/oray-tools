//! The core stays a protocol layer: endpoints, request/response shapes and
//! data exchange only. Anything that involves a human (reading stdin,
//! prompting, opening a browser, spawning a process, touching the
//! filesystem) belongs to `oray-cli`.
//!
//! This test scans every `.rs` file of `oray-core` and fails when one of the
//! interaction primitives shows up, so the boundary cannot erode silently.

use std::fs;
use std::path::{Path, PathBuf};

/// Interaction primitives that must never appear in `oray-core`.
///
/// Written as fragments so this file itself does not trip the scan.
const FORBIDDEN: &[&str] = &[
    "std::io",
    "stdin",
    "stdout",
    "read_line",
    "println",
    "print!",
    "eprint",
    "prompt",
    "std::process",
    "Command::new",
    "TcpListener",
    "TcpStream",
    "open_browser",
    "std::fs",
    "File::",
    "std::env",
];

/// Drop `//` line comments, ignoring `//` that lives inside a string literal
/// (the core carries full URLs, e.g. `https://…`).
fn strip_comments(line: &str) -> String {
    let mut in_string = false;
    let mut escaped = false;
    let bytes: Vec<char> = line.chars().collect();
    let mut i = 0;
    while i < bytes.len() {
        let c = bytes[i];
        if in_string {
            if escaped {
                escaped = false;
            } else if c == '\\' {
                escaped = true;
            } else if c == '"' {
                in_string = false;
            }
        } else if c == '"' {
            in_string = true;
        } else if c == '/' && bytes.get(i + 1) == Some(&'/') {
            return bytes[..i].iter().collect();
        }
        i += 1;
    }
    line.to_string()
}

fn collect_sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(entries) = fs::read_dir(dir) else {
        return;
    };
    for entry in entries.flatten() {
        let path = entry.path();
        if path.is_dir() {
            collect_sources(&path, out);
        } else if path.extension().is_some_and(|e| e == "rs") {
            out.push(path);
        }
    }
}

#[test]
fn core_contains_no_human_interaction() {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("src");
    let mut files = Vec::new();
    collect_sources(&root, &mut files);
    assert!(
        !files.is_empty(),
        "no sources found under {}",
        root.display()
    );

    let mut violations = Vec::new();
    for file in &files {
        let Ok(text) = fs::read_to_string(file) else {
            continue;
        };
        for (index, line) in text.lines().enumerate() {
            let code = strip_comments(line);
            for token in FORBIDDEN {
                if code.contains(token) {
                    let rel = file.strip_prefix(&root).unwrap_or(file);
                    violations.push(format!("{}:{}: `{token}`", rel.display(), index + 1));
                }
            }
        }
    }

    assert!(
        violations.is_empty(),
        "oray-core must stay a protocol/data layer with no human interaction; \
         move these to oray-cli (prompt.rs / captcha.rs / support.rs):\n  {}",
        violations.join("\n  ")
    );
}
