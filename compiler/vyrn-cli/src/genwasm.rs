//! RFC-0076 — running generators as compiled wasm instead of interpreting them.
//!
//! A `gen fn` is ordinary Vyrn, and the interpreter walks it. `std/vyx` compiles
//! a page by scanning bytes and accumulating output, which measured about 165x
//! slower walked than compiled. This module compiles the generator instead.
//!
//! The plan, validated by hand before any of this was written (RFC-0076 §M1
//! validation): clear `is_gen` on the target function, synthesize a `main` that
//! calls it with the constant arguments and prints the result, compile that to
//! wasm, and take stdout as the module source. Both routes hashed identically.
//!
//! Why swapping engines is safe here at all: the sacred invariant is that
//! interp == native == wasm, byte-identical including traps, proven over every
//! example on every commit. A generator is a Vyrn program, so that invariant is
//! exactly the correctness condition this needs.
//!
//! M1 serves only capability-free generators. Anything else is declined and the
//! interpreter runs it, so this path can make generation faster but never
//! different.

use std::path::{Path, PathBuf};

use vyrn_frontend::ast::{Block, Expr, Function, Param, Program, Stmt, Type};
use vyrn_frontend::consteval::ConstVal;
use vyrn_frontend::interp::{GenInputs, GenOutput};

/// Capabilities M1 cannot serve. A generator reaching any of these is handed
/// back to the interpreter — see [`engine`].
const MEDIATED: &[&str] = &[
    "readFile",
    "readFileBytes",
    "listDir",
    "moduleInterface",
    "writeFile",
    "writeAtomic",
    "renameFile",
    "fsyncFile",
];

/// Install the wasm generation engine (RFC-0076). Called once from `main`.
pub fn install() {
    vyrn_frontend::interp::set_gen_engine(Box::new(engine));
}

/// Claim a generation run, or decline it.
///
/// Declining returns `None`, which the frontend routes to the interpreter. That
/// is the whole fallback story: a generator this path cannot handle is slower,
/// never broken.
fn engine(
    program: &Program,
    fn_name: &str,
    args: &[ConstVal],
    inputs: &GenInputs<'_>,
) -> Option<Result<GenOutput, String>> {
    if reaches_capability(program) {
        return None;
    }
    match run(program, fn_name, args, inputs) {
        Err(EngineError::Unsupported) => None,
        Err(EngineError::Failed(e)) => Some(Err(e)),
        Ok(out) => Some(Ok(out)),
    }
}

/// `VYRN_GENWASM_TRACE=1` — per-phase timings on stderr. The only way to tell a
/// compile from a cache hit from an execution without guessing.
fn trace(phase: &str, d: std::time::Duration) {
    if std::env::var("VYRN_GENWASM_TRACE").is_ok() {
        eprintln!("genwasm {phase}: {} ms", d.as_millis());
    }
}

enum EngineError {
    /// This path cannot serve the generator; the interpreter should.
    Unsupported,
    /// The generator itself failed, and the interpreter would fail too.
    Failed(String),
}

/// Whether any function in the program calls a capability M1 does not implement.
///
/// Reuses the checker's `fn_calls` — the same walker the comptime-purity check
/// uses to ask what a generator reaches. Deliberately whole-program rather than
/// reachability-precise: if a mediated call appears anywhere in the generator's
/// module closure, decline. Being wrong in this direction costs speed; being
/// wrong in the other would run a generator outside its sandbox.
fn reaches_capability(program: &Program) -> bool {
    program.functions.iter().any(|f| {
        vyrn_frontend::checker::fn_calls(&f.body)
            .iter()
            .any(|c| MEDIATED.contains(&c.as_str()))
    })
}

