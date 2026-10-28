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
pub mod wasi;

#[cfg(feature = "host")]
use std::path::PathBuf;
#[cfg(feature = "host")]
use std::sync::mpsc;

use std::sync::Arc;
use vyrn_frontend::ast::{Block, Expr, Function, Id, Param, Program, Stmt, Type};
use vyrn_frontend::consteval::ConstVal;
#[cfg(feature = "host")]
use vyrn_frontend::gen::{compiler_identity, GenEngine, GenRead};
use vyrn_frontend::gen::{
    Author, GenError, GenInputs, GenOutput, Kind, Pos, Rope, Shown, Span, Spliced, Written,
};
use vyrn_frontend::loader::GEN_HOST;

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

/// Returns the wasm generation engine, which a host passes to each load.
#[cfg(feature = "host")]
pub fn engine() -> Box<GenEngine> {
    vyrn_lower::gen_engine(claim)
}

/// Claims a generation run, or declines it with `None`, which the frontend
/// reports as an error.
#[cfg(feature = "host")]
fn claim(
    program: &Program,
    fn_name: &str,
    args: &[ConstVal],
    inputs: &GenInputs<'_>,
) -> Option<Result<GenOutput, GenError>> {
    match run(program, fn_name, args, inputs) {
        // A decline judges the program (`vyrn_lower::gen_engine`), which only a
        // checked program may meet.
        Err(EngineError::Unsupported) if !inputs.checked => Some(Err(GenError::Unchecked)),
        Err(EngineError::Unsupported) => None,
        Err(EngineError::Failed(e)) => Some(Err(GenError::Failed(e))),
        Err(EngineError::Refused(ds)) => Some(Err(GenError::Refused(ds))),
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
    /// The typed judgment refused the generator's program.
    Refused(Vec<vyrn_frontend::diagnostics::Diagnostic>),
}

/// The first call anywhere in the program this path cannot serve.
///
/// Whole-program rather than reachability-precise: a false decline costs
/// speed, a false accept would run a generator outside its sandbox.
fn reaches_unserved(program: &Program) -> Option<String> {
    program.functions.iter().find_map(|f| {
        let mut hits: Vec<String> = vyrn_frontend::checker::fn_calls(&f.body, &Default::default())
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
    // Judged only when the artifact is built: the key names the program's
    // sources, so a cached artifact is one a program with no unserved call built.
    let written = run_module(&key, persist, &argv, program, inputs, &mut reads, || {
        if let Some(what) = reaches_unserved(program) {
            return Err(decline(&format!("the module reaches `{what}`")));
        }
        let wrapper = wrapper_program(program)
            .ok_or_else(|| decline("the wrapper program cannot be synthesized"))?;
        compile_to_wasm(&key, &wrapper)
    })?;
    trace("run", t.elapsed());
    if written.source.len() > inputs.max_output {
        return Err(EngineError::Failed(format!(
            "generator output exceeds the {} byte cap",
            inputs.max_output
        )));
    }
    Ok(GenOutput { written, reads })
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
    // `codeAt` copies only bytes this run read: the bytes the host served for
    // the path, answered with the key they were read under.
    if mode == MODE_CODE_AT {
        let key = vyrn_frontend::gen::gen_scoped_path(
            &inputs.importer_dir,
            &inputs.allowed,
            &inputs.aliased,
            path,
        )?;
        let read = reads
            .iter()
            .find_map(|(k, b)| (*k == key).then_some(b.as_ref()));
        return match read.flatten() {
            Some(bytes) => Ok(Served::Copy(key, bytes.clone())),
            None => Err(format!(
                "`codeAt` copies only an input the generator read, and it did not read `{path}`"
            )),
        };
    }
    // The frontend's own implementation, so the recorded reads are every
    // module the link touched.
    if mode == MODE_MODULE_INTERFACE {
        return vyrn_frontend::gen::gen_module_interface_lit(
            inputs.engine,
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
/// with the argv and the `TypeArg`'s atoms, and answers the module's stdout
/// and the [`Arena`] its code imports ran in.
///
/// Only a generator whose one parameter is a `TypeArg` is served: `exec`'s
/// host answers no read and no module reflection, so any other generator is
/// declined (`None`).
pub fn run_pure(
    program: &Program,
    fn_name: &str,
    args: &[ConstVal],
    inputs: &GenInputs<'_>,
    exec: impl FnOnce(&[u8], &[String], &[Atom]) -> Result<(Vec<u8>, Arena), String>,
) -> Option<Result<GenOutput, GenError>> {
    let target = program.functions.iter().find(|f| f.name == fn_name)?;
    let arg = inputs.type_arg.as_ref()?;
    if !dispatchable(target) || !takes_type_arg(target) || !args.is_empty() {
        return None;
    }
    if reaches_unserved(program).is_some() {
        return None;
    }
    let types = type_map(program);
    let mut atoms = Vec::new();
    let run = || -> Result<GenOutput, GenError> {
        encode(&Type::Named("TypeArg".into()), arg, &types, &mut atoms)
            .map_err(GenError::Failed)?;
        let wrapper = wrapper_program(program).ok_or(GenError::Failed(
            "the wrapper program cannot be synthesized".into(),
        ))?;
        let bytes = vyrn_codegen::direct::compile_gen_host(&wrapper)?;
        let (stdout, mut code) =
            exec(&bytes, &[fn_name.to_string()], &atoms).map_err(GenError::Failed)?;
        let written = match code.written() {
            Some(written) => written,
            None => {
                let (_, source) =
                    unframe_result(&stdout).map_err(|e| GenError::Failed(e.into()))?;
                let source = String::from_utf8(source)
                    .map_err(|_| GenError::Failed("generator emitted invalid UTF-8".into()))?;
                Written {
                    source,
                    spans: None,
                }
            }
        };
        let reads = Vec::new();
        Ok(GenOutput { written, reads })
    };
    Some(run())
}

/// Whether the engine can serve this function: an exported `gen fn` returning
/// `String` or `Code` and taking `String` or `Int64` parameters (written to argv
/// and read back, `Int64` by `parse`), or one `TypeArg` (reflected, as
/// `moduleInterface` is).
fn dispatchable(f: &Function) -> bool {
    f.is_gen
        && f.exported
        && vyrn_frontend::gen::writes_module(&f.ret)
        && (takes_type_arg(f)
            || f.params
                .iter()
                .all(|par| matches!(par.ty, Type::Str | Type::Int)))
}

fn returns_code(f: &Function) -> bool {
    f.ret == Type::Named("Code".into())
}

fn takes_type_arg(f: &Function) -> bool {
    matches!(f.params.as_slice(), [p] if p.ty == Type::Named("TypeArg".into()))
}

/// The generator's module with `is_gen` cleared, plus a `main` that dispatches
/// on `args()[0]` to the requested generator and hands over its result: a
/// `String` printed between the markers, a `Code` handle through the `result`
/// import.
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
    // The locals are spelled with `@`, which no source can write: a bare call
    // resolves to a local first, so a generator named `g` would hit one.
    let argv = |i: usize| {
        Expr::call(
            GEN_HOST.at,
            vec![Expr::var("@argv", 0), Expr::int(i as i64)],
            0,
        )
    };
    let mut body = vec![
        Stmt::let_("@argv", Expr::call(GEN_HOST.args, vec![], 0), 0),
        Stmt::Let {
            id: Id::NEW,
            name: "@g".into(),
            mutable: false,
            ty: Some(Type::Str),
            clause: None,
            value: argv(0),
            line: 0,
            col: 0,
        },
    ];
    for f in p.functions.iter().filter(|f| dispatchable(f)) {
        let call = Expr::call(
            &f.name,
            f.params
                .iter()
                .enumerate()
                .map(|(i, par)| match takes_type_arg(f) {
                    true => Expr::var("@typeArg", 0),
                    false => at_type(argv(i + 1), &par.ty),
                })
                .collect(),
            0,
        );
        let hand_over = match returns_code(f) {
            true => vec![Stmt::expr(Expr::call(
                vyrn_codegen::GEN_RESULT,
                vec![call],
                0,
            ))],
            // Framed between marker lines; see `unframe_result`.
            false => vec![
                Stmt::expr(Expr::call(GEN_HOST.print, vec![Expr::str(RESULT_BEGIN)], 0)),
                Stmt::expr(Expr::call(GEN_HOST.print, vec![call], 0)),
                Stmt::expr(Expr::call(GEN_HOST.print, vec![Expr::str(RESULT_END)], 0)),
            ],
        };
        body.push(Stmt::If {
            id: Id::NEW,
            cond: Expr::Binary {
                id: Id::NEW,
                op: vyrn_frontend::ast::BinOp::Eq,
                lhs: Box::new(Expr::var("@g", 0)),
                rhs: Box::new(Expr::str(f.name.clone())),
                line: 0,
            },
            then_block: Block {
                id: Id::NEW,
                stmts: takes_type_arg(f)
                    // Bound, not passed inline: the release of an argument
                    // temporary is refused.
                    .then(|| {
                        Stmt::let_(
                            "@typeArg",
                            Expr::call(vyrn_codegen::GEN_ENTRY_TYPE_ARG, vec![], 0),
                            0,
                        )
                    })
                    .into_iter()
                    .chain(hand_over)
                    .chain([Stmt::ret(Expr::int(0), 0)])
                    .collect(),
            },
            else_block: None,
            line: 0,
        });
    }
    // Unreachable: `run` checks the target first. Falling off the chain would
    // emit an empty module, so read past the end of `args()` and trap instead.
    body.push(Stmt::expr(Expr::call(
        GEN_HOST.print,
        vec![argv(1_000_000_000)],
        0,
    )));
    body.push(Stmt::ret(Expr::int(0), 0));

    p.functions
        .push(Function::synth("main", Vec::new(), Type::Int, body));
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
    p.host.gen = true;
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
        scrutinee: Box::new(Expr::call(GEN_HOST.parse, vec![e], 0)),
        arms: vec![
            MatchArm {
                pattern: Pattern::Variant("Some".into(), vec![Binder::synthetic("v")]),
                body: ArmBody::Expr(Expr::var("v", 0)),
            },
            MatchArm {
                pattern: Pattern::Variant("None".into(), Vec::new()),
                body: ArmBody::Expr(Expr::int(0)),
            },
        ],
        line: 0,
    }
}

/// Appends an entry point per reachable structured builtin, plus its decoders.
/// `None` declines the whole run.
fn reflect_entries(p: &mut Program) -> Option<()> {
    let reaches = |what: &str| {
        p.functions
            .iter()
            .any(|f| vyrn_frontend::checker::fn_calls(&f.body, &Default::default()).contains(what))
    };
    let named = |n: &str| Type::Named(n.to_string());
    let mut dec = Decoders::new(p);
    let mut entries: Vec<Function> = Vec::new();
    let str_param = |n: &str| Param::synth(n, Type::Str);
    // `fn <entry>(arg) -> T { @reflect(kind, arg); return <decode T>() }`.
    let mut entry =
        |name: String, params: Vec<Param>, ret: Type, kind: i64, arg: Expr, d: &mut Decoders| {
            let body = d.decode(&ret)?;
            entries.push(Function::synth(
                &name,
                params,
                ret,
                vec![
                    Stmt::expr(Expr::call(
                        vyrn_codegen::GEN_REFLECT,
                        vec![Expr::int(kind), arg],
                        0,
                    )),
                    Stmt::ret(body, 0),
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
            Expr::var("path", 0),
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
            Expr::str(""),
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
            Expr::var("src", 0),
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
                Expr::str(name),
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
            Type::Str => Expr::call(vyrn_codegen::GEN_NEXT_STR, vec![], 0),
            Type::Named(n) if n == "Code" => Expr::call(
                vyrn_codegen::GEN_CODE,
                vec![Expr::call(vyrn_codegen::GEN_NEXT_STR, vec![], 0)],
                0,
            ),
            Type::Int | Type::IntN { .. } => Expr::call(vyrn_codegen::GEN_NEXT_INT, vec![], 0),
            Type::Bool => Expr::Binary {
                id: Id::NEW,
                op: vyrn_frontend::ast::BinOp::Eq,
                lhs: Box::new(Expr::call(vyrn_codegen::GEN_NEXT_INT, vec![], 0)),
                rhs: Box::new(Expr::int(1)),
                line: 0,
            },
            _ => {
                let name = self.materialize(ty)?;
                Expr::call(&name, vec![], 0)
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
                        clause: None,
                        value: Expr::call(vyrn_codegen::GEN_NEXT_INT, vec![], 0),
                        line: 0,
                        col: 0,
                    },
                    Stmt::Let {
                        id: Id::NEW,
                        name: "xs".into(),
                        mutable: true,
                        ty: Some(ty.clone()),
                        clause: None,
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
                        clause: None,
                        value: Expr::int(0),
                        line: 0,
                        col: 0,
                    },
                    Stmt::While {
                        id: Id::NEW,
                        cond: Expr::Binary {
                            id: Id::NEW,
                            op: vyrn_frontend::ast::BinOp::Lt,
                            lhs: Box::new(Expr::var("i", 0)),
                            rhs: Box::new(Expr::var("n", 0)),
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
                                    value: Expr::call(
                                        GEN_HOST.push,
                                        vec![Expr::var("xs", 0), elem],
                                        0,
                                    ),
                                    line: 0,
                                },
                                Stmt::Assign {
                                    id: Id::NEW,
                                    name: "i".into(),
                                    value: Expr::Binary {
                                        id: Id::NEW,
                                        op: vyrn_frontend::ast::BinOp::Add,
                                        lhs: Box::new(Expr::var("i", 0)),
                                        rhs: Box::new(Expr::int(1)),
                                        line: 0,
                                    },
                                    line: 0,
                                },
                            ],
                        },
                        line: 0,
                    },
                    Stmt::ret(Expr::var("xs", 0), 0),
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
                            lhs: Box::new(Expr::call(vyrn_codegen::GEN_NEXT_INT, vec![], 0)),
                            rhs: Box::new(Expr::int(1)),
                            line: 0,
                        },
                        then_block: Block {
                            id: Id::NEW,
                            stmts: vec![Stmt::ret(Expr::call("Some", vec![some], 0), 0)],
                        },
                        else_block: None,
                        line: 0,
                    },
                    Stmt::ret(Expr::var("None", 0), 0),
                ]
            }
            // Fields in declaration order, which the host pushed: both sides
            // read `record_fields`.
            Type::Record(_) => {
                let Type::Named(rec) = ty else { return None };
                let fields = vyrn_frontend::types::record_fields(ty, &self.types)?.into_owned();
                let mut lit = Vec::new();
                for f in &fields {
                    lit.push((f.name.clone(), self.decode(&f.ty)?));
                }
                vec![Stmt::ret(
                    Expr::StructLit {
                        id: Id::NEW,
                        name: rec.clone(),
                        fields: lit,
                        line: 0,
                    },
                    0,
                )]
            }
            _ => return None,
        };
        self.fns
            .push(Function::synth(&name, Vec::new(), ty.clone(), body));
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
    vyrn_codegen::direct::compile_gen_host(program).map_err(|e| match e {
        GenError::Refused(ds) => EngineError::Refused(ds),
        GenError::Failed(e) => decline(&e),
        GenError::Unchecked => decline("the backend asked to check a checked program"),
    })
}

/// The `read` import's modes, shared with the emitter.
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
/// Not a read: `codeAt` asks for the bytes a read already served.
#[cfg(feature = "host")]
const MODE_CODE_AT: i32 = 5;

/// One unit of a structured host result. Encoder and decoder walk the same
/// static type, so the variant is not a tag the decoder consults: a `nextInt`
/// that finds a string means the two walks disagreed.
pub enum Atom {
    Int(i64),
    Str(Vec<u8>),
}

/// The host side of the `vyrn_gen` imports: the code arena, the atom stream,
/// the stash, and the declarations `contractOf` and `lex` read.
#[derive(Default)]
#[cfg(feature = "host")]
pub struct GenState {
    /// The last served read or `render`, waiting for `fetch` to copy it into
    /// guest memory. The host never allocates on the guest's side.
    stash: Vec<u8>,
    /// The code-quote arena. Per generation run, so a handle from one run is
    /// meaningless in the next.
    code: Arena,
    /// The structured result being handed over, and the decoder's position. A
    /// stream is fully consumed before the next `reflect`.
    atoms: Vec<Atom>,
    cursor: usize,
    /// The generator's declarations, so `contractOf` and `lex` are answered on
    /// this thread without the resolver.
    types: std::sync::Arc<std::collections::HashMap<String, vyrn_frontend::ast::TypeDecl>>,
    contracts: std::sync::Arc<Vec<vyrn_frontend::ast::ContractDecl>>,
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
            types: std::sync::Arc::new(type_map(p)),
            contracts: std::sync::Arc::new(p.contracts.clone()),
            ..GenState::default()
        }
    }
}

