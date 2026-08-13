//! ServiceNow AMB (Asynchronous Message Bus) — Bayeux/CometD over WebSocket.
//!
//! This is the transport behind ServiceNow's record watchers: the thing that
//! makes a form update itself when someone else saves the record. It is not in
//! any published API spec; what follows was established against a live instance.
//!
//! # Why a websocket can't just carry the profile's credentials
//!
//! `/amb` **ignores the `Authorization` header entirely**. It authenticates by
//! session cookie and nothing else, so — unlike every other command in this CLI
//! — a watcher cannot open its connection straight from the profile. It has to
//! make one ordinary authenticated HTTP request first, purely so the instance
//! mints a session, then carry the resulting `JSESSIONID`/`glide_user_route`
//! cookies onto the upgrade. That is [`crate::client::Client::session_cookies`].
//!
//! Whether that request authenticated with Basic or an OAuth bearer token is
//! immaterial — both yield the same cookies, and AMB accepts either. OAuth/SSO
//! profiles therefore need no special handling here.
//!
//! # Two traps, both of which look like success
//!
//! 1. **`Origin` is mandatory.** Omit it and the upgrade is rejected `403`
//!    however good the cookies are. Browsers set it implicitly, which is why it
//!    is absent from every packet capture of the ServiceNow UI — and therefore
//!    from every write-up derived from one.
//!
//! 2. **A successful upgrade proves nothing about auth.** An *unauthenticated*
//!    socket still gets `101 Switching Protocols`, a `successful: true`
//!    handshake, and a real `clientId`. It only comes apart at
//!    `/meta/subscribe`, which fails with `404::message_deleted` — an error that
//!    never mentions authentication. The single honest signal is the handshake's
//!    `ext["glide.session.status"]`: `session.logged.in` when the cookies were
//!    accepted, `session.invalidated` when they were not. [`Amb::connect`]
//!    checks it and raises an auth error, because the alternative is a client
//!    that reports "connected" and then silently receives nothing forever.

use crate::error::{Error, Result, NO_HTTP_STATUS};
use crate::observability::{log_body, log_request};
use base64::engine::general_purpose::STANDARD as BASE64;
use base64::Engine;
use rustls::client::danger::{HandshakeSignatureValid, ServerCertVerified, ServerCertVerifier};
use rustls::crypto::CryptoProvider;
use rustls::{DigitallySignedStruct, SignatureScheme};
use rustls_pki_types::pem::PemObject;
use rustls_pki_types::{CertificateDer, ServerName, UnixTime};
use serde_json::{json, Value};
use std::io::ErrorKind;
use std::net::{TcpStream, ToSocketAddrs};
use std::sync::Arc;
use std::time::Duration;
use tungstenite::client::IntoClientRequest;
use tungstenite::stream::MaybeTlsStream;
use tungstenite::{Connector, Message, WebSocket};

/// The handshake `ext` value that means the session cookies were accepted.
const SESSION_LOGGED_IN: &str = "session.logged.in";

/// Servlet path of the AMB endpoint. A trailing slash yields `400`.
const AMB_PATH: &str = "/amb";

// ── channel names ───────────────────────────────────────────────────────────

/// Encode a filter into the base64 variant AMB uses in channel names.
///
/// This is *not* base64url: the alphabet is standard (`+` and `/` are kept), and
/// only the padding differs — `==` becomes `--` and a lone `=` becomes `-`.
pub fn encode_filter(filter: &str) -> String {
    let b64 = BASE64.encode(filter.as_bytes());
    if let Some(core) = b64.strip_suffix("==") {
        format!("{core}--")
    } else if let Some(core) = b64.strip_suffix('=') {
        format!("{core}-")
    } else {
        b64
    }
}

/// Channel carrying one event per matching record change.
pub fn record_channel(table: &str, filter: &str) -> String {
    format!("/rw/default/{table}/{}", encode_filter(filter))
}

// ── TLS ─────────────────────────────────────────────────────────────────────

