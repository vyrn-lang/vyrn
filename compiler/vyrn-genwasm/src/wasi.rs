//! The `wasi_snapshot_preview1` host every embedded run uses: `vyrn run`, `serve`
//! and `test` in the driver, and a generation here. It answers the calls
//! `vyrn_codegen::WASI_IMPORTS` lists and nothing else. Hand-written rather than
//! `wasmtime-wasi`, which brings an async runtime.
//!
//! A [`Policy`] decides what the guest sees of this process. A program sees it
//! as `wasmtime run --dir . --env ..` shows it (`vyrn-cli/tests/fixtures.rs`
//! checks the two agree); a generator sees nothing ([`Policy::generator`]).
//! Enter through [`instantiate`]; read the outcome with [`exit_code`].

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use wasmtime::{Caller, Instance, Linker, Memory, Module, Store};

use crate::GenState;

/// What the guest sees of this process, and how its run is bounded.
pub struct Policy {
    /// This process, as `wasmtime run --dir <root> --env ..` shows it: its
    /// environment, `<root>` preopened as fd 3, stdin, the clocks and the random
    /// source. `None` shows none of it: the environment is empty, and the file,
    /// stdin, clock and random calls are not linked, so a guest that reaches
    /// one traps.
    pub ambient: Option<PathBuf>,
    /// Keeps standard output in [`Wasi::stdout`] instead of writing it through.
    pub capture_stdout: bool,
    /// Keeps standard error in [`Wasi::stderr`] instead of writing it through.
    pub capture_stderr: bool,
    /// The store's fuel. `Some` needs a module compiled by an engine with fuel
    /// on.
    pub fuel: Option<u64>,
}

impl Policy {
    /// What a generator sees: nothing of this process, both streams captured,
    /// and `fuel` as its step budget. A generator's output depends only on its
    /// module, its arguments and the reads its host serves, so one source gives
    /// one result on every machine.
    pub fn generator(fuel: u64) -> Policy {
        Policy {
            ambient: None,
            capture_stdout: true,
            capture_stderr: true,
            fuel: Some(fuel),
        }
    }
}

/// A store's data: the WASI state, the `vyrn_gen` state, and the embedder's own.
pub struct Guest<X> {
    pub wasi: Wasi,
    /// Empty for a module that imports no `vyrn_gen` call.
    pub gen: GenState,
    pub x: X,
}

/// The guest's process state.
pub struct Wasi {
    /// NUL-terminated, `argv[0]` first.
    argv: Vec<Vec<u8>>,
    /// NUL-terminated `KEY=VALUE` strings.
    environ: Vec<Vec<u8>>,
    /// Standard output if captured; `None` writes it through.
    pub stdout: Option<Vec<u8>>,
    /// Standard error if captured; `None` writes it through.
    pub stderr: Option<Vec<u8>>,
    files: HashMap<i32, std::fs::File>,
    /// A directory opened for `fd_readdir`: its entries as `(name, filetype)`,
    /// read once at the open, `.` and `..` first as the `wasmtime` CLI reports
    /// them. The cookie is an index into it.
    dirs: HashMap<i32, Vec<(Vec<u8>, u8)>>,
    next_fd: i32,
    started: std::time::Instant,
    /// Set by [`instantiate`] before `_start` runs.
    pub mem: Option<Memory>,
}

impl Wasi {
    fn new(policy: &Policy, argv: &[String]) -> Wasi {
        let environ = match policy.ambient {
            Some(_) => std::env::vars_os()
                .map(|(k, v)| {
                    let mut e = k.into_encoded_bytes();
                    e.push(b'=');
                    e.extend(v.into_encoded_bytes());
                    e.push(0);
                    e
                })
                .collect(),
            None => Vec::new(),
        };
        Wasi {
            argv: argv
                .iter()
                .map(|a| [a.as_bytes(), b"\0"].concat())
                .collect(),
            environ,
            stdout: policy.capture_stdout.then(Vec::new),
            stderr: policy.capture_stderr.then(Vec::new),
            files: HashMap::new(),
            dirs: HashMap::new(),
            next_fd: PREOPEN_FD + 1,
            started: std::time::Instant::now(),
            mem: None,
        }
    }

    /// Appends to standard error, or writes it through.
    pub fn write_err(&mut self, bytes: &[u8]) {
        match &mut self.stderr {
            Some(buf) => buf.extend_from_slice(bytes),
            None => {
                let mut e = std::io::stderr().lock();
                let _ = e.write_all(bytes);
                let _ = e.flush();
            }
        }
    }
}

