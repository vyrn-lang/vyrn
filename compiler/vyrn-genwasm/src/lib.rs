//! Runs a `gen fn` as compiled wasm: the generation engine.
//!
//! The generator's module is compiled with a synthesized `main` that
//! dispatches on `args()[0]` and prints the result between two sentinel lines.
//! File reads go through `GenInputs.resolver` and are recorded in
//! `GenOutput.reads`; `lex`, `moduleInterface` and `contractOf` results cross
//! by a host encoder and a synthesized decoder that walk the same static type.
//! A generator this path cannot serve is declined, and `gen::generate` fails
//! the run with "the installed generation engine declined it". A separate
//! crate so `vyrn-lsp` can reach it; outside the default workspace because
//! `wasmtime` is an external dependency.

#[cfg(feature = "host")]
use std::path::PathBuf;
#[cfg(feature = "host")]
use std::sync::mpsc;

use vyrn_frontend::ast::{Block, Expr, Function, Id, Param, Program, Stmt, Type};
use vyrn_frontend::consteval::ConstVal;
#[cfg(feature = "host")]
use vyrn_frontend::gen::{compiler_identity, CodePiece, GenRead, Spliced};
use vyrn_frontend::gen::{GenInputs, GenOutput};

/// Calls this path cannot serve; a module containing one is declined (see
/// [`engine`]). A `gen fn` may not call the write capabilities, so this only
/// declines a module that contains one somewhere.
const UNSERVED: &[&str] = &["writeFile", "writeAtomic", "renameFile", "fsyncFile"];

/// The wrapper frames the generated source between these two lines
/// ([`wrapper_program`]), so the generator's compile-time `print` output splits
/// off. Any count but one of each means the guest wrote a marker itself, and
/// the run is declined.
const RESULT_BEGIN: &str = "<<vyrn-genwasm-result>>";
const RESULT_END: &str = "<<vyrn-genwasm-result-end>>";

/// Installs the wasm generation engine. Called once from `main`.
#[cfg(feature = "host")]
pub fn install() {
    vyrn_frontend::gen::set_gen_engine(vyrn_lower::gen_engine(engine));
}

/// Claims a generation run, or declines it with `None`, which the frontend
/// reports as an error.
#[cfg(feature = "host")]
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

/// `VYRN_GENWASM_TRACE=1` prints per-phase timings on stderr.
#[cfg(feature = "host")]
fn trace(phase: &str, d: std::time::Duration) {
    if std::env::var("VYRN_GENWASM_TRACE").is_ok() {
        eprintln!("genwasm {phase}: {:.2} ms", d.as_secs_f64() * 1000.0);
    }
}

/// Declines, saying why under `VYRN_GENWASM_TRACE`; otherwise a decline is
/// invisible.
#[cfg(feature = "host")]
fn decline(why: &str) -> EngineError {
    if std::env::var("VYRN_GENWASM_TRACE").is_ok() {
        eprintln!("genwasm declined: {why}");
    }
    EngineError::Unsupported
}

#[cfg(feature = "host")]
enum EngineError {
    /// This path cannot serve the generator.
    Unsupported,
    /// The generator itself failed.
    Failed(String),
}

/// The first call anywhere in the program this path cannot serve.
///
/// Whole-program rather than reachability-precise: a false decline costs
/// speed, a false accept would run a generator outside its sandbox.
fn reaches_unserved(program: &Program) -> Option<String> {
    program.functions.iter().find_map(|f| {
        let mut hits: Vec<String> = vyrn_frontend::checker::fn_calls(&f.body)
            .into_iter()
            .filter(|c| UNSERVED.contains(&c.as_str()))
            .collect();
        hits.sort();
        hits.into_iter().next()
    })
}

#[cfg(feature = "host")]
fn run(
    program: &Program,
    fn_name: &str,
    args: &[ConstVal],
    inputs: &GenInputs<'_>,
) -> Result<GenOutput, EngineError> {
    // Checked here because the key does not name the target: a sibling's
    // artifact is a cache hit, and dispatching to a name the wrapper never
    // emitted traps rather than declines.
    let target = match program.functions.iter().find(|f| f.name == fn_name) {
        Some(f) if dispatchable(f) && f.params.len() == args.len() + takes_type_arg(f) as usize => {
            f
        }
        _ => return Err(decline("the generator is not one this path serves")),
    };

    // The arguments travel as argv, so one compiled artifact serves every call.
    // argv[0] is the generator's name, which `main` dispatches on: the artifact
    // is one per module, not per generator.
    let mut argv: Vec<String> = vec![fn_name.to_string()];
    if takes_type_arg(target) && inputs.type_arg.is_none() {
        return Err(decline("a `TypeArg` parameter with no type argument"));
    }
    for (a, p) in args.iter().zip(&target.params) {
        argv.push(match (a, &p.ty) {
            (ConstVal::Str(s), Type::Str) => s.clone(),
            // Decimal, which `parse` reads.
            (ConstVal::Int(n), Type::Int) => n.to_string(),
            _ => return Err(decline("a constant argument this path cannot write")),
        });
    }

    let t = std::time::Instant::now();
    let key = artifact_key(program, inputs.sources_fingerprint.as_deref());
    trace("key", t.elapsed());

    let t = std::time::Instant::now();
    let mut reads = Vec::new();
    // Only a fingerprinted key is trusted across processes; see `artifact_key`.
    let persist = inputs.sources_fingerprint.is_some();
    let source = run_module(&key, persist, &argv, program, inputs, &mut reads, || {
        let wrapper = wrapper_program(program)
            .ok_or_else(|| decline("the wrapper program cannot be synthesized"))?;
        compile_to_wasm(&key, &wrapper)
    })?;
    trace("run", t.elapsed());
    if source.len() > inputs.max_output {
        return Err(EngineError::Failed(format!(
            "generator output exceeds the {} byte cap",
            inputs.max_output
        )));
    }
    Ok(GenOutput { source, reads })
}

