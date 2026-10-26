//! Outbound HTTP (`std/fetch`): `tests/fetch/roundtrip.vyrn` against a server this test
//! starts on a port the system picks. Every host prints the same bytes; the free audit finds
//! nothing left after a fetch that completes, one that fails and one a `timeout` cancels;
//! the cancelled request is aborted on the host; a replay of the recorded deliveries sends no
//! request and prints the same bytes.

mod common;
use common::*;
use std::io::{BufRead, BufReader, Read, Write};
use std::net::{TcpListener, TcpStream};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{mpsc, Arc, Mutex};
use std::time::Duration;
use vyrn_cli::wasmrun::{run, Outcome, Run, Wake};

/// The server the program fetches from: its port, the requests it took, and a message per
/// `/slow` request whose client left before the answer.
struct Upstream {
    port: u16,
    hits: Arc<AtomicUsize>,
    aborted: mpsc::Receiver<()>,
}

fn upstream() -> Upstream {
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a port");
    let port = listener.local_addr().expect("the bound port").port();
    let hits = Arc::new(AtomicUsize::new(0));
    let (tx, aborted) = mpsc::channel();
    let tx = Arc::new(Mutex::new(tx));
    let counted = Arc::clone(&hits);
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            counted.fetch_add(1, Ordering::SeqCst);
            let tx = Arc::clone(&tx);
            std::thread::spawn(move || answer(s, &tx));
        }
    });
    Upstream {
        port,
        hits,
        aborted,
    }
}

/// Answers one request by its target. `/slow` answers after three seconds, unless its client
/// leaves first, which it reports on `aborted`.
fn answer(s: TcpStream, aborted: &Mutex<mpsc::Sender<()>>) {
    let mut r = BufReader::new(s.try_clone().expect("clone the stream"));
    let mut line = String::new();
    if r.read_line(&mut line).is_err() {
        return;
    }
    let mut parts = line.split_whitespace();
    let method = parts.next().unwrap_or_default().to_string();
    let target = parts.next().unwrap_or_default().to_string();
    let (mut echo, mut len) = (String::new(), 0usize);
    loop {
        let mut h = String::new();
        if r.read_line(&mut h).unwrap_or(0) == 0 {
            return;
        }
        let h = h.trim_end();
        if h.is_empty() {
            break;
        }
        if let Some((k, v)) = h.split_once(':') {
            match k.to_ascii_lowercase().as_str() {
                "x-echo" => echo = v.trim().to_string(),
                "content-length" => len = v.trim().parse().unwrap_or(0),
                _ => {}
            }
        }
    }
    let mut body = vec![0; len];
    if r.read_exact(&mut body).is_err() {
        return;
    }
    let reply: Vec<u8> = match target.as_str() {
        "/echo?q=1" => {
            let b = format!(
                "{method} {target} x-echo={echo} body={}",
                String::from_utf8_lossy(&body)
            );
            format!(
                "HTTP/1.1 201 Created\r\nContent-Type: text/plain\r\nVary: x-echo\r\nX-Two: b\r\n\
                 X-One: 1\r\nx-two: a\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{b}",
                b.len()
            )
            .into_bytes()
        }
        "/chunked" => b"HTTP/1.1 200 OK\r\nTransfer-Encoding: chunked\r\nConnection: close\r\n\r\n\
                        3\r\nabc\r\n2\r\nde\r\n0\r\n\r\n"
            .to_vec(),
        "/latin" => {
            b"HTTP/1.1 200 OK\r\nContent-Length: 1\r\nConnection: close\r\n\r\n\xff".to_vec()
        }
        "/slow" => {
            let _ = r.get_ref().set_read_timeout(Some(Duration::from_secs(3)));
            let mut b = [0u8; 1];
            match r.read(&mut b) {
                Ok(0) => {
                    let _ = aborted.lock().expect("the sender").send(());
                    return;
                }
                Err(e)
                    if !matches!(
                        e.kind(),
                        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                    ) =>
                {
                    let _ = aborted.lock().expect("the sender").send(());
                    return;
                }
                _ => b"HTTP/1.1 200 OK\r\nContent-Length: 4\r\nConnection: close\r\n\r\nlate"
                    .to_vec(),
            }
        }
        _ => b"HTTP/1.1 404 Not Found\r\nContent-Length: 0\r\nConnection: close\r\n\r\n".to_vec(),
    };
    let mut w = &s;
    let _ = w.write_all(&reply);
}

