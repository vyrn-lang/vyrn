//! Outbound HTTP for the embedded engine: the requests a module starts through
//! `vyrn_wait.fetch`. Each runs on a thread of its own and posts its packed
//! reply ([`crate::wire`]) to the event loop, which wakes the module with it
//! ([`crate::wasmrun`]). A cancelled request is aborted and never posted.
//!
//! HTTP/1.1 with one request per connection: `Connection: close`, no
//! redirect followed, no compression asked for. `https://` is TLS through
//! rustls with the `ring` provider, verified against the `webpki-root-certs`
//! roots and no others but the ones the test harness adds; nothing turns the
//! verification off. The reply's headers are the
//! Fetch standard's sort and combine, which a browser's `Headers` iterates:
//! names lower-case and sorted by bytes, the values of one name joined by
//! `, `, and `set-cookie` left out.

use std::io::{BufRead, Read, Write};
use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{mpsc, Arc, Mutex};

/// The requests in flight and the channel their replies come back on.
pub struct Net {
    tx: mpsc::Sender<(i64, Vec<u8>)>,
    rx: mpsc::Receiver<(i64, Vec<u8>)>,
    /// Each request in flight, by its operation's id, in start order.
    flying: Vec<(i64, Arc<Abort>)>,
    tls: Arc<Tls>,
}

/// The TLS client every `https://` request of one run shares, built by the
/// first that needs it.
struct Tls {
    /// DER certificates trusted beside `webpki-root-certs`: only a test harness
    /// adds one ([`crate::wasmrun::Run::test_roots`]).
    extra: Vec<Vec<u8>>,
    config: std::sync::OnceLock<Result<Arc<rustls::ClientConfig>, String>>,
}

impl Tls {
    fn config(&self) -> Result<Arc<rustls::ClientConfig>, Fail> {
        let built = self.config.get_or_init(|| {
            let mut roots = rustls::RootCertStore::empty();
            roots.add_parsable_certificates(
                webpki_root_certs::TLS_SERVER_ROOT_CERTS.iter().cloned(),
            );
            for der in &self.extra {
                roots
                    .add(rustls::pki_types::CertificateDer::from(der.clone()))
                    .map_err(|e| e.to_string())?;
            }
            let provider = Arc::new(rustls::crypto::ring::default_provider());
            let config = rustls::ClientConfig::builder_with_provider(provider)
                .with_safe_default_protocol_versions()
                .map_err(|e| e.to_string())?
                .with_root_certificates(roots)
                .with_no_client_auth();
            Ok(Arc::new(config))
        });
        built.clone().map_err(|_| Fail::Untrusted)
    }
}

/// What a cancellation reaches of a request's thread: the flag it reads
/// before it posts, and the connection, which a shutdown unblocks.
#[derive(Default)]
struct Abort {
    cancelled: AtomicBool,
    stream: Mutex<Option<std::net::TcpStream>>,
}

impl Net {
    /// No request in flight; `test_roots` are trusted beside `webpki-root-certs`.
    pub fn new(test_roots: Vec<Vec<u8>>) -> Self {
        let (tx, rx) = mpsc::channel();
        Net {
            tx,
            rx,
            flying: Vec::new(),
            tls: Arc::new(Tls {
                extra: test_roots,
                config: std::sync::OnceLock::new(),
            }),
        }
    }

    /// Sends the packed `request` as operation `id` on a thread of its own.
    pub fn start(&mut self, id: i64, request: Vec<u8>) {
        let abort = Arc::new(Abort::default());
        self.flying.push((id, Arc::clone(&abort)));
        let (tx, tls) = (self.tx.clone(), Arc::clone(&self.tls));
        std::thread::spawn(move || {
            let reply = send(&request, &abort, &tls);
            if !abort.cancelled.load(Ordering::SeqCst) {
                let _ = tx.send((id, reply));
            }
        });
    }

