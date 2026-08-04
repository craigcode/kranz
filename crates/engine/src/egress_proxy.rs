//! Userspace filtering egress proxy for `fs+net` sandboxed sessions (ticket
//! `egress-grant-sandbox-instrumentation`, step 3.3a — enforcement + signal).
//!
//! One proxy per sandboxed run with `enforce: fs+net`, bound to an ephemeral
//! 127.0.0.1 port whose number is read back after bind. The run wires
//! `HTTPS_PROXY`/`HTTP_PROXY`/`NO_PROXY` into the session env; HTTPS egress
//! then flows through CONNECT only (a plain-HTTP forward is NOT v1 — the API
//! endpoints and registries missions need are TLS). The per-host allowlist is
//! enforced at CONNECT time: [`crate::sandbox::effective_egress`] (the
//! `DEFAULT_EGRESS` Anthropic endpoints + the role's configured `egress[]` +
//! the mission's operator-granted `egress_grants`, folded into the sandbox
//! inputs at spec build). A denied CONNECT gets a `403` plus a structured
//! `{"ts","host","port"}` record appended — and fsynced — to
//! `<mission_dir>/runs/egress-denials.jsonl` (gitignored runtime). The run
//! reads the records ITS proxy wrote into `RunOutcome.denied_egress`, so a
//! later grant flow (3.3b) has a trigger that names the destination.
//!
//! Platform coverage:
//!
//! - macOS Seatbelt: the `fs+net` SBPL restricts outbound TCP to loopback, so
//!   the proxy is the ONLY way out — a hard boundary. (Seatbelt accepts only
//!   `*`/`localhost` network hosts, which is why per-host filtering lives
//!   here instead of in the profile.)
//! - Tier-3 containers (`provider: container`, `fs+net` with a non-empty
//!   egress list): the runtime bridge can reach the host-side proxy via
//!   `host.docker.internal`, and the session env is forwarded into the
//!   container. Env-based routing is advisory there — a process that ignores
//!   the proxy vars bypasses the filter on the bridge. A hard container
//!   boundary (internal-network sidecar) is follow-up work; `fs+net` with an
//!   EMPTY egress list keeps the `--network none` hard boundary and runs no
//!   proxy.
//! - Linux bubblewrap: OUT OF SCOPE for v1 — bwrap's all-or-nothing netns
//!   cannot reach a host-side proxy without veth plumbing, so `fs+net` there
//!   stays `--unshare-net` with no proxy and no denial signal. The macOS
//!   unified-log tail (`Sandbox: … deny(1) network-outbound`) is likewise out
//!   of scope: the proxy makes it unnecessary.
//!
//! Failure posture: a proxy that cannot bind or open its denial file fails
//! the run CLOSED — the session never launches without its enforcement, the
//! same discipline as `resolve_sandbox_or_refuse`. A denied host always
//! produces a denial record, never a silent timeout.
//!
//! v1 denial correlation is mission-level: each run's proxy appends to the
//! shared mission file (one line per denied CONNECT, fsynced), and
//! `RunOutcome.denied_egress` carries exactly the records this run's proxy
//! wrote (kept in memory alongside the file, so concurrent M3 runs never
//! cross-attribute). Per-run attribution inside the shared file via a
//! Proxy-Authorization token is documented later work, not built.

use crate::backend::SessionSpec;
use crate::error::{EngineError, Result};
use crate::paths::MissionPaths;
use crate::sandbox::SandboxBackend;
use crate::types::SandboxEnforce;
use serde::{Deserialize, Serialize};
use std::net::{Ipv4Addr, SocketAddr};
use std::path::PathBuf;
use std::sync::{Arc, Mutex};
use tokio::io::{AsyncBufReadExt, AsyncWriteExt, BufReader};
use tokio::net::{TcpListener, TcpStream};
use tokio::task::JoinHandle;

/// `HTTPS_PROXY`/`HTTP_PROXY` value scheme is always `http://` (CONNECT).
pub const HTTPS_PROXY_ENV: &str = "HTTPS_PROXY";
pub const HTTP_PROXY_ENV: &str = "HTTP_PROXY";
pub const NO_PROXY_ENV: &str = "NO_PROXY";
/// Loopback must never route through the proxy (it would recurse).
pub const NO_PROXY_VALUE: &str = "localhost,127.0.0.1";

/// One denied CONNECT: the destination the allowlist refused. Carried on
/// [`crate::runner::RunOutcome::denied_egress`]; the JSONL record adds `ts`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EgressDenial {
    pub host: String,
    pub port: u16,
}

/// Max bytes of CONNECT request head read before the connection is refused.
const MAX_HEAD_BYTES: usize = 8192;
/// Bound on a client dribbling headers, so idle connections cannot pin tasks.
const HEAD_TIMEOUT: std::time::Duration = std::time::Duration::from_secs(10);