/// A port nothing listens on: bound, read, and released.
fn closed_port() -> u16 {
    let l = TcpListener::bind("127.0.0.1:0").expect("bind a port");
    l.local_addr().expect("the bound port").port()
}

/// `tests/fetch/roundtrip.vyrn` with its two placeholder ports replaced, in `dir`.
fn program(dir: &std::path::Path, up: &Upstream) -> std::path::PathBuf {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fetch/roundtrip.vyrn"),
    )
    .expect("the program");
    let src = src
        .replace(
            "\"http://127.0.0.1:1\"",
            &format!("\"http://127.0.0.1:{}\"", up.port),
        )
        .replace(
            "\"http://127.0.0.1:2\"",
            &format!("\"http://127.0.0.1:{}\"", closed_port()),
        );
    let path = dir.join("roundtrip.vyrn");
    std::fs::write(&path, src).expect("write the program");
    path
}

/// What every host prints.
const PRINTED: &str = "201 text/plain vary=x-echo\n\
connection: close\n\
content-length: 40\n\
x-one: 1\n\
x-two: b, a\n\
POST /echo?q=1 x-echo=seven body=payload\n\
body: abcde\n\
error: a bad response\n\
error: unreachable\n\
error: a bad URL\n\
slow: timed out\n";

fn say(o: &std::process::Output) -> String {
    format!(
        "exit {:?}\n{}{}",
        o.status.code(),
        norm(&o.stdout),
        norm(&o.stderr)
    )
}

/// Ends the test binary with a failure once `secs` have passed, so a run that never settles
/// fails in seconds rather than at the suite's timeout. Each test arms it first.
fn deadline(test: &'static str, secs: u64) {
    std::thread::spawn(move || {
        std::thread::sleep(Duration::from_secs(secs));
        eprintln!("{test}: still running after {secs} s");
        std::process::exit(101);
    });
}

/// The `/slow` request each host ran was aborted where the `timeout` cancelled it.
fn aborted(up: &Upstream, host: &str) {
    let got = up.aborted.recv_timeout(Duration::from_secs(10));
    assert!(got.is_ok(), "{host}: the slow request was not aborted");
}

#[test]
fn a_round_trip_prints_the_same_bytes_on_every_host() {
    deadline("a_round_trip_prints_the_same_bytes_on_every_host", 240);
    let up = upstream();
    let dir = scratch("fetch");
    let src = program(&dir, &up);
    let engine = vyrn()
        .arg("run")
        .arg(&src)
        .env("VYRN_LEAK_CHECK", "1")
        .output()
        .expect("run vyrn run");
    assert_eq!(say(&engine), format!("exit Some(0)\n{PRINTED}"), "vyrn run");
    aborted(&up, "vyrn run");
    let exe = dir.join("roundtrip.exe");
    let built = vyrn()
        .arg("build")
        .arg(&src)
        .arg("-o")
        .arg(&exe)
        .output()
        .expect("run vyrn build");
    assert!(built.status.success(), "{}", norm(&built.stderr));
    let native = std::process::Command::new(&exe)
        .output()
        .expect("run the binary");
    assert_eq!(say(&native), say(&engine), "native");
    aborted(&up, "native");
    let Some(node) = node() else {
        eprintln!("NOTE: no node, so the shim's round trip is unverified on this machine");
        return;
    };
    let wasm = dir.join("roundtrip.wasm");
    let built = vyrn()
        .args(["build", "--target", "wasm"])
        .arg(&src)
        .arg("-o")
        .arg(&wasm)
        .output()
        .expect("run vyrn build");
    assert!(built.status.success(), "{}", norm(&built.stderr));
    let web = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../web/wasi-min.js");
    std::fs::copy(web, dir.join("wasi-min.mjs")).expect("copy the shim");
    std::fs::write(dir.join("run.mjs"), SHIM_RUN).expect("write the runner");
    let shim = std::process::Command::new(node)
        .arg(dir.join("run.mjs"))
        .arg(&wasm)
        .output()
        .expect("node");
    assert_eq!(say(&shim), say(&engine), "shim");
    aborted(&up, "shim");
}

const SHIM_RUN: &str = r#"import { readFile } from "node:fs/promises";
import { runVyrn } from "./wasi-min.mjs";

const r = await runVyrn(await readFile(process.argv[2]), {});
process.stdout.write(r.stdout);
process.stderr.write(r.stderr);
process.exitCode = r.exitCode;
"#;