fn run(
    program: &Program,
    fn_name: &str,
    args: &[ConstVal],
    inputs: &GenInputs<'_>,
) -> Result<GenOutput, EngineError> {
    // String-only, because the arguments travel as argv rather than being baked
    // into the module — which is what lets ONE compiled artifact serve every
    // call. Every generator in this repo takes constant paths and names.
    let argv: Vec<String> = args
        .iter()
        .map(|a| match a {
            ConstVal::Str(s) => Some(s.clone()),
            _ => None,
        })
        .collect::<Option<Vec<_>>>()
        .ok_or(EngineError::Unsupported)?;

    let t = std::time::Instant::now();
    let key = artifact_key(program, fn_name);
    trace("key", t.elapsed());

    let t = std::time::Instant::now();
    let mut source = run_module(key, &argv, || {
        let wrapper = wrapper_program(program, fn_name, argv.len()).ok_or(EngineError::Unsupported)?;
        compile_to_wasm(key, &wrapper)
    })?;
    trace("run", t.elapsed());
    // `print` appends a newline; the generator's own source did not have it.
    if source.ends_with('\n') {
        source.pop();
    }
    if source.len() > inputs.max_output {
        return Err(EngineError::Failed(format!(
            "generator output exceeds the {} byte cap",
            inputs.max_output
        )));
    }
    // M1 serves only capability-free generators, so nothing was read.
    Ok(GenOutput { source, reads: Vec::new() })
}

/// The program actually compiled: the generator's own module with `is_gen`
/// cleared on the target, plus a `main` that calls it with `args()` and prints
/// the result.
///
/// Clearing `is_gen` is what makes it compilable — a `gen fn` is comptime-only
/// by construction. Everything else about the function is untouched, which is
/// why the output matches.
///
/// The arguments come from `args()` (argv[1..], RFC-0014) rather than being
/// baked in as literals, so the artifact does not depend on them: `std/vyx`
/// compiles ten pages with one compilation, not ten.
fn wrapper_program(program: &Program, fn_name: &str, arity: usize) -> Option<Program> {
    // A generator module with its own `main` would collide with the synthesized
    // one. Rare enough not to be worth renaming around.
    if program.functions.iter().any(|f| f.name == "main") {
        return None;
    }
    let mut p = program.clone();
    let target = p.functions.iter_mut().find(|f| f.name == fn_name)?;
    if target.params.len() != arity {
        return None;
    }
    if target.params.iter().any(|par| par.ty != Type::Str) {
        return None;
    }
    target.is_gen = false;

    let call = Expr::Call {
        name: fn_name.to_string(),
        // `args()[i]` — the parser's own desugar for indexing.
        args: (0..arity)
            .map(|i| Expr::Call {
                name: "at".to_string(),
                args: vec![
                    Expr::Call { name: "args".to_string(), args: vec![], line: 0 },
                    Expr::Int(i as i64),
                ],
                line: 0,
            })
            .collect(),
        line: 0,
    };
    p.functions.push(Function {
        name: "main".to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        type_bounds: Default::default(),
        params: Vec::<Param>::new(),
        ret: Type::Int,
        body: Block {
            stmts: vec![
                Stmt::Expr(Expr::Call { name: "print".to_string(), args: vec![call], line: 0 }),
                Stmt::Return { value: Some(Expr::Int(0)), line: 0 },
            ],
        },
        line: 0,
        is_extern: false,
        is_export_extern: false,
        is_gen: false,
    });
    Some(p)
}

