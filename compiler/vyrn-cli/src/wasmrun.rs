//! The WASI host that `vyrn run` runs a program's own wasm under, in this
//! process by the embedded wasmtime. It answers the `wasi_snapshot_preview1`
//! imports `vyrn_codegen::WASI_IMPORTS` lists and nothing else: an `extern`
//! import gets the terminal's refusal (see [`open`]), any other import traps.
//! Hand-written rather than `wasmtime-wasi`, which brings an async runtime.
//! The setup matches `wasmtime run --dir . --env ..`: argv, this process's
//! environment, stdio passed through, and the working directory preopened as
//! fd 3; `tests/fixtures.rs` checks the two agree.

use std::collections::HashMap;
use std::io::{Read, Write};
use std::path::{Path, PathBuf};
use wasmtime::{Caller, Engine, Global, Linker, Memory, Module, Store, WasmParams, WasmResults};

/// What one run produced. The exit code is `proc_exit`'s argument, or 1 when
/// the module trapped.
pub struct Outcome {
    pub code: i32,
    /// Standard output if captured; otherwise empty and already written through.
    pub stdout: Vec<u8>,
    /// Standard error if captured; otherwise empty and already written through.
    pub stderr: Vec<u8>,
    pub meter: Option<Meter>,
}

pub struct Run {
    /// `argv[0]` is the program's name; the rest is `args()`.
    pub argv: Vec<String>,
    /// Bytes standard input serves before this process's own; the test
    /// harness passes the guest its body's index this way.
    pub stdin_prefix: Vec<u8>,
    /// Keeps the guest's standard output in [`Outcome::stdout`]; `vyrn routes`
    /// reads its answer out of it.
    pub capture_stdout: bool,
    /// Keeps the guest's standard error in [`Outcome::stderr`]; the test
    /// harness reads a trap's message out of it.
    pub capture_stderr: bool,
    /// Times the phases and counts operations into [`Outcome::meter`], for
    /// `vyrn run --profile`. Off by default: the fuel counter costs every block.
    pub meter: bool,
}

/// What a metered run measured: wall time per host phase and one count. No
/// per-function row is possible without instrumenting the bytes, which would
/// profile a program nobody ships.
pub struct Meter {
    /// Cranelift compiling the module.
    pub translate: std::time::Duration,
    /// Linking, instantiating, and reading the memory export.
    pub instantiate: std::time::Duration,
    /// `_start`, from the call to the exit.
    pub run: std::time::Duration,
    /// Operations the guest executed, as wasmtime counts fuel. The same number
    /// on every machine and every run of the same program and input.
    pub fuel: u64,
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
    stdout: Option<Vec<u8>>,
    stderr: Option<Vec<u8>>,
    files: HashMap<i32, std::fs::File>,
    /// A directory opened for `fd_readdir`: its entries as `(name, filetype)`,
    /// read once at the open, `.` and `..` first as the `wasmtime` CLI reports
    /// them. The cookie is an index into it.
    dirs: HashMap<i32, Vec<(Vec<u8>, u8)>>,
    next_fd: i32,
    root: PathBuf,
    started: std::time::Instant,
    mem: Option<Memory>,
    /// Cranelift's time over the module, measured in [`open`], reported by [`run`].
    translate: std::time::Duration,
    /// The host side of the `vyrn_gen` imports, for a module compiled as a
    /// generator host (`vyrn test` over `test` bodies that reach a `gen fn`).
    /// Empty for every other module.
    gen: vyrn_genwasm::GenState,
    /// The check oracle's rows ([`check_rows`]) and each one's count of runs.
    checks: std::sync::Arc<Vec<String>>,
    counts: Vec<u64>,
}

/// Served by `vyrn_genwasm`, so a `test` block's generator meets the same
/// arena, splice rule and atom stream a generation does.
impl vyrn_genwasm::GenHost for Host {
    fn gen(&mut self) -> &mut vyrn_genwasm::GenState {
        &mut self.gen
    }
    fn memory(&self) -> Option<Memory> {
        self.mem
    }
}

