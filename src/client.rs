use crate::config::ResolvedProfile;
use crate::error::{Error, Result};
use crate::observability::{log_body, log_request, log_response, log_response_headers};
use reqwest::blocking::{Client as ReqwestClient, Response};
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderValue, SET_COOKIE, USER_AGENT};
use reqwest::{Method, StatusCode};
use serde_json::Value;
use std::io::{ErrorKind, Read, Write};
use std::time::{Duration, Instant};

/// How a `Client` attaches credentials to each request. Resolved at build time
/// so `send()` stays a single, branchless-per-request decision.
#[derive(Clone)]
pub enum Auth {
    Basic {
        username: String,
        password: String,
    },
    Bearer {
        token: String,
    },
    /// No credentials attached — used for the OAuth token endpoint, which
    /// authenticates via form parameters (or a Basic client_id:secret) instead.
    None,
}

pub struct Client {
    http: ReqwestClient,
    base_url: String,
    auth: Auth,
    /// Caller-supplied request headers (`sn raw --header`). Applied **last** in
    /// [`Client::send`] so they beat both the client defaults and the
    /// per-request `Content-Type`; empty for every command but `sn raw`.
    extra_headers: HeaderMap,
}

impl std::fmt::Debug for Client {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let scheme = match &self.auth {
            Auth::Basic { username, .. } => format!("basic({username})"),
            Auth::Bearer { .. } => "bearer".to_string(),
            Auth::None => "none".to_string(),
        };
        f.debug_struct("Client")
            .field("base_url", &self.base_url)
            .field("auth", &scheme)
            .finish_non_exhaustive()
    }
}

pub struct ClientBuilder {
    timeout: Duration,
    user_agent: String,
    proxy: Option<String>,
    no_proxy: Option<String>,
    insecure: bool,
    ca_cert: Option<String>,
    proxy_ca_cert: Option<String>,
    proxy_username: Option<String>,
    proxy_password: Option<String>,
    auth: Option<Auth>,
    extra_headers: HeaderMap,
}

impl Default for ClientBuilder {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            user_agent: format!("sn/{}", env!("CARGO_PKG_VERSION")),
            proxy: None,
            no_proxy: None,
            insecure: false,
            ca_cert: None,
            proxy_ca_cert: None,
            proxy_username: None,
            proxy_password: None,
            auth: None,
            extra_headers: HeaderMap::new(),
        }
    }
}

impl ClientBuilder {
    pub fn timeout(mut self, d: Duration) -> Self {
        self.timeout = d;
        self
    }

    pub fn proxy(mut self, url: Option<String>) -> Self {
        self.proxy = url;
        self
    }

    pub fn no_proxy(mut self, hosts: Option<String>) -> Self {
        self.no_proxy = hosts;
        self
    }

    pub fn insecure(mut self, yes: bool) -> Self {
        self.insecure = yes;
        self
    }

    pub fn ca_cert(mut self, path: Option<String>) -> Self {
        self.ca_cert = path;
        self
    }

    pub fn proxy_ca_cert(mut self, path: Option<String>) -> Self {
        self.proxy_ca_cert = path;
        self
    }

    pub fn proxy_auth(mut self, username: Option<String>, password: Option<String>) -> Self {
        self.proxy_username = username;
        self.proxy_password = password;
        self
    }

    /// Override how requests authenticate. When unset, `build()` falls back to
    /// HTTP Basic using the profile's username/password (backward compatible).
    pub fn auth(mut self, auth: Auth) -> Self {
        self.auth = Some(auth);
        self
    }

    /// Attach caller-supplied request headers, applied last in
    /// [`Client::send`].
    ///
    /// Deliberately *not* merged into `default_headers`: reqwest gives
    /// request-level headers priority over client defaults, so a default-header
    /// merge would be silently overridden — no error, the old value just goes
    /// out — for exactly the two headers callers reach for: `Content-Type` (set
    /// per request in [`Client::request`]) and `Authorization` (set per request
    /// in `send`). A name present here replaces whatever the client would
    /// otherwise send under it; repeats of one name in the map are preserved as
    /// multiple values.
    pub fn extra_headers(mut self, headers: HeaderMap) -> Self {
        self.extra_headers = headers;
        self
    }

