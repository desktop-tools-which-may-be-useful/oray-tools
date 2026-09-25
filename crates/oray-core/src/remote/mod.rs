use crate::Error;
use crate::trace::{self, RawResult, RequestLog, Traced, TracedError, TracedResult};
use reqwest::blocking::Client;
use serde::{Deserialize, Serialize};

/// Thin wrapper over the Oray remote-device HTTP endpoints on
/// `api-std.sunlogin.oray.com`. Stateless: callers own the access token and
/// device identity.
///
/// The `reqwest` client is injected so callers control timeouts, proxies,
/// connection reuse and test doubles; `clone()` is cheap (shared connection
/// pool), so reuse a single client across all API calls.
pub struct RemoteApi {
    client: Client,
    api_base: String,
}

/// Remote device as returned by `GET /remotes`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct Remote {
    pub remote_id: u64,
    #[serde(default)]
    pub mac: String,
    #[serde(default)]
    pub owner_id: u64,
    #[serde(default)]
    pub create_time: u64,
    #[serde(default)]
    pub statuscode: i64,
    #[serde(default)]
    pub client: String,
    /// Rich device info block (`info`).
    #[serde(default)]
    pub info: RemoteInfo,
    /// Online state block; absent when the remote is not reachable.
    #[serde(default)]
    pub state: Option<RemoteState>,
    #[serde(default)]
    pub limit_control: String,
}

/// Extended hardware/OS info (`info`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RemoteInfo {
    #[serde(default)]
    pub remote_id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub pc_name: String,
    #[serde(default)]
    pub cpu: String,
    #[serde(default)]
    pub memory: String,
    #[serde(default)]
    pub os_name: String,
    /// Memo/备注; may be empty or absent on some endpoints.
    #[serde(default)]
    pub description: String,
    #[serde(default)]
    pub disk_drive: String,
    #[serde(default)]
    pub video_controller: String,
    #[serde(default)]
    pub network_adapter: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub os: String,
    #[serde(default)]
    pub screen_size: String,
}

/// Runtime state block for a remote (`state`).
#[derive(Debug, Clone, Default, Deserialize, Serialize)]
pub struct RemoteState {
    #[serde(default)]
    pub addr: String,
    #[serde(default)]
    pub version: String,
    #[serde(default)]
    pub ip: String,
    #[serde(default)]
    pub login_time: u64,
    #[serde(default)]
    pub fastcode: String,
    #[serde(default)]
    pub owner_id: u64,
}

impl RemoteState {
    /// Whether the remote is reachable right now. The cloud returns an empty
    /// `state: {}` object when the remote is offline, so a present-but-empty
    /// state does not count as online.
    pub fn is_online(&self) -> bool {
        !(self.addr.is_empty()
            && self.version.is_empty()
            && self.ip.is_empty()
            && self.login_time == 0
            && self.fastcode.is_empty()
            && self.owner_id == 0)
    }
}

/// The page size this client asks `GET /remotes` for, and the default of the
/// CLI's `remote list --limit`.
///
/// It mirrors the endpoint's own `page_size_limit` (observed as 10 000 in
/// every response), so a bigger request can only be clamped server-side.
/// [`RemoteApi::find`] and `remote list` both derive their default from this
/// constant, which is why neither carries a literal `10_000` any more.
pub const REMOTE_PAGE_LIMIT: u64 = 10_000;

#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct RemotesResponse {
    /// The page itself. `#[serde(default)]` because the endpoint **omits the
    /// key entirely** when the page is empty — `offset >= total` answers
    /// `{"total":1,"page_size_limit":10000}` — and a required field would
    /// surface that as a parse error instead of an empty list.
    #[serde(default)]
    pub remotes: Vec<Remote>,
    #[serde(default)]
    pub total: Option<u64>,
    #[serde(default)]
    pub page_size_limit: Option<u64>,
}