/// The `Code` values of one generation run, and the one home of what each
/// `vyrn_gen` code import does: the native linker and the playground's exports
/// decode the guest's arguments and call the method of the import's name. A
/// `Code` value is an index into `ropes`. An `Err` is a refusal that ends the
/// run.
#[derive(Default)]
pub struct Arena {
    ropes: Vec<Arc<Rope>>,
    /// A `Code` generator's result, set once by the wrapper's `result` call.
    result: Option<Arc<Rope>>,
}

impl Arena {
    /// The rope a handle names. Compiled code only holds handles the imports
    /// made, but the index is guest-supplied, so it is checked.
    fn rope(&self, h: i64) -> Result<&Arc<Rope>, String> {
        (self.ropes.get(h as usize)).ok_or_else(|| format!("bad code handle {h}"))
    }

    fn intern(&mut self, rope: Arc<Rope>) -> i64 {
        self.ropes.push(rope);
        self.ropes.len() as i64 - 1
    }

    fn leaf(&mut self, span: Span) -> i64 {
        self.intern(Arc::new(Rope::Leaf(vec![span])))
    }

    /// A quote's part, written by module `key` at `line:col`; the loader
    /// stamped the key after the parse.
    pub fn text(&mut self, text: String, key: String, line: i64, col: i64) -> i64 {
        let at = Pos {
            line: line as usize,
            col: col as usize,
        };
        let author = Author::File { key, at };
        let kind = Kind::Code;
        self.leaf(Span {
            text,
            author,
            shown: None,
            kind,
        })
    }