/// The audited module of the program, built against `up`.
fn audited(dir: &std::path::Path, up: &Upstream) -> Vec<u8> {
    let src = program(dir, up);
    let wasm = dir.join("audited.wasm");
    let out = vyrn()
        .args(["build", "--target", "wasm"])
        .arg(&src)
        .arg("-o")
        .arg(&wasm)
        .env("VYRN_LEAK_CHECK", "1")
        .output()
        .expect("run vyrn build");
    assert!(out.status.success(), "{}", norm(&out.stderr));
    std::fs::read(&wasm).expect("the built module")
}

fn quiet(virtual_clock: bool, replay: Option<Vec<Wake>>) -> Run {
    Run {
        argv: vec!["roundtrip.vyrn".to_string()],
        capture_stdout: true,
        capture_stderr: true,
        virtual_clock,
        replay,
        ..Default::default()
    }
}

fn seen(o: &Outcome) -> (i32, String, String) {
    (o.code, norm(&o.stdout), norm(&o.stderr))
}

/// The replay gate: what the host delivered, replies included, replayed with no request sent,
/// prints the same bytes. Each delivery the host cancels instead ends `main` there with exit 1
/// and leaves nothing for the free audit.
#[test]
fn a_replay_of_the_recorded_replies_prints_the_same_bytes() {
    deadline(
        "a_replay_of_the_recorded_replies_prints_the_same_bytes",
        120,
    );
    let up = upstream();
    let dir = scratch("fetch-replay");
    let bytes = audited(&dir, &up);
    let first = run(&bytes, quiet(false, None)).expect("the module runs");
    assert_eq!(seen(&first), (0, PRINTED.to_string(), String::new()));
    aborted(&up, "the recorded run");
    let sent = up.hits.load(Ordering::SeqCst);
    let again = run(&bytes, quiet(true, Some(first.wakes.clone()))).expect("the module runs");
    assert_eq!(seen(&again), seen(&first));
    assert_eq!(again.wakes, first.wakes);
    assert_eq!(
        up.hits.load(Ordering::SeqCst),
        sent,
        "a replay sent a request"
    );
    for k in 0..first.wakes.len() {
        let mut wakes = first.wakes[..=k].to_vec();
        wakes[k].status = 1;
        wakes[k].reply = None;
        let out = run(&bytes, quiet(true, Some(wakes))).expect("the module runs");
        let printed = norm(&out.stdout);
        assert!(PRINTED.starts_with(&printed), "wake {k}: {printed}");
        assert_eq!(
            (out.code, norm(&out.stderr)),
            (1, String::new()),
            "wake {k}"
        );
    }
}

/// On the virtual clock a request takes no time: the slow reply comes before the `timeout`'s
/// timer, whatever its delay.
#[test]
fn a_request_takes_no_time_on_the_virtual_clock() {
    deadline("a_request_takes_no_time_on_the_virtual_clock", 60);
    let up = upstream();
    let dir = scratch("fetch-virtual");
    let bytes = audited(&dir, &up);
    let out = run(&bytes, quiet(true, None)).expect("the module runs");
    let want = PRINTED.replace("slow: timed out", "body: late");
    assert_eq!(seen(&out), (0, want, String::new()));
}

/// The fetch tests' TLS root and the certificate and key it signs for `127.0.0.1`, as DER.
const ROOT: &[u8] = include_bytes!("fetch/root.der");
const SERVER_CERT: &[u8] = include_bytes!("fetch/server.der");
const SERVER_KEY: &[u8] = include_bytes!("fetch/server.key.der");

/// A TLS server that answers every request with `secure hello`; its port.
fn tls_upstream() -> u16 {
    use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
    let provider = std::sync::Arc::new(rustls::crypto::ring::default_provider());
    let config = rustls::ServerConfig::builder_with_provider(provider)
        .with_safe_default_protocol_versions()
        .expect("the provider's versions")
        .with_no_client_auth()
        .with_single_cert(
            vec![CertificateDer::from(SERVER_CERT.to_vec())],
            PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(SERVER_KEY.to_vec())),
        )
        .expect("the test certificate");
    let config = std::sync::Arc::new(config);
    let listener = TcpListener::bind("127.0.0.1:0").expect("bind a port");
    let port = listener.local_addr().expect("the bound port").port();
    std::thread::spawn(move || {
        for s in listener.incoming().flatten() {
            let config = std::sync::Arc::clone(&config);
            std::thread::spawn(move || {
                let conn = rustls::ServerConnection::new(config).expect("a TLS session");
                let mut tls = rustls::StreamOwned::new(conn, s);
                let mut head = Vec::new();
                let mut b = [0u8; 1];
                while !head.ends_with(b"\r\n\r\n") {
                    match tls.read(&mut b) {
                        Ok(1) => head.push(b[0]),
                        _ => return,
                    }
                }
                let reply = b"HTTP/1.1 200 OK\r\nContent-Length: 12\r\nConnection: close\r\n\r\nsecure hello";
                let _ = tls.write_all(reply);
                tls.conn.send_close_notify();
                let _ = tls.flush();
            });
        }
    });
    port
}

