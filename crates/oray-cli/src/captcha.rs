//! Browser handshake for the Aliyun slider captcha.
//!
//! The shield endpoint that sends the SMS login code refuses any request
//! without a captcha result (`aliyun_captcha_response`), and a captcha can
//! only be produced by running Aliyun's widget in a real browser. This module
//! serves the widget page from a loopback socket, opens it in the user's
//! browser and waits for the solved token, which is then handed back to the
//! caller.
//!
//! The page itself lives in `captcha_page.html`; it posts the token to
//! `POST /token` on the same loopback origin.

use anyhow::{Context, Result, bail};
use std::io::{Read, Write};
use std::net::{TcpListener, TcpStream};
use std::process::Command;
use std::sync::mpsc;
use std::time::Duration;

/// Embedded captcha page (`{{MOBILE}}` is replaced with the masked number).
const PAGE: &str = include_str!("captcha_page.html");
/// Cap on the request head we are willing to buffer.
const MAX_HEAD: usize = 16 * 1024;
/// Cap on the captcha POST body.
const MAX_BODY: usize = 64 * 1024;
/// How long to wait for the user to solve the captcha.
pub const DEFAULT_TIMEOUT: Duration = Duration::from_secs(300);

/// A decoded loopback HTTP request (head + body).
#[derive(Debug, Default)]
pub struct Request {
    pub method: String,
    pub path: String,
    pub body: Vec<u8>,
}

/// Resolve a request to `(status, content type, body)`. `POST /token` also
/// forwards the captcha token to `token`.
fn route(req: &Request, mobile: &str, token: &mpsc::Sender<String>) -> (u16, &'static str, String) {
    match (req.method.as_str(), req.path.as_str()) {
        ("GET", "/") | ("GET", "/index.html") => (
            200,
            "text/html; charset=utf-8",
            PAGE.replace("{{MOBILE}}", &escape(mobile)),
        ),
        ("POST", "/token") => {
            let value: serde_json::Value =
                serde_json::from_slice(&req.body).unwrap_or(serde_json::Value::Null);
            let captcha = value
                .get("captchaVerifyParam")
                .and_then(|v| v.as_str())
                .filter(|s| !s.is_empty())
                .map(str::to_string)
                .or_else(|| {
                    value
                        .get("raw")
                        .and_then(|v| v.as_str())
                        .filter(|s| !s.is_empty())
                        .map(str::to_string)
                });
            match captcha {
                // The receiver is gone once the first token arrived.
                Some(t) => match token.send(t) {
                    Ok(()) => (200, "text/plain; charset=utf-8", "ok".to_string()),
                    Err(_) => (
                        410,
                        "text/plain; charset=utf-8",
                        "already received".to_string(),
                    ),
                },
                None => (
                    400,
                    "text/plain; charset=utf-8",
                    "missing captcha".to_string(),
                ),
            }
        }
        _ => (404, "text/plain; charset=utf-8", "not found".to_string()),
    }
}

/// Minimal HTML escaping for the page interpolation.
fn escape(s: &str) -> String {
    s.replace('&', "&amp;")
        .replace('<', "&lt;")
        .replace('>', "&gt;")
        .replace('"', "&quot;")
}