    pub fn raw(&mut self, text: String) -> i64 {
        self.leaf(Span::data(text))
    }

    pub fn raw_at(&mut self, text: String, path: String, line: i64, col: i64) -> i64 {
        let at = Pos {
            line: line as usize,
            col: col as usize,
        };
        let shown = Some(Shown { path, at });
        self.leaf(Span {
            shown,
            ..Span::data(text)
        })
    }

    pub fn name(&mut self, s: &str) -> Result<i64, String> {
        Ok(self.leaf(vyrn_frontend::gen::name_span(s)?))
    }

    pub fn diag(&mut self, sev: &str, anchor: &str, msg: &str) -> Result<i64, String> {
        Ok(self.leaf(vyrn_frontend::gen::diag_span(sev, anchor, msg)?))
    }

    /// `codeAt`: `bytes` are what the run read from `path`, which resolved to
    /// module `key`.
    #[cfg(feature = "host")]
    fn code_at(
        &mut self,
        key: &str,
        path: &str,
        bytes: &[u8],
        at: (i64, i64),
        text: &str,
    ) -> Result<i64, String> {
        let at = (at.0.max(0) as usize, at.1.max(0) as usize);
        Ok(self.leaf(vyrn_frontend::gen::copied_span(key, path, bytes, at, text)?))
    }

