//! RFC-0076 — running generators as compiled wasm instead of interpreting them.
//!
//! A `gen fn` is ordinary Vyrn, and the interpreter walks it. `std/vyx` compiles
//! a page by scanning bytes and accumulating output, which measured about 165x
//! slower walked than compiled. This module compiles the generator instead.
//!
//! The plan, validated by hand before any of this was written (RFC-0076 §M1
//! validation): clear `is_gen`, synthesize a `main` that calls the generator
//! with the constant arguments and prints the result, compile that to wasm, and
//! take stdout as the module source. Both routes hashed identically.
//!
//! Why swapping engines is safe here at all: the sacred invariant is that
//! interp == native == wasm, byte-identical including traps, proven over every
//! example on every commit. A generator is a Vyrn program, so that invariant is
//! exactly the correctness condition this needs.
//!
//! M2 adds the byte capabilities. A generator does NOT read the filesystem — it
//! reads through `GenInputs.resolver`, which in the LSP serves unsaved buffers
//! and elsewhere serves vendored or remote modules. So `readFile`/`listDir` are
//! host imports backed by that resolver, mediated by the same
//! [`vyrn_frontend::interp::gen_scoped_path`] the interpreter uses and recorded
//! into `GenOutput.reads` the same way, which is what the on-disk generator
//! cache validates against.
//!
//! M3b adds the structured results. `lex`, `moduleInterface` and `contractOf`
//! each hand back a value of a known named type, so there is ONE transfer: the
//! host encodes by walking the static type, and a decoder synthesized here (as
//! ordinary Vyrn, not IR) walks the same type pulling it back. Both walks read
//! the same `record_fields`, which is what makes them agree — there is no
//! self-describing format for two implementations to read differently.
//!
//! Anything still unserved is declined and the interpreter runs it, so this path
//! can make generation faster but never different.
//!
//! M4 gave it this crate. It started inside `vyrn-cli`, which is a binary, so
//! `vyrn-lsp` could not reach it — and the LSP is the whole point: a compiled
//! artifact is argument-independent and cached for the process, so a long-lived
//! one pays clang once per generator instead of once per call. Excluded from the
//! default workspace like `vyrn-lsp` and `vyrn-codegen-llvm`, for the same
//! reason: `wasmtime` is an external dependency and `cargo build` must not need
//! one.

use std::path::{Path, PathBuf};
use std::sync::mpsc;

use vyrn_frontend::ast::{Block, Expr, Function, Param, Program, Stmt, Type};
use vyrn_frontend::consteval::ConstVal;
use vyrn_frontend::interp::{CodePiece, GenInputs, GenOutput};

/// What this path cannot serve. A generator reaching any of these is handed back
/// to the interpreter — see [`engine`].
///
/// The RFC-0054 code quotes left this list in M3a (`Code` became an opaque
/// handle into a host-side arena) and `lex`/`moduleInterface`/`contractOf` left
/// it in M3b (they became a host encoder and a synthesized decoder that both
/// walk the static type).
///
/// The write capabilities are not a milestone at all: a `gen fn` may not call
/// them (comptime purity forbids it), so their only effect here is to decline a
/// module that merely CONTAINS one somewhere.
const UNSERVED: &[&str] = &["writeFile", "writeAtomic", "renameFile", "fsyncFile"];

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
    if let Some(what) = reaches_unserved(program) {
        decline(&format!("the module reaches `{what}`"));
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
        // Fractions of a millisecond, because the phases now differ by three
        // orders of magnitude: a cache-hit `run` and the `key` that guards it
        // both round to 0 ms, and the key is what every keystroke pays.
        eprintln!("genwasm {phase}: {:.2} ms", d.as_secs_f64() * 1000.0);
    }
}

/// Decline, saying why under `VYRN_GENWASM_TRACE`. A decline is invisible by
/// design — the interpreter just runs the generator — so without this the only
/// symptom of a broken engine is that it silently never runs.
fn decline(why: &str) -> EngineError {
    if std::env::var("VYRN_GENWASM_TRACE").is_ok() {
        eprintln!("genwasm declined: {why}");
    }
    EngineError::Unsupported
}

enum EngineError {
    /// This path cannot serve the generator; the interpreter should.
    Unsupported,
    /// The generator itself failed, and the interpreter would fail too.
    Failed(String),
}