    /// Aborts request `id`: its reply is never posted.
    pub fn cancel(&mut self, id: i64) {
        let Some(at) = self.flying.iter().position(|f| f.0 == id) else {
            return;
        };
        let (_, abort) = self.flying.remove(at);
        abort.cancelled.store(true, Ordering::SeqCst);
        let stream = vyrn_frontend::session::locked(&abort.stream);
        if let Some(s) = stream.as_ref() {
            let _ = s.shutdown(std::net::Shutdown::Both);
        }
    }

    /// The next reply, waited for at most `left`, or until one comes when
    /// `left` is `None`; `None` when the time ran out or nothing is in flight.
    /// `first` waits for the earliest request in start order alone, the
    /// virtual clock's rule.
    pub fn next(
        &mut self,
        left: Option<std::time::Duration>,
        first: bool,
    ) -> Option<(i64, Vec<u8>)> {
        let mut early: Vec<(i64, Vec<u8>)> = Vec::new();
        let got = loop {
            if first {
                let want = self.flying.first()?.0;
                if let Some(at) = early.iter().position(|e| e.0 == want) {
                    break Some(early.remove(at));
                }
            } else if self.flying.is_empty() {
                break None;
            }
            let r = match left {
                Some(d) if !first => self.rx.recv_timeout(d).ok(),
                _ => self.rx.recv().ok(),
            };
            let Some((id, reply)) = r else { break None };
            if !self.flying.iter().any(|f| f.0 == id) {
                continue;
            }
            if first {
                early.push((id, reply));
                continue;
            }
            break Some((id, reply));
        };
        // A reply that came before the one waited for goes back for the next call.
        for e in early {
            let _ = self.tx.send(e);
        }
        let (id, reply) = got?;
        self.flying.retain(|f| f.0 != id);
        Some((id, reply))
    }
}

/// A reply with no response: the outcome alone.
fn failed(outcome: &str) -> Vec<u8> {
    let mut out = Vec::new();
    crate::wire::push(&mut out, outcome.as_bytes());
    out
}

/// Why a request has no response, as the reply's outcome names it.
#[derive(Debug, PartialEq)]
enum Fail {
    BadUrl,
    Unreachable,
    Untrusted,
    BadResponse,
}

impl Fail {
    fn outcome(&self) -> &'static str {
        match self {
            Fail::BadUrl => "badUrl",
            Fail::Unreachable => "unreachable",
            Fail::Untrusted => "untrusted",
            Fail::BadResponse => "badResponse",
        }
    }
}

impl From<std::io::Error> for Fail {
    fn from(_: std::io::Error) -> Self {
        Fail::Unreachable
    }
}

/// Sends the packed `request` and returns the packed reply.
fn send(request: &[u8], abort: &Abort, tls: &Tls) -> Vec<u8> {
    match exchange(request, abort, tls) {
        Ok(reply) => reply,
        Err(f) => failed(f.outcome()),
    }
}

/// Where a request goes: whether over TLS, the host as the URL spells it, the
/// port, and the request target.
#[derive(Debug, PartialEq)]
struct Url<'a> {
    tls: bool,
    host: &'a str,
    port: u16,
    /// The `Host` header: the host, and the port when it is not the default.
    authority: String,
    target: String,
}

/// Reads an `http://` or `https://` URL. A fragment is dropped; userinfo, an
/// empty host and a port that is not a number are refused.
fn parse_url(url: &str) -> Result<Url<'_>, Fail> {
    let (tls, rest) = match (url.strip_prefix("https://"), url.strip_prefix("http://")) {
        (Some(rest), _) => (true, rest),
        (None, Some(rest)) => (false, rest),
        (None, None) => return Err(Fail::BadUrl),
    };
    let default = if tls { 443 } else { 80 };
    let rest = rest.split('#').next().unwrap_or_default();
    let end = rest.find(['/', '?']).unwrap_or(rest.len());
    let (auth, path) = rest.split_at(end);
    if auth.contains('@') {
        return Err(Fail::BadUrl);
    }
    let (host, port) = match auth.rfind(':') {
        Some(c) if !auth[c..].contains(']') => (&auth[..c], Some(&auth[c + 1..])),
        _ => (auth, None),
    };
    let port = match port {
        Some(p) => number(p, 10)
            .and_then(|p| u16::try_from(p).ok())
            .ok_or(Fail::BadUrl)?,
        None => default,
    };
    let bare = host.trim_start_matches('[').trim_end_matches(']');
    if bare.is_empty() || bare.len() + 2 < host.len() {
        return Err(Fail::BadUrl);
    }
    let authority = match port == default {
        true => host.to_string(),
        false => format!("{host}:{port}"),
    };
    let target = match path.starts_with('/') {
        true => path.to_string(),
        false => format!("/{path}"),
    };
    Ok(Url {
        tls,
        host: bare,
        port,
        authority,
        target,
    })
}

