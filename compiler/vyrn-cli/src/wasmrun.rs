//! `vyrn run --engine wasm` (RFC-0125 §2.5, M5): the program's own wasm, run in
//! this process by the embedded wasmtime.
//!
//! The module is what `vyrn build --target wasm` writes, byte for byte. This
//! file is the WASI host it runs under: the fifteen `wasi_snapshot_preview1`
//! imports `vyrn_codegen::direct` declares, and nothing else. An RFC-0012
//! `extern` import is answered with the one refusal a terminal owes it (see
//! [`run`]); any other import traps, so a module that reaches past this list
//! says so instead of running wrong.
//!
//! Hand-written, not `wasmtime-wasi`, for the reason RFC-0076 gave when it wrote
//! the generator engine's shim and `web/wasi-min.js` before that: the surface is
//! fifteen calls, and the crate would bring an async runtime for a program that
//! reads a line and writes a file. The parity harness's wasm column is the
//! `wasmtime` CLI; the fixtures gate (`tests/fixtures.rs`) is what says this host
//! and that one agree.
//!
//! What the host sets up matches the harness's `wasmtime run --dir . --env ..`
//! line: argv is the module's path and the program's arguments, the environment
//! is this process's, standard input, output and error pass through, and the
//! working directory is the one preopened directory (fd 3).

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use wasmtime::{Caller, Engine, Linker, Memory, Module, Store};

/// What one run produced. The exit code is `proc_exit`'s argument, or 1 when
/// the module trapped.
pub struct Outcome {
    pub code: i32,
    /// Standard error, when the caller asked for it to be captured; otherwise
    /// empty and already written through.
    pub stderr: Vec<u8>,
}

/// One run's inputs beyond the module.
pub struct Run {
    /// `argv[0]` is the program's name; the rest is `args()`.
    pub argv: Vec<String>,
    /// Bytes standard input serves BEFORE this process's own — the test
    /// harness hands the guest its body's index this way.
    pub stdin_prefix: Vec<u8>,
    /// Keep the guest's standard error in [`Outcome::stderr`] instead of
    /// writing it through — the test harness reads a trap's message out of it.
    pub capture_stderr: bool,
}

// WASI preview1 errno values, by name.
const SUCCESS: i32 = 0;
const ACCES: i32 = 2;
const BADF: i32 = 8;
const EXIST: i32 = 20;
const IO: i32 = 29;
const ISDIR: i32 = 31;
const NOENT: i32 = 44;
const NOTDIR: i32 = 54;
const NOTCAPABLE: i32 = 76;

// `path_open` bits, from the witx.
const OFLAGS_CREAT: i32 = 1;
const OFLAGS_DIRECTORY: i32 = 2;
const OFLAGS_EXCL: i32 = 4;
const OFLAGS_TRUNC: i32 = 8;
const RIGHT_FD_READ: i64 = 1 << 1;
const RIGHT_FD_WRITE: i64 = 1 << 6;
const FDFLAGS_APPEND: i32 = 1;

/// The preopened directory: the working directory, as fd 3, named `.`.
const PREOPEN_FD: i32 = 3;

// `filetype` values, from the witx.
const FILETYPE_UNKNOWN: u8 = 0;
const FILETYPE_DIRECTORY: u8 = 3;
const FILETYPE_REGULAR_FILE: u8 = 4;
const FILETYPE_SYMBOLIC_LINK: u8 = 7;

/// `proc_exit`'s argument, carried out of the guest as an error so the call
/// stack unwinds the way a trap's does.
#[derive(Debug)]
struct Exit(i32);

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit {}", self.0)
    }
}

impl std::error::Error for Exit {}

struct Host {
    argv: Vec<Vec<u8>>,
    environ: Vec<Vec<u8>>,
    stdin_prefix: Vec<u8>,
    stderr: Option<Vec<u8>>,
    files: HashMap<i32, std::fs::File>,
    /// A directory opened for `fd_readdir` (RFC-0125 §3 M5): its entries as
    /// `(name, filetype)`, read once at the open, with `.` and `..` first the
    /// way the `wasmtime` CLI reports them. The cookie is an index into it.
    dirs: HashMap<i32, Vec<(Vec<u8>, u8)>>,
    next_fd: i32,
    root: PathBuf,
    started: std::time::Instant,
    mem: Option<Memory>,
}

