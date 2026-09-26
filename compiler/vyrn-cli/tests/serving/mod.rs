//! The wait for a spawned `vyrn serve`, shared by the suites that start one
//! (`rpc`, `serve`, `universal_pages`), and the handle that stops it.
//!
//! The server runs with `--port 0` and names the port the OS gave it in its
//! `serving <file> on http://localhost:<port>` line on stderr. [`drain`] reads
//! each of the child's streams on a thread of its own, so the child never
//! blocks on a full pipe, and [`wait_for_port`] reads the port off what they
//! captured.

use std::io::Read;
use std::process::Child;
use std::sync::{Arc, Mutex};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

/// A running `vyrn serve` child and the port it serves on. Dropping it kills
/// the child, so a test that fails or passes leaves no server behind.
pub struct Server {
    pub child: Child,
    pub port: u16,
}

impl Drop for Server {
    fn drop(&mut self) {
        let _ = self.child.kill();
        let _ = self.child.wait();
    }
}

/// What a stream of the child printed so far, and the thread reading it to its
/// end.
pub type Drained = (Arc<Mutex<String>>, JoinHandle<()>);

/// Read `r` into a buffer on a background thread until it closes.
pub fn drain<R: Read + Send + 'static>(mut r: R) -> Drained {
    let acc = Arc::new(Mutex::new(String::new()));
    let a = acc.clone();
    let reader = std::thread::spawn(move || {
        let mut buf = [0u8; 1024];
        loop {
            match r.read(&mut buf) {
                Ok(0) | Err(_) => break,
                Ok(n) => a
                    .lock()
                    .unwrap()
                    .push_str(&String::from_utf8_lossy(&buf[..n])),
            }
        }
    });
    (acc, reader)
}

/// The port `child` names in its serving banner on `stderr`.
///
/// The whole number must have arrived: a digit run that reaches the end of
/// what was captured could still be half a port, so the wait ends on the
/// character after it.
///
/// # Errors
///
/// A child that exits before its banner fails the wait at once, and one that
/// prints none within `timeout` fails it then. Either error carries everything
/// the child printed on both streams; on an exit the readers are joined first,
/// so the capture is whole.
pub fn wait_for_port(
    child: &mut Child,
    stdout: Drained,
    stderr: Drained,
    timeout: Duration,
) -> Result<u16, String> {
    let start = Instant::now();
    let port = |s: &str| {
        let (_, rest) = s.split_once("http://localhost:")?;
        let (digits, _) = rest.split_once(|c: char| !c.is_ascii_digit())?;
        digits.parse().ok()
    };
    let (out, err) = (stdout.0.clone(), stderr.0.clone());
    let printed = || format!("{}{}", out.lock().unwrap(), err.lock().unwrap());
    let mut readers = Some([stdout.1, stderr.1]);
    loop {
        if let Some(p) = port(&err.lock().unwrap()) {
            return Ok(p);
        }
        if let Ok(Some(status)) = child.try_wait() {
            for r in readers.take().into_iter().flatten() {
                let _ = r.join();
            }
            // The banner may be the last thing it printed before it exited.
            if let Some(p) = port(&err.lock().unwrap()) {
                return Ok(p);
            }
            return Err(format!(
                "the server exited ({status}) before its serving banner:\n{}",
                printed()
            ));
        }
        if start.elapsed() > timeout {
            return Err(format!(
                "timed out waiting for the serving banner; captured so far:\n{}",
                printed()
            ));
        }
        std::thread::sleep(Duration::from_millis(10));
    }
}