/// Whether any function in the program calls something this path cannot lower.
///
/// Reuses the checker's `fn_calls` — the same walker the comptime-purity check
/// uses to ask what a generator reaches. Deliberately whole-program rather than
/// reachability-precise: if an unserved call appears anywhere in the generator's
/// module closure, decline. Being wrong in this direction costs speed; being
/// wrong in the other would run a generator outside its sandbox.
fn reaches_unserved(program: &Program) -> Option<String> {
    program.functions.iter().find_map(|f| {
        let mut hits: Vec<String> = vyrn_frontend::checker::fn_calls(&f.body)
            .into_iter()
            .filter(|c| UNSERVED.contains(&c.as_str()))
            .collect();
        // `fn_calls` returns a set; sort so the reported blocker is stable.
        hits.sort();
        hits.into_iter().next()
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
        .ok_or_else(|| decline("a non-String constant argument"))?;

    let t = std::time::Instant::now();
    let key = artifact_key(program, fn_name, inputs.sources_fingerprint.as_deref());
    trace("key", t.elapsed());

    let t = std::time::Instant::now();
    let mut reads = Vec::new();
    // Only a fingerprinted key describes the generator well enough to trust a
    // file written by a previous process — see `artifact_key`.
    let persist = inputs.sources_fingerprint.is_some();
    let mut source = run_module(&key, persist, &argv, program, inputs, &mut reads, || {
        let wrapper = wrapper_program(program, fn_name, argv.len())
            .ok_or_else(|| decline("the wrapper program cannot be synthesized"))?;
        compile_to_wasm(&key, &wrapper)
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
    Ok(GenOutput { source, reads })
}

/// Serve one mediated capability request from the guest, exactly as
/// `Interp::gen_read_file` / `gen_list_dir` do: scope the path, go through the
/// resolver, record the bytes, and answer in the status alphabet the compiled
/// caller already renders errors from (0 ok / 1 io / 3 embedded NUL).
///
/// `Err` is a scoping violation, which must abort generation rather than become
/// a value the generator can observe — the guest never sees it.
fn serve(
    inputs: &GenInputs<'_>,
    reads: &mut Vec<(String, Vec<u8>)>,
    path: &str,
    mode: i32,
) -> Result<Served, String> {
    // `moduleInterface` links the reflected module and records EVERY module the
    // link touched, which is what makes editing a closure type's defining file
    // miss the generator cache (RFC-0031). The interpreter's own implementation,
    // called here, so the recorded reads cannot differ by engine.
    if mode == MODE_MODULE_INTERFACE {
        return vyrn_frontend::interp::gen_module_interface_lit(
            inputs.resolver,
            inputs.opts,
            &inputs.importer_dir,
            &inputs.allowed,
            reads,
            path,
        )
        .map(|lit| Served::Lit(Box::new(lit)));
    }
    let resolved =
        vyrn_frontend::interp::gen_scoped_path(&inputs.importer_dir, &inputs.allowed, path)?;
    if mode == MODE_LIST {
        return match inputs.resolver.list(&resolved) {
            Ok(mut names) => {
                names.sort();
                // Recorded as a synthetic input under the directory key, in the
                // interpreter's own encoding, so a directory whose contents
                // change invalidates the same cache entry.
                let joined = names.join("\n").into_bytes();
                reads.push((format!("{resolved}/"), joined.clone()));
                Ok(Served::Bytes(0, joined))
            }
            Err(_) => Ok(Served::Bytes(1, Vec::new())),
        };
    }
    match inputs.resolver.read(&resolved) {
        Ok(content) => {
            let bytes = content.into_bytes();
            reads.push((resolved, bytes.clone()));
            // Recorded before the NUL rule rejects it, like the interpreter: the
            // file was read, and the cache must notice when it changes.
            if mode == MODE_READ && bytes.contains(&0) {
                return Ok(Served::Bytes(3, Vec::new()));
            }
            Ok(Served::Bytes(0, bytes))
        }
        Err(_) => Ok(Served::Bytes(1, Vec::new())),
    }
}

/// The program actually compiled: the generator's own module with `is_gen`
/// cleared, plus a `main` that calls the target with `args()` and prints the
/// result.
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
    {
        let target = p.functions.iter().find(|f| f.name == fn_name)?;
        if target.params.len() != arity {
            return None;
        }
        if target.params.iter().any(|par| par.ty != Type::Str) {
            return None;
        }
        // A `gen fn` may return `Code` directly, which the interpreter renders
        // for it (RFC-0054). Here that would print the handle, so decline —
        // nothing in this repo does it, and a wrong answer is worse than a slow
        // one.
        if target.ret != Type::Str {
            return None;
        }
    }
    // Every `gen fn`, not just the target: a generator calls its helpers, and in
    // this repo those helpers are themselves `gen fn` (the convention that keeps
    // generation-only I/O out of shipped binaries). Clearing only the target
    // emitted calls into functions codegen had skipped, and the link failed —
    // which is why M1 could not have served `std/tw` even with the capabilities.
    for f in p.functions.iter_mut() {
        f.is_gen = false;
    }

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
    p.functions.push(func(
        "main",
        Vec::new(),
        Type::Int,
        vec![
            Stmt::Expr(Expr::Call { name: "print".to_string(), args: vec![call], line: 0 }),
            Stmt::Return { value: Some(Expr::Int(0)), line: 0 },
        ],
    ));
    // RFC-0076 M3b: the builtins that hand back a structured value get an entry
    // point plus the decoders it needs, synthesized before the emitter sees the
    // program (so the string pool, the ownership analysis and the array lowering
    // all cover them like any other function).
    reflect_entries(&mut p)?;
    Some(p)
}

// ---------------------------------------------------------------------------
// RFC-0076 M3b — structured host results.
//
// `lex`, `moduleInterface` and `contractOf` are one problem: each returns a
// value of a KNOWN NAMED TYPE built out of strings, ints, bools, records and
// arrays. So there is one transfer, walked from both ends. The host encoder
// (`encode`) walks the static type over the value; the decoder synthesized here
// walks the same type pulling atoms back. Neither side reads a schema and
// neither side tags anything beyond an array's length and an Option's presence,
// because the reader always knows what it is about to read.
//
// The decoders are ordinary Vyrn, not hand-written IR: the arrays, records and
// Options they build are the ones every other program gets, so a change to how
// codegen lowers a record cannot make the two walks disagree.
// ---------------------------------------------------------------------------

fn func(name: &str, params: Vec<Param>, ret: Type, stmts: Vec<Stmt>) -> Function {
    Function {
        name: name.to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        type_bounds: Default::default(),
        params,
        ret,
        body: Block { stmts },
        line: 0,
        is_extern: false,
        is_export_extern: false,
        is_gen: false,
    }
}

fn call(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Call { name: name.to_string(), args, line: 0 }
}

fn var(name: &str) -> Expr {
    Expr::Var { name: name.to_string(), line: 0 }
}

/// Append an entry point per reachable structured builtin, plus the decoders
/// they need. `None` declines the whole run — a missing declaration is a
/// generator this path will not guess at.
fn reflect_entries(p: &mut Program) -> Option<()> {
    let reaches = |what: &str| {
        p.functions
            .iter()
            .any(|f| vyrn_frontend::checker::fn_calls(&f.body).contains(what))
    };
    let named = |n: &str| Type::Named(n.to_string());
    let mut dec = Decoders::new(p);
    let mut entries: Vec<Function> = Vec::new();
    let str_param = |n: &str| Param {
        name: n.to_string(),
        capability: vyrn_frontend::ast::Capability::Read,
        ty: Type::Str,
    };
    // `fn <entry>(arg) -> T { @reflect(kind, arg); return <decode T>() }`.
    let mut entry = |name: String, params: Vec<Param>, ret: Type, kind: i64, arg: Expr, d: &mut Decoders| {
        let body = d.decode(&ret)?;
        entries.push(func(
            &name,
            params,
            ret,
            vec![
                Stmt::Expr(call(vyrn_codegen::GEN_REFLECT, vec![Expr::Int(kind), arg])),
                Stmt::Return { value: Some(body), line: 0 },
            ],
        ));
        Some(())
    };

    if reaches("moduleInterface") {
        entry(
            vyrn_codegen::GEN_ENTRY_MODULE_INTERFACE.to_string(),
            vec![str_param("path")],
            named("ModuleInterface"),
            vyrn_codegen::REFLECT_MODULE_INTERFACE,
            var("path"),
            &mut dec,
        )?;
    }
    // `lex` is a common word and therefore shadowable (the checker's rule): a
    // user function of the same name wins, and emitting no entry for it is what
    // makes codegen leave that call site alone.
    if reaches("lex") && !p.functions.iter().any(|f| f.name == "lex") {
        entry(
            vyrn_codegen::GEN_ENTRY_LEX.to_string(),
            vec![str_param("src")],
            Type::Array(Box::new(named("Token"))),
            vyrn_codegen::REFLECT_LEX,
            var("src"),
            &mut dec,
        )?;
    }
    // A contract name is a declaration, not a value, so the entry is per-contract
    // and nullary. One per declared contract rather than one per call site: there
    // are a handful in a module closure, and finding the call sites would mean a
    // second walk of the whole AST to learn what codegen already knows.
    if reaches("contractOf") {
        for name in p.contracts.iter().map(|c| c.name.clone()).collect::<Vec<_>>() {
            entry(
                format!("{}{name}", vyrn_codegen::GEN_ENTRY_CONTRACT_OF),
                Vec::new(),
                named("ContractInfo"),
                vyrn_codegen::REFLECT_CONTRACT_OF,
                Expr::Str(name),
                &mut dec,
            )?;
        }
    }
    p.functions.extend(entries);
    p.functions.extend(dec.fns);
    Some(())
}

/// The synthesized decoders, one per composite type, memoized by name.
struct Decoders {
    types: std::collections::HashMap<String, vyrn_frontend::ast::TypeDecl>,
    fns: Vec<Function>,
    made: std::collections::HashSet<String>,
}

impl Decoders {
    fn new(p: &Program) -> Self {
        Decoders {
            types: p.type_decls.iter().map(|t| (t.name.clone(), t.clone())).collect(),
            fns: Vec::new(),
            made: std::collections::HashSet::new(),
        }
    }

    /// The expression that decodes one value of `ty`. Scalars are the stream
    /// primitives inline; everything else is a call to a decoder function this
    /// materializes on demand.
    fn decode(&mut self, ty: &Type) -> Option<Expr> {
        Some(match vyrn_frontend::types::resolve(ty, &self.types) {
            Type::Str => call(vyrn_codegen::GEN_NEXT_STR, vec![]),
            Type::Int | Type::IntN { .. } => call(vyrn_codegen::GEN_NEXT_INT, vec![]),
            Type::Bool => Expr::Binary {
                op: vyrn_frontend::ast::BinOp::Eq,
                lhs: Box::new(call(vyrn_codegen::GEN_NEXT_INT, vec![])),
                rhs: Box::new(Expr::Int(1)),
                line: 0,
            },
            _ => {
                let name = self.materialize(ty)?;
                call(&name, vec![])
            }
        })
    }

    /// Emit (once) the decoder for a composite type and return its name.
    fn materialize(&mut self, ty: &Type) -> Option<String> {
        let name = format!("__vyrnGenDec_{}", mangle(ty)?);
        if !self.made.insert(name.clone()) {
            return Some(name);
        }
        let body = match vyrn_frontend::types::resolve(ty, &self.types) {
            // The length, then that many elements.
            Type::Array(inner) => {
                let elem = self.decode(&inner)?;
                vec![
                    Stmt::Let {
                        name: "n".into(),
                        mutable: false,
                        ty: Some(Type::Int),
                        value: call(vyrn_codegen::GEN_NEXT_INT, vec![]),
                        line: 0,
                    },
                    Stmt::Let {
                        name: "xs".into(),
                        mutable: true,
                        ty: Some(ty.clone()),
                        value: Expr::ArrayLit { elems: Vec::new(), line: 0 },
                        line: 0,
                    },
                    Stmt::Let {
                        name: "i".into(),
                        mutable: true,
                        ty: Some(Type::Int),
                        value: Expr::Int(0),
                        line: 0,
                    },
                    Stmt::While {
                        cond: Expr::Binary {
                            op: vyrn_frontend::ast::BinOp::Lt,
                            lhs: Box::new(var("i")),
                            rhs: Box::new(var("n")),
                            line: 0,
                        },
                        body: Block {
                            stmts: vec![
                                // The parser's own write-back for a statement
                                // `xs.push(v)`: push returns the reallocated
                                // triple, so it must be stored back.
                                Stmt::Assign {
                                    name: "xs".into(),
                                    value: call("push", vec![var("xs"), elem]),
                                    line: 0,
                                },
                                Stmt::Assign {
                                    name: "i".into(),
                                    value: Expr::Binary {
                                        op: vyrn_frontend::ast::BinOp::Add,
                                        lhs: Box::new(var("i")),
                                        rhs: Box::new(Expr::Int(1)),
                                        line: 0,
                                    },
                                    line: 0,
                                },
                            ],
                        },
                        line: 0,
                    },
                    Stmt::Return { value: Some(var("xs")), line: 0 },
                ]
            }
            // One tag atom, then the payload only when it is there.
            Type::Option(inner) => {
                let some = self.decode(&inner)?;
                vec![
                    Stmt::If {
                        cond: Expr::Binary {
                            op: vyrn_frontend::ast::BinOp::Eq,
                            lhs: Box::new(call(vyrn_codegen::GEN_NEXT_INT, vec![])),
                            rhs: Box::new(Expr::Int(1)),
                            line: 0,
                        },
                        then_block: Block {
                            stmts: vec![Stmt::Return {
                                value: Some(call("Some", vec![some])),
                                line: 0,
                            }],
                        },
                        else_block: None,
                        line: 0,
                    },
                    Stmt::Return { value: Some(var("None")), line: 0 },
                ]
            }
            // Fields in the DECLARATION's order, which is the order the host
            // pushed them — both sides read `record_fields`, so there is no field
            // -order convention for either to remember.
            Type::Record(_) => {
                let Type::Named(rec) = ty else { return None };
                let fields = vyrn_frontend::types::record_fields(ty, &self.types)?;
                let mut lit = Vec::new();
                for f in &fields {
                    lit.push((f.name.clone(), self.decode(&f.ty)?));
                }
                vec![Stmt::Return {
                    value: Some(Expr::StructLit {
                        name: rec.clone(),
                        fields: lit,
                        line: 0,
                    }),
                    line: 0,
                }]
            }
            _ => return None,
        };
        self.fns.push(func(&name, Vec::new(), ty.clone(), body));
        Some(name)
    }
}

/// A decoder's name suffix. Only the shapes the reflected types are built from —
/// anything else declines rather than being encoded by accident.
fn mangle(ty: &Type) -> Option<String> {
    Some(match ty {
        Type::Named(n) => n.clone(),
        Type::Array(t) => format!("Arr_{}", mangle(t)?),
        Type::Option(t) => format!("Opt_{}", mangle(t)?),
        Type::Str => "Str".into(),
        Type::Int => "Int".into(),
        Type::Bool => "Bool".into(),
        _ => return None,
    })
}

/// Emit IR, compile it against wasi-libc, and read back the module bytes.
///
/// Deliberately its own orchestration rather than a refactor of `build`: that
/// function owns argument parsing, output naming, native/wasm selection and its
/// own error reporting, none of which belongs on this path. The pieces that
/// matter — clang, the sysroot, the builtins archive, the runtime shim — are the
/// same ones `build` uses.
fn compile_to_wasm(key: &str, program: &Program) -> Result<Vec<u8>, EngineError> {
    // `emit_gen_host`, not `emit`: the same emitter, plus the one lowering that
    // only makes sense with the host imports below it (`listDir`).
    let ir = vyrn_codegen::emit_gen_host(program).map_err(|e| decline(&format!("codegen: {e}")))?;

    let dir = std::env::temp_dir().join(format!("vyrn-genwasm-{}", std::process::id()));
    std::fs::create_dir_all(&dir).map_err(|e| EngineError::Failed(e.to_string()))?;
    let ll = dir.join(format!("{key}.ll"));
    let shim = dir.join(format!("{key}.shim.c"));
    let out = dir.join(format!("{key}.wasm"));
    std::fs::write(&ll, ir).map_err(|e| EngineError::Failed(e.to_string()))?;
    // No extern trap stubs: this is a wasm build, and a generator cannot call
    // `extern` anyway (comptime purity forbids it).
    std::fs::write(&shim, vyrn_codegen::toolchain::RUNTIME_SHIM).map_err(|e| EngineError::Failed(e.to_string()))?;

    let clang = vyrn_codegen::toolchain::find_clang().ok_or_else(|| decline("no clang"))?;
    let sysroot = wasi_sysroot().ok_or_else(|| decline("no wasi sysroot"))?;
    let builtins = wasi_builtins(&sysroot).ok_or_else(|| decline("no wasi builtins"))?;

    let st = std::process::Command::new(clang)
        .arg(&ll)
        .arg(&shim)
        .arg("-o")
        .arg(&out)
        .arg("-Wno-override-module")
        // Swaps the shim's stdio reads for the resolver-backed host imports.
        // Only this path defines it; an ordinary `vyrn build` is untouched.
        .arg("-DVYRN_GEN_HOST")
        .arg("--target=wasm32-wasip1")
        .arg(format!("--sysroot={}", sysroot.display()))
        .arg("-nodefaultlibs")
        .arg(&builtins)
        .arg("-lc")
        .output()
        .map_err(|e| decline(&format!("clang: {e}")))?;
    if !st.status.success() {
        // The generator runs under the interpreter; if it will not compile here
        // that is this path's problem, not the program's.
        return Err(decline(&String::from_utf8_lossy(&st.stderr)));
    }
    std::fs::read(&out).map_err(|e| EngineError::Failed(e.to_string()))
}

fn wasi_sysroot() -> Option<PathBuf> {
    match std::env::var("WASI_SYSROOT") {
        Ok(s) if Path::new(&s).exists() => Some(PathBuf::from(s)),
        _ => vyrn_codegen::toolchain::discovered_wasi_sysroot(),
    }
}

fn wasi_builtins(sysroot: &Path) -> Option<PathBuf> {
    match std::env::var("WASI_BUILTINS") {
        Ok(b) if Path::new(&b).exists() => Some(PathBuf::from(b)),
        _ => vyrn_codegen::toolchain::builtins_near_sysroot(sysroot),
    }
}

// ---------------------------------------------------------------------------
// The runtime: an embedded wasmtime plus a hand-written minimal WASI.
// ---------------------------------------------------------------------------

/// `__vyrn_gen_read`'s modes, shared with the C shim.
const MODE_READ: i32 = 0;
const MODE_LIST: i32 = 2;
/// Not a read at all: `moduleInterface`, which needs the resolver AND the
/// loader, so it is served on the host thread like one (RFC-0076 M3b).
const MODE_MODULE_INTERFACE: i32 = 3;

/// One unit of a structured host result (RFC-0076 M3b).
///
/// The stream carries no type information beyond this, and does not need to:
/// both the host encoder and the synthesized decoder walk the same static type,
/// so the reader always knows what it is about to read. The distinction between
/// the two variants is therefore not a tag the decoder consults — it is a
/// tripwire, and a `nextInt` that finds a string means the two walks disagreed.
enum Atom {
    Int(i64),
    Str(Vec<u8>),
}

/// The guest's world: its argv, what it wrote, and its line to the host.
#[derive(Default)]
struct Streams {
    /// NUL-terminated argv, argv[0] first — the shape `args_get` writes.
    argv: Vec<Vec<u8>>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    /// The bytes of the last served read or `render`, waiting for `fetch` to
    /// copy them into guest memory. The host never allocates on the guest's side
    /// of the wall.
    stash: Vec<u8>,
    /// The code-quote arena (RFC-0076 M3a): a `Code` value is an index into
    /// this. Per generation run and nowhere near the module cache — a handle
    /// from one run must be meaningless in the next, and being an index into a
    /// vector that no longer exists is the strongest form of that.
    code: Vec<Vec<CodePiece>>,
    /// The structured result being handed over, and how far the decoder has read
    /// (RFC-0076 M3b). One at a time: a decoder only ever calls `nextInt`/
    /// `nextStr`, so a stream is always fully consumed before the next `reflect`.
    atoms: Vec<Atom>,
    cursor: usize,
    /// The generator's own declarations, so `contractOf` and `lex` can be
    /// answered on this thread — neither needs the resolver.
    types: std::collections::HashMap<String, vyrn_frontend::ast::TypeDecl>,
    contracts: Vec<vyrn_frontend::ast::ContractDecl>,
    caps: Option<Caps>,
}

impl Streams {
    /// The pieces a handle names, or the trap for a handle that names nothing.
    /// Unreachable from compiled code — every handle this sees came out of one
    /// of the imports below — but the arena is indexed by a guest-supplied
    /// integer, and an index from the guest is checked.
    fn pieces(&self, h: i64) -> wasmtime::Result<&Vec<CodePiece>> {
        self.code
            .get(h as usize)
            .ok_or_else(|| wasmtime::Error::msg(format!("bad code handle {h}")))
    }

    fn intern(&mut self, pieces: Vec<CodePiece>) -> i64 {
        self.code.push(pieces);
        self.code.len() as i64 - 1
    }

    /// Start handing over `lit` as a value of type `ty`.
    ///
    /// The previous stream must be exhausted: an unread atom means the decoder
    /// walked a shorter type than the encoder did, which is the one failure this
    /// design can have and the one worth catching loudly.
    fn stream(&mut self, ty: &Type, lit: &Expr) -> wasmtime::Result<()> {
        self.drained()?;
        let mut atoms = Vec::new();
        encode(ty, lit, &self.types, &mut atoms).map_err(wasmtime::Error::msg)?;
        self.atoms = atoms;
        self.cursor = 0;
        Ok(())
    }

    fn drained(&self) -> wasmtime::Result<()> {
        if self.cursor != self.atoms.len() {
            return Err(wasmtime::Error::msg(format!(
                "generator decoder left {} atoms unread — the host and guest walks of the \
                 reflected type disagree",
                self.atoms.len() - self.cursor
            )));
        }
        Ok(())
    }

    fn next_atom(&mut self) -> wasmtime::Result<&Atom> {
        let a = self.atoms.get(self.cursor).ok_or_else(|| {
            wasmtime::Error::msg(
                "generator decoder read past the end of a reflected value — the host and guest \
                 walks of the reflected type disagree",
            )
        })?;
        self.cursor += 1;
        Ok(a)
    }
}

/// Push `lit` — a record literal built by the compiler's own reflection — onto
/// the atom stream, walking the STATIC TYPE rather than the literal (RFC-0076
/// M3b).
///
/// Walking the type is the whole design. The decoder walks it too, from
/// `record_fields`/`resolve`, so the two agree by construction instead of by a
/// convention each side has to remember. Fields are pulled out of the literal BY
/// NAME for the same reason: `module_interface_lit` happens to build them in
/// declaration order today, and depending on that would be a silent,
/// load-bearing coincidence.
fn encode(
    ty: &Type,
    lit: &Expr,
    types: &std::collections::HashMap<String, vyrn_frontend::ast::TypeDecl>,
    out: &mut Vec<Atom>,
) -> Result<(), String> {
    let wrong = || format!("cannot encode {lit:?} as {ty}");
    match vyrn_frontend::types::resolve(ty, types) {
        Type::Str => match lit {
            Expr::Str(s) => out.push(Atom::Str(s.clone().into_bytes())),
            _ => return Err(wrong()),
        },
        Type::Int | Type::IntN { .. } => match lit {
            Expr::Int(n) => out.push(Atom::Int(*n)),
            _ => return Err(wrong()),
        },
        Type::Bool => match lit {
            Expr::Bool(b) => out.push(Atom::Int(*b as i64)),
            _ => return Err(wrong()),
        },
        // `Some(x)` / `None` — one presence atom, then the payload if there is one.
        Type::Option(inner) => match lit {
            Expr::Call { name, args, .. } if name == "Some" && args.len() == 1 => {
                out.push(Atom::Int(1));
                encode(&inner, &args[0], types, out)?;
            }
            Expr::Var { name, .. } if name == "None" => out.push(Atom::Int(0)),
            _ => return Err(wrong()),
        },
        Type::Array(inner) => match lit {
            Expr::ArrayLit { elems, .. } => {
                out.push(Atom::Int(elems.len() as i64));
                for e in elems {
                    encode(&inner, e, types, out)?;
                }
            }
            _ => return Err(wrong()),
        },
        Type::Record(_) => {
            let Expr::StructLit { fields, .. } = lit else {
                return Err(wrong());
            };
            let decl = vyrn_frontend::types::record_fields(ty, types).ok_or_else(wrong)?;
            for f in &decl {
                let v = fields
                    .iter()
                    .find(|(k, _)| *k == f.name)
                    .map(|(_, v)| v)
                    .ok_or_else(|| format!("reflected literal has no field `{}`", f.name))?;
                encode(&f.ty, v, types, out)?;
            }
        }
        _ => return Err(wrong()),
    }
    Ok(())
}

/// The guest's end of the capability channel.
///
/// The guest runs on its own thread and the resolver stays on the caller's,
/// because wasmtime requires the store's data to be `'static` and the resolver
/// is borrowed for the call. A pair of channels buys that without an `unsafe`
/// lifetime extension in a workspace that has none; the thread costs tens of
/// microseconds against a generation measured in milliseconds.
struct Caps {
    req: mpsc::Sender<(String, i32)>,
    resp: mpsc::Receiver<Result<Served, String>>,
}

/// What the host thread answers a guest request with: bytes for a read or a
/// listing, and the compiler's reflection literal for `moduleInterface`.
enum Served {
    Bytes(i32, Vec<u8>),
    /// Encoded on the GUEST thread, so all three structured builtins share one
    /// call to [`encode`] rather than one per request kind.
    Lit(Box<Expr>),
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

/// A host-side refusal, carried out of the guest as a trap: a read outside the
/// generator's declared inputs, or a value with no splice rule. Both abort
/// generation under the interpreter too — neither may reach the generator as an
/// error value it could swallow.
#[derive(Debug)]
struct Denied(String);

impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

impl std::error::Error for Denied {}

/// The guest's linear memory and its store data, together — every host import
/// that touches a pointer needs both, and `data_and_store_mut` is the only way
/// to hold them at once.
fn guest_mem<'a>(
    caller: &'a mut wasmtime::Caller<'_, Streams>,
) -> wasmtime::Result<(&'a mut [u8], &'a mut Streams)> {
    let Some(wasmtime::Extern::Memory(mem)) = caller.get_export("memory") else {
        return Err(wasmtime::Error::msg("generator has no memory"));
    };
    Ok(mem.data_and_store_mut(caller))
}

/// A NUL-terminated guest string. Vyrn strings are validated UTF-8 with no
/// interior NUL by construction, so this is a copy, not a parse.
fn cstr(data: &[u8], at: i32) -> wasmtime::Result<String> {
    let rest = data
        .get(at as usize..)
        .ok_or_else(|| wasmtime::Error::msg("bad string pointer"))?;
    let n = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    Ok(String::from_utf8_lossy(&rest[..n]).into_owned())
}

/// Rebuild the interpreter value a `@codeSplice` call is splicing, from the tag
/// codegen chose statically and the one word it sent (RFC-0076 M3a).
///
/// The point of the round trip is that the splice rule then runs on a `Val`,
/// which is what the interpreter would have handed it — so there is one rule,
/// not two that agree. Floats cross as bit patterns because the formatting
/// (`{f:?}`, shortest-roundtrip) belongs on this side; a guest-side rendering
/// would be a second float formatter.
fn splice_value(
    tag: i32,
    bits: i64,
    p: i32,
    data: &[u8],
    streams: &Streams,
) -> wasmtime::Result<vyrn_frontend::interp::Val> {
    use vyrn_frontend::interp::Val;
    Ok(match tag {
        vyrn_codegen::TAG_STR => Val::Str(std::rc::Rc::new(cstr(data, p)?)),
        vyrn_codegen::TAG_CODE => Val::Code(streams.pieces(bits)?.clone()),
        vyrn_codegen::TAG_BOOL => Val::Bool(bits != 0),
        // `Val::Int` renders as the signed decimal, which is what a signed
        // integer of any width becomes after codegen's `sext`.
        vyrn_codegen::TAG_INT => Val::Int(bits),
        vyrn_codegen::TAG_UINT => Val::IntN { v: bits, signed: false, bits: 64 },
        vyrn_codegen::TAG_F64 => Val::Float(f64::from_bits(bits as u64)),
        vyrn_codegen::TAG_F32 => Val::Float32(f32::from_bits(bits as u32)),
        other => return Err(wasmtime::Error::msg(format!("bad splice tag {other}"))),
    })
}

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
/// Two keys, because there are two ways to describe that closure and only one of
/// them is cheap. The loader hands over content hashes of the generator's own
/// sources (`sources_fingerprint`) — it hashes those files anyway to write the
/// generation cache entry, so keying on them is free, where hashing the whole
/// `Debug` of a 4,536-line `std/vyx` cost 1.1–1.9 ms of a 54 ms keystroke.
/// Without a fingerprint (a generated module in the closure, which no resolver
/// can re-read) the `Debug` hash is still the only complete description, and it
/// stays the fallback — a cheap key that could miss an edit would be a stale
/// artifact, which is a silently wrong program.
///
/// Only the fingerprinted key crosses to disk, and it carries the compiler's own
/// identity: the artifact is this codegen's output, so a rebuilt `vyrn` must not
/// inherit the last one's artifacts.
fn artifact_key(program: &Program, fn_name: &str, fingerprint: Option<&str>) -> String {
    use std::fmt::Write as _;

    if let Some(fp) = fingerprint {
        return vyrn_frontend::hash::sha256_hex(
            format!("{fn_name}\u{0}{}\u{0}{fp}", compiler_identity()).as_bytes(),
        );
    }

    struct Sink(u64);
    impl std::fmt::Write for Sink {
        fn write_str(&mut self, s: &str) -> std::fmt::Result {
            for b in s.as_bytes() {
                self.0 = (self.0 ^ *b as u64).wrapping_mul(0x0100_0000_01b3);
            }
            Ok(())
        }
    }

    // Hashed straight out of `Debug` into the hasher, so nothing is materialized
    // to hash it.
    let mut sink = Sink(0xcbf2_9ce4_8422_2325);
    let _ = write!(sink, "{fn_name}\u{0}{program:?}");
    format!("{:016x}", sink.0)
}

/// Which build of the compiler produced an artifact. Every crate here is version
/// `0.0.0`, so the only honest answer is the executable itself — its size and
/// mtime change on every rebuild, which is exactly when a persisted artifact
/// stops being this codegen's output.
fn compiler_identity() -> String {
    static ID: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    ID.get_or_init(|| {
        let m = std::env::current_exe().and_then(|p| std::fs::metadata(p));
        match m {
            Ok(m) => format!(
                "{}:{:?}",
                m.len(),
                m.modified().ok().and_then(|t| t
                    .duration_since(std::time::UNIX_EPOCH)
                    .ok()
                    .map(|d| d.as_nanos()))
            ),
            // No answer is not "any answer": an artifact that cannot be tied to a
            // build must not be reused across processes at all.
            Err(_) => format!("unknown-{}", std::process::id()),
        }
    })
    .clone()
}

/// Where compiled artifacts persist, beside the generation cache they belong to
/// (`~/.vyrn/cache/gen`, `VYRN_GEN_CACHE_DIR`). Inside it rather than next to it
/// so that clearing the generation cache clears these too — an artifact is a
/// compiled generator, and the two go stale together.
///
/// The location is the CLI's `remote::gen_cache_dir` rule, restated because this
/// crate is BELOW the CLI: the frontend's cache port carries `String`, and a
/// serialized module is bytes.
fn artifact_dir() -> PathBuf {
    let base = match std::env::var("VYRN_GEN_CACHE_DIR") {
        Ok(d) => PathBuf::from(d),
        Err(_) => {
            let home = std::env::var("USERPROFILE")
                .or_else(|_| std::env::var("HOME"))
                .unwrap_or_else(|_| ".".to_string());
            Path::new(&home).join(".vyrn/cache/gen")
        }
    };
    base.join("wasm")
}

/// Read a cranelift-compiled artifact back, skipping the expensive half of a
/// cold start: opening the first `.vyx` page of a session compiled seven
/// artifacts and cost ~900 ms, all of it clang and cranelift.
///
/// The one `unsafe` in this workspace, confined here. `Module::deserialize`
/// trusts its input completely — it maps in native code — so what makes this
/// sound is that the input is a file THIS process's cache directory wrote, keyed
/// by a content hash that includes the compiler build. wasmtime's own header
/// carries its version and configuration and refuses anything foreign, and every
/// failure (missing, truncated, foreign, corrupt) is a cache MISS that
/// recompiles rather than an error the user ever sees.
fn load_artifact(key: &str) -> Option<wasmtime::Module> {
    let bytes = std::fs::read(artifact_dir().join(key)).ok()?;
    // SAFETY: see above — our own cache directory, our own serialization, and a
    // rejection is a miss.
    unsafe { wasmtime::Module::deserialize(wasm_engine(), &bytes) }.ok()
}

/// Store an artifact for the next session. Best-effort: a full disk or a
/// read-only home costs a recompile, nothing more.
fn store_artifact(key: &str, module: &wasmtime::Module) {
    let Ok(bytes) = module.serialize() else { return };
    let dir = artifact_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    // Written to a per-process name and renamed, so a concurrent reader (the LSP
    // and a build share this directory) never sees a half-written artifact.
    let tmp = dir.join(format!("{key}.{}.tmp", std::process::id()));
    if std::fs::write(&tmp, &bytes).is_ok() && std::fs::rename(&tmp, dir.join(key)).is_err() {
        let _ = std::fs::remove_file(&tmp);
    }
}

/// RFC-0021's step budget, converted into the unit the guest is metered in.
///
/// The units do not correspond and cannot be made to: the interpreter spends one
/// step per Vyrn STATEMENT, wasmtime spends one fuel per wasm INSTRUCTION. So
/// the mapping is deliberately biased LOOSE — anything that finishes inside the
/// interpreter's budget must finish inside this one, which leaves the only
/// divergence in a pathological band where wasm succeeds and the interpreter
/// would have failed. That direction never breaks a generator that worked.
///
/// The multiplier is measured, not guessed. Every generator call in the repo,
/// under both engines (`VYRN_GEN_STEPS` against `VYRN_GENWASM_TRACE`), spends
/// between 56 and 755 fuel per interpreted step; the worst SUSTAINED ratio, once
/// the fixed ~23k of libc startup is discounted, is ~410, and the string-heavy
/// ones that dominate real work (`std/tw` 307, `std/vyx` 152, `std/i18n` 74) sit
/// well below it. 1,000 is ~2.4x above the worst measured, and the flat 1M
/// absorbs that startup cost so a generator of a few dozen statements is not
/// killed by it (`examples/gendemo` is 31 steps and 23,416 fuel).
///
/// It is a margin, not a proof: one Vyrn statement can copy an unbounded number
/// of bytes, so a ratio can always be constructed past any multiplier. The
/// measured ones do not come close, and the guardrail's job is to stop a runaway
/// generator hanging the editor, which it does — the default budget burns out in
/// ~1.6 s, against ~3.4 s for the same generator interpreted.
fn wasm_fuel(steps: u64) -> u64 {
    steps.saturating_mul(1_000).saturating_add(1_000_000)
}

/// One wasmtime engine and one compiled module per artifact, for the process.
///
/// Cranelift compilation is the expensive half of instantiation, and a `Module`
/// is an `Arc` internally, so caching it is what turns a repeat generation into
/// milliseconds.
fn wasm_engine() -> &'static wasmtime::Engine {
    static ENGINE: std::sync::OnceLock<wasmtime::Engine> = std::sync::OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut cfg = wasmtime::Config::new();
        // Fuel, not epochs (RFC-0076 M5). An epoch is wall-clock, so the same
        // generator would die on a slow machine and pass on a fast one; fuel is
        // counted instructions, and determinism is what this whole path rests on.
        cfg.consume_fuel(true);
        wasmtime::Engine::new(&cfg).unwrap_or_default()
    })
}