/// One engine for the process. Cranelift's default is speed, which is what the
/// `wasmtime` CLI compiles with; no fuel, because a program is not a generator
/// and has no budget (RFC-0076 M5 is about generators only).
fn engine() -> &'static Engine {
    static ENGINE: std::sync::OnceLock<Engine> = std::sync::OnceLock::new();
    ENGINE.get_or_init(|| Engine::new(&wasmtime::Config::new()).unwrap_or_default())
}

/// Compile and run `bytes` as a WASI command.
///
/// `Err` is a failure of this host or of the module's shape — a module with no
/// `_start`, a trap that is not `proc_exit`. A program that traps on its own
/// terms (`error: ..` on fd 2, then `proc_exit(1)`) is an `Ok` with code 1,
/// because that is the program's output and not this host's.
pub fn run(bytes: &[u8], run: Run) -> Result<Outcome, String> {
    let engine = engine();
    let module = Module::new(engine, bytes).map_err(|e| format!("wasm: {e:?}"))?;
    let mut linker: Linker<Host> = Linker::new(engine);
    link_wasi(&mut linker).map_err(|e| e.to_string())?;
    // A directly-emitted module imports only what it calls after `sweep`, so a
    // `vyrn` import here is an `extern fn` (RFC-0012) the program REACHES. Only
    // a browser page supplies that namespace. A terminal answers each name with
    // the one refusal — `interp::extern_unavailable`'s sentence on fd 2, then
    // exit 1 — because that is what the interpreter prints and what native's C
    // stub prints (`vyrn_codegen::toolchain::extern_trap_stubs`), and a reached
    // `extern` must fail the same way on every engine. RFC-0125 §3 M5, the
    // `extern-unavailable` row. The import stays in the module: the page still
    // fills it, and only the host that cannot answer names the function.
    for imp in module.imports() {
        if imp.module() != "vyrn" {
            continue;
        }
        let Some(ty) = imp.ty().func().cloned() else {
            continue;
        };
        let msg = format!(
            "error: {}\n",
            vyrn_frontend::interp::extern_unavailable(imp.name())
        );
        linker
            .func_new("vyrn", imp.name(), ty, move |mut caller, _, _| {
                write_err(caller.data_mut(), msg.as_bytes());
                Err(Exit(1).into())
            })
            .map_err(|e| e.to_string())?;
    }
    // Anything else the module asks for is neither WASI nor an `extern`, so the
    // trap is this host's own and says the module reached past its list.
    linker
        .define_unknown_imports_as_traps(&module)
        .map_err(|e| e.to_string())?;

    let root = std::env::current_dir().map_err(|e| format!("cwd: {e}"))?;
    let host = Host {
        argv: run
            .argv
            .iter()
            .map(|a| [a.as_bytes(), b"\0"].concat())
            .collect(),
        environ: std::env::vars_os()
            .map(|(k, v)| {
                let mut e = k.into_encoded_bytes();
                e.push(b'=');
                e.extend(v.into_encoded_bytes());
                e.push(0);
                e
            })
            .collect(),
        stdin_prefix: run.stdin_prefix,
        stderr: run.capture_stderr.then(Vec::new),
        files: HashMap::new(),
        dirs: HashMap::new(),
        next_fd: PREOPEN_FD + 1,
        root,
        started: std::time::Instant::now(),
        mem: None,
    };
    let mut store = Store::new(engine, host);
    let inst = linker
        .instantiate(&mut store, &module)
        .map_err(|e| format!("instantiate: {e}"))?;
    let start = inst
        .get_typed_func::<(), ()>(&mut store, "_start")
        .map_err(|e| format!("_start: {e}"))?;
    store.data_mut().mem = match inst.get_export(&mut store, "memory") {
        Some(wasmtime::Extern::Memory(m)) => Some(m),
        _ => return Err("the module exports no memory".into()),
    };
    let code = match start.call(&mut store, ()) {
        // `_start` always ends in `proc_exit`; a plain return is exit 0 too.
        Ok(()) => 0,
        Err(e) => match e.downcast_ref::<Exit>() {
            Some(Exit(code)) => *code,
            // A trap the program did not spell — a wasm `unreachable`, an
            // out-of-bounds memory access. Not program output, so the wording
            // is this host's; the `wasmtime` CLI prints its own around the same
            // trap text (recorded in RFC-0125 §3 M5).
            None => {
                let msg = format!("error: {}\n", first_line(&format!("{e:?}")));
                write_err(store.data_mut(), msg.as_bytes());
                1
            }
        },
    };
    let host = store.into_data();
    Ok(Outcome {
        code,
        stderr: host.stderr.unwrap_or_default(),
    })
}