/// Top-level wrapper for `GET /console/remotes/{id}`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConsoleResponse {
    pub remote: ConsoleRemote,
}

/// Remote detail as returned by `GET /console/remotes/{id}`. Only fields that
/// are safe and useful to display are modeled; sensitive license material is
/// deliberately excluded.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct ConsoleRemote {
    pub remote_id: u64,
    #[serde(default)]
    pub mac: String,
    #[serde(default)]
    pub owner_id: u64,
    #[serde(default)]
    pub create_time: u64,
    #[serde(default)]
    pub statuscode: i64,
    #[serde(default)]
    pub client: String,
    #[serde(default)]
    pub info: RemoteInfo,
    #[serde(default)]
    pub state: Option<RemoteState>,
    #[serde(default)]
    pub limit_control: String,
}

/// Acknowledgement returned by `PATCH /remotes/{id}/info`.
#[derive(Debug, Clone, Deserialize, Serialize)]
pub struct UpdateResponse {
    pub remote_id: u64,
    #[serde(default)]
    pub name: String,
    #[serde(default)]
    pub description: String,
}

/// Payload for `PATCH /remotes/{id}/info`. The Oray app always sends both
/// fields together with `update_type = 1`.
#[derive(Debug, serde::Serialize)]
pub struct RemoteUpdate<'a> {
    pub name: &'a str,
    pub description: &'a str,
    pub update_type: u8,
}

impl<'a> RemoteUpdate<'a> {
    pub fn new(name: &'a str, description: &'a str) -> Self {
        Self {
            name,
            description,
            update_type: 1,
        }
    }
}

/// `GET {api_base}/remotes?offset={offset}&limit={limit}&version=v2&new_server=1`
/// — the query shape the Oray app sends, kept in one place so the URL is
/// unit-testable and `offset`/`limit` cannot drift between callers.
fn remotes_list_url(api_base: &str, offset: u64, limit: u64) -> String {
    format!("{api_base}/remotes?offset={offset}&limit={limit}&version=v2&new_server=1")
}

impl RemoteApi {
    /// Wrap an injected client for the API base
    /// (e.g. `https://api-std.sunlogin.oray.com`).
    pub fn new(client: Client, api_base: &str) -> Self {
        Self {
            client,
            api_base: api_base.trim_end_matches('/').to_string(),
        }
    }

    /// Execute one request with the shared api-std headers, requiring `2xx`
    /// (see [`trace::expect_2xx`]) and returning the trace plus the body.
    /// `content_type` is appended when the request carries a body.
    fn send(
        &self,
        token: &str,
        what: &'static str,
        method: &'static str,
        url: &str,
        body: Option<String>,
        content_type: Option<&'static str>,
    ) -> RawResult<(Vec<RequestLog>, String)> {
        let mut headers = trace::api_headers(token);
        if let Some(content_type) = content_type {
            headers.push(("Content-Type".into(), content_type.into()));
        }
        let ex = trace::execute(
            &self.client,
            trace::Request {
                method,
                url: url.to_string(),
                headers,
                body,
            },
        )?;
        trace::expect_2xx(ex, what)
    }

    /// List remote devices (same query shape the Oray app uses).
    ///
    /// Both halves of the query are caller-controlled: `offset` selects the
    /// page (the endpoint honours it — an `offset` past `total` answers an
    /// empty page) and `limit` is its size. Ask for at most
    /// [`REMOTE_PAGE_LIMIT`]; a bigger value is not an error, but the server
    /// answers with at most its advertised `page_size_limit` items.
    pub fn list(&self, token: &str, offset: u64, limit: u64) -> TracedResult<RemotesResponse> {
        let url = remotes_list_url(&self.api_base, offset, limit);
        let (calls, text) = self.send(token, "list remotes", "GET", &url, None, None)?;
        trace::finish(
            calls,
            serde_json::from_str(&text).map_err(|e| Error::bad_body(text, e)),
        )
    }