/// Read one HTTP request from `stream`, returning `None` when the peer hangs
/// up without sending a complete request head.
fn read_request(stream: &mut TcpStream) -> std::io::Result<Option<Request>> {
    let mut buf = Vec::new();
    let head_end = loop {
        if let Some(pos) = find_head_end(&buf) {
            break pos;
        }
        if buf.len() > MAX_HEAD {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "request head too large",
            ));
        }
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => return Ok(None),
            Ok(n) => buf.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock
                        | std::io::ErrorKind::TimedOut
                        | std::io::ErrorKind::ConnectionReset
                ) =>
            {
                return Ok(None);
            }
            Err(e) => return Err(e),
        }
    };

    let head = String::from_utf8_lossy(&buf[..head_end]).to_string();
    let mut lines = head.split("\r\n");
    let first = lines.next().unwrap_or_default();
    let mut parts = first.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let path = parts.next().unwrap_or_default().to_string();
    let mut content_length = 0usize;
    for line in lines {
        if let Some((name, value)) = line.split_once(':')
            && name.eq_ignore_ascii_case("content-length")
        {
            content_length = value.trim().parse().unwrap_or(0).min(MAX_BODY);
        }
    }

    let mut body = buf[head_end..].to_vec();
    while body.len() < content_length {
        let mut chunk = [0u8; 4096];
        match stream.read(&mut chunk) {
            Ok(0) => break,
            Ok(n) => body.extend_from_slice(&chunk[..n]),
            Err(e)
                if matches!(
                    e.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ) =>
            {
                break;
            }
            Err(e) => return Err(e),
        }
    }
    body.truncate(content_length);
    Ok(Some(Request { method, path, body }))
}

/// Offset just past the `\r\n\r\n` terminator, if present.
fn find_head_end(buf: &[u8]) -> Option<usize> {
    buf.windows(4).position(|w| w == b"\r\n\r\n").map(|p| p + 4)
}

