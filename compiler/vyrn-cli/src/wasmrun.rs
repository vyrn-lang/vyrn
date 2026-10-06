//! How `vyrn run`, `serve` and `test` run a program's own wasm, in this process
//! by the embedded wasmtime. The WASI host is `vyrn_genwasm::wasi`'s, with the
//! working directory as the ambient root: the setup of `wasmtime run --dir .
//! --env ..`. This file adds the terminal's refusal of an `extern` import (see
//! [`open`]), the check oracle's imports, and the resident instance `serve`
//! answers on.

use std::io::Write;
use std::path::Path;
use vyrn_frontend::trap;
use vyrn_genwasm::wasi;
use wasmtime::{Caller, Engine, Global, Module, Store, WasmParams, WasmResults};

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

#[derive(Default)]
pub struct Run {
    /// `argv[0]` is the program's name; the rest is `args()`.
    pub argv: Vec<String>,
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

/// The check oracle's rows ([`check_rows`]) and each one's count of runs.
struct Oracle {
    checks: std::sync::Arc<Vec<String>>,
    counts: Vec<u64>,
}

type Host = wasi::Guest<Oracle>;

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
    let module = compile(bytes, run.meter)?;
    let (mut store, inst) = open(&module, &run, None)?;
    let translate = module.translate;
    let instantiate = clock.elapsed().saturating_sub(translate);
    let start = inst
        .get_typed_func::<(), ()>(&mut store, "_start")
        .map_err(|e| format!("_start: {e}"))?;
    let clock = std::time::Instant::now();
    let code = match wasi::exit_code(start.call(&mut store, ())) {
        Ok(code) => code,
        // A trap the program did not spell (`unreachable`, an out-of-bounds
        // access): the wording is this host's.
        Err(e) => {
            let msg = trap::line(&host_trap(&e));
            store.data_mut().wasi.write_err(msg.as_bytes());
            1
        }
    };
    let meter = run.meter.then(|| Meter {
        translate,
        instantiate,
        run: clock.elapsed(),
        fuel: u64::MAX - store.get_fuel().unwrap_or(u64::MAX),
    });
    let host = store.into_data();
    if let vyrn_lower::check::Mode::Count(log) = vyrn_lower::check::mode() {
        log_checks(log, run.argv.first().map_or("", |a| a.as_str()), &host.x)?;
    }
    Ok(Outcome {
        code,
        stdout: host.wasi.stdout.unwrap_or_default(),
        stderr: host.wasi.stderr.unwrap_or_default(),
        meter,
    })
}

/// Instantiates `module` under this process's environment and working
/// directory: everything before `_start`. Separate from [`run`] because a
/// [`Resident`] instance outlives `_start`.
fn open(
    module: &Compiled,
    run: &Run,
    gen: Option<vyrn_genwasm::GenState>,
) -> Result<(Store<Host>, wasmtime::Instance), String> {
    let policy = wasi::Policy {
        ambient: Some(std::env::current_dir().map_err(|e| format!("cwd: {e}"))?),
        capture_stdout: run.capture_stdout,
        capture_stderr: run.capture_stderr,
        // A budget nothing exhausts: the counter is read, never a stop.
        fuel: module.meter.then_some(u64::MAX),
    };
    let oracle = Oracle {
        checks: module.checks.clone(),
        counts: vec![0; module.checks.len()],
    };
    let gen = gen.unwrap_or_default();
    wasi::instantiate(&module.module, &policy, &run.argv, gen, oracle, |linker| {
        // After `sweep`, a `vyrn` import is an `extern fn` the program reaches.
        // Only a browser page supplies that namespace. A terminal answers each
        // name with `trap::extern_unavailable`'s sentence on fd 2, then exit 1,
        // as the interpreter and `vyrn_codegen::toolchain::wasi_host_c` do, so a
        // reached `extern` fails the same way on every engine.
        for imp in module.module.imports() {
            if imp.module() != "vyrn" {
                continue;
            }
            let Some(ty) = imp.ty().func().cloned() else {
                continue;
            };
            let msg = trap::line(&trap::extern_unavailable(imp.name()));
            linker.func_new("vyrn", imp.name(), ty, move |mut caller, _, _| {
                caller.data_mut().wasi.write_err(msg.as_bytes());
                Err(wasi::Exit(1).into())
            })?;
        }
        // The check oracle's imports ([`vyrn_lower::check::Mode::Count`]).
        linker.func_wrap("vyrn_check", "hit", |mut c: Caller<'_, Host>, id: i32| {
            if let Some(n) = c.data_mut().x.counts.get_mut(id as usize) {
                *n += 1;
            }
        })?;
        linker.func_wrap(
            "vyrn_check",
            "fail",
            |mut c: Caller<'_, Host>, id: i32| -> wasmtime::Result<()> {
                let row = c
                    .data()
                    .x
                    .checks
                    .get(id as usize)
                    .map_or(String::new(), |r| r.replace('\t', " "));
                let msg = trap::line(&format!("{}: {row}", trap::PROVED_CHECK_FAILED));
                c.data_mut().wasi.write_err(msg.as_bytes());
                Err(wasi::Exit(1).into())
            },
        )?;
        Ok(())
    })
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
    let code = wasi::exit_code(entry.call(&mut store, ())).map_err(|e| host_trap(&e))?;
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
        let mem = self
            .store
            .data()
            .wasi
            .mem
            .expect("memory is set before _start");
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
        match &mut self.store.data_mut().wasi.stderr {
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
        let mem = self
            .store
            .data()
            .wasi
            .mem
            .expect("memory is set before _start");
        let data = mem.data_mut(&mut self.store);
        let mut write = || -> Option<()> {
            wasi::wr32(data, base, n as u32)?;
            wasi::wr32(data, base + 4, n as u32)?;
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
        let mem = self
            .store
            .data()
            .wasi
            .mem
            .expect("memory is set before _start");
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
        match trap::split(&self.drain_err()).1 {
            Some(msg) => msg.to_string(),
            None => format!("{door}: {}", host_trap(&e)),
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
            Ok(r) => match wasi::exit_code(r) {
                Ok(0) => Ok(()),
                Ok(code) => Err(format!("exit {code}")),
                Err(e) => Err(host_trap(&e)),
            },
        };
        let said = self.drain_err();
        let Err(host) = outcome else {
            return (said, None);
        };
        let (out, msg) = trap::split(&said);
        (out.to_string(), Some(msg.map_or(host, str::to_string)))
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
fn log_checks(log: &Path, program: &str, host: &Oracle) -> Result<(), String> {
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
            expansions: vyrn_frontend::project::Expansions::shared(),
            ..Default::default()
        };
        let mut program =
            vyrn_lower::load(PROBE, "probe.vyrn", &opts, &files, None).expect("the probe loads");
        let diags = vyrn_lower::check_and_synthesize(&mut program, None);
        assert!(diags.is_empty(), "the probe checks: {diags:?}");
        let world = vyrn_lower::analyze(&program);
        vyrn_codegen::direct::compile(&program, world).expect("the probe compiles")
    }

    fn quiet() -> Run {
        Run {
            argv: vec!["probe.vyrn".to_string()],
            capture_stdout: true,
            capture_stderr: true,
            ..Run::default()
        }
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