fn first_line(s: &str) -> &str {
    s.lines().next().unwrap_or(s)
}

fn write_err(host: &mut Host, bytes: &[u8]) {
    match &mut host.stderr {
        Some(buf) => buf.extend_from_slice(bytes),
        None => {
            let mut e = std::io::stderr().lock();
            let _ = e.write_all(bytes);
            let _ = e.flush();
        }
    }
}

/// The guest's memory and the host, borrowed together for one import call.
fn guest<'a>(caller: &'a mut Caller<'_, Host>) -> (&'a mut [u8], &'a mut Host) {
    let mem = caller.data().mem.expect("memory is set before _start");
    mem.data_and_store_mut(caller)
}

fn rd32(data: &[u8], at: i32) -> Option<u32> {
    let at = at as usize;
    data.get(at..at + 4)
        .map(|b| u32::from_le_bytes([b[0], b[1], b[2], b[3]]))
}

fn wr32(data: &mut [u8], at: i32, v: u32) -> Option<()> {
    let at = at as usize;
    data.get_mut(at..at + 4)?.copy_from_slice(&v.to_le_bytes());
    Some(())
}

fn wr64(data: &mut [u8], at: i32, v: u64) -> Option<()> {
    let at = at as usize;
    data.get_mut(at..at + 8)?.copy_from_slice(&v.to_le_bytes());
    Some(())
}

/// The `(ptr, len)` pairs of an iovec array.
fn iovs(data: &[u8], iovs: i32, n: i32) -> Option<Vec<(usize, usize)>> {
    (0..n)
        .map(|i| {
            let head = iovs + i * 8;
            Some((rd32(data, head)? as usize, rd32(data, head + 4)? as usize))
        })
        .collect()
}

fn errno(e: &std::io::Error) -> i32 {
    use std::io::ErrorKind::*;
    match e.kind() {
        NotFound => NOENT,
        PermissionDenied => ACCES,
        AlreadyExists => EXIST,
        IsADirectory => ISDIR,
        NotADirectory => NOTDIR,
        _ => IO,
    }
}

/// A guest path under the preopen, or `None` when it would leave it — an
/// absolute path, or more `..` than there are segments above it. That is the
/// capability rule the `wasmtime` CLI applies to `--dir .`, and the reason a
/// program cannot read outside the directory it was started in.
fn under_root(root: &Path, guest: &str) -> Option<PathBuf> {
    if guest.starts_with('/') || guest.starts_with('\\') || Path::new(guest).is_absolute() {
        return None;
    }
    let mut depth = 0i32;
    for seg in guest.split(['/', '\\']) {
        match seg {
            "" | "." => {}
            ".." => {
                depth -= 1;
                if depth < 0 {
                    return None;
                }
            }
            _ => depth += 1,
        }
    }
    Some(root.join(guest))
}

/// Fill `buf` from the operating system's random source: `/dev/urandom`, or
/// `BCryptGenRandom` on Windows. `random_get` is what seeds `randomSeed()`
/// when no `VYRN_FIXED_SEED` is set, and the `wasmtime` CLI answers it from
/// the same sources.
fn os_random(buf: &mut [u8]) -> bool {
    #[cfg(windows)]
    {
        #[link(name = "bcrypt")]
        extern "system" {
            fn BCryptGenRandom(
                algorithm: *mut std::ffi::c_void,
                buffer: *mut u8,
                len: u32,
                flags: u32,
            ) -> i32;
        }
        const BCRYPT_USE_SYSTEM_PREFERRED_RNG: u32 = 2;
        // SAFETY: a null algorithm handle with the system-preferred flag is the
        // documented way to ask for the default generator; `buf` is a valid
        // writable slice of the stated length.
        let status = unsafe {
            BCryptGenRandom(
                std::ptr::null_mut(),
                buf.as_mut_ptr(),
                buf.len() as u32,
                BCRYPT_USE_SYSTEM_PREFERRED_RNG,
            )
        };
        status == 0
    }
    #[cfg(not(windows))]
    {
        std::fs::File::open("/dev/urandom")
            .and_then(|mut f| f.read_exact(buf))
            .is_ok()
    }
}