    pub fn build(self, profile: &ResolvedProfile) -> Result<Client> {
        let mut headers = HeaderMap::new();
        headers.insert(ACCEPT, HeaderValue::from_static("application/json"));
        headers.insert(USER_AGENT, HeaderValue::from_str(&self.user_agent).unwrap());

        let mut builder = ReqwestClient::builder()
            .timeout(self.timeout)
            .default_headers(headers);

        if let Some(ref proxy_url) = self.proxy {
            let valid_scheme = proxy_url.starts_with("http://")
                || proxy_url.starts_with("https://")
                || proxy_url.starts_with("socks5://")
                || proxy_url.starts_with("socks5h://");
            if !valid_scheme {
                return Err(Error::Config(format!(
                    "invalid proxy URL '{proxy_url}': must start with http://, https://, or socks5://"
                )));
            }
            let mut proxy = reqwest::Proxy::all(proxy_url)
                .map_err(|e| Error::Config(format!("invalid proxy URL '{proxy_url}': {e}")))?;
            if let (Some(u), Some(p)) = (&self.proxy_username, &self.proxy_password) {
                proxy = proxy.basic_auth(u, p);
            }
            if let Some(ref hosts) = self.no_proxy {
                proxy = proxy.no_proxy(reqwest::NoProxy::from_string(hosts));
            }
            builder = builder.proxy(proxy);
        }

        if self.insecure {
            builder = builder.danger_accept_invalid_certs(true);
        }

        if let Some(ref path) = self.ca_cert {
            let pem = std::fs::read(path)
                .map_err(|e| Error::Config(format!("read CA cert '{}': {e}", path)))?;
            let cert = reqwest::Certificate::from_pem(&pem)
                .map_err(|e| Error::Config(format!("parse CA cert '{}': {e}", path)))?;
            builder = builder.add_root_certificate(cert);
        }

        if let Some(ref path) = self.proxy_ca_cert {
            let pem = std::fs::read(path)
                .map_err(|e| Error::Config(format!("read proxy CA cert '{}': {e}", path)))?;
            let cert = reqwest::Certificate::from_pem(&pem)
                .map_err(|e| Error::Config(format!("parse proxy CA cert '{}': {e}", path)))?;
            builder = builder.add_root_certificate(cert);
        }

        let http = builder
            .build()
            .map_err(|e| Error::Transport(format!("build client: {e}")))?;

        let base_url = normalize_base_url(&profile.instance);
        let auth = self.auth.unwrap_or_else(|| Auth::Basic {
            username: profile.username.clone(),
            password: profile.password.clone(),
        });
        Ok(Client {
            http,
            base_url,
            auth,
            extra_headers: self.extra_headers,
        })
    }
}

/// Turn a stored `instance` into an absolute base URL. Profiles persist the host
/// without a scheme (`acme.service-now.com`), so anything that builds a URL from
/// one — not just this client — has to come through here or it emits a
/// scheme-less string that no browser or HTTP stack will accept.
pub(crate) fn normalize_base_url(instance: &str) -> String {
    if instance.starts_with("http://") || instance.starts_with("https://") {
        instance.trim_end_matches('/').to_string()
    } else {
        format!("https://{}", instance.trim_end_matches('/'))
    }
}

fn parse_response(resp: Response) -> Result<Value> {
    parse_response_inner(resp, false)
}

/// Like [`parse_response`] but redacts OAuth secrets from the `-ddd` body log.
/// Used only by the token endpoint (`post_form`), whose responses carry
/// `access_token`/`refresh_token`/`id_token` in cleartext. The returned `Value`
/// is still fully parsed — only what hits stderr is masked.
fn parse_response_redacted(resp: Response) -> Result<Value> {
    parse_response_inner(resp, true)
}

fn parse_response_inner(resp: Response, redact_secrets: bool) -> Result<Value> {
    let status = resp.status();
    let tx = transaction_id(&resp);
    log_response_headers(resp.headers());
    let text = resp
        .text()
        .map_err(|e| Error::Transport(format!("read body: {e}")))?;
    if redact_secrets {
        log_body("<", &redact_token_json(&text));
    } else {
        log_body("<", &text);
    }
    if status.is_success() {
        if text.is_empty() {
            return Ok(Value::Null);
        }
        return serde_json::from_str(&text)
            .map_err(|e| Error::Transport(format!("parse response: {e}")));
    }
    Err(from_http_text(status, tx, &text))
}

/// OAuth token-response keys whose values are secrets and must never be logged.
const SENSITIVE_TOKEN_KEYS: [&str; 3] = ["access_token", "refresh_token", "id_token"];