/// Emit IR, compile it against wasi-libc, and read back the module bytes.
///
/// Deliberately its own orchestration rather than a refactor of `build`: that
/// function owns argument parsing, output naming, native/wasm selection and its
/// own error reporting, none of which belongs on this path. The pieces that
/// matter — clang, the sysroot, the builtins archive, the runtime shim — are the
/// same ones `build` uses.
fn compile_to_wasm(key: u64, program: &Program) -> Result<Vec<u8>, EngineError> {
    let ir = vyrn_codegen::emit(program).map_err(|_| EngineError::Unsupported)?;

    let dir = std::env::temp_dir().join(format!("vyrn-genwasm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| EngineError::Failed(e.to_string()))?;
    let ll = dir.join(format!("{key:016x}.ll"));
    let shim = dir.join(format!("{key:016x}.shim.c"));
    let out = dir.join(format!("{key:016x}.wasm"));
    std::fs::write(&ll, ir).map_err(|e| EngineError::Failed(e.to_string()))?;
    // No extern trap stubs: this is a wasm build, and a generator cannot call
    // `extern` anyway (comptime purity forbids it).
    std::fs::write(&shim, crate::RUNTIME_SHIM).map_err(|e| EngineError::Failed(e.to_string()))?;

    let clang = crate::find_clang().ok_or(EngineError::Unsupported)?;
    let sysroot = wasi_sysroot().ok_or(EngineError::Unsupported)?;
    let builtins = wasi_builtins(&sysroot).ok_or(EngineError::Unsupported)?;

    let st = std::process::Command::new(clang)
        .arg(&ll)
        .arg(&shim)
        .arg("-o")
        .arg(&out)
        .arg("-Wno-override-module")
        .arg("--target=wasm32-wasip1")
        .arg(format!("--sysroot={}", sysroot.display()))
        .arg("-nodefaultlibs")
        .arg(&builtins)
        .arg("-lc")
        .output()
        .map_err(|_| EngineError::Unsupported)?;
    if !st.status.success() {
        // The generator runs under the interpreter; if it will not compile here
        // that is this path's problem, not the program's.
        return Err(EngineError::Unsupported);
    }
    std::fs::read(&out).map_err(|e| EngineError::Failed(e.to_string()))
}

fn wasi_sysroot() -> Option<PathBuf> {
    match std::env::var("WASI_SYSROOT") {
        Ok(s) if Path::new(&s).exists() => Some(PathBuf::from(s)),
        _ => crate::discovered_wasi_sysroot(),
    }
}

fn wasi_builtins(sysroot: &Path) -> Option<PathBuf> {
    match std::env::var("WASI_BUILTINS") {
        Ok(b) if Path::new(&b).exists() => Some(PathBuf::from(b)),
        _ => crate::builtins_near_sysroot(sysroot),
    }
}

// ---------------------------------------------------------------------------
// The runtime: an embedded wasmtime plus a hand-written minimal WASI.
// ---------------------------------------------------------------------------

/// The guest's world: its argv, and what it wrote.
#[derive(Default)]
struct Streams {
    /// NUL-terminated argv, argv[0] first — the shape `args_get` writes.
    argv: Vec<Vec<u8>>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
}

/// `proc_exit`, carried out of the guest as an error because that is the only
/// way to stop it.
#[derive(Debug)]
struct Exit(i32);

impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit {}", self.0)
    }
}

impl std::error::Error for Exit {}

const ERRNO_SUCCESS: i32 = 0;
const ERRNO_BADF: i32 = 8;
const ERRNO_SPIPE: i32 = 29;

fn wr32(data: &mut [u8], at: i32, v: u32) -> Option<()> {
    let at = at as usize;
    data.get_mut(at..at + 4)?.copy_from_slice(&v.to_le_bytes());
    Some(())
}

fn rd32(data: &[u8], at: i32) -> Option<u32> {
    let at = at as usize;
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// What a compiled artifact is keyed on: the generator's whole module closure
/// and the function. NOT the arguments — those arrive as argv, so one artifact
/// serves every call, which is what makes a cache hit the common case.
///
/// A hit skips clang AND cranelift. Hashed straight out of `Debug` into the
/// hasher, so nothing is materialized to hash it.
fn artifact_key(program: &Program, fn_name: &str) -> u64 {
    use std::fmt::Write as _;

    struct Sink(u64);
    impl std::fmt::Write for Sink {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            for b in s.as_bytes() {
                self.0 = (self.0 ^ *b as u64).wrapping_mul(0x0100_0000_01b3);
            }
            Ok(())
        }
    }

    let mut sink = Sink(0xcbf2_9ce4_8422_2325);
    let _ = write!(sink, "{fn_name}\u{0}{program:?}");
    sink.0
}

/// One wasmtime engine and one compiled module per artifact, for the process.
///
/// Cranelift compilation is the expensive half of instantiation, and a `Module`
/// is an `Arc` internally, so caching it is what turns a repeat generation into
/// milliseconds.
fn wasm_engine() -> &'static wasmtime::Engine {
    static ENGINE: std::sync::OnceLock<wasmtime::Engine> = std::sync::OnceLock::new();
    ENGINE.get_or_init(wasmtime::Engine::default)
}