/// Serves one capability request from the guest as `Interp::gen_read_file` /
/// `gen_list_dir` do: scope the path, read through the resolver, record the
/// bytes, and answer a status (0 ok, 1 io, 3 embedded NUL).
///
/// `Err` is a scoping violation or an unreadable remote pin; it aborts
/// generation and the guest never sees it.
#[cfg(feature = "host")]
fn serve(
    inputs: &GenInputs<'_>,
    reads: &mut Vec<GenRead>,
    path: &str,
    mode: i32,
) -> Result<Served, String> {
    // The frontend's own implementation, so the recorded reads are every
    // module the link touched.
    if mode == MODE_MODULE_INTERFACE {
        return vyrn_frontend::gen::gen_module_interface_lit(
            inputs.resolver,
            inputs.opts,
            &inputs.importer_dir,
            &inputs.allowed,
            &inputs.aliased,
            reads,
            path,
        )
        .map(|lit| Served::Lit(Box::new(lit)));
    }
    let resolved = vyrn_frontend::gen::gen_scoped_path(
        &inputs.importer_dir,
        &inputs.allowed,
        &inputs.aliased,
        path,
    )?;
    if mode == MODE_LIST || mode == MODE_LIST_KINDS {
        return match inputs.resolver.list_kinds(&resolved) {
            Ok(mut names) => {
                // Both modes record the kinds listing under the directory key,
                // the listing the cache re-hashes, so a changed listing or kind
                // misses.
                names.sort();
                let joined = names.join("\n").into_bytes();
                reads.push((format!("{resolved}/"), Some(joined.clone())));
                if mode == MODE_LIST {
                    for n in &mut names {
                        if n.ends_with('/') {
                            n.pop();
                        }
                    }
                    names.sort();
                    names.dedup();
                    return Ok(Served::Bytes(0, names.join("\n").into_bytes()));
                }
                Ok(Served::Bytes(0, joined))
            }
            Err(_) => {
                // Recorded as absent, so the entry misses when it appears.
                reads.push((format!("{resolved}/"), None));
                Ok(Served::Bytes(1, Vec::new()))
            }
        };
    }
    match inputs.resolver.read(&resolved) {
        Ok(content) => {
            let bytes = content.into_bytes();
            reads.push((resolved, Some(bytes.clone())));
            // Recorded before the NUL rule rejects it: the cache must notice
            // when the file changes.
            if mode == MODE_READ && bytes.contains(&0) {
                return Ok(Served::Bytes(3, Vec::new()));
            }
            Ok(Served::Bytes(0, bytes))
        }
        Err(why) => {
            let remote = vyrn_frontend::loader::is_remote(&resolved);
            reads.push((resolved, None));
            if remote {
                // A pinned dependency that cannot be produced aborts with the
                // resolver's refusal, as `Interp::gen_read_file` does; status 1
                // would lose the remedy.
                return Err(why);
            }
            Ok(Served::Bytes(1, Vec::new()))
        }
    }
}

/// Runs generator `fn_name` without wasmtime, for the playground, where the
/// browser runs the module. `exec` instantiates the module bytes, runs them
/// with the argv and the `TypeArg`'s atoms, and answers the module's stdout.
///
/// Only a generator whose one parameter is a `TypeArg` is served: `exec`'s
/// host answers no read, no module reflection and no code quote, so any other
/// generator is declined (`None`).
pub fn run_pure(
    program: &Program,
    fn_name: &str,
    args: &[ConstVal],
    inputs: &GenInputs<'_>,
    exec: impl FnOnce(&[u8], &[String], &[Atom]) -> Result<Vec<u8>, String>,
) -> Option<Result<GenOutput, String>> {
    let target = program.functions.iter().find(|f| f.name == fn_name)?;
    let arg = inputs.type_arg.as_ref()?;
    if !dispatchable(target) || !takes_type_arg(target) || !args.is_empty() {
        return None;
    }
    if reaches_unserved(program).is_some() {
        return None;
    }
    let types = program
        .type_decls
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect();
    let mut atoms = Vec::new();
    let run = || -> Result<GenOutput, String> {
        encode(&Type::Named("TypeArg".into()), arg, &types, &mut atoms)?;
        let wrapper =
            wrapper_program(program).ok_or("the wrapper program cannot be synthesized")?;
        let bytes = vyrn_codegen::direct::compile_gen_host(&wrapper)?;
        let stdout = exec(&bytes, &[fn_name.to_string()], &atoms)?;
        let (_, source) = unframe_result(&stdout)?;
        let source =
            String::from_utf8(source).map_err(|_| "generator emitted invalid UTF-8".to_string())?;
        Ok(GenOutput {
            source,
            reads: Vec::new(),
        })
    };
    Some(run())
}

/// Whether the engine can serve this function: an exported `gen fn` returning
/// `String` and taking `String` or `Int64` parameters (written to argv and read
/// back, `Int64` by `parse`), or one `TypeArg` (reflected, as `moduleInterface`
/// is). A generator returning `Code` is not served: the guest would print the
/// handle.
fn dispatchable(f: &Function) -> bool {
    f.is_gen
        && f.exported
        && f.ret == Type::Str
        && (takes_type_arg(f)
            || f.params
                .iter()
                .all(|par| matches!(par.ty, Type::Str | Type::Int)))
}

fn takes_type_arg(f: &Function) -> bool {
    matches!(f.params.as_slice(), [p] if p.ty == Type::Named("TypeArg".into()))
}

/// The generator's module with `is_gen` cleared, plus a `main` that dispatches
/// on `args()[0]` to the requested generator and prints its result.
///
/// Arguments and the name come from `args()`, so one artifact per module serves
/// every generator in it for every call; `std/vyx` exports three generators
/// over the same program.
fn wrapper_program(program: &Program) -> Option<Program> {
    // A module with its own `main` would collide with the synthesized one.
    if program.functions.iter().any(|f| f.name == "main") {
        return None;
    }
    let mut p = program.clone();
    let argv = |i: usize| call("@at", vec![var("argv"), Expr::Int(i as i64, Id::NEW)]);
    let mut body = vec![
        Stmt::Let {
            id: Id::NEW,
            name: "argv".into(),
            mutable: false,
            ty: None,
            value: call("args", vec![]),
            line: 0,
            col: 0,
        },
        Stmt::Let {
            id: Id::NEW,
            name: "g".into(),
            mutable: false,
            ty: Some(Type::Str),
            value: argv(0),
            line: 0,
            col: 0,
        },
    ];
    for f in p.functions.iter().filter(|f| dispatchable(f)) {
        body.push(Stmt::If {
            id: Id::NEW,
            cond: Expr::Binary {
                id: Id::NEW,
                op: vyrn_frontend::ast::BinOp::Eq,
                lhs: Box::new(var("g")),
                rhs: Box::new(Expr::Str(f.name.clone(), Id::NEW)),
                line: 0,
            },
            then_block: Block {
                id: Id::NEW,
                stmts: takes_type_arg(f)
                    .then(|| Stmt::Let {
                        // Bound, not passed inline: the release of an
                        // argument temporary is refused.
                        id: Id::NEW,
                        name: "typeArg".into(),
                        mutable: false,
                        ty: None,
                        value: call(vyrn_codegen::GEN_ENTRY_TYPE_ARG, vec![]),
                        line: 0,
                        col: 0,
                    })
                    .into_iter()
                    .chain([
                        // Framed between marker lines; see `unframe_result`.
                        Stmt::Expr(
                            call("print", vec![Expr::Str(RESULT_BEGIN.into(), Id::NEW)]),
                            Id::NEW,
                        ),
                        Stmt::Expr(
                            call(
                                "print",
                                vec![call(
                                    &f.name,
                                    f.params
                                        .iter()
                                        .enumerate()
                                        .map(|(i, par)| match takes_type_arg(f) {
                                            true => var("typeArg"),
                                            false => at_type(argv(i + 1), &par.ty),
                                        })
                                        .collect(),
                                )],
                            ),
                            Id::NEW,
                        ),
                        Stmt::Expr(
                            call("print", vec![Expr::Str(RESULT_END.into(), Id::NEW)]),
                            Id::NEW,
                        ),
                        Stmt::Return {
                            id: Id::NEW,
                            value: Some(Expr::Int(0, Id::NEW)),
                            line: 0,
                        },
                    ])
                    .collect(),
            },
            else_block: None,
            line: 0,
        });
    }
    // Unreachable: `run` checks the target first. Falling off the chain would
    // emit an empty module, so read past the end of `args()` and trap instead.
    body.push(Stmt::Expr(
        call("print", vec![argv(1_000_000_000)]),
        Id::NEW,
    ));
    body.push(Stmt::Return {
        id: Id::NEW,
        value: Some(Expr::Int(0, Id::NEW)),
        line: 0,
    });

    p.functions.push(func("main", Vec::new(), Type::Int, body));
    prepare(&mut p)?;
    Some(p)
}