/// Redact secret values from an OAuth token-endpoint JSON body so it is safe to
/// print at `-ddd`. Replaces the values of `access_token`/`refresh_token`/
/// `id_token` with `"****"` while leaving non-secret keys (`token_type`,
/// `expires_in`, `scope`, `error`, `error_description`, …) readable for
/// debugging. Any body that is not the flat JSON object OAuth mandates collapses
/// to a placeholder — a malformed/HTML response can't smuggle a token to stderr.
fn redact_token_json(body: &str) -> String {
    match serde_json::from_str::<Value>(body) {
        Ok(Value::Object(mut map)) => {
            for key in SENSITIVE_TOKEN_KEYS {
                if let Some(v) = map.get_mut(key) {
                    *v = Value::String("****".to_string());
                }
            }
            Value::Object(map).to_string()
        }
        _ => "<token response: redacted>".to_string(),
    }
}

impl Client {
    pub fn builder() -> ClientBuilder {
        ClientBuilder::default()
    }

    /// Normalized instance base URL (scheme + host, no trailing slash).
    pub fn base_url(&self) -> &str {
        &self.base_url
    }

    fn url(&self, path: &str) -> String {
        format!("{}{}", self.base_url, path)
    }

    fn send(
        &self,
        mut req: reqwest::blocking::RequestBuilder,
        method_name: &str,
        url: &str,
    ) -> Result<Response> {
        req = match &self.auth {
            Auth::Basic { username, password } => req.basic_auth(username, Some(password)),
            Auth::Bearer { token } => req.bearer_auth(token),
            Auth::None => req,
        };
        // Caller headers go on last, after every request-level header this
        // client sets: the `Content-Type` that `request()` puts on bodied calls
        // and the auth header just above. `RequestBuilder::headers` *replaces*
        // the values of the names it carries (unlike `.header()`, which
        // appends), so a caller's `Content-Type` supplants ours instead of
        // arriving as a second value. Client defaults (`Accept`, `User-Agent`)
        // are merged at execute time into names the request does not already
        // carry, so they lose to a caller header without anything done here.
        if !self.extra_headers.is_empty() {
            req = req.headers(self.extra_headers.clone());
        }
        log_request(method_name, url);
        let start = Instant::now();
        let resp = req.send().map_err(|e| Error::Transport(format!("{e}")))?;
        log_response(resp.status().as_u16(), start.elapsed().as_millis());
        Ok(resp)
    }

    /// Single JSON request/response pipeline used by every verb.
    fn request(
        &self,
        method: Method,
        path: &str,
        query: &[(String, String)],
        body: Option<&Value>,
    ) -> Result<Value> {
        let url = self.url(path);
        let mut req = self.http.request(method.clone(), &url).query(query);
        if let Some(b) = body {
            log_body(">", &b.to_string());
            req = req.header(CONTENT_TYPE, "application/json").json(b);
        }
        parse_response(self.send(req, method.as_str(), &url)?)
    }

    pub fn get(&self, path: &str, query: &[(String, String)]) -> Result<Value> {
        self.request(Method::GET, path, query, None)
    }

    pub fn post(&self, path: &str, query: &[(String, String)], body: &Value) -> Result<Value> {
        self.request(Method::POST, path, query, Some(body))
    }

    pub fn put(&self, path: &str, query: &[(String, String)], body: &Value) -> Result<Value> {
        self.request(Method::PUT, path, query, Some(body))
    }

    pub fn patch(&self, path: &str, query: &[(String, String)], body: &Value) -> Result<Value> {
        self.request(Method::PATCH, path, query, Some(body))
    }

    /// DELETE that expects no response body (returns unit on success).
    pub fn delete(&self, path: &str, query: &[(String, String)]) -> Result<()> {
        self.request(Method::DELETE, path, query, None).map(|_| ())
    }

    /// DELETE that expects a JSON response body.
    pub fn delete_json(&self, path: &str, query: &[(String, String)]) -> Result<Value> {
        self.request(Method::DELETE, path, query, None)
    }

    pub fn upload_file(
        &self,
        path: &str,
        query: &[(String, String)],
        body: Vec<u8>,
        content_type: &str,
    ) -> Result<Value> {
        let url = self.url(path);
        log_body(">", &format!("<{} bytes, {}>", body.len(), content_type));
        let req = self
            .http
            .request(Method::POST, &url)
            .query(query)
            .header(CONTENT_TYPE, content_type)
            .body(body);
        parse_response(self.send(req, "POST", &url)?)
    }