/// One engine per process, and a second, metered one: fuel is a counter the
/// guest decrements in every block, so only `vyrn run --profile` pays for it.
fn engine(metered: bool) -> &'static Engine {
    static PLAIN: std::sync::OnceLock<Engine> = std::sync::OnceLock::new();
    static METERED: std::sync::OnceLock<Engine> = std::sync::OnceLock::new();
    let config = |fuel: bool| {
        let mut cfg = wasmtime::Config::new();
        cfg.consume_fuel(fuel);
        cfg.max_wasm_stack(vyrn_frontend::trap::WASM_STACK_BYTES);
        // wasmtime refuses a wasm stack larger than the async one.
        cfg.async_stack_size(vyrn_frontend::trap::RUN_STACK_BYTES);
        Engine::new(&cfg).expect("the engine's configuration is fixed and valid")
    };
    if metered {
        METERED.get_or_init(|| config(true))
    } else {
        PLAIN.get_or_init(|| config(false))
    }
}

/// The wording of a trap the program did not spell: a stack overflow as
/// [`vyrn_frontend::trap::STACK_EXHAUSTED`], anything else as the engine's
/// first line.
fn host_trap(e: &wasmtime::Error) -> String {
    match e.downcast_ref::<wasmtime::Trap>() {
        Some(wasmtime::Trap::StackOverflow) => vyrn_frontend::trap::STACK_EXHAUSTED.to_string(),
        _ => first_line(&format!("{e:?}")).to_string(),
    }
}

/// Compiles and runs `bytes` as a WASI command.
///
/// `Err` is a failure of this host or of the module's shape: no `_start`, or a
/// trap that is not `proc_exit`. A program that traps on its own terms
/// (`error: ..` on fd 2, then `proc_exit(1)`) is `Ok` with code 1.
pub fn run(bytes: &[u8], run: Run) -> Result<Outcome, String> {
    let clock = std::time::Instant::now();
    let (mut store, inst) = open(&compile(bytes, run.meter)?, &run, None)?;
    // `open` compiles and instantiates; the rest of its span is instantiation.
    let translate = store.data().translate;
    let instantiate = clock.elapsed().saturating_sub(translate);
    let start = inst
        .get_typed_func::<(), ()>(&mut store, "_start")
        .map_err(|e| format!("_start: {e}"))?;
    let clock = std::time::Instant::now();
    let code = match start.call(&mut store, ()) {
        // `_start` always ends in `proc_exit`; a plain return is exit 0 too.
        Ok(()) => 0,
        Err(e) => match e.downcast_ref::<Exit>() {
            Some(Exit(code)) => *code,
            // A trap the program did not spell (`unreachable`, an out-of-bounds
            // access): the wording is this host's.
            None => {
                let msg = format!("error: {}\n", host_trap(&e));
                write_err(store.data_mut(), msg.as_bytes());
                1
            }
        },
    };
    let meter = run.meter.then(|| Meter {
        translate,
        instantiate,
        run: clock.elapsed(),
        fuel: u64::MAX - store.get_fuel().unwrap_or(u64::MAX),
    });
    let host = store.into_data();
    if let vyrn_lower::check::Mode::Count(log) = vyrn_lower::check::mode() {
        log_checks(log, run.argv.first().map_or("", |a| a.as_str()), &host)?;
    }
    Ok(Outcome {
        code,
        stdout: host.stdout.unwrap_or_default(),
        stderr: host.stderr.unwrap_or_default(),
        meter,
    })
}