/// A connection a request is sent and read over: a socket, or TLS over one.
trait Conn: Read + Write + Send {}
impl<T: Read + Write + Send> Conn for T {}

/// TLS over `tcp` to `host`, its handshake done: a certificate that does not
/// verify is `Untrusted`, any other failure `Unreachable`.
fn secure(tcp: std::net::TcpStream, host: &str, tls: &Tls) -> Result<Box<dyn Conn>, Fail> {
    let name =
        rustls::pki_types::ServerName::try_from(host.to_string()).map_err(|_| Fail::BadUrl)?;
    let conn = rustls::ClientConnection::new(tls.config()?, name).map_err(|_| Fail::Unreachable)?;
    let mut s = rustls::StreamOwned::new(conn, tcp);
    while s.conn.is_handshaking() {
        if let Err(e) = s.conn.complete_io(&mut s.sock) {
            let cert = e.get_ref().and_then(|i| i.downcast_ref::<rustls::Error>());
            return Err(match cert {
                Some(
                    rustls::Error::InvalidCertificate(_) | rustls::Error::NoCertificatesPresented,
                ) => Fail::Untrusted,
                _ => Fail::Unreachable,
            });
        }
    }
    Ok(Box::new(s))
}

/// One request and its response over a connection of its own.
fn exchange(request: &[u8], abort: &Abort, tls: &Tls) -> Result<Vec<u8>, Fail> {
    let f = crate::wire::fields(request).ok_or(Fail::BadUrl)?;
    let text = |b: &[u8]| {
        std::str::from_utf8(b)
            .map_err(|_| Fail::BadUrl)
            .map(str::to_string)
    };
    let (method, url) = (
        text(f.first().ok_or(Fail::BadUrl)?)?,
        text(f.get(1).ok_or(Fail::BadUrl)?)?,
    );
    let n = number(&text(f.get(2).ok_or(Fail::BadUrl)?)?, 10).ok_or(Fail::BadUrl)?;
    if n.checked_mul(2).and_then(|k| k.checked_add(4)) != Some(f.len()) {
        return Err(Fail::BadUrl);
    }
    let url = parse_url(&url)?;
    let tcp = std::net::TcpStream::connect((url.host, url.port))?;
    *vyrn_frontend::session::locked(&abort.stream) = Some(tcp.try_clone()?);
    if abort.cancelled.load(Ordering::SeqCst) {
        return Err(Fail::Unreachable);
    }
    let mut conn: Box<dyn Conn> = match url.tls {
        true => secure(tcp, url.host, tls)?,
        false => Box::new(tcp),
    };
    let mut head = format!(
        "{method} {} HTTP/1.1\r\nHost: {}\r\n",
        url.target, url.authority
    );
    for i in 0..n {
        let (k, v) = (text(f[3 + 2 * i])?, text(f[4 + 2 * i])?);
        head.push_str(&format!("{k}: {v}\r\n"));
    }
    let body = f[3 + 2 * n];
    if !body.is_empty() {
        head.push_str(&format!("Content-Length: {}\r\n", body.len()));
    }
    head.push_str("Connection: close\r\n\r\n");
    conn.write_all(head.as_bytes())?;
    conn.write_all(body)?;
    conn.flush()?;
    read_response(&mut std::io::BufReader::new(conn), method == "HEAD")
}