    /// POST `application/x-www-form-urlencoded` and parse the JSON response
    /// without unwrapping a `result` envelope. Used for the OAuth token
    /// endpoint, whose responses are flat (`access_token`, `refresh_token`, …).
    pub fn post_form(&self, path: &str, form: &[(String, String)]) -> Result<Value> {
        let url = self.url(path);
        // Avoid logging secrets (codes, tokens) — record only the field names.
        let names: Vec<&str> = form.iter().map(|(k, _)| k.as_str()).collect();
        log_body(">", &format!("<form: {}>", names.join(", ")));
        let req = self.http.request(Method::POST, &url).form(form);
        // Redact the response body: it carries access/refresh/id tokens that the
        // plain `parse_response` would print verbatim at -ddd.
        parse_response_redacted(self.send(req, "POST", &url)?)
    }

    /// Mint a ServiceNow session and return the `Cookie:` header value that
    /// authenticates an AMB websocket upgrade.
    ///
    /// The AMB endpoint ignores `Authorization` entirely and authenticates by
    /// session cookie alone, so a websocket cannot present this client's
    /// credentials the way every other call does. One ordinary authenticated
    /// request is enough to make the instance mint a session — and because it
    /// goes out through `send()`, it authenticates however the profile does.
    /// Basic and OAuth bearer yield the same cookies, and AMB accepts either, so
    /// SSO profiles need no separate path.
    ///
    /// Uses the same `sys_user` read that `sn ping` uses as its auth check, so a
    /// profile that can ping can watch.
    pub fn session_cookies(&self) -> Result<String> {
        let url = self.url("/api/now/table/sys_user");
        let req = self
            .http
            .request(Method::GET, &url)
            .query(&[("sysparm_limit", "1"), ("sysparm_fields", "sys_id")]);
        let resp = self.send(req, "GET", &url)?;
        let status = resp.status();
        let tx = transaction_id(&resp);
        log_response_headers(resp.headers());
        if !status.is_success() {
            let text = resp
                .text()
                .map_err(|e| Error::Transport(format!("read body: {e}")))?;
            return Err(from_http_text(status, tx, &text));
        }

        let jar = collect_session_cookies(resp.headers());
        if !jar.iter().any(|(name, _)| name == SESSION_COOKIE) {
            return Err(Error::Auth {
                status: status.as_u16(),
                message: format!(
                    "instance returned no {SESSION_COOKIE} cookie; \
                     cannot authenticate the AMB websocket"
                ),
                transaction_id: tx,
            });
        }
        Ok(jar
            .iter()
            .map(|(n, v)| format!("{n}={v}"))
            .collect::<Vec<_>>()
            .join("; "))
    }

    /// Start a binary download: performs the request, resolves the status, and
    /// returns with the **body unread**.
    ///
    /// Returning a [`Download`] instead of `(Vec<u8>, Option<String>)` is what
    /// keeps peak memory independent of attachment size — the old shape had to
    /// materialize the whole file before the caller could write a byte of it, so
    /// an attachment larger than RAM was simply not downloadable.
    ///
    /// The *status* is still resolved eagerly, and that split matters: a 404 or
    /// a 401 must fail before the caller touches the filesystem, so a failed
    /// download never creates (and then has to clean up) a file at or beside the
    /// destination.
    pub fn download_file(&self, path: &str) -> Result<Download> {
        let url = self.url(path);
        let req = self.http.request(Method::GET, &url);
        let resp = self.send(req, "GET", &url)?;
        let status = resp.status();
        let tx = transaction_id(&resp);
        log_response_headers(resp.headers());
        if !status.is_success() {
            // Error bodies are small JSON/HTML; buffering one is not the hazard
            // the success path is, and `from_http_text` needs it whole.
            let text = resp
                .text()
                .map_err(|e| Error::Transport(format!("read body: {e}")))?;
            log_body("<", &text);
            return Err(from_http_text(status, tx, &text));
        }
        Ok(Download { resp, written: 0 })
    }
}

/// Bytes moved per read/write cycle by [`Download::copy_to`], and therefore the
/// entire memory footprint of a download regardless of file size.
pub const DOWNLOAD_BUFFER_BYTES: usize = 64 * 1024;