/// Links this host to `module` and instantiates it: everything before `_start`.
/// Separate from [`run`] because a [`Resident`] instance outlives `_start`.
fn open(
    module: &Compiled,
    run: &Run,
    gen: Option<vyrn_genwasm::GenState>,
) -> Result<(Store<Host>, wasmtime::Instance), String> {
    let engine = engine(module.meter);
    let translate = module.translate;
    let meter = module.meter;
    let checks = module.checks.clone();
    let module = &module.module;
    let mut linker: Linker<Host> = Linker::new(engine);
    link_wasi(&mut linker).map_err(|e| e.to_string())?;
    // Imported only by a module compiled as a generator host.
    vyrn_genwasm::link(&mut linker).map_err(|e| e.to_string())?;
    // After `sweep`, a `vyrn` import is an `extern fn` the program reaches.
    // Only a browser page supplies that namespace. A terminal answers each name
    // with `trap::extern_unavailable`'s sentence on fd 2, then exit 1, as the
    // interpreter and `vyrn_codegen::toolchain::wasi_host_c` do, so a reached
    // `extern` fails the same way on every engine.
    for imp in module.imports() {
        if imp.module() != "vyrn" {
            continue;
        }
        let Some(ty) = imp.ty().func().cloned() else {
            continue;
        };
        let msg = format!(
            "error: {}\n",
            vyrn_frontend::trap::extern_unavailable(imp.name())
        );
        linker
            .func_new("vyrn", imp.name(), ty, move |mut caller, _, _| {
                write_err(caller.data_mut(), msg.as_bytes());
                Err(Exit(1).into())
            })
            .map_err(|e| e.to_string())?;
    }
    // The check oracle's imports ([`vyrn_lower::check::Mode::Count`]).
    linker
        .func_wrap("vyrn_check", "hit", |mut c: Caller<'_, Host>, id: i32| {
            if let Some(n) = c.data_mut().counts.get_mut(id as usize) {
                *n += 1;
            }
        })
        .map_err(|e| e.to_string())?;
    linker
        .func_wrap(
            "vyrn_check",
            "fail",
            |mut c: Caller<'_, Host>, id: i32| -> wasmtime::Result<()> {
                let row = c
                    .data()
                    .checks
                    .get(id as usize)
                    .map_or(String::new(), |r| r.replace('\t', " "));
                let msg = format!(
                    "error: {}: {row}\n",
                    vyrn_frontend::trap::PROVED_CHECK_FAILED
                );
                write_err(c.data_mut(), msg.as_bytes());
                Err(Exit(1).into())
            },
        )
        .map_err(|e| e.to_string())?;
    // Anything else is neither WASI nor an `extern`: trap.
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
        stdin_prefix: run.stdin_prefix.clone(),
        stdout: run.capture_stdout.then(Vec::new),
        stderr: run.capture_stderr.then(Vec::new),
        files: HashMap::new(),
        dirs: HashMap::new(),
        next_fd: PREOPEN_FD + 1,
        root,
        started: std::time::Instant::now(),
        mem: None,
        translate,
        gen: gen.unwrap_or_default(),
        counts: vec![0; checks.len()],
        checks,
    };
    let mut store = Store::new(engine, host);
    // A budget nothing exhausts: the counter is read, never a stop.
    if meter {
        store.set_fuel(u64::MAX).map_err(|e| e.to_string())?;
    }
    let inst = linker
        .instantiate(&mut store, &module)
        .map_err(|e| format!("instantiate: {e}"))?;
    store.data_mut().mem = match inst.get_export(&mut store, "memory") {
        Some(wasmtime::Extern::Memory(m)) => Some(m),
        _ => return Err("the module exports no memory".into()),
    };
    Ok((store, inst))
}

/// One module, translated once and instantiable many times: `--workers N`
/// costs one Cranelift compile and N instantiations.
pub struct Compiled {
    module: Module,
    meter: bool,
    translate: std::time::Duration,
    checks: std::sync::Arc<Vec<String>>,
}

pub fn compile(bytes: &[u8], meter: bool) -> Result<Compiled, String> {
    let clock = std::time::Instant::now();
    let module = Module::new(engine(meter), bytes).map_err(|e| {
        // A module the engine refuses is a compiler defect, and the bytes are
        // its only evidence (#444).
        let kept = std::env::temp_dir().join(format!("vyrn-invalid-{}.wasm", std::process::id()));
        match std::fs::write(&kept, bytes) {
            Ok(()) => format!(
                "wasm: {e:?}\n  note: the module is kept at {}",
                kept.display()
            ),
            Err(_) => format!("wasm: {e:?}"),
        }
    })?;
    Ok(Compiled {
        module,
        meter,
        translate: clock.elapsed(),
        checks: std::sync::Arc::new(check_rows(bytes)),
    })
}