/// Reads one response and packs it as a reply: `ok`, the status, the header
/// count, each header's name and value, the body. Interim 1xx responses are
/// skipped. `head` says the request was `HEAD`, whose response has no body.
fn read_response(r: &mut impl BufRead, head: bool) -> Result<Vec<u8>, Fail> {
    let (status, headers) = loop {
        let line = read_line(r)?;
        let mut parts = line.splitn(3, ' ');
        let version = parts.next().unwrap_or_default();
        let code = parts.next().unwrap_or_default();
        if !version.starts_with("HTTP/1.") || code.len() != 3 {
            return Err(Fail::BadResponse);
        }
        let status = number(code, 10).ok_or(Fail::BadResponse)?;
        let mut headers: Vec<(String, String)> = Vec::new();
        loop {
            let line = read_line(r)?;
            if line.is_empty() {
                break;
            }
            let (k, v) = line.split_once(':').ok_or(Fail::BadResponse)?;
            if k.is_empty() || k.contains([' ', '\t']) {
                return Err(Fail::BadResponse);
            }
            headers.push((
                k.to_ascii_lowercase(),
                v.trim_matches([' ', '\t']).to_string(),
            ));
        }
        if !(100..200).contains(&status) || status == 101 {
            break (status, headers);
        }
    };
    let get = |name: &str| headers.iter().find(|h| h.0 == name).map(|h| h.1.as_str());
    let body = if head || status == 204 || status == 304 {
        Vec::new()
    } else if get("transfer-encoding").is_some_and(|t| t.eq_ignore_ascii_case("chunked")) {
        read_chunked(r)?
    } else if let Some(len) = get("content-length") {
        let len = number(len, 10).ok_or(Fail::BadResponse)?;
        let mut body = Vec::new();
        read_exact(r, &mut body, len)?;
        body
    } else {
        let mut body = Vec::new();
        r.read_to_end(&mut body)?;
        body
    };
    if std::str::from_utf8(&body).is_err() {
        return Err(Fail::BadResponse);
    }
    Ok(pack_reply(status, headers, &body))
}

/// The reply for a response: `ok`, the status, the headers sorted and
/// combined as the module's doc says, the body.
fn pack_reply(status: usize, mut headers: Vec<(String, String)>, body: &[u8]) -> Vec<u8> {
    headers.retain(|h| h.0 != "set-cookie");
    headers.sort_by(|a, b| a.0.cmp(&b.0));
    let mut combined: Vec<(String, String)> = Vec::new();
    for (k, v) in headers {
        match combined.last_mut() {
            Some(last) if last.0 == k => {
                last.1.push_str(", ");
                last.1.push_str(&v);
            }
            _ => combined.push((k, v)),
        }
    }
    let mut out = Vec::new();
    crate::wire::push(&mut out, b"ok");
    crate::wire::push(&mut out, status.to_string().as_bytes());
    crate::wire::push(&mut out, combined.len().to_string().as_bytes());
    for (k, v) in &combined {
        crate::wire::push(&mut out, k.as_bytes());
        crate::wire::push(&mut out, v.as_bytes());
    }
    crate::wire::push(&mut out, body);
    out
}

/// Appends the next `n` bytes to `out`, which grows as they come rather than
/// by what the peer claims; an end of the stream first is an unreachable host.
fn read_exact(r: &mut impl BufRead, out: &mut Vec<u8>, n: usize) -> Result<(), Fail> {
    let want = n as u64;
    if r.by_ref().take(want).read_to_end(out)? as u64 != want {
        return Err(Fail::Unreachable);
    }
    Ok(())
}

/// A number of digits in `radix` and nothing else, no sign or space; `None`
/// past `usize`.
fn number(s: &str, radix: u32) -> Option<usize> {
    if s.is_empty() || !s.chars().all(|c| c.is_digit(radix)) {
        return None;
    }
    usize::from_str_radix(s, radix).ok()
}

