//! The egress proxy (D-067): every sandboxed shell's network goes through here, so the kernel
//! sees, rates and decides every host a command reaches.
//!
//! - One Unix socket per session (`<zen home>/proxy/<session>.sock`, folder mode 700), bound into the
//!   session's sandbox. Inside, `zend bridge` (main.rs) listens on 127.0.0.1:3128 in the sandbox's
//!   own network namespace and forwards there; `HTTP(S)_PROXY` point at it, so curl, git, cargo,
//!   pip and npm use it as they are.
//! - `CONNECT host:port` (TLS, decided by host) and plain `http://` requests; ports 80 and 443 only.
//!   Hosts resolve through web.rs's public-only resolver: nothing on the VM's network is reachable.
//! - Each new host is decided by S1's `egress` decision (the session's context level and the host's
//!   rating: the trust list, else System One's source rating), remembered for the session, and
//!   recorded (`egress`). A held host is a queue item the owner can allow for the session.
//! - What a command reached rates its output (`contacted_since`): content from an unknown host
//!   raises the session like a page fetched with `web_fetch`.

use std::collections::{HashMap, HashSet};
use std::path::PathBuf;
use std::sync::{LazyLock, Mutex};
use std::time::Instant;

use anyhow::{Context, Result};
use sqlx::PgPool;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::{TcpStream, UnixListener, UnixStream};
use uuid::Uuid;

/// The port the bridge listens on inside the sandbox.
pub const BRIDGE_PORT: u16 = 3128;

pub fn dir() -> PathBuf {
    crate::zen_home().join("proxy")
}

pub fn socket_path(session: Uuid) -> PathBuf {
    dir().join(format!("{session}.sock"))
}

static LISTENING: LazyLock<Mutex<HashSet<Uuid>>> = LazyLock::new(Default::default);
/// A host a session reached: its name, rating and when.
type Contact = (String, i16, Instant);
/// Hosts each session reached.
static CONTACTED: LazyLock<Mutex<HashMap<Uuid, Vec<Contact>>>> = LazyLock::new(Default::default);

/// Start the session's proxy socket if it isn't listening yet; returns its path.
pub async fn ensure(db: &PgPool, session: Uuid) -> Result<PathBuf> {
    let path = socket_path(session);
    if LISTENING.lock().unwrap_or_else(|e| e.into_inner()).contains(&session) && path.exists() {
        return Ok(path);
    }
    std::fs::create_dir_all(dir())?;
    std::fs::set_permissions(dir(), std::os::unix::fs::PermissionsExt::from_mode(0o700))?;
    let _ = std::fs::remove_file(&path);
    let listener = UnixListener::bind(&path).with_context(|| format!("binding {}", path.display()))?;
    LISTENING.lock().unwrap_or_else(|e| e.into_inner()).insert(session);
    let db = db.clone();
    tokio::spawn(async move {
        loop {
            match listener.accept().await {
                Ok((stream, _)) => {
                    let db = db.clone();
                    tokio::spawn(async move {
                        if let Err(e) = handle(&db, session, stream).await {
                            tracing::debug!("egress proxy ({session}): {e:#}");
                        }
                    });
                }
                Err(e) => {
                    tracing::warn!("egress proxy ({session}) stopped accepting: {e}");
                    LISTENING.lock().unwrap_or_else(|e| e.into_inner()).remove(&session);
                    return;
                }
            }
        }
    });
    Ok(path)
}

/// The highest rating among the hosts the session reached since `since` (None: none reached).
pub fn contacted_since(session: Uuid, since: Instant) -> Option<i16> {
    let map = CONTACTED.lock().unwrap_or_else(|e| e.into_inner());
    map.get(&session)?.iter().filter(|(_, _, at)| *at >= since).map(|(_, r, _)| *r).max()
}

/// What the client asked for: a tunnel (`CONNECT host:port`) or a plain HTTP request.
#[derive(Debug, PartialEq)]
struct Target {
    host: String,
    port: u16,
    tunnel: bool,
    /// For plain HTTP: the request head rewritten to origin form.
    head: Vec<u8>,
}