/// Certificate policy for the websocket. The AMB socket is opened directly
/// rather than through reqwest, so it does not inherit the HTTP client's TLS
/// settings and has to be told them.
#[derive(Debug, Default, Clone)]
pub struct TlsOptions {
    /// Skip certificate-chain validation entirely. DANGEROUS.
    pub insecure: bool,
    /// Extra CA to trust, *in addition to* the built-in roots (mirroring how
    /// `reqwest::ClientBuilder::add_root_certificate` behaves for HTTP).
    pub ca_cert: Option<String>,
}

/// Build the connector for the websocket's TLS handshake.
///
/// Always constructs the config explicitly, even when no flag asked for anything
/// unusual. Letting tungstenite fall back to its own default would mean the trust
/// store silently depended on which flags were passed — its bundled roots on the
/// plain path, ours on the `--insecure`/`--ca-cert` path. One path, one root
/// store, and no reliance on a process-level rustls provider being installed.
fn tls_connector(tls: &TlsOptions) -> Result<Connector> {
    // Pin the provider: rustls 0.23 refuses to choose when more than one is
    // compiled in, and `ring` is what the rest of this binary already links via
    // reqwest. Defaulting would risk a second crypto backend in the build.
    let provider = Arc::new(rustls::crypto::ring::default_provider());
    let builder = rustls::ClientConfig::builder_with_provider(provider.clone())
        .with_safe_default_protocol_versions()
        .map_err(|e| Error::Transport(format!("tls config: {e}")))?;

    let config = if tls.insecure {
        // --insecure wins over --ca-cert: nothing is verified, so a custom root
        // would have no one to convince.
        builder
            .dangerous()
            .with_custom_certificate_verifier(Arc::new(AcceptAnyCert(provider)))
            .with_no_client_auth()
    } else {
        let mut roots = rustls::RootCertStore::empty();
        roots.extend(webpki_roots::TLS_SERVER_ROOTS.iter().cloned());
        // A custom CA is *added* to the built-in roots, never swapped for them —
        // matching reqwest's `add_root_certificate`. Replacing them would make a
        // corporate CA break every ordinary public certificate.
        if let Some(path) = &tls.ca_cert {
            for cert in read_ca_pem(path)? {
                roots
                    .add(cert)
                    .map_err(|e| Error::Config(format!("add CA cert '{path}': {e}")))?;
            }
        }
        builder.with_root_certificates(roots).with_no_client_auth()
    };

    Ok(Connector::Rustls(Arc::new(config)))
}

fn read_ca_pem(path: &str) -> Result<Vec<CertificateDer<'static>>> {
    let sections = CertificateDer::pem_file_iter(path)
        .map_err(|e| Error::Config(format!("read CA cert '{path}': {e}")))?;

    let mut certs = Vec::new();
    for section in sections {
        match section {
            Ok(cert) => certs.push(cert),
            // Having parsed nothing, the file simply holds no certificate — say
            // that, rather than surfacing a PEM-parser detail the caller cannot
            // act on. Having parsed some, a trailing non-certificate section (a
            // key, a comment) must not sink an otherwise usable bundle.
            Err(e) if certs.is_empty() => {
                return Err(Error::Config(format!(
                    "no certificates found in CA cert '{path}': {e}"
                )))
            }
            Err(_) => break,
        }
    }

    if certs.is_empty() {
        return Err(Error::Config(format!(
            "no certificates found in CA cert '{path}'"
        )));
    }
    Ok(certs)
}

/// Accepts any server certificate. Only ever built when the caller asked for
/// `--insecure`, which the help text already flags as DANGEROUS — on this socket
/// it is doubly so, because what travels over it is the session cookie.
///
/// Signature verification is deliberately left intact; only chain validation is
/// skipped. That is exactly what reqwest's `danger_accept_invalid_certs` does, so
/// `--insecure` means the same thing on both transports.
#[derive(Debug)]
struct AcceptAnyCert(Arc<CryptoProvider>);