/// A started download: response headers resolved, body still on the wire.
///
/// Reading the body through [`Read`] rather than `Response::bytes()` also
/// changes what the client's `timeout` means *for this request*, and that is the
/// point. reqwest's blocking client keeps its timeout on the blocking side only
/// (the inner async client is built without one) and enforces it by wrapping
/// whichever future the caller is parked on. `bytes()` parks on the *whole
/// body*, so the 30s cap covers the entire transfer and a large-but-perfectly-
/// healthy download dies at the cap. `Read::read` parks on *one chunk*, so the
/// same 30s becomes a per-read idle timeout: slow-but-alive runs as long as it
/// needs, a stalled socket still dies 30s after the last byte arrived. The
/// header phase is unchanged (one bounded wait in reqwest's `execute_request`),
/// so connect/handshake is still capped and no non-download request's timeout
/// semantics move.
pub struct Download {
    resp: Response,
    written: u64,
}

/// Which side of a streaming copy failed. The distinction is not cosmetic: a
/// source failure is a transport error (exit 3) and leaves the destination
/// undefined, while a sink failure is local and belongs to the caller, which is
/// the only party that knows what the destination *is*.
#[derive(Debug)]
pub enum DownloadError {
    /// The body stopped arriving — network drop, stall, truncated stream.
    Source(Error),
    /// The destination refused the bytes — disk full, broken pipe, permissions.
    Sink(std::io::Error),
}

impl Download {
    /// Bytes handed to the sink so far. Meaningful *after* a failed
    /// [`copy_to`](Self::copy_to): a caller writing to an unrecallable sink
    /// (stdout) needs to say how much truncated output it already emitted.
    pub fn bytes_written(&self) -> u64 {
        self.written
    }

    /// Stream the body into `sink` through a [`DOWNLOAD_BUFFER_BYTES`] buffer,
    /// returning the total byte count.
    pub fn copy_to<W: Write>(&mut self, sink: &mut W) -> std::result::Result<u64, DownloadError> {
        let mut buf = vec![0u8; DOWNLOAD_BUFFER_BYTES];
        let result = stream_body(&mut self.resp, sink, &mut buf, &mut self.written);
        if result.is_ok() {
            log_body("<", &format!("<{} bytes streamed>", self.written));
        }
        result
    }
}

/// The copy loop, split out from [`Download::copy_to`] so it can be exercised
/// against an arbitrary reader: the property that matters (no allocation grows
/// with payload size) is invisible from the outside otherwise.
fn stream_body<R: Read, W: Write>(
    src: &mut R,
    sink: &mut W,
    buf: &mut [u8],
    written: &mut u64,
) -> std::result::Result<u64, DownloadError> {
    loop {
        let n = match src.read(buf) {
            Ok(0) => break,
            Ok(n) => n,
            // A signal interrupting the read is not a failed transfer.
            Err(e) if e.kind() == ErrorKind::Interrupted => continue,
            Err(e) => {
                return Err(DownloadError::Source(Error::Transport(format!(
                    "read body: {e}"
                ))));
            }
        };
        sink.write_all(&buf[..n]).map_err(DownloadError::Sink)?;
        *written += n as u64;
    }
    Ok(*written)
}

impl Client {
    /// Stream records from a paginated list endpoint, following Link: rel="next" headers.
    pub fn paginate(
        &self,
        initial_path: &str,
        initial_query: &[(String, String)],
        max_records: Option<u32>,
    ) -> Paginator<'_> {
        Paginator::new(
            self,
            initial_path.to_string(),
            initial_query.to_vec(),
            max_records,
        )
    }
}

pub struct Paginator<'a> {
    client: &'a Client,
    next_url: Option<String>,
    next_query: Vec<(String, String)>,
    buffer: std::collections::VecDeque<Value>,
    emitted: u32,
    cap: Option<u32>,
    finished: bool,
}

impl<'a> Paginator<'a> {
    fn new(
        client: &'a Client,
        path: String,
        query: Vec<(String, String)>,
        cap: Option<u32>,
    ) -> Self {
        Self {
            client,
            next_url: Some(format!("{}{path}", client.base_url)),
            next_query: query,
            buffer: std::collections::VecDeque::new(),
            emitted: 0,
            cap,
            finished: false,
        }
    }