// ---------------------------------------------------------------------------
// Allowlist
// ---------------------------------------------------------------------------

/// One parsed `host:port` allowlist entry (`*.suffix` wildcards subdomains).
#[derive(Debug, Clone, PartialEq, Eq)]
struct AllowEntry {
    /// Lowercased host pattern; the suffix when `wildcard` is set.
    host: String,
    port: u16,
    wildcard: bool,
}

impl AllowEntry {
    /// Strict parse: `host` or `host:port` (port defaults to 443), optional
    /// `*.` wildcard prefix. A malformed entry is an ERROR, never silently
    /// dropped — the caller fails closed rather than run with a narrower
    /// allowlist than the operator configured. No catch-all (`*`) support.
    fn parse(raw: &str) -> Result<AllowEntry> {
        let raw = raw.trim();
        let (host, port) = match raw.rsplit_once(':') {
            Some((host, port)) => {
                let port = port.parse::<u16>().map_err(|_| {
                    EngineError::Config(format!(
                        "egress allowlist entry {raw:?} has an invalid port"
                    ))
                })?;
                (host, port)
            }
            None => (raw, 443),
        };
        let (host, wildcard) = match host.strip_prefix("*.") {
            Some(suffix) => (suffix, true),
            None => (host, false),
        };
        let host = host.to_ascii_lowercase();
        if host.is_empty() || port == 0 || host.contains(['/', ' ', '\t']) || host.starts_with('[')
        {
            return Err(EngineError::Config(format!(
                "egress allowlist entry {raw:?} is not a valid host[:port]"
            )));
        }
        Ok(AllowEntry {
            host,
            port,
            wildcard,
        })
    }

    /// Exact host:port match, or — for `*.suffix` entries — any subdomain of
    /// `suffix` (the apex itself is NOT matched; give it its own entry).
    /// Hosts compare case-insensitively (DNS is case-insensitive).
    fn matches(&self, host: &str, port: u16) -> bool {
        if self.port != port {
            return false;
        }
        let host = host.to_ascii_lowercase();
        if self.wildcard {
            host.len() > self.host.len() && host.ends_with(&format!(".{}", self.host))
        } else {
            host == self.host
        }
    }
}

fn parse_allowlist(raw: &[String]) -> Result<Vec<AllowEntry>> {
    raw.iter().map(|entry| AllowEntry::parse(entry)).collect()
}

// ---------------------------------------------------------------------------
// CONNECT request parsing
// ---------------------------------------------------------------------------

/// A well-formed `CONNECT host:port HTTP/1.x` target (IPv4/hostname only —
/// IPv6 literals are refused, not mis-parsed).
#[derive(Debug, Clone, PartialEq, Eq)]
struct ConnectTarget {
    host: String,
    port: u16,
}

/// Parse the request head (request line + headers, CRLF-terminated). Only the
/// request line is interpreted; headers (including any Proxy-Authorization —
/// the documented per-run attribution hook) are ignored in v1. Returns `None`
/// for anything that is not exactly `CONNECT <authority> HTTP/1.x` — the
/// caller answers 400 and closes; a malformed request is never proxied.
fn parse_connect_request(head: &str) -> Option<ConnectTarget> {
    let request_line = head.lines().next()?;
    let mut parts = request_line.split_whitespace();
    let method = parts.next()?;
    let authority = parts.next()?;
    let version = parts.next()?;
    if method != "CONNECT" || parts.next().is_some() || !version.starts_with("HTTP/1.") {
        return None;
    }
    let (host, port) = authority.rsplit_once(':')?;
    if host.is_empty() || host.contains(['/', ' ', '\t']) || host.starts_with('[') {
        return None;
    }
    let port = port.parse::<u16>().ok().filter(|port| *port != 0)?;
    Some(ConnectTarget {
        host: host.to_string(),
        port,
    })
}

// ---------------------------------------------------------------------------
// Denial sink: shared mission JSONL (fsynced) + exact in-memory records
// ---------------------------------------------------------------------------

struct DenialSink {
    file: tokio::sync::Mutex<tokio::fs::File>,
    records: Mutex<Vec<EgressDenial>>,
}