/// One instance that outlives `_start`, which `vyrn serve` answers requests on.
///
/// `proc_exit` unwinds the call and not the store, so the module's state
/// survives `main`. Every door is an `export extern fn` the CLI appended, with
/// the extern String ABI: an argument is a pointer to NUL-terminated UTF-8 the
/// caller allocates with `__vyrn_malloc` behind the `{ len, cap }` header, and
/// a returned String is the caller's to `__vyrn_free`. `web/wasi-min.js` does
/// the same marshalling.
pub struct Resident {
    store: Store<Host>,
    inst: wasmtime::Instance,
    /// The stack pointer and the nesting words' address, which a module with
    /// a door exports (`vyrn_codegen::wasm::Module::export_entry_state`).
    entry: Option<(Global, usize)>,
}

/// The eight-byte `{ len, cap }` header in front of every Vyrn String.
const STR_HDR: i32 = 8;

/// Compiles `bytes`, instantiates, and runs `_start`, leaving the store open.
/// A non-zero exit code from `main` aborts the serve, as under the interpreter.
pub fn start(
    bytes: &[u8],
    run: &Run,
    gen: Option<vyrn_genwasm::GenState>,
) -> Result<(Resident, i32), String> {
    start_on(&compile(bytes, false)?, run, gen)
}

/// [`start`] on a module already translated: one worker of the pool.
pub fn start_on(
    module: &Compiled,
    run: &Run,
    gen: Option<vyrn_genwasm::GenState>,
) -> Result<(Resident, i32), String> {
    let (mut store, inst) = open(module, run, gen)?;
    let entry = inst
        .get_typed_func::<(), ()>(&mut store, "_start")
        .map_err(|e| format!("_start: {e}"))?;
    let code = match entry.call(&mut store, ()) {
        Ok(()) => 0,
        Err(e) => match e.downcast_ref::<Exit>() {
            Some(Exit(code)) => *code,
            None => return Err(host_trap(&e)),
        },
    };
    let sp = inst.get_global(&mut store, vyrn_codegen::wasm::SP_EXPORT);
    let nesting = inst
        .get_global(&mut store, vyrn_codegen::wasm::NESTING_EXPORT)
        .and_then(|g| g.get(&mut store).i32());
    let entry = sp.zip(nesting.map(|at| at as usize));
    Ok((Resident { store, inst, entry }, code))
}

impl Resident {
    /// Calls export `name`. The outer error is a missing or mistyped export,
    /// the inner one a trap. A trap abandons the frames and regions the call
    /// took, so after one this restores the stack pointer and the nesting words
    /// it read before. A trapped call's heap blocks stay allocated.
    fn call<P: WasmParams, R: WasmResults>(
        &mut self,
        name: &str,
        args: P,
    ) -> Result<wasmtime::Result<R>, String> {
        let f = self
            .inst
            .get_typed_func::<P, R>(&mut self.store, name)
            .map_err(|e| format!("{name}: {e}"))?;
        let Some((sp, at)) = self.entry else {
            return Ok(f.call(&mut self.store, args));
        };
        // The words lie in the statics, which memory always covers.
        let words = at..at + 8;
        let mem = self.store.data().mem.expect("memory is set before _start");
        let top = sp.get(&mut self.store);
        let mut nesting = [0u8; 8];
        nesting.copy_from_slice(&mem.data(&self.store)[words.clone()]);
        let out = f.call(&mut self.store, args);
        if out.is_err() {
            sp.set(&mut self.store, top)
                .map_err(|e| format!("{name}: {e}"))?;
            mem.data_mut(&mut self.store)[words].copy_from_slice(&nesting);
        }
        Ok(out)
    }

    /// Takes what the guest wrote to standard error since the last drain. A
    /// trap inside a door writes its `error: ..` line there before it exits.
    pub fn drain_err(&mut self) -> String {
        match &mut self.store.data_mut().stderr {
            Some(buf) => String::from_utf8_lossy(&std::mem::take(buf)).into_owned(),
            None => String::new(),
        }
    }