/// Parse a request head (up to the blank line).
fn parse(head: &[u8]) -> Result<Target, String> {
    let text = std::str::from_utf8(head).map_err(|_| "not an HTTP request".to_string())?;
    let mut lines = text.split("\r\n");
    let first = lines.next().unwrap_or("");
    let mut parts = first.split(' ');
    let (method, target, version) = (parts.next().unwrap_or(""), parts.next().unwrap_or(""), parts.next().unwrap_or(""));
    if !version.starts_with("HTTP/1.") {
        return Err("only HTTP/1.x requests".into());
    }
    let split_port = |hp: &str, default: u16| -> Result<(String, u16), String> {
        let (h, p) = match hp.rsplit_once(':') {
            Some((h, p)) if !h.ends_with(']') || hp.starts_with('[') => (h, p.parse().map_err(|_| format!("bad port in {hp}"))?),
            _ => (hp, default),
        };
        Ok((h.trim_matches(['[', ']']).to_ascii_lowercase(), p))
    };
    if method == "CONNECT" {
        let (host, port) = split_port(target, 443)?;
        return Ok(Target { host, port, tunnel: true, head: Vec::new() });
    }
    let rest = target.strip_prefix("http://").ok_or("only absolute http:// requests (or CONNECT for https)")?;
    let (hostport, path) = rest.split_once('/').map(|(h, p)| (h, format!("/{p}"))).unwrap_or((rest, "/".into()));
    let (host, port) = split_port(hostport, 80)?;
    let mut out = format!("{method} {path} {version}\r\n");
    for l in lines {
        // Hop-by-hop proxy headers stay here.
        if l.to_ascii_lowercase().starts_with("proxy-") {
            continue;
        }
        out.push_str(l);
        out.push_str("\r\n");
    }
    Ok(Target { host, port, tunnel: false, head: out.trim_end_matches("\r\n").to_string().into_bytes().into_iter().chain(*b"\r\n\r\n").collect() })
}

async fn deny(stream: &mut UnixStream, status: &str, why: &str) -> Result<()> {
    let body = format!("{why}\n");
    stream.write_all(format!("HTTP/1.1 {status}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}", body.len()).as_bytes()).await?;
    Ok(())
}

async fn handle(db: &PgPool, session: Uuid, mut client: UnixStream) -> Result<()> {
    // The head, up to 16 KiB; anything read past it is forwarded.
    let mut buf = Vec::with_capacity(4096);
    let mut chunk = [0u8; 4096];
    let end = loop {
        let n = client.read(&mut chunk).await?;
        if n == 0 {
            return Ok(());
        }
        buf.extend_from_slice(&chunk[..n]);
        if let Some(i) = buf.windows(4).position(|w| w == b"\r\n\r\n") {
            break i + 4;
        }
        if buf.len() > 16 * 1024 {
            return deny(&mut client, "431 Request Header Fields Too Large", "request head too large").await;
        }
    };
    let target = match parse(&buf[..end]) {
        Ok(t) => t,
        Err(e) => return deny(&mut client, "400 Bad Request", &e).await,
    };
    // Tests only: e2e reaches a stub on loopback (production never sets this).
    let test_loopback = target.host == "127.0.0.1" && std::env::var_os("ZEN_EGRESS_LOOPBACK_FOR_TESTS").is_some();
    if !matches!(target.port, 80 | 443) && !test_loopback {
        return deny(&mut client, "403 Forbidden", &format!("zenbot's egress proxy: only ports 80 and 443 ({}:{} refused)", target.host, target.port)).await;
    }
    let rating = match crate::gate::egress(db, session, &target.host).await {
        Ok(r) => r,
        Err(why) => return deny(&mut client, "403 Forbidden", &format!("zenbot's egress proxy: {why}")).await,
    };
    let addrs = match if test_loopback { Ok(vec![std::net::SocketAddr::from(([127, 0, 0, 1], target.port))]) } else { crate::web::public_addrs(&target.host, target.port).await } {
        Ok(a) => a,
        Err(e) => return deny(&mut client, "403 Forbidden", &format!("zenbot's egress proxy: {e}")).await,
    };
    let mut upstream = match tokio::time::timeout(std::time::Duration::from_secs(10), TcpStream::connect(&addrs[..])).await {
        Ok(Ok(s)) => s,
        _ => return deny(&mut client, "502 Bad Gateway", &format!("couldn't reach {}:{}", target.host, target.port)).await,
    };
    CONTACTED.lock().unwrap_or_else(|e| e.into_inner()).entry(session).or_default().push((target.host.clone(), rating, Instant::now()));
    if target.tunnel {
        client.write_all(b"HTTP/1.1 200 Connection established\r\n\r\n").await?;
    } else {
        upstream.write_all(&target.head).await?;
    }
    if end < buf.len() {
        upstream.write_all(&buf[end..]).await?;
    }
    let _ = tokio::io::copy_bidirectional(&mut client, &mut upstream).await;
    Ok(())
}