fn module_cache() -> &'static std::sync::Mutex<std::collections::HashMap<u64, wasmtime::Module>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<u64, wasmtime::Module>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Compile (once) and run, returning what the guest wrote to stdout.
fn run_module(
    key: u64,
    argv: &[String],
    build: impl FnOnce() -> Result<Vec<u8>, EngineError>,
) -> Result<String, EngineError> {
    let cached = module_cache().lock().ok().and_then(|c| c.get(&key).cloned());
    let module = match cached {
        Some(m) => m,
        None => {
            let t = std::time::Instant::now();
            let bytes = build()?;
            trace("clang", t.elapsed());
            let t = std::time::Instant::now();
            let m = wasmtime::Module::new(wasm_engine(), &bytes)
                .map_err(|e| EngineError::Failed(format!("wasm: {e}")))?;
            trace("cranelift", t.elapsed());
            if let Ok(mut c) = module_cache().lock() {
                c.insert(key, m.clone());
            }
            m
        }
    };
    run_wasm(&module, argv)
}

/// Instantiate the module and return what it wrote to stdout.
///
/// Embedded rather than spawned, and that is not an optimization: measured, the
/// wasmtime CLI's process launch is ~106 ms and precompiling does not reduce it,
/// so a subprocess per generator call is SLOWER than interpreting a small one.
/// The whole point of this path is that instantiation replaces process spawn.
///
/// WASI is hand-written rather than taken from `wasmtime-wasi`, following the
/// precedent already in this repo: `web/wasi-min.js` is the same shim for the
/// browser. A capability-free generator only ever writes to stdout, so the
/// surface is small, and every import outside it traps rather than existing.
fn run_wasm(module: &wasmtime::Module, argv: &[String]) -> Result<String, EngineError> {
    use wasmtime::*;

    let engine = wasm_engine();
    let mut linker: Linker<Streams> = Linker::new(engine);
    let wasi = "wasi_snapshot_preview1";

    // fd_write(fd, iovs, iovs_len, nwritten) — the only import that does work.
    linker
        .func_wrap(
            wasi,
            "fd_write",
            |mut caller: Caller<'_, Streams>, fd: i32, iovs: i32, iovs_len: i32, nwritten: i32| {
                let Some(Extern::Memory(mem)) = caller.get_export("memory") else {
                    return ERRNO_BADF;
                };
                if fd != 1 && fd != 2 {
                    return ERRNO_BADF;
                }
                let (data, streams) = mem.data_and_store_mut(&mut caller);
                let mut text = Vec::new();
                let mut written = 0u32;
                for i in 0..iovs_len {
                    let head = iovs + i * 8;
                    let (Some(base), Some(len)) = (rd32(data, head), rd32(data, head + 4)) else {
                        return ERRNO_BADF;
                    };
                    let Some(chunk) = data.get(base as usize..(base + len) as usize) else {
                        return ERRNO_BADF;
                    };
                    text.extend_from_slice(chunk);
                    written += len;
                }
                if fd == 1 {
                    streams.stdout.extend_from_slice(&text);
                } else {
                    streams.stderr.extend_from_slice(&text);
                }
                match wr32(data, nwritten, written) {
                    Some(()) => ERRNO_SUCCESS,
                    None => ERRNO_BADF,
                }
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;

    // fd_fdstat_get — wasi-libc asks what stdout is before writing to it; a
    // character device (a tty) is the answer that makes it unbuffered-safe.
    linker
        .func_wrap(
            wasi,
            "fd_fdstat_get",
            |mut caller: Caller<'_, Streams>, fd: i32, buf: i32| {
                let Some(Extern::Memory(mem)) = caller.get_export("memory") else {
                    return ERRNO_BADF;
                };
                if fd > 2 {
                    return ERRNO_BADF;
                }
                let (data, _) = mem.data_and_store_mut(&mut caller);
                let Some(slot) = data.get_mut(buf as usize..buf as usize + 24) else {
                    return ERRNO_BADF;
                };
                slot.fill(0);
                slot[0] = 2; // filetype: character_device
                ERRNO_SUCCESS
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;

    linker
        .func_wrap(wasi, "proc_exit", |code: i32| -> Result<()> {
            Err(Error::new(Exit(code)))
        })
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    linker
        .func_wrap(wasi, "fd_close", |_: i32| ERRNO_SUCCESS)
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    linker
        .func_wrap(wasi, "fd_seek", |_: i32, _: i64, _: i32, _: i32| ERRNO_SPIPE)
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    // args_sizes_get / args_get — how the generator's arguments reach it. The
    // artifact is argument-independent precisely because these are real.
    linker
        .func_wrap(
            wasi,
            "args_sizes_get",
            |mut caller: Caller<'_, Streams>, count: i32, size: i32| {
                let Some(Extern::Memory(mem)) = caller.get_export("memory") else {
                    return ERRNO_BADF;
                };
                let (data, streams) = mem.data_and_store_mut(&mut caller);
                let n = streams.argv.len() as u32;
                let bytes: u32 = streams.argv.iter().map(|a| a.len() as u32).sum();
                match (wr32(data, count, n), wr32(data, size, bytes)) {
                    (Some(()), Some(())) => ERRNO_SUCCESS,
                    _ => ERRNO_BADF,
                }
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    linker
        .func_wrap(
            wasi,
            "args_get",
            |mut caller: Caller<'_, Streams>, ptrs: i32, buf: i32| {
                let Some(Extern::Memory(mem)) = caller.get_export("memory") else {
                    return ERRNO_BADF;
                };
                let (data, streams) = mem.data_and_store_mut(&mut caller);
                let argv = std::mem::take(&mut streams.argv);
                let mut at = buf;
                for (i, arg) in argv.iter().enumerate() {
                    let Some(slot) = data.get_mut(at as usize..at as usize + arg.len()) else {
                        return ERRNO_BADF;
                    };
                    slot.copy_from_slice(arg);
                    if wr32(data, ptrs + i as i32 * 4, at as u32).is_none() {
                        return ERRNO_BADF;
                    }
                    at += arg.len() as i32;
                }
                let (_, streams) = mem.data_and_store_mut(&mut caller);
                streams.argv = argv;
                ERRNO_SUCCESS
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    // No environment: a generator gets none, under either engine.
    linker
        .func_wrap(
            wasi,
            "environ_sizes_get",
            |mut caller: Caller<'_, Streams>, count: i32, size: i32| {
                let Some(Extern::Memory(mem)) = caller.get_export("memory") else {
                    return ERRNO_BADF;
                };
                let (data, _) = mem.data_and_store_mut(&mut caller);
                match (wr32(data, count, 0), wr32(data, size, 0)) {
                    (Some(()), Some(())) => ERRNO_SUCCESS,
                    _ => ERRNO_BADF,
                }
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    linker
        .func_wrap(wasi, "environ_get", |_: i32, _: i32| ERRNO_SUCCESS)
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    // Anything else — a filesystem or clock import — is a generator this path
    // said it would not serve, so it traps loudly instead of silently lying.
    linker
        .define_unknown_imports_as_traps(module)
        .map_err(|e| EngineError::Failed(e.to_string()))?;

    // argv[0] is the program name, which `args()` (argv[1..]) skips.
    let mut world = Streams::default();
    world.argv.push(b"gen\0".to_vec());
    world
        .argv
        .extend(argv.iter().map(|a| [a.as_bytes(), b"\0"].concat()));

    let mut store = Store::new(engine, world);
    let inst = linker
        .instantiate(&mut store, module)
        .map_err(|e| EngineError::Failed(format!("instantiate: {e}")))?;
    let start = inst
        .get_typed_func::<(), ()>(&mut store, "_start")
        .map_err(|e| EngineError::Failed(format!("_start: {e}")))?;

    let result = start.call(&mut store, ());
    let streams = store.into_data();
    match result {
        Ok(()) => {}
        Err(e) => match e.downcast_ref::<Exit>() {
            Some(Exit(0)) => {}
            // The guest failed on its own terms — its message is already on
            // stderr, in the canonical wording both engines share.
            Some(Exit(code)) => {
                let msg = String::from_utf8_lossy(&streams.stderr).trim_end().to_string();
                return Err(EngineError::Failed(if msg.is_empty() {
                    format!("generator exited with {code}")
                } else {
                    msg
                }));
            }
            None => return Err(EngineError::Failed(format!("generator trapped: {e}"))),
        },
    }
    String::from_utf8(streams.stdout)
        .map_err(|_| EngineError::Failed("generator emitted invalid UTF-8".into()))
}