    /// Writes a String argument into guest memory: header, bytes, NUL. Returns
    /// the string's address; the block starts eight bytes before.
    fn alloc(&mut self, s: &str) -> Result<i32, String> {
        let n = s.len() as i32;
        let base = self
            .call::<i64, i32>("__vyrn_malloc", (STR_HDR + n + 1) as i64)?
            .map_err(|e| format!("__vyrn_malloc: {e}"))?;
        let mem = self.store.data().mem.expect("memory is set before _start");
        let data = mem.data_mut(&mut self.store);
        let mut write = || -> Option<()> {
            wr32(data, base, n as u32)?;
            wr32(data, base + 4, n as u32)?;
            let at = (base + STR_HDR) as usize;
            data.get_mut(at..at + s.len())?
                .copy_from_slice(s.as_bytes());
            *data.get_mut(at + s.len())? = 0;
            Some(())
        };
        write().ok_or_else(|| "the guest's memory is too small for the argument".to_string())?;
        Ok(base + STR_HDR)
    }

    /// Frees a String argument; the block starts at its header.
    fn release(&mut self, ptr: i32) -> Result<(), String> {
        self.call::<i32, ()>("__vyrn_free", ptr - STR_HDR)?
            .map_err(|e| format!("__vyrn_free: {e}"))
    }

    /// Reads a returned String up to its NUL, then frees it: the result is the
    /// caller's.
    fn text(&mut self, ptr: i32) -> Result<String, String> {
        let mem = self.store.data().mem.expect("memory is set before _start");
        let data = mem.data(&self.store);
        let at = ptr as usize;
        let end = data
            .get(at..)
            .and_then(|rest| rest.iter().position(|b| *b == 0))
            .ok_or_else(|| "a returned String has no terminator".to_string())?;
        let out = String::from_utf8_lossy(&data[at..at + end]).into_owned();
        self.release(ptr)?;
        Ok(out)
    }

    /// The message for a trap out of a door: the guest's own `error: ..` line
    /// when it wrote one before `proc_exit`, else this host's wording.
    fn trapped(&mut self, door: &str, e: wasmtime::Error) -> String {
        let said = self.drain_err();
        let line = said
            .lines()
            .next()
            .unwrap_or("")
            .trim_start_matches("error: ")
            .to_string();
        if line.is_empty() {
            format!("{door}: {}", host_trap(&e))
        } else {
            line
        }
    }

    /// Passes a door its String arguments and calls it for its effect.
    pub fn tell(&mut self, door: &str, args: &[&str]) -> Result<(), String> {
        let mut ptrs = Vec::with_capacity(args.len());
        for a in args {
            ptrs.push(self.alloc(a)?);
        }
        let out = match ptrs.len() {
            0 => self.call(door, ())?,
            1 => self.call(door, ptrs[0])?,
            2 => self.call(door, (ptrs[0], ptrs[1]))?,
            n => return Err(format!("{door}: {n} arguments is not a door shape")),
        };
        // The caller owns a String argument, so it is freed even after a trap.
        for p in ptrs {
            let _ = self.release(p);
        }
        out.map_err(|e| self.trapped(door, e))
    }

    /// Calls a nullary door and returns what the guest wrote to standard error
    /// before its last `error: ..` line, and that line if it refused. A test
    /// body's output passes through whether the body passed or failed.
    pub fn call_body(&mut self, door: &str) -> (String, Option<String>) {
        let outcome = match self.call::<(), ()>(door, ()) {
            Err(e) => Err(e),
            Ok(Ok(())) => Ok(()),
            Ok(Err(e)) => match e.downcast_ref::<Exit>() {
                Some(Exit(0)) => Ok(()),
                Some(Exit(code)) => Err(format!("exit {code}")),
                None => Err(host_trap(&e)),
            },
        };
        let said = self.drain_err();
        let Err(host) = outcome else {
            return (said, None);
        };
        match said.rfind("error: ") {
            Some(at) if at == 0 || said.as_bytes()[at - 1] == b'\n' => (
                said[..at].to_string(),
                Some(said[at + 7..].trim_end_matches('\n').to_string()),
            ),
            _ => (said, Some(host)),
        }
    }

    pub fn ask_bool(&mut self, door: &str) -> Result<bool, String> {
        match self.call::<(), i32>(door, ())? {
            Ok(v) => Ok(v != 0),
            Err(e) => Err(self.trapped(door, e)),
        }
    }

    pub fn ask_int(&mut self, door: &str) -> Result<i64, String> {
        match self.call::<(), i64>(door, ())? {
            Ok(v) => Ok(v),
            Err(e) => Err(self.trapped(door, e)),
        }
    }