    /// Look up a single remote by id from the live list.
    ///
    /// This is a **list + find**, not a lookup by id: the endpoint set has no
    /// "get remote by id" call, so the whole account list is fetched first
    /// (one page of [`REMOTE_PAGE_LIMIT`] remotes, `list(token, 0,
    /// REMOTE_PAGE_LIMIT)`) and scanned in memory. A miss therefore means
    /// "not among the first 10 000 remotes listed", which is what the error
    /// message reports.
    pub fn find(&self, token: &str, remote_id: u64) -> TracedResult<Remote> {
        let all = self.list(token, 0, REMOTE_PAGE_LIMIT)?;
        let calls = all.calls;
        let remotes = all.data.remotes;
        let scanned = remotes.len();
        match remotes.into_iter().find(|r| r.remote_id == remote_id) {
            Some(remote) => Ok(Traced {
                data: remote,
                calls,
            }),
            None => Err(TracedError {
                error: Error::Api(format!(
                    "remote {remote_id} not found among {scanned} remotes listed"
                )),
                calls,
            }),
        }
    }

    /// Fetch extended detail for a remote from the console endpoint.
    pub fn detail(&self, token: &str, remote_id: u64) -> TracedResult<ConsoleRemote> {
        let url = format!(
            "{}/console/remotes/{remote_id}?with_powerplan=true&with_extend=true&new_server=1",
            self.api_base
        );
        let (calls, text) = self.send(token, "get remote detail", "GET", &url, None, None)?;
        match serde_json::from_str::<ConsoleResponse>(&text).map_err(|e| Error::bad_body(text, e)) {
            Ok(parsed) => Ok(Traced {
                data: parsed.remote,
                calls,
            }),
            Err(error) => Err(TracedError { error, calls }),
        }
    }

    /// Update the device name and/or memo (description) of a remote.
    pub fn update(
        &self,
        token: &str,
        remote_id: u64,
        update: &RemoteUpdate<'_>,
    ) -> TracedResult<UpdateResponse> {
        let url = format!("{}/remotes/{remote_id}/info", self.api_base);
        let body = serde_json::to_string(update)
            .map_err(|e| Error::Api(format!("serialize update payload: {e}")))?;
        let (calls, text) = self.send(
            token,
            "update remote",
            "PATCH",
            &url,
            Some(body),
            Some("application/json"),
        )?;
        trace::finish(
            calls,
            serde_json::from_str(&text).map_err(|e| Error::bad_body(text, e)),
        )
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn parse_remotes_v2() {
        let json = r#"{"remotes":[{"remote_id":800001,"mac":"aa:bb:cc:dd:ee:02","owner_id":600001,"create_time":1784993926,"statuscode":379547,"client":"SLRC_WINDOWS","info":{"remote_id":800001,"name":"Demo-Desktop","pc_name":"Demo-Desktop","cpu":"Example CPU @ 3.0GHz","memory":"16384MB","os_name":"ExampleOS 10 Pro","description":"demo memo","disk_drive":"SAMPLE","version":"1.2.3","os":"ExampleOS 10 Pro","screen_size":"2560:1600"},"state":{"addr":"https://example.invalid","ip":"198.51.100.7","login_time":1788438331,"fastcode":"123456"},"limit_control":"0"}],"total":1,"page_size_limit":10000}"#;
        let parsed: RemotesResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.total, Some(1));
        let r = &parsed.remotes[0];
        assert_eq!(r.remote_id, 800001);
        assert_eq!(r.info.name, "Demo-Desktop");
        assert_eq!(r.info.memory, "16384MB");
        assert_eq!(r.info.description, "demo memo");
        assert!(r.state.is_some());
    }

    #[test]
    fn parse_update_response() {
        let json = r#"{"remote_id":800001,"name":"Renamed-Desktop","description":"note","configmodifiedtime":1788509492}"#;
        let parsed: UpdateResponse = serde_json::from_str(json).unwrap();
        assert_eq!(parsed.name, "Renamed-Desktop");
        assert_eq!(parsed.description, "note");
    }