impl DenialSink {
    /// Append `{"ts","host","port"}` to the mission JSONL and fsync it, then
    /// keep the in-memory copy the run reads back at shutdown. Host/port only
    /// — never request headers or credentials. A write failure loses audit
    /// but must not change the denial outcome; the caller logs and continues.
    async fn record(&self, host: &str, port: u16) -> std::io::Result<()> {
        let line = serde_json::json!({
            "ts": chrono::Utc::now().to_rfc3339(),
            "host": host,
            "port": port,
        });
        let mut bytes = line.to_string().into_bytes();
        bytes.push(b'\n');
        {
            let mut file = self.file.lock().await;
            file.write_all(&bytes).await?;
            file.sync_data().await?;
        }
        self.records
            .lock()
            .expect("denial records lock")
            .push(EgressDenial {
                host: host.to_string(),
                port,
            });
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Proxy
// ---------------------------------------------------------------------------

/// A running egress proxy: accept loop + per-connection tunnel tasks, all
/// torn down by [`EgressProxy::shutdown`].
pub struct EgressProxy {
    addr: SocketAddr,
    sink: Arc<DenialSink>,
    accept_task: JoinHandle<()>,
    conn_tasks: Arc<Mutex<tokio::task::JoinSet<()>>>,
}

impl std::fmt::Debug for EgressProxy {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("EgressProxy")
            .field("addr", &self.addr)
            .finish_non_exhaustive()
    }
}

impl EgressProxy {
    /// Bind an ephemeral 127.0.0.1 port and start serving. `allowlist` is the
    /// raw `host:port` strings (strictly parsed — a bad entry fails closed);
    /// `denial_file` is created/appended, fsynced per record.
    pub async fn start(allowlist: Vec<String>, denial_file: PathBuf) -> Result<EgressProxy> {
        EgressProxy::start_bound(
            SocketAddr::from((Ipv4Addr::LOCALHOST, 0)),
            allowlist,
            denial_file,
        )
        .await
    }

    /// [`EgressProxy::start`] on an explicit address — the seam tests use to
    /// force a bind conflict (fail-closed proof).
    pub async fn start_bound(
        addr: SocketAddr,
        allowlist: Vec<String>,
        denial_file: PathBuf,
    ) -> Result<EgressProxy> {
        let entries = parse_allowlist(&allowlist)?;
        let listener = TcpListener::bind(addr).await.map_err(|e| {
            EngineError::Backend(format!("egress proxy failed to bind {addr}: {e}"))
        })?;
        let bound = listener.local_addr().map_err(EngineError::Io)?;
        if let Some(parent) = denial_file.parent() {
            std::fs::create_dir_all(parent).map_err(|e| {
                EngineError::Backend(format!(
                    "egress proxy failed to create denial file dir {}: {e}",
                    parent.display()
                ))
            })?;
        }
        let file = tokio::fs::OpenOptions::new()
            .create(true)
            .append(true)
            .open(&denial_file)
            .await
            .map_err(|e| {
                EngineError::Backend(format!(
                    "egress proxy failed to open denial file {}: {e}",
                    denial_file.display()
                ))
            })?;
        let sink = Arc::new(DenialSink {
            file: tokio::sync::Mutex::new(file),
            records: Mutex::new(Vec::new()),
        });
        let conn_tasks = Arc::new(Mutex::new(tokio::task::JoinSet::new()));
        let accept_task = {
            let sink = Arc::clone(&sink);
            let conn_tasks = Arc::clone(&conn_tasks);
            tokio::spawn(async move {
                loop {
                    match listener.accept().await {
                        Ok((stream, _peer)) => {
                            let sink = Arc::clone(&sink);
                            let entries = entries.clone();
                            let mut tasks = conn_tasks.lock().expect("conn tasks lock");
                            tasks.spawn(handle_connection(stream, entries, sink));
                            // Reap finished tunnels so the set cannot grow
                            // unboundedly over a long session.
                            while tasks.try_join_next().is_some() {}
                        }
                        Err(e) => {
                            tracing::warn!(error = %e, "egress proxy accept failed; stopping accept loop");
                            break;
                        }
                    }
                }
            })
        };
        tracing::info!(
            addr = %bound,
            denial_file = %denial_file.display(),
            "egress proxy listening"
        );
        Ok(EgressProxy {
            addr: bound,
            sink,
            accept_task,
            conn_tasks,
        })
    }

    /// The address the proxy actually bound (ephemeral port read back).
    pub fn addr(&self) -> SocketAddr {
        self.addr
    }

    pub fn port(&self) -> u16 {
        self.addr.port()
    }