    /// A door that answers a `String`, called with an index or with nothing.
    pub fn ask_text(&mut self, door: &str, at: Option<i64>) -> Result<String, String> {
        let got = match at {
            None => self.call::<(), i32>(door, ())?,
            Some(i) => self.call::<i64, i32>(door, i)?,
        };
        match got {
            Ok(p) => self.text(p),
            Err(e) => Err(self.trapped(door, e)),
        }
    }
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

/// The rows of a module's `vyrn:checks` section, one per check the oracle counts; empty for a
/// module without one. The walk trusts the bytes, which the engine has already validated.
fn check_rows(bytes: &[u8]) -> Vec<String> {
    fn leb(b: &[u8], at: &mut usize) -> usize {
        let (mut v, mut shift) = (0usize, 0);
        while let Some(&x) = b.get(*at) {
            *at += 1;
            v |= usize::from(x & 0x7f) << shift;
            shift += 7;
            if x & 0x80 == 0 {
                break;
            }
        }
        v
    }
    let mut at = 8;
    while at < bytes.len() {
        let id = bytes[at];
        at += 1;
        let size = leb(bytes, &mut at);
        let end = at + size;
        if id == 0 {
            let mut p = at;
            let n = leb(bytes, &mut p);
            if bytes.get(p..p + n) == Some(b"vyrn:checks".as_slice()) {
                let payload = String::from_utf8_lossy(&bytes[p + n..end]);
                return payload.split('\n').map(str::to_string).collect();
            }
        }
        at = end;
    }
    Vec::new()
}

/// Appends each check row's count to `log`: the program, then the row, tab-separated.
fn log_checks(log: &Path, program: &str, host: &Host) -> Result<(), String> {
    if host.checks.is_empty() {
        return Ok(());
    }
    let mut out = String::new();
    for (row, n) in host.checks.iter().zip(&host.counts) {
        out.push_str(&format!("{program}\t{row}\t{n}\n"));
    }
    std::fs::OpenOptions::new()
        .create(true)
        .append(true)
        .open(log)
        .and_then(|mut f| f.write_all(out.as_bytes()))
        .map_err(|e| format!("{}: {e}", log.display()))
}

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
            // prestat { tag: u8 = dir, pr_name_len: u32 }; the name is `.`.
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

    #[test]
    fn a_refused_module_is_kept_and_named() {
        let bad = b"\0asm\x01\0\0\0\x01";
        let Err(e) = compile(bad, false) else {
            panic!("a truncated section compiled");
        };
        let kept = e
            .split("kept at ")
            .nth(1)
            .expect("the error names the file");
        assert_eq!(std::fs::read(kept).expect("the file is there"), bad);
        let _ = std::fs::remove_file(kept);
    }

    /// A program with module state and an exported door.
    const PROBE: &str = r#"let mut hits = 0

export extern fn bump() -> Int64 {
    hits = hits + 1
    return hits
}

fn main() -> Int64 {
    hits = 100
    return 0
}
"#;

    // A compiled program calls `std/runtime`, which the loader injects from
    // the std root; this library target has no driver to find it on disk, so
    // the needed files are embedded.
    fn probe_bytes() -> Vec<u8> {
        let files = vyrn_frontend::loader::MapResolver(
            [
                (
                    "std/runtime.vyrn".to_string(),
                    include_str!("../../../std/runtime.vyrn").to_string(),
                ),
                (
                    "std/mem.vyrn".to_string(),
                    include_str!("../../../std/mem.vyrn").to_string(),
                ),
                (
                    "std/text.vyrn".to_string(),
                    include_str!("../../../std/text.vyrn").to_string(),
                ),
                (
                    "std/strpred.vyrn".to_string(),
                    include_str!("../../../std/strpred.vyrn").to_string(),
                ),
                (
                    "std/num.vyrn".to_string(),
                    include_str!("../../../std/num.vyrn").to_string(),
                ),
                (
                    "std/codecs.vyrn".to_string(),
                    include_str!("../../../std/codecs.vyrn").to_string(),
                ),
                (
                    "std/hash.vyrn".to_string(),
                    include_str!("../../../std/hash.vyrn").to_string(),
                ),
            ]
            .into_iter()
            .collect(),
        );
        let opts = vyrn_frontend::loader::LoadOptions {
            std_root: Some("std".into()),
            ..Default::default()
        };
        let (mut program, memo) = vyrn_frontend::project::Memo::load(|| {
            vyrn_lower::load(PROBE, "probe.vyrn", &opts, &files)
        })
        .expect("the probe loads");
        // Without the lowering the placer never runs, and the instance is not
        // the one `vyrn serve` holds.
        vyrn_lower::install();
        let diags = vyrn_lower::check_and_synthesize(&mut program);
        assert!(diags.is_empty(), "the probe checks: {diags:?}");
        vyrn_codegen::direct::compile(&program, &memo).expect("the probe compiles")
    }