    pub fn at(&mut self, h: i64, path: &str, line: i64, col: i64) -> Result<i64, String> {
        let at = (line.max(0) as usize, col.max(0) as usize);
        let rope = vyrn_frontend::gen::shown_at(self.rope(h)?, path, at);
        Ok(self.intern(Arc::new(rope)))
    }

    /// Splices the value codegen sent as a tag, one word and, for a `String`,
    /// its text, re-indented to `indent`. Floats cross as bits because the
    /// shortest-roundtrip formatting belongs on this side.
    pub fn splice(
        &mut self,
        tag: i32,
        bits: i64,
        text: Option<String>,
        indent: &str,
    ) -> Result<i64, String> {
        let val = match (tag, text) {
            (vyrn_codegen::TAG_STR, Some(s)) => Spliced::Str(s),
            (vyrn_codegen::TAG_CODE, None) => Spliced::Code(self.rope(bits)?.clone()),
            (vyrn_codegen::TAG_BOOL, None) => Spliced::Bool(bits != 0),
            // Codegen sign-extends a signed integer of any width.
            (vyrn_codegen::TAG_INT, None) => Spliced::Int {
                v: bits,
                signed: true,
            },
            (vyrn_codegen::TAG_UINT, None) => Spliced::Int {
                v: bits,
                signed: false,
            },
            (vyrn_codegen::TAG_F64, None) => Spliced::F64(f64::from_bits(bits as u64)),
            (vyrn_codegen::TAG_F32, None) => Spliced::F32(f32::from_bits(bits as u32)),
            (other, _) => return Err(format!("bad splice tag {other}")),
        };
        Ok(self.intern(vyrn_frontend::gen::gen_code_splice(&val, indent)?))
    }

    pub fn concat(&mut self, a: i64, b: i64) -> Result<i64, String> {
        let cat = Rope::Cat(self.rope(a)?.clone(), self.rope(b)?.clone());
        Ok(self.intern(Arc::new(cat)))
    }

    pub fn result(&mut self, h: i64) -> Result<(), String> {
        let rope = self.rope(h)?.clone();
        match self.result.replace(rope) {
            Some(_) => Err("the generator handed over two results".into()),
            None => Ok(()),
        }
    }

    pub fn render(&self, h: i64) -> Result<String, String> {
        Ok(vyrn_frontend::gen::render_code(self.rope(h)?))
    }