/// `tests/fetch/tls.vyrn`, audited, against the TLS server on `port`.
fn tls_module(dir: &std::path::Path, port: u16) -> Vec<u8> {
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fetch/tls.vyrn"),
    )
    .expect("the program")
    .replace(
        "\"https://127.0.0.1:1\"",
        &format!("\"https://127.0.0.1:{port}\""),
    );
    let path = dir.join("tls.vyrn");
    std::fs::write(&path, src).expect("write the program");
    let wasm = dir.join("tls.wasm");
    let out = vyrn()
        .args(["build", "--target", "wasm"])
        .arg(&path)
        .arg("-o")
        .arg(&wasm)
        .env("VYRN_LEAK_CHECK", "1")
        .output()
        .expect("run vyrn build");
    assert!(out.status.success(), "{}", norm(&out.stderr));
    std::fs::read(&wasm).expect("the built module")
}

/// HTTPS verifies the server: with the test's root the request answers, and without it,
/// as the `vyrn` binary runs every program, the certificate does not verify.
#[test]
fn an_https_request_answers_only_when_its_certificate_verifies() {
    deadline(
        "an_https_request_answers_only_when_its_certificate_verifies",
        60,
    );
    let port = tls_upstream();
    let dir = scratch("fetch-tls");
    let bytes = tls_module(&dir, port);
    let trusted = Run {
        test_roots: vec![ROOT.to_vec()],
        ..quiet(false, None)
    };
    let out = run(&bytes, trusted).expect("the module runs");
    assert_eq!(
        seen(&out),
        (0, "body: secure hello\n".into(), String::new())
    );
    let out = run(&bytes, quiet(false, None)).expect("the module runs");
    assert_eq!(seen(&out), (0, "error: untrusted\n".into(), String::new()));
}

/// A native build verifies as the embedded engine does: built with the test's root
/// (`VYRN_TEST_TLS_ROOT`, which the build announces) it prints what `vyrn run` prints with
/// that root, and built without it the certificate does not verify.
#[test]
fn a_native_https_request_answers_as_the_engine_does() {
    deadline("a_native_https_request_answers_as_the_engine_does", 240);
    let port = tls_upstream();
    let dir = scratch("fetch-tls-native");
    let src = std::fs::read_to_string(
        std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fetch/tls.vyrn"),
    )
    .expect("the program")
    .replace(
        "\"https://127.0.0.1:1\"",
        &format!("\"https://127.0.0.1:{port}\""),
    );
    let path = dir.join("tls.vyrn");
    std::fs::write(&path, src).expect("write the program");
    let root = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("tests/fetch/root.pem");
    let build = |exe: &std::path::Path, root: Option<&std::path::Path>| {
        let mut cmd = vyrn();
        cmd.arg("build").arg(&path).arg("-o").arg(exe);
        match root {
            Some(r) => cmd.env("VYRN_TEST_TLS_ROOT", r),
            None => cmd.env_remove("VYRN_TEST_TLS_ROOT"),
        };
        let out = cmd.output().expect("run vyrn build");
        assert!(out.status.success(), "{}", norm(&out.stderr));
        norm(&out.stderr)
    };
    let trusted = dir.join("trusted.exe");
    let said = build(&trusted, Some(&root));
    assert!(
        said.contains("trusts an extra TLS root from VYRN_TEST_TLS_ROOT"),
        "{said}"
    );
    let out = std::process::Command::new(&trusted)
        .output()
        .expect("run the binary");
    assert_eq!(say(&out), "exit Some(0)\nbody: secure hello\n");
    let plain = dir.join("plain.exe");
    assert_eq!(build(&plain, None), "");
    let out = std::process::Command::new(&plain)
        .output()
        .expect("run the binary");
    assert_eq!(say(&out), "exit Some(0)\nerror: untrusted\n");
}