impl ServerCertVerifier for AcceptAnyCert {
    fn verify_server_cert(
        &self,
        _end_entity: &CertificateDer<'_>,
        _intermediates: &[CertificateDer<'_>],
        _server_name: &ServerName<'_>,
        _ocsp_response: &[u8],
        _now: UnixTime,
    ) -> std::result::Result<ServerCertVerified, rustls::Error> {
        Ok(ServerCertVerified::assertion())
    }

    fn verify_tls12_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls12_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn verify_tls13_signature(
        &self,
        message: &[u8],
        cert: &CertificateDer<'_>,
        dss: &DigitallySignedStruct,
    ) -> std::result::Result<HandshakeSignatureValid, rustls::Error> {
        rustls::crypto::verify_tls13_signature(
            message,
            cert,
            dss,
            &self.0.signature_verification_algorithms,
        )
    }

    fn supported_verify_schemes(&self) -> Vec<SignatureScheme> {
        self.0.signature_verification_algorithms.supported_schemes()
    }
}

// ── client ──────────────────────────────────────────────────────────────────

/// One event delivered on a subscribed channel.
#[derive(Debug, Clone)]
pub struct Event {
    pub channel: String,
    pub data: Value,
}

/// A single AMB websocket session.
///
/// Deliberately scoped to one socket lifetime: it does not reconnect. Recovery
/// needs a *fresh session cookie*, which needs the HTTP `Client`, so supervision
/// lives in `cli::watch` where both are in scope.
pub struct Amb {
    ws: WebSocket<MaybeTlsStream<TcpStream>>,
    client_id: String,
    next_id: u64,
    /// A death verdict noticed mid-batch, held so the events sharing that frame
    /// could be delivered first. The next [`Amb::poll`] reports it.
    dying: Option<String>,
}

impl Amb {
    /// Open the socket, handshake, and arm the long-poll.
    ///
    /// `base_url` is the normalized instance URL (`https://acme.service-now.com`);
    /// `cookies` is a `Cookie:` header value from [`crate::client::Client::session_cookies`].
    pub fn connect(
        base_url: &str,
        cookies: &str,
        connect_timeout: Duration,
        tls: &TlsOptions,
    ) -> Result<Self> {
        let (ws_url, host, port, origin) = endpoint(base_url)?;

        log_request("WS", &ws_url);
        let mut request = ws_url
            .as_str()
            .into_client_request()
            .map_err(|e| Error::Transport(format!("build websocket request: {e}")))?;
        {
            let h = request.headers_mut();
            // Without Origin the upgrade is 403 regardless of the cookies.
            h.insert(
                "Origin",
                origin
                    .parse()
                    .map_err(|_| Error::Transport(format!("invalid origin: {origin}")))?,
            );
            h.insert(
                "Cookie",
                cookies
                    .parse()
                    .map_err(|_| Error::Transport("session cookie is not a valid header".into()))?,
            );
            h.insert(
                "User-Agent",
                format!("sn/{}", env!("CARGO_PKG_VERSION"))
                    .parse()
                    .expect("static user agent is a valid header"),
            );
        }

        // Connect the TCP socket by hand so `--timeout` bounds the dial; the
        // convenience `tungstenite::connect` offers no timeout hook.
        let addr = (host.as_str(), port)
            .to_socket_addrs()
            .map_err(|e| Error::Transport(format!("resolve {host}: {e}")))?
            .next()
            .ok_or_else(|| Error::Transport(format!("resolve {host}: no addresses")))?;
        let stream = TcpStream::connect_timeout(&addr, connect_timeout)
            .map_err(|e| Error::Transport(format!("connect {host}:{port}: {e}")))?;

        let connector = tls_connector(tls)?;
        let (ws, _resp) =
            tungstenite::client_tls_with_config(request, stream, None, Some(connector))
                .map_err(|e| Error::Transport(format!("amb websocket upgrade: {e}")))?;

        let mut amb = Self {
            ws,
            client_id: String::new(),
            next_id: 0,
            dying: None,
        };
        amb.set_read_timeout(Some(connect_timeout))?;
        amb.handshake()?;
        amb.open_long_poll()?;
        Ok(amb)
    }