    /// Stop accepting and abort any in-flight tunnels, then return the
    /// denials THIS proxy recorded (first-seen order). Called once the run's
    /// session stream has closed, so the returned vec is complete for the run.
    pub async fn shutdown(self) -> Vec<EgressDenial> {
        self.accept_task.abort();
        self.conn_tasks.lock().expect("conn tasks lock").abort_all();
        let records = std::mem::take(&mut *self.sink.records.lock().expect("denial records lock"));
        tracing::info!(
            addr = %self.addr,
            denials = records.len(),
            "egress proxy shut down"
        );
        records
    }
}

async fn handle_connection(stream: TcpStream, allowlist: Vec<AllowEntry>, sink: Arc<DenialSink>) {
    if let Err(e) = handle_connection_inner(stream, &allowlist, &sink).await {
        tracing::debug!(error = %e, "egress proxy connection closed with an error");
    }
}

async fn handle_connection_inner(
    stream: TcpStream,
    allowlist: &[AllowEntry],
    sink: &DenialSink,
) -> std::io::Result<()> {
    let mut reader = BufReader::new(stream);
    let head = read_request_head(&mut reader).await?;
    let Some(target) = parse_connect_request(&head) else {
        write_response(
            reader.get_mut(),
            "400 Bad Request",
            b"kranz egress proxy: expected 'CONNECT host:port HTTP/1.x'\r\n",
        )
        .await?;
        return Ok(());
    };
    if !allowlist
        .iter()
        .any(|entry| entry.matches(&target.host, target.port))
    {
        tracing::info!(host = %target.host, port = target.port, "egress denied");
        if let Err(e) = sink.record(&target.host, target.port).await {
            tracing::warn!(error = %e, host = %target.host, "egress denial record write failed (audit lost; denial stands)");
        }
        let body = format!(
            "kranz egress proxy: {}:{} is not in the mission egress allowlist\r\n",
            target.host, target.port
        );
        write_response(reader.get_mut(), "403 Forbidden", body.as_bytes()).await?;
        return Ok(());
    }
    let mut upstream = match TcpStream::connect((target.host.as_str(), target.port)).await {
        Ok(upstream) => upstream,
        Err(e) => {
            let body = format!(
                "kranz egress proxy: connect to {}:{} failed: {e}\r\n",
                target.host, target.port
            );
            write_response(reader.get_mut(), "502 Bad Gateway", body.as_bytes()).await?;
            return Ok(());
        }
    };
    reader
        .get_mut()
        .write_all(b"HTTP/1.1 200 Connection Established\r\n\r\n")
        .await?;
    // Any bytes the client pipelined past the header block must reach the
    // target before the raw tunnel starts (into_inner discards the buffer).
    let leftover = reader.buffer().to_vec();
    let mut client = reader.into_inner();
    if !leftover.is_empty() {
        upstream.write_all(&leftover).await?;
    }
    tokio::io::copy_bidirectional(&mut client, &mut upstream).await?;
    Ok(())
}

/// Read lines until the CRLF terminator, bounded by [`MAX_HEAD_BYTES`] and
/// [`HEAD_TIMEOUT`]. Returns the whole head (request line + headers).
async fn read_request_head(reader: &mut BufReader<TcpStream>) -> std::io::Result<String> {
    let mut head = String::new();
    loop {
        let mut line = String::new();
        let read = tokio::time::timeout(HEAD_TIMEOUT, reader.read_line(&mut line))
            .await
            .map_err(|_| {
                std::io::Error::new(
                    std::io::ErrorKind::TimedOut,
                    "egress proxy: CONNECT header read timed out",
                )
            })??;
        if read == 0 {
            return Err(std::io::Error::new(
                std::io::ErrorKind::UnexpectedEof,
                "egress proxy: client closed before the CONNECT head completed",
            ));
        }
        head.push_str(&line);
        if head.len() > MAX_HEAD_BYTES {
            return Err(std::io::Error::new(
                std::io::ErrorKind::InvalidData,
                "egress proxy: CONNECT head exceeds 8 KiB",
            ));
        }
        if line == "\r\n" {
            return Ok(head);
        }
    }
}

/// One status line + a plain-text body, then close (Connection: close).
async fn write_response(stream: &mut TcpStream, status: &str, body: &[u8]) -> std::io::Result<()> {
    let head = format!(
        "HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
        body.len()
    );
    stream.write_all(head.as_bytes()).await?;
    stream.write_all(body).await?;
    stream.flush().await
}

// ---------------------------------------------------------------------------
// Runner seam: spawn the proxy for fs+net sessions and wire the session env
// ---------------------------------------------------------------------------

/// Spawn an egress proxy for `spec` when its resolved sandbox routes `fs+net`
/// through one, pointing `spec.env` at it. Returns `None` — no proxy, no env —
/// for unsandboxed runs, `fs`/`off`, bubblewrap (netns cannot reach a host
/// proxy; v1 out of scope), and container `fs+net` with an empty egress list
/// (`--network none`). A start failure is an Err: the run must fail closed
/// rather than launch the session without enforcement.
pub async fn maybe_start_for_session(
    spec: &mut SessionSpec,
    paths: &MissionPaths,
) -> Result<Option<EgressProxy>> {
    maybe_start_for_session_with(spec, paths, |allowlist, denial_file| {
        Box::pin(EgressProxy::start(allowlist, denial_file))
    })
    .await
}

/// [`maybe_start_for_session`] with the proxy-start step injected, so tests
/// can force a start failure and prove the fail-closed posture.
async fn maybe_start_for_session_with(
    spec: &mut SessionSpec,
    paths: &MissionPaths,
    start: impl FnOnce(
        Vec<String>,
        PathBuf,
    ) -> std::pin::Pin<
        Box<dyn std::future::Future<Output = Result<EgressProxy>> + Send>,
    >,
) -> Result<Option<EgressProxy>> {
    let Some(sandbox) = &spec.sandbox else {
        return Ok(None);
    };
    if sandbox.inputs.enforce != SandboxEnforce::FsNet {
        return Ok(None);
    }
    // Where the SESSION reaches the proxy: Seatbelt sessions run on the host
    // (loopback); container sessions cross the runtime bridge via
    // host.docker.internal (reachable from Docker Desktop's VM to the host's
    // loopback-bound listener; Linux-docker gateway plumbing is follow-up).
    let route_host = match sandbox.backend {
        SandboxBackend::Seatbelt => "127.0.0.1",
        SandboxBackend::Container if !sandbox.inputs.egress.is_empty() => "host.docker.internal",
        SandboxBackend::Bubblewrap | SandboxBackend::Container => return Ok(None),
    };
    let allowlist = crate::sandbox::effective_egress(&sandbox.inputs.egress);
    let denial_file = paths.egress_denials_file();
    let proxy = start(allowlist, denial_file).await.map_err(|e| {
        EngineError::Backend(format!(
            "egress proxy failed to start for an fs+net session; refusing to run without enforcement: {e}"
        ))
    })?;
    let url = format!("http://{}:{}", route_host, proxy.port());
    spec.env.insert(HTTPS_PROXY_ENV.to_string(), url.clone());
    spec.env.insert(HTTP_PROXY_ENV.to_string(), url);
    spec.env
        .insert(NO_PROXY_ENV.to_string(), NO_PROXY_VALUE.to_string());
    Ok(Some(proxy))
}

#[cfg(test)]
mod tests {
    use super::*;
    use tokio::io::{AsyncReadExt, AsyncWriteExt};