    fn quiet() -> Run {
        Run {
            argv: vec!["probe.vyrn".to_string()],
            stdin_prefix: Vec::new(),
            capture_stdout: true,
            capture_stderr: true,
            meter: false,
        }
    }

    /// `link_wasi` defines each `vyrn_codegen::WASI_IMPORTS` call once, with its signature, and
    /// no other. `open` answers an import it lacks with a trap, so a run fails only at the call.
    #[test]
    fn the_host_links_exactly_the_declared_wasi_calls() {
        use std::collections::BTreeMap;
        use vyrn_codegen::wasm::ValType;
        let (mut store, _) = open(
            &compile(&probe_bytes(), false).expect("compile"),
            &quiet(),
            None,
        )
        .expect("open");
        let mut linker = Linker::new(engine(false));
        link_wasi(&mut linker).expect("link");
        let defs: Vec<(String, wasmtime::Extern)> = linker
            .iter(&mut store)
            .map(|(_, name, e)| (name.to_string(), e))
            .collect();
        let enc = |t: wasmtime::ValType| match t {
            wasmtime::ValType::I32 => ValType::I32,
            wasmtime::ValType::I64 => ValType::I64,
            t => panic!("no WASI call takes {t}"),
        };
        let linked: BTreeMap<_, _> = defs
            .into_iter()
            .map(|(name, e)| {
                let ty = e.ty(&store).unwrap_func().clone();
                (
                    name,
                    (
                        ty.params().map(enc).collect(),
                        ty.results().map(enc).collect(),
                    ),
                )
            })
            .collect();
        let want: BTreeMap<_, (Vec<_>, Vec<_>)> = vyrn_codegen::WASI_IMPORTS
            .iter()
            .map(|(n, p, r)| (n.to_string(), (p.to_vec(), r.to_vec())))
            .collect();
        assert_eq!(linked, want);
    }

    /// Prints the resident and fresh per-answer costs with `--nocapture`; asserts
    /// only the order, because other worktrees' gates share the machine.
    #[test]
    fn a_resident_answer_is_cheaper_than_a_fresh_instance() {
        let bytes = probe_bytes();
        let run = quiet();
        let (mut store, inst) =
            open(&compile(&bytes, false).expect("compile"), &run, None).expect("open");
        let start = inst
            .get_typed_func::<(), ()>(&mut store, "_start")
            .expect("_start");
        let _ = start.call(&mut store, ());
        let bump = inst.get_typed_func::<(), i64>(&mut store, "bump").unwrap();
        let n = 1000;
        let clock = std::time::Instant::now();
        for _ in 0..n {
            bump.call(&mut store, ()).expect("resident call");
        }
        let resident = clock.elapsed() / n;

        let m = 20;
        let clock = std::time::Instant::now();
        for _ in 0..m {
            let (mut s, i) =
                open(&compile(&bytes, false).expect("compile"), &run, None).expect("open");
            let start = i.get_typed_func::<(), ()>(&mut s, "_start").unwrap();
            let _ = start.call(&mut s, ());
            i.get_typed_func::<(), i64>(&mut s, "bump")
                .unwrap()
                .call(&mut s, ())
                .expect("fresh call");
        }
        let fresh = clock.elapsed() / m;
        eprintln!("resident {resident:?} per answer, fresh {fresh:?} per answer");
        assert!(resident < fresh, "resident {resident:?}, fresh {fresh:?}");
    }
}