    fn handshake(&mut self) -> Result<()> {
        self.send(json!({
            "channel": "/meta/handshake",
            "version": "1.0",
            "minimumVersion": "1.0",
            "supportedConnectionTypes": ["websocket", "long-polling"],
            "advice": {"timeout": 60_000, "interval": 0},
            "ext": {"supportsSubscribeCommandFlow": true},
        }))?;
        let reply = self.await_reply("/meta/handshake")?;

        // Check the session BEFORE trusting `successful`: an unauthenticated
        // handshake is also "successful" and also hands back a clientId. Only
        // this field distinguishes them, and only here — by /meta/subscribe the
        // failure has degraded into `404::message_deleted`.
        if let Some(status) = session_status(&reply) {
            if status != SESSION_LOGGED_IN {
                return Err(Error::Auth {
                    status: 401,
                    message: format!(
                        "ServiceNow rejected the AMB session ({status}); \
                         the websocket authenticates by session cookie, not by credentials"
                    ),
                    transaction_id: None,
                });
            }
        }

        if reply.get("successful").and_then(Value::as_bool) != Some(true) {
            let err = reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            return Err(Error::Transport(format!("amb handshake failed: {err}")));
        }

        self.client_id = reply
            .get("clientId")
            .and_then(Value::as_str)
            .ok_or_else(|| Error::Transport("amb handshake returned no clientId".into()))?
            .to_string();
        Ok(())
    }

    /// Send the priming `/meta/connect` (`advice.timeout: 0`, answered at once),
    /// then arm the long-poll that actually carries events.
    fn open_long_poll(&mut self) -> Result<()> {
        self.send(json!({
            "channel": "/meta/connect",
            "connectionType": "websocket",
            "advice": {"timeout": 0},
            "clientId": self.client_id,
        }))?;
        self.await_reply("/meta/connect")?;
        self.rearm()
    }

    /// Re-issue `/meta/connect`. This is the delivery vehicle, not a keepalive:
    /// events only flow while a connect is outstanding, so every connect
    /// response must be answered with another one or the stream goes quiet.
    fn rearm(&mut self) -> Result<()> {
        self.send(json!({
            "channel": "/meta/connect",
            "connectionType": "websocket",
            "clientId": self.client_id,
        }))
    }

    /// Subscribe to a channel. Fails loudly: a silently-refused subscription is
    /// indistinguishable from a table where nothing happens to be changing.
    pub fn subscribe(&mut self, channel: &str) -> Result<()> {
        self.send(json!({
            "channel": "/meta/subscribe",
            "subscription": channel,
            "clientId": self.client_id,
        }))?;
        let reply = self.await_reply("/meta/subscribe")?;
        if reply.get("successful").and_then(Value::as_bool) != Some(true) {
            let err = reply
                .get("error")
                .and_then(Value::as_str)
                .unwrap_or("unknown error");
            // The refusal is an in-band Bayeux reply; the only HTTP exchange
            // here was the 101 upgrade, so there is no status to publish.
            return Err(Error::Api {
                status: NO_HTTP_STATUS,
                message: format!("amb subscribe to {channel} failed: {err}"),
                detail: Some(
                    "check the table name and that the query is a valid encoded query".into(),
                ),
                transaction_id: None,
                sn_error: None,
            });
        }
        Ok(())
    }

    /// Wait up to `wait` for events. An empty vec means the poll simply expired
    /// — the caller uses that as its tick for deadlines and Ctrl-C.
    pub fn poll(&mut self, wait: Duration) -> Result<Vec<Event>> {
        // A death noticed mid-batch last time: its frame's events have been
        // delivered, so now the session's end can be reported.
        if let Some(reason) = self.dying.take() {
            return Err(Error::Transport(reason));
        }
        self.set_read_timeout(Some(wait))?;
        let Some(batch) = self.read_batch()? else {
            return Ok(Vec::new());
        };

        let mut events = Vec::new();
        let mut death: Option<String> = None;
        for msg in batch {
            let channel = msg.get("channel").and_then(Value::as_str).unwrap_or("");
            if channel == "/meta/connect" {
                // A condemned session must not be re-armed, but the batch must
                // still be drained: a Bayeux frame legally carries events beside
                // the verdict, and returning early would drop records the wire
                // actually delivered.
                if let Some(reason) = death_verdict(&msg) {
                    death = Some(reason);
                } else {
                    self.rearm()?;
                }
            } else if !channel.starts_with("/meta/") && !channel.is_empty() {
                if let Some(data) = msg.get("data") {
                    events.push(Event {
                        channel: channel.to_string(),
                        data: data.clone(),
                    });
                }
            }
        }
        match death {
            Some(reason) if events.is_empty() => Err(Error::Transport(reason)),
            Some(reason) => {
                self.dying = Some(reason);
                Ok(events)
            }
            None => Ok(events),
        }
    }