    fn temp_paths(dir: &tempfile::TempDir) -> MissionPaths {
        MissionPaths::new(dir.path(), "m-test")
    }

    fn fs_net_spec(
        backend: SandboxBackend,
        egress: Vec<String>,
        dir: &std::path::Path,
    ) -> SessionSpec {
        SessionSpec {
            cwd: dir.to_path_buf(),
            prompt: crate::backend::PromptMode::SingleShot("task".to_string()),
            append_system_prompt: None,
            model: "mock".to_string(),
            effort: "medium".to_string(),
            session_id: "sess".to_string(),
            resume: None,
            permission_mode: None,
            allowed_tools: Vec::new(),
            disallowed_tools: Vec::new(),
            tools: Vec::new(),
            writable: true,
            settings_json: None,
            json_schema: None,
            max_budget_usd: None,
            max_turns: None,
            env: std::collections::HashMap::new(),
            sandbox: Some(crate::sandbox::ResolvedSandbox {
                backend,
                inputs: crate::sandbox::SandboxInputs {
                    enforce: SandboxEnforce::FsNet,
                    session_cwd: dir.to_path_buf(),
                    mission_dir: dir.to_path_buf(),
                    tmpdir: std::env::temp_dir(),
                    extra_write: Vec::new(),
                    egress,
                    validator_read_deny_roots: Vec::new(),
                },
                container: None,
            }),
        }
    }

    // -- CONNECT parsing ----------------------------------------------------

    #[test]
    fn egress_proxy_connect_parsing_accepts_well_formed() {
        let target = parse_connect_request(
            "CONNECT api.anthropic.com:443 HTTP/1.1\r\nHost: api.anthropic.com:443\r\n\r\n",
        )
        .expect("well-formed CONNECT parses");
        assert_eq!(target.host, "api.anthropic.com");
        assert_eq!(target.port, 443);

        // HTTP/1.0 and extra inter-token whitespace are still well-formed.
        let target = parse_connect_request("CONNECT  example.com:8443  HTTP/1.0\r\n\r\n")
            .expect("HTTP/1.0 with wide spacing parses");
        assert_eq!(target.host, "example.com");
        assert_eq!(target.port, 8443);
    }