/// `zend bridge <socket> <port> <command…>`: run inside the sandbox. Listens on 127.0.0.1:<port> in
/// the sandbox's network namespace, forwards each connection to the proxy socket, runs the command,
/// and exits with its status when it ends.
pub fn bridge(args: &[String]) -> ! {
    let code = (|| -> Result<i32> {
        let [socket, port, command @ ..] = args else { anyhow::bail!("usage: zend bridge <socket> <port> <command…>") };
        anyhow::ensure!(!command.is_empty(), "usage: zend bridge <socket> <port> <command…>");
        let port: u16 = port.parse()?;
        let socket = PathBuf::from(socket);
        let rt = tokio::runtime::Builder::new_current_thread().enable_all().build()?;
        rt.block_on(async move {
            let listener = tokio::net::TcpListener::bind(("127.0.0.1", port)).await.context("the sandbox's proxy bridge couldn't listen")?;
            let mut child = tokio::process::Command::new(&command[0]).args(&command[1..]).spawn().context("starting the command")?;
            loop {
                tokio::select! {
                    status = child.wait() => return Ok(status?.code().unwrap_or(1)),
                    conn = listener.accept() => {
                        if let Ok((mut tcp, _)) = conn {
                            let socket = socket.clone();
                            tokio::spawn(async move {
                                if let Ok(mut unix) = UnixStream::connect(&socket).await {
                                    let _ = tokio::io::copy_bidirectional(&mut tcp, &mut unix).await;
                                }
                            });
                        }
                    }
                }
            }
        })
    })();
    match code {
        Ok(c) => std::process::exit(c),
        Err(e) => {
            eprintln!("zend bridge: {e:#}");
            std::process::exit(127);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn requests_parse_to_their_host() {
        let t = parse(b"CONNECT crates.io:443 HTTP/1.1\r\nHost: crates.io:443\r\n\r\n").unwrap();
        assert_eq!((t.host.as_str(), t.port, t.tunnel), ("crates.io", 443, true));
        let t = parse(b"GET http://Example.com/a/b?c=1 HTTP/1.1\r\nHost: example.com\r\nProxy-Connection: keep-alive\r\n\r\n").unwrap();
        assert_eq!((t.host.as_str(), t.port, t.tunnel), ("example.com", 80, false));
        let head = String::from_utf8(t.head).unwrap();
        assert!(head.starts_with("GET /a/b?c=1 HTTP/1.1\r\nHost: example.com\r\n"), "{head}");
        assert!(!head.to_lowercase().contains("proxy-connection") && head.ends_with("\r\n\r\n"), "{head}");
        assert!(parse(b"GET /relative HTTP/1.1\r\n\r\n").is_err());
        assert!(parse(b"CONNECT [::1]:443 HTTP/1.1\r\n\r\n").is_ok_and(|t| t.host == "::1"));
        assert!(parse(b"garbage\r\n\r\n").is_err());
    }
}