/// Makes a program compilable by [`vyrn_codegen::direct::compile_gen_host`]:
/// clears every `is_gen` and synthesizes the entry points and decoders the
/// structured builtins need. The driver calls it too, for a `test` block that
/// calls a generator.
pub fn prepare(p: &mut Program) -> Option<()> {
    // Every `gen fn`, not only the dispatched ones: a generator's helpers are
    // `gen fn` too, and codegen skips a `gen fn`.
    for f in p.functions.iter_mut() {
        f.is_gen = false;
    }
    // Synthesized before the emitter sees the program, so every pass covers
    // them like any other function.
    reflect_entries(p)
}

/// Splits the guest's stdout into its compile-time prints and the generated
/// source, by the framing [`wrapper_program`] emits: prints, begin marker,
/// source, end marker. A generator's `print` output stays out of its result.
/// Any other shape declines.
fn unframe_result(stdout: &[u8]) -> Result<(Vec<u8>, Vec<u8>), &'static str> {
    let locate = |marker: &str| -> Vec<usize> {
        let m = marker.as_bytes();
        if stdout.len() < m.len() {
            return Vec::new();
        }
        (0..=stdout.len() - m.len())
            .filter(|i| &stdout[*i..*i + m.len()] == m)
            .collect()
    };
    let begins = locate(RESULT_BEGIN);
    let ends = locate(RESULT_END);
    if begins.len() != 1 || ends.len() != 1 {
        return Err("the guest did not frame its result exactly once");
    }
    let (begin, end) = (begins[0], ends[0]);
    if begin >= end {
        return Err("the guest framed its result backwards");
    }
    // Each `print` appends one newline.
    let Some(inner) = stdout[begin + RESULT_BEGIN.len()..end].strip_prefix(b"\n") else {
        return Err("the framed result does not begin where the wrapper put it");
    };
    let Some(source) = inner.strip_suffix(b"\n") else {
        return Err("the framed result does not end where the wrapper put it");
    };
    if stdout[end + RESULT_END.len()..] != *b"\n" {
        return Err("the framed result does not end where the wrapper put it");
    }
    Ok((stdout[..begin].to_vec(), source.to_vec()))
}

// Structured host results. `lex`, `moduleInterface` and `contractOf` each
// return a value of a known named type, so one transfer serves them: the host
// (`encode`) walks the static type over the value, and a synthesized decoder
// walks the same type pulling atoms back. Only an array's length and an
// Option's presence are tagged, because the reader knows what comes next. The
// decoders are ordinary Vyrn, so a change to record lowering cannot make the
// two walks disagree.

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
        body: Block { id: Id::NEW, stmts },
        line: 0,
        col: 0,
        is_extern: false,
        is_export_extern: false,
        is_gen: false,
        is_mut: false,
    }
}

/// One `args()[i]` read back at the parameter's declared type. The `None` arm
/// of `parse` is unreachable, because [`dispatchable`] admits only what argv
/// carries; its `0` gives the `match` a type.
fn at_type(e: Expr, ty: &Type) -> Expr {
    use vyrn_frontend::ast::{ArmBody, Binder, MatchArm, Pattern};
    if *ty == Type::Str {
        return e;
    }
    Expr::Match {
        id: Id::NEW,
        stmt_pos: false,
        scrutinee: Box::new(call("parse", vec![e])),
        arms: vec![
            MatchArm {
                pattern: Pattern::Variant("Some".into(), vec![Binder::synthetic("v")]),
                body: ArmBody::Expr(var("v")),
            },
            MatchArm {
                pattern: Pattern::Variant("None".into(), Vec::new()),
                body: ArmBody::Expr(Expr::Int(0, Id::NEW)),
            },
        ],
        line: 0,
    }
}

fn call(name: &str, args: Vec<Expr>) -> Expr {
    Expr::Call {
        id: Id::NEW,
        dot: false,
        type_args: Vec::new(),
        name: name.to_string(),
        args,
        line: 0,
    }
}

fn var(name: &str) -> Expr {
    Expr::Var {
        id: Id::NEW,
        name: name.to_string(),
        line: 0,
    }
}