    #[test]
    fn egress_proxy_connect_parsing_refuses_malformed() {
        for head in [
            // Not CONNECT at all (plain-HTTP forward is NOT v1).
            "GET http://example.com/ HTTP/1.1\r\n\r\n",
            "GET / HTTP/1.1\r\nHost: example.com\r\n\r\n",
            // Missing / extra tokens.
            "CONNECT example.com:443\r\n\r\n",
            "CONNECT example.com:443 HTTP/1.1 EXTRA\r\n\r\n",
            "CONNECT\r\n\r\n",
            // Authority without a port, with a bad port, with port 0.
            "CONNECT example.com HTTP/1.1\r\n\r\n",
            "CONNECT example.com:notaport HTTP/1.1\r\n\r\n",
            "CONNECT example.com:0 HTTP/1.1\r\n\r\n",
            "CONNECT example.com:99999 HTTP/1.1\r\n\r\n",
            // Empty host / path-shaped authority / IPv6 literal.
            "CONNECT :443 HTTP/1.1\r\n\r\n",
            "CONNECT example.com/x:443 HTTP/1.1\r\n\r\n",
            "CONNECT [::1]:443 HTTP/1.1\r\n\r\n",
            // Not HTTP.
            "CONNECT example.com:443 FTP/2\r\n\r\n",
            "",
        ] {
            assert!(
                parse_connect_request(head).is_none(),
                "malformed CONNECT must be refused: {head:?}"
            );
        }
    }

    // -- Allowlist ----------------------------------------------------------

    #[test]
    fn egress_proxy_allowlist_matching() {
        let entries = parse_allowlist(&[
            "api.anthropic.com:443".to_string(),
            "*.anthropic.com:443".to_string(),
            "127.0.0.1:8080".to_string(),
        ])
        .unwrap();

        assert!(entries.iter().any(|e| e.matches("api.anthropic.com", 443)));
        // Case-insensitive host compare.
        assert!(entries.iter().any(|e| e.matches("API.Anthropic.COM", 443)));
        // Wildcard covers subdomains at any depth, but not the apex.
        assert!(entries.iter().any(|e| e.matches("x.anthropic.com", 443)));
        assert!(entries.iter().any(|e| e.matches("a.b.anthropic.com", 443)));
        assert!(!entries.iter().any(|e| e.matches("anthropic.com", 443)));
        assert!(!entries.iter().any(|e| e.matches("notanthropic.com", 443)));
        // Port must match.
        assert!(!entries.iter().any(|e| e.matches("api.anthropic.com", 8443)));
        // IP literal exact entry.
        assert!(entries.iter().any(|e| e.matches("127.0.0.1", 8080)));
        assert!(!entries.iter().any(|e| e.matches("127.0.0.1", 8081)));
    }

    #[test]
    fn egress_proxy_allowlist_defaults_port_443_and_rejects_bad_entries() {
        let entries = parse_allowlist(&["example.com".to_string()]).unwrap();
        assert!(entries.iter().any(|e| e.matches("example.com", 443)));
        assert!(!entries.iter().any(|e| e.matches("example.com", 80)));

        for bad in [
            "",
            ":443",
            "example.com:nope",
            "example.com:0",
            "ex ample.com:443",
        ] {
            assert!(
                parse_allowlist(&[bad.to_string()]).is_err(),
                "bad allowlist entry must fail closed: {bad:?}"
            );
        }
    }

    // -- Live proxy over loopback --------------------------------------------

    /// Read one response head from `stream` (through the CRLF terminator).
    async fn read_response_head(stream: &mut TcpStream) -> String {
        let mut reader = BufReader::new(stream);
        let mut head = String::new();
        loop {
            let mut line = String::new();
            reader.read_line(&mut line).await.unwrap();
            let done = line == "\r\n";
            head.push_str(&line);
            if done {
                return head;
            }
        }
    }

    #[tokio::test]
    async fn egress_proxy_tunnels_allowed_host() {
        // Loopback echo server as the CONNECT target.
        let echo = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let echo_addr = echo.local_addr().unwrap();
        tokio::spawn(async move {
            let (mut socket, _) = echo.accept().await.unwrap();
            let mut buf = [0u8; 64];
            let n = socket.read(&mut buf).await.unwrap();
            socket.write_all(&buf[..n]).await.unwrap();
        });

        let dir = tempfile::tempdir().unwrap();
        let denial_file = dir.path().join("denials.jsonl");
        let proxy =
            EgressProxy::start(vec![format!("127.0.0.1:{}", echo_addr.port())], denial_file)
                .await
                .unwrap();

        let mut client = TcpStream::connect(proxy.addr()).await.unwrap();
        client
            .write_all(
                format!("CONNECT 127.0.0.1:{} HTTP/1.1\r\n\r\n", echo_addr.port()).as_bytes(),
            )
            .await
            .unwrap();
        let (read_half, mut write_half) = client.into_split();
        let mut head_reader = BufReader::new(read_half);
        let mut status = String::new();
        head_reader.read_line(&mut status).await.unwrap();
        assert!(
            status.starts_with("HTTP/1.1 200"),
            "allowed CONNECT gets a 200: {status}"
        );
        // Drain the rest of the response head.
        loop {
            let mut line = String::new();
            head_reader.read_line(&mut line).await.unwrap();
            if line == "\r\n" {
                break;
            }
        }
        write_half.write_all(b"ping-through-proxy").await.unwrap();
        let mut buf = vec![0u8; b"ping-through-proxy".len()];
        head_reader.read_exact(&mut buf).await.unwrap();
        assert_eq!(buf, b"ping-through-proxy", "bytes tunnel both ways");

        let denials = proxy.shutdown().await;
        assert!(denials.is_empty(), "an allowed CONNECT records no denial");
    }