fn module_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, wasmtime::Module>> {
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, wasmtime::Module>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Compile (once) and run, returning what the guest wrote to stdout.
///
/// Three tiers, cheapest first: this process's modules, then the artifacts a
/// previous process left on disk, then clang plus cranelift.
fn run_module(
    key: &str,
    persist: bool,
    argv: &[String],
    program: &Program,
    inputs: &GenInputs<'_>,
    reads: &mut Vec<(String, Vec<u8>)>,
    build: impl FnOnce() -> Result<Vec<u8>, EngineError>,
) -> Result<String, EngineError> {
    let cached = module_cache().lock().ok().and_then(|c| c.get(key).cloned());
    let module = match cached {
        Some(m) => m,
        None => {
            let t = std::time::Instant::now();
            let from_disk = persist.then(|| load_artifact(key)).flatten();
            let m = match from_disk {
                Some(m) => {
                    trace("deserialize", t.elapsed());
                    m
                }
                None => {
                    let bytes = build()?;
                    trace("clang", t.elapsed());
                    let t = std::time::Instant::now();
                    let m = wasmtime::Module::new(wasm_engine(), &bytes)
                        .map_err(|e| EngineError::Failed(format!("wasm: {e}")))?;
                    trace("cranelift", t.elapsed());
                    if persist {
                        store_artifact(key, &m);
                    }
                    m
                }
            };
            if let Ok(mut c) = module_cache().lock() {
                c.insert(key.to_string(), m.clone());
            }
            m
        }
    };
    run_hosted(&module, argv, program, inputs, reads)
}

/// Run the guest on its own thread and serve its capability requests from this
/// one, which is where the resolver lives. The guest's `Sender` dies with its
/// store, which is what ends the loop.
fn run_hosted(
    module: &wasmtime::Module,
    argv: &[String],
    program: &Program,
    inputs: &GenInputs<'_>,
    reads: &mut Vec<(String, Vec<u8>)>,
) -> Result<String, EngineError> {
    let fuel = wasm_fuel(inputs.fuel);
    let (req_tx, req_rx) = mpsc::channel::<(String, i32)>();
    let (resp_tx, resp_rx) = mpsc::channel::<Result<Served, String>>();

    let module = module.clone();
    let argv: Vec<String> = argv.to_vec();
    // The declarations `contractOf` and `lex` reflect over. Cloned across because
    // the store's data must be `'static`, and cheap beside the `program.clone()`
    // the wrapper already pays.
    let types = program.type_decls.iter().map(|t| (t.name.clone(), t.clone())).collect();
    let contracts = program.contracts.clone();
    let guest = std::thread::Builder::new()
        // Cranelift-compiled code runs on this stack; a deeply recursive
        // generator (every parser here is one) would otherwise overflow the
        // default well before wasmtime's own wasm-stack limit bites.
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            run_wasm(
                &module,
                &argv,
                types,
                contracts,
                fuel,
                Caps {
                    req: req_tx,
                    resp: resp_rx,
                },
            )
        })
        .map_err(|e| EngineError::Failed(e.to_string()))?;

    while let Ok((path, mode)) = req_rx.recv() {
        if resp_tx.send(serve(inputs, reads, &path, mode)).is_err() {
            break;
        }
    }
    match guest.join() {
        Ok(r) => r,
        Err(_) => Err(EngineError::Failed("generator panicked".into())),
    }
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
fn run_wasm(
    module: &wasmtime::Module,
    argv: &[String],
    types: std::collections::HashMap<String, vyrn_frontend::ast::TypeDecl>,
    contracts: Vec<vyrn_frontend::ast::ContractDecl>,
    fuel: u64,
    caps: Caps,
) -> Result<String, EngineError> {
    use wasmtime::*;

    let engine = wasm_engine();
    let mut linker: Linker<Streams> = Linker::new(engine);
    let wasi = "wasi_snapshot_preview1";

    // The mediated capabilities (RFC-0076 M2). `read` resolves, mediates, reads
    // and stashes; `fetch` copies the stash into a buffer the GUEST allocated,
    // so nothing on the host side has to allocate inside linear memory.
    linker
        .func_wrap(
            "vyrn_gen",
            "read",
            |mut caller: Caller<'_, Streams>, path: i32, mode: i32| -> Result<i64> {
                let (data, streams) = guest_mem(&mut caller)?;
                let path = cstr(data, path)?;
                let caps = streams.caps.as_ref().ok_or_else(|| Error::msg("no host"))?;
                caps.req
                    .send((path, mode))
                    .map_err(|_| Error::msg("generator host is gone"))?;
                match caps.resp.recv() {
                    Ok(Ok(Served::Bytes(status, bytes))) => {
                        let len = bytes.len() as i64;
                        streams.stash = bytes;
                        Ok((status as i64) << 32 | len)
                    }
                    Ok(Ok(Served::Lit(_))) => Err(Error::msg("read answered with a literal")),
                    // A scoping violation unwinds out of `_start` instead of
                    // becoming an `Err` value — same as the interpreter's trap.
                    Ok(Err(msg)) => Err(Error::new(Denied(msg))),
                    Err(_) => Err(Error::msg("generator host is gone")),
                }
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    linker
        .func_wrap(
            "vyrn_gen",
            "fetch",
            |mut caller: Caller<'_, Streams>, dest: i32| -> Result<()> {
                let (data, streams) = guest_mem(&mut caller)?;
                let stash = std::mem::take(&mut streams.stash);
                let slot = data
                    .get_mut(dest as usize..dest as usize + stash.len())
                    .ok_or_else(|| Error::msg("bad fetch destination"))?;
                slot.copy_from_slice(&stash);
                Ok(())
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;

    // The code-quote arena (RFC-0076 M3a). `Code` is an i64 handle and every
    // operation on it happens here, so the splice rules, the string escaping and
    // the float formatting are the interpreter's own — byte-identical by
    // construction, not by testing.
    linker
        .func_wrap(
            "vyrn_gen",
            "text",
            |mut caller: Caller<'_, Streams>, s: i32| -> Result<i64> {
                let (data, streams) = guest_mem(&mut caller)?;
                let text = cstr(data, s)?;
                Ok(streams.intern(vec![CodePiece::Text(text)]))
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    linker
        .func_wrap(
            "vyrn_gen",
            "rawAt",
            |mut caller: Caller<'_, Streams>, s: i32, path: i32, line: i64, col: i64| -> Result<i64> {
                let (data, streams) = guest_mem(&mut caller)?;
                let text = cstr(data, s)?;
                let path = cstr(data, path)?;
                Ok(streams.intern(vec![CodePiece::Origin { path, line, col, text }]))
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    linker
        .func_wrap(
            "vyrn_gen",
            "splice",
            |mut caller: Caller<'_, Streams>, tag: i32, bits: i64, p: i32, ctx: i64| -> Result<i64> {
                let (data, streams) = guest_mem(&mut caller)?;
                let val = splice_value(tag, bits, p, data, streams)?;
                // A splice violation — an identifier that is not one, a value of
                // a type with no splice rule — is a trap under the interpreter,
                // so it unwinds out of `_start` rather than becoming a value.
                let pieces = vyrn_frontend::interp::gen_code_splice(&val, ctx)
                    .map_err(|m| Error::new(Denied(m)))?;
                Ok(caller.data_mut().intern(pieces))
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    linker
        .func_wrap(
            "vyrn_gen",
            "concat",
            |mut caller: Caller<'_, Streams>, a: i64, b: i64| -> Result<i64> {
                let s = caller.data_mut();
                let mut pieces = s.pieces(a)?.clone();
                pieces.extend(s.pieces(b)?.iter().cloned());
                Ok(s.intern(pieces))
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    linker
        .func_wrap(
            "vyrn_gen",
            "render",
            |mut caller: Caller<'_, Streams>, h: i64| -> Result<i64> {
                let s = caller.data_mut();
                let text = vyrn_frontend::interp::render_code(s.pieces(h)?);
                s.stash = text.into_bytes();
                Ok(s.stash.len() as i64)
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;

    // Structured host results (RFC-0076 M3b). `reflect` computes a value of a
    // known named type in the HOST — the lexer, the linker and the contract
    // tables are compiler machinery and a guest-side copy would be a second
    // answer — and leaves it as a flat atom stream the decoder pulls back.
    linker
        .func_wrap(
            "vyrn_gen",
            "reflect",
            |mut caller: Caller<'_, Streams>, kind: i64, arg: i32| -> Result<()> {
                let (data, streams) = guest_mem(&mut caller)?;
                let arg = cstr(data, arg)?;
                match kind {
                    // The one kind that needs the resolver, so it goes to the
                    // host thread and comes back as the compiler's own literal.
                    vyrn_codegen::REFLECT_MODULE_INTERFACE => {
                        let caps = streams.caps.as_ref().ok_or_else(|| Error::msg("no host"))?;
                        caps.req
                            .send((arg, MODE_MODULE_INTERFACE))
                            .map_err(|_| Error::msg("generator host is gone"))?;
                        let lit = match caps.resp.recv() {
                            Ok(Ok(Served::Lit(lit))) => lit,
                            Ok(Ok(Served::Bytes(..))) => {
                                return Err(Error::msg("moduleInterface answered with bytes"))
                            }
                            // An unreadable module or a load failure is a trap
                            // under the interpreter too, so it unwinds out of
                            // `_start` rather than becoming a value.
                            Ok(Err(msg)) => return Err(Error::new(Denied(msg))),
                            Err(_) => return Err(Error::msg("generator host is gone")),
                        };
                        streams.stream(&Type::Named("ModuleInterface".into()), &lit)
                    }
                    // A contract is a declaration the generator's own module
                    // closure carries, so this needs nothing from the host thread.
                    vyrn_codegen::REFLECT_CONTRACT_OF => {
                        let decl = streams
                            .contracts
                            .iter()
                            .find(|c| c.name == arg)
                            .ok_or_else(|| {
                                Error::new(Denied(format!(
                                    "`contractOf` needs a declared contract name; `{arg}` is not \
                                     a contract"
                                )))
                            })?;
                        let lit = vyrn_frontend::schema_reflect::contract_info_lit(decl);
                        streams.stream(&Type::Named("ContractInfo".into()), &lit)
                    }
                    vyrn_codegen::REFLECT_LEX => {
                        let lit = vyrn_frontend::interp::gen_lex_tokens_lit(&arg);
                        streams.stream(&Type::Array(Box::new(Type::Named("Token".into()))), &lit)
                    }
                    other => Err(Error::msg(format!("bad reflect kind {other}"))),
                }
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    linker
        .func_wrap(
            "vyrn_gen",
            "nextInt",
            |mut caller: Caller<'_, Streams>| -> Result<i64> {
                match caller.data_mut().next_atom()? {
                    Atom::Int(n) => Ok(*n),
                    Atom::Str(_) => Err(Error::msg("reflected value: expected an Int atom")),
                }
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    // Length, then `fetch` — the M2 stash protocol, so the host still never
    // allocates inside guest memory.
    linker
        .func_wrap(
            "vyrn_gen",
            "nextStr",
            |mut caller: Caller<'_, Streams>| -> Result<i64> {
                let s = caller.data_mut();
                let bytes = match s.next_atom()? {
                    Atom::Str(b) => b.clone(),
                    Atom::Int(_) => return Err(Error::msg("reflected value: expected a Str atom")),
                };
                s.stash = bytes;
                Ok(s.stash.len() as i64)
            },
        )
        .map_err(|e| EngineError::Failed(e.to_string()))?;

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
    let mut world = Streams {
        caps: Some(caps),
        types,
        contracts,
        ..Streams::default()
    };
    world.argv.push(b"gen\0".to_vec());
    world
        .argv
        .extend(argv.iter().map(|a| [a.as_bytes(), b"\0"].concat()));

    let mut store = Store::new(engine, world);
    store
        .set_fuel(fuel)
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    let inst = linker
        .instantiate(&mut store, module)
        .map_err(|e| EngineError::Failed(format!("instantiate: {e}")))?;
    let start = inst
        .get_typed_func::<(), ()>(&mut store, "_start")
        .map_err(|e| EngineError::Failed(format!("_start: {e}")))?;

    let result = start.call(&mut store, ());
    if std::env::var("VYRN_GENWASM_TRACE").is_ok() {
        // What a real generator actually costs, which is the only honest way to
        // pick the multiplier in `wasm_fuel`.
        let spent = fuel - store.get_fuel().unwrap_or(0);
        eprintln!("genwasm fuel: {spent}");
    }
    let streams = store.into_data();
    match result {
        // Out of fuel is the ONE trap that must be re-worded rather than passed
        // through: the guest never got to print anything, and the interpreter's
        // step budget says exactly this.
        Err(e) if e.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel) => {
            return Err(EngineError::Failed("generator exceeded its step budget".into()))
        }
        Ok(()) => {}
        Err(e) => match e.downcast_ref::<Exit>() {
            Some(Exit(0)) => {}
            // The guest failed on its own terms — its message is already on
            // stderr, in the canonical wording both engines share.
            //
            // Share, but not present identically: the compiled runtime writes
            // `error: division by zero`, because at the TOP level the CLI prints
            // that same prefix for an interpreted trap and parity compares the
            // two. Inside generation there is no CLI — the interpreter hands the
            // loader the bare message and the loader supplies the context — so
            // the prefix has to come off here or the same trap reads differently
            // by engine. The message is the LAST line for the same reason it is
            // the whole buffer's tail: a trap is the last thing a guest writes.
            Some(Exit(code)) => {
                let err = String::from_utf8_lossy(&streams.stderr);
                let msg = err.trim_end().lines().last().unwrap_or_default();
                let msg = msg.strip_prefix("error: ").unwrap_or(msg).to_string();
                return Err(EngineError::Failed(if msg.is_empty() {
                    format!("generator exited with {code}")
                } else {
                    msg
                }));
            }
            // A rejected read reads exactly as it does interpreted. The message
            // is taken from the payload, not from `e`: wasmtime wraps a host
            // error in a guest backtrace on the way out.
            None if e.downcast_ref::<Denied>().is_some() => {
                return Err(EngineError::Failed(
                    e.downcast_ref::<Denied>().unwrap().0.clone(),
                ))
            }
            None => return Err(EngineError::Failed(format!("generator trapped: {e}"))),
        },
    }
    // The last reflected value has no following `reflect` to check it, so its
    // stream is checked here: an unread atom means the decoder walked a shorter
    // type than the encoder did.
    streams
        .drained()
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    String::from_utf8(streams.stdout)
        .map_err(|_| EngineError::Failed("generator emitted invalid UTF-8".into()))
}