    /// What a `Code` generator wrote: the value its wrapper handed to `result`.
    /// `None` for a `String` generator, whose result is framed in stdout.
    pub fn written(&mut self) -> Option<Written> {
        self.result.take().map(|code| Written::code(&code))
    }
}

/// A loop's accumulator is a chain of `Cat` nodes as deep as the loop ran, and
/// dropping its root first would recurse down the chain. Dropped newest first,
/// each node's children are still held by the arena, so no drop recurses.
impl Drop for Arena {
    fn drop(&mut self) {
        self.result = None;
        while self.ropes.pop().is_some() {}
    }
}

#[cfg(feature = "host")]
impl GenState {
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
        // A reflected type: its spelling, which the decoder turns into `Code`.
        Type::Named(n) if n == "Code" => match lit {
            Expr::Call { name, args, .. } if name == vyrn_codegen::GEN_CODE => {
                match args.as_slice() {
                    [Expr::Str(s, _)] => out.push(Atom::Str(s.clone().into_bytes())),
                    _ => return Err(wrong()),
                }
            }
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
            for f in decl.iter() {
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

/// The host thread's answer: bytes for a read or a listing, the reflection
/// literal for `moduleInterface`, or the key and bytes `codeAt` copies from.
#[cfg(feature = "host")]
enum Served {
    Bytes(i32, Vec<u8>),
    Copy(String, Vec<u8>),
    /// Encoded on the guest thread, so one [`encode`] call serves all three
    /// structured builtins.
    Lit(Box<Expr>),
}

/// A host-side refusal carried out as a trap: a read outside the declared
/// inputs, or a value with no splice rule. Both abort generation; neither may
/// reach the generator as a value.
#[derive(Debug)]
#[cfg(feature = "host")]
struct Denied(String);

#[cfg(feature = "host")]
impl std::fmt::Display for Denied {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.0)
    }
}

#[cfg(feature = "host")]
impl std::error::Error for Denied {}

/// An [`Arena`] refusal as the trap that carries it out.
#[cfg(feature = "host")]
fn denied<V>(r: Result<V, String>) -> wasmtime::Result<V> {
    r.map_err(|m| wasmtime::Error::new(Denied(m)))
}

/// Serves the `vyrn_gen` imports [`vyrn_codegen::direct::compile_gen_host`]
/// emits. Splicing, escaping and float formatting are the frontend's own
/// functions (`vyrn_frontend::gen`). [`wasi::instantiate`] links them for every
/// module.
#[cfg(feature = "host")]
fn link<X: 'static>(linker: &mut wasmtime::Linker<wasi::Guest<X>>) -> wasmtime::Result<()> {
    use wasmtime::{Caller, Error, Result};
    type T<X> = wasi::Guest<X>;
    // `read` resolves and stashes; `fetch` copies the stash into a buffer the
    // guest allocated, so the host never allocates in linear memory.
    linker.func_wrap(
        "vyrn_gen",
        "read",
        |mut caller: Caller<'_, T<X>>, path: i32, mode: i32| -> Result<i64> {
            let (data, host) = guest_mem(&mut caller)?;
            let streams = &mut host.gen;
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
                Ok(Ok(Served::Lit(_) | Served::Copy(..))) => {
                    Err(Error::msg("read answered with a literal"))
                }
                // A scoping violation traps.
                Ok(Err(msg)) => Err(Error::new(Denied(msg))),
                Err(_) => Err(Error::msg("generator host is gone")),
            }
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "fetch",
        |mut caller: Caller<'_, T<X>>, dest: i32| -> Result<()> {
            let (data, host) = guest_mem(&mut caller)?;
            let streams = &mut host.gen;
            let stash = std::mem::take(&mut streams.stash);
            let slot = data
                .get_mut(dest as usize..dest as usize + stash.len())
                .ok_or_else(|| Error::msg("bad fetch destination"))?;
            slot.copy_from_slice(&stash);
            Ok(())
        },
    )?;

    // The code-quote arena: `Code` is an i64 handle, and every operation on it
    // runs in [`Arena`]; a refusal traps.
    linker.func_wrap(
        "vyrn_gen",
        "text",
        |mut caller: Caller<'_, T<X>>, s: i32, key: i32, line: i64, col: i64| -> Result<i64> {
            let (data, host) = guest_mem(&mut caller)?;
            Ok((host.gen.code).text(cstr(data, s)?, cstr(data, key)?, line, col))
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "raw",
        |mut caller: Caller<'_, T<X>>, s: i32| -> Result<i64> {
            let (data, host) = guest_mem(&mut caller)?;
            Ok(host.gen.code.raw(cstr(data, s)?))
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "name",
        |mut caller: Caller<'_, T<X>>, s: i32| -> Result<i64> {
            let (data, host) = guest_mem(&mut caller)?;
            denied(host.gen.code.name(&cstr(data, s)?))
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "diag",
        |mut caller: Caller<'_, T<X>>, sev: i32, anchor: i32, msg: i32| -> Result<i64> {
            let (data, host) = guest_mem(&mut caller)?;
            let (sev, anchor, msg) = (cstr(data, sev)?, cstr(data, anchor)?, cstr(data, msg)?);
            denied(host.gen.code.diag(&sev, &anchor, &msg))
        },
    )?;
    // `codeAt` asks the host thread for the bytes the run read at `path`.
    linker.func_wrap(
        "vyrn_gen",
        "codeAt",
        |mut caller: Caller<'_, T<X>>, path: i32, line: i64, col: i64, s: i32| -> Result<i64> {
            let (data, host) = guest_mem(&mut caller)?;
            let streams = &mut host.gen;
            let (path, text) = (cstr(data, path)?, cstr(data, s)?);
            let caps = streams.caps.as_ref().ok_or_else(|| Error::msg("no host"))?;
            caps.req
                .send((path.clone(), MODE_CODE_AT))
                .map_err(|_| Error::msg("generator host is gone"))?;
            let (key, bytes) = match caps.resp.recv() {
                Ok(Ok(Served::Copy(key, bytes))) => (key, bytes),
                Ok(Ok(_)) => return Err(Error::msg("codeAt answered with no copy")),
                Ok(Err(msg)) => return Err(Error::new(Denied(msg))),
                Err(_) => return Err(Error::msg("generator host is gone")),
            };
            denied((streams.code).code_at(&key, &path, &bytes, (line, col), &text))
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "at",
        |mut caller: Caller<'_, T<X>>, h: i64, path: i32, line: i64, col: i64| -> Result<i64> {
            let (data, host) = guest_mem(&mut caller)?;
            denied(host.gen.code.at(h, &cstr(data, path)?, line, col))
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "rawAt",
        |mut caller: Caller<'_, T<X>>, s: i32, path: i32, line: i64, col: i64| -> Result<i64> {
            let (data, host) = guest_mem(&mut caller)?;
            Ok((host.gen.code).raw_at(cstr(data, s)?, cstr(data, path)?, line, col))
        },
    )?;
    // A `String` crosses as a pointer and anything else as `bits` and a null.
    linker.func_wrap(
        "vyrn_gen",
        "splice",
        |mut caller: Caller<'_, T<X>>, tag: i32, bits: i64, p: i32, indent: i32| -> Result<i64> {
            let (data, host) = guest_mem(&mut caller)?;
            let text = (p != 0).then(|| cstr(data, p)).transpose()?;
            let indent = cstr(data, indent)?;
            denied(host.gen.code.splice(tag, bits, text, &indent))
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "concat",
        |mut caller: Caller<'_, T<X>>, a: i64, b: i64| -> Result<i64> {
            denied(caller.data_mut().gen.code.concat(a, b))
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "result",
        |mut caller: Caller<'_, T<X>>, h: i64| -> Result<()> {
            denied(caller.data_mut().gen.code.result(h))
        },
    )?;
    linker.func_wrap(
        "vyrn_gen",
        "render",
        |mut caller: Caller<'_, T<X>>, h: i64| -> Result<i64> {
            let s = &mut caller.data_mut().gen;
            s.stash = denied(s.code.render(h))?.into_bytes();
            Ok(s.stash.len() as i64)
        },
    )?;

    // `reflect` computes a value of a known named type in the host (the
    // lexer, linker and contract tables are compiler machinery) and leaves it
    // as an atom stream the decoder pulls back.
    linker.func_wrap(
        "vyrn_gen",
        "reflect",
        |mut caller: Caller<'_, T<X>>, kind: i64, arg: i32| -> Result<()> {
            let (data, host) = guest_mem(&mut caller)?;
            let streams = &mut host.gen;
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
                        Ok(Ok(Served::Bytes(..) | Served::Copy(..))) => {
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
        |mut caller: Caller<'_, T<X>>| -> Result<i64> {
            match caller.data_mut().gen.next_atom()? {
                Atom::Int(n) => Ok(*n),
                Atom::Str(_) => Err(Error::msg("reflected value: expected an Int atom")),
            }
        },
    )?;
    // Length, then `fetch`, so the host never allocates in guest memory.
    linker.func_wrap(
        "vyrn_gen",
        "nextStr",
        |mut caller: Caller<'_, T<X>>| -> Result<i64> {
            let s = &mut caller.data_mut().gen;
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
fn guest_mem<'a, X>(
    caller: &'a mut wasmtime::Caller<'_, wasi::Guest<X>>,
) -> wasmtime::Result<(&'a mut [u8], &'a mut wasi::Guest<X>)> {
    let Some(mem) = caller.data().wasi.mem else {
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
fn load_artifact(dir: &std::path::Path, key: &str) -> Option<wasmtime::Module> {
    let bytes = std::fs::read(dir.join(key)).ok()?;
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
fn store_artifact(dir: &std::path::Path, key: &str, module: &wasmtime::Module) {
    let Ok(bytes) = module.serialize() else {
        return;
    };
    let mut tagged = vyrn_frontend::loader::gen_cache_tag(key, &bytes).into_bytes();
    debug_assert_eq!(tagged.len(), ARTIFACT_TAG_LEN);
    tagged.extend_from_slice(&bytes);
    let bytes = tagged;
    if std::fs::create_dir_all(dir).is_err() {
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

/// Compiles once and runs, returning what the generator wrote.
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
) -> Result<Written, EngineError> {
    let cached = module_cache().lock().ok().and_then(|c| c.get(key).cloned());
    let module = match cached {
        Some(m) => m,
        None => {
            let t = std::time::Instant::now();
            let from_disk = persist
                .then(|| load_artifact(&artifact_dir(), key))
                .flatten();
            let m = match from_disk {
                Some(m) => {
                    trace("deserialize", t.elapsed());
                    m
                }
                // Only a checked program compiles.
                None if !inputs.checked => return Err(EngineError::Unsupported),
                None => {
                    // A compile the typed judgment refused fails here, so no
                    // cache holds a module for it (`direct::compile_gen_host`).
                    let bytes = build()?;
                    trace("emit", t.elapsed());
                    let t = std::time::Instant::now();
                    let m = wasmtime::Module::new(wasm_engine(), &bytes)
                        .map_err(|e| EngineError::Failed(format!("wasm: {e}")))?;
                    trace("cranelift", t.elapsed());
                    if persist {
                        store_artifact(&artifact_dir(), key, &m);
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
    run_hosted(key, &module, argv, program, inputs, reads)
}

/// A program's type declarations by name, which reflection resolves against.
fn type_map(p: &Program) -> std::collections::HashMap<String, vyrn_frontend::ast::TypeDecl> {
    (p.type_decls.iter())
        .map(|t| (t.name.clone(), t.clone()))
        .collect()
}

/// What every run of one artifact shares: the module linked for a generator,
/// and the declarations its reflection reads.
#[cfg(feature = "host")]
struct Prepared {
    pre: wasmtime::InstancePre<wasi::Guest<()>>,
    types: std::sync::Arc<std::collections::HashMap<String, vyrn_frontend::ast::TypeDecl>>,
    contracts: std::sync::Arc<Vec<vyrn_frontend::ast::ContractDecl>>,
}

/// The [`Prepared`] of the artifact `key`, made once per process. The key names
/// the program, so `program` only fills a miss.
#[cfg(feature = "host")]
fn prepared(
    key: &str,
    module: &wasmtime::Module,
    program: &Program,
) -> Result<std::sync::Arc<Prepared>, EngineError> {
    type Cache = std::sync::Mutex<std::collections::HashMap<String, std::sync::Arc<Prepared>>>;
    static CACHE: std::sync::OnceLock<Cache> = std::sync::OnceLock::new();
    let cache = CACHE.get_or_init(Default::default);
    if let Some(p) = cache.lock().ok().and_then(|c| c.get(key).cloned()) {
        return Ok(p);
    }
    let policy = wasi::Policy::generator(0);
    let p = std::sync::Arc::new(Prepared {
        pre: wasi::linked(module, &policy, |_| Ok(())).map_err(EngineError::Failed)?,
        types: std::sync::Arc::new(type_map(program)),
        contracts: std::sync::Arc::new(program.contracts.clone()),
    });
    if let Ok(mut c) = cache.lock() {
        c.insert(key.to_string(), p.clone());
    }
    Ok(p)
}

/// A job for a guest thread.
#[cfg(feature = "host")]
type Job = Box<dyn FnOnce() + Send>;

#[cfg(feature = "host")]
thread_local! {
    /// This thread's idle guest thread, reused across runs: spawning one with
    /// its 16 MB stack costs as much as a small generator's run. A run takes it
    /// out, so a generator nested in another's capability request gets a thread
    /// of its own.
    static GUEST: std::cell::RefCell<Option<mpsc::Sender<Job>>> =
        const { std::cell::RefCell::new(None) };
}

/// A guest thread that runs each job it receives until its sender is dropped.
#[cfg(feature = "host")]
fn guest_thread() -> Result<mpsc::Sender<Job>, EngineError> {
    let (tx, rx) = mpsc::channel::<Job>();
    std::thread::Builder::new()
        // Compiled code runs on this stack; a deeply recursive generator would
        // overflow the default before wasmtime's own stack limit.
        .stack_size(16 * 1024 * 1024)
        .spawn(move || {
            while let Ok(job) = rx.recv() {
                job();
            }
        })
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    Ok(tx)
}

/// Runs the guest on its own thread and serves its capability requests from
/// this one, where the resolver lives. The loop ends when the guest's store,
/// and with it its `Sender`, is dropped.
#[cfg(feature = "host")]
fn run_hosted(
    key: &str,
    module: &wasmtime::Module,
    argv: &[String],
    program: &Program,
    inputs: &GenInputs<'_>,
    reads: &mut Vec<GenRead>,
) -> Result<Written, EngineError> {
    let fuel = wasm_fuel(inputs.fuel);
    let (req_tx, req_rx) = mpsc::channel::<(String, i32)>();
    let (resp_tx, resp_rx) = mpsc::channel::<Result<Served, String>>();
    let (done_tx, done_rx) = mpsc::channel::<Result<Written, EngineError>>();

    let prepared = prepared(key, module, program)?;
    let argv: Vec<String> = argv.to_vec();
    // Cloned because the store's data must be `'static`.
    let type_arg = inputs.type_arg.clone();
    let job: Job = Box::new(move || {
        let caps = Caps {
            req: req_tx,
            resp: resp_rx,
        };
        let _ = done_tx.send(run_wasm(&prepared, &argv, type_arg, fuel, caps));
    });
    let mut worker = match GUEST.with(|g| g.borrow_mut().take()) {
        Some(w) => w,
        None => guest_thread()?,
    };
    // A thread a panic ended refuses the job, which comes back for a new one.
    if let Err(mpsc::SendError(job)) = worker.send(job) {
        worker = guest_thread()?;
        let _ = worker.send(job);
    }

    // The loop ends when the run drops its store, and with it the sender.
    while let Ok((path, mode)) = req_rx.recv() {
        if resp_tx.send(serve(inputs, reads, &path, mode)).is_err() {
            break;
        }
    }
    match done_rx.recv() {
        Ok(r) => {
            GUEST.with(|g| *g.borrow_mut() = Some(worker));
            r
        }
        Err(_) => Err(EngineError::Failed("generator panicked".into())),
    }
}

/// Instantiates the module under [`wasi::Policy::generator`] and returns what
/// the generator wrote: a `String` framed on stdout, or a `Code` handed over
/// through the `result` import.
///
/// Embedded rather than spawned: the `wasmtime` CLI's launch measured ~106 ms.
#[cfg(feature = "host")]
fn run_wasm(
    prepared: &Prepared,
    argv: &[String],
    type_arg: Option<Expr>,
    fuel: u64,
    caps: Caps,
) -> Result<Written, EngineError> {
    let gen = GenState {
        caps: Some(caps),
        types: prepared.types.clone(),
        contracts: prepared.contracts.clone(),
        type_arg,
        ..GenState::default()
    };
    // argv[0] is the program name, which `args()` skips.
    let argv: Vec<String> = std::iter::once("gen".to_string())
        .chain(argv.iter().cloned())
        .collect();
    let policy = wasi::Policy::generator(fuel);
    let (mut store, inst) =
        wasi::start(&prepared.pre, &policy, &argv, gen, ()).map_err(EngineError::Failed)?;
    let result = match inst.get_typed_func::<(), ()>(&mut store, "_start") {
        Ok(start) => start.call(&mut store, ()),
        Err(e) => return Err(EngineError::Failed(format!("_start: {e}"))),
    };
    if std::env::var("VYRN_GENWASM_TRACE").is_ok() {
        // The fuel a real generator spends, which sizes `wasm_fuel`.
        let spent = fuel - store.get_fuel().unwrap_or(0);
        eprintln!("genwasm fuel: {spent}");
    }
    let mut guest = store.into_data();
    match wasi::exit_code(result) {
        Ok(0) => {}
        // The guest failed on its own terms. The prefix comes off because
        // generation takes the bare message and the loader adds context.
        Ok(code) => {
            let err = String::from_utf8_lossy(guest.wasi.stderr.as_deref().unwrap_or_default());
            return Err(EngineError::Failed(
                match vyrn_frontend::trap::split(&err).1 {
                    Some(msg) => msg.to_string(),
                    None => format!("generator exited with {code}"),
                },
            ));
        }
        // Out of fuel is reworded as the step-budget message.
        Err(e) if e.downcast_ref::<wasmtime::Trap>() == Some(&wasmtime::Trap::OutOfFuel) => {
            return Err(EngineError::Failed(
                "generator exceeded its step budget".into(),
            ))
        }
        // The message comes from the payload, not `e`: wasmtime wraps a host
        // error in a guest backtrace.
        Err(e) => {
            return Err(EngineError::Failed(match e.downcast_ref::<Denied>() {
                Some(Denied(msg)) => msg.clone(),
                None => format!("generator trapped: {e}"),
            }))
        }
    }
    // The last reflected value has no following `reflect` to check it.
    guest
        .gen
        .drained()
        .map_err(|e| EngineError::Failed(e.to_string()))?;
    // Compile-time prints go to the compiler's stdout, never into the source.
    // A `Code` result came through the `result` import, so all of stdout is
    // prints.
    let stdout = guest.wasi.stdout.unwrap_or_default();
    let (prints, written) = match guest.gen.code.written() {
        Some(written) => (stdout, written),
        None => {
            let (prints, source) = unframe_result(&stdout).map_err(decline)?;
            let source = String::from_utf8(source)
                .map_err(|_| EngineError::Failed("generator emitted invalid UTF-8".into()))?;
            let spans = None;
            (prints, Written { source, spans })
        }
    };
    if !prints.is_empty() {
        use std::io::Write;
        let mut out = std::io::stdout();
        let _ = out.write_all(&prints);
        let _ = out.flush();
    }
    Ok(written)
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

    #[test]
    fn a_long_concatenation_chain_drops_without_recursing() {
        let mut arena = Arena::default();
        let leaf = arena.leaf(Span::data("x".into()));
        let mut acc = leaf;
        for _ in 0..1_000_000 {
            let cat = Rope::Cat(
                arena.rope(acc).unwrap().clone(),
                arena.rope(leaf).unwrap().clone(),
            );
            acc = arena.intern(Arc::new(cat));
        }
        arena.result = arena.rope(acc).ok().cloned();
        drop(arena);
    }

    /// `load_artifact` maps its bytes in as native code, so this is its
    /// `unsafe` block's soundness argument.
    #[test]
    fn an_artifact_this_compiler_did_not_write_is_never_deserialized() {
        let tmp = std::env::temp_dir().join(format!("vyrn-artifact-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&tmp);

        let module = wasmtime::Module::new(wasm_engine(), [0, b'a', b's', b'm', 1, 0, 0, 0])
            .expect("an empty module compiles");
        let key = "artifacttest";
        store_artifact(&tmp, key, &module);
        let path = tmp.join(key);
        assert!(path.is_file(), "the control: it was stored");
        assert!(
            load_artifact(&tmp, key).is_some(),
            "the control: it reads back"
        );
        let stored = std::fs::read(&path).unwrap();

        // The module altered under an otherwise honest tag.
        let mut swapped = stored.clone();
        let last = swapped.len() - 1;
        swapped[last] ^= 0xff;
        std::fs::write(&path, &swapped).unwrap();
        assert!(
            load_artifact(&tmp, key).is_none(),
            "the tag covers the module"
        );

        // The same artifact filed under another key.
        std::fs::write(tmp.join("otherkey"), &stored).unwrap();
        assert!(
            load_artifact(&tmp, "otherkey").is_none(),
            "the tag covers the key"
        );

        // An untagged file.
        std::fs::write(&path, &stored[ARTIFACT_TAG_LEN..]).unwrap();
        assert!(
            load_artifact(&tmp, key).is_none(),
            "untagged is not trusted"
        );

        std::fs::write(&path, b"short").unwrap();
        assert!(load_artifact(&tmp, key).is_none());

        let _ = std::fs::remove_dir_all(&tmp);
    }
}