    #[tokio::test]
    async fn egress_proxy_denied_connect_gets_403_and_fsynced_record() {
        let dir = tempfile::tempdir().unwrap();
        let denial_file = dir.path().join("denials.jsonl");
        let proxy =
            EgressProxy::start(vec!["allowed.example:443".to_string()], denial_file.clone())
                .await
                .unwrap();

        // Denied host: 403, and the denial lands in the JSONL with ts+host+port.
        let mut client = TcpStream::connect(proxy.addr()).await.unwrap();
        client
            .write_all(b"CONNECT denied.example:443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let head = read_response_head(&mut client).await;
        assert!(head.starts_with("HTTP/1.1 403"), "denied CONNECT: {head}");

        // A second denied CONNECT to a different host appends a second line.
        let mut client = TcpStream::connect(proxy.addr()).await.unwrap();
        client
            .write_all(b"CONNECT other.example:8443 HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let head = read_response_head(&mut client).await;
        assert!(head.starts_with("HTTP/1.1 403"), "denied CONNECT: {head}");

        let denials = proxy.shutdown().await;
        assert_eq!(
            denials,
            vec![
                EgressDenial {
                    host: "denied.example".to_string(),
                    port: 443
                },
                EgressDenial {
                    host: "other.example".to_string(),
                    port: 8443
                },
            ]
        );

        // The fsynced JSONL carries the same records plus a timestamp.
        let content = std::fs::read_to_string(&denial_file).unwrap();
        let lines: Vec<&str> = content.lines().collect();
        assert_eq!(lines.len(), 2, "one JSONL line per denial: {content}");
        for (line, denial) in lines.iter().zip(denials.iter()) {
            let record: serde_json::Value = serde_json::from_str(line).unwrap();
            assert_eq!(record["host"], denial.host);
            assert_eq!(record["port"], denial.port);
            assert!(record["ts"].is_string(), "record carries ts: {line}");
        }
    }

    #[tokio::test]
    async fn egress_proxy_malformed_connect_gets_400_and_no_record() {
        let dir = tempfile::tempdir().unwrap();
        let denial_file = dir.path().join("denials.jsonl");
        let proxy =
            EgressProxy::start(vec!["allowed.example:443".to_string()], denial_file.clone())
                .await
                .unwrap();

        let mut client = TcpStream::connect(proxy.addr()).await.unwrap();
        client
            .write_all(b"GET http://example.com/ HTTP/1.1\r\n\r\n")
            .await
            .unwrap();
        let head = read_response_head(&mut client).await;
        assert!(
            head.starts_with("HTTP/1.1 400"),
            "malformed request: {head}"
        );

        let denials = proxy.shutdown().await;
        assert!(
            denials.is_empty(),
            "a malformed request is not a policy denial"
        );
        assert_eq!(
            std::fs::read_to_string(&denial_file).unwrap(),
            "",
            "no denial record for a malformed request"
        );
    }

    #[tokio::test]
    async fn egress_proxy_allowed_but_unreachable_gets_502_and_no_record() {
        // A closed loopback port that is ON the allowlist.
        let closed = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let closed_port = closed.local_addr().unwrap().port();
        drop(closed);

        let dir = tempfile::tempdir().unwrap();
        let denial_file = dir.path().join("denials.jsonl");
        let proxy = EgressProxy::start(
            vec![format!("127.0.0.1:{closed_port}")],
            denial_file.clone(),
        )
        .await
        .unwrap();

        let mut client = TcpStream::connect(proxy.addr()).await.unwrap();
        client
            .write_all(format!("CONNECT 127.0.0.1:{closed_port} HTTP/1.1\r\n\r\n").as_bytes())
            .await
            .unwrap();
        let head = read_response_head(&mut client).await;
        assert!(
            head.starts_with("HTTP/1.1 502"),
            "unreachable target: {head}"
        );

        let denials = proxy.shutdown().await;
        assert!(
            denials.is_empty(),
            "an allowed-but-unreachable target is not a policy denial"
        );
        assert_eq!(std::fs::read_to_string(&denial_file).unwrap(), "");
    }

    #[tokio::test]
    async fn egress_proxy_bind_conflict_fails_closed() {
        // Occupy a loopback port; starting the proxy on it must error (the
        // runner turns this into a refused run, never an unenforced session).
        let blocker = TcpListener::bind((Ipv4Addr::LOCALHOST, 0)).await.unwrap();
        let occupied = blocker.local_addr().unwrap();

        let dir = tempfile::tempdir().unwrap();
        let err = EgressProxy::start_bound(
            occupied,
            vec!["example.com:443".to_string()],
            dir.path().join("denials.jsonl"),
        )
        .await
        .expect_err("a bind conflict must fail closed");
        assert!(
            err.to_string().contains("failed to bind"),
            "bind failure is named: {err}"
        );
    }

    // -- Runner seam ---------------------------------------------------------

    #[tokio::test]
    async fn egress_proxy_maybe_start_wires_env_for_seatbelt_fs_net() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);
        let mut spec = fs_net_spec(SandboxBackend::Seatbelt, vec![], dir.path());

        let proxy = maybe_start_for_session(&mut spec, &paths)
            .await
            .unwrap()
            .expect("seatbelt fs+net spawns a proxy");
        let expected = format!("http://127.0.0.1:{}", proxy.port());
        assert_eq!(
            spec.env.get(HTTPS_PROXY_ENV).map(String::as_str),
            Some(expected.as_str())
        );
        assert_eq!(
            spec.env.get(HTTP_PROXY_ENV).map(String::as_str),
            Some(expected.as_str())
        );
        assert_eq!(
            spec.env.get(NO_PROXY_ENV).map(String::as_str),
            Some(NO_PROXY_VALUE)
        );
        assert!(proxy.shutdown().await.is_empty());
    }