/// `proc_exit`'s argument, carried out of the guest as an error so the call
/// stack unwinds the way a trap's does.
#[derive(Debug)]
pub struct Exit(pub i32);

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit {}", self.0)
    }
}

impl std::error::Error for Exit {}

/// The exit code of a call into the guest: 0 for a return, `proc_exit`'s
/// argument for an exit. `Err` is a trap the guest did not spell.
pub fn exit_code(call: wasmtime::Result<()>) -> wasmtime::Result<i32> {
    match call {
        Ok(()) => Ok(0),
        Err(e) => match e.downcast_ref::<Exit>() {
            Some(Exit(code)) => Ok(*code),
            None => Err(e),
        },
    }
}

/// Links the WASI calls `policy` serves and the `vyrn_gen` imports, then
/// `extra`'s, traps every other import, and instantiates `module` on a store
/// under `policy` with `argv`. The store's memory is set; `_start` has not run.
pub fn instantiate<X: 'static>(
    module: &Module,
    policy: &Policy,
    argv: &[String],
    gen: GenState,
    x: X,
    extra: impl FnOnce(&mut Linker<Guest<X>>) -> wasmtime::Result<()>,
) -> Result<(Store<Guest<X>>, Instance), String> {
    let mut linker = Linker::new(module.engine());
    link(&mut linker, policy).map_err(|e| e.to_string())?;
    crate::link(&mut linker).map_err(|e| e.to_string())?;
    extra(&mut linker).map_err(|e| e.to_string())?;
    linker
        .define_unknown_imports_as_traps(module)
        .map_err(|e| e.to_string())?;
    let guest = Guest {
        wasi: Wasi::new(policy, argv),
        gen,
        x,
    };
    let mut store = Store::new(module.engine(), guest);
    if let Some(fuel) = policy.fuel {
        store.set_fuel(fuel).map_err(|e| e.to_string())?;
    }
    let inst = linker
        .instantiate(&mut store, module)
        .map_err(|e| format!("instantiate: {e}"))?;
    store.data_mut().wasi.mem = match inst.get_export(&mut store, "memory") {
        Some(wasmtime::Extern::Memory(m)) => Some(m),
        _ => return Err("the module exports no memory".into()),
    };
    Ok((store, inst))
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

/// The preopened directory, as fd 3, named `.`.
const PREOPEN_FD: i32 = 3;

// `filetype` values, from the witx.
const FILETYPE_UNKNOWN: u8 = 0;
const FILETYPE_DIRECTORY: u8 = 3;
const FILETYPE_REGULAR_FILE: u8 = 4;
const FILETYPE_SYMBOLIC_LINK: u8 = 7;

fn guest<'a, X>(caller: &'a mut Caller<'_, Guest<X>>) -> (&'a mut [u8], &'a mut Wasi) {
    let mem = caller.data().wasi.mem.expect("memory is set before _start");
    let (data, g) = mem.data_and_store_mut(caller);
    (data, &mut g.wasi)
}