    #[test]
    fn empty_state_is_offline() {
        // `"state":{}` is what the cloud returns while a remote is offline.
        let json = r#"{"remotes":[{"remote_id":800001,"mac":"aa:bb:cc:dd:ee:02","owner_id":600001,"create_time":1784993926,"statuscode":379547,"client":"SLRC_WINDOWS","info":{"remote_id":800001,"name":"Demo-Desktop","os_name":"ExampleOS"},"state":{},"limit_control":"0"}],"total":1,"page_size_limit":10000}"#;
        let parsed: RemotesResponse = serde_json::from_str(json).unwrap();
        let s = parsed.remotes[0].state.as_ref().unwrap();
        assert!(!s.is_online());
    }

    #[test]
    fn populated_state_is_online() {
        let json = r#"{"remotes":[{"remote_id":800001,"mac":"aa:bb:cc:dd:ee:02","owner_id":600001,"create_time":1784993926,"statuscode":379547,"client":"SLRC_WINDOWS","info":{"remote_id":800001,"name":"Demo-Desktop","os_name":"ExampleOS"},"state":{"ip":"198.51.100.7"},"limit_control":"0"}],"total":1,"page_size_limit":10000}"#;
        let parsed: RemotesResponse = serde_json::from_str(json).unwrap();
        let s = parsed.remotes[0].state.as_ref().unwrap();
        assert!(s.is_online());
    }

    #[test]
    fn missing_state_is_offline() {
        let json = r#"{"remotes":[{"remote_id":800001,"mac":"aa:bb:cc:dd:ee:02","owner_id":600001,"create_time":1784993926,"statuscode":379547,"client":"SLRC_WINDOWS","info":{"remote_id":800001,"name":"Demo-Desktop","os_name":"ExampleOS"},"limit_control":"0"}],"total":1,"page_size_limit":10000}"#;
        let parsed: RemotesResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.remotes[0].state.is_none());
    }

    #[test]
    fn update_payload_serializes() {
        let u = RemoteUpdate::new("New-Name", "New-Memo");
        let v = serde_json::to_value(u).unwrap();
        assert_eq!(v["name"], "New-Name");
        assert_eq!(v["description"], "New-Memo");
        assert_eq!(v["update_type"], 1);
    }

    /// `offset >= total` answers a body **without** the `remotes` key —
    /// captured live as `GET /remotes?offset=1&limit=1` on a one-remote
    /// account returning `{"total":1,"page_size_limit":10000}`. It has to
    /// parse as an empty page: before `#[serde(default)]` this exact body
    /// failed with `missing field `remotes``, which would have turned
    /// `--offset` past the end into a bogus parse error.
    #[test]
    fn parse_remotes_empty_page_without_remotes_key() {
        let json = r#"{"total":1,"page_size_limit":10000}"#;
        let parsed: RemotesResponse = serde_json::from_str(json).unwrap();
        assert!(parsed.remotes.is_empty());
        assert_eq!(parsed.total, Some(1));
        assert_eq!(parsed.page_size_limit, Some(10_000));
    }

    /// The query shape is shared by `list`, `find` and the CLI default, and
    /// stays byte-identical to what the app sends.
    #[test]
    fn list_url_carries_offset_and_limit() {
        assert_eq!(REMOTE_PAGE_LIMIT, 10_000);
        assert_eq!(
            remotes_list_url("https://api-std.sunlogin.oray.com", 0, REMOTE_PAGE_LIMIT),
            "https://api-std.sunlogin.oray.com/remotes?offset=0&limit=10000&version=v2&new_server=1"
        );
        assert_eq!(
            remotes_list_url("https://api-std.sunlogin.oray.com", 42, 7),
            "https://api-std.sunlogin.oray.com/remotes?offset=42&limit=7&version=v2&new_server=1"
        );
    }
}