    fn fetch_next_page(&mut self) -> Result<()> {
        let Some(url) = self.next_url.take() else {
            self.finished = true;
            return Ok(());
        };
        let req = self
            .client
            .http
            .request(Method::GET, &url)
            .query(&self.next_query);
        let resp = self.client.send(req, "GET", &url)?;
        let status = resp.status();
        let tx = transaction_id(&resp);
        let link = resp
            .headers()
            .get("Link")
            .and_then(|v| v.to_str().ok())
            .map(ToString::to_string);
        log_response_headers(resp.headers());
        let text = resp
            .text()
            .map_err(|e| Error::Transport(format!("read body: {e}")))?;
        log_body("<", &text);
        if !status.is_success() {
            return Err(from_http_text(status, tx, &text));
        }
        let mut body: Value = serde_json::from_str(&text)
            .map_err(|e| Error::Transport(format!("parse response: {e}")))?;
        if let Some(Value::Array(records)) = body.get_mut("result").map(Value::take) {
            for r in records {
                self.buffer.push_back(r);
            }
        }
        self.next_query.clear(); // next link carries all params
        self.next_url = link.and_then(parse_next_link);
        if self.next_url.is_none() {
            self.finished = true;
        }
        Ok(())
    }
}

fn parse_next_link(header: String) -> Option<String> {
    // ServiceNow Link: <...>;rel="next", <...>;rel="first", ...
    for part in header.split(',') {
        let part = part.trim();
        if let Some((url_part, rel_part)) = part.split_once(';') {
            let rel = rel_part.trim();
            if rel.contains("rel=\"next\"") {
                let u = url_part
                    .trim()
                    .trim_start_matches('<')
                    .trim_end_matches('>');
                return Some(u.to_string());
            }
        }
    }
    None
}

impl<'a> Iterator for Paginator<'a> {
    type Item = Result<Value>;
    fn next(&mut self) -> Option<Self::Item> {
        if let Some(cap) = self.cap
            && cap != 0
            && self.emitted >= cap
        {
            return None;
        }
        if self.buffer.is_empty()
            && !self.finished
            && let Err(e) = self.fetch_next_page()
        {
            self.finished = true;
            return Some(Err(e));
        }
        match self.buffer.pop_front() {
            Some(v) => {
                self.emitted += 1;
                Some(Ok(v))
            }
            None => None,
        }
    }
}

fn transaction_id(resp: &Response) -> Option<String> {
    resp.headers()
        .get("X-Transaction-ID")
        .and_then(|v| v.to_str().ok())
        .map(ToString::to_string)
}

/// The cookie the AMB websocket actually authenticates with. Without it the
/// upgrade still succeeds and the Bayeux handshake still reports success — the
/// connection simply never delivers anything (see `crate::amb`).
const SESSION_COOKIE: &str = "JSESSIONID";

/// Pull the session cookies out of a response's `Set-Cookie` headers.
///
/// ServiceNow clears `glide_user`/`glide_user_session` by re-sending them *empty*
/// with `Max-Age=0`. Those tombstones must be dropped: echoing an empty value
/// back on the upgrade contributes nothing and, on some stacks, unsets the real
/// cookie. Only name/value survives — attributes (`Path`, `HttpOnly`, …) are
/// response-side directives and are not valid in a request `Cookie:` header.
fn collect_session_cookies(headers: &HeaderMap) -> Vec<(String, String)> {
    headers
        .get_all(SET_COOKIE)
        .iter()
        .filter_map(|v| v.to_str().ok())
        .filter_map(|raw| raw.split(';').next())
        .filter_map(|pair| pair.split_once('='))
        .map(|(name, value)| (name.trim().to_string(), value.trim().to_string()))
        .filter(|(_, value)| !value.is_empty())
        .collect()
}

fn from_http_text(status: StatusCode, tx: Option<String>, raw: &str) -> Error {
    let body: Option<Value> = serde_json::from_str(raw).ok();
    let (message, detail, sn_error) = body
        .as_ref()
        .and_then(|v| v.get("error"))
        .map(|err| {
            (
                err.get("message")
                    .and_then(|m| m.as_str())
                    .unwrap_or("ServiceNow error")
                    .to_string(),
                err.get("detail")
                    .and_then(|d| d.as_str())
                    .map(ToString::to_string),
                Some(err.clone()),
            )
        })
        .unwrap_or_else(|| {
            let fallback_detail = if raw.trim().is_empty() {
                None
            } else {
                Some(truncate_body(raw, 500))
            };
            (format!("HTTP {status}"), fallback_detail, None)
        });
    match status.as_u16() {
        401 | 403 => Error::Auth {
            status: status.as_u16(),
            message,
            transaction_id: tx,
        },
        s => Error::Api {
            status: s,
            message,
            detail,
            transaction_id: tx,
            sn_error,
        },
    }
}