    /// Best-effort clean shutdown: tell the server to drop the client, then close.
    pub fn disconnect(&mut self) {
        let _ = self.send(json!({
            "channel": "/meta/disconnect",
            "clientId": self.client_id,
        }));
        let _ = self.ws.close(None);
        let _ = self.ws.flush();
    }

    // ── internals ───────────────────────────────────────────────────────────

    fn send(&mut self, mut msg: Value) -> Result<()> {
        self.next_id += 1;
        msg["id"] = json!(self.next_id.to_string());
        let text = Value::Array(vec![msg]).to_string();
        log_body(">", &text);
        self.ws
            .send(Message::Text(text.into()))
            .map_err(|e| Error::Transport(format!("amb send: {e}")))
    }

    /// Read until the reply for `channel` arrives, re-arming the long-poll for
    /// any connect responses that overtake it.
    fn await_reply(&mut self, channel: &str) -> Result<Value> {
        loop {
            let Some(batch) = self.read_batch()? else {
                return Err(Error::Transport(format!(
                    "timed out waiting for {channel} response"
                )));
            };
            let mut rearm = false;
            for msg in &batch {
                if msg.get("channel").and_then(Value::as_str) == Some(channel) {
                    return Ok(msg.clone());
                }
                if msg.get("channel").and_then(Value::as_str) == Some("/meta/connect") {
                    // A death verdict overtaking the awaited reply must not be
                    // re-armed away: without this check a session reaped during
                    // subscribe degrades into `404::message_deleted` — an error
                    // that blames the caller's table name for a session problem.
                    if let Some(reason) = death_verdict(msg) {
                        return Err(Error::Transport(reason));
                    }
                    rearm = true;
                }
            }
            if rearm {
                self.rearm()?;
            }
        }
    }

    /// One websocket frame's worth of Bayeux messages. `Ok(None)` = read timed
    /// out with nothing pending, which is normal and not an error.
    fn read_batch(&mut self) -> Result<Option<Vec<Value>>> {
        loop {
            match self.ws.read() {
                Ok(Message::Text(text)) => {
                    log_body("<", text.as_str());
                    let parsed: Value = serde_json::from_str(text.as_str())
                        .map_err(|e| Error::Transport(format!("parse amb message: {e}")))?;
                    return Ok(Some(match parsed {
                        Value::Array(msgs) => msgs,
                        single => vec![single],
                    }));
                }
                // tungstenite queues the pong itself; nothing to do but keep reading.
                Ok(
                    Message::Ping(_) | Message::Pong(_) | Message::Binary(_) | Message::Frame(_),
                ) => continue,
                Ok(Message::Close(_)) => {
                    return Err(Error::Transport("amb websocket closed by server".into()))
                }
                Err(tungstenite::Error::Io(e)) if is_timeout(&e) => return Ok(None),
                Err(tungstenite::Error::ConnectionClosed | tungstenite::Error::AlreadyClosed) => {
                    return Err(Error::Transport("amb websocket closed".into()))
                }
                Err(e) => return Err(Error::Transport(format!("amb websocket: {e}"))),
            }
        }
    }

