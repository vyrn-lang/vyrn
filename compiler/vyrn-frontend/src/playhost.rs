//! The host boundary for the browser playground, and nothing else.
//!
//! Compiled ONLY for `wasm32-unknown-unknown` — the target `compiler/vyrn-play`
//! builds. Every other target, native or `wasm32-wasip1`, has an operating
//! system underneath it and takes the ordinary paths in [`crate::interp`].
//!
//! A browser tab is not an OS. There is no stdout to write to, no stdin to read
//! from and no clock the module can call. So the three things the interpreter
//! needs from a host live here as plain buffers, and `vyrn-play` fills and drains
//! them around one run:
//!
//!   - **Output.** `print` and a stderr logger append here instead of a file
//!     descriptor. `vyrn-play` reads both back after the run and hands them to
//!     the page as `stdout` and `stderr`, so the two stay separable exactly as
//!     they are in a terminal.
//!   - **Input.** `readLine()` walks the string the page typed into its stdin
//!     box. At its end it reports EOF, which is what a piped file does.
//!   - **The clock.** Sampled ONCE, from `Date.now()`, and passed in before the
//!     run starts. A wasm module has no clock of its own and `std::time` panics
//!     on this target, so the alternative to one sampled value is an import the
//!     page must supply on every call. Time therefore does not advance during a
//!     run; the note on the page says so.
//!
//! The state is thread-local because that is the cheapest correct thing here:
//! this target has exactly one thread, and a `thread_local!` needs no lock and
//! no `unsafe`.

use std::cell::{Cell, RefCell};

thread_local! {
    static OUT: RefCell<String> = const { RefCell::new(String::new()) };
    static ERR: RefCell<String> = const { RefCell::new(String::new()) };
    /// The page's stdin, and how far `readLine` has read into it.
    static IN: RefCell<(Vec<u8>, usize)> = const { RefCell::new((Vec::new(), 0)) };
    static NOW_MS: Cell<i64> = const { Cell::new(0) };
}

/// Arm the host for one run: the stdin it may read, and the wall clock it reads
/// as "now". Clears whatever the previous run wrote.
pub fn arm(stdin: &[u8], now_ms: i64) {
    OUT.with(|o| o.borrow_mut().clear());
    ERR.with(|e| e.borrow_mut().clear());
    IN.with(|i| *i.borrow_mut() = (stdin.to_vec(), 0));
    NOW_MS.with(|n| n.set(now_ms));
}

/// Everything the run wrote: `(stdout, stderr)`.
pub fn drain() -> (String, String) {
    (
        OUT.with(|o| std::mem::take(&mut *o.borrow_mut())),
        ERR.with(|e| std::mem::take(&mut *e.borrow_mut())),
    )
}

/// Text to stdout with NO newline — what `writeStdout` does natively, minus
/// the bytes a web page cannot hold (RFC-0111). The caller has already replaced
/// anything that is not UTF-8; this only appends.
pub fn out_text(text: &str) {
    OUT.with(|o| o.borrow_mut().push_str(text));
}

/// One line to stdout, newline included — what `println!` does natively.
pub fn out_line(args: std::fmt::Arguments) {
    OUT.with(|o| {
        use std::fmt::Write as _;
        let mut b = o.borrow_mut();
        let _ = b.write_fmt(args);
        b.push('\n');
    });
}

/// One line to stderr, newline included.
pub fn err_line(args: std::fmt::Arguments) {
    ERR.with(|e| {
        use std::fmt::Write as _;
        let mut b = e.borrow_mut();
        let _ = b.write_fmt(args);
        b.push('\n');
    });
}

/// The next raw line of stdin — bytes up to and including `\n`, or to the end —
/// or an empty vector at EOF. The same shape `read_until(b'\n', ..)` returns, so
/// the caller's decoding rules (CR stripping, the NUL rule, UTF-8) are unchanged.
pub fn read_line() -> Vec<u8> {
    IN.with(|i| {
        let mut st = i.borrow_mut();
        let (buf, at) = &mut *st;
        if *at >= buf.len() {
            return Vec::new();
        }
        let end = match buf[*at..].iter().position(|&b| b == b'\n') {
            Some(k) => *at + k + 1,
            None => buf.len(),
        };
        let line = buf[*at..end].to_vec();
        *at = end;
        line
    })
}

/// Milliseconds since the Unix epoch, as the page sampled them before the run.
pub fn now_ms() -> i64 {
    NOW_MS.with(|n| n.get())
}