fn truncate_body(s: &str, max_chars: usize) -> String {
    if s.chars().count() <= max_chars {
        s.to_string()
    } else {
        let mut out: String = s.chars().take(max_chars).collect();
        out.push('…');
        out
    }
}

#[cfg(test)]
mod tests {
    use super::{
        DOWNLOAD_BUFFER_BYTES, DownloadError, collect_session_cookies, redact_token_json,
        stream_body,
    };
    use reqwest::header::{HeaderMap, HeaderValue, SET_COOKIE};
    use std::io::{self, Read, Write};

    fn headers(raw: &[&str]) -> HeaderMap {
        let mut h = HeaderMap::new();
        for v in raw {
            h.append(SET_COOKIE, HeaderValue::from_str(v).unwrap());
        }
        h
    }

    #[test]
    fn session_cookies_keep_name_value_and_drop_attributes() {
        let h = headers(&[
            "JSESSIONID=ABC123; Path=/; HttpOnly",
            "glide_user_route=node42; Path=/; Secure; HttpOnly",
        ]);
        let jar = collect_session_cookies(&h);
        assert_eq!(
            jar,
            vec![
                ("JSESSIONID".to_string(), "ABC123".to_string()),
                ("glide_user_route".to_string(), "node42".to_string()),
            ]
        );
    }

    #[test]
    fn tombstone_cookies_are_dropped() {
        // ServiceNow clears these by re-sending them empty with Max-Age=0.
        // Forwarding an empty value would unset the real cookie on some stacks.
        let h = headers(&[
            "JSESSIONID=ABC123; Path=/; HttpOnly",
            "glide_user=; Max-Age=0; Expires=Thu, 01-Jan-1970 00:00:10 GMT; Path=/",
            "glide_user_session=; Max-Age=0; Path=/",
        ]);
        let jar = collect_session_cookies(&h);
        assert_eq!(jar.len(), 1, "only the live cookie survives: {jar:?}");
        assert_eq!(jar[0].0, "JSESSIONID");
    }

    #[test]
    fn cookie_value_containing_equals_is_preserved() {
        // Base64-ish session values can carry '='; only the FIRST '=' separates.
        let h = headers(&["glide_session_store=aGVsbG8=; Path=/"]);
        let jar = collect_session_cookies(&h);
        assert_eq!(jar[0].1, "aGVsbG8=");
    }

    #[test]
    fn no_cookies_yields_empty_jar() {
        assert!(collect_session_cookies(&HeaderMap::new()).is_empty());
    }

    #[test]
    fn token_secrets_are_masked_but_metadata_stays_readable() {
        let body = r#"{"access_token":"AT_SECRET","refresh_token":"RT_SECRET","id_token":"ID_SECRET","token_type":"Bearer","expires_in":1800,"scope":"useraccount"}"#;
        let out = redact_token_json(body);

        // No secret value survives.
        assert!(!out.contains("AT_SECRET"), "access_token leaked: {out}");
        assert!(!out.contains("RT_SECRET"), "refresh_token leaked: {out}");
        assert!(!out.contains("ID_SECRET"), "id_token leaked: {out}");
        assert!(out.contains("****"), "expected mask marker: {out}");

        // Non-secret metadata remains legible for debugging.
        assert!(out.contains("\"token_type\":\"Bearer\""), "{out}");
        assert!(out.contains("\"expires_in\":1800"), "{out}");
        assert!(out.contains("\"scope\":\"useraccount\""), "{out}");
    }

    #[test]
    fn error_response_passes_through_without_masking() {
        // Token-endpoint errors carry no secret and are useful verbatim.
        let body = r#"{"error":"invalid_grant","error_description":"code expired"}"#;
        let out = redact_token_json(body);
        assert!(out.contains("invalid_grant"), "{out}");
        assert!(out.contains("code expired"), "{out}");
    }

    #[test]
    fn non_object_body_collapses_to_placeholder() {
        // An HTML error page (or any non-object) must not reach the log verbatim.
        let out = redact_token_json("<html>access_token=leaked</html>");
        assert_eq!(out, "<token response: redacted>");
        assert!(!out.contains("leaked"));
    }

    /// A reader that yields `total` bytes of a repeating pattern in chunks the
    /// caller cannot control, recording the largest buffer it was ever handed.
    struct PatternReader {
        remaining: usize,
        offset: u8,
        max_buf_seen: usize,
    }