    fn set_read_timeout(&mut self, d: Option<Duration>) -> Result<()> {
        let sock = match self.ws.get_ref() {
            MaybeTlsStream::Plain(s) => s,
            MaybeTlsStream::Rustls(s) => &s.sock,
            _ => return Ok(()),
        };
        sock.set_read_timeout(d)
            .map_err(|e| Error::Transport(format!("set websocket read timeout: {e}")))
    }
}

/// A read that expired rather than failed. Platforms disagree on which kind they
/// report for a socket timeout, so both count.
fn is_timeout(e: &std::io::Error) -> bool {
    matches!(e.kind(), ErrorKind::WouldBlock | ErrorKind::TimedOut)
}

/// The `ext["glide.session.status"]` a Bayeux reply carries, if any. The one
/// place the server reports the backing HTTP session's health — checked at
/// handshake (where a bad status means the cookies were rejected) and on every
/// connect reply (where it means the session has since been reaped).
fn session_status(msg: &Value) -> Option<&str> {
    msg.get("ext")?.get("glide.session.status")?.as_str()
}

/// Whether a connect reply reports the backing HTTP session dead. A reply that
/// carries no status is not a verdict — only an explicit non-logged-in value
/// condemns the session, mirroring the handshake check.
fn session_dead(msg: &Value) -> bool {
    session_status(msg).is_some_and(|s| s != SESSION_LOGGED_IN)
}

/// Why a connect reply condemns the session, if it does. Both read loops use
/// this before re-arming: the instance reaps a watcher's session mid-stream
/// (~60-120s after mint, measured; neither AMB traffic nor HTTP touches prevent
/// it), and a dead session still answers `successful: true` and still accepts
/// re-arms — it just never delivers another event. Re-arming it would be the
/// silent-zombie failure mode the handshake check exists to prevent, so the
/// death surfaces as a recoverable drop and the supervisor reconnects on fresh
/// cookies. The server can also order a full re-handshake (session gone, node
/// failed over), which cannot be satisfied in place for the same reason.
fn death_verdict(msg: &Value) -> Option<String> {
    if session_dead(msg) {
        return Some("amb session invalidated by the instance".into());
    }
    if msg.pointer("/advice/reconnect").and_then(Value::as_str) == Some("handshake") {
        return Some("amb session expired; server requested re-handshake".into());
    }
    None
}

/// Derive `(ws_url, host, port, origin)` from a normalized instance base URL.
fn endpoint(base_url: &str) -> Result<(String, String, u16, String)> {
    let base = base_url.trim_end_matches('/');
    let (scheme, rest) = base
        .split_once("://")
        .ok_or_else(|| Error::Config(format!("instance URL has no scheme: {base}")))?;
    let (ws_scheme, default_port) = match scheme {
        "https" => ("wss", 443),
        "http" => ("ws", 80),
        other => {
            return Err(Error::Config(format!(
                "instance URL has unsupported scheme '{other}'"
            )))
        }
    };
    let hostport = rest.split('/').next().unwrap_or(rest);
    let (host, port) = match hostport.rsplit_once(':') {
        Some((h, p)) => (
            h.to_string(),
            p.parse::<u16>()
                .map_err(|_| Error::Config(format!("invalid port in instance URL: {base}")))?,
        ),
        None => (hostport.to_string(), default_port),
    };
    Ok((
        format!("{ws_scheme}://{hostport}{AMB_PATH}"),
        host,
        port,
        base.to_string(),
    ))
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn unpadded_filter_is_plain_base64() {
        // 39 bytes → an exact multiple of 3 → no padding to rewrite.
        let f = "sys_id=1c741bd70b2322007518478d83673af3";
        assert_eq!(
            encode_filter(f),
            "c3lzX2lkPTFjNzQxYmQ3MGIyMzIyMDA3NTE4NDc4ZDgzNjczYWYz"
        );
        assert!(!encode_filter(f).ends_with('-'));
    }

    #[test]
    fn single_pad_becomes_one_dash() {
        // "active=true" is 11 bytes → one '=' of padding.
        let e = encode_filter("active=true");
        assert!(e.ends_with('-'), "{e}");
        assert!(!e.ends_with("--"), "{e}");
        assert!(!e.contains('='), "padding must be rewritten: {e}");
    }

    #[test]
    fn double_pad_becomes_two_dashes() {
        // "priority=1" is 10 bytes → two '=' of padding.
        let e = encode_filter("priority=1");
        assert!(e.ends_with("--"), "{e}");
        assert!(!e.contains('='), "padding must be rewritten: {e}");
    }

    #[test]
    fn record_channel_carries_table_and_encoded_query() {
        assert!(record_channel("incident", "priority=1").starts_with("/rw/default/incident/"));
    }

    #[test]
    fn a_rehandshake_order_is_a_death_verdict() {
        let msg = json!({
            "channel": "/meta/connect",
            "successful": false,
            "advice": {"reconnect": "handshake"},
        });
        assert!(death_verdict(&msg).is_some());
        assert!(death_verdict(&json!({"channel": "/meta/connect", "successful": true})).is_none());
    }

    #[test]
    fn a_logged_in_connect_reply_is_not_a_death() {
        let msg = json!({
            "channel": "/meta/connect",
            "successful": true,
            "ext": {"glide.session.status": "session.logged.in"},
        });
        assert!(!session_dead(&msg));
    }

    #[test]
    fn an_invalidated_connect_reply_is_a_death_despite_successful_true() {
        // Captured live: the reaped session keeps answering `successful: true`.
        let msg = json!({
            "channel": "/meta/connect",
            "successful": true,
            "ext": {
                "glide.amb.active": true,
                "glide.session.status": "session.invalidated",
                "glide.session.time.remaining.in.seconds": 0,
            },
        });
        assert!(session_dead(&msg));
    }

    #[test]
    fn a_reply_without_a_session_status_is_not_a_verdict() {
        assert!(!session_dead(
            &json!({"channel": "/meta/connect", "successful": true})
        ));
        assert!(!session_dead(
            &json!({"channel": "/meta/connect", "ext": {}})
        ));
    }

    #[test]
    fn endpoint_maps_https_to_wss() {
        let (url, host, port, origin) = endpoint("https://dev1.service-now.com").unwrap();
        assert_eq!(url, "wss://dev1.service-now.com/amb");
        assert_eq!(host, "dev1.service-now.com");
        assert_eq!(port, 443);
        // Origin must be the plain instance URL, not the ws:// one.
        assert_eq!(origin, "https://dev1.service-now.com");
    }

    #[test]
    fn endpoint_honors_explicit_port() {
        let (url, host, port, _) = endpoint("https://localhost:8443").unwrap();
        assert_eq!(url, "wss://localhost:8443/amb");
        assert_eq!(host, "localhost");
        assert_eq!(port, 8443);
    }

    #[test]
    fn endpoint_rejects_schemeless_instance() {
        assert!(endpoint("dev1.service-now.com").is_err());
    }

    #[test]
    fn every_path_builds_a_connector_so_trust_never_depends_on_flags() {
        // Every path builds its own connector, so the trust store can never
        // silently depend on which flags were passed.
        assert!(tls_connector(&TlsOptions::default()).is_ok());
        assert!(tls_connector(&TlsOptions {
            insecure: true,
            ca_cert: None,
        })
        .is_ok());
    }

    #[test]
    fn insecure_overrides_a_ca_cert_and_never_reads_it() {
        // Nothing is verified, so the custom root has no one to convince; the
        // path must not even be opened, or --insecure would fail on a bad path.
        assert!(tls_connector(&TlsOptions {
            insecure: true,
            ca_cert: Some("/nonexistent/nope.pem".into()),
        })
        .is_ok());
    }

    /// `Connector` is not `Debug`, so `unwrap_err()` cannot format the Ok side.
    fn expect_err(tls: TlsOptions) -> Error {
        match tls_connector(&tls) {
            Err(e) => e,
            Ok(_) => panic!("expected a config error, got a connector"),
        }
    }

    #[test]
    fn a_missing_ca_cert_is_a_config_error() {
        let err = expect_err(TlsOptions {
            insecure: false,
            ca_cert: Some("/nonexistent/nope.pem".into()),
        });
        assert_eq!(
            err.exit_code(),
            1,
            "bad path is usage/config, not transport"
        );
    }

    #[test]
    fn a_pem_with_no_certificates_is_rejected() {
        // rustls_pemfile silently yields zero certs for junk input; without an
        // explicit check that would build a connector trusting only the built-in
        // roots, quietly ignoring the CA the caller asked for.
        let mut f = tempfile::NamedTempFile::new().unwrap();
        std::io::Write::write_all(&mut f, b"not a certificate\n").unwrap();
        let err = expect_err(TlsOptions {
            insecure: false,
            ca_cert: Some(f.path().to_string_lossy().into_owned()),
        });
        assert!(err.to_string().contains("no certificates"), "{err}");
    }
}