fn link_wasi(linker: &mut Linker<Host>) -> wasmtime::Result<()> {
    let wasi = "wasi_snapshot_preview1";

    linker.func_wrap(
        wasi,
        "fd_write",
        |mut caller: Caller<'_, Host>, fd: i32, iov: i32, n: i32, nwritten: i32| -> i32 {
            let (data, host) = guest(&mut caller);
            let Some(chunks) = iovs(data, iov, n) else {
                return BADF;
            };
            let mut bytes = Vec::new();
            for (at, len) in chunks {
                let Some(c) = data.get(at..at + len) else {
                    return BADF;
                };
                bytes.extend_from_slice(c);
            }
            let ok = match fd {
                1 => {
                    let mut o = std::io::stdout().lock();
                    o.write_all(&bytes).and_then(|()| o.flush()).is_ok()
                }
                2 => {
                    write_err(host, &bytes);
                    true
                }
                _ => match host.files.get_mut(&fd) {
                    Some(f) => f.write_all(&bytes).is_ok(),
                    None => return BADF,
                },
            };
            if !ok {
                return IO;
            }
            match wr32(data, nwritten, bytes.len() as u32) {
                Some(()) => SUCCESS,
                None => BADF,
            }
        },
    )?;

    linker.func_wrap(
        wasi,
        "fd_read",
        |mut caller: Caller<'_, Host>, fd: i32, iov: i32, n: i32, nread: i32| -> i32 {
            let (data, host) = guest(&mut caller);
            let Some(chunks) = iovs(data, iov, n) else {
                return BADF;
            };
            // One read into the first buffer with room, as a read syscall does;
            // the guest loops until it has what it wants.
            let Some(&(at, len)) = chunks.iter().find(|(_, len)| *len > 0) else {
                return match wr32(data, nread, 0) {
                    Some(()) => SUCCESS,
                    None => BADF,
                };
            };
            let Some(buf) = data.get_mut(at..at + len) else {
                return BADF;
            };
            let got = if fd == 0 {
                if !host.stdin_prefix.is_empty() {
                    let k = host.stdin_prefix.len().min(buf.len());
                    buf[..k].copy_from_slice(&host.stdin_prefix[..k]);
                    host.stdin_prefix.drain(..k);
                    Ok(k)
                } else {
                    std::io::stdin().lock().read(buf)
                }
            } else {
                match host.files.get_mut(&fd) {
                    Some(f) => f.read(buf),
                    None => return BADF,
                }
            };
            match got {
                Ok(k) => match wr32(data, nread, k as u32) {
                    Some(()) => SUCCESS,
                    None => BADF,
                },
                Err(e) => errno(&e),
            }
        },
    )?;

    linker.func_wrap(
        wasi,
        "fd_close",
        |mut caller: Caller<'_, Host>, fd: i32| -> i32 {
            if fd <= PREOPEN_FD {
                return SUCCESS;
            }
            let host = caller.data_mut();
            match (host.files.remove(&fd), host.dirs.remove(&fd)) {
                (None, None) => BADF,
                _ => SUCCESS,
            }
        },
    )?;

    linker.func_wrap(
        wasi,
        "fd_readdir",
        |mut caller: Caller<'_, Host>,
         fd: i32,
         buf: i32,
         buf_len: i32,
         cookie: i64,
         bufused: i32|
         -> i32 {
            let (data, host) = guest(&mut caller);
            let Some(listed) = host.dirs.get(&fd) else {
                return BADF;
            };
            // Entries from the cookie on, laid end to end — each a `dirent`
            // header (`d_next: u64, d_ino: u64, d_namlen: u32, d_type: u8`, three
            // bytes of padding) and the name. The last is cut where the buffer
            // ends, which is how the guest learns to ask again from that
            // entry's predecessor.
            let mut bytes = Vec::new();
            for (i, (name, kind)) in listed.iter().enumerate().skip(cookie.max(0) as usize) {
                bytes.extend_from_slice(&(i as u64 + 1).to_le_bytes());
                bytes.extend_from_slice(&0u64.to_le_bytes());
                bytes.extend_from_slice(&(name.len() as u32).to_le_bytes());
                bytes.push(*kind);
                bytes.extend_from_slice(&[0, 0, 0]);
                bytes.extend_from_slice(name);
                if bytes.len() >= buf_len as usize {
                    break;
                }
            }
            bytes.truncate(buf_len as usize);
            let Some(slot) = data.get_mut(buf as usize..buf as usize + bytes.len()) else {
                return BADF;
            };
            slot.copy_from_slice(&bytes);
            match wr32(data, bufused, bytes.len() as u32) {
                Some(()) => SUCCESS,
                None => BADF,
            }
        },
    )?;

    linker.func_wrap(
        wasi,
        "fd_sync",
        |mut caller: Caller<'_, Host>, fd: i32| -> i32 {
            match caller.data_mut().files.get(&fd) {
                Some(f) => match f.sync_all() {
                    Ok(()) => SUCCESS,
                    Err(e) => errno(&e),
                },
                None => BADF,
            }
        },
    )?;

    linker.func_wrap(wasi, "proc_exit", |code: i32| -> wasmtime::Result<()> {
        Err(wasmtime::Error::new(Exit(code)))
    })?;

    linker.func_wrap(
        wasi,
        "path_open",
        |mut caller: Caller<'_, Host>,
         dirfd: i32,
         _dirflags: i32,
         path: i32,
         path_len: i32,
         oflags: i32,
         rights: i64,
         _rights_inheriting: i64,
         fdflags: i32,
         out: i32|
         -> i32 {
            let (data, host) = guest(&mut caller);
            if dirfd != PREOPEN_FD {
                return BADF;
            }
            let Some(raw) = data.get(path as usize..(path + path_len) as usize) else {
                return BADF;
            };
            let Ok(name) = std::str::from_utf8(raw) else {
                return NOENT;
            };
            let Some(full) = under_root(&host.root, name) else {
                return NOTCAPABLE;
            };
            if oflags & OFLAGS_DIRECTORY != 0 {
                let entries = match std::fs::read_dir(&full) {
                    Ok(it) => it,
                    Err(e) => return errno(&e),
                };
                let mut listed = vec![
                    (b".".to_vec(), FILETYPE_DIRECTORY),
                    (b"..".to_vec(), FILETYPE_DIRECTORY),
                ];
                for e in entries.flatten() {
                    let kind = match e.file_type() {
                        Ok(t) if t.is_dir() => FILETYPE_DIRECTORY,
                        Ok(t) if t.is_file() => FILETYPE_REGULAR_FILE,
                        Ok(t) if t.is_symlink() => FILETYPE_SYMBOLIC_LINK,
                        _ => FILETYPE_UNKNOWN,
                    };
                    listed.push((
                        e.file_name().to_string_lossy().into_owned().into_bytes(),
                        kind,
                    ));
                }
                let fd = host.next_fd;
                host.next_fd += 1;
                host.dirs.insert(fd, listed);
                return match wr32(data, out, fd as u32) {
                    Some(()) => SUCCESS,
                    None => BADF,
                };
            }
            let mut opts = std::fs::OpenOptions::new();
            opts.read(rights & RIGHT_FD_READ != 0)
                .write(rights & RIGHT_FD_WRITE != 0)
                .append(fdflags & FDFLAGS_APPEND != 0)
                .create(oflags & OFLAGS_CREAT != 0)
                .create_new(oflags & OFLAGS_EXCL != 0)
                .truncate(oflags & OFLAGS_TRUNC != 0);
            match opts.open(&full) {
                Ok(f) => {
                    // A directory opens on some hosts; the guest asked for a
                    // file, and the CLI's host refuses the read with EISDIR.
                    if f.metadata().map(|m| m.is_dir()).unwrap_or(false) {
                        return ISDIR;
                    }
                    let fd = host.next_fd;
                    host.next_fd += 1;
                    host.files.insert(fd, f);
                    match wr32(data, out, fd as u32) {
                        Some(()) => SUCCESS,
                        None => BADF,
                    }
                }
                Err(e) => errno(&e),
            }
        },
    )?;

    linker.func_wrap(
        wasi,
        "path_rename",
        |mut caller: Caller<'_, Host>,
         old_fd: i32,
         old: i32,
         old_len: i32,
         new_fd: i32,
         new: i32,
         new_len: i32|
         -> i32 {
            let (data, host) = guest(&mut caller);
            if old_fd != PREOPEN_FD || new_fd != PREOPEN_FD {
                return BADF;
            }
            let name = |at: i32, len: i32| -> Option<&str> {
                std::str::from_utf8(data.get(at as usize..(at + len) as usize)?).ok()
            };
            let (Some(from), Some(to)) = (name(old, old_len), name(new, new_len)) else {
                return NOENT;
            };
            let (Some(from), Some(to)) = (under_root(&host.root, from), under_root(&host.root, to))
            else {
                return NOTCAPABLE;
            };
            match std::fs::rename(from, to) {
                Ok(()) => SUCCESS,
                Err(e) => errno(&e),
            }
        },
    )?;

    linker.func_wrap(
        wasi,
        "fd_prestat_get",
        |mut caller: Caller<'_, Host>, fd: i32, buf: i32| -> i32 {
            if fd != PREOPEN_FD {
                return BADF;
            }
            let (data, _) = guest(&mut caller);
            // prestat { tag: u8 = dir, pr_name_len: u32 } — the name is `.`.
            match (wr32(data, buf, 0), wr32(data, buf + 4, 1)) {
                (Some(()), Some(())) => SUCCESS,
                _ => BADF,
            }
        },
    )?;

    linker.func_wrap(
        wasi,
        "args_sizes_get",
        |mut caller: Caller<'_, Host>, count: i32, size: i32| -> i32 {
            let (data, host) = guest(&mut caller);
            sizes(data, &host.argv, count, size)
        },
    )?;
    linker.func_wrap(
        wasi,
        "args_get",
        |mut caller: Caller<'_, Host>, ptrs: i32, buf: i32| -> i32 {
            let (data, host) = guest(&mut caller);
            fill(data, &host.argv, ptrs, buf)
        },
    )?;
    linker.func_wrap(
        wasi,
        "environ_sizes_get",
        |mut caller: Caller<'_, Host>, count: i32, size: i32| -> i32 {
            let (data, host) = guest(&mut caller);
            sizes(data, &host.environ, count, size)
        },
    )?;
    linker.func_wrap(
        wasi,
        "environ_get",
        |mut caller: Caller<'_, Host>, ptrs: i32, buf: i32| -> i32 {
            let (data, host) = guest(&mut caller);
            fill(data, &host.environ, ptrs, buf)
        },
    )?;

    linker.func_wrap(
        wasi,
        "clock_time_get",
        |mut caller: Caller<'_, Host>, id: i32, _precision: i64, out: i32| -> i32 {
            let (data, host) = guest(&mut caller);
            let nanos = match id {
                // realtime
                0 => std::time::SystemTime::now()
                    .duration_since(std::time::UNIX_EPOCH)
                    .map(|d| d.as_nanos() as u64)
                    .unwrap_or(0),
                // monotonic, process_cputime, thread_cputime: one steady clock
                _ => host.started.elapsed().as_nanos() as u64,
            };
            match wr64(data, out, nanos) {
                Some(()) => SUCCESS,
                None => BADF,
            }
        },
    )?;

    linker.func_wrap(
        wasi,
        "random_get",
        |mut caller: Caller<'_, Host>, buf: i32, len: i32| -> i32 {
            let (data, _) = guest(&mut caller);
            let Some(slot) = data.get_mut(buf as usize..(buf + len) as usize) else {
                return BADF;
            };
            if os_random(slot) {
                SUCCESS
            } else {
                IO
            }
        },
    )?;
    Ok(())
}

/// `args_sizes_get` and `environ_sizes_get`: how many strings, and the bytes
/// they take with their terminators.
fn sizes(data: &mut [u8], strings: &[Vec<u8>], count: i32, size: i32) -> i32 {
    let bytes: usize = strings.iter().map(Vec::len).sum();
    match (
        wr32(data, count, strings.len() as u32),
        wr32(data, size, bytes as u32),
    ) {
        (Some(()), Some(())) => SUCCESS,
        _ => BADF,
    }
}

/// `args_get` and `environ_get`: the strings laid end to end at `buf`, and a
/// pointer to each at `ptrs`.
fn fill(data: &mut [u8], strings: &[Vec<u8>], ptrs: i32, buf: i32) -> i32 {
    let mut at = buf;
    for (i, s) in strings.iter().enumerate() {
        let Some(slot) = data.get_mut(at as usize..at as usize + s.len()) else {
            return BADF;
        };
        slot.copy_from_slice(s);
        if wr32(data, ptrs + i as i32 * 4, at as u32).is_none() {
            return BADF;
        }
        at += s.len() as i32;
    }
    SUCCESS
}