fn rd32(data: &[u8], at: i32) -> Option<u32> {
    let at = at as usize;
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// Writes `v` little-endian at `at`; `None` when it does not fit.
pub fn wr32(data: &mut [u8], at: i32, v: u32) -> Option<()> {
    let at = at as usize;
    data.get_mut(at..at + 4)?.copy_from_slice(&v.to_le_bytes());
    Some(())
}

fn wr64(data: &mut [u8], at: i32, v: u64) -> Option<()> {
    let at = at as usize;
    data.get_mut(at..at + 8)?.copy_from_slice(&v.to_le_bytes());
    Some(())
}

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

/// A guest path under the preopen, or `None` when it leaves it: an absolute
/// path, or more `..` than segments above it. The `wasmtime` CLI applies the
/// same rule to `--dir .`.
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

/// Fills `buf` from the OS random source (`/dev/urandom`, or `BCryptGenRandom`
/// on Windows), as the `wasmtime` CLI does. Seeds `randomSeed()` when no
/// `VYRN_FIXED_SEED` is set.
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

/// Links the calls `policy` serves: every `WASI_IMPORTS` call with an ambient
/// root, and only the ones that read nothing of this process without one.
fn link<X: 'static>(linker: &mut Linker<Guest<X>>, policy: &Policy) -> wasmtime::Result<()> {
    let wasi = "wasi_snapshot_preview1";

    linker.func_wrap(
        wasi,
        "fd_write",
        |mut caller: Caller<'_, Guest<X>>, fd: i32, iov: i32, n: i32, nwritten: i32| -> i32 {
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
                1 => match &mut host.stdout {
                    Some(buf) => {
                        buf.extend_from_slice(&bytes);
                        true
                    }
                    None => {
                        let mut o = std::io::stdout().lock();
                        o.write_all(&bytes).and_then(|()| o.flush()).is_ok()
                    }
                },
                2 => {
                    host.write_err(&bytes);
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
        "fd_close",
        |mut caller: Caller<'_, Guest<X>>, fd: i32| -> i32 {
            if fd <= PREOPEN_FD {
                return SUCCESS;
            }
            let host = &mut caller.data_mut().wasi;
            match (host.files.remove(&fd), host.dirs.remove(&fd)) {
                (None, None) => BADF,
                _ => SUCCESS,
            }
        },
    )?;

    linker.func_wrap(wasi, "proc_exit", |code: i32| -> wasmtime::Result<()> {
        Err(wasmtime::Error::new(Exit(code)))
    })?;

    linker.func_wrap(
        wasi,
        "args_sizes_get",
        |mut caller: Caller<'_, Guest<X>>, count: i32, size: i32| -> i32 {
            let (data, host) = guest(&mut caller);
            sizes(data, &host.argv, count, size)
        },
    )?;
    linker.func_wrap(
        wasi,
        "args_get",
        |mut caller: Caller<'_, Guest<X>>, ptrs: i32, buf: i32| -> i32 {
            let (data, host) = guest(&mut caller);
            fill(data, &host.argv, ptrs, buf)
        },
    )?;
    linker.func_wrap(
        wasi,
        "environ_sizes_get",
        |mut caller: Caller<'_, Guest<X>>, count: i32, size: i32| -> i32 {
            let (data, host) = guest(&mut caller);
            sizes(data, &host.environ, count, size)
        },
    )?;
    linker.func_wrap(
        wasi,
        "environ_get",
        |mut caller: Caller<'_, Guest<X>>, ptrs: i32, buf: i32| -> i32 {
            let (data, host) = guest(&mut caller);
            fill(data, &host.environ, ptrs, buf)
        },
    )?;

    let Some(root) = policy.ambient.clone() else {
        return Ok(());
    };

    linker.func_wrap(
        wasi,
        "fd_read",
        |mut caller: Caller<'_, Guest<X>>, fd: i32, iov: i32, n: i32, nread: i32| -> i32 {
            let (data, host) = guest(&mut caller);
            let Some(chunks) = iovs(data, iov, n) else {
                return BADF;
            };
            // One read into the first buffer with room, as a read syscall does;
            // the guest loops.
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
                std::io::stdin().lock().read(buf)
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
        "fd_readdir",
        |mut caller: Caller<'_, Guest<X>>,
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
            // Entries from the cookie on, each a `dirent` header (`d_next: u64,
            // d_ino: u64, d_namlen: u32, d_type: u8`, three bytes of padding)
            // then the name. The last is cut at the buffer's end, so the guest
            // asks again from that entry's predecessor.
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
        |mut caller: Caller<'_, Guest<X>>, fd: i32| -> i32 {
            match caller.data_mut().wasi.files.get(&fd) {
                Some(f) => match f.sync_all() {
                    Ok(()) => SUCCESS,
                    Err(e) => errno(&e),
                },
                None => BADF,
            }
        },
    )?;

    let open_root = root.clone();
    linker.func_wrap(
        wasi,
        "path_open",
        move |mut caller: Caller<'_, Guest<X>>,
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
            let Some(full) = under_root(&open_root, name) else {
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
                    // A directory opens as a file on some hosts; the `wasmtime`
                    // CLI refuses it with EISDIR.
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
        move |mut caller: Caller<'_, Guest<X>>,
              old_fd: i32,
              old: i32,
              old_len: i32,
              new_fd: i32,
              new: i32,
              new_len: i32|
              -> i32 {
            let (data, _) = guest(&mut caller);
            if old_fd != PREOPEN_FD || new_fd != PREOPEN_FD {
                return BADF;
            }
            let name = |at: i32, len: i32| -> Option<&str> {
                std::str::from_utf8(data.get(at as usize..(at + len) as usize)?).ok()
            };
            let (Some(from), Some(to)) = (name(old, old_len), name(new, new_len)) else {
                return NOENT;
            };
            let (Some(from), Some(to)) = (under_root(&root, from), under_root(&root, to)) else {
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
        |mut caller: Caller<'_, Guest<X>>, fd: i32, buf: i32| -> i32 {
            if fd != PREOPEN_FD {
                return BADF;
            }
            let (data, _) = guest(&mut caller);
            // prestat { tag: u8 = dir, pr_name_len: u32 }; the name is `.`.
            match (wr32(data, buf, 0), wr32(data, buf + 4, 1)) {
                (Some(()), Some(())) => SUCCESS,
                _ => BADF,
            }
        },
    )?;

    linker.func_wrap(
        wasi,
        "clock_time_get",
        |mut caller: Caller<'_, Guest<X>>, id: i32, _precision: i64, out: i32| -> i32 {
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
        |mut caller: Caller<'_, Guest<X>>, buf: i32, len: i32| -> i32 {
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

/// Answers `args_sizes_get` and `environ_sizes_get`: the string count, and
/// their bytes with terminators.
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

/// Answers `args_get` and `environ_get`: the strings end to end at `buf`, and
/// a pointer to each at `ptrs`.
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

#[cfg(test)]
mod tests {
    use super::*;
    use std::collections::BTreeMap;
    use vyrn_codegen::wasm::ValType;

    /// The calls `policy` links, with their signatures.
    fn linked(policy: &Policy) -> BTreeMap<String, (Vec<ValType>, Vec<ValType>)> {
        let engine = wasmtime::Engine::default();
        let mut linker = Linker::new(&engine);
        link(&mut linker, policy).expect("link");
        let guest = Guest {
            wasi: Wasi::new(policy, &[]),
            gen: GenState::default(),
            x: (),
        };
        let mut store = Store::new(&engine, guest);
        let enc = |t: wasmtime::ValType| match t {
            wasmtime::ValType::I32 => ValType::I32,
            wasmtime::ValType::I64 => ValType::I64,
            t => panic!("no WASI call takes {t}"),
        };
        let defs: Vec<_> = linker
            .iter(&mut store)
            .map(|(_, name, e)| (name.to_string(), e))
            .collect();
        defs.into_iter()
            .map(|(name, e)| {
                let ty = e.ty(&store).unwrap_func().clone();
                let sig = (
                    ty.params().map(enc).collect(),
                    ty.results().map(enc).collect(),
                );
                (name, sig)
            })
            .collect()
    }

    fn declared(only: Option<&[&str]>) -> BTreeMap<String, (Vec<ValType>, Vec<ValType>)> {
        vyrn_codegen::WASI_IMPORTS
            .iter()
            .filter(|(n, _, _)| only.is_none_or(|o| o.contains(n)))
            .map(|(n, p, r)| (n.to_string(), (p.to_vec(), r.to_vec())))
            .collect()
    }

    /// With an ambient root the host defines each `WASI_IMPORTS` call once, with
    /// its signature, and no other. A module importing anything else traps at
    /// the call.
    #[test]
    fn a_program_gets_exactly_the_declared_wasi_calls() {
        let program = Policy {
            ambient: Some(PathBuf::from(".")),
            capture_stdout: false,
            capture_stderr: false,
            fuel: None,
        };
        assert_eq!(linked(&program), declared(None));
    }

    /// A generator's calls read nothing of this process. A call added to the
    /// run host lands outside this list and fails here before it can reach a
    /// generator.
    #[test]
    fn a_generator_gets_no_call_that_reads_the_process() {
        let gen = &[
            "fd_write",
            "fd_close",
            "proc_exit",
            "args_sizes_get",
            "args_get",
            "environ_sizes_get",
            "environ_get",
        ];
        assert_eq!(linked(&Policy::generator(7)), declared(Some(gen)));
    }

    #[test]
    fn a_generator_sees_no_environment_and_writes_nothing_through() {
        assert!(
            std::env::vars_os().next().is_some(),
            "the test process has an environment"
        );
        let policy = Policy::generator(7);
        let wasi = Wasi::new(&policy, &["gen".to_string()]);
        assert!(wasi.environ.is_empty());
        assert_eq!(wasi.argv, vec![b"gen\0".to_vec()]);
        assert_eq!(
            (wasi.stdout, wasi.stderr),
            (Some(Vec::new()), Some(Vec::new()))
        );
        assert_eq!((policy.ambient, policy.fuel), (None, Some(7)));
    }
}