fn respond(stream: &mut TcpStream, status: u16, content_type: &str, body: &str) {
    let reason = match status {
        200 => "OK",
        400 => "Bad Request",
        404 => "Not Found",
        410 => "Gone",
        _ => "Error",
    };
    let head = format!(
        "HTTP/1.1 {status} {reason}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    let _ = stream.write_all(head.as_bytes());
    let _ = stream.write_all(body.as_bytes());
    let _ = stream.flush();
}

fn serve(listener: TcpListener, mobile: String, token: mpsc::Sender<String>) {
    for incoming in listener.incoming() {
        let Ok(mut stream) = incoming else { continue };
        let mobile = mobile.clone();
        let token = token.clone();
        std::thread::spawn(move || {
            let _ = stream.set_read_timeout(Some(Duration::from_secs(5)));
            let _ = stream.set_write_timeout(Some(Duration::from_secs(5)));
            match read_request(&mut stream) {
                Ok(Some(req)) => {
                    let (status, content_type, body) = route(&req, &mobile, &token);
                    respond(&mut stream, status, content_type, &body);
                }
                Ok(None) => respond(&mut stream, 400, "text/plain; charset=utf-8", "bad request"),
                Err(_) => respond(&mut stream, 400, "text/plain; charset=utf-8", "bad request"),
            }
        });
    }
}

/// Open a URL in the user's browser. Returns `false` when no opener could be
/// spawned, in which case the caller prints the URL for manual opening.
pub fn open_browser(url: &str) -> bool {
    let candidates: Vec<(&str, Vec<String>)> = if cfg!(target_os = "windows") {
        vec![(
            "cmd",
            vec![
                "/c".to_string(),
                "start".to_string(),
                String::new(),
                url.to_string(),
            ],
        )]
    } else if cfg!(target_os = "macos") {
        vec![("open", vec![url.to_string()])]
    } else {
        vec![
            ("xdg-open", vec![url.to_string()]),
            ("gio", vec!["open".to_string(), url.to_string()]),
            ("sensible-browser", vec![url.to_string()]),
        ]
    };
    for (cmd, args) in candidates {
        if Command::new(cmd).args(&args).spawn().is_ok() {
            return true;
        }
    }
    false
}

/// Serve the captcha page on a loopback port, open it in the browser and
/// wait for the solved token.
///
/// `mobile` is only used for display on the page (masked before it gets
/// there). With `open` set the default browser is launched; otherwise the URL
/// is printed for the user to open by hand.
pub fn obtain_token(mobile: &str, open: bool, timeout: Duration) -> Result<String> {
    let listener = TcpListener::bind(("127.0.0.1", 0)).context("bind loopback captcha server")?;
    let port = listener.local_addr().context("local address")?.port();
    let url = format!("http://127.0.0.1:{port}/");
    let (tx, rx) = mpsc::channel::<String>();
    let mobile = mobile.to_string();
    std::thread::spawn(move || serve(listener, mobile, tx));

    eprintln!("Solve the security check in your browser: {url}");
    if open && open_browser(&url) {
        eprintln!(
            "Waiting for the captcha to be solved (timeout {}s)...",
            timeout.as_secs()
        );
    } else {
        eprintln!(
            "Open the URL above in a browser to continue (timeout {}s)...",
            timeout.as_secs()
        );
    }
    match rx.recv_timeout(timeout) {
        Ok(token) => Ok(token),
        Err(_) => bail!(
            "timed out after {}s waiting for the captcha; re-run the command and solve the \
             slider in the opened browser",
            timeout.as_secs()
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn request(method: &str, path: &str, body: &[u8]) -> Request {
        Request {
            method: method.to_string(),
            path: path.to_string(),
            body: body.to_vec(),
        }
    }

    #[test]
    fn page_carries_the_client_scene() {
        let (tx, rx) = mpsc::channel();
        let (status, content_type, body) = route(&request("GET", "/", b""), "123****8901", &tx);
        assert_eq!(status, 200);
        assert!(content_type.starts_with("text/html"));
        assert!(body.contains("1sdsal45"), "Aliyun scene id missing");
        assert!(body.contains("16ck57"), "Aliyun prefix missing");
        assert!(body.contains("123****8901"));
        assert!(!body.contains("{{MOBILE}}"));
        assert!(rx.try_recv().is_err(), "GET must not produce a token");
    }

    #[test]
    fn token_post_is_forwarded() {
        let (tx, rx) = mpsc::channel();
        let body = br#"{"captchaVerifyParam":"TOKEN123","raw":"x"}"#;
        let (status, _, _) = route(&request("POST", "/token", body), "123****8901", &tx);
        assert_eq!(status, 200);
        assert_eq!(rx.try_recv().unwrap(), "TOKEN123");
    }

    #[test]
    fn token_post_without_param_falls_back_to_raw() {
        let (tx, rx) = mpsc::channel();
        let body = br#"{"raw":"RAWTOKEN"}"#;
        let (status, _, _) = route(&request("POST", "/token", body), "123****8901", &tx);
        assert_eq!(status, 200);
        assert_eq!(rx.try_recv().unwrap(), "RAWTOKEN");
    }

    #[test]
    fn empty_token_is_rejected() {
        let (tx, rx) = mpsc::channel();
        let (status, _, _) = route(&request("POST", "/token", b"{}"), "123****8901", &tx);
        assert_eq!(status, 400);
        assert!(rx.try_recv().is_err());
    }

    #[test]
    fn unknown_paths_404() {
        let (tx, _) = mpsc::channel();
        let (status, _, _) = route(&request("GET", "/favicon.ico", b""), "x", &tx);
        assert_eq!(status, 404);
    }

    #[test]
    fn decodes_a_complete_request() {
        let raw = b"POST /token HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 7\r\n\r\n{\"a\":1}";
        let listener = TcpListener::bind(("127.0.0.1", 0)).unwrap();
        let addr = listener.local_addr().unwrap();
        let handle = std::thread::spawn(move || {
            let (mut stream, _) = listener.accept().unwrap();
            let _ = stream.set_read_timeout(Some(Duration::from_secs(2)));
            read_request(&mut stream).unwrap()
        });
        let mut client = TcpStream::connect(addr).unwrap();
        client.write_all(raw).unwrap();
        let req = handle.join().unwrap().expect("request decoded");
        assert_eq!(req.method, "POST");
        assert_eq!(req.path, "/token");
        assert_eq!(req.body, b"{\"a\":1}");
    }

    #[test]
    fn escape_neutralises_markup() {
        assert_eq!(escape("<b>\"&"), "&lt;b&gt;&quot;&amp;");
    }
}