/// Appends an entry point per reachable structured builtin, plus its decoders.
/// `None` declines the whole run.
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
        id: Id::NEW,
        name: n.to_string(),
        capability: vyrn_frontend::ast::Capability::Read,
        ty: Type::Str,
        line: 0,
        col: 0,
    };
    // `fn <entry>(arg) -> T { @reflect(kind, arg); return <decode T>() }`.
    let mut entry =
        |name: String, params: Vec<Param>, ret: Type, kind: i64, arg: Expr, d: &mut Decoders| {
            let body = d.decode(&ret)?;
            entries.push(func(
                &name,
                params,
                ret,
                vec![
                    Stmt::Expr(
                        call(
                            vyrn_codegen::GEN_REFLECT,
                            vec![Expr::Int(kind, Id::NEW), arg],
                        ),
                        Id::NEW,
                    ),
                    Stmt::Return {
                        id: Id::NEW,
                        value: Some(body),
                        line: 0,
                    },
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
    // `prepare` has cleared `is_gen`, so `dispatchable` no longer answers.
    if p.functions.iter().any(|f| f.exported && takes_type_arg(f)) {
        entry(
            vyrn_codegen::GEN_ENTRY_TYPE_ARG.to_string(),
            Vec::new(),
            named("TypeArg"),
            vyrn_codegen::REFLECT_TYPE_ARG,
            Expr::Str(String::new(), Id::NEW),
            &mut dec,
        )?;
    }
    // `lex` is shadowable: a user function of that name wins, and emitting no
    // entry leaves that call site alone.
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
    // One nullary entry per declared contract: a contract name is a
    // declaration, not a value, and there are few per module closure.
    if reaches("contractOf") {
        for name in p
            .contracts
            .iter()
            .map(|c| c.name.clone())
            .collect::<Vec<_>>()
        {
            entry(
                vyrn_frontend::checker::gen_entry_contract_of(&name),
                Vec::new(),
                named("ContractInfo"),
                vyrn_codegen::REFLECT_CONTRACT_OF,
                Expr::Str(name, Id::NEW),
                &mut dec,
            )?;
        }
    }
    p.functions.extend(entries);
    p.functions.extend(dec.fns);
    p.number();
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
            types: p
                .type_decls
                .iter()
                .map(|t| (t.name.clone(), t.clone()))
                .collect(),
            fns: Vec::new(),
            made: std::collections::HashSet::new(),
        }
    }

    /// The expression that decodes one value of `ty`: a stream primitive for a
    /// scalar, else a call to a decoder materialized on demand.
    fn decode(&mut self, ty: &Type) -> Option<Expr> {
        Some(match vyrn_frontend::types::resolve(ty, &self.types) {
            Type::Str => call(vyrn_codegen::GEN_NEXT_STR, vec![]),
            Type::Int | Type::IntN { .. } => call(vyrn_codegen::GEN_NEXT_INT, vec![]),
            Type::Bool => Expr::Binary {
                id: Id::NEW,
                op: vyrn_frontend::ast::BinOp::Eq,
                lhs: Box::new(call(vyrn_codegen::GEN_NEXT_INT, vec![])),
                rhs: Box::new(Expr::Int(1, Id::NEW)),
                line: 0,
            },
            _ => {
                let name = self.materialize(ty)?;
                call(&name, vec![])
            }
        })
    }

    /// Emits the decoder for a composite type once and returns its name.
    fn materialize(&mut self, ty: &Type) -> Option<String> {
        // The readable suffix alone is not injective (a type named `Opt_Str`
        // mangles as `Option<String>` does), so the structural hash joins it.
        let name = format!(
            "__vyrnGenDec_{}_h{}",
            mangle(ty)?,
            &vyrn_frontend::hash::sha256_hex(format!("{ty:?}").as_bytes())[..16]
        );
        if !self.made.insert(name.clone()) {
            return Some(name);
        }
        let resolved = vyrn_frontend::types::resolve(ty, &self.types);
        // A resolved `Option<T>` is a variant list, so
        // the payload comes from the reader. A declared enum of other variants
        // declines.
        let opt = vyrn_frontend::types::option_payload(&resolved).cloned();
        let body = match resolved {
            // The length, then that many elements.
            Type::Array(inner) => {
                let elem = self.decode(&inner)?;
                vec![
                    Stmt::Let {
                        id: Id::NEW,
                        name: "n".into(),
                        mutable: false,
                        ty: Some(Type::Int),
                        value: call(vyrn_codegen::GEN_NEXT_INT, vec![]),
                        line: 0,
                        col: 0,
                    },
                    Stmt::Let {
                        id: Id::NEW,
                        name: "xs".into(),
                        mutable: true,
                        ty: Some(ty.clone()),
                        value: Expr::ArrayLit {
                            id: Id::NEW,
                            elems: Vec::new(),
                            line: 0,
                        },
                        line: 0,
                        col: 0,
                    },
                    Stmt::Let {
                        id: Id::NEW,
                        name: "i".into(),
                        mutable: true,
                        ty: Some(Type::Int),
                        value: Expr::Int(0, Id::NEW),
                        line: 0,
                        col: 0,
                    },
                    Stmt::While {
                        id: Id::NEW,
                        cond: Expr::Binary {
                            id: Id::NEW,
                            op: vyrn_frontend::ast::BinOp::Lt,
                            lhs: Box::new(var("i")),
                            rhs: Box::new(var("n")),
                            line: 0,
                        },
                        body: Block {
                            id: Id::NEW,
                            stmts: vec![
                                // `push` returns the reallocated array, so it
                                // is stored back.
                                Stmt::Assign {
                                    id: Id::NEW,
                                    name: "xs".into(),
                                    value: call("@push", vec![var("xs"), elem]),
                                    line: 0,
                                },
                                Stmt::Assign {
                                    id: Id::NEW,
                                    name: "i".into(),
                                    value: Expr::Binary {
                                        id: Id::NEW,
                                        op: vyrn_frontend::ast::BinOp::Add,
                                        lhs: Box::new(var("i")),
                                        rhs: Box::new(Expr::Int(1, Id::NEW)),
                                        line: 0,
                                    },
                                    line: 0,
                                },
                            ],
                        },
                        line: 0,
                    },
                    Stmt::Return {
                        id: Id::NEW,
                        value: Some(var("xs")),
                        line: 0,
                    },
                ]
            }
            // One tag atom, then the payload only when it is there.
            _ if opt.is_some() => {
                let some = self.decode(&opt.clone()?)?;
                vec![
                    Stmt::If {
                        id: Id::NEW,
                        cond: Expr::Binary {
                            id: Id::NEW,
                            op: vyrn_frontend::ast::BinOp::Eq,
                            lhs: Box::new(call(vyrn_codegen::GEN_NEXT_INT, vec![])),
                            rhs: Box::new(Expr::Int(1, Id::NEW)),
                            line: 0,
                        },
                        then_block: Block {
                            id: Id::NEW,
                            stmts: vec![Stmt::Return {
                                id: Id::NEW,
                                value: Some(call("Some", vec![some])),
                                line: 0,
                            }],
                        },
                        else_block: None,
                        line: 0,
                    },
                    Stmt::Return {
                        id: Id::NEW,
                        value: Some(var("None")),
                        line: 0,
                    },
                ]
            }
            // Fields in declaration order, which the host pushed: both sides
            // read `record_fields`.
            Type::Record(_) => {
                let Type::Named(rec) = ty else { return None };
                let fields = vyrn_frontend::types::record_fields(ty, &self.types)?;
                let mut lit = Vec::new();
                for f in &fields {
                    lit.push((f.name.clone(), self.decode(&f.ty)?));
                }
                vec![Stmt::Return {
                    id: Id::NEW,
                    value: Some(Expr::StructLit {
                        id: Id::NEW,
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

/// A decoder's name suffix, for the shapes reflected types are built from;
/// anything else declines.
fn mangle(ty: &Type) -> Option<String> {
    Some(match ty {
        Type::Named(n) => n.clone(),
        Type::Array(t) => format!("Arr_{}", mangle(t)?),
        // The built-in `Option`, however spelled; `encode` reads the same payload.
        _ if vyrn_frontend::types::option_payload(ty).is_some() => format!(
            "Opt_{}",
            mangle(vyrn_frontend::types::option_payload(ty).expect("an Option payload"))?
        ),
        Type::Str => "Str".into(),
        Type::Int => "Int".into(),
        Type::Bool => "Bool".into(),
        _ => return None,
    })
}

/// Compiles the wrapper program with the direct backend, which needs no C
/// toolchain. `vyrn-cli/tests/genwasm.rs` runs every generator example under
/// both engines and fails on a byte of difference; a program the backend
/// cannot compile declines.
#[cfg(feature = "host")]
fn compile_to_wasm(_key: &str, program: &Program) -> Result<Vec<u8>, EngineError> {
    vyrn_codegen::direct::compile_gen_host(program).map_err(|e| decline(&e))
}

/// `__vyrn_gen_read`'s modes, shared with the emitter.
#[cfg(feature = "host")]
const MODE_READ: i32 = 0;
#[cfg(feature = "host")]
const MODE_LIST: i32 = 2;
/// Not a read: `moduleInterface` needs the resolver and the loader, so it is
/// served on the host thread like one.
#[cfg(feature = "host")]
const MODE_MODULE_INTERFACE: i32 = 3;
/// `listDirKinds`: `MODE_LIST`'s encoding with `/` appended to each directory.
#[cfg(feature = "host")]
const MODE_LIST_KINDS: i32 = 4;

/// One unit of a structured host result. Encoder and decoder walk the same
/// static type, so the variant is not a tag the decoder consults: a `nextInt`
/// that finds a string means the two walks disagreed.
pub enum Atom {
    Int(i64),
    Str(Vec<u8>),
}

#[derive(Default)]
#[cfg(feature = "host")]
struct Streams {
    /// NUL-terminated argv, argv[0] first, as `args_get` writes it.
    argv: Vec<Vec<u8>>,
    stdout: Vec<u8>,
    stderr: Vec<u8>,
    gen: GenState,
    /// The linear memory, held rather than looked up per call: a separate shim
    /// module exports it, and imports are called from both modules.
    mem: Option<wasmtime::Memory>,
}

#[cfg(feature = "host")]
impl GenHost for Streams {
    fn gen(&mut self) -> &mut GenState {
        &mut self.gen
    }
    fn memory(&self) -> Option<wasmtime::Memory> {
        self.mem
    }
}

/// A store that can serve the `vyrn_gen` imports through [`link`]: this crate's
/// generation run, and the driver's `test` run of a module that reaches a
/// generator.
#[cfg(feature = "host")]
pub trait GenHost {
    fn gen(&mut self) -> &mut GenState;
    fn memory(&self) -> Option<wasmtime::Memory>;
}

/// The host side of the `vyrn_gen` imports: the code arena, the atom stream,
/// the stash, and the declarations `contractOf` and `lex` read.
#[derive(Default)]
#[cfg(feature = "host")]
pub struct GenState {
    /// The last served read or `render`, waiting for `fetch` to copy it into
    /// guest memory. The host never allocates on the guest's side.
    stash: Vec<u8>,
    /// The code-quote arena; a `Code` value is an index into it. Per generation
    /// run, so a handle from one run is meaningless in the next.
    code: Vec<Vec<CodePiece>>,
    /// The structured result being handed over, and the decoder's position. A
    /// stream is fully consumed before the next `reflect`.
    atoms: Vec<Atom>,
    cursor: usize,
    /// The generator's declarations, so `contractOf` and `lex` are answered on
    /// this thread without the resolver.
    types: std::collections::HashMap<String, vyrn_frontend::ast::TypeDecl>,
    contracts: Vec<vyrn_frontend::ast::ContractDecl>,
    /// What the generator's `TypeArg` parameter receives.
    type_arg: Option<Expr>,
    /// The line to the thread that holds the resolver; `None` for a `test`
    /// door, whose `readFile` and `moduleInterface` are refused.
    caps: Option<Caps>,
}

#[cfg(feature = "host")]
impl GenState {
    /// Takes the declarations `contractOf` and `lex` reflect over.
    pub fn new(p: &Program) -> Self {
        GenState {
            types: p
                .type_decls
                .iter()
                .map(|t| (t.name.clone(), t.clone()))
                .collect(),
            contracts: p.contracts.clone(),
            ..GenState::default()
        }
    }
}

#[cfg(feature = "host")]
impl GenState {
    /// The pieces a handle names. Compiled code only holds handles the imports
    /// made, but the index is guest-supplied, so it is checked.
    fn pieces(&self, h: i64) -> wasmtime::Result<&Vec<CodePiece>> {
        self.code
            .get(h as usize)
            .ok_or_else(|| wasmtime::Error::msg(format!("bad code handle {h}")))
    }

    fn intern(&mut self, pieces: Vec<CodePiece>) -> i64 {
        self.code.push(pieces);
        self.code.len() as i64 - 1
    }

    /// Starts handing over `lit` as a value of type `ty`. An unread atom from
    /// the previous stream means the decoder walked a shorter type than the
    /// encoder, and fails the run.
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

/// Pushes `lit`, a literal from the compiler's reflection, onto the atom stream
/// by walking the static type, as the decoder does. Fields are taken from the
/// literal by name, so their order in the literal does not matter.
pub fn encode(
    ty: &Type,
    lit: &Expr,
    types: &std::collections::HashMap<String, vyrn_frontend::ast::TypeDecl>,
    out: &mut Vec<Atom>,
) -> Result<(), String> {
    let wrong = || format!("cannot encode {lit:?} as {ty}");
    let resolved = vyrn_frontend::types::resolve(ty, types);
    // As in `Decoders::materialize`: the payload comes from the reader.
    let opt = vyrn_frontend::types::option_payload(&resolved).cloned();
    match resolved {
        Type::Str => match lit {
            Expr::Str(s, _) => out.push(Atom::Str(s.clone().into_bytes())),
            _ => return Err(wrong()),
        },
        Type::Int | Type::IntN { .. } => match lit {
            Expr::Int(n, _) => out.push(Atom::Int(*n)),
            _ => return Err(wrong()),
        },
        Type::Bool => match lit {
            Expr::Bool(b, _) => out.push(Atom::Int(*b as i64)),
            _ => return Err(wrong()),
        },
        // One presence atom, then the payload if there is one.
        _ if opt.is_some() => match lit {
            Expr::Call { name, args, .. } if name == "Some" && args.len() == 1 => {
                out.push(Atom::Int(1));
                encode(&opt.clone().ok_or_else(wrong)?, &args[0], types, out)?;
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
/// The guest runs on its own thread because wasmtime needs `'static` store data
/// and the resolver is borrowed; channels avoid an `unsafe` lifetime extension.
#[cfg(feature = "host")]
struct Caps {
    req: mpsc::Sender<(String, i32)>,
    resp: mpsc::Receiver<Result<Served, String>>,
}

/// The host thread's answer: bytes for a read or a listing, or the reflection
/// literal for `moduleInterface`.
#[cfg(feature = "host")]
enum Served {
    Bytes(i32, Vec<u8>),
    /// Encoded on the guest thread, so one [`encode`] call serves all three
    /// structured builtins.
    Lit(Box<Expr>),
}

/// `proc_exit`, carried out of the guest as an error: the only way to stop it.
#[derive(Debug)]
#[cfg(feature = "host")]
struct Exit(i32);

#[cfg(feature = "host")]
#[cfg(feature = "host")]
impl std::fmt::Display for Exit {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "exit {}", self.0)
    }
}

#[cfg(feature = "host")]
#[cfg(feature = "host")]
impl std::error::Error for Exit {}

/// A host-side refusal carried out as a trap: a read outside the declared
/// inputs, or a value with no splice rule. Both abort generation; neither may
/// reach the generator as a value.
#[derive(Debug)]
#[cfg(feature = "host")]
struct Denied(String);

#[cfg(feature = "host")]
#[cfg(feature = "host")]
impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(feature = "host")]
#[cfg(feature = "host")]
impl std::error::Error for Denied {}

/// Serves the `vyrn_gen` imports [`vyrn_codegen::direct::compile_gen_host`]
/// emits, on any [`GenHost`] store. Splicing, escaping and float formatting
/// are the frontend's own functions (`vyrn_frontend::gen`).
#[cfg(feature = "host")]
pub fn link<T: GenHost + 'static>(linker: &mut wasmtime::Linker<T>) -> wasmtime::Result<()> {
    use wasmtime::{Caller, Error, Result};
    // `read` resolves and stashes; `fetch` copies the stash into a buffer the
    // guest allocated, so the host never allocates in linear memory.
    linker.func_wrap(
        "vyrn_gen",
        "read",
        |mut caller: Caller<'_, T>, path: i32, mode: i32| -> Result<i64> {
            let (data, host) = guest_mem(&mut caller)?;
            let streams = host.gen();
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
                // A scoping violation traps.
                Ok(Err(msg)) => Err(Error::new(Denied(msg))),
                Err(_) => Err(Error::msg("generator host is gone")),
            }
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "fetch",
        |mut caller: Caller<'_, T>, dest: i32| -> Result<()> {
            let (data, host) = guest_mem(&mut caller)?;
            let streams = host.gen();
            let stash = std::mem::take(&mut streams.stash);
            let slot = data
                .get_mut(dest as usize..dest as usize + stash.len())
                .ok_or_else(|| Error::msg("bad fetch destination"))?;
            slot.copy_from_slice(&stash);
            Ok(())
        },
    )?;

    // The code-quote arena: `Code` is an i64 handle, and every operation on it
    // runs here.
    linker.func_wrap(
        "vyrn_gen",
        "text",
        |mut caller: Caller<'_, T>, s: i32| -> Result<i64> {
            let (data, host) = guest_mem(&mut caller)?;
            let streams = host.gen();
            let text = cstr(data, s)?;
            Ok(streams.intern(vec![CodePiece::Text(text)]))
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "rawAt",
        |mut caller: Caller<'_, T>, s: i32, path: i32, line: i64, col: i64| -> Result<i64> {
            let (data, host) = guest_mem(&mut caller)?;
            let streams = host.gen();
            let text = cstr(data, s)?;
            let path = cstr(data, path)?;
            Ok(streams.intern(vec![CodePiece::Origin {
                path,
                line,
                col,
                text,
            }]))
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "splice",
        |mut caller: Caller<'_, T>, tag: i32, bits: i64, p: i32, ctx: i64| -> Result<i64> {
            let (data, host) = guest_mem(&mut caller)?;
            let streams = host.gen();
            let val = splice_value(tag, bits, p, data, streams)?;
            // A splice violation traps.
            let pieces = vyrn_frontend::gen::gen_code_splice(&val, ctx)
                .map_err(|m| Error::new(Denied(m)))?;
            Ok(caller.data_mut().gen().intern(pieces))
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "concat",
        |mut caller: Caller<'_, T>, a: i64, b: i64| -> Result<i64> {
            let s = caller.data_mut().gen();
            let mut pieces = s.pieces(a)?.clone();
            pieces.extend(s.pieces(b)?.iter().cloned());
            Ok(s.intern(pieces))
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "render",
        |mut caller: Caller<'_, T>, h: i64| -> Result<i64> {
            let s = caller.data_mut().gen();
            let text = vyrn_frontend::gen::render_code(s.pieces(h)?);
            s.stash = text.into_bytes();
            Ok(s.stash.len() as i64)
        },
    )?;

    // `reflect` computes a value of a known named type in the host (the
    // lexer, linker and contract tables are compiler machinery) and leaves it
    // as an atom stream the decoder pulls back.
    linker.func_wrap(
        "vyrn_gen",
        "reflect",
        |mut caller: Caller<'_, T>, kind: i64, arg: i32| -> Result<()> {
            let (data, host) = guest_mem(&mut caller)?;
            let streams = host.gen();
            let arg = cstr(data, arg)?;
            match kind {
                // Needs the resolver, so it goes to the host thread.
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
                        // A scoping violation traps.
                        Ok(Err(msg)) => return Err(Error::new(Denied(msg))),
                        Err(_) => return Err(Error::msg("generator host is gone")),
                    };
                    streams.stream(&Type::Named("ModuleInterface".into()), &lit)
                }
                // The generator's module closure carries its contracts.
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
                vyrn_codegen::REFLECT_TYPE_ARG => {
                    let lit = streams
                        .type_arg
                        .clone()
                        .ok_or_else(|| Error::msg("no type argument"))?;
                    streams.stream(&Type::Named("TypeArg".into()), &lit)
                }
                vyrn_codegen::REFLECT_LEX => {
                    let lit = vyrn_frontend::gen::gen_lex_tokens_lit(&arg);
                    streams.stream(&Type::Array(Box::new(Type::Named("Token".into()))), &lit)
                }
                other => Err(Error::msg(format!("bad reflect kind {other}"))),
            }
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "nextInt",
        |mut caller: Caller<'_, T>| -> Result<i64> {
            match caller.data_mut().gen().next_atom()? {
                Atom::Int(n) => Ok(*n),
                Atom::Str(_) => Err(Error::msg("reflected value: expected an Int atom")),
            }
        },
    )?;
    // Length, then `fetch`, so the host never allocates in guest memory.
    linker.func_wrap(
        "vyrn_gen",
        "nextStr",
        |mut caller: Caller<'_, T>| -> Result<i64> {
            let s = caller.data_mut().gen();
            let bytes = match s.next_atom()? {
                Atom::Str(b) => b.clone(),
                Atom::Int(_) => return Err(Error::msg("reflected value: expected a Str atom")),
            };
            s.stash = bytes;
            Ok(s.stash.len() as i64)
        },
    )?;
    Ok(())
}

/// The guest's memory and its store data, held at once.
#[cfg(feature = "host")]
fn guest_mem<'a, T: GenHost>(
    caller: &'a mut wasmtime::Caller<'_, T>,
) -> wasmtime::Result<(&'a mut [u8], &'a mut T)> {
    let Some(mem) = caller.data().memory() else {
        return Err(wasmtime::Error::msg("generator has no memory"));
    };
    Ok(mem.data_and_store_mut(caller))
}

/// A NUL-terminated guest string. Vyrn strings are UTF-8 with no interior NUL,
/// so this is a copy, not a parse.
#[cfg(feature = "host")]
fn cstr(data: &[u8], at: i32) -> wasmtime::Result<String> {
    let rest = data
        .get(at as usize..)
        .ok_or_else(|| wasmtime::Error::msg("bad string pointer"))?;
    let n = rest.iter().position(|b| *b == 0).unwrap_or(rest.len());
    Ok(String::from_utf8_lossy(&rest[..n]).into_owned())
}

/// Rebuilds the value a `@codeSplice` call splices, from the tag codegen chose
/// and the one word it sent, so the frontend's one splice rule runs on it.
/// Floats cross as bits because the shortest-roundtrip formatting belongs on
/// this side.
#[cfg(feature = "host")]
fn splice_value(
    tag: i32,
    bits: i64,
    p: i32,
    data: &[u8],
    streams: &GenState,
) -> wasmtime::Result<Spliced> {
    Ok(match tag {
        vyrn_codegen::TAG_STR => Spliced::Str(cstr(data, p)?),
        vyrn_codegen::TAG_CODE => Spliced::Code(streams.pieces(bits)?.clone()),
        vyrn_codegen::TAG_BOOL => Spliced::Bool(bits != 0),
        // Codegen sign-extends a signed integer of any width.
        vyrn_codegen::TAG_INT => Spliced::Int {
            v: bits,
            signed: true,
        },
        vyrn_codegen::TAG_UINT => Spliced::Int {
            v: bits,
            signed: false,
        },
        vyrn_codegen::TAG_F64 => Spliced::F64(f64::from_bits(bits as u64)),
        vyrn_codegen::TAG_F32 => Spliced::F32(f32::from_bits(bits as u32)),
        other => return Err(wasmtime::Error::msg(format!("bad splice tag {other}"))),
    })
}

#[cfg(feature = "host")]
const ERRNO_SUCCESS: i32 = 0;
#[cfg(feature = "host")]
const ERRNO_BADF: i32 = 8;
#[cfg(feature = "host")]
const ERRNO_SPIPE: i32 = 29;

#[cfg(feature = "host")]
fn wr32(data: &mut [u8], at: i32, v: u32) -> Option<()> {
    let at = at as usize;
    data.get_mut(at..at + 4)?.copy_from_slice(&v.to_le_bytes());
    Some(())
}

#[cfg(feature = "host")]
fn rd32(data: &[u8], at: i32) -> Option<u32> {
    let at = at as usize;
    Some(u32::from_le_bytes(data.get(at..at + 4)?.try_into().ok()?))
}

/// The key of a compiled artifact: the generator's whole module closure, not
/// the function or the arguments (both arrive as argv), so one artifact serves
/// every generator in the module for every call.
///
/// With a fingerprint (content hashes of the closure's sources, which the
/// loader computes anyway) the key is cheap, carries the compiler's identity,
/// and may persist to disk. Without one (a generated module in the closure) the
/// key hashes the program's `Debug` and stays in memory: a cheap key that
/// missed an edit would run a stale artifact.
#[cfg(feature = "host")]
fn artifact_key(program: &Program, fingerprint: Option<&str>) -> String {
    use std::fmt::Write as _;

    // The `gen1` tag keeps artifacts of other key schemes out of this namespace.
    if let Some(fp) = fingerprint {
        return vyrn_frontend::hash::sha256_hex(
            format!("gen1\u{0}{}\u{0}{fp}", compiler_identity()).as_bytes(),
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

    let mut sink = Sink(0xcbf2_9ce4_8422_2325);
    let _ = write!(sink, "gen1\u{0}{program:?}");
    format!("{:016x}", sink.0)
}

/// Where compiled artifacts persist: inside the generation cache, so clearing it
/// clears them.
#[cfg(feature = "host")]
fn artifact_dir() -> PathBuf {
    vyrn_frontend::manifest::gen_cache_dir().join("wasm")
}

/// Reads a stored artifact back, skipping Cranelift on a cold start.
///
/// `Module::deserialize` maps in native code and trusts its input, so a file is
/// deserialized only when its tag verifies under the per-user secret of
/// `vyrn_frontend::loader::gen_cache_tag`: any process running as the user can
/// write to the cache directory. Every failure is a cache miss.
#[cfg(feature = "host")]
fn load_artifact(key: &str) -> Option<wasmtime::Module> {
    let bytes = std::fs::read(artifact_dir().join(key)).ok()?;
    let (tag, module) = bytes.split_at_checked(ARTIFACT_TAG_LEN)?;
    if vyrn_frontend::loader::gen_cache_tag(key, module).as_bytes() != tag {
        return None;
    }
    // SAFETY: the tag proves this user's compiler serialized these bytes, and
    // wasmtime's header refuses a foreign build.
    unsafe { wasmtime::Module::deserialize(wasm_engine(), module) }.ok()
}

/// The authentication tag prefixed to every stored artifact: a sha256 in hex.
#[cfg(feature = "host")]
const ARTIFACT_TAG_LEN: usize = 64;

/// Stores an artifact for the next session. Best-effort: a failure costs a
/// recompile.
#[cfg(feature = "host")]
fn store_artifact(key: &str, module: &wasmtime::Module) {
    let Ok(bytes) = module.serialize() else {
        return;
    };
    let mut tagged = vyrn_frontend::loader::gen_cache_tag(key, &bytes).into_bytes();
    debug_assert_eq!(tagged.len(), ARTIFACT_TAG_LEN);
    tagged.extend_from_slice(&bytes);
    let bytes = tagged;
    let dir = artifact_dir();
    if std::fs::create_dir_all(&dir).is_err() {
        return;
    }
    let _ = vyrn_frontend::manifest::write_whole(&dir.join(key), &bytes);
}

/// The generator step budget converted into fuel.
///
/// A step is one statement, and wasmtime spends one fuel per wasm
/// instruction, so the mapping is biased loose: whatever runs within the step
/// budget runs within this one. Measured fuel per step over
/// every generator in the repo ranges from 74 (`std/i18n`) to 411 (`std/rpc`);
/// 1,000 per step plus a flat 1M leaves ~2.4x margin. One statement can copy
/// unbounded bytes, so this is a margin, not a proof. The default budget burns
/// out in about 0.7 s.
#[cfg(feature = "host")]
fn wasm_fuel(steps: u64) -> u64 {
    steps.saturating_mul(1_000).saturating_add(1_000_000)
}

/// The process's wasmtime engine, with fuel metering on.
#[cfg(feature = "host")]
fn wasm_engine() -> &'static wasmtime::Engine {
    static ENGINE: std::sync::OnceLock<wasmtime::Engine> = std::sync::OnceLock::new();
    ENGINE.get_or_init(|| {
        let mut cfg = wasmtime::Config::new();
        // Fuel, not epochs: an epoch is wall-clock, and the same generator must
        // pass or fail on every machine.
        cfg.consume_fuel(true);
        wasmtime::Engine::new(&cfg).unwrap_or_default()
    })
}

#[cfg(feature = "host")]
fn module_cache() -> &'static std::sync::Mutex<std::collections::HashMap<String, wasmtime::Module>>
{
    static CACHE: std::sync::OnceLock<
        std::sync::Mutex<std::collections::HashMap<String, wasmtime::Module>>,
    > = std::sync::OnceLock::new();
    CACHE.get_or_init(Default::default)
}

/// Compiles once and runs, returning the source the guest framed on stdout.
/// Three tiers, cheapest first: this process's modules, artifacts on disk,
/// then emit plus Cranelift.
#[cfg(feature = "host")]
fn run_module(
    key: &str,
    persist: bool,
    argv: &[String],
    program: &Program,
    inputs: &GenInputs<'_>,
    reads: &mut Vec<GenRead>,
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
                    trace("emit", t.elapsed());
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

/// Runs the guest on its own thread and serves its capability requests from
/// this one, where the resolver lives. The loop ends when the guest's store,
/// and with it its `Sender`, is dropped.
#[cfg(feature = "host")]
fn run_hosted(
    module: &wasmtime::Module,
    argv: &[String],
    program: &Program,
    inputs: &GenInputs<'_>,
    reads: &mut Vec<GenRead>,
) -> Result<String, EngineError> {
    let fuel = wasm_fuel(inputs.fuel);
    let (req_tx, req_rx) = mpsc::channel::<(String, i32)>();
    let (resp_tx, resp_rx) = mpsc::channel::<Result<Served, String>>();

    let module = module.clone();
    let argv: Vec<String> = argv.to_vec();
    // Cloned because the store's data must be `'static`.
    let types = program
        .type_decls
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect();
    let contracts = program.contracts.clone();
    let type_arg = inputs.type_arg.clone();
    let guest = std::thread::Builder::new()
        // Compiled code runs on this stack; a deeply recursive generator would
        // overflow the default before wasmtime's own stack limit.
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            run_wasm(
                &module,
                &argv,
                types,
                contracts,
                type_arg,
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

/// Instantiates the module and returns the source it framed on stdout.
///
/// Embedded rather than spawned: the `wasmtime` CLI's launch measured ~106 ms. WASI is a hand-written minimum;
/// every other import traps.
#[cfg(feature = "host")]
fn run_wasm(
    module: &wasmtime::Module,
    argv: &[String],
    types: std::collections::HashMap<String, vyrn_frontend::ast::TypeDecl>,
    contracts: Vec<vyrn_frontend::ast::ContractDecl>,
    type_arg: Option<Expr>,
    fuel: u64,
    caps: Caps,
) -> Result<String, EngineError> {
    use wasmtime::*;

    let engine = wasm_engine();
    let mut linker: Linker<Streams> = Linker::new(engine);
    let wasi = "wasi_snapshot_preview1";
    link(&mut linker).map_err(|e| EngineError::Failed(e.to_string()))?;

    // The only import that does work.
    linker
        .func_wrap(
            wasi,
            "fd_write",
            |mut caller: Caller<'_, Streams>, fd: i32, iovs: i32, iovs_len: i32, nwritten: i32| {
                let Some(mem) = caller.data().mem else {
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

    // A character device (a tty) as every stdio fd's type.
    linker
        .func_wrap(
            wasi,
            "fd_fdstat_get",
            |mut caller: Caller<'_, Streams>, fd: i32, buf: i32| {
                let Some(mem) = caller.data().mem else {
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
        .func_wrap(wasi, "fd_seek", |_: i32, _: i64, _: i32, _: i32| {
            ERRNO_SPIPE
        })
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    // The generator's arguments; the artifact is argument-independent because
    // they arrive here.
    linker
        .func_wrap(
            wasi,
            "args_sizes_get",
            |mut caller: Caller<'_, Streams>, count: i32, size: i32| {
                let Some(mem) = caller.data().mem else {
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
                let Some(mem) = caller.data().mem else {
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
    // No environment: a generator gets none under either engine.
    linker
        .func_wrap(
            wasi,
            "environ_sizes_get",
            |mut caller: Caller<'_, Streams>, count: i32, size: i32| {
                let Some(mem) = caller.data().mem else {
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
    // argv[0] is the program name, which `args()` skips.
    let mut world = Streams {
        gen: GenState {
            caps: Some(caps),
            types,
            contracts,
            type_arg,
            ..GenState::default()
        },
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

    // Any other import is a generator this path does not serve; it traps
    // rather than answers wrong.
    linker
        .define_unknown_imports_as_traps(module)
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    let inst = linker
        .instantiate(&mut store, module)
        .map_err(|e| EngineError::Failed(format!("instantiate: {e}")))?;

    let result = {
        let start = inst
            .get_typed_func::<(), ()>(&mut store, "_start")
            .map_err(|e| EngineError::Failed(format!("_start: {e}")))?;
        store.data_mut().mem = match inst.get_export(&mut store, "memory") {
            Some(Extern::Memory(m)) => Some(m),
            _ => return Err(EngineError::Failed("generator has no memory".into())),
        };
        start.call(&mut store, ())
    };
    if std::env::var("VYRN_GENWASM_TRACE").is_ok() {
        // The fuel a real generator spends, which sizes `wasm_fuel`.
        let spent = fuel - store.get_fuel().unwrap_or(0);
        eprintln!("genwasm fuel: {spent}");
    }
    let streams = store.into_data();
    match result {
        // Out of fuel is reworded as the step-budget message.
        Err(e) if e.downcast_ref::<Trap>() == Some(&Trap::OutOfFuel) => {
            return Err(EngineError::Failed(
                "generator exceeded its step budget".into(),
            ))
        }
        Ok(()) => {}
        Err(e) => match e.downcast_ref::<Exit>() {
            Some(Exit(0)) => {}
            // The guest failed on its own terms; its message is the last line
            // of stderr. The `error: ` prefix comes off because generation
            // takes the bare message and the loader adds context.
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
            // The message comes from the payload, not `e`: wasmtime wraps a
            // host error in a guest backtrace.
            None if e.downcast_ref::<Denied>().is_some() => {
                return Err(EngineError::Failed(
                    e.downcast_ref::<Denied>().unwrap().0.clone(),
                ))
            }
            None => return Err(EngineError::Failed(format!("generator trapped: {e}"))),
        },
    }
    // The last reflected value has no following `reflect` to check it.
    streams
        .gen
        .drained()
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    // Compile-time prints go to the compiler's stdout, never into the source.
    let (prints, source) = match unframe_result(&streams.stdout) {
        Ok(framed) => framed,
        Err(why) => return Err(decline(why)),
    };
    if !prints.is_empty() {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = out.write_all(&prints);
        let _ = out.flush();
    }
    String::from_utf8(source)
        .map_err(|_| EngineError::Failed("generator emitted invalid UTF-8".into()))
}

#[cfg(test)]
#[cfg(feature = "host")]
mod tests {
    use super::*;

    /// The framing `wrapper_program` emits, as the guest's stdout carries it.
    fn framed(prints: &str, source: &str) -> Vec<u8> {
        format!("{prints}{RESULT_BEGIN}\n{source}\n{RESULT_END}\n").into_bytes()
    }

    #[test]
    fn a_generators_own_prints_never_enter_the_source() {
        let (prints, source) = unframe_result(&framed("debug\n", "fn f() -> Int64 { return 1 }"))
            .expect("the wrapper's own framing unframes");
        assert_eq!(prints, b"debug\n");
        assert_eq!(source, b"fn f() -> Int64 { return 1 }");

        let (prints, source) = unframe_result(&framed("", "fn g() -> Str { return \"\" }"))
            .expect("an unprinted frame still unframes");
        assert!(prints.is_empty());
        assert_eq!(source, b"fn g() -> Str { return \"\" }");
    }

    #[test]
    fn stdout_that_does_not_frame_exactly_once_is_declined() {
        // No markers.
        assert!(unframe_result(b"fn f() -> Int64 { return 1 }\n").is_err());
        // The generator printed a marker of its own: two begins.
        let echoed = framed(
            &format!("x{RESULT_BEGIN}\n"),
            "fn f() -> Int64 { return 1 }",
        );
        assert!(unframe_result(&echoed).is_err());
        // The result contains a marker: two ends.
        let inside = framed("", &format!("fn f() -> Str {{ return \"{RESULT_END}\" }}"));
        assert!(unframe_result(&inside).is_err());
    }

    /// `load_artifact` maps its bytes in as native code, so this is its
    /// `unsafe` block's soundness argument.
    #[test]
    fn an_artifact_this_compiler_did_not_write_is_never_deserialized() {
        let tmp = std::env::temp_dir().join(format!("vyrn-artifact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);
        std::env::set_var("VYRN_GEN_CACHE_DIR", &tmp);

        let module = wasmtime::Module::new(wasm_engine(), [0, b'a', b's', b'm', 1, 0, 0, 0])
            .expect("an empty module compiles");
        let key = "artifacttest";
        store_artifact(key, &module);
        let path = artifact_dir().join(key);
        assert!(path.is_file(), "the control: it was stored");
        assert!(load_artifact(key).is_some(), "the control: it reads back");
        let stored = std::fs::read(&path).unwrap();

        // The module altered under an otherwise honest tag.
        let mut swapped = stored.clone();
        let last = swapped.len() - 1;
        swapped[last] ^= 0xff;
        std::fs::write(&path, &swapped).unwrap();
        assert!(load_artifact(key).is_none(), "the tag covers the module");

        // The same artifact filed under another key.
        std::fs::write(artifact_dir().join("otherkey"), &stored).unwrap();
        assert!(
            load_artifact("otherkey").is_none(),
            "the tag covers the key"
        );

        // An untagged file.
        std::fs::write(&path, &stored[ARTIFACT_TAG_LEN..]).unwrap();
        assert!(load_artifact(key).is_none(), "untagged is not trusted");

        std::fs::write(&path, b"short").unwrap();
        assert!(load_artifact(key).is_none());

        std::env::remove_var("VYRN_GEN_CACHE_DIR");
        let _ = std::fs::remove_dir_all(&tmp);
    }
}