    #[tokio::test]
    async fn egress_proxy_maybe_start_routes_container_via_host_internal() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);
        // Container fs+net with a non-empty egress list: bridge + proxy env
        // pointing at the host-side proxy (empty egress stays --network none).
        let mut spec = fs_net_spec(
            SandboxBackend::Container,
            vec!["crates.io:443".to_string()],
            dir.path(),
        );

        let proxy = maybe_start_for_session(&mut spec, &paths)
            .await
            .unwrap()
            .expect("container fs+net with egress spawns a proxy");
        let url = spec
            .env
            .get(HTTPS_PROXY_ENV)
            .expect("proxy env wired")
            .clone();
        assert!(
            url.starts_with("http://host.docker.internal:"),
            "container sessions reach the proxy via host.docker.internal: {url}"
        );
        assert_eq!(
            spec.env.get(NO_PROXY_ENV).map(String::as_str),
            Some(NO_PROXY_VALUE)
        );
        proxy.shutdown().await;
    }

    #[tokio::test]
    async fn egress_proxy_maybe_start_skips_non_proxy_backends() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);

        // No sandbox at all.
        let mut spec = fs_net_spec(SandboxBackend::Seatbelt, vec![], dir.path());
        spec.sandbox = None;
        assert!(maybe_start_for_session(&mut spec, &paths)
            .await
            .unwrap()
            .is_none());
        assert!(spec.env.is_empty());

        // fs (not fs+net): no proxy.
        let mut spec = fs_net_spec(SandboxBackend::Seatbelt, vec![], dir.path());
        spec.sandbox.as_mut().unwrap().inputs.enforce = SandboxEnforce::Fs;
        assert!(maybe_start_for_session(&mut spec, &paths)
            .await
            .unwrap()
            .is_none());
        assert!(spec.env.is_empty());

        // Bubblewrap fs+net: netns cannot reach a host proxy (v1 out of scope).
        let mut spec = fs_net_spec(SandboxBackend::Bubblewrap, vec![], dir.path());
        assert!(maybe_start_for_session(&mut spec, &paths)
            .await
            .unwrap()
            .is_none());
        assert!(spec.env.is_empty());

        // Container fs+net with an EMPTY egress list keeps --network none.
        let mut spec = fs_net_spec(SandboxBackend::Container, vec![], dir.path());
        assert!(maybe_start_for_session(&mut spec, &paths)
            .await
            .unwrap()
            .is_none());
        assert!(spec.env.is_empty());
    }

    #[tokio::test]
    async fn egress_proxy_maybe_start_failure_is_fail_closed_and_sets_no_env() {
        let dir = tempfile::tempdir().unwrap();
        let paths = temp_paths(&dir);
        let mut spec = fs_net_spec(SandboxBackend::Seatbelt, vec![], dir.path());

        let err = maybe_start_for_session_with(&mut spec, &paths, |_allowlist, _denial_file| {
            Box::pin(async move { Err(EngineError::Backend("injected start failure".to_string())) })
        })
        .await
        .expect_err("a proxy start failure must error the run");
        let message = err.to_string();
        assert!(
            message.contains("refusing to run without enforcement"),
            "{message}"
        );
        assert!(message.contains("injected start failure"), "{message}");
        assert!(
            !spec.env.contains_key(HTTPS_PROXY_ENV),
            "a failed start must not leave proxy env behind"
        );
    }
}