    impl Read for PatternReader {
        fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
            self.max_buf_seen = self.max_buf_seen.max(buf.len());
            if self.remaining == 0 {
                return Ok(0);
            }
            // Short reads are legal and normal on a socket; hand back well under
            // the buffer so the loop cannot assume it fills every time.
            let n = buf.len().min(self.remaining).min(7919);
            for (i, slot) in buf.iter_mut().take(n).enumerate() {
                *slot = self.offset.wrapping_add(i as u8);
            }
            self.offset = self.offset.wrapping_add(n as u8);
            self.remaining -= n;
            Ok(n)
        }
    }

    fn run_stream(total: usize) -> (Vec<u8>, usize, u64) {
        let mut src = PatternReader {
            remaining: total,
            offset: 0,
            max_buf_seen: 0,
        };
        let mut sink = Vec::new();
        let mut buf = vec![0u8; DOWNLOAD_BUFFER_BYTES];
        let mut written = 0u64;
        let n = stream_body(&mut src, &mut sink, &mut buf, &mut written).unwrap();
        (sink, src.max_buf_seen, n)
    }

    #[test]
    fn streaming_never_reads_more_than_the_fixed_buffer() {
        // The claim under test is the whole point of the change: memory does not
        // track payload size. 4 MiB through a 64 KiB buffer, byte-exact.
        let total = 4 * 1024 * 1024;
        let (sink, max_buf_seen, n) = run_stream(total);
        assert_eq!(n, total as u64);
        assert_eq!(sink.len(), total);
        assert!(
            max_buf_seen <= DOWNLOAD_BUFFER_BYTES,
            "read buffer grew to {max_buf_seen}, past the {DOWNLOAD_BUFFER_BYTES}-byte bound"
        );
        // Contents survive the chunk boundaries, not just the length.
        assert!(sink.iter().enumerate().all(|(i, b)| *b == i as u8));
    }

    #[test]
    fn empty_body_streams_to_zero_bytes() {
        let (sink, _, n) = run_stream(0);
        assert_eq!(n, 0);
        assert!(sink.is_empty());
    }

    struct FailingReader;

    impl Read for FailingReader {
        fn read(&mut self, _buf: &mut [u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::ConnectionReset, "peer gone"))
        }
    }

    struct FailingWriter;

    impl Write for FailingWriter {
        fn write(&mut self, _buf: &[u8]) -> io::Result<usize> {
            Err(io::Error::new(io::ErrorKind::StorageFull, "disk full"))
        }
        fn flush(&mut self) -> io::Result<()> {
            Ok(())
        }
    }

    #[test]
    fn source_and_sink_failures_stay_distinguishable() {
        let mut buf = vec![0u8; DOWNLOAD_BUFFER_BYTES];
        let mut written = 0u64;
        let err =
            stream_body(&mut FailingReader, &mut Vec::new(), &mut buf, &mut written).unwrap_err();
        assert!(matches!(err, DownloadError::Source(_)), "{err:?}");
        assert_eq!(written, 0);

        let mut src = PatternReader {
            remaining: 1024,
            offset: 0,
            max_buf_seen: 0,
        };
        let mut written = 0u64;
        let err = stream_body(&mut src, &mut FailingWriter, &mut buf, &mut written).unwrap_err();
        assert!(matches!(err, DownloadError::Sink(_)), "{err:?}");
        // Nothing the sink rejected may be counted as delivered.
        assert_eq!(written, 0);
    }

    #[test]
    fn interrupted_reads_resume_instead_of_failing() {
        struct InterruptOnce {
            done: bool,
            payload: Vec<u8>,
        }
        impl Read for InterruptOnce {
            fn read(&mut self, buf: &mut [u8]) -> io::Result<usize> {
                if !self.done {
                    self.done = true;
                    return Err(io::Error::new(io::ErrorKind::Interrupted, "signal"));
                }
                let n = buf.len().min(self.payload.len());
                buf[..n].copy_from_slice(&self.payload[..n]);
                self.payload.drain(..n);
                Ok(n)
            }
        }
        let mut src = InterruptOnce {
            done: false,
            payload: b"hello".to_vec(),
        };
        let mut sink = Vec::new();
        let mut buf = vec![0u8; DOWNLOAD_BUFFER_BYTES];
        let mut written = 0u64;
        let n = stream_body(&mut src, &mut sink, &mut buf, &mut written).unwrap();
        assert_eq!(n, 5);
        assert_eq!(sink, b"hello");
    }
}