/// One line without its CRLF or LF. A line that is not UTF-8 is a bad
/// response; an end of the stream before it ends is an unreachable host.
fn read_line(r: &mut impl BufRead) -> Result<String, Fail> {
    let mut line = Vec::new();
    r.read_until(b'\n', &mut line)?;
    if line.pop() != Some(b'\n') {
        return Err(Fail::Unreachable);
    }
    if line.last() == Some(&b'\r') {
        line.pop();
    }
    String::from_utf8(line).map_err(|_| Fail::BadResponse)
}

/// A `chunked` body: each chunk's hexadecimal size line and its bytes, up to
/// the empty chunk and the trailer section, which is skipped.
fn read_chunked(r: &mut impl BufRead) -> Result<Vec<u8>, Fail> {
    let mut body = Vec::new();
    loop {
        let line = read_line(r)?;
        let size = line.split(';').next().unwrap_or_default();
        let n = number(size.trim_matches([' ', '\t']), 16).ok_or(Fail::BadResponse)?;
        if n == 0 {
            while !read_line(r)?.is_empty() {}
            return Ok(body);
        }
        read_exact(r, &mut body, n)?;
        if !read_line(r)?.is_empty() {
            return Err(Fail::BadResponse);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn a_url_names_its_host_port_and_target() {
        let u = parse_url("http://127.0.0.1:8080/a?b=1#c").expect("a URL");
        assert_eq!(
            (u.host, u.port, u.authority.as_str(), u.target.as_str()),
            ("127.0.0.1", 8080, "127.0.0.1:8080", "/a?b=1")
        );
        let u = parse_url("http://example.com?q").expect("a URL");
        assert_eq!(
            (u.port, u.authority.as_str(), u.target.as_str()),
            (80, "example.com", "/?q")
        );
        let u = parse_url("http://[::1]:9/").expect("a URL");
        assert_eq!(
            (u.host, u.port, u.authority.as_str()),
            ("::1", 9, "[::1]:9")
        );
        let u = parse_url("https://127.0.0.1/").expect("a URL");
        assert_eq!(
            (u.tls, u.port, u.authority.as_str()),
            (true, 443, "127.0.0.1")
        );
        for bad in [
            "ftp://x/",
            "http://",
            "http://u@x/",
            "http://x:y/",
            "http://x:70000/",
            "http://[]/",
        ] {
            assert_eq!(parse_url(bad), Err(Fail::BadUrl), "{bad}");
        }
    }

    #[test]
    fn a_response_is_read_by_length_by_chunks_and_to_the_close() {
        let read = |raw: &[u8]| read_response(&mut &raw[..], false);
        let got = read(b"HTTP/1.1 200 OK\r\nX-A: 1\r\nContent-Length: 5\r\n\r\nhello!");
        let want = pack_reply(
            200,
            vec![
                ("content-length".into(), "5".into()),
                ("x-a".into(), "1".into()),
            ],
            b"hello",
        );
        assert_eq!(got, Ok(want));
        let chunked = "HTTP/1.1 100 Continue\r\n\r\nHTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\n\r\n3\r\nhel\r\n2;x\r\nlo\r\n0\r\nT: 1\r\n\r\n";
        let got = read(chunked.as_bytes()).expect("a response");
        let f = crate::wire::fields(&got).expect("fields");
        assert_eq!(
            f[..5],
            [&b"ok"[..], b"200", b"1", b"transfer-encoding", b"chunked"]
        );
        assert_eq!(f[5], b"hello");
        let closed =
            read(b"HTTP/1.0 404 Nope\nB: 2\nb: 3\nA: 1\nSet-Cookie: x\n\nbye").expect("a response");
        let f = crate::wire::fields(&closed).expect("fields");
        assert_eq!(
            f,
            [&b"ok"[..], b"404", b"2", b"a", b"1", b"b", b"2, 3", b"bye"]
        );
        assert_eq!(
            read(b"HTTP/1.1 200 OK\r\nContent-Length: 9\r\n\r\nshort"),
            Err(Fail::Unreachable)
        );
        assert_eq!(read(b"SMTP 220\r\n\r\n"), Err(Fail::BadResponse));
        assert_eq!(read(b"HTTP/1.1 200 OK\r\n\r\n\xff"), Err(Fail::BadResponse));
    }
}
