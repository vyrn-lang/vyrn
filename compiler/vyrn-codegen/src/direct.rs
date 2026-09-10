//! Lowering Vyrn straight to wasm, with no LLVM in between (RFC-0077 M2).
//!
//! M2a was a vertical slice — one example, scalars only. M2b is the width the
//! ladder said was worth having: the **aggregate ABI** (records and every other
//! `{ .. }` shape, through the shadow stack) and **`String`**, which between them
//! were 62 of the 78 examples' first blocker.
//!
//! Everything not yet lowered is [`unsupported`]: a named construct and a source
//! line, never a fallback to the LLVM path. A silent fallback would make the
//! ladder report a number that is not about this backend at all, and the ladder
//! is the milestone's real deliverable.
//!
//! # The four constraints this is built around
//!
//! **Structured control flow, straight from the AST** (M2's pre-flight). wasm has
//! no `goto`, and this needs no relooper because `if`/`while` map onto
//! `if`/`block`+`loop` and `break`/`continue` onto `br <depth>`. What that costs
//! is bookkeeping: every construct that opens a wasm block pushes one onto
//! [`Fn_::depth`], because a `return` is a `br` past all of them.
//!
//! **A body must not emit `return`** (M1). It would jump past the shadow-stack
//! epilogue `wasm::Module::add` emits and leak the frame for the rest of the
//! program. So a body is wrapped in one `block`, and `return` is a `br` to it.
//!
//! **Scalars in wasm locals, aggregates in frame slots** (M0). An aggregate is
//! never a wasm value: on the operand stack it is always the `i32` address of a
//! slot. That one decision is the entire ABI — a parameter is an address the
//! callee copies out of, a return is a hidden leading address the callee writes
//! through, and a field access is an offset.
//!
//! **Destination-first at joins** (M0). wasm has no aggregate values, so an
//! aggregate `if`-expression has nothing to leave on the stack: the slot is
//! allocated BEFORE the branch and each arm copies into it. [`Fn_::join`] is that
//! rule, and it is indifferent to how many arms there are — which is what M2a's
//! pre-flight said mattered, 46 of the 149 joins having four to seven edges.

use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use vyrn_frontend::ast::*;
use vyrn_frontend::own::DropKind;
/// RFC-0101 M4's exit vocabulary, shared with `vyrn-lower` and the other two
/// engines so the placement and the walks are compared without a translation.
use vyrn_frontend::own::Exit as ExitKind;
use vyrn_frontend::types as ftypes;
use vyrn_frontend::types::INT32;
/// RFC-0125 §2.3's own vocabulary: the statements the emitter walks, what each
/// one computes, and the values it computes it from. `Body` is spelled out at
/// each use, because this file's own `Body` is the AST's.
use vyrn_lower::core::{Callee, Lit, Op, Rhs, St, Val};

use crate::layout::{self, Layout};
use crate::llt_of;
use crate::wasm::{self, BlockType, Frame, Instruction, MemArg, Module, ValType, HEAP_BASE};
use crate::{ArmRef, BodyRef};

/// What the direct backend cannot lower yet: the construct, and where.
///
/// One shape for every gap, because the ladder groups its blocker list by the
/// text after the colon — a message that varies by site would report the same
/// gap as several.
fn unsupported<T>(what: &str, line: usize) -> Result<T, String> {
    Err(gap(what, line))
}

fn gap(what: &str, line: usize) -> String {
    format!("direct backend: no lowering for {what} at line {line}")
}

/// A size the emitter can name but cannot express. Sibling of [`gap`]: that one
/// is a shape with no lowering, this one is a shape whose lowering does not fit
/// in the `u32` every offset, `malloc` argument and copy length here is.
fn too_big(what: &str, bytes: u64, line: usize) -> String {
    format!(
        "direct backend: {what} needs {bytes} bytes at line {line}, past the {} one value may \
         occupy; a fixed array this big belongs on the heap as `Array<T>`",
        i32::MAX
    )
}

/// The `wasi_snapshot_preview1` calls a directly-emitted module makes.
///
/// **All of them are DECLARED, and then swept.** An import has to be declared
/// before the first body (M1: one index space, imports at the bottom), and
/// nothing knows which builtins a program reaches until the bodies are walked —
/// so the alternative was a pre-scan over the AST, i.e. a second traversal that
/// has to agree with lowering about what it needs. That is the failure mode
/// `llt_of` (M2b) and `predicate_binds` (M2d) exist to prevent, and M2e refused a
/// standalone instantiation walker for the same reason.
///
/// M2p is the other end of it: [`wasm::Module::sweep`] drops what no export
/// reaches AFTER the bodies exist, so the fifteen here cost a program only what
/// it calls (`fib.wasm` imports two). Which is why `path_rename` could be added at
/// all — M2o refused it as a thirteenth UNCONDITIONAL import, renumbering every
/// module in the corpus — and `fd_sync` and `fd_readdir` join on the same terms.
///
/// The set is implemented twice over: wasmtime provides all of preview1, and
/// `web/wasi-min.js` implements exactly these for the browser — with RFC-0014's
/// graceful degradation (no argv, EOF on stdin, no preopens, every `path_open`
/// NOENT), which is what a page's `readFile` is supposed to be.
#[derive(Clone, Copy, Default)]
struct Wasi {
    fd_write: u32,
    fd_read: u32,
    fd_close: u32,
    proc_exit: u32,
    path_open: u32,
    path_rename: u32,
    fd_sync: u32,
    fd_prestat_get: u32,
    args_sizes_get: u32,
    args_get: u32,
    environ_sizes_get: u32,
    environ_get: u32,
    clock_time_get: u32,
    random_get: u32,
    /// `listDir` (RFC-0125 §3 M5): the directory's entries, in the host's
    /// order, which `list_dir` sorts.
    fd_readdir: u32,
}

fn wasi_imports(m: &mut Module) -> Wasi {
    use ValType::{I32, I64};
    let mut im = |name: &str, params: &[ValType], results: &[ValType]| {
        m.import("wasi_snapshot_preview1", name, params, results)
    };
    Wasi {
        fd_write: im("fd_write", &[I32, I32, I32, I32], &[I32]),
        fd_read: im("fd_read", &[I32, I32, I32, I32], &[I32]),
        fd_close: im("fd_close", &[I32], &[I32]),
        proc_exit: im("proc_exit", &[I32], &[]),
        path_open: im(
            "path_open",
            &[I32, I32, I32, I32, I32, I64, I64, I32, I32],
            &[I32],
        ),
        path_rename: im("path_rename", &[I32, I32, I32, I32, I32, I32], &[I32]),
        fd_sync: im("fd_sync", &[I32], &[I32]),
        fd_prestat_get: im("fd_prestat_get", &[I32, I32], &[I32]),
        args_sizes_get: im("args_sizes_get", &[I32, I32], &[I32]),
        args_get: im("args_get", &[I32, I32], &[I32]),
        environ_sizes_get: im("environ_sizes_get", &[I32, I32], &[I32]),
        environ_get: im("environ_get", &[I32, I32], &[I32]),
        clock_time_get: im("clock_time_get", &[I32, I64, I32], &[I32]),
        random_get: im("random_get", &[I32, I32], &[I32]),
        fd_readdir: im("fd_readdir", &[I32, I32, I32, I64, I32], &[I32]),
    }
}

/// The `vyrn_gen` imports a GENERATOR module makes (RFC-0076 M7).
///
/// A generator compiled by this backend runs inside the compiler's own wasmtime,
/// not under a WASI host, and everything it reaches for is compiler machinery: the
/// loader's resolver (which serves unsaved editor buffers, so a guest that opened
/// files itself would read different bytes than the interpreter), the RFC-0054
/// piece arena, the real lexer, the real linker. All of it stays in the host, and
/// the guest holds handles and pulls atoms — which is what makes the splice rules,
/// the escaping and the float formatting single-sourced rather than agreed upon.
///
/// This is exactly the surface the textual emitter used to add under
/// `-DVYRN_GEN_HOST`, declared from [`crate::CODE_IMPORTS`] — the same list, in the
/// same LLVM spelling, so no signature on this boundary is written twice: `read`
/// mediates and stashes, `fetch` copies a stash into a buffer the GUEST allocated
/// (the host must not allocate inside guest memory), the five `Code` operations work
/// on arena handles, and `reflect`/`nextInt`/`nextStr` are M3b's one transfer.
#[derive(Clone, Copy)]
struct Gen {
    read: u32,
    fetch: u32,
    text: u32,
    splice: u32,
    raw_at: u32,
    concat: u32,
    render: u32,
    reflect: u32,
    next_int: u32,
    next_str: u32,
}

fn gen_imports(m: &mut Module) -> Gen {
    let mut at: HashMap<&str, u32> = HashMap::new();
    for (decl, name) in crate::CODE_IMPORTS {
        let (params, results) = wasm::declare_sig(decl);
        at.insert(name, m.import("vyrn_gen", name, &params, &results));
    }
    // Every field named, so a name that stops being in the list is a panic here
    // rather than an import nothing satisfies.
    let g = |n: &str| at[n];
    Gen {
        read: g("read"),
        fetch: g("fetch"),
        text: g("text"),
        splice: g("splice"),
        raw_at: g("rawAt"),
        concat: g("concat"),
        render: g("render"),
        reflect: g("reflect"),
        next_int: g("nextInt"),
        next_str: g("nextStr"),
    }
}

/// One RFC-0012 `extern fn` — a host function the module imports from the fixed
/// `vyrn` namespace, or one of RFC-0043's three host-boundary names.
#[derive(Clone)]
struct Ext {
    /// The import's function index, or `None` for `hostNowMillis` and friends,
    /// which the emitted runtime serves out of WASI itself (M2j) and so import
    /// nothing.
    index: Option<u32>,
    params: Vec<Type>,
    ret: Type,
}

/// The wasm signature one `extern fn` crosses as.
///
/// Both halves come from the textual emitter's own [`crate::extern_abi_ll`] mapped
/// through [`wasm::abi`], for the reason `SHIM_IMPORTS` is a list of names: an ABI
/// spelled a second time here is a misread argument rather than a link error. The
/// one shape-level fact is `String`, which crosses as a `(ptr, len)` **pair** —
/// the asymmetry `web/README.md` documents against an *export*, where a `String`
/// parameter is a single pointer because the JS caller can allocate inside the
/// module.
fn extern_abi_sig(f: &Function) -> (Vec<ValType>, Vec<ValType>) {
    let mut params = Vec::new();
    for p in &f.params {
        if matches!(p.ty, Type::Str) {
            params.push(ValType::I32);
            params.push(ValType::I64);
        } else {
            params.extend(wasm::abi(crate::extern_abi_ll(&p.ty)));
        }
    }
    (
        params,
        wasm::abi(crate::extern_abi_ll(&f.ret))
            .into_iter()
            .collect(),
    )
}

/// Compile a whole program to a self-contained `wasm32-wasi` module.
///
/// One file: it defines its own memory, its own heap and its own runtime, and
/// imports nothing but the `wasi_snapshot_preview1` calls it makes and the
/// RFC-0012 `extern`s it declares. That is what makes `vyrn build --target wasm`
/// need no clang, no wasi sysroot and no builtins archive (RFC-0077 M5).
///
/// M2i built a second shape that imported memory and the C runtime from RFC-0076's
/// pre-compiled shim, selected by `VYRN_WASM_BACKEND=direct-shim`. It is gone with
/// the flag, and the argument for deleting it is M2i's and M2j's own: the split
/// makes a module LARGER (the runtime the shim would supply is already emitted and
/// parity-proven), it needs a C toolchain and so could never be the default, and
/// after M2j served RFC-0043's clock out of WASI directly it passed nothing this
/// shape does not. The boundary audit it was said to protect —
/// `vyrn-codegen/tests/shim_link.rs` — builds its own guest module out of
/// `wasm::Module` and never went through here.
pub fn compile(program: &Program) -> Result<Vec<u8>, String> {
    crate::set_gen_host(false);
    compile_inner(program)
}

/// The same module [`compile`] emits, as WAT (`vyrn emit-wat`).
///
/// It is exactly `emit`'s textual IR for the other compiled backend: a form a
/// test can read. Until this existed, a property that no program output can show
/// — one bounds check for four lanes, a header moved and not copied — could be
/// pinned on the native backend by grepping `emit-ir` and could not be pinned
/// here at all, so half the compiled surface was gated on behaviour only.
///
/// Printing is not part of `compile`: `vyrn build` writes bytes, and a text form
/// nothing but a reader asks for should not be on the path that produces them.
pub fn wat(program: &Program) -> Result<String, String> {
    let bytes = compile(program)?;
    wasmprinter::print_bytes(&bytes).map_err(|e| e.to_string())
}

/// Compile `program` to run as a GENERATOR under RFC-0076's engine: the same
/// traversal, plus the `vyrn_gen` host imports and the two lowerings that only
/// make sense with them (`listDir`, and `Code` as an opaque `i64` handle).
///
/// This is RFC-0076 M7, and it closes RFC-0077's own opening complaint. The
/// generation engine reached the wasm target through `emit_gen_host` and clang, so
/// `find_clang() == None` made it DECLINE — a `.vyx` keystroke was 54 ms or 250 ms
/// depending on whether someone had installed a C toolchain, which is the shape
/// RFC-0077 exists to remove and which M5 removed for `vyrn build` only.
/// The functions with no run-time lowering because they reach a generator.
///
/// RFC-0021: a `gen fn` runs at generation time and has no runtime lowering at
/// all, so neither has an ordinary function that calls one — nor one that calls
/// THAT. [`compile`] skips the closure for the reason it skips an
/// unspecializable shell: failing the whole build over a function nothing calls
/// is the wrong answer. A call to a skipped name refuses at its own call site,
/// naming it. `std/vyx-hints`'s `checkOf` is the shape: a helper its own test
/// blocks call, in a module every `vyxHints` program imports.
///
/// Public because the driver asks the same question about a DOOR (RFC-0125 §3
/// M5): a `test` body in this closure is a body that has to be compiled as
/// generation, and the answer must be the one this backend gives, not a second
/// walk that agrees with it today.
pub fn gen_reach(program: &Program) -> std::collections::HashSet<String> {
    let mut reach: std::collections::HashSet<String> = program
        .functions
        .iter()
        .filter(|f| f.is_gen)
        .map(|f| f.name.clone())
        .collect();
    loop {
        let before = reach.len();
        for f in &program.functions {
            if f.is_extern || reach.contains(&f.name) {
                continue;
            }
            if vyrn_frontend::checker::fn_calls(&f.body)
                .iter()
                .any(|c| reach.contains(c))
            {
                reach.insert(f.name.clone());
            }
        }
        if reach.len() == before {
            break;
        }
    }
    reach
}

pub fn compile_gen_host(program: &Program) -> Result<Vec<u8>, String> {
    // The flag is thread-local because `llt_of` reads it (a `Code` is an `i64`
    // handle only here) and `llt_of` is shared with the textual emitter. Cleared
    // on the way out so a later `compile` on this thread cannot inherit it.
    crate::set_gen_host(true);
    let r = compile_inner(program);
    crate::set_gen_host(false);
    r
}

fn compile_inner(program: &Program) -> Result<Vec<u8>, String> {
    // RFC-0125 §3 M5: this emitter reads every expression's type off the
    // checker's record rather than deriving one. `vyrn build` has already
    // asked for it, through the lowering; a host that compiles a program the
    // lowering never walked — a generator, a probe, a test — asks here, and a
    // program the core already holds costs the key comparison and nothing
    // else. The guard lives as long as the emit, so a record made here is not
    // left behind for whatever `Program` next lands at this address.
    let _decided = vyrn_lower::core::decide(program);
    let mut m = Module::new();
    // Imports first — they share the function index space with definitions, so
    // `wasm::Module` panics if one arrives late.
    let wasi = wasi_imports(&mut m);
    let gen = crate::gen_host().then(|| gen_imports(&mut m));
    // RFC-0012 M1: every `extern fn` is one import from the fixed `vyrn`
    // namespace, which `web/wasi-min.js` fills from the page's own hooks. Declared
    // from the DECLARATIONS, before any body — not a pre-scan of the kind M2e and
    // M2j refused, because there is nothing here for lowering to disagree with: an
    // `extern fn` *is* the import, one for one, and `Module::sweep` drops the ones
    // a program never calls. On native the same declaration becomes a C trap stub
    // instead; only a *call* crosses a boundary.
    let mut externs: HashMap<String, Ext> = HashMap::new();
    for f in program.functions.iter().filter(|f| f.is_extern) {
        // RFC-0043's three host-boundary names are not host imports on any target
        // — see the note at their call site.
        let index = if crate::host_boundary_extern(&f.name).is_some() {
            None
        } else {
            let (params, results) = extern_abi_sig(f);
            Some(m.import("vyrn", &f.name, &params, &results))
        };
        externs.insert(
            f.name.clone(),
            Ext {
                index,
                params: f.params.iter().map(|p| p.ty.clone()).collect(),
                ret: f.ret.clone(),
            },
        );
    }

    // PLAN-0125-runtime §6 step 1: the runtime functions written in Vyrn are
    // reserved before the hand-emitted runtime, which calls them.
    let mut vyrn_rt = VyrnRt::reserve(&mut m);
    let rt = runtime(&mut m, &wasi, &vyrn_rt);

    let types: HashMap<String, TypeDecl> = program
        .type_decls
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect();
    let mut variants: HashMap<String, Vec<(String, u64, Vec<Type>)>> = HashMap::new();
    for d in &program.type_decls {
        if let Some(vs) = vyrn_frontend::types::declared_variants(&d.base) {
            for (i, v) in vs.iter().enumerate() {
                variants.entry(v.name.clone()).or_default().push((
                    d.name.clone(),
                    i as u64,
                    v.payload.clone(),
                ));
            }
        }
    }
    // Three kinds of function define nothing, and are skipped exactly as the
    // textual driver skips them (`lib.rs`, step 1). Lowering an unspecializable
    // shell would fail the whole build over a function nothing calls. The
    // fourth kind is [`gen_reach`]'s.
    let gen_reach = gen_reach(program);
    let mut generics: HashMap<String, &Function> = HashMap::new();
    let mut higher_order: HashMap<String, &Function> = HashMap::new();
    let mut user: Vec<&Function> = Vec::new();
    for f in &program.functions {
        // An `extern` is an import (declared above); a `gen fn` (RFC-0021) runs
        // only in the compiler's own interpreter and may use builtins with no
        // lowering at all.
        if f.is_extern || f.is_gen {
            continue;
        }
        // An ENTRY POINT is never dropped — `main`, an export a host knocks on,
        // a lifted test or serve door. It stays, and lowering it refuses at the
        // call the closure was computed from, naming that call rather than
        // reporting a program with no `main`. Everything else in `gen_reach` is
        // a function no run-time entry can reach.
        let entry = f.name == "main" || f.exported || f.is_export_extern;
        if gen_reach.contains(&f.name) && !entry {
            continue;
        }
        // PLAN-0125-runtime §3.2: a `std/mem` declaration has no body this
        // emitter reads. `Fn_::mem_prim` lowers each call to one instruction.
        if f.name.starts_with(vyrn_frontend::loader::MEM_PREFIX) {
            continue;
        }
        // RFC-0023: a function taking a `fn`-typed parameter exists only as
        // higher-order specializations, one per set of resolved targets. The shell
        // has no first-order definition to emit — a `fn` parameter is not a value
        // in the lowered code at all.
        if f.params.iter().any(|p| matches!(p.ty, Type::Fn(..))) {
            higher_order.insert(f.name.clone(), f);
            continue;
        }
        if !f.type_params.is_empty() {
            generics.insert(f.name.clone(), f);
            continue;
        }
        user.push(f);
    }
    let protocol_methods: HashMap<String, String> = program
        .protocols
        .iter()
        .flat_map(|p| {
            // Projection requirements (RFC-0123 M2) dispatch by receiver type
            // through the places table, never as mangled methods.
            p.methods
                .iter()
                .filter(|m| m.result_cap.is_none())
                .map(|m| (m.name.clone(), p.name.clone()))
        })
        .collect();

    let ownership = vyrn_frontend::own::analyze(program);
    // RFC-0114 §25's instrument, and never on the GENERATOR path (RFC-0076
    // M7): a generator module runs inside the compiler and its exit code is a
    // protocol between the host and the module, so a residue report there
    // would fail the build instead of measuring the program.
    let audited = gen.is_none() && vyrn_frontend::loader::audit_build();
    let mut cx = Cx {
        types,
        lambdas: vyrn_frontend::ast::lambdas(program),
        impls: program.impls.clone(),
        sigs: HashMap::new(),
        rt,
        gen,
        variants,
        generics,
        higher_order,
        protocol_methods,
        subst: HashMap::new(),
        mono: RefCell::new(Mono::default()),
        fnvals: RefCell::new(Vec::new()),
        fnval_copy: 0,
        dispatch: RefCell::new(Dispatch::default()),
        shapes: RefCell::new(Shapes::default()),
        globals: HashMap::new(),
        gappend: HashMap::new(),
        externs,
        // Every call-argument temporary this program releases at the call
        // (`rfcs/census-call-arguments.md`), taken before `ownership` is moved
        // from.
        plan: ownership.plan.clone(),
        // The core's own answers, folded once by the placer inside
        // `own::analyze` above (RFC-0125 §3 M3).
        facts: vyrn_lower::core::facts(),
        releases: ownership.releases,
        // RFC-0093 M2, flattened across functions: the key is the `let`'s node
        // address, which is unique in the program.
        owned: ownership.proto,
        log_level: program.log_level,
        log_sink: program.log_sink.clone(),
        // Reserved only for a file sink, so every console-sink module — which is
        // every example — is byte-for-byte what it was.
        log_fd: matches!(program.log_sink, LogSink::File(_)).then(|| m.reserve(4, 4)),
        audit: audited,
    };

    // Every function the module will define, indexed before any body exists, so a
    // call can name a callee that has not been emitted. Recursion and forward
    // references both need this; there is no fixup pass.
    //
    // A RESERVATION rather than an index computed from the emission order. Through
    // M2l the order WAS the numbering, and keeping the two in step was a discipline
    // — `Mono::insts` append-only, `done` only forward, FIFO by construction, and
    // an assertion at every drain. RFC-0037's dispatchers cannot satisfy any such
    // discipline (their bodies are complete only after the last body is walked), so
    // the discipline is replaced by the mechanism it was standing in for: an index
    // is handed out by the encoder and the body is filled whenever it exists. An
    // out-of-turn body is now impossible rather than asserted against.
    for f in user.iter() {
        let s = cx.signature(f)?;
        let (wp, wr) = cx.wasm_sig(&s, f.line)?;
        let index = match vyrn_rt.take(&f.name, &wp, &wr, f.line)? {
            Some(reserved) => reserved,
            None => m.reserve_func(&wp, &wr),
        };
        cx.sigs.insert(f.name.clone(), Sig { index, ..s });
    }
    vyrn_rt.check()?;

    // Module state (RFC-0013), before any body: a top-level `let` is one fixed
    // address per binding, reserved zeroed, and every read and write anywhere in
    // the program resolves to it through `Fn_::lookup`'s fallback. The addresses
    // have to exist before the first body is walked, since a body may read one —
    // and after the signatures, because an unannotated initializer may be a call
    // whose type only a signature knows.
    for g in &program.globals {
        let ty = match &g.ty {
            Some(t) => t.clone(),
            None => top_level(&cx).peek(&g.init, g.line)?,
        };
        let l = layout::of_ll(&cx.ll(&ty)).map_err(|e| format!("direct backend: {e}"))?;
        if cx.repr(&ty, g.line)? == Repr::Unit {
            return unsupported("module state of Unit", g.line);
        }
        cx.globals.insert(
            g.name.clone(),
            (Place::Static(m.reserve(l.size, l.align)), ty),
        );
    }

    // One ownership word per module-state accumulator, in static memory for the
    // reason the local's word sits in the frame: the helper writes it back, and
    // wasm has no way to pass anything by reference. Reserved zeroed, and the
    // initializer sets it to what the global's own initializer made true.
    //
    // This loop is why `global_append_candidates` gives back an ORDERED set: a
    // reservation is an address, and it moves every reservation after it.
    let gaccs: Vec<String> = crate::global_append_candidates(program)
        .into_iter()
        .filter(|n| {
            cx.globals
                .get(n)
                .is_some_and(|(_, ty)| cx.resolve(ty) == Type::Str)
        })
        .collect();
    for name in gaccs {
        let at = m.reserve(4, 4);
        cx.gappend.insert(name, at);
    }

    // The initializer's index, reserved like every other so nothing depends on
    // where in the sequence it lands.
    let has_globals = !program.globals.is_empty();
    let init_index = m.reserve_func(&[], &[]);
    // RFC-0114 §25: the teardown that drops every module-state binding after
    // `main`, so what the instrument reports is the program's residue and not
    // the module state it never had a place to release. Reserved only in an
    // audited build, which is the whole of what keeps an ordinary module's
    // indices where they were.
    let teardown_index = (cx.audit && has_globals).then(|| m.reserve_func(&[], &[]));
    // The derived `fn`-value copy (Phase 10b), reserved for a dispatcher's
    // reason: its switch covers every construction in the module, so its body
    // cannot be written until the last body is walked, while a copy site in the
    // middle of that walk has to be able to call it.
    cx.fnval_copy = m.reserve_func(&[ValType::I64, ValType::I32], &[ValType::I32]);

    for f in &user {
        let sig = cx.sigs[&f.name].clone();
        crate::observe::note_inst(crate::observe::Site::Wasm, &f.name, &[]);
        lower_fn(&mut m, f, &sig, &cx, HashMap::new())?;
    }

    // The initializers, in DECLARATION order — which the loader has already made
    // linker order, dependencies first, so `statemod`'s diamond initializes its
    // shared store before either arm reads it. One function, called once from
    // `_start`, so nothing runs per call and nothing runs twice. Filled even when
    // the program has no module state, because a reservation nobody fills is not a
    // module — and an empty body is two bytes.
    let init = lower_globals_init(&mut m, program, &cx)?;
    m.fill(init_index, init);
    // Before the drain, like every other body: a global of a DECLARED generic
    // release reaches the teardown and nowhere else, and its instance has to
    // be on a worklist the drain below still reads. That instance is what
    // `vyrn-lower`'s `<teardown>` root queues.
    if let Some(ti) = teardown_index {
        let t = lower_globals_teardown(&mut m, program, &cx)?;
        m.fill(ti, t);
    }

    // Drain what the bodies discovered, and then the dispatchers the drain
    // discovered, until neither has anything left. One body may discover more of
    // either — a generic instance calling a generic, an RFC-0023 instance calling
    // either, a lifted lambda doing any of it, any of them calling a stored `fn`
    // value — so this reads both lists afresh every turn rather than iterating a
    // snapshot. That is what "the worklists feed each other" is here: appending to
    // the list being read.
    //
    // A frame past the limit waits for the drain to end. In a polymorphic
    // recursion the frames double every instance, so the frame limit trips turns
    // before the instantiation limit does — and the instantiation refusal is the
    // one `vyrn check` and the textual backend give for that program (audit
    // A5.2, RFC-0125 §3 M5). One program, one sentence: the drain goes on, and the
    // frame refusal is returned only when no instantiation refusal came.
    let mut deferred: Option<String> = None;
    loop {
        let p = {
            let mono = cx.mono.borrow();
            mono.insts.get(mono.done).cloned()
        };
        if let Some(p) = p {
            // Audit A5.2: the same cap the textual backend takes, at this
            // backend's own worklist. Both drain until nothing is left, and
            // polymorphic recursion leaves something every turn.
            crate::check_inst_depth(&p.f.name, p.subst.values(), p.f.line, &cx.types)?;
            // RFC-0101 M2's shadow. A `Key::Lambda` is not a function of the
            // program and has no name to record; the other two kinds are one
            // named callee at one list of type arguments, which is exactly the
            // identity `vyrn-lower`'s worklist keys on.
            match &p.key {
                Key::Generic(n, args) | Key::Ho(n, args, _) => {
                    crate::observe::note_inst(crate::observe::Site::Wasm, n, args)
                }
                Key::Lambda(..) => {}
            }
            cx.subst = p.subst.clone();
            // RFC-0101 M6's second phase: the one body in this backend that
            // was still a clone is the lifted lambda's, so the answers given
            // while walking one were off-program by construction. The third
            // phase gave the borrow back ([`Cx::lambdas`]), so the mark is on
            // what is left rather than on the KIND: a shell is a clone, and a
            // lambda whose literal the program holds is not.
            let was = crate::observe::set_ctx(match p.body {
                Body::Shell => "lambda",
                _ => "",
            });
            let body = lower_body(&mut m, &p.f, p.body, &p.sig, &cx, p.binds.clone());
            crate::observe::set_ctx(was);
            cx.subst = HashMap::new();
            cx.mono.borrow_mut().done += 1;
            match body {
                Ok(body) => {
                    m.fill(p.sig.index, body);
                    if std::env::var_os("VYRN_WASM_NAMES").is_some() {
                        m.name(p.sig.index, &p.f.name);
                    }
                }
                Err(e) if e.contains(crate::FRAME_LIMIT_NEEDLE) => {
                    deferred.get_or_insert(e);
                }
                Err(e) => return Err(e),
            }
            continue;
        }
        // The release and the copy of a type, one body each (RFC-0125 §3 M3,
        // the two shape walks). They are drained before the dispatchers and
        // after the instances for the same reason the instances are drained
        // this way: a shape body reaches a declared `release`, which is an
        // ordinary call and may be a generic one, so writing it can put an
        // instance on the list this loop has just read to the end. The
        // substitution is EMPTY here — every shape's type was substituted at
        // the site that asked for it.
        let sh = {
            let s = cx.shapes.borrow();
            s.todo.get(s.done).cloned()
        };
        if let Some((index, (rel, ty, holes), line)) = sh {
            cx.subst = HashMap::new();
            let body = lower_shape(&mut m, &cx, rel, &ty, &holes, line)?;
            m.fill(index, body);
            cx.shapes.borrow_mut().done += 1;
            continue;
        }
        // A dispatcher's body is the one thing that cannot be written when its
        // index is handed out: it switches over every construction of its
        // signature ANYWHERE in the module, so it is only complete once the last
        // body is walked. Hence `Module::reserve_func` — see [`Fn_::dispatcher`].
        let d = {
            let disp = cx.dispatch.borrow();
            disp.sigs.get(disp.done).cloned()
        };
        let Some((sig_ty, dsig)) = d else { break };
        let body = lower_dispatcher(&mut m, &cx, &sig_ty, &dsig)?;
        m.fill(dsig.index, body);
        cx.dispatch.borrow_mut().done += 1;
    }
    if let Some(e) = deferred {
        return Err(e);
    }

    // RFC-0114 §26's finish check stood here. It counted the rows the PLAN
    // placed and this emission never queried, and the emission queries no
    // plan row any more (RFC-0125 §3 M3, the emitter-reads-the-core-alone
    // slice). What answers the same question about the core's rows is
    // measurement: the residue ratchet and the memory suite.

    // The registry is closed now, so the derived copy can be written.
    let fncopy = lower_fnval_copy(&cx)?;
    m.fill(cx.fnval_copy, fncopy);

    // `_start`: WASI's entry point. The exit code is `main & 255`, the same
    // truncation `vyrn_entry` does natively — `vyrn run` and the native binary
    // both give the OS one byte, so wasm has to as well or parity is off by 256.
    let main = cx
        .sigs
        .get("main")
        .ok_or_else(|| "direct backend: program has no `main`".to_string())?;
    if main.ret != Repr::Scalar(ValType::I64) {
        return unsupported("a `main` that does not return Int64", 0);
    }
    let main = main.index;
    // RFC-0008's `file(..)` sink: a descriptor opened ONCE and held, which is what
    // the native backend's `fopen`/`@__vyrn_log_file`/`fclose` around `vyrn_main`
    // is — and the shape `writeFile` cannot express, since it opens, truncates and
    // closes per call. `path_open` with CREAT|TRUNC is `fopen(path, "w")`, and M2j
    // already put it in this module.
    //
    // A failure leaves -1 in the slot and every write is swallowed by `write_all`'s
    // errno test, which is the interpreter's behaviour (`if let Some(f) = ..`) and
    // RFC-0008's Q6 leaning. It is also what a browser gets: no preopens, so
    // `open_at` returns -1 for every path and a page's file sink degrades to
    // silence rather than trapping.
    let log_open = cx.log_fd.map(|at| {
        let LogSink::File(path) = &cx.log_sink else {
            unreachable!("log_fd implies a file sink")
        };
        (at, cx.rt.intern(&mut m, path), cx.rt.open_at)
    });
    // RFC-0114 §25's instrument, when the build is audited: the four wordings
    // interned here and handed to `auditInit`, the way `boolStr` and
    // `strFromBytes` are handed theirs. The two indices are ordinary module
    // functions — nothing but this reaches them, so an unaudited build sweeps
    // them away with the data.
    let audit = if cx.audit {
        let words = [
            cx.rt.intern(&mut m, "free audit: "),
            cx.rt.intern(&mut m, " block(s), "),
            cx.rt.intern(&mut m, " bytes, never freed\n"),
            cx.rt.intern(&mut m, "free audit: double or foreign free\n"),
        ];
        let at = |n: &str| {
            cx.sigs.get(n).map(|s| s.index).ok_or_else(|| {
                format!("direct backend: VYRN_LEAK_CHECK is set and `std/runtime` has no `{n}`")
            })
        };
        Some((at("runtime$auditInit")?, at("runtime$auditExit")?, words))
    } else {
        None
    };
    let start = m.func(&[], &[], &[], 0, |b| {
        // First of all, so no block is handed out before the instrument can
        // mark it: `auditInit` allocates its own state and only then arms.
        if let Some((init, _, words)) = audit {
            for w in words {
                b.ins(&Instruction::I32Const(w as i32));
            }
            b.ins(&Instruction::Call(init));
        }
        // Before the initializers, because a top-level `let` may log — the same
        // order `vyrn_entry` uses.
        if let Some((at, path, open_at)) = log_open {
            b.ins(&Instruction::I32Const(at as i32))
                .ins(&Instruction::I32Const(path as i32))
                .ins(&Instruction::I32Const(OFLAGS_CREAT_TRUNC))
                .ins(&Instruction::I64Const(RIGHT_FD_WRITE))
                .ins(&Instruction::Call(open_at))
                .ins(&Instruction::I32Store(word()));
        }
        if has_globals {
            b.ins(&Instruction::Call(init_index));
        }
        b.ins(&Instruction::Call(main));
        // Whatever `print` left in the standard output buffer, on the way out.
        // A zero-length write to a descriptor that is not fd 1 flushes and
        // writes nothing, which is exactly the two things needed here — and it
        // is why both trap paths need no flush of their own: they write their
        // message to fd 2 first.
        b.ins(&Instruction::I32Const(2))
            .ins(&Instruction::I32Const(0))
            .ins(&Instruction::I32Const(0))
            .ins(&Instruction::Call(cx.rt.write_all))
            .ins(&Instruction::Drop);
        if let Some((at, ..)) = log_open {
            b.ins(&Instruction::I32Const(at as i32))
                .ins(&Instruction::I32Load(word()))
                .ins(&Instruction::Call(wasi.fd_close))
                .ins(&Instruction::Drop);
        }
        // After the flush, so a leaking program's own output arrives before the
        // report, and before the exit code, because the report REPLACES it with
        // 135. The old instrument sat in exactly this place — after
        // `vyrn_main` returned, on the normal path only, so a program that
        // trapped reported nothing.
        if let Some(ti) = teardown_index {
            b.ins(&Instruction::Call(ti));
        }
        if let Some((_, exit, _)) = audit {
            b.ins(&Instruction::Call(exit));
        }
        b.ins(&Instruction::I64Const(255))
            .ins(&Instruction::I64And)
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::Call(wasi.proc_exit));
    });
    m.export("_start", start);
    // RFC-0012's `export extern fn`, under its own name — what `wasm-export-name`
    // tells wasm-ld on the LLVM path, and what `--export-all` was doing for it by
    // accident. Named here for two reasons: the direct backend had no export but
    // `_start` at all, so a JS caller had nothing to call; and an export is what
    // makes a function a sweep ROOT, so the two facts are one fact.
    for f in &user {
        if f.is_export_extern {
            m.export(&f.name, cx.sigs[&f.name].index);
        }
    }
    // A `String` ARGUMENT to an exported function is a pointer into this module's
    // memory, so the JS caller has to allocate inside it before it can call in —
    // which is the whole reason an export's String ABI differs from an import's
    // (one `ptr`, not a `(ptr, len)` pair). On the LLVM path this is
    // `-Wl,--export=__vyrn_malloc`, under exactly the same condition. The emitted
    // `malloc` IS the boundary's signature — `unsigned long long`, so `i64`, the
    // BigInt `wasi-min.js` passes — so it is exported as itself. It used to go
    // out through an `i32.wrap` wrapper, which was the one place a JS caller
    // could ask for 5 GiB and be handed a pointer to 1.
    //
    // `__vyrn_free` goes out with it, and it is not a convenience. RFC-0012 M?
    // settled that the CALLER owns a `String` argument, and across this boundary
    // the caller is JS — so before M6 the only allocator symbol a module exported
    // had no counterpart and `wasi-min.js` could not do anything but forget the
    // pointer. 20000 keystrokes into `domdemo` cost 18 MB that way.
    //
    // A String RETURN asks for the same pair (RFC-0089 M3b). Rule 3 makes the
    // result the caller's, and across this boundary the caller is JS, so the
    // wrapper frees it after decoding — which it cannot do without the symbol.
    // `__vyrn_malloc` goes out with `__vyrn_free` rather than alone, because the
    // free list is the allocator's and half of it is not a boundary.
    if user.iter().any(|f| {
        f.is_export_extern
            && (f.params.iter().any(|p| matches!(p.ty, Type::Str)) || matches!(f.ret, Type::Str))
    }) {
        m.export("__vyrn_malloc", cx.rt.malloc);
        m.export("__vyrn_free", cx.rt.free);
    }
    // Keep only what those exports reach (M2p). Everything above emits eagerly —
    // 39 runtime helpers, 12 WASI imports, every function of every linked module —
    // because nothing knows what a program reaches until its bodies are walked.
    // This is where that is known.
    //
    // And the data with them: every literal of every linked module was interned
    // on its way past, and `runtime` below interned the UTF-8 table, the six I/O
    // wordings and the trap rows before a body existed. `Module::sweep_pool`
    // asks the same question of those bytes (RFC-0125 §3 M4).
    m.sweep();
    abi_section(&mut m, &user, program);
    m.finish()
}

/// How a value crosses the JS boundary, as the DECLARATION says rather than as
/// the wasm slot happens to look. The two are not the same fact: `String`,
/// `Bool`, `Int32` and `UInt32` all lower to a wasm `i32`.
///
/// `"opaque"` is unreachable — [`extern_abi_type_ok`] in the checker closes the
/// domain to exactly these — but it is written down rather than asserted, so a
/// later type that widens the domain arrives at the shim as a loud refusal
/// instead of a silent mis-encoding.
///
/// [`extern_abi_type_ok`]: vyrn_frontend::checker
fn abi_kind(ty: &Type) -> &'static str {
    match ty {
        Type::Str => "string",
        Type::Bool => "bool",
        Type::Unit => "unit",
        Type::Float => "f64",
        Type::Float32 => "f32",
        Type::Int => "i64",
        Type::IntN { bits, signed } => match (bits, signed) {
            (64, true) => "i64",
            (64, false) => "u64",
            (_, true) => "i32",
            (_, false) => "u32",
        },
        _ => "opaque",
    }
}

/// The `vyrn:exports` custom section (RFC-0012 M3): the declared signature of
/// every function that crosses the JS boundary, in both directions.
///
/// **Why the module carries this.** The wasm ABI is lossy in the one direction a
/// host needs: `String`, `Bool`, `Int32` and `UInt32` all arrive as `i32`, and a
/// `String` import arrives as two slots that look exactly like an `(Int32,
/// Int64)` pair. `web/wasi-min.js` used to recover the difference by reading the
/// module's own type/import/function/export sections and guessing from the
/// shape — an `i32` followed by an `i64` IS a String — with the collision written
/// down as a caveat in `web/README.md`, and export ARGUMENTS decided by the JS
/// runtime type of whatever the caller happened to pass. Passing `42` to
/// `greet(name: String)` handed the module 42 as a pointer.
///
/// M3 wrote down half of it: the `String`/`Bool` RESULTS. This writes the rest.
/// The compiler knows every one of these types exactly, and a consumer inferring
/// them from instruction shapes is guessing at something nobody has to guess at.
///
/// **Payload** (version 2):
///
/// ```text
/// u8            version = 2
/// uleb          export count
///   per entry:  name:str  ret:kind  uleb param count  param:kind …
/// uleb          import count  (the `vyrn.*` namespace)
///   per entry:  name:str  ret:kind  uleb param count  param:kind …
/// ```
///
/// `str` is a uleb length then UTF-8 bytes; `kind` is a `str` from the closed
/// set [`abi_kind`] returns. A module with nothing on the boundary carries no
/// section at all.
///
/// The section lists every declaration, including one `Module::sweep` dropped.
/// That is deliberate and costs nothing: the shim learns WHICH functions exist
/// from `WebAssembly.Module.imports` and `instance.exports`, which is what the
/// platform already answers, and reads this only for the types it cannot.
fn abi_section(m: &mut wasm::Module, user: &[&Function], program: &Program) {
    fn leb(out: &mut Vec<u8>, mut n: u32) {
        loop {
            let b = (n & 0x7f) as u8;
            n >>= 7;
            if n == 0 {
                out.push(b);
                return;
            }
            out.push(b | 0x80);
        }
    }
    fn name(out: &mut Vec<u8>, s: &str) {
        leb(out, s.len() as u32);
        out.extend_from_slice(s.as_bytes());
    }
    fn sig(out: &mut Vec<u8>, f: &Function) {
        name(out, &f.name);
        name(out, abi_kind(&f.ret));
        leb(out, f.params.len() as u32);
        for p in &f.params {
            name(out, abi_kind(&p.ty));
        }
    }
    let exports: Vec<&&Function> = user.iter().filter(|f| f.is_export_extern).collect();
    // RFC-0043's three host-boundary names are lowered in place on every target,
    // so they are not `vyrn.*` imports and the page never supplies them.
    let imports: Vec<&Function> = program
        .functions
        .iter()
        .filter(|f| f.is_extern && crate::host_boundary_extern(&f.name).is_none())
        .collect();
    if exports.is_empty() && imports.is_empty() {
        return;
    }
    let mut payload = vec![2u8];
    leb(&mut payload, exports.len() as u32);
    for f in exports {
        sig(&mut payload, f);
    }
    leb(&mut payload, imports.len() as u32);
    for f in imports {
        sig(&mut payload, f);
    }
    m.custom("vyrn:exports", payload);
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

/// Which key family a `Map` runs on (RFC-0117): `String` pointers, `Int64`
/// values, or packed user keys of a fixed stride (M2).
#[derive(Clone, Copy, PartialEq, Eq)]
enum MapKey {
    Str,
    I64,
    Pack(u32),
}

impl MapKey {
    /// The `(kind, klen)` pair `std/runtime`'s one map body takes
    /// (PLAN-0125-runtime §6 step 5): 0 for a String column, 1 for an `Int64`
    /// column, 2 for a packed user key of `klen` bytes. Kind 3, a byte window
    /// against a String column, is `tallyBytes`'s alone and has no `MapKey`.
    fn kind(self) -> (i32, i32) {
        match self {
            MapKey::Str => (0, 0),
            MapKey::I64 => (1, 8),
            MapKey::Pack(n) => (2, n as i32),
        }
    }

    /// The bytes one entry of the key column takes.
    fn stride(self) -> i32 {
        match self {
            MapKey::Str => 4,
            MapKey::I64 => 8,
            MapKey::Pack(n) => n as i32,
        }
    }
}

/// How a value of some Vyrn type travels: nothing, a wasm value, or the address
/// of a shadow-stack slot.
#[derive(Clone, Debug, PartialEq, Eq)]
enum Repr {
    Unit,
    Scalar(ValType),
    Agg(Layout),
}

impl Repr {
    /// The wasm type this crosses a call boundary as — an aggregate crosses as
    /// its address, which is the whole convention in one line.
    fn val(&self) -> Option<ValType> {
        match self {
            Repr::Unit => None,
            Repr::Scalar(v) => Some(*v),
            Repr::Agg(_) => Some(ValType::I32),
        }
    }

    fn agg(&self) -> Option<&Layout> {
        match self {
            Repr::Agg(l) => Some(l),
            _ => None,
        }
    }
}

/// What a call to a function needs to know about it.
#[derive(Clone, PartialEq)]
struct Sig {
    index: u32,
    params: Vec<Type>,
    /// Which parameters are `modify` (RFC-0004 §1) and therefore cross as the
    /// address of the caller's binding rather than as a value. A caller cannot
    /// read this off the parameter's TYPE — a `modify Counter` and a `read
    /// Counter` are the same type and different ABIs — which is why it travels in
    /// the signature, exactly as `param_caps` does in the textual backend.
    modify: Vec<bool>,
    ret: Repr,
    ret_ty: Type,
}

/// What identifies a body discovered during emission, so that a second site
/// reaching the same one reuses its function index instead of emitting a twin.
///
/// Deliberately the type arguments and targets THEMSELVES rather than a mangled
/// name: `mangle_name` is the textual backend's symbol and it is not injective
/// (every record mangles as `Rec`), so two distinct specializations can produce
/// one symbol and the textual driver's `emitted.insert(sym)` silently skips the
/// second. A wasm function has no symbol at all, so there is nothing to gain by
/// narrowing the key to a string that could collide on the thing being
/// distinguished.
#[derive(Clone, PartialEq)]
enum Key {
    /// A generic instantiation (M2e): the callee and its type arguments.
    Generic(String, Vec<Type>),
    /// An RFC-0023 specialization: the callee, its type arguments, and the target
    /// each `fn`-typed parameter resolved to. Two call sites passing the same
    /// lambda to the same generic instance share one instance; passing a
    /// different lambda is a different function, which is the whole of what
    /// "monomorphized away" means.
    Ho(String, Vec<Type>, Vec<FnTarget>),
    /// A lifted lambda: the literal's own node address, the concrete shape it was
    /// typed at (captures, parameters, return), and the substitution its body is
    /// under. The address alone is not enough — one literal inside a generic body
    /// lifts a distinct copy per instantiation, and the shape need not differ if
    /// the type parameter is used only in a statement.
    Lambda(usize, Vec<Type>, Vec<(String, Type)>),
}

/// What a queued body's statements ARE.
///
/// Every one of them is the program's own AST, which is the whole point: a
/// backend walking a copy asks about nodes no recorded type can reach
/// (RFC-0101 §1.2). [`Body::Shell`] is the one exception left and it is not
/// reachable from any program — see its note.
#[derive(Clone, Copy)]
enum Body<'a> {
    /// A block the program holds: a generic instance's callee, an RFC-0023
    /// specialization's, or a `|x| { .. }` literal's own body.
    Block(&'a Block),
    /// A `|x| e` literal's expression. The block form's `return e` is a
    /// STATEMENT, and writing one here would mean owning a copy of `e` — which
    /// is exactly the clone this milestone deleted — so the value and the branch
    /// are emitted directly (see [`Fn_::lambda_value`]).
    Value(&'a Expr),
    /// The shell's own statements: a lifted lambda whose literal is not one of
    /// the program's nodes, so [`Cx::lambdas`] cannot hand back a borrow of it
    /// and the synthesized block is all there is to walk. A literal inside a
    /// leaked desugar is the only way to get one, and the corpus reaches none.
    Shell,
}

/// One body discovered while another was being emitted, with the function index
/// it was promised.
#[derive(Clone)]
struct Pending<'a> {
    key: Key,
    /// The SHELL: the name, the line and the signature this body is lowered
    /// under. It carries no statements for a [`Key::Generic`] or a [`Key::Ho`] —
    /// those walk [`Pending::body`], which is the program's own — and it carries
    /// the synthesized block for a [`Key::Lambda`]. An `Rc` rather than a clone
    /// per drain turn, because [`Key::Lambda`] keys on a node address inside it
    /// and a fresh deep clone every turn would move the addresses of anything
    /// nested.
    f: Rc<Function>,
    /// The statements to walk, borrowed from the checked program (RFC-0101 §6.1:
    /// the form borrows, and so does this).
    ///
    /// This used to be a deep clone of the callee, made once per instantiation,
    /// and the clone was the whole reason RFC-0101 M3's delete half could not
    /// land: a backend walking a copy of the AST asks about nodes the program
    /// does not have, so no recorded type can reach them. 9,505 answers were
    /// about such nodes.
    body: Body<'a>,
    sig: Sig,
    /// The monomorphization the body is lowered under; empty for a lifted lambda
    /// outside any generic.
    subst: HashMap<String, Type>,
    /// RFC-0023: the target each `fn`-typed parameter is bound to, by name.
    binds: HashMap<String, FnBinding>,
}

/// The specialization worklist (RFC-0077 M2e, widened by M2m).
///
/// This RFC said "monomorphization runs before any instruction is emitted". It
/// does not, in either backend: a specialization is *discovered* at a call site,
/// so the only thing that can feed a worklist is a body being lowered. There is
/// no pre-pass to consume, and writing one would be a second traversal that has
/// to agree with lowering about what gets instantiated — a new source of truth,
/// free to drift, which is the failure mode `llt_of` and `predicate_binds` exist
/// to prevent.
///
/// So this is fed from inside [`Fn_`] and drained by [`compile`], exactly as
/// `Gen::instantiations` is by the textual driver. One thing is stricter here: a
/// wasm call names a function INDEX, not a symbol, so an index is handed out at
/// discovery and the bodies must be added in that same order. `insts` is
/// append-only and `done` only moves forward — FIFO by construction, because a
/// queue that could reorder would silently renumber every call in the module.
///
/// The textual driver runs **two** worklists that feed each other (a generic body
/// may take `fn` parameters; a specialized instance may call generics) plus a
/// dedup set for lifted lambdas, and drains each with `pop()`. It is right to:
/// nothing there depends on the order, because every reference is a name. Here
/// the order IS the numbering, so there is **one** queue holding all three kinds
/// — which makes "they feed each other" a property of appending to the list you
/// are reading rather than an alternation to get right.
#[derive(Default)]
struct Mono<'a> {
    insts: Vec<Pending<'a>>,
    done: usize,
}

/// One source a stored `fn` value can come from (RFC-0037): a lifted lambda, a
/// named function, or a `fn`-typed parameter inside a specialization.
///
/// Defunctionalization: a stored value is `{ i64 tag, i64 payload }` where the tag
/// indexes THIS list globally and the payload is a heap block of captures (0 when
/// there are none). Every call goes through the signature's dispatcher, which
/// switches on the tag and makes a DIRECT call — so no function pointer exists
/// anywhere, and M2a's measurement of zero indirect calls holds for stored values
/// as much as for `fn` parameters.
#[derive(Clone)]
struct FnVal {
    /// The normalized signature this variant belongs to. Two spellings of one
    /// signature must land here as one type, or a dispatcher misses a variant —
    /// which is [`crate::normalize_fn_sig`]'s whole job.
    sig: Type,
    target: FnTarget,
}

/// The dispatchers, and how far the driver has got through them.
///
/// Separate from [`Mono`] because a dispatcher is not discovered from a body being
/// walked, it is discovered from a SIGNATURE being called through — and its body
/// cannot be written until every body has been walked, since a construction
/// anywhere in the module adds a variant to it.
#[derive(Default)]
struct Dispatch {
    sigs: Vec<(Type, Sig)>,
    done: usize,
}

/// The release and the copy of one type, each as a function of its own —
/// RFC-0125 §2.3's "`drop` to a call" (§3 M3, the two shape walks).
///
/// The shape of a release is the TYPE: which fields, which elements, which
/// payload slot under which tag. Nothing above this file can state it, because
/// every step of it is a byte offset and a byte offset is [`crate::layout`]'s.
/// What §2.3 asks for is the other half — that a drop SITE emit a call — and
/// that is what this holds: one body per type, written once, called wherever a
/// release or a copy of that type is reached.
#[derive(Default)]
struct Shapes {
    /// `(a release rather than a copy, the substituted type, the take holes)`
    /// → the function's index. A linear scan because [`Type`] is `Eq` and not
    /// `Hash`, and a module has tens of these.
    known: Vec<(ShapeKey, u32)>,
    /// The bodies still to write, and how many of them are written. A body may
    /// reach a type nothing has released yet, so the driver reads this list
    /// afresh every turn exactly as it reads [`Mono`]'s.
    todo: Vec<(u32, ShapeKey, usize)>,
    done: usize,
}

/// What identifies one shape body. The holes are RFC-0093 M2's: a `consume`
/// took a place, so a release that walks it would free what has an owner
/// already — which makes a walk around them a DIFFERENT function.
type ShapeKey = (bool, Type, Vec<String>);

/// What a `fn`-typed argument resolved to (RFC-0023): the function a call through
/// that parameter goes to **directly**, and how many of its leading parameters are
/// captures the outer call site supplies.
///
/// Captures first, then the `fn` type's own parameters — a lifted lambda's shape,
/// and the shape a bare named function already has with zero captures. So calling
/// a target is [`Fn_::emit_call`] with the captures prepended to the argument
/// list, and no second call path exists to disagree with the first about the
/// aggregate convention, `modify`, or coercion.
///
/// This is why the backend needs no function table. RFC-0037 defunctionalized
/// closures, so nothing here is ever an address: a target is a compile-time
/// function index, and M2a's measurement of zero indirect calls survives.
#[derive(Clone, PartialEq)]
struct FnTarget {
    sig: Sig,
    ncaps: usize,
}

/// A `fn`-typed parameter as seen from inside a specialization: its target, and
/// the names of this instance's own leading capture parameters to forward.
///
/// The captures are values, fixed at the OUTER call site — RFC-0023's
/// capture-timing lock. An instance that re-read them would be a closure over a
/// mutable environment, which is a different language.
#[derive(Clone)]
struct FnBinding {
    target: FnTarget,
    cap_srcs: Vec<String>,
}

struct Cx<'a> {
    types: HashMap<String, TypeDecl>,
    /// The program's OWN type declarations, for the one thing the map above
    /// cannot answer: a `where` predicate's node ADDRESS (RFC-0101 M6's third
    /// phase).
    ///
    /// Every lambda literal the PROGRAM holds, by node address (RFC-0101 M6's
    /// third phase).
    ///
    /// [`Fn_::lift_lambda`] is handed the literal by a walk that has erased its
    /// lifetime, so it could not park the literal's own body on the worklist and
    /// cloned it instead — 532 of the corpus's off-program answers, because a
    /// backend walking a copy asks about nodes no recorded type can reach. This
    /// gives the borrow back: one walk over the program indexes every literal,
    /// and an address that hits IS the program's node, since the program outlives
    /// every walk and nothing else can be living at one of its addresses.
    ///
    /// A miss is a literal inside a tree the program does not hold — a leaked
    /// desugar — and the caller keeps its clone for that.
    lambdas: HashMap<usize, &'a LambdaBody>,
    /// Every `impl` block, for `place` projection lookup (RFC-0091 M2). A
    /// projection is not a function, so `sigs` cannot answer for it.
    impls: Vec<vyrn_frontend::ast::ImplBlock>,
    sigs: HashMap<String, Sig>,
    rt: Rt,
    /// The `vyrn_gen` host imports, on the generator path only (RFC-0076 M7).
    /// `None` is an ordinary `vyrn build --target wasm`, where every one of those
    /// builtins is refused by name exactly as it was.
    gen: Option<Gen>,
    /// Variant name → every enum that declares it, with the variant's tag and
    /// payload types. A name may belong to two enums, which is why the
    /// expectation decides and an ambiguous one is a gap rather than a guess.
    variants: HashMap<String, Vec<(String, u64, Vec<Type>)>>,
    /// Generic functions by name. They have no index and no body of their own —
    /// only specializations do — so a call to one is a discovery.
    generics: HashMap<String, &'a Function>,
    /// Functions with a `fn`-typed parameter (RFC-0023). Like a generic they have
    /// no index and no body of their own — only specializations do — so a call to
    /// one is a discovery, and the shell is skipped exactly as the textual driver
    /// skips it.
    higher_order: HashMap<String, &'a Function>,
    /// Protocol method name → its protocol (RFC-0002 §5). A bounded generic is
    /// what protocols are for, so `x.show()` inside one has to resolve.
    protocol_methods: HashMap<String, String>,
    /// The monomorphization whose body is being lowered; empty for an ordinary
    /// function.
    subst: HashMap<String, Type>,
    mono: RefCell<Mono<'a>>,
    /// RFC-0037's variant registry, module-global so a tag means the same thing in
    /// every body that builds one.
    fnvals: RefCell<Vec<FnVal>>,
    /// The module's one derived copy over that registry (Phase 10b, census §16):
    /// `(tag, block) -> block`. Reserved up front and filled after the drain
    /// loop, for the reason a dispatcher is — a variant's capture layout is only
    /// complete once the last body is walked.
    fnval_copy: u32,
    dispatch: RefCell<Dispatch>,
    /// One release and one copy per type, and the worklist of the bodies still
    /// to write — see [`Shapes`].
    shapes: RefCell<Shapes>,
    /// Module state (RFC-0013): name → its fixed address and declared type. Every
    /// body sees all of them, which is the textual backend's `globals` fallback in
    /// [`Gen::lookup`] — the checker already forbids an initializer reading a
    /// global declared after it, so there is nothing for a partial view to catch.
    globals: HashMap<String, (Place, Type)>,
    /// Module-state `String` accumulators (census P1): name → the fixed address of
    /// its one ownership word. Present only for a global that
    /// [`crate::global_append_candidates`] cleared, so `g = g + …` grows the
    /// buffer in place instead of building a new one and dropping the old on the
    /// floor. The local twin of this map is [`Fn_::str_append`], keyed by wasm
    /// local; a global has no local, so it needs its own word in static memory.
    gappend: HashMap<String, u32>,
    /// `extern fn` declarations by name (RFC-0012): the `vyrn` import's index and
    /// the signature the ABI is read off. There is no body to lower — a call is the
    /// only thing that crosses — and what one returns is the declaration's business
    /// rather than this file's, which is also how RFC-0043's host boundary is
    /// reached by name.
    externs: HashMap<String, Ext>,
    /// Per function: every release step PLACED — at the exit that runs it, in
    /// the order it runs (RFC-0101 M4). One order for three engines, read at
    /// the exit instead of derived from a frame stack.
    releases: HashMap<String, Vec<vyrn_frontend::own::Release>>,
    /// The per-node release decisions (RFC-0114 §26) — the same artifact the
    /// textual backend reads, so the two cannot disagree about a site.
    plan: vyrn_frontend::own::ReleasePlan,
    /// What the CORE says about the releases this emitter emits. The only
    /// source: `own.rs` states none of them (RFC-0125 §3 M3, the
    /// emitter-reads-the-core-alone slice). `None` when the placer is not
    /// installed (`VYRN_NO_PLACER=1`), and then no such release is emitted,
    /// because the placer is the pass that states them.
    facts: Option<vyrn_lower::core::Facts>,
    /// The `Owned` table (RFC-0086 M1) — the same one `own` decided with, so a
    /// user type's declared `release` reaches this backend without a second list.
    owned: vyrn_frontend::declared::Owned,
    /// RFC-0008's threshold, as an ordinal. Compile-time, and that is the point:
    /// with `logging { level: warn }` a `.debug(..)` call emits no write at all,
    /// which is what makes a disabled log site cost nothing on every engine. A
    /// runtime comparison here would be a behaviour change dressed as a lowering.
    log_level: usize,
    /// Where a log line goes. Compile-time-known, so the write names its
    /// descriptor directly rather than looking one up.
    log_sink: LogSink,
    /// The four bytes holding the file sink's descriptor, when there is one —
    /// [`LogSink::File`]'s answer to the native backend's `@__vyrn_log_file`.
    /// `None` for a console sink, so a program that does not log to a file
    /// reserves nothing and its module is unchanged.
    log_fd: Option<u32>,
    /// RFC-0114 §25: this build is AUDITED, so `std/runtime`'s four `audit`
    /// calls are emitted and `_start` arms the instrument and asserts on it.
    /// False is every ordinary build: the calls are dropped, the bodies reach
    /// no export, [`wasm::Module::sweep`] takes them, and not one byte moves.
    audit: bool,
}

/// The reserved prefix of the exit-residue instrument's calls inside
/// `std/runtime` (RFC-0114 §25). One prefix rather than four names, so a
/// fifth hook needs no edit here.
const AUDIT_PREFIX: &str = "runtime$audit";

impl<'a> Cx<'a> {
    /// RFC-0114 R1′ read off the core (RFC-0125 §3 M3, the
    /// deletion-preparation slice): does this frame release the unnamed
    /// receiver of the field read at `node`, and around which holes?
    ///
    /// The core states it as a `St::Drop` of the name whose
    /// `NameInfo::receiver` is this node, with the name's own hole set.
    ///
    /// There is no second answer to fall back to: `own.rs` states no
    /// receiver table any more (RFC-0125 §3 M3, the emitter-reads-the-core-
    /// alone slice).
    fn receiver_row(&self, node: usize) -> Option<Vec<String>> {
        let f = self.facts.as_ref()?;
        f.receivers.get(&self.plan.key_of(node)).cloned()
    }

    /// RFC-0114 M2 and exit-residue round eighteen read off the core
    /// (RFC-0125 §3 M3, the emitter-reads-the-core slice): does the store at
    /// `node` release the value it displaces?
    ///
    /// The core states the plan's two tables as ONE answer (`St::Store`'s
    /// `releases`), because both compiled backends read them as one — the row, the
    /// `mentions_place` guard, and round eighteen's `fresh_str` exception,
    /// all of which the core now spells (`core::Builder::stmt`). What stays
    /// the emitter's is the region gate, which is a property of where the
    /// code stands and not of the store.
    ///
    /// A site the core states nothing for falls back to the plan: RFC-0091
    /// M2's `place at` rewrite BUILDS the store statements a user
    /// container's `c[h] = v` becomes, the checker walks those, and this
    /// pass walks the source statement. `compiler/vyrn-cli/tests/coretables.rs`
    /// pins that residue at twelve rows over the corpus.
    fn store_row(&self, node: usize) -> bool {
        self.store_fact(node).unwrap_or(false)
    }

    /// The core's answer alone, or `None` where it states none — a body this
    /// pass could not lower, or a statement RFC-0091 M2's rewrite built. A
    /// store to a NAME asks for it this way, because the answer it falls
    /// back to is the plan's row AND the guards around it, not the row
    /// alone.
    fn store_fact(&self, node: usize) -> Option<bool> {
        let f = self.facts.as_ref()?;
        f.stores.get(&self.plan.key_of(node)).copied()
    }

    /// Round twenty-eight read off the core (RFC-0125 §3 M3, the
    /// emitter-reads-the-core slice): does this statement discard an owned
    /// result the emission frees rather than drops? The core states it as a
    /// `St::Drop` of the temporary the statement's value bound, keyed by the
    /// `Stmt::Expr` node.
    ///
    /// There is no second answer to fall back to: `own.rs` states no
    /// discarded table any more (RFC-0125 §3 M3, the
    /// emitter-reads-the-core-alone slice).
    fn discarded_row(&self, node: usize) -> bool {
        self.facts
            .as_ref()
            .is_some_and(|f| f.discarded.contains(&self.plan.key_of(node)))
    }

    /// Does this `for` give its container back where it ends? The core states
    /// it at the loop ([`vyrn_lower::core::Facts::loop_gives_back`]); this
    /// emitter read the word `consume` off the source until RFC-0125 §3 M3's
    /// event-stream slice.
    fn loop_gives_back(&self, node: usize) -> bool {
        self.facts
            .as_ref()
            .is_some_and(|f| f.loop_gives_back.contains(&self.plan.key_of(node)))
    }

    /// Does the container's release at this `for` walk the BUFFER alone? The
    /// core states it at the loop
    /// ([`vyrn_lower::core::Facts::loop_buffer_only`]), out of the same
    /// sentence that says whose an element is; this emitter read the KIND of
    /// the plan's own row until RFC-0125 §3 M3's container slice.
    fn loop_buffer_only(&self, node: usize) -> bool {
        self.facts
            .as_ref()
            .is_some_and(|f| f.loop_buffer_only.contains(&self.plan.key_of(node)))
    }

    /// RFC-0114 M1, stated by the core (RFC-0125 §3 M3, the last table's
    /// slice): does the caller free this argument's value after the call or
    /// operator above it? The core carries the key on the name the argument
    /// bound ([`vyrn_lower::core::NameInfo`]'s `arg_drop`), which is where an
    /// operator's operand gets one too — `a + b` is `@concat(a, b)` here.
    ///
    /// There is no second answer to fall back to, exactly as [`Cx::store_row`]
    /// has none: `own.rs` states no argument table any more.
    fn arg_drop_row(&self, node: usize) -> bool {
        self.facts
            .as_ref()
            .is_some_and(|f| f.arg_drops.contains(&self.plan.key_of(node)))
    }

    /// RFC-0114 Rule N read off the core (RFC-0125 §3 M3, the derivation
    /// slice): the releases one edge of the join at `node` owes because
    /// another edge took the name. The core states each as a `St::Drop` at a
    /// `Site::Edge`, which is the join and the edge — a position in a branch
    /// is not a key, and this is why the drop carries one. The kernel's
    /// `equalize` is the one statement of the rule, and `own.rs` states none.
    fn edge_rows(&self, node: usize) -> Vec<(String, u32)> {
        let Some(f) = self.facts.as_ref() else {
            return Vec::new();
        };
        f.edges
            .get(&self.plan.key_of(node))
            .cloned()
            .unwrap_or_default()
    }

    /// Whether the construct at `node` switches on a value the frame MADE
    /// ([`vyrn_lower::core::Facts::owns_scrutinee`]), so the boxes its
    /// binders came out of are its own to give back.
    ///
    /// A wider question than [`Cx::match_consumes`] and it contains it: a
    /// take is one way to own the scrutinee, and a `consume`, a call's
    /// result, a literal and a `Map` lookup are the others. Each compiled
    /// backend read those four off the SOURCE, beside the table that answered
    /// the take; the core states all of them as one row (RFC-0125 §3 M3, the
    /// box slice). A site the core has no answer for is a body no core was
    /// built for.
    fn owns_scrutinee(&self, node: usize) -> bool {
        self.facts
            .as_ref()
            .is_some_and(|f| f.owns_scrutinee.contains(&self.plan.key_of(node)))
    }

    /// Round forty's table read off the core (RFC-0125 §3 M3, the
    /// deletion-preparation slice): the payload binders the arm at
    /// `(key, arm)` releases at its end, each with the holes the arm left in
    /// it. The core states them as the trailing run of `St::Drop` in
    /// `Arm::body`, with `NameInfo::holes` on each binder;
    /// Since RFC-0125 §3 M3's derivation slice the rows are the kernel's own
    /// answer over the core, and `own.rs` states none: the core answers at
    /// every switch it lowers — a `match`, an `if let` and a `?` alike — and
    /// a site with no answer is a body no core was built for.
    fn arm_row(&self, key: usize, arm: u32) -> Option<Vec<(String, Vec<String>)>> {
        let f = self.facts.as_ref()?;
        let rows = f.arms.get(&(self.plan.key_of(key), arm))?;
        // The kind the core carries beside each binder is the interpreter's
        // reader; this backend reads the release off the type itself.
        (!rows.is_empty()).then(|| {
            rows.iter()
                .map(|(n, h, _)| (n.clone(), h.clone()))
                .collect()
        })
    }

    /// Substitute the monomorphization this lowering is inside.
    ///
    /// The chokepoint, and the point of having one: [`Cx::resolve`], [`Cx::ll`],
    /// [`Cx::fields`] and [`Cx::ty_gap`] all go through it, so a `Type::Param`
    /// cannot reach `llt_of` — where it lowers to `void`, which is not an error
    /// but a smaller function — by any route that asks this `Cx` about a type.
    /// That is what makes M0's `Type::Param` arm and `ty_gap`'s refusal
    /// unreachable rather than merely unhit.
    ///
    /// Note this substitutes into the type EXPRESSION, before any `App` is
    /// expanded: `Box<T>` and `fn f<T>` may both spell their parameter `T`, and
    /// `resolve` builds the declaration's own substitution from the `App`'s
    /// arguments afterwards. So the two `T`s cannot be confused.
    fn sub(&self, ty: &Type) -> Type {
        if self.subst.is_empty() {
            ty.clone()
        } else {
            ftypes::substitute(ty, &self.subst)
        }
    }

    /// The LLVM shape of `ty` — `Gen`'s own answer, so layout and lowering
    /// cannot drift apart (RFC-0077 M0's whole argument for parsing the string).
    fn ll(&self, ty: &Type) -> String {
        llt_of(&self.sub(ty), &self.types)
    }

    fn resolve(&self, ty: &Type) -> Type {
        ftypes::resolve(&self.sub(ty), &self.types)
    }

    /// RFC-0126 §8.4's `words(t)`: the slots a payload of type `t` rides in.
    /// `Gen`'s own answer, for [`Cx::ll`]'s reason.
    fn words(&self, ty: &Type) -> usize {
        crate::payload_words_of(&self.sub(ty), &self.types)
    }

    /// The aggregate member index the `i`th payload of a variant starts at
    /// (member 0 is the tag).
    fn payload_slot(&self, payload: &[Type], i: usize) -> usize {
        1 + payload[..i].iter().map(|p| self.words(p)).sum::<usize>()
    }

    /// The variants of the sum `ty`, in TAG order — [`crate::sum_variants_of`],
    /// under this emitter's substitution. `Gen`'s own answer, for [`Cx::ll`]'s
    /// reason, and what makes a release and a copy one walk per sum rather than
    /// one per SPELLING of a sum (RFC-0126 §8.11, M4a).
    fn sum_vs(&self, ty: &Type) -> Option<Vec<EnumVariant>> {
        crate::sum_variants_of(&self.sub(ty), &self.types)
    }

    /// The PROGRAM's own body for a lambda literal at this address, or `None`
    /// for a literal the program does not hold — see [`Cx::lambdas`].
    fn lambda(&self, at: &Expr) -> Option<&'a LambdaBody> {
        self.lambdas.get(&(at as *const Expr as usize)).copied()
    }

    /// Whether a narrow scalar load of `ty` has to sign-extend — [`load_of`]'s
    /// second argument, kept here so it comes off the same type the shape does
    /// rather than being decided at the load.
    fn signed(&self, ty: &Type) -> bool {
        Num::of(&self.resolve(ty)).is_some_and(|n| n.signed)
    }

    /// The signature of a body discovered during emission, handing out its
    /// function index if this is the first site to ask for it.
    ///
    /// `f` arrives already substituted, with its type parameters cleared: the
    /// signature belongs to the SPECIALIZATION, not to whatever the discovering
    /// body happens to be inside. [`Cx::signature`] then does the rest — a
    /// representation per parameter, the `modify` flags — so an instance, a lifted
    /// lambda and an ordinary function are all checked by one function.
    fn enqueue(
        &self,
        m: &mut Module,
        key: Key,
        f: Rc<Function>,
        body: Body<'a>,
        subst: HashMap<String, Type>,
        binds: HashMap<String, FnBinding>,
    ) -> Result<Sig, String> {
        if let Some(p) = self.mono.borrow().insts.iter().find(|p| p.key == key) {
            return Ok(p.sig.clone());
        }
        let s = self.signature(&f)?;
        let (wp, wr) = self.wasm_sig(&s, f.line)?;
        let sig = Sig {
            index: m.reserve_func(&wp, &wr),
            ..s
        };
        let mut mono = self.mono.borrow_mut();
        mono.insts.push(Pending {
            key,
            f,
            body,
            sig: sig.clone(),
            subst,
            binds,
        });
        Ok(sig)
    }

    /// A generic instantiation (M2e): [`Cx::enqueue`] with no `fn` parameters.
    fn instantiate(
        &self,
        m: &mut Module,
        f: &'a Function,
        type_args: Vec<Type>,
        subst: HashMap<String, Type>,
    ) -> Result<Sig, String> {
        let mut sf = shell_of(f);
        for p in &f.params {
            sf.params.push(Param {
                name: p.name.clone(),
                capability: p.capability,
                ty: ftypes::substitute(&p.ty, &subst),
            });
        }
        sf.ret = ftypes::substitute(&f.ret, &subst);
        self.enqueue(
            m,
            Key::Generic(f.name.clone(), type_args),
            Rc::new(sf),
            Body::Block(&f.body),
            subst,
            HashMap::new(),
        )
    }

    fn repr(&self, ty: &Type, line: usize) -> Result<Repr, String> {
        if let Some(why) = self.ty_gap(ty, 0) {
            return unsupported(&why, line);
        }
        let ll = self.ll(ty);
        Ok(match ll.as_str() {
            "void" => Repr::Unit,
            // RFC-0083: wasm's own 128-bit vector type. Read off the textual
            // backend's spelling for the same reason every other repr is — one
            // copy of the lowering decision — and matched before the aggregate
            // test because `<4 x float>` is a wasm VALUE, not a memory shape.
            // A `Mask32x4` is `<4 x i32>` all-ones/all-zeros on both backends —
            // the one bit pattern, so `v128.bitselect` here and `select` there
            // consume the same thing.
            // M4's wide width and its mask are the same 128 bits under two more
            // spellings — wasm has ONE vector type and the lane interpretation
            // belongs to the instruction, which is exactly why four rows here read
            // one `V128`.
            "<4 x float>" | "<4 x i32>" | "<2 x double>" | "<2 x i64>" => {
                Repr::Scalar(ValType::V128)
            }
            _ if ll.starts_with('{') || ll.starts_with('[') => Repr::Agg(
                layout::of_ll(&ll)
                    .map_err(|e| format!("direct backend: layout of {ll} at line {line}: {e}"))?,
            ),
            _ => match wasm::abi(&ll) {
                Some(v) => Repr::Scalar(v),
                None => Repr::Unit,
            },
        })
    }

    /// Why `ty` cannot be lowered, if it cannot.
    ///
    /// The dangerous case is silent rather than loud without this: an
    /// unresolvable name lowers to `void`, i.e. to nothing at all.
    ///
    /// A validated type (`type Age = Int64 where value >= 0`) used to be refused
    /// here for a related reason — it has the SAME representation as its base, so
    /// it lowers cleanly and simply never checks the refinement. That is now
    /// [`Fn_::coerce`]'s job instead: the check belongs at the flow, not in the
    /// type, because the type is where it would have to be re-decided at every
    /// site. `a_validated_type_is_checked_wherever_it_is_reached` is the test that
    /// followed the refusal.
    ///
    /// Depth-bounded because a record may hold a `Ref` to its own type, which is
    /// finite in memory and infinite as a tree.
    fn ty_gap(&self, ty: &Type, depth: usize) -> Option<String> {
        if depth > 6 {
            return None;
        }
        let ty = &self.sub(ty);
        match ty {
            // Unreachable for a well-typed program since M2e: every type this
            // `Cx` is asked about goes through [`Cx::sub`] first, so a surviving
            // parameter means the instantiation that should have fixed it did
            // not. Kept as a refusal rather than trusted, because `llt_of` prints
            // `void` for a parameter and a `void` is not a diagnostic.
            Type::Param(p) => return Some(format!("the unsolved type parameter `{p}`")),
            Type::Named(n) | Type::App(n, _) => match self.types.get(n) {
                Some(_) => {}
                // `Code` and `Token` are builtins `resolve` knows without a decl.
                None if n == "Code" || n == "Token" => {}
                None => return Some(format!("the unknown type `{n}`")),
            },
            _ => {}
        }
        match self.resolve(ty) {
            Type::Record(fs) => fs.iter().find_map(|f| self.ty_gap(&f.ty, depth + 1)),
            Type::Array(i) | Type::ArrayN(i, _) => self.ty_gap(&i, depth + 1),
            Type::Map(a, b) => self
                .ty_gap(&a, depth + 1)
                .or_else(|| self.ty_gap(&b, depth + 1)),
            // Every sum, one arm — the two built-in ones resolve to their
            // variant lists since RFC-0126 §8.11's M4b.
            Type::Enum(vs) => vs
                .iter()
                .flat_map(|v| v.payload.iter())
                .find_map(|p| self.ty_gap(p, depth + 1)),
            _ => None,
        }
    }

    fn fields(&self, ty: &Type) -> Option<Vec<Field>> {
        ftypes::record_fields(&self.sub(ty), &self.types)
    }

    /// The signature a call site sees. `index` is filled in by the caller, which
    /// is the only thing that knows where in the module this lands.
    fn signature(&self, f: &Function) -> Result<Sig, String> {
        if !f.type_params.is_empty() {
            return unsupported(&format!("generic function `{}`", f.name), f.line);
        }
        for p in &f.params {
            // A parameter's representation has to exist even though the call
            // site does not read it back, or a gap in a callee would surface as
            // a mystery at every caller instead. A `modify` one included: it
            // crosses as an address, but the callee still copies the pointed-to
            // value in and out, so its shape has to be describable.
            self.repr(&p.ty, f.line)?;
        }
        Ok(Sig {
            index: 0,
            params: f.params.iter().map(|p| p.ty.clone()).collect(),
            modify: f
                .params
                .iter()
                .map(|p| p.capability == Capability::Modify)
                .collect(),
            ret: self.repr(&f.ret, f.line)?,
            ret_ty: f.ret.clone(),
        })
    }

    /// The wasm signature of a Vyrn function: an aggregate return becomes a
    /// hidden leading pointer the callee writes through, and every aggregate
    /// parameter is its address.
    fn wasm_sig(&self, sig: &Sig, line: usize) -> Result<(Vec<ValType>, Vec<ValType>), String> {
        let mut params = Vec::new();
        if sig.ret.agg().is_some() {
            params.push(ValType::I32);
        }
        for (i, p) in sig.params.iter().enumerate() {
            // A `modify` parameter is a pointer whatever it points at, so even a
            // scalar one crosses as an `i32`.
            if sig.modify.get(i) == Some(&true) {
                self.repr(p, line)?;
                params.push(ValType::I32);
                continue;
            }
            match self.repr(p, line)?.val() {
                Some(v) => params.push(v),
                None => return unsupported("a Unit parameter", line),
            }
        }
        let results = match &sig.ret {
            Repr::Scalar(v) => vec![*v],
            _ => vec![],
        };
        Ok((params, results))
    }
}

// ---------------------------------------------------------------------------
// Function lowering
// ---------------------------------------------------------------------------

/// Where a binding lives: a wasm local for a scalar, a frame slot for an
/// aggregate, or a fixed address for module state.
///
/// The third case is RFC-0013's top-level `let` (RFC-0077 M2f), and it is a
/// separate variant rather than a flag on `Slot` because a frame offset is
/// relative to a base that changes every call and a global's address does not —
/// which is the whole of what makes it survive between them.
///
/// `Static` covers a scalar global as well as an aggregate one, so there is one
/// mechanism rather than two. A wasm global holds one value type and could not
/// have held a record, and the textual backend's globals are memory too, so
/// matching it costs nothing: a scalar global is a load and a store where a local
/// would have been a `local.get`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum Place {
    Local(u32),
    Slot(u32),
    Static(u32),
}

/// A stream's step signature (RFC-0075 M2b), which is a function of the ELEMENT
/// type and nothing else — the cursor is two plain `Int64`s precisely so that it
/// is. Both the construction site and the loop that dispatches through it derive
/// the signature from here, because a stored `fn` value is keyed by its signature
/// and two spellings of one type would be two dispatchers.
///
/// The third parameter is the closing flag (RFC-0090 M3): a stream gives its
/// cursor slot back by asking its own step, because the slab is `std/stream`'s
/// and a release cannot name it.
fn stream_step_sig(elem: &Type) -> Type {
    Type::Fn(
        vec![Type::Int, Type::Int, Type::Bool],
        Box::new(Type::option(elem.clone())),
    )
}

/// What one owned binding is released with, in the backend's own vocabulary —
/// RFC-0101 §2.3's half of the split. The placement says which of these runs at
/// which exit and in what order; this says what running one emits.
#[derive(Clone)]
struct RelSlot {
    place: Place,
    rel: Rel,
    /// Registration order. Under a stack discipline the live bindings come off
    /// in reverse of it, which is the one thing a stream cursor's position still
    /// needs (see [`Fn_::cursors`]).
    seq: u32,
}

/// What a release frame entry reclaims (RFC-0075 M2b added the second one,
/// RFC-0077 M6 the last two).
#[derive(Clone, PartialEq, Eq, Debug)]
enum Rel {
    /// A `Stream<T>` — its buffer if it is one, its cursor slot if it is a
    /// producer. The element type comes along because releasing a producer means
    /// CALLING its step, and a step is dispatched by element type (RFC-0090 M3).
    Stream(Type),
    /// A `String` — the place holds the buffer pointer itself.
    Str,
    /// An aggregate owning heap buffers at these byte offsets: an `Array`'s data,
    /// a `Map`'s keys and values, a `SmallArray`'s `data` (null while inline,
    /// which `free` refuses).
    ///
    /// Offsets rather than a `Type`, because the layout is a compile-time fact and
    /// carrying the type would mean asking `layout_of` a second time at the
    /// release — the place a wrong answer is silent.
    Buffers(Vec<u32>),
    /// An aggregate the engines copy by value, holding heap in its places
    /// (RFC-0089 rule 4, Phase 5): a record field, an enum or `Option`/`Result`
    /// payload, a closure's capture block. The walk is the type, so the type is
    /// what this carries — a variant payload is chosen at run time and only the
    /// live one is released.
    ///
    /// The second field is RFC-0093 M2's hole set — the places a `consume` took
    /// out of THIS binding, relative to it. Empty everywhere but at a `let` that
    /// was drained, and the walk skips exactly these.
    Deep(Type, Vec<String>),
    /// A type that declared `impl Owned` (RFC-0086 M1) — call the `release` it
    /// declared, whose flattened name this carries. The receiver's own type, so
    /// the call goes through the ordinary path rather than a second ABI.
    Call(String, Type),
}

impl Place {
    /// Push the address `off` bytes into this place, or `None` for a wasm local —
    /// the one place with no address at all, which is exactly why a scalar passed
    /// as a `modify` argument has to be spilled.
    fn addr(self, b: &mut Frame, off: u32) -> Option<()> {
        match self {
            Place::Local(_) => None,
            Place::Slot(base) => {
                b.slot(base + off);
                Some(())
            }
            Place::Static(at) => {
                b.ins(&Instruction::I32Const((at + off) as i32));
                Some(())
            }
        }
    }
}

/// Storage an aggregate is built INTO (RFC-0125 §2.1, M1's second slice): a
/// frame slot, or an address a wasm local holds plus an offset.
///
/// A record or array literal, and a call that returns an aggregate, write
/// straight into one of these when the consumer already owns the storage. A
/// nested literal then costs no frame of its own, which is what §1.4's
/// per-node copy charged: every intermediate aggregate of a literal landed in
/// the frame, and the frame was the sum of them.
///
/// The rule, stated once: **a value is built in place only into storage
/// nothing can name while it is being built** — a fresh `let`'s slot, a field
/// or element of a literal under construction, a call's result. A store into
/// named storage (an assignment, a field or element store whose value reads
/// the binding, module state) keeps the copy, because a field written early
/// would be visible to a later field's initializer otherwise, and the
/// interpreter builds the whole value first.
#[derive(Clone, Copy)]
enum Dest {
    Slot(u32),
    Addr(u32, u32),
}

impl Dest {
    /// Push the address `off` bytes into this destination.
    fn addr(self, b: &mut Frame, off: u32) {
        match self {
            Dest::Slot(base) => {
                b.slot(base + off);
            }
            Dest::Addr(l, base) => {
                b.ins(&Instruction::LocalGet(l));
                if base + off != 0 {
                    b.ins(&Instruction::I32Const((base + off) as i32));
                    b.ins(&Instruction::I32Add);
                }
            }
        }
    }

    /// The destination `off` bytes into this one.
    fn at(self, off: u32) -> Dest {
        match self {
            Dest::Slot(base) => Dest::Slot(base + off),
            Dest::Addr(l, base) => Dest::Addr(l, base + off),
        }
    }

    /// A binding's place as a destination. Module state is named storage and
    /// is never one (the rule above); a scalar local has no address.
    fn of(p: Place) -> Option<Dest> {
        match p {
            Place::Slot(off) => Some(Dest::Slot(off)),
            Place::Local(_) | Place::Static(_) => None,
        }
    }
}
/// The spelling a lifted lambda's shell is named by, followed by the name of
/// the function that holds the literal: `@lambda main`. Reserved, so no Vyrn
/// identifier can be it.
const LAMBDA: &str = "@lambda";

/// An empty `Function` to fill in for a lifted lambda (RFC-0023).
///
/// A synthesized declaration rather than a bespoke lowering path: the captures
/// become ordinary read parameters, so [`lower_fn`] emits it with no case of its
/// own. [`Fn_::lift_lambda`] names it `@lambda <owner>`: the analysis records
/// a lambda's release rows under the ENCLOSING function's name, keyed by the
/// lambda's own nodes, and [`lower_body`] reads `Cx::releases` under the
/// owner (RFC-0125 M3, third slice). Before that the
/// shell owned no rows, and a row inside a lambda was placed and never run.
fn f_shell(line: usize) -> Function {
    Function {
        name: LAMBDA.to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        type_bounds: HashMap::new(),
        params: Vec::new(),
        ret: Type::Unit,
        body: Block { stmts: Vec::new() },
        line,
        col: 0,
        is_extern: false,
        is_export_extern: false,
        is_gen: false,
        is_mut: false,
    }
}

/// The declaration a specialization is lowered under, with no statements in it.
///
/// A specialization differs from its callee in its SIGNATURE — the type
/// parameters are gone, a `fn`-typed parameter has become the captures its
/// target needs — and never in its body. So the shell carries the difference and
/// [`Pending::body`] points at the callee's own block, which is the block the
/// checker typed and `vyrn-lower` recorded. Cloning it instead is what made
/// 9,505 backend answers unreachable from any recorded type (RFC-0101 §3 M2).
fn shell_of(f: &Function) -> Function {
    Function {
        name: f.name.clone(),
        line: f.line,
        ..f_shell(f.line)
    }
}

/// One function being lowered.
struct Fn_<'a, 'p> {
    cx: &'a Cx<'p>,
    /// Name → where it lives and what it is. A scope stack rather than a map per
    /// block: shadowing pushes, and leaving a block truncates.
    scope: Vec<(String, Place, Type)>,
    /// wasm blocks open between here and the function's outermost one. A
    /// `return` is `br depth`.
    depth: u32,
    /// (break target, continue target, region depth) per enclosing loop. The
    /// first two are the depth each was opened at, so `br` distance is
    /// `depth - opened - 1`; the third is how many `region` blocks were open
    /// when the loop started, which an exit edge has to close (RFC-0004 §4).
    ///
    /// **The release boundary that used to be the third field is gone**
    /// (RFC-0101 M4): a `break` reads the steps the placement put at that
    /// `break`, so no engine derives an index into its own frames any more.
    loops: Vec<(u32, u32, u32)>,
    /// RFC-0125 M1: the header parts of every binding a `while` hoisted
    /// (`hoist_walks`), keyed by name, live for the loop's extent.
    walks: HashMap<String, Walk>,
    /// RFC-0125 M1: the two locals a failed bounds check parks its message
    /// and index in before branching to the function's one trap site — see
    /// `bounds_check`. `None` for a frame that has no site (the globals
    /// initializer), which keeps the call at the check.
    trap_site: Option<(u32, u32)>,
    ret: Repr,
    ret_ty: Type,
    /// The wasm local holding the hidden aggregate-return pointer, if any.
    dest: Option<u32>,
    /// Reusable scratch, taken on first use. Every use is a set immediately
    /// followed by the reads that consume it, so one pair suffices however
    /// deeply expressions nest.
    scratch: HashMap<(ValType, u8), u32>,
    /// What each owned binding of this function is released WITH, keyed by
    /// `own`'s own key — the `Stmt::Let`'s node address, or the construct's for
    /// a temporary it owns. `seq` is the order it was registered in.
    ///
    /// **RFC-0101 M4: this is a lookup table, not a plan.** Until the deletion
    /// phase it was one frame per open block, walked from a boundary index —
    /// the same stack the textual backend and the interpreter each kept
    /// privately, each asserting "innermost first, newest first" separately
    /// (§1.4). The order is [`vyrn_frontend::own::Ownership::releases`]' now.
    /// What is left here is the half §2.3 leaves in a backend: the place a value
    /// lives in and the [`Rel`] that says which instructions reclaim it —
    /// `Rel::Buffers` carries LAYOUT OFFSETS, which is target vocabulary.
    rel_slots: HashMap<usize, RelSlot>,
    /// Registrations so far, which is what a [`RelSlot::seq`] counts.
    rel_seq: u32,
    /// The rows that still name a frame slot, each with the frame top it was
    /// registered at — the floor below which no statement may give a slot back.
    ///
    /// [`Frame::alloc`]'s rule reads "a slot is a statement's unless the
    /// statement bound a name", and the body walker tested exactly that: the
    /// scope's length. A NAME is not the only thing that outlives a statement.
    /// A `for` over an unnamed iterable, an `if let` over a temporary and a
    /// `match` over one each copy that value into a slot and register a release
    /// row for it, and WHERE that row runs is the core's answer, not this
    /// emitter's: for a `for` over an array literal the core says the
    /// function's exit, because `declared::type_of` names no array literal's
    /// type and `own` therefore places nothing at the loop's own end. The
    /// statement bound no name, so the walker gave the slot back, the next
    /// statement built its own temporary over it, and the two rows at the exit
    /// released the second statement's value twice — a trap at address -16 in
    /// `free`, on two `for` loops in one body.
    ///
    /// A row leaves this list when it is released on a FALL-THROUGH exit — a
    /// block's or a construct's own — because the path that carries on is the
    /// path that no longer holds it. A release at a `return`, a `?`, a `break`
    /// or a `continue` is on a branch, and the fall-through still holds the
    /// value, so such a row keeps its floor. Clearing on every release instead
    /// would give the slot back on a path that still names it; keeping every
    /// row to the end of the body instead summed a statement's temporaries
    /// again, and 250 `print(match parseFloat64(..) { .. })` statements in one
    /// `main` then wanted 10,048 bytes of a frame limited to 8,192.
    rel_pending: Vec<(usize, u32)>,
    /// RFC-0101 M4: the release steps placed at every exit of this body, keyed
    /// by the node the exit is AT. Read, never derived.
    placed: HashMap<(ExitKind, usize), Vec<(usize, Option<Vec<String>>)>>,
    /// The stream cursors a `for x in pull()` opened, innermost last, with the
    /// registration count each was opened at.
    ///
    /// **The one step the placement has nothing for** (RFC-0101 M4's phase-2
    /// gate names it `StreamCursor`): the cursor is not a row of `own`'s map,
    /// because RFC-0075 M2b closes a stream's producer from the loop that made
    /// it rather than from a reclamation rule. Only a function exit reaches one
    /// — a `break` leaves through the loop's own release — and its POSITION in
    /// such a walk is still frame structure, so a step registered before the
    /// cursor is a frame outside the loop and the cursor runs first.
    cursors: Vec<(Place, Type, u32)>,
    /// Lexical `region` nesting depth within this body, so an exit edge knows how
    /// many arena scopes it is leaving. The runtime counter is dynamic (a callee's
    /// region nests inside its caller's); this is only the part one body can see,
    /// which is exactly the part its own `br`s unwind past.
    region_depth: u32,
    /// One local per region open in this body, outermost first: the mark
    /// [`Fn_::region_enter`] took, which [`Fn_::region_exit`] hands back to
    /// `std/runtime`. Parallel to `region_depth`, which is its length while a
    /// statement is being lowered.
    region_marks: Vec<u32>,
    /// The locals holding the argument temporaries this frame releases, innermost
    /// call last. Teed where the argument is EVALUATED and handed back where its
    /// call ends — see [`Fn_::call`].
    arg_frees: Vec<(u32, Type)>,
    /// The holes the walk in progress must skip, relative to the place it is
    /// looking at (RFC-0093 M2). Taken at the top of [`Fn_::rel_at`], so a walk
    /// into anything that is not a record starts empty.
    rel_holes: Vec<String>,
    /// The type a value is being built FOR, innermost last.
    ///
    /// `None` and `Some(x)` do not say what they are — an `Option<T>`'s `T`
    /// comes from the position, not the constructor — so the sum constructors
    /// read it back off here. Same mechanism the LLVM emitter uses, for the
    /// same reason.
    expect: Vec<Type>,
    /// RFC-0023: inside a specialization, each `fn`-typed parameter's resolved
    /// direct-call target. Empty in every ordinary function — which is what makes
    /// calling a `fn` parameter a lookup here rather than a value on the stack,
    /// and why no function table exists.
    fn_binds: HashMap<String, FnBinding>,
    /// The local `String` accumulators `s = s + …` may grow in place, from
    /// [`crate::append_candidates`] — the SAME whitelist the textual backend
    /// clears, not a second one. Two copies of that rule would be two answers to
    /// "may this buffer move", and one of them a use-after-free.
    append_ok: std::collections::HashSet<String>,
    /// Whether this body is a declared `release` (RFC-0086): the CALLER walks
    /// the receiver's payload boxes after the call, so a `match consume self`
    /// inside one must not free them — see [`Fn_::frees_boxes`].
    is_release: bool,
    /// wasm local holding the accumulator's pointer → the frame slot holding its
    /// ownership flag. Keyed by local index rather than by name because the
    /// local IS the binding: two `let out`s in one body are two accumulators, and
    /// a global (a `Place::Static`) never gets an entry at all.
    str_append: HashMap<u32, u32>,
    /// RFC-0125 M1: the storage the NEXT expression may build itself into, and
    /// the type that storage holds. Set by a consumer that owns fresh storage
    /// ([`Fn_::agg_into`]), taken at the top of [`Fn_::expr_inner`] so only the
    /// immediate expression sees it; a literal or a call that uses it says so
    /// through `dest_used`, and the consumer skips its copy.
    dest_hint: Option<(Dest, Type)>,
    dest_used: bool,
    /// The hint an `Expr::Call` carried into [`Fn_::call_inner`], which takes it
    /// before any argument is lowered so a nested call cannot claim it.
    call_dest: Option<(Dest, Type)>,
    /// The declared function this frame's plan rows are under: the function
    /// itself, or for a lifted lambda the function that holds the literal
    /// (RFC-0125 M3, third slice).
    owner: String,
    /// This frame's own core body, and where each of its names lives — RFC-0125
    /// §3 M3, the interleave slice.
    ///
    /// The driver used to pick its walk per FUNCTION, so an AST arm could only
    /// go when every body of the corpus went through the core. The unit is the
    /// STATEMENT now: the two walks share this frame — its locals, its scope
    /// and the places below — and each statement goes to whichever walk carries
    /// it. `None` for a frame the core states no body for.
    core: Option<std::rc::Rc<vyrn_lower::core::Body>>,
    /// The core's rows for each source statement of this body
    /// ([`vyrn_lower::core::Body::rows_by_statement`]).
    core_at: HashMap<usize, Vec<St>>,
    /// Where the core's names live, and what the operand stack is holding.
    /// Shared by the two walks: a name the AST arm bound is found through
    /// [`Fn_::scope`], and one this walk bound is pushed onto it.
    core_w: Walked,
}

/// A lowering context with nothing in scope and nothing to return to: what the
/// globals initializer is, and what typing an initializer outside any function
/// needs. Module state itself is still visible, because it lives in [`Cx`].
fn top_level<'a, 'p>(cx: &'a Cx<'p>) -> Fn_<'a, 'p> {
    Fn_ {
        cx,
        scope: Vec::new(),
        depth: 0,
        loops: Vec::new(),
        walks: HashMap::new(),
        trap_site: None,
        ret: Repr::Unit,
        ret_ty: Type::Unit,
        dest: None,
        scratch: HashMap::new(),
        rel_slots: HashMap::new(),
        rel_seq: 0,
        rel_pending: Vec::new(),
        placed: HashMap::new(),
        cursors: Vec::new(),
        region_depth: 0,
        region_marks: Vec::new(),
        arg_frees: Vec::new(),
        rel_holes: Vec::new(),
        expect: Vec::new(),
        fn_binds: HashMap::new(),
        append_ok: std::collections::HashSet::new(),
        is_release: false,
        str_append: HashMap::new(),
        dest_hint: None,
        dest_used: false,
        call_dest: None,
        owner: String::new(),
        core: None,
        core_at: HashMap::new(),
        core_w: Walked::default(),
    }
}

/// The module-state initializer (RFC-0013): every top-level `let`'s value stored
/// into its fixed address, in declaration order, in one function `_start` calls
/// before `main`.
///
/// It is a body like any other — the initializers go through [`Fn_::store_into`]
/// and therefore through the M2d coercion seam, so a `let n: Age = f()` at the top
/// level validates exactly as one inside a function does. That is why this is not
/// a data segment of constants: an initializer may be a string, an array literal
/// that has to reach the heap, or a call.
///
/// No wrapping `block`, because there is no `return` to route: an initializer is
/// an expression.
fn lower_globals_init(m: &mut Module, program: &Program, cx: &Cx<'_>) -> Result<Frame, String> {
    let mut b = Frame::new(0, &[], 0);
    let mut f = top_level(cx);
    for g in &program.globals {
        let (place, ty) = cx.globals[&g.name].clone();
        let r = cx.repr(&ty, g.line)?;
        f.store_into(m, &mut b, place, &r, &g.init, &ty, false)?;
        // The accumulator's ownership word starts true for every initializer but a
        // literal, which is data-segment storage nothing allocated. Getting this
        // wrong one way abandons the initializer's buffer at the first append; the
        // other way frees a data segment, which `free` refuses anyway.
        if let Some(&at) = cx.gappend.get(&g.name) {
            let owns = !matches!(g.init, Expr::Str(_));
            b.ins(&Instruction::I32Const(at as i32))
                .ins(&Instruction::I32Const(owns as i32))
                .ins(&Instruction::I32Store(word()));
        }
        // One frame holds every initializer's temporaries, so the bound is
        // checked per global: the one that crossed it is the one to name.
        frame_fits(&b, &g.name, g.line)?;
    }
    Ok(b)
}

/// The module-state teardown (RFC-0114 §25), emitted only in an audited
/// build: every top-level `let`'s binding released at its fixed address, in
/// reverse declaration order — the order the bindings would come off a stack.
///
/// It is what makes the instrument's question the PROGRAM's. Module state
/// outlives `main` by design (RFC-0013), so without a teardown every global
/// that owns heap is residue and the ratchet would measure the language's own
/// rule instead of a defect. A binding whose value is a data-segment literal
/// releases nothing: `free` refuses an address below `HEAP_BASE`.
fn lower_globals_teardown(m: &mut Module, program: &Program, cx: &Cx<'_>) -> Result<Frame, String> {
    let mut b = Frame::new(0, &[], 0);
    let mut f = top_level(cx);
    for g in program.globals.iter().rev() {
        let (place, ty) = cx.globals[&g.name].clone();
        if let Some(rel) = f.rel_for(&ty, g.line)? {
            f.emit_rel(m, &mut b, place, &rel, g.line)?;
        }
        frame_fits(&b, &g.name, g.line)?;
    }
    Ok(b)
}

/// Lower one body. `sig` is passed rather than looked up because a specialization
/// has no entry in `Cx::sigs` — it is keyed on its type arguments and its
/// RFC-0023 targets, not on its name, and several instances share one `Function`.
///
/// `binds` is non-empty for an RFC-0023 specialization only, and its keys are the
/// callee's `fn`-typed parameter names — which are NOT in `f.params`, because a
/// specialization's synthesized signature replaces each of them with the capture
/// parameters its target needs.
fn lower_fn(
    m: &mut Module,
    f: &Function,
    sig: &Sig,
    cx: &Cx<'_>,
    binds: HashMap<String, FnBinding>,
) -> Result<(), String> {
    let frame = lower_body(m, f, Body::Block(&f.body), sig, cx, binds)?;
    m.fill(sig.index, frame);
    if std::env::var_os("VYRN_WASM_NAMES").is_some() {
        m.name(sig.index, &f.name);
    }
    Ok(())
}

/// The body itself, before it is installed at the index reserved for it.
///
/// `f` is the DECLARATION — the name, the line and the signature — and `body` is
/// the statements. They are two arguments because a specialization's signature is
/// synthesized and its statements are the callee's own, borrowed rather than
/// cloned (see [`Pending::body`]).
fn lower_body(
    m: &mut Module,
    f: &Function,
    body: Body<'_>,
    sig: &Sig,
    cx: &Cx<'_>,
    binds: HashMap<String, FnBinding>,
) -> Result<Frame, String> {
    // A `|x| e` literal has no statements to walk at all; everything else does.
    let stmts = match body {
        Body::Block(b) => Some(b),
        Body::Shell => Some(&f.body),
        Body::Value(_) => None,
    };
    let sig = sig.clone();
    let (params, _results) = cx.wasm_sig(&sig, f.line)?;
    let dest = sig.ret.agg().map(|_| 0u32);
    let shift = dest.map_or(0, |_| 1);
    // A lifted lambda's rows are the enclosing function's (see `f_shell`).
    let owner = f
        .name
        .strip_prefix(LAMBDA)
        .map(|rest| rest.trim_start().to_string())
        .filter(|o| !o.is_empty())
        .unwrap_or_else(|| f.name.clone());

    let mut b = Frame::new(params.len(), &[], 0);
    let mut cx_fn = Fn_ {
        cx,
        scope: Vec::new(),
        depth: 0,
        loops: Vec::new(),
        walks: HashMap::new(),
        trap_site: None,
        ret: sig.ret.clone(),
        // As DECLARED, not resolved. A function returning `Age` has to validate
        // at its `return`, and `Age` resolved to `Int64` is the flow that does
        // not — which is the whole class of silent hole M2d exists to close.
        ret_ty: sig.ret_ty.clone(),
        dest,
        scratch: HashMap::new(),
        rel_slots: HashMap::new(),
        rel_seq: 0,
        rel_pending: Vec::new(),
        // RFC-0101 M4: the order this body releases in, decided once in
        // `own::place_body` and read here.
        placed: cx
            .releases
            .get(&owner)
            .map(|steps| vyrn_frontend::own::placed(steps))
            .unwrap_or_default(),
        cursors: Vec::new(),
        region_depth: 0,
        region_marks: Vec::new(),
        arg_frees: Vec::new(),
        rel_holes: Vec::new(),
        expect: Vec::new(),
        fn_binds: binds,
        // A lambda's bare expression cannot qualify a name: the whitelist is
        // grown by `x = x + ..`, which is a STATEMENT, and there is one
        // expression here.
        append_ok: stmts.map(crate::append_candidates).unwrap_or_default(),
        is_release: cx.owned.is_release_fn(&f.name),
        str_append: HashMap::new(),
        dest_hint: None,
        dest_used: false,
        call_dest: None,
        owner,
        // `VYRN_NO_CORE_WALK=1` takes the AST walk back for the whole body,
        // which is how the two are compared (`compiler/vyrn-cli/tests/coredrive.rs`).
        core: (!core_walk_off())
            .then(|| vyrn_lower::core::body_of(&f.name))
            .flatten()
            .map(std::rc::Rc::new),
        core_at: HashMap::new(),
        core_w: Walked::default(),
    };
    if let Some(core) = cx_fn.core.clone() {
        cx_fn.core_at = core.rows_by_statement();
        cx_fn.core_w = Walked {
            at: vec![None; core.names.len()],
            reads: core.reads(),
            held: None,
        };
    }

    // By-value parameter semantics: an aggregate arrives as the caller's
    // address, so the prologue copies it into a slot of our own. M0 measured
    // that the LLVM emitter already does exactly this (every parameter is stored
    // into a fresh alloca), so the convention costs nothing new.
    //
    // A `modify` parameter (RFC-0004 §1) is call-by-value-**result**: the local
    // holds the caller's address, the value is copied IN here and copied back OUT
    // at the epilogue. Working through the pointer instead would be smaller code
    // and different semantics — the caller would see each write as it happened —
    // and the textual backend already chose copy-in/copy-out, so parity decides
    // this rather than taste.
    let mut copy_out: Vec<(u32, Place, Repr, String)> = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        let local = shift + i as u32;
        // The DECLARED type, for the same reason `ret_ty` is: a binding whose
        // type is `Age` must validate what is assigned to it, and one whose type
        // has already been resolved to `Int64` cannot know to.
        let ty = p.ty.clone();
        let r = cx.repr(&p.ty, f.line)?;
        let place = if p.capability == Capability::Modify {
            let ll = cx.ll(&p.ty);
            let place = match &r {
                Repr::Agg(l) => {
                    let off = b.alloc(l.size, l.align);
                    b.slot(off);
                    b.ins(&Instruction::LocalGet(local));
                    b.ins(&Instruction::I32Const(l.size as i32));
                    b.ins(&Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                    Place::Slot(off)
                }
                Repr::Scalar(v) => {
                    let own = b.local(*v);
                    b.ins(&Instruction::LocalGet(local));
                    b.ins(&load_of(&ll, 0, cx.signed(&p.ty)));
                    b.ins(&Instruction::LocalSet(own));
                    Place::Local(own)
                }
                Repr::Unit => return unsupported("a `modify` parameter of Unit", f.line),
            };
            copy_out.push((local, place, r.clone(), ll));
            place
        } else {
            match &r {
                Repr::Agg(l) => {
                    let off = b.alloc(l.size, l.align);
                    b.slot(off);
                    b.ins(&Instruction::LocalGet(local));
                    b.ins(&Instruction::I32Const(l.size as i32));
                    b.ins(&Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                    Place::Slot(off)
                }
                _ => Place::Local(local),
            }
        };
        // RFC-0114: an owned `consume` parameter the body neither moves nor
        // drops is released at exit — same row, same placement, same key as
        // the textual backend.
        if p.capability == Capability::Consume {
            let key = p as *const vyrn_frontend::ast::Param as usize;
            if cx_fn.releases_whole(key) {
                if let Some(r) = cx_fn.rel_for(&ty, f.line)? {
                    cx_fn.register_rel(&b, key, place, r);
                }
            }
        }
        cx_fn.scope.push((p.name.clone(), place, ty));
    }
    // The core's parameters are the declaration's in order, and the prologue
    // has just put each one where it lives, so the two walks agree about them
    // before either runs.
    if let Some(core) = cx_fn.core.clone() {
        for (i, n) in core.params.iter().enumerate() {
            if let Some((_, place, ty)) = cx_fn.scope.get(i) {
                cx_fn.core_w.at[*n as usize] = Some((*place, ty.clone()));
            }
        }
    }

    // Audit A5.3: one frame of the language's call-depth budget. A lifted lambda
    // is skipped — it has no name to call itself by (RFC-0037), so it cannot
    // recurse without passing through a named function, and counting it here
    // would count a call the interpreter and the textual backend do not.
    // Nor a `std/runtime` function (PLAN-0125-runtime §6 step 1): the
    // hand-emitted copies it replaces had no prologue, and a program that
    // traps at the limit has to trap where it did.
    let counted =
        !f.name.starts_with(LAMBDA) && !f.name.starts_with(vyrn_frontend::loader::RUNTIME_PREFIX);
    if counted {
        call_depth_enter(&mut b, cx);
    }
    // RFC-0125 M1: the trap site. A check that fails parks its trap-table ROW
    // and the value the row names in these two locals and branches OUT of this
    // block; the one call to `trapAt` stands after it. Seven of §2.3's eight
    // rows reach it — the eighth is the prologue's own, which stands before
    // the block and cannot branch into it. Measured on nbody's inner
    // loop under Cranelift: twenty-nine checks each carrying their own call
    // cost 3.56 s against 1.71 s with the compare kept and the call gone —
    // the call site, not the check, was what the engine paid for.
    cx_fn.trap_site = Some((b.local(ValType::I32), b.local(ValType::I64)));
    // The one block every `return` targets. Its result IS the function's when
    // that is a scalar; an aggregate return travels through `dest` instead, so
    // the block carries nothing.
    b.ins(&Instruction::Block(match &sig.ret {
        Repr::Scalar(v) => BlockType::Result(*v),
        _ => BlockType::Empty,
    }));
    // Inside it, the trap block wraps the body: a failed check branches out of
    // THIS block and lands on the trap call below, while a `return` branches
    // out of the function block as it always did — past the call, into the
    // epilogue and the frame's own stack pop. A body must never emit `return`
    // (the M1 note on `Frame::ins`), and this structure is how the trap site
    // obeys that: the first cut returned from inside and leaked the frame.
    b.ins(&Instruction::Block(BlockType::Empty));
    cx_fn.depth += 1;
    // RFC-0125 §2.3: "the emitter reads the core and writes wasm". Where the
    // core's rows carry the whole body, its statements are what this walks;
    // everywhere else the AST dispatch below is what it always was, and it
    // asks the core again at every statement ([`Fn_::core_took`]).
    let from_core = cx_fn
        .core
        .clone()
        .filter(|core| cx_fn.core_walkable(core, stmts));
    WALKS.with(|w| {
        let (from, all) = w.get();
        w.set((from + usize::from(from_core.is_some()), all + 1));
    });
    match (from_core, stmts) {
        (Some(core), _) => cx_fn.core_body(m, &mut b, &core)?,
        (None, Some(blk)) => cx_fn.block(m, &mut b, blk)?,
        (None, None) => match body {
            Body::Value(e) => cx_fn.lambda_value(m, &mut b, e)?,
            _ => unreachable!("only a lambda's expression has no statements"),
        },
    }
    // A lowering that reaches an argument node outside [`Fn_::call`] would leave
    // its local here. Nothing in the corpus does; if anything ever did, the local
    // is simply never read, which is a leak rather than a free of a value still
    // in use — the direction every release decision in this compiler takes.
    cx_fn.arg_frees.clear();
    // Falling off the end of a value-returning function is unreachable — the
    // checker proves every path returns — but the validator needs to be told,
    // since it cannot see the proof. A unit or aggregate body that falls off
    // its end leaves the trap block the way a `return` does, over the call.
    if matches!(sig.ret, Repr::Scalar(_)) {
        b.ins(&Instruction::Unreachable);
    } else {
        b.ins(&Instruction::Br(cx_fn.depth));
    }
    b.ins(&Instruction::End);
    cx_fn.depth -= 1;
    if let Some((trule, tval)) = cx_fn.trap_site {
        b.ins(&Instruction::LocalGet(trule));
        b.ins(&Instruction::LocalGet(tval));
        b.ins(&Instruction::I32Const(cx.rt.trap_table as i32));
        b.ins(&Instruction::Call(cx.rt.trap_at));
    }
    // The helper exits the process; the validator is told so.
    b.ins(&Instruction::Unreachable);
    b.ins(&Instruction::End);

    // The copy-out, once, AFTER the block every `return` branches to — which is
    // why M1's no-`return`-in-a-body rule pays for itself a second time here. A
    // backend that emitted a real `return` would need this at every exit; there
    // is only one exit, so there is only one copy. The instructions are
    // stack-neutral, so a scalar result already sitting on the stack (the block's
    // own value) survives them untouched, the same property M2d needed for a
    // validation.
    for (arg, place, r, ll) in &copy_out {
        match (place, r) {
            (Place::Local(own), _) => {
                b.ins(&Instruction::LocalGet(*arg));
                b.ins(&Instruction::LocalGet(*own));
                b.ins(&store_of(ll));
            }
            (Place::Slot(off), Repr::Agg(l)) => {
                b.ins(&Instruction::LocalGet(*arg));
                b.slot(*off);
                b.ins(&Instruction::I32Const(l.size as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }
            _ => return unsupported("a `modify` parameter of this shape", f.line),
        }
    }

    // Give the frame back, at the one exit the copy-out just proved this backend
    // has. Stack-neutral, so a scalar result already on the operand stack rides
    // through it untouched — the same property the copy-out needs.
    if counted {
        call_depth_bump(&mut b, cx, -1);
    }

    // RFC-0125 M1: `VYRN_FRAME_TRACE=1` prints every body's frame, refused or
    // not, so the biggest frames of a program can be read off stderr.
    if std::env::var_os("VYRN_FRAME_TRACE").is_some() {
        eprintln!("frame	{}	{}", b.bytes(), f.name);
    }
    frame_fits(&b, &f.name, f.line)?;
    Ok(b)
}

/// Refuse a frame this backend's stack cannot hold at every depth the call
/// counter admits.
///
/// The comparison the backend never made, against two numbers it already owned.
/// A body whose locals came to more than the whole stack used to compile in a
/// tenth of a second into a module that trapped `out of bounds memory access` at
/// a wild address on its first statement. A body far under it was no safer: a
/// 256-byte frame reached the same trap at depth 256, while the other two
/// engines ran the same program to 1,000 and stopped with
/// `error: call depth exceeds 1000`. Both are one missing comparison, so both
/// are this one — against [`vyrn_frontend::trap::FRAME_LIMIT`], which is the
/// stack divided by the depth every engine allows.
///
/// Naming the function and its line is the point: the size is the sum of its
/// locals, and the author is the one who can make them smaller.
///
/// The wording follows [`crate::check_inst_depth`], the other refusal a backend
/// makes about a program the checker let through — the message names what is too
/// big, and a note names the declaration.
fn frame_fits(b: &Frame, name: &str, line: usize) -> Result<(), String> {
    let limit = vyrn_frontend::trap::FRAME_LIMIT;
    if b.bytes() <= limit {
        return Ok(());
    }
    Err(format!(
        "`{name}` needs {} bytes of stack for one call, {} of {limit}\n  \
         note: `{name}` is declared on line {line}, and the shadow stack holds {limit} bytes \
         for each of the {} calls a program may have in flight\n  \
         note: the size is the sum of this function's aggregate locals; a big one belongs on \
         the heap — an `Array<T>` rather than a fixed `Array<T, N>` or a record of records",
        b.bytes(),
        crate::FRAME_LIMIT_NEEDLE,
        vyrn_frontend::trap::CALL_DEPTH_LIMIT,
    ))
}

/// Take one call frame, or trap.
///
/// This is an instruction sequence, not a runtime function, and
/// PLAN-0125-runtime §6 step 9 priced the alternative before leaving it here:
/// with `enter` and `leave` a `std/runtime` pair called from every counted
/// prologue and epilogue, nbody at 25 M steps went from 2.155 s to 2.306 s
/// under wasmtime 46 and fannkuch at n = 11 from 3.599 s to 4.014 s, medians
/// of five, base and head interleaved — 7 and 12 percent for two calls per
/// user call, which is step 5's four nanoseconds on the path every program
/// takes. The counter is a load, a compare and a store at the one site that
/// has the frame in hand, so it stays here (RFC-0125 §3 M4).
fn call_depth_enter(b: &mut Frame, cx: &Cx<'_>) {
    let at = cx.rt.call_depth;
    // The one trap-table row that cannot go through the function's trap site:
    // the prologue stands BEFORE the block a check branches out to, so it
    // calls `trapAt` itself (RFC-0125 §2.3, and M1's structure).
    b.ins(&Instruction::I32Const(at as i32))
        .ins(&Instruction::I32Load(word()))
        .ins(&Instruction::I32Const(
            vyrn_frontend::trap::CALL_DEPTH_LIMIT as i32,
        ))
        .ins(&Instruction::I32GeU)
        .ins(&Instruction::If(BlockType::Empty))
        .ins(&Instruction::I32Const(
            vyrn_frontend::trap::Rule::CallDepth.index() as i32,
        ))
        .ins(&Instruction::I64Const(0))
        .ins(&Instruction::I32Const(cx.rt.trap_table as i32))
        .ins(&Instruction::Call(cx.rt.trap_at))
        .ins(&Instruction::End);
    call_depth_bump(b, cx, 1);
}

fn call_depth_bump(b: &mut Frame, cx: &Cx<'_>, by: i32) {
    let at = cx.rt.call_depth;
    b.ins(&Instruction::I32Const(at as i32))
        .ins(&Instruction::I32Const(at as i32))
        .ins(&Instruction::I32Load(word()))
        .ins(&Instruction::I32Const(by))
        .ins(&Instruction::I32Add)
        .ins(&Instruction::I32Store(word()));
}

/// One signature's dispatcher (RFC-0037): switch on the tag, unpack the
/// variant's capture block, and DIRECT-call the target.
///
/// This is the body that cannot be written when its index is handed out — a
/// construction anywhere in the module adds a variant — which is the whole reason
/// `wasm::Module::reserve_func` exists.
///
/// A chain of nested `if`/`else` rather than a `block` and a `br_table`: the
/// innermost `else` is the defensive arm, and its `unreachable` is what satisfies a
/// result-typed chain without any arm having to branch out. That also keeps M1's
/// rule (no `return` in a body) true by construction.
/// The module's one derived copy over the defunctionalized enum (RFC-0037 ×
/// RFC-0089 rule 4, Phase 10b, census §16): `(tag, block) -> block`.
///
/// `x.copy()` of a stored `fn` value has to duplicate the capture block, and the
/// block's size is a property of the TAG, chosen at run time. Nothing at the
/// copy site can measure it. The defunctionalizer chose those tags and holds
/// every one's capture types, so the size comes off the registry here — a chain
/// of tag tests, then one `malloc` and one `memory.copy`.
///
/// The copy is **shallow**: the block, not what the captures point at. Two
/// lambdas over one String already build two blocks holding one pointer, so a
/// deep copy would need a deep release to match and the release would then free
/// that pointer twice. The two stay mirrors, both shallow.
fn lower_fnval_copy(cx: &Cx<'_>) -> Result<Frame, String> {
    let mut b = Frame::new(2, &[], 0);
    let f = top_level(cx);
    let (tag, pay) = (0u32, 1u32);
    let vals = cx.fnvals.borrow().clone();
    for (i, v) in vals.iter().enumerate() {
        let cap_tys = &v.target.sig.params[..v.target.ncaps];
        // No captures means payload 0, and 0 copies to itself.
        if cap_tys.is_empty() {
            continue;
        }
        let size = f.cap_block(cap_tys)?.size;
        b.ins(&Instruction::LocalGet(tag));
        b.ins(&Instruction::I64Const(i as i64));
        b.ins(&Instruction::I64Eq);
        b.ins(&Instruction::If(BlockType::Empty));
        let dst = b.local(ValType::I32);
        b.ins(&Instruction::I64Const(size as i64));
        b.ins(&Instruction::Call(cx.rt.malloc));
        b.ins(&Instruction::LocalTee(dst));
        b.ins(&Instruction::LocalGet(pay));
        b.ins(&Instruction::I32Const(size as i32));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        b.ins(&Instruction::LocalGet(dst));
        b.ins(&Instruction::Return);
        b.ins(&Instruction::End);
    }
    // Every other tag has no block to copy: the payload is 0 and the copy is the
    // value, exactly as a scalar's is.
    b.ins(&Instruction::LocalGet(pay));
    Ok(b)
}

/// One shape body: `(addr) -> ()`, releasing or copying the value at `addr`.
///
/// The parameter IS the address, which is what an aggregate in a wasm local
/// already is — so the body is the walk it used to be inline, reading local 0
/// where the site read the local it had made. Every place the walk reaches is
/// under that address, so the function needs no frame of its own; a field with
/// a DECLARED release is the one thing that does, and it takes it exactly as any
/// other call site does.
fn lower_shape(
    m: &mut Module,
    cx: &Cx<'_>,
    rel: bool,
    ty: &Type,
    holes: &[String],
    line: usize,
) -> Result<Frame, String> {
    let mut b = Frame::new(1, &[], 0);
    let mut f = top_level(cx);
    if rel {
        f.rel_body(m, &mut b, 0, ty, holes, line)?;
    } else {
        f.copy_body(m, &mut b, 0, ty, line)?;
    }
    Ok(b)
}

fn lower_dispatcher(
    m: &mut Module,
    cx: &Cx<'_>,
    sig_ty: &Type,
    dsig: &Sig,
) -> Result<Frame, String> {
    let Type::Fn(ptys, ret) = sig_ty else {
        return unsupported("a dispatcher for a non-function type", 0);
    };
    let (params, _) = cx.wasm_sig(dsig, 0)?;
    let mut b = Frame::new(params.len(), &[], 0);
    let mut f = top_level(cx);

    // param 0 is the aggregate-return destination when there is one, then the fn
    // value's address, then the signature's own parameters.
    let dest = dsig.ret.agg().map(|_| 0u32);
    let shift = u32::from(dest.is_some());
    let fv = shift;
    for (i, pty) in ptys.iter().enumerate() {
        let local = shift + 1 + i as u32;
        // An aggregate parameter arrives as an address in a wasm local, which is
        // the one thing `Place` cannot name (M2f) — so it is copied into a slot of
        // its own exactly as `lower_body`'s prologue does, which is also the
        // by-value copy the convention owes.
        let place = match cx.repr(pty, 0)? {
            Repr::Agg(l) => {
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                b.ins(&Instruction::LocalGet(local));
                b.ins(&Instruction::I32Const(l.size as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                Place::Slot(off)
            }
            Repr::Scalar(_) => Place::Local(local),
            Repr::Unit => return unsupported("a Unit parameter of a stored `fn`", 0),
        };
        f.scope.push((format!("@a{i}"), place, pty.clone()));
    }
    let args: Vec<Expr> = (0..ptys.len())
        .map(|i| Expr::Var {
            name: format!("@a{i}"),
            line: 0,
        })
        .collect();

    let fl = layout::of_ll(&cx.ll(sig_ty)).map_err(|e| format!("direct backend: {e}"))?;
    let tag = b.local(ValType::I64);
    let pl = b.local(ValType::I32);
    b.ins(&Instruction::LocalGet(fv));
    b.ins(&Instruction::I64Load(at(fl.fields[0])));
    b.ins(&Instruction::LocalSet(tag));
    b.ins(&Instruction::LocalGet(fv));
    b.ins(&Instruction::I64Load(at(fl.fields[1])));
    b.ins(&Instruction::I32WrapI64);
    b.ins(&Instruction::LocalSet(pl));

    let variants: Vec<(usize, FnVal)> = cx
        .fnvals
        .borrow()
        .iter()
        .enumerate()
        .filter(|(_, v)| v.sig == *sig_ty)
        .map(|(i, v)| (i, v.clone()))
        .collect();
    let arm_ty = match &dsig.ret {
        Repr::Scalar(v) => BlockType::Result(*v),
        _ => BlockType::Empty,
    };
    for (i, v) in &variants {
        b.ins(&Instruction::LocalGet(tag));
        b.ins(&Instruction::I64Const(*i as i64));
        b.ins(&Instruction::I64Eq);
        b.ins(&Instruction::If(arm_ty));
        f.depth += 1;
        let mark = f.scope.len();
        // The capture block, copied off the heap into a frame slot so each capture
        // has a `Place`. The copy is what the textual backend's `load {block_ll}`
        // is, and it is also the by-value read a capture is.
        let cap_tys = v.target.sig.params[..v.target.ncaps].to_vec();
        let mut all: Vec<Expr> = Vec::new();
        if !cap_tys.is_empty() {
            let bl = f.cap_block(&cap_tys)?;
            let blk = b.alloc(bl.size, bl.align);
            b.slot(blk);
            b.ins(&Instruction::LocalGet(pl));
            b.ins(&Instruction::I32Const(bl.size as i32));
            b.ins(&Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });
            for (ci, ct) in cap_tys.iter().enumerate() {
                let at_off = blk + bl.fields[ci];
                let place = match cx.repr(ct, 0)? {
                    Repr::Scalar(vt) => {
                        let loc = b.local(vt);
                        b.slot(at_off);
                        b.ins(&load_of(&cx.ll(ct), 0, cx.signed(ct)));
                        b.ins(&Instruction::LocalSet(loc));
                        Place::Local(loc)
                    }
                    Repr::Agg(_) => Place::Slot(at_off),
                    Repr::Unit => return unsupported("a captured Unit value", 0),
                };
                f.scope.push((format!("@c{ci}"), place, ct.clone()));
                all.push(Expr::Var {
                    name: format!("@c{ci}"),
                    line: 0,
                });
            }
        }
        all.extend(args.iter().cloned());
        // An aggregate result is written through OUR destination, so it goes on the
        // stack under the call — M2d's "a value may sit beneath an `if`" applied to
        // the call rather than to a check.
        if let Some(d) = dest {
            b.ins(&Instruction::LocalGet(d));
        }
        let got = f.emit_call(m, &mut b, &v.target.sig, &all, None)?;
        match (&dsig.ret, cx.repr(&got, 0)?) {
            // The target's declared result may differ from the signature's — a
            // named source's validated scalar, a wider record — so it crosses the
            // M2d seam like any other flow.
            (Repr::Scalar(_), _) => f.coerce(m, &mut b, None, &got, ret, 0)?,
            (Repr::Agg(l), Repr::Agg(_)) => {
                f.coerce(m, &mut b, None, &got, ret, 0)?;
                b.ins(&Instruction::I32Const(l.size as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }
            // A Unit-signature slot may hold a value-returning function: the
            // result is discarded, exactly as a Unit-returning lambda's is.
            (Repr::Unit, Repr::Scalar(_) | Repr::Agg(_)) => {
                b.ins(&Instruction::Drop);
            }
            (Repr::Unit, Repr::Unit) => {}
            _ => return unsupported("a stored `fn` whose result shape is not its signature's", 0),
        }
        f.scope.truncate(mark);
        b.ins(&Instruction::Else);
    }
    // Unreachable by construction — a tag only ever comes from a registered
    // construction — so this is the defensive arm, with the wording the textual
    // backend's `@.fnval.bad` carries.
    let msg = cx.rt.intern(
        m,
        &vyrn_frontend::trap::line(vyrn_frontend::trap::BAD_FN_VALUE),
    );
    b.ins(&Instruction::I32Const(msg as i32));
    b.ins(&Instruction::Call(cx.rt.trap));
    b.ins(&Instruction::Unreachable);
    for _ in &variants {
        f.depth -= 1;
        b.ins(&Instruction::End);
    }
    Ok(b)
}

impl<'p> Fn_<'_, 'p> {
    /// Scratch local `n` of type `t`, taken on first use.
    ///
    /// Reusable because every use is a set immediately followed by the reads
    /// that consume it — a nested expression evaluates to completion before the
    /// outer one touches scratch, and anything already on the operand stack is
    /// untouched by a local.
    fn scratch(&mut self, b: &mut Frame, t: ValType, n: u8) -> u32 {
        *self.scratch.entry((t, n)).or_insert_with(|| b.local(t))
    }

    /// Keep the `String` now on top of the stack, if the expression that made it
    /// ALLOCATED it (RFC-0096 M3). `None` means there is nothing to release.
    ///
    /// A fresh local rather than [`Fn_::scratch`], and the reason is the shape
    /// of the thing being kept: the interpolation spine folds left, so
    /// `"a\{x}b\{y}c\{z}"` is nested `@concat`s as deep as it has holes, and
    /// every level holds its left half across the lowering of its right one. A
    /// numbered scratch slot would be clobbered by the level below it; the
    /// scratch doc says so itself — "a nested expression evaluates to completion
    /// before the outer one touches scratch" is exactly what is false here.
    fn tee_str_temp(&mut self, b: &mut Frame, e: &Expr) -> Option<u32> {
        if !vyrn_frontend::declared::str_temporary(e) {
            return None;
        }
        let l = b.local(ValType::I32);
        b.ins(&Instruction::LocalTee(l));
        Some(l)
    }

    /// Hand back what [`Fn_::tee_str_temp`] kept, after the concatenation has
    /// copied out of it.
    ///
    /// The concatenation's own result is on the stack and stays there: a local
    /// read and a call push and pop above it. `free` refuses anything below
    /// `HEAP_BASE`, so this is a no-op on a data-segment literal for the same
    /// reason `drop s` is.
    fn free_str_temp(&mut self, b: &mut Frame, kept: Option<u32>) {
        let Some(l) = kept else { return };
        b.ins(&Instruction::LocalGet(l));
        str_hdr(b);
        b.ins(&Instruction::Call(self.cx.rt.free));
    }

    /// The VALUE half of a `return`: the expression, coerced to the DECLARED
    /// return type, written through `dest` when the function returns an
    /// aggregate. The unwinding half stays at the statement, which is the only
    /// place that has a node to look a placement up by.
    ///
    /// Split out because a `|x| e` lambda returns `e` without there being a
    /// `Stmt::Return` anywhere to say so — writing one would mean owning a copy
    /// of `e` (see [`Body::Value`]).
    fn ret_value(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        e: &Expr,
        line: usize,
    ) -> Result<(), String> {
        match self.ret.clone() {
            Repr::Scalar(_) => {
                let want = self.ret_ty.clone();
                self.expr_as(m, b, e, &want)?;
            }
            Repr::Agg(l) => {
                // Destination-first, at the function's own boundary: the
                // caller's slot address is already in `dest`. The caller
                // handed over storage nothing names (its own fresh slot, or
                // storage IT was handed under the same rule), so a literal
                // is built in it directly (RFC-0125 M1).
                let want = self.ret_ty.clone();
                let dest = Dest::Addr(self.dest.unwrap(), 0);
                self.agg_into(m, b, dest, l.size, e, &want, true)?;
            }
            Repr::Unit => {
                return unsupported("a return whose value does not match the signature", line);
            }
        }
        Ok(())
    }

    /// A `|x| e` literal's whole body: the `return e` the block form writes by
    /// hand, emitted without a statement to write it with (RFC-0101 M6).
    ///
    /// A Unit-returning signature is the exception, and not a cosmetic one:
    /// `each(xs, |x| print(x))` has an expression body whose value the signature
    /// does not carry, so it is a statement rather than a return. The textual
    /// emitter reaches the same split by testing `llt(ret) == "void"`.
    ///
    /// What [`Stmt::Return`]'s arm does between the value and the branch is
    /// nothing here, twice over: `own` places no release step inside a lambda
    /// body (M4's phase-1 finding, still counted by the corpus gate) and the
    /// shell has no name in [`Cx::releases`] to look one up under; and a body
    /// starts outside every region.
    fn lambda_value(&mut self, m: &mut Module, b: &mut Frame, e: &Expr) -> Result<(), String> {
        if matches!(self.ret, Repr::Unit) {
            // A call for its effect leaves its result on the stack; drop it, or
            // the block's type will not check — [`Stmt::Expr`]'s arm.
            if !matches!(
                self.cx.repr(&self.expr(m, b, e)?, Expr::line(e))?,
                Repr::Unit
            ) {
                b.ins(&Instruction::Drop);
            }
            return Ok(());
        }
        self.ret_value(m, b, e, Expr::line(e))?;
        self.exit_regions_above(b, 0, false);
        b.ins(&Instruction::Br(self.depth));
        Ok(())
    }

    fn block(&mut self, m: &mut Module, b: &mut Frame, blk: &Block) -> Result<(), String> {
        let mark = self.scope.len();
        let mut k = 0;
        while k < blk.stmts.len() {
            // RFC-0125 M1: a statement's temporaries are its own. A statement
            // that bound nothing at this level gives its slots back (the rule
            // on `Frame::alloc`); one that did — a `let`, a refutable `let` —
            // keeps everything it took, binding and temporaries alike, because
            // the cheap test is the scope's length and not which slot is which.
            //
            // A NAME is not the only thing that outlives a statement:
            // [`Fn_::rel_pending`] is the floor the release rows that still
            // name a slot raise, and the reset stops there.
            let (frame, scope) = (b.mark(), self.scope.len());
            if let Some(n) = self.elem_field_store(m, b, &blk.stmts[k..])? {
                k += n;
                b.reset(frame.max(self.rel_floor()));
                continue;
            }
            self.stmt(m, b, &blk.stmts[k])?;
            if self.scope.len() == scope {
                b.reset(frame.max(self.rel_floor()));
            }
            k += 1;
        }
        // The fall-through exit. An early `return`/`break`/`continue` releases the
        // same frames before its branch, so this runs after a branch only in code
        // wasm has already marked unreachable.
        self.emit_releases(m, b, ExitKind::Block, blk as *const Block as usize)?;
        self.scope.truncate(mark);
        Ok(())
    }

    /// `a[i].f = v` as ONE store through the element's address — RFC-0125 M1.
    ///
    /// The parser hands every engine the RFC-0082 idiom for this statement:
    /// `let mut a[] = @at(a, a[]idx)`, then `a[].f = v`, then `a[a[]idx] = a[]`.
    /// That is a copy of the whole element out, one field store, and a copy
    /// back — two `memory.copy` per field write for a store of one scalar. In
    /// nbody's inner loop it is 21 copies per iteration, and it is why the same
    /// program runs 13x slower under Cranelift than under LLVM: LLVM's scalar
    /// replacement deletes the copies and a wasm engine keeps them (§1.4).
    ///
    /// The three statements are recognised here by the unspellable temp and
    /// lowered as a bounds check, an address and a store — exactly what
    /// `Stmt::IndexSet` does for `a[i] = v`, plus a field offset. HEAPLESS
    /// elements only: with nothing to release, the skipped `let` has no placed
    /// release to leave behind, and the old field value owes none either. An
    /// element that holds heap keeps the idiom, whose releases the placement
    /// already accounted for.
    ///
    /// Returns how many statements were consumed, or `None` when the window is
    /// not the idiom and the caller lowers the statement as it always did.
    fn elem_field_store(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        stmts: &[Stmt],
    ) -> Result<Option<usize>, String> {
        let [Stmt::Let {
            name: tmp,
            mutable: true,
            value: load,
            ..
        }, Stmt::SetField {
            name: t2,
            field,
            value,
            line,
        }, Stmt::IndexSet {
            name: parent,
            index: Expr::Var { name: idx2, .. },
            value: Expr::Var { name: t3, .. },
            ..
        }, ..] = stmts
        else {
            return Ok(None);
        };
        if !vyrn_frontend::ast::is_place_temp(tmp) || t2 != tmp || t3 != tmp {
            return Ok(None);
        }
        let Expr::Call { name: at, args, .. } = load else {
            return Ok(None);
        };
        if at != "@at" || args.len() != 2 {
            return Ok(None);
        }
        let (Expr::Var { name: p2, .. }, Expr::Var { name: idx, .. }) = (&args[0], &args[1]) else {
            return Ok(None);
        };
        if p2 != parent || idx != idx2 {
            return Ok(None);
        }
        let (place, ty) = self.lookup(parent, *line)?;
        let elem = match self.cx.resolve(&ty) {
            Type::Array(i) | Type::ArrayN(i, _) | Type::SmallArray(i, _) => *i,
            _ => return Ok(None),
        };
        if self.rel_for(&elem, *line)?.is_some() {
            return Ok(None);
        }
        let (foff, fty) = self.field_of(&elem, field, *line)?;
        let fr = self.cx.repr(&fty, *line)?;
        if matches!(fr, Repr::Unit) || matches!(place, Place::Local(_)) {
            return Ok(None);
        }
        // A heapless element owes no release, so nothing is emitted for the
        // store decision on the idiom's own statements.
        // From here on, code is emitted: the same prefix as `Stmt::IndexSet`.
        let w = match self.walks.get(parent.as_str()).cloned() {
            Some(w) => w,
            None => {
                place.addr(b, 0);
                self.walk(b, &ty, *line)?
            }
        };
        self.expr_as(m, b, &args[1], &Type::Int)?;
        let i = b.local(ValType::I64);
        b.ins(&Instruction::LocalSet(i));
        self.bounds_check(b, &w, i, false);
        self.elem_addr(b, &w, i);
        if foff != 0 {
            b.ins(&Instruction::I32Const(foff as i32));
            b.ins(&Instruction::I32Add);
        }
        match &fr {
            Repr::Scalar(_) => {
                self.expr_as(m, b, value, &fty)?;
                b.ins(&store_of(&self.cx.ll(&fty)));
            }
            Repr::Agg(l) => {
                self.expr_as(m, b, value, &fty)?;
                b.ins(&Instruction::I32Const(l.size as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }
            Repr::Unit => unreachable!("refused above"),
        }
        Ok(Some(3))
    }

    /// The hoisted header of `e`, when `e` names a binding a `while` hoisted.
    fn cached_walk(&self, e: &Expr) -> Option<Walk> {
        match e {
            Expr::Var { name, .. } => self.walks.get(name.as_str()).cloned(),
            _ => None,
        }
    }

    /// The read half of RFC-0125 M1. Before a `while`, take apart every
    /// array, fixed array, small array or String this frame binds that the
    /// loop indexes and never moves, so the body reads `data` and `len` from
    /// locals instead of reloading the header at every access.
    ///
    /// `walk` reloads because a store into linear memory may alias the
    /// header's slot and no wasm engine can prove it does not — nbody's inner
    /// loop paid 29 reloads per iteration for that (§1.4). The proof is made
    /// here instead, on the syntax, and it is conservative: `header_invariant`
    /// refuses the hoist on anything that could move the header. Module state
    /// is never hoisted, because a callee can grow it. An element store moves
    /// no header, so `a[i] = v` and the field-store idiom keep the hoist.
    ///
    /// Returns what each hoisted name held before, for `While` to put back.
    fn hoist_walks(
        &mut self,
        b: &mut Frame,
        cond: &Expr,
        body: &Block,
        line: usize,
    ) -> Result<Vec<(String, Option<Walk>)>, String> {
        let mut out = Vec::new();
        for name in indexed_names(cond, body) {
            if self.walks.contains_key(&name) {
                continue;
            }
            let Some((place, ty)) = self
                .scope
                .iter()
                .rev()
                .find(|(n, _, _)| *n == name)
                .map(|(_, p, t)| (*p, t.clone()))
            else {
                continue;
            };
            if matches!(place, Place::Static(_)) {
                continue;
            }
            if !matches!(
                self.cx.resolve(&ty),
                Type::Array(_) | Type::ArrayN(..) | Type::SmallArray(..) | Type::Str
            ) {
                continue;
            }
            if !header_invariant(cond, body, &name) {
                continue;
            }
            // The binding's value, the way `Expr::Var` leaves it — a local's
            // value or a slot's address — emitted here rather than through a
            // synthesized `Var` node: the lowered-form gate counts backend
            // answers about nodes no instantiation holds, and a node made up
            // here would be one.
            match place {
                Place::Local(l) => {
                    b.ins(&Instruction::LocalGet(l));
                }
                Place::Slot(off) => {
                    b.slot(off);
                }
                Place::Static(_) => continue,
            }
            let w = self.walk(b, &ty, line)?;
            out.push((name.clone(), self.walks.insert(name, w)));
        }
        Ok(out)
    }

    /// Emit the releases the lowering PLACED at one exit — RFC-0101 M4.
    ///
    /// **This is the whole of the consumption.** What it replaced was a walk
    /// over `self.releases[boundary..]`, from an index this engine derived for
    /// itself, asserting an order the other two engines asserted separately
    /// (§1.4). The order is `own::place_body`'s now; this is a lookup and an
    /// encode.
    ///
    /// Nothing is popped, and that is still what makes an early exit safe: the
    /// enclosing [`Fn_::block`] still emits its own copy, which lands after the
    /// branch and is therefore unreachable rather than a second release.
    fn emit_releases(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        exit: ExitKind,
        at: usize,
    ) -> Result<(), String> {
        let steps = self.placed.get(&(exit, at)).cloned().unwrap_or_default();
        // Only a function exit reaches a stream cursor — see [`Fn_::cursors`].
        let mut cursors = match exit {
            ExitKind::Return | ExitKind::Try => self.cursors.clone(),
            _ => Vec::new(),
        };
        // A FALL-THROUGH exit ends what the row holds on the path that carries
        // on, so the slot is the next statement's — see [`Fn_::rel_pending`].
        // A `return`, a `?`, a `break` or a `continue` releases on a BRANCH and
        // leaves the fall-through holding the value, so the floor stands.
        if matches!(exit, ExitKind::Block | ExitKind::Scrutinee) {
            self.rel_pending
                .retain(|(k, _)| !steps.iter().any(|(step, _)| step == k));
        }
        let mut run: Vec<(Place, Rel)> = Vec::new();
        for (step, holes) in steps {
            let Some(r) = self.rel_slots.get(&step) else {
                continue;
            };
            let (place, mut rel, seq) = (r.place, r.rel.clone(), r.seq);
            // A row that carries its own hole set walks around exactly
            // those: round fifty-two's pre-take exit walks the WHOLE value
            // (the empty set), and the placer's row walks the rest of what
            // the kernel saw taken at this exit (RFC-0125 M3). The textual
            // backend's twin.
            if let Some(h) = holes {
                if let Rel::Deep(ty, _) = rel {
                    rel = Rel::Deep(ty, h);
                }
            }
            while cursors.last().is_some_and(|(_, _, at)| *at > seq) {
                let (p, elem, _) = cursors.pop().unwrap();
                run.push((p, Rel::Stream(elem)));
            }
            run.push((place, rel));
        }
        for (p, elem, _) in cursors.into_iter().rev() {
            run.push((p, Rel::Stream(elem)));
        }
        for (p, k) in run {
            self.emit_rel(m, b, p, &k, 0)?;
        }
        Ok(())
    }

    /// The floor the rows that still name a frame slot hold — see
    /// [`Fn_::rel_pending`].
    fn rel_floor(&self) -> u32 {
        self.rel_pending
            .iter()
            .map(|(_, at)| *at)
            .max()
            .unwrap_or(0)
    }

    /// Say what one owned binding is released WITH. The placement already said
    /// where and in what order.
    fn register_rel(&mut self, b: &Frame, key: usize, place: Place, rel: Rel) {
        let seq = self.rel_seq;
        self.rel_seq += 1;
        // The row names this slot until the exit the core placed it at, which
        // may be past the statement that made it. So the statement cannot give
        // the slot back: [`Fn_::rel_pending`] states the floor once, here, for
        // every construct that registers one.
        if matches!(place, Place::Slot(_)) {
            self.rel_pending.retain(|(k, _)| *k != key);
            self.rel_pending.push((key, b.mark()));
        }
        self.rel_slots.insert(key, RelSlot { place, rel, seq });
    }

    /// Reclaim one binding, whichever of the four shapes it is.
    fn emit_rel(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        p: Place,
        k: &Rel,
        line: usize,
    ) -> Result<(), String> {
        match k {
            Rel::Stream(elem) => {
                let elem = elem.clone();
                self.stream_release(m, b, p, &elem, line)
            }
            Rel::Str => {
                // A `String` is a scalar, so a `Place::Local` holds the pointer —
                // the opposite of what a local holding an aggregate means, which is
                // why this and `Rel::Buffers` are two arms and not one.
                //
                // The block base is the pointer less its header (RFC-0089 M1a).
                // `free` still refuses anything below `HEAP_BASE`, which is what
                // makes `drop s` on a literal a no-op.
                match p {
                    Place::Local(l) => b.ins(&Instruction::LocalGet(l)),
                    _ => {
                        p.addr(b, 0)
                            .ok_or_else(|| gap("a String with no place", line))?;
                        b.ins(&Instruction::I32Load(word()))
                    }
                };
                str_hdr(b);
                b.ins(&Instruction::Call(self.cx.rt.free));
                Ok(())
            }
            Rel::Buffers(offs) => {
                for &off in offs {
                    match p {
                        Place::Local(a) => b
                            .ins(&Instruction::LocalGet(a))
                            .ins(&Instruction::I32Load(word_at(off))),
                        _ => {
                            p.addr(b, off)
                                .ok_or_else(|| gap("an aggregate with no place", line))?;
                            b.ins(&Instruction::I32Load(word()))
                        }
                    };
                    b.ins(&Instruction::Call(self.cx.rt.free));
                }
                Ok(())
            }
            // The walk is the type, and it needs an address: an aggregate in a
            // wasm local IS its address, and everything else has one.
            Rel::Deep(ty, holes) => {
                let a = self.addr_local(b, p, 0);
                let t = ty.clone();
                // RFC-0093 M2: the places a take gave away. Empty for every
                // binding nothing took from, which is nearly all of them.
                self.rel_holes = holes.clone();
                let r = self.rel_at(m, b, a, &t, line);
                self.rel_holes.clear();
                r
            }
            // The receiver is parked under a reserved name so the ordinary call
            // path finds it as it finds any argument — the same trick `?` uses
            // for `@try` — and the release is then just a call.
            Rel::Call(f, ty) => {
                let mark = self.scope.len();
                self.scope.push(("@rel".to_string(), p, ty.clone()));
                let recv = [Expr::Var {
                    name: "@rel".to_string(),
                    line,
                }];
                let r = self.call(m, b, f, &recv, line);
                self.scope.truncate(mark);
                r?;
                // RFC-0096: the payload boxes are the enum's own storage and the
                // declaration cannot reach them. Only a user enum has any, and
                // only it gets an address taken for one.
                let ty = ty.clone();
                if !matches!(self.cx.resolve(&ty), Type::Enum(_)) {
                    return Ok(());
                }
                let a = self.addr_local(b, p, 0);
                self.free_declared_boxes(b, a, &ty, line)
            }
        }
    }

    /// Free the payload BOXES of an enum whose release the type declared, and
    /// nothing else — RFC-0096.
    ///
    /// A declared `release` takes the enum BY VALUE and gives its payloads back
    /// by name. The BLOCK a wide payload travels in is the enum's own
    /// representation: the match that reads the payload loads out of it, no Vyrn
    /// surface names it, and the structural walk was the only thing that ever
    /// freed it. So a declared release leaked one block per boxed payload per
    /// value — 16 bytes a node over a released tree, small enough to read steady
    /// against 500 calls and plain against 32,000.
    ///
    /// Everything that is not a user enum answers `Ok(())`, which is every other
    /// declared row: a record and a container carry their storage inline or in a
    /// buffer the declaration itself hands back.
    fn free_declared_boxes(
        &mut self,
        b: &mut Frame,
        a: u32,
        ty: &Type,
        line: usize,
    ) -> Result<(), String> {
        let Type::Enum(vs) = self.cx.resolve(ty) else {
            return Ok(());
        };
        let l = self.layout_of(ty, line)?;
        for (tag, var) in vs.iter().enumerate() {
            let mut boxed = Vec::new();
            for (j, pty) in var.payload.clone().iter().enumerate() {
                if self.owns_heap(pty) && matches!(self.word2(pty)?, Word::Boxed) {
                    boxed.push(self.cx.payload_slot(&var.payload, j));
                }
            }
            if boxed.is_empty() {
                continue;
            }
            tag_eq(b, a, tag as i64);
            b.ins(&Instruction::If(BlockType::Empty));
            self.depth += 1;
            for j in boxed {
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[j])));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::Call(self.cx.rt.free));
            }
            self.depth -= 1;
            b.ins(&Instruction::End);
        }
        Ok(())
    }

    /// How a value of `ty` is reclaimed, or `None` for one that owns no heap.
    ///
    /// The single rule both drop paths read — `own`'s automatic block-exit release
    /// and an explicit `drop x` — so the two cannot free different sets. It is the
    /// same set the textual backend's [`crate::Gen::emit_drop`] frees, minus the
    /// `Stream`, which reaches its release through the stream lowering rather than
    /// through `own`.
    /// Whether releasing this `Array<T>` releases its elements too (RFC-0092
    /// M2). `own` decides, and this asks it — the same answer the textual
    /// backend reads, including the stop for a self-referring element.
    fn array_releases_elems(&self, arr: &Type) -> bool {
        matches!(self.cx.owned.release_kind(arr), Some(DropKind::Deep(_)))
    }

    /// Whether `own` gives `ty` a walking release rather than a buffer one.
    /// [`Fn_::array_releases_elems`] under its general name, for the `Map` and
    /// `SmallArray` rows RFC-0092 M3 adds.
    fn deep_row(&self, ty: &Type) -> bool {
        self.array_releases_elems(ty)
    }

    /// The address of a `SmallArray`'s live slots: the inline block while
    /// `cap == N`, the spilled buffer otherwise. The branch [`Fn_::copy_at`]
    /// takes, lifted so the release takes the same one.
    fn sa_base(&mut self, b: &mut Frame, a: u32, ty: &Type, line: usize) -> Result<u32, String> {
        let Type::SmallArray(_, cap_n) = self.cx.resolve(ty) else {
            return Err(gap("a SmallArray base", line));
        };
        let l = self.layout_of(ty, line)?;
        let base = b.local(ValType::I32);
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I32Const(l.fields[3] as i32));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalSet(base));
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I64Load(at(l.fields[1])));
        b.ins(&Instruction::I64Const(cap_n as i64));
        b.ins(&Instruction::I64Ne);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I32Load(word_at(l.fields[2])));
        b.ins(&Instruction::LocalSet(base));
        self.depth -= 1;
        b.ins(&Instruction::End);
        Ok(base)
    }

    fn rel_for(&mut self, ty: &Type, line: usize) -> Result<Option<Rel>, String> {
        // A declared row (RFC-0086 M1) answers before any built-in shape does,
        // and it is keyed by the type's NAME — which resolving away would lose.
        if let Some(DropKind::Release(f, _)) = self.cx.owned.release_kind(ty) {
            return Ok(Some(Rel::Call(f, ty.clone())));
        }
        let t = self.cx.resolve(ty);
        Ok(match &t {
            Type::Str => Some(Rel::Str),
            Type::Stream(i) => Some(Rel::Stream((**i).clone())),
            // An `Array<T>` gives back its buffer, and its ELEMENTS too where
            // `own` says so (RFC-0092 M2, census U4). The question is asked of
            // `own` rather than re-derived from the element, so the guards that
            // answer live in one file — including the one that stops a
            // self-referring element type, whose walk has no bottom. The element
            // walk needs an address, so that answer is `Deep` and the
            // buffer-only one stays a `Buffers`.
            Type::Array(_) | Type::Map(..) | Type::SmallArray(..) if self.deep_row(&t) => {
                Some(Rel::Deep(t.clone(), Vec::new()))
            }
            Type::Array(_) => Some(Rel::Buffers(vec![self.layout_of(&t, line)?.fields[0]])),
            Type::Map(..) => {
                let l = self.layout_of(&t, line)?;
                Some(Rel::Buffers(vec![l.fields[0], l.fields[1], l.fields[4]]))
            }
            // `{ i64 len, i64 cap, ptr data, [N x T] inline }` — field 2, and it is
            // null until the array spills, which is exactly the case `free`
            // refuses. The inline slots need no reclamation.
            Type::SmallArray(..) => Some(Rel::Buffers(vec![self.layout_of(&t, line)?.fields[2]])),
            // Phase 5: an aggregate owns its places. Phase 10b: a stored `fn`
            // value owns its capture block, which is one allocation whatever the
            // tag — so it needs no registry to release, only to copy.
            //
            // RFC-0092 M3 adds the record, the user enum and the fixed
            // `[N x T]`. Whether they go is `own`'s answer, asked rather than
            // re-derived: it carries the stop for a type that reaches itself,
            // whose walk has no bottom.
            Type::Fn(..) if self.owns_heap(&t) => Some(Rel::Deep(t, Vec::new())),
            Type::Record(_) | Type::Enum(_) | Type::ArrayN(..)
                if matches!(self.cx.owned.release_kind(ty), Some(DropKind::Deep(_))) =>
            {
                Some(Rel::Deep(t, Vec::new()))
            }
            _ => None,
        })
    }

    /// The index of the release (or the copy) of `ty`, reserving the function
    /// and putting its body on the worklist the first time one is asked for.
    ///
    /// The type is SUBSTITUTED here rather than at the body, because the body is
    /// written later — after the drain has moved on to another instance — and a
    /// walk over `T` would then read a different `T` than the site meant.
    fn shape_fn(&self, m: &mut Module, rel: bool, ty: &Type, line: usize) -> u32 {
        let key: ShapeKey = (
            rel,
            self.cx.sub(ty),
            if rel {
                self.rel_holes.clone()
            } else {
                Vec::new()
            },
        );
        let mut s = self.cx.shapes.borrow_mut();
        if let Some((_, i)) = s.known.iter().find(|(k, _)| *k == key) {
            return *i;
        }
        let index = m.reserve_func(&[ValType::I32], &[]);
        s.known.push((key.clone(), index));
        s.todo.push((index, key, line));
        index
    }

    /// Release the heap the value at `a` holds: RFC-0125 §2.3's "`drop` to a
    /// call", which is what this emits — the walk itself is the body of the
    /// function the call names ([`Fn_::rel_body`]), written once per type.
    ///
    /// Two answers stay here, because neither is a call. A type that owns no
    /// heap releases nothing. A type that DECLARED its release is already a
    /// call, and one indirection is enough.
    fn rel_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        a: u32,
        ty: &Type,
        line: usize,
    ) -> Result<(), String> {
        // A type that declares its own release keeps it, so the walk CALLS that
        // release rather than reaching past the declaration into its fields —
        // which would reclaim what the declaration says it reclaims, in a
        // different order, and without the print a user `release` may do.
        //
        // It used to return here and call nothing, which was right at the top of
        // a drop (`emit_rel` has its own `Rel::Call` arm) and wrong for every
        // place under one. An aggregate in a wasm local IS its address, so the
        // place a walk holds is exactly what that arm parks under `@rel`.
        // RFC-0092 M4 is where the gap is observable: a container carries its
        // element's obligation now, so the compiler demands a discharge the
        // discharge did not perform.
        if let Some(DropKind::Release(f, _)) = self.cx.owned.release_kind(ty) {
            self.rel_holes.clear();
            // `emit_rel`'s `Rel::Call` arm frees the payload boxes after the
            // call (RFC-0096), so this reaches them too.
            return self.emit_rel(m, b, Place::Local(a), &Rel::Call(f, ty.clone()), line);
        }
        if !self.owns_heap(ty) {
            self.rel_holes.clear();
            return Ok(());
        }
        let f = self.shape_fn(m, true, ty, line);
        // RFC-0093 M2: the holes belong to the place this call is looking at.
        // They are part of the key above, so the body is walked around exactly
        // these, and every other reader starts empty — which is right: `own`
        // refuses a hole under an element, a payload or a buffer.
        self.rel_holes.clear();
        b.ins(&Instruction::LocalGet(a)).ins(&Instruction::Call(f));
        Ok(())
    }

    /// What releasing a value of `ty` MEANS — the mirror of [`Fn_::copy_body`],
    /// with `free` where that has `malloc`.
    ///
    /// One walk, both directions: `copy` decided what a value's own storage IS,
    /// and a release of that value gives exactly that storage back. Writing the
    /// two as one shape is what keeps them from disagreeing about a boxed enum
    /// payload, which is the encoding Phase 3 measured and the one a hand-written
    /// release gets wrong.
    ///
    /// It releases an `Array<T>`'s ELEMENTS since RFC-0092 M2 — census U4 — each
    /// the way that element's own type is released, so an element with no row of
    /// its own is left alone. `m.keys()` and `sa.toArray()`, which used to hand
    /// back a buffer of somebody else's element words, copy them now. A `Map`
    /// and a `SmallArray` still give back their buffers alone: their element
    /// rows are M3.
    fn rel_body(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        a: u32,
        ty: &Type,
        holes: &[String],
        line: usize,
    ) -> Result<(), String> {
        match self.cx.resolve(ty) {
            // This arm asks nothing about the region: the ownership test is the
            // block header and `free` states it once (an arena block carries a
            // class word of 0 and is refused in silence), so the walk hands
            // back every block it holds and the arena keeps the ones that are
            // its. Standing aside here instead claimed for the arena every
            // `String` a CALLEE minted at this depth, which the arena never
            // had — `examples/matchown.vyrn` leaked four blocks a turn.
            Type::Str => {
                b.ins(&Instruction::LocalGet(a))
                    .ins(&Instruction::I32Load(word()));
                str_hdr(b);
                b.ins(&Instruction::Call(self.cx.rt.free));
                Ok(())
            }
            // The elements first, then the buffer they live in — the reverse of
            // the order `copy_at` builds them, and the only order in which the
            // walk may still read the buffer it is about to free.
            Type::Array(inner) if self.array_releases_elems(&self.cx.resolve(ty)) => {
                let l = self.layout_of(ty, line)?;
                let stride = self.stride(&inner, line)?;
                let (n, data) = (b.local(ValType::I32), b.local(ValType::I32));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[1])));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::LocalSet(n));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I32Load(word_at(l.fields[0])));
                b.ins(&Instruction::LocalSet(data));
                self.rel_each(m, b, data, n, stride, &inner, line)?;
                b.ins(&Instruction::LocalGet(data));
                b.ins(&Instruction::Call(self.cx.rt.free));
                Ok(())
            }
            // A `SmallArray<T, N>` is `{ i64 len, i64 cap, ptr data, [N x T]
            // inline }`. The live slots are the inline block while it fits and
            // `data` once it has spilled, and `sa_base` answers which — the same
            // branch `copy_at` takes. RFC-0092 M3: the slots go back too, and the
            // `data` pointer is null while inline, which `free` refuses.
            Type::SmallArray(inner, _) if self.deep_row(ty) => {
                let l = self.layout_of(ty, line)?;
                let stride = self.stride(&inner, line)?;
                let n = b.local(ValType::I32);
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[0])));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::LocalSet(n));
                let base = self.sa_base(b, a, ty, line)?;
                self.rel_each(m, b, base, n, stride, &inner, line)?;
                b.ins(&Instruction::LocalGet(a))
                    .ins(&Instruction::I32Load(word_at(l.fields[2])))
                    .ins(&Instruction::Call(self.cx.rt.free));
                Ok(())
            }
            // Two parallel buffers. String keys are released per entry
            // (RFC-0092 M3); Int64 keys go with their buffer (RFC-0117). The
            // elements first, then the buffers they live in.
            Type::Map(kt, vt) if self.deep_row(ty) => {
                let ik = self.cx.resolve(&kt) == Type::Int;
                let l = self.layout_of(ty, line)?;
                let vstride = self.stride(&vt, line)?;
                let n = b.local(ValType::I32);
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[2])));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::LocalSet(n));
                for (i, (stride, elem)) in [(4u32, Type::Str), (vstride, (*vt).clone())]
                    .into_iter()
                    .enumerate()
                {
                    let buf = b.local(ValType::I32);
                    b.ins(&Instruction::LocalGet(a));
                    b.ins(&Instruction::I32Load(word_at(l.fields[i])));
                    b.ins(&Instruction::LocalSet(buf));
                    if !(ik && i == 0) {
                        self.rel_each(m, b, buf, n, stride, &elem, line)?;
                    }
                    b.ins(&Instruction::LocalGet(buf))
                        .ins(&Instruction::Call(self.cx.rt.free));
                }
                // The index holds no elements — it holds positions — so it is
                // freed and not walked.
                b.ins(&Instruction::LocalGet(a))
                    .ins(&Instruction::I32Load(word_at(l.fields[4])))
                    .ins(&Instruction::Call(self.cx.rt.free));
                Ok(())
            }
            Type::Array(_) | Type::SmallArray(..) | Type::Map(..) => {
                let offs = match self.rel_for(ty, line)? {
                    Some(Rel::Buffers(o)) => o,
                    _ => return Ok(()),
                };
                for off in offs {
                    b.ins(&Instruction::LocalGet(a))
                        .ins(&Instruction::I32Load(word_at(off)));
                    b.ins(&Instruction::Call(self.cx.rt.free));
                }
                Ok(())
            }
            Type::Record(_) => {
                let l = self.layout_of(ty, line)?;
                let fields = self
                    .cx
                    .fields(ty)
                    .ok_or_else(|| gap(&format!("the fields of `{ty}`"), line))?;
                for (i, f) in fields.iter().enumerate() {
                    if !self.owns_heap(&f.ty) {
                        continue;
                    }
                    // RFC-0093 M2. A `consume` took this field, so it has an
                    // owner already and this walk is not it.
                    if holes.iter().any(|h| *h == f.name) {
                        continue;
                    }
                    let p = b.local(ValType::I32);
                    b.ins(&Instruction::LocalGet(a));
                    b.ins(&Instruction::I32Const(l.fields[i] as i32));
                    b.ins(&Instruction::I32Add);
                    b.ins(&Instruction::LocalSet(p));
                    self.rel_holes = vyrn_frontend::declared::holes_under(holes, &f.name);
                    self.rel_at(m, b, p, &f.ty, line)?;
                }
                Ok(())
            }
            // A fixed `[N x T]` is inline memory, so there is no buffer to hand
            // back — only its slots (RFC-0092 M3). The mirror of `copy_at`'s own
            // arm, with the same count in a local.
            Type::ArrayN(inner, n) => {
                let stride = self.stride(&inner, line)?;
                let count = b.local(ValType::I32);
                b.ins(&Instruction::I32Const(n as i32));
                b.ins(&Instruction::LocalSet(count));
                self.rel_each(m, b, a, count, stride, &inner, line)
            }
            // ANY sum: the payload slots of the live variant, and only the ones
            // this walk has something to give back for. One walk since RFC-0126
            // §8.11's M4a — the built-in two used to have arms of their own here,
            // testing only tag 1 and writing a `Result` as one `if`/`else` where
            // the enum writes one `if` per variant in tag order.
            //
            // A payload the emitter BOXED is one of them whatever it holds, and
            // reading the guard as `owns_heap(payload)` alone is what skipped it.
            // `Option<Handle<Node>>` is the witness: a `Handle` is three `Int64`
            // fields, it owns nothing, and the sum still holds one `malloc` block
            // per live payload — which is why `own::owns_heap` asks
            // `types::payload_boxed` at the SUM. The two guards have to ask the
            // same question of the payload, and this one asks the emitter's own
            // rule ([`Fn_::word2`]) because the box is the emitter's.
            // `rel_word`'s `Word::Boxed` arm already frees exactly the box.
            Type::Enum(_) => {
                let vs = self.cx.sum_vs(ty).unwrap_or_default();
                let l = self.layout_of(ty, line)?;
                for (tag, var) in vs.iter().enumerate() {
                    let mut live = false;
                    for p in &var.payload {
                        live |= self.owns_heap(p) || self.word2(p)? == Word::Boxed;
                    }
                    if !live {
                        continue;
                    }
                    tag_eq(b, a, tag as i64);
                    b.ins(&Instruction::If(BlockType::Empty));
                    self.depth += 1;
                    for (j, pty) in var.payload.clone().iter().enumerate() {
                        let w = self.word2(pty)?;
                        if !self.owns_heap(pty) && w != Word::Boxed {
                            continue;
                        }
                        let at = self.cx.payload_slot(&var.payload, j);
                        self.rel_word(m, b, a, l.fields[at], pty, w, line)?;
                    }
                    self.depth -= 1;
                    b.ins(&Instruction::End);
                }
                Ok(())
            }
            // A stored function value is `{ i64 tag, i64 captures }` (RFC-0037).
            // The captures are one heap block, read by value at the construction
            // site, and 0 when there are none — which `free` refuses. Census §16.
            Type::Fn(..) => {
                let l = self.layout_of(ty, line)?;
                b.ins(&Instruction::LocalGet(a))
                    .ins(&Instruction::I32Load(word_at(l.fields[1])))
                    .ins(&Instruction::Call(self.cx.rt.free));
                Ok(())
            }
            // A fixed `[N x T]` is a container, so its elements are U4's
            // question, not this one. A handle names something somebody else
            // reclaims.
            _ => Ok(()),
        }
    }

    /// Release what the map entry at address `a` holds — a key or a value whose
    /// slot is about to be overwritten or shifted away (RFC-0028).
    ///
    /// [`Gen::release_entry`] on the textual backend, instruction for
    /// instruction: deeper than [`Fn_::snap_at`] because the entry is read out
    /// of its slot rather than overwritten under the walk, and skipping the
    /// stream and the declared `release` because both are observable from inside
    /// the language and the interpreter runs neither when a value is replaced.
    fn rel_entry(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        a: u32,
        ty: &Type,
        line: usize,
    ) -> Result<(), String> {
        match self.cx.owned.release_kind(ty) {
            None | Some(DropKind::CloseStream) | Some(DropKind::Release(..)) => Ok(()),
            _ => self.rel_at(m, b, a, ty, line),
        }
    }

    /// Release the sum payload word at `a + off`.
    ///
    /// The mirror of [`Fn_::copy_word`], and the reason it is a function of its
    /// own: a payload has two encodings. A `String` rides in the word, and
    /// anything wider is a pointer to a block — Phase 3 measured that a user
    /// enum's `String` payload boxes while an `Option<String>`'s does not, and a
    /// release that knew only one of them would free a stack address or leak.
    fn rel_word(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        a: u32,
        off: u32,
        pty: &Type,
        w: Word,
        line: usize,
    ) -> Result<(), String> {
        match w {
            // The sixth site of the rule [`Fn_::str_owned`] states, and the one a
            // container and a record field do not reach: those descend through
            // [`Fn_::rel_at`], whose `Str` arm draws the region key, and a sum
            // payload comes here instead. A `String` payload that rides IN the
            // word is freed on this line, so `Some(a + b)` inside a region — and
            // `Ok`, `Err`, and every user variant carrying a `String` — handed the
            // arena's block to the allocator a second time. Both encodings route
            // through this one function, so both are answered here: the boxed arm
            // below frees only the BOX, which is `malloc`'s at every depth the way
            // an `Array` buffer is, and the `String` inside it is `rel_at`'s.
            Word::Ext(ValType::I32) if matches!(self.cx.resolve(pty), Type::Str) => {
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(off)));
                b.ins(&Instruction::I32WrapI64);
                str_hdr(b);
                b.ins(&Instruction::Call(self.cx.rt.free));
                Ok(())
            }
            Word::Boxed => {
                let p = b.local(ValType::I32);
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(off)));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::LocalSet(p));
                self.rel_at(m, b, p, pty, line)?;
                b.ins(&Instruction::LocalGet(p))
                    .ins(&Instruction::Call(self.cx.rt.free));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// The buffers a value of `ty` holds, as `(byte offset, carries a String
    /// header)`, for a **store** that replaces it (RFC-0089 rule 4).
    ///
    /// A deliberate subset of [`Fn_::rel_for`]. A cell, a stream and a declared
    /// `release` are all observable from inside the language — a stale cell traps
    /// and a user `release` is ordinary Vyrn that may print — and the interpreter
    /// reclaims those from the value the binding took at its `let`, not from the
    /// slot's last one. Releasing them on a store would make the three engines run
    /// different programs, so a store leaves all three alone. Phase 8c deletes the
    /// first two outright.
    fn store_bufs(&mut self, ty: &Type, line: usize) -> Result<Vec<(u32, bool)>, String> {
        Ok(match self.rel_for(ty, line)? {
            Some(Rel::Str) => vec![(0, true)],
            Some(Rel::Buffers(offs)) => offs.into_iter().map(|o| (o, false)).collect(),
            // An `Array<T>` whose elements have a release row answers `Deep`
            // (RFC-0092 M2). A store hands back the one buffer it always did:
            // the elements it held leak, exactly as they did before the row
            // landed, and freeing them here would mean reading a length the
            // store is in the middle of replacing.
            Some(Rel::Deep(t, _)) => match self.cx.resolve(&t) {
                Type::Array(_) => vec![(self.layout_of(&t, line)?.fields[0], false)],
                Type::Map(..) => {
                    let l = self.layout_of(&t, line)?;
                    vec![
                        (l.fields[0], false),
                        (l.fields[1], false),
                        (l.fields[4], false),
                    ]
                }
                Type::SmallArray(..) => vec![(self.layout_of(&t, line)?.fields[2], false)],
                // A RECORD, shallowly (round eighteen) — the textual backend's
                // `snap_val` twin: each heap-owning field's buffer, at the
                // field's offset plus wherever the field's own shape keeps it,
                // recursively. Elements and boxed payloads still leak rather
                // than risk reading through a value the store is replacing.
                ref rec => match vyrn_frontend::types::record_fields(rec, &self.cx.types) {
                    Some(fields) => {
                        let l = self.layout_of(&t, line)?;
                        let bases: Vec<u32> = l.fields.clone();
                        let mut out = Vec::new();
                        for (i, f) in fields.iter().enumerate() {
                            for (o, h) in self.store_bufs(&f.ty, line)? {
                                out.push((bases[i] + o, h));
                            }
                        }
                        out
                    }
                    None => Vec::new(),
                },
            },
            _ => Vec::new(),
        })
    }

    // `place_owns` lived here until §26 steps 3–4: the ownedness of a field
    // or element store is the plan's per-statement answer now
    // (`store_owned_at`), folded once in `own::analyze` from module-state
    // rule 4 and the placed rows — the same two ways to own it tested,
    // read from one artifact instead of two per-binding registries. The
    // region caveat its doc carried — a String reassigned in module state
    // inside a `region` must not free arena memory — is gone with the rest of
    // the lexical region rule: `free` refuses an arena block by the class word
    // in its header, so the store snapshots and releases at every depth.

    /// The address of `p` plus `off`, in a fresh local. A wasm local holding an
    /// aggregate holds its ADDRESS, which is the one case [`Place::addr`] cannot
    /// answer; a local holding a scalar has no address at all and never reaches
    /// here (its caller snapshots the value itself).
    fn addr_local(&mut self, b: &mut Frame, p: Place, off: u32) -> u32 {
        match p {
            Place::Local(l) => {
                b.ins(&Instruction::LocalGet(l));
                if off != 0 {
                    b.ins(&Instruction::I32Const(off as i32))
                        .ins(&Instruction::I32Add);
                }
            }
            _ => {
                p.addr(b, off);
            }
        }
        let a = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(a));
        a
    }

    /// Copy the buffer pointers a value of `ty` at `addr` holds into fresh locals,
    /// so the store may overwrite the place before they are handed back.
    ///
    /// The snapshot is taken BEFORE the store and freed AFTER it, which is the
    /// same order as "compute the new value, then release the old" and survives an
    /// aggregate that is built destination-first. It is only ever reached where the
    /// new value does not name the place ([`vyrn_frontend::movecheck::mentions`]),
    /// so nothing the store computes can read the snapshot.
    fn snap_at(
        &mut self,
        b: &mut Frame,
        addr: u32,
        ty: &Type,
        line: usize,
    ) -> Result<Vec<(u32, bool)>, String> {
        let mut out = Vec::new();
        for (off, hdr) in self.store_bufs(ty, line)? {
            b.ins(&Instruction::LocalGet(addr))
                .ins(&Instruction::I32Load(word_at(off)));
            let t = b.local(ValType::I32);
            b.ins(&Instruction::LocalSet(t));
            out.push((t, hdr));
        }
        // A sum's payload BOX is a buffer too, and the one [`Fn_::store_bufs`]
        // cannot answer with an offset: which slot holds it depends on the tag,
        // so it is read under a tag test rather than off the address. Everything
        // else about it is the flat case — snapshot before, free after.
        //
        // The local is zeroed FIRST. A store inside a loop reaches this code
        // once and runs it every turn, so a local left over from a turn whose
        // tag matched would be freed again on a turn whose tag does not.
        // `free` refuses a null, which is what a variant with no box leaves.
        for (off, sty) in self.store_boxes(ty, line)? {
            let base = self.addr_local(b, Place::Local(addr), off);
            let vs = self.cx.sum_vs(&sty).unwrap_or_default();
            let l = self.layout_of(&sty, line)?;
            for (tag, var) in vs.iter().enumerate() {
                let mut boxed = Vec::new();
                for (j, p) in var.payload.iter().enumerate() {
                    if self.word2(p)? == Word::Boxed {
                        boxed.push(self.cx.payload_slot(&var.payload, j));
                    }
                }
                if boxed.is_empty() {
                    continue;
                }
                let locals: Vec<u32> = boxed
                    .iter()
                    .map(|_| {
                        let t = b.local(ValType::I32);
                        b.ins(&Instruction::I32Const(0))
                            .ins(&Instruction::LocalSet(t));
                        t
                    })
                    .collect();
                tag_eq(b, base, tag as i64);
                b.ins(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                for (k, slot) in boxed.iter().enumerate() {
                    b.ins(&Instruction::LocalGet(base))
                        .ins(&Instruction::I64Load(at(l.fields[*slot])))
                        .ins(&Instruction::I32WrapI64)
                        .ins(&Instruction::LocalSet(locals[k]));
                }
                self.depth -= 1;
                b.ins(&Instruction::End);
                out.extend(locals.into_iter().map(|t| (t, false)));
            }
        }
        Ok(out)
    }

    /// The sums a value of `ty` holds, each as `(byte offset, the sum's type)`
    /// — the places whose reclamation needs a tag and so cannot be one of
    /// [`Fn_::store_bufs`]'s flat offsets.
    ///
    /// The same subset that function takes, walked the same way: through record
    /// fields, and stopping at a declared `release`, whose timing is the
    /// language's and not a store's. An `Array`, a `Map` and a `SmallArray` stop
    /// here as they do there — a store hands back the buffer and the elements
    /// in it leak, which is the answer this side has always given.
    fn store_boxes(&mut self, ty: &Type, line: usize) -> Result<Vec<(u32, Type)>, String> {
        if matches!(
            self.cx.owned.release_kind(ty),
            Some(DropKind::Release(..)) | None
        ) {
            return Ok(Vec::new());
        }
        let t = self.cx.resolve(ty);
        if self.cx.sum_vs(&t).is_some() {
            return Ok(vec![(0, t)]);
        }
        let Some(fields) = vyrn_frontend::types::record_fields(&t, &self.cx.types) else {
            return Ok(Vec::new());
        };
        let l = self.layout_of(&t, line)?;
        let mut out = Vec::new();
        for (i, f) in fields.iter().enumerate() {
            for (o, st) in self.store_boxes(&f.ty, line)? {
                out.push((l.fields[i] + o, st));
            }
        }
        Ok(out)
    }

    /// Hand a snapshot back, after the store that replaced it. `free` refuses a
    /// data-segment address and a null, so a place that held a literal or an
    /// unspilled `SmallArray` costs one silent call and nothing else.
    fn free_snap(&mut self, b: &mut Frame, snap: &[(u32, bool)]) {
        for &(t, hdr) in snap {
            b.ins(&Instruction::LocalGet(t));
            if hdr {
                str_hdr(b);
            }
            b.ins(&Instruction::Call(self.cx.rt.free));
        }
    }

    /// Push a region scope: trap if this would be the 65th, else bump the counter
    /// and take the arena's mark into a local of its own.
    ///
    /// The bound is the LLVM prelude's fixed 64-slot region stack and the
    /// interpreter's own `region_depth >= 64`, so all three engines refuse the same
    /// nesting with the same words. The counter and its trap stay inline rather
    /// than moving into the runtime: they are fourteen instructions at a handful
    /// of sites, and a program that traps at the limit must trap where it did.
    ///
    /// The mark is `std/runtime`'s (PLAN-0125-runtime §4.3), and a fresh local
    /// rather than a scratch slot because every region open at an exit edge
    /// needs its own — see [`Fn_::exit_regions_above`].
    fn region_enter(&mut self, b: &mut Frame) {
        let sp = self.cx.rt.region_sp;
        b.ins(&Instruction::I32Const(sp as i32))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::I32Const(REGION_MAX as i32))
            .ins(&Instruction::I32GeU)
            .ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        self.trap_row(b, vyrn_frontend::trap::Rule::RegionDepth, None);
        self.depth -= 1;
        b.ins(&Instruction::End);
        self.region_bump(b, 1);
        let mark = b.local(ValType::I32);
        b.ins(&Instruction::Call(self.cx.rt.region_enter))
            .ins(&Instruction::LocalSet(mark));
        self.region_marks.push(mark);
    }

    /// Pop a region scope and free what it allocated. Stack-neutral, so it may be
    /// emitted with a return value already on the operand stack — the same
    /// property M2f's `modify` copy-out needs and M2d's note about a value sitting
    /// under a block established.
    ///
    /// This used to reclaim nothing, on the argument that `malloc` here was a bump
    /// pointer that never freed, "so the difference is not observable". The
    /// premise died at M6, when this backend got a segregated free list, and the
    /// omission then leaked every `String` a region held: one source file measured
    /// 13.4 MB native against 3,664.5 MB and `out of memory` under wasmtime (the
    /// external audit's finding C2.1). The side vector that answered it is gone in
    /// turn: step 8 makes the arena the bump `std/runtime` describes, and this is
    /// one call with the mark [`Fn_::region_enter`] took.
    ///
    /// Routing is LEXICAL, as `Gen::heap_alloc` routes in the textual backend:
    /// [`Fn_::arena_route`] raises the runtime's flag around a `String` the
    /// emitter allocates inside a region, and lowers it again. Reading an open
    /// region's depth instead would arena-allocate a callee's `String` that the
    /// region escape guard never examined, and — at `malloc` — an `Array`
    /// buffer that `checker.rs`'s `contains_heap` says is never the arena's, so
    /// a global array grown inside a region would die at the brace under a live
    /// binding. That is why the routing is written at the emitter's allocation
    /// sites and not at the allocator.
    fn region_exit(&mut self, b: &mut Frame, mark: u32) {
        b.ins(&Instruction::LocalGet(mark))
            .ins(&Instruction::Call(self.cx.rt.region_exit));
        self.region_bump(b, -1);
    }

    /// Call `std/runtime`'s `strFromBytes` for the bytes at `src` — a local
    /// holding an `Array<UInt8>` header — writing the `Result<String, String>`
    /// it answers into the frame slot at `dest`.
    ///
    /// The WHOLE call, and that is the point: the destination, the data
    /// pointer, the count, the check's answer and the two interned messages
    /// (PLAN-0125-runtime §6 step 4). A callee's argument list is the callee's
    /// rule, so it is stated once, here.
    ///
    /// The DFA table used to be the third argument. RFC-0125 §3 M6 (the third
    /// judgment's fifth slice) replaced it with the answer of `std/text`'s
    /// `stringFault` — the one check every engine calls — so the runtime
    /// function builds and decides nothing. That slice made the answer the
    /// CALLER's to push, and this call has two callers: `stringFromBytes` was
    /// given the new argument and [`Fn_::map_tally_bytes`] was not, so
    /// `tallyBytes` emitted five operands into a six-operand signature and
    /// wasmtime refused the module (RFC-0125 §3 M5, the eighteenth slice).
    /// Neither caller can be short of an argument it does not spell.
    fn str_from_bytes(
        &mut self,
        b: &mut Frame,
        dest: u32,
        src: u32,
        al: &Layout,
        line: usize,
    ) -> Result<(), String> {
        let check = vyrn_frontend::loader::STRING_FAULT;
        let Some(check_idx) = self.cx.sigs.get(check).map(|s| s.index) else {
            // `std/text` is injected into any program that mentions a builtin
            // building a `String` out of bytes, so reaching this means a
            // program built without a std root.
            return unsupported(
                "`stringFromBytes` with no `std/text` in the link (its check is Vyrn)",
                line,
            );
        };
        let fault = self.scratch(b, ValType::I32, 1);
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::Call(check_idx));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::LocalSet(fault));
        b.slot(dest);
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::I32Load(word_at(al.fields[0])));
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::I64Load(at(al.fields[1])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::LocalGet(fault));
        b.ins(&Instruction::I32Const(self.cx.rt.bnul as i32))
            .ins(&Instruction::I32Const(self.cx.rt.butf8 as i32))
            .ins(&Instruction::Call(self.cx.rt.str_from_bytes));
        Ok(())
    }

    /// The call about to be emitted (`on`), or just emitted (`!on`), allocates a
    /// `String` that inside a `region` is the ARENA's. Raise `std/runtime`'s
    /// routing flag around it and lower it again. `strNew`, the funnel every
    /// `String` block comes through, is what reads the flag and bumps in the
    /// region's chunks; `malloc` never reads it, so a program with no region
    /// pays nothing (PLAN-0125-runtime §4.3).
    ///
    /// Stack-neutral, so it may be emitted with the call's operands already on
    /// the stack. The window covers the whole call, so a `String` the callee
    /// mints on the way — `strFromBytes`'s buffer under `intStr` — is the
    /// arena's too; it is either freed inside the call (a silent refusal on an
    /// arena block) or reclaimed by the brace, and it cannot leave, because
    /// none of these callees runs user code.
    ///
    /// This is `Gen::heap_alloc`'s `region_depth > 0` test, and the set of call
    /// sites is `Gen::str_alloc`'s set of call sites — the two must stay equal,
    /// because a block one backend gives the arena and the other gives the
    /// ownership walk is a block the two backends own differently. The mapping,
    /// site for site:
    ///
    /// | `Gen::str_alloc` caller | here |
    /// |---|---|
    /// | `Gen::emit_str_concat`'s region path | `rt.concat`, at `+` and at `@concat` |
    /// | `Gen::deep_copy`'s `Str` arm | [`Fn_::str_dup`], the funnel `copy_stack`, `copy_at` and `copy_word` share |
    /// | `@str` of an `Int` / a sized int | `rt.int_str` |
    /// | `@str` of a `Bool` / a `String` | `str_dup` again |
    /// | `Gen::emit_str_append`'s take-ownership path | unreachable: both backends refuse the in-place append inside a region |
    /// | `stringFromBytes`'s `Err` message | no block: this backend hands out the interned message itself |
    ///
    /// `@str` of a `Float` is in NEITHER set: both backends format it by calling
    /// `std/num`'s `f64Str`, so the block is the callee's and the arena of the
    /// caller's region never sees it.
    fn arena_route(&mut self, b: &mut Frame, on: bool) {
        if self.region_depth > 0 {
            b.ins(&Instruction::GlobalGet(HEAP_BASE))
                .ins(&Instruction::I32Const(ARENA_ON as i32))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Const(i32::from(on)))
                .ins(&Instruction::I32Store(word()));
        }
    }

    /// Leave a region WITHOUT giving its blocks back, for a `return` (and a `?`)
    /// that carries one of them out. The value belongs to the caller now; the
    /// bump stays where it is, so the frame's other blocks leak, which is the
    /// trade the textual `__vyrn_region_pop` makes for the same reason. Only the
    /// nesting counter moves.
    fn region_pop(&mut self, b: &mut Frame) {
        self.region_bump(b, -1);
    }

    fn region_bump(&mut self, b: &mut Frame, by: i32) {
        let sp = self.cx.rt.region_sp;
        b.ins(&Instruction::I32Const(sp as i32))
            .ins(&Instruction::I32Const(sp as i32))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::I32Const(by))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Store(word()));
    }

    /// Close every region scope open past `depth`, for an edge that leaves them.
    /// `frees` is false on the one edge that hands a block out — see
    /// [`Fn_::region_pop`].
    fn exit_regions_above(&mut self, b: &mut Frame, depth: u32, frees: bool) {
        for i in (depth..self.region_depth).rev() {
            if frees {
                let mark = self.region_marks[i as usize];
                self.region_exit(b, mark);
            } else {
                self.region_pop(b);
            }
        }
    }

    /// `br` distance to a block that was opened when `depth` had the given value.
    fn br_to(&self, opened: u32) -> u32 {
        self.depth - opened - 1
    }

    fn lookup(&self, name: &str, line: usize) -> Result<(Place, Type), String> {
        self.scope
            .iter()
            .rev()
            .find(|(n, _, _)| n == name)
            .map(|(_, p, t)| (*p, t.clone()))
            // Module state (RFC-0013) is the fallback rather than a scope frame,
            // so a local always shadows a global — the same order the textual
            // backend's `lookup` uses.
            .or_else(|| self.cx.globals.get(name).cloned())
            .ok_or_else(|| gap(&format!("the name `{name}` (not a local)"), line))
    }

    // -- statements ---------------------------------------------------------

    /// Lower one statement.
    ///
    /// Exhaustive over `Stmt`, and deliberately without a catch-all: `region` was
    /// the last unlowered kind, so the gap reporter that used to sit here (and the
    /// `stmt_name`/`stmt_line` pair feeding it) was dead code claiming to cover
    /// something. A statement kind added to the AST is now a compile error naming
    /// this match. Expressions keep theirs — `expr_name` still has work.
    fn stmt(&mut self, m: &mut Module, b: &mut Frame, s: &Stmt) -> Result<(), String> {
        // RFC-0125 §2.3, per STATEMENT: where the core's rows carry this one,
        // they are what is emitted and no arm below runs. The unit was the
        // function until the interleave slice, which is why an arm could not go
        // while one body of the corpus still needed it.
        if self.core_took(m, b, s)? {
            return Ok(());
        }
        count(stmt_form(s), false);
        match s {
            Stmt::Let {
                name,
                ty,
                value,
                line,
                ..
            } => {
                let want = match ty {
                    Some(t) => {
                        self.cx.repr(t, *line)?;
                        // The annotation as written: `let mut m: Age = 21`
                        // re-validates on every later assignment, and it can only
                        // do that if the binding remembers it is an `Age`.
                        Some(t.clone())
                    }
                    None => None,
                };
                let (place, bound) = match &want {
                    // Annotated: the slot's shape is known before the
                    // initializer runs, so it can be written into directly.
                    Some(t) => {
                        let r = self.cx.repr(t, *line)?;
                        let place = self.place_for(b, &r, *line)?;
                        self.store_into(m, b, place, &r, value, t, true)?;
                        (place, t.clone())
                    }
                    // Unannotated, and the initializer is a literal or a call
                    // whose type the typer knows before it runs: the slot is
                    // taken first and the value is built in it (RFC-0125 M1).
                    None if matches!(
                        value,
                        Expr::StructLit { .. } | Expr::ArrayLit { .. } | Expr::Call { .. }
                    ) && self
                        .peek(value, *line)
                        .ok()
                        .is_some_and(|t| matches!(self.cx.repr(&t, *line), Ok(Repr::Agg(_)))) =>
                    {
                        let t = self.peek(value, *line)?;
                        let Repr::Agg(l) = self.cx.repr(&t, *line)? else {
                            unreachable!("the guard above checked the shape")
                        };
                        let off = b.alloc(l.size, l.align);
                        self.agg_into(m, b, Dest::Slot(off), l.size, value, &t, true)?;
                        (Place::Slot(off), t)
                    }
                    None => {
                        // Unannotated: the type is whatever the initializer
                        // produced, so evaluate first and bind after.
                        let got = self.expr(m, b, value)?;
                        let r = self.cx.repr(&got, *line)?;
                        let place = self.place_for(b, &r, *line)?;
                        match (place, &r) {
                            (Place::Local(l), _) => {
                                b.ins(&Instruction::LocalSet(l));
                            }
                            (Place::Slot(off), Repr::Agg(l)) => {
                                // The value is already in a slot; this one is
                                // the binding's own, so the copy is what makes
                                // `let a = b` two independent records.
                                let src = self.scratch(b, ValType::I32, 0);
                                b.ins(&Instruction::LocalSet(src));
                                b.slot(off);
                                b.ins(&Instruction::LocalGet(src));
                                b.ins(&Instruction::I32Const(l.size as i32));
                                b.ins(&Instruction::MemoryCopy {
                                    src_mem: 0,
                                    dst_mem: 0,
                                });
                            }
                            _ => return unsupported("a `let` of a Unit value", *line),
                        }
                        (place, got)
                    }
                };
                // A String accumulator gets its append shadow here, at the one
                // declaration site — the same place, under the same whitelist, as
                // the textual backend's.
                //
                // It starts OWNED when this `let` owns its initializer, which is
                // the fact `own` already decided. Starting it unowned abandoned
                // the initializer's buffer at the first append — Phase 4c recorded
                // that leak and this is where it closes. Starting it owned for a
                // binding that names somebody else's storage (`let mut s = r.name`
                // is a borrow, not a move) would free that storage instead, which
                // is why the answer is read rather than assumed.
                //
                // A LITERAL initializer is somebody else's storage too: `let mut
                // acc = ""` is released (a placed row answers for the buffer the
                // loop ENDS on) and its first append would otherwise grow a data
                // segment address in place. The module-state seed always did.
                let owns = self.releases_whole(s as *const Stmt as usize);
                if let Place::Local(l) = place {
                    if self.cx.resolve(&bound) == Type::Str
                        && self.append_ok.contains(name.as_str())
                    {
                        self.str_append_shadow(b, l, owns && !matches!(value, Expr::Str(_)));
                    }
                }
                // A `let` that owns a heap value is reclaimed when this block
                // exits. The key is the statement's node address, which is `own`'s
                // own identity for it — the textual backend reads the same map with
                // the same key, so the two cannot disagree about which `let` owns
                // what.
                self.scope.push((name.clone(), place, bound.clone()));
                if owns {
                    if let Some(r) = self.rel_for(&bound, *line)? {
                        self.register_rel(b, s as *const Stmt as usize, place, r);
                    }
                }
            }
            Stmt::Assign { name, value, line } => {
                let (place, ty) = self.lookup(name, *line)?;
                let r = self.cx.repr(&ty, *line)?;
                // `s = s + a + b` on an eligible local String: grow the buffer
                // instead of building a new one. `concat` allocates and copies
                // both halves every time, which makes the shape every writer is
                // written in quadratic — `toJson` of 40k `Int64` did not merely
                // take 1.4 s here, it exhausted linear memory and trapped.
                //
                // Only outside a `region` (arena memory is not the bump heap the
                // helper grows out of) and only for a local that owns a shadow,
                // which is exactly a `let`-declared one the whitelist cleared. The
                // spine is [`crate::self_append_spine`], shared with the textual
                // backend: what counts as a self-append is one rule, so the two
                // backends cannot recognize different sets of writers and diverge
                // on which one still copies.
                //
                // Module state qualifies too since Phase 5: `Cx::gappend` is the
                // same whitelist read over every body, and census P1 measured what
                // the global's exclusion cost — 4.92 s and 12.2 GB against the
                // local's 0.095 s, for the same eight lines.
                let shadow = match place {
                    Place::Local(l) => self.str_append.get(&l).copied().map(Place::Slot),
                    Place::Static(_) => self.cx.gappend.get(name).copied().map(Place::Static),
                    Place::Slot(_) => None,
                };
                // The ONE thing a `region` is still asked, and it is not a
                // question about ownership: the arena is a bump with no
                // `realloc`, so a `String` it handed out cannot GROW in place.
                // The take-ownership append stays refused inside a region on
                // both backends, which is what `Fn_::arena_route`'s site table
                // records for `Gen::emit_str_append`, and the general store
                // below copies instead.
                if self.region_depth == 0 {
                    if let Some(own) = shadow {
                        if let Some(parts) = crate::self_append_spine(name, value) {
                            // The spine handles this store's ownership itself
                            // (§22's own state machine) — and the fold's
                            // per-statement answer decides one more thing
                            // here, exactly as in the textual backend's
                            // `emit_str_append_owned`: when the shadow flag
                            // says the buffer is not this path's, the first
                            // append COPIES out of it and abandons it. Right
                            // for a borrow; a leak when the store that put the
                            // buffer there was owned — the general store below
                            // resets the flag on every reassign, so `s = a + b`
                            // then `s = s + c` abandoned the `a + b` buffer
                            // (exit-residue round sixteen).
                            let owned_here = self.cx.store_row(s as *const Stmt as usize);
                            // Save the flag and the incoming pointer before the
                            // appends; free after them, only if the take ran
                            // (entry flag was 0) and the buffer is heap — an
                            // interned literal's `cap` is `u32::MAX` and is
                            // nobody's to free.
                            let taken = if owned_here {
                                let f0 = b.local(ValType::I32);
                                let op = b.local(ValType::I32);
                                own.addr(b, 0)
                                    .ok_or_else(|| gap("an append flag with no address", *line))?;
                                b.ins(&Instruction::I32Load(word()))
                                    .ins(&Instruction::LocalSet(f0));
                                match place {
                                    Place::Local(l) => {
                                        b.ins(&Instruction::LocalGet(l));
                                    }
                                    Place::Static(at) => {
                                        b.ins(&Instruction::I32Const(at as i32))
                                            .ins(&Instruction::I32Load(word()));
                                    }
                                    Place::Slot(_) => {
                                        return unsupported("an in-place append into a slot", *line)
                                    }
                                }
                                b.ins(&Instruction::LocalSet(op));
                                Some((f0, op))
                            } else {
                                None
                            };
                            // A CALL-producer part's `Released` row is teed by
                            // `expr` into `arg_frees`, and this fast path was
                            // the one consumer with no drain — the textual
                            // backend's twin has the same note (exit-residue
                            // round five, herofield's per-glyph temporary).
                            let mark = self.arg_frees.len();
                            for p in parts {
                                self.append_once(m, b, place, own, p)?;
                            }
                            for (l, t2) in self.arg_frees.split_off(mark) {
                                self.free_arg_temp(m, b, l, &t2, *line)?;
                            }
                            if let Some((f0, op)) = taken {
                                b.ins(&Instruction::LocalGet(f0))
                                    .ins(&Instruction::I32Eqz)
                                    .ins(&Instruction::If(BlockType::Empty))
                                    .ins(&Instruction::LocalGet(op));
                                str_hdr(b);
                                b.ins(&Instruction::I32Load(cap_at()))
                                    .ins(&Instruction::I32Const(-1))
                                    .ins(&Instruction::I32Ne)
                                    .ins(&Instruction::If(BlockType::Empty))
                                    .ins(&Instruction::LocalGet(op));
                                str_hdr(b);
                                b.ins(&Instruction::Call(self.cx.rt.free))
                                    .ins(&Instruction::End)
                                    .ins(&Instruction::End);
                            }
                            return Ok(());
                        }
                    }
                }
                // RFC-0089 rule 4: the store releases what the place held. Not
                // when the new value names the place — `a = @push(a, i)` grows the
                // old buffer and hands it back, so freeing it would be a double
                // free.
                //
                // RFC-0125 §3 M3: the rule above, the row, and round
                // eighteen's `store_fresh` are ONE answer, and the core
                // states it at the store's own node (`Cx::store_fact`). What
                // is left here is the region gate: arena memory is not this
                // path's to free, whatever the store displaces. Where the
                // core states nothing — a body it could not lower — the
                // three read as they always did. The answer is taken FIRST
                // so a region-gated site still counts as considered (§26's
                // finish check).
                let owned_here = self
                    .cx
                    .store_fact(s as *const Stmt as usize)
                    .unwrap_or(false);
                let snap = if owned_here {
                    match (place, &r) {
                        // A scalar local IS the pointer; it has no address.
                        (Place::Local(l), Repr::Scalar(_)) => {
                            if self.store_bufs(&ty, *line)?.is_empty() {
                                Vec::new()
                            } else {
                                let t = b.local(ValType::I32);
                                b.ins(&Instruction::LocalGet(l))
                                    .ins(&Instruction::LocalSet(t));
                                vec![(t, true)]
                            }
                        }
                        _ => {
                            let a = self.addr_local(b, place, 0);
                            self.snap_at(b, a, &ty, *line)?
                        }
                    }
                } else {
                    Vec::new()
                };
                self.store_into(m, b, place, &r, value, &ty.clone(), false)?;
                self.free_snap(b, snap.as_slice());
                // The place now holds a pointer this path did not allocate, so the
                // next append copies rather than grows. Claiming ownership here
                // instead would free a borrowed buffer wherever rule 2 still lets
                // one through — so the flag stays honest and the APPEND recovers
                // the buffer, freeing what its take copied out of when the fold
                // proves the store that put it there was owned (round sixteen;
                // the spine branch above).
                if let Some(own) = shadow {
                    own.addr(b, 0);
                    b.ins(&Instruction::I32Const(0))
                        .ins(&Instruction::I32Store(word()));
                }
            }
            Stmt::SetField {
                name,
                field,
                value,
                line,
            } => {
                let (place, ty) = self.lookup(name, *line)?;
                let (foff, fty) = self.field_of(&ty, field, *line)?;
                let fr = self.cx.repr(&fty, *line)?;
                // Rule 4 through a field: the record owns what its field holds, so
                // storing over it releases the old one. Census §4's second row.
                // §26 steps 3–4: the plan's per-statement answer replaces the
                // per-binding registry guess (`place_owns`), queried before
                // the region gate so an arena-owned site still counts as
                // considered. The value-alias guard folded with it.
                let snap = if self.cx.store_row(s as *const Stmt as usize) {
                    let a = self.addr_local(b, place, foff);
                    self.snap_at(b, a, &fty, *line)?
                } else {
                    Vec::new()
                };
                match &fr {
                    Repr::Scalar(_) => {
                        place
                            .addr(b, foff)
                            .ok_or_else(|| gap("a field assignment to a non-record", *line))?;
                        self.expr_as(m, b, value, &fty)?;
                        b.ins(&store_of(&self.cx.ll(&fty)));
                    }
                    // RFC-0125 M1: built in the field when the value cannot
                    // see the binding while it is made; module state is
                    // never built in place (see `Dest`).
                    Repr::Agg(l) => match Dest::of(place) {
                        Some(d) => {
                            let fresh = !observes(value, name);
                            self.agg_into(m, b, d.at(foff), l.size, value, &fty, fresh)?;
                        }
                        None => {
                            place
                                .addr(b, foff)
                                .ok_or_else(|| gap("a field assignment to a non-record", *line))?;
                            self.expr_as(m, b, value, &fty)?;
                            b.ins(&Instruction::I32Const(l.size as i32));
                            b.ins(&Instruction::MemoryCopy {
                                src_mem: 0,
                                dst_mem: 0,
                            });
                        }
                    },
                    Repr::Unit => return unsupported("a Unit field", *line),
                }
                self.free_snap(b, snap.as_slice());
            }
            Stmt::Return { value, line } => {
                match value {
                    Some(e) => self.ret_value(m, b, e, *line)?,
                    None if matches!(self.ret, Repr::Unit) => {}
                    None => {
                        return unsupported(
                            "a return whose value does not match the signature",
                            *line,
                        );
                    }
                }
                // Every open frame, before the branch. The value is already on the
                // operand stack (or written through `dest`), and a release does not
                // disturb it — M2d's note that a value may sit under a block.
                // Ownership analysis has un-tracked anything the return escapes, so
                // this cannot release what is being handed back.
                self.emit_releases(m, b, ExitKind::Return, s as *const Stmt as usize)?;
                // And every region scope, for the same reason the interpreter
                // decrements its counter on this path: a `return` out of a region
                // leaves it. It POPS rather than frees, because a returned
                // `a + b` built inside the region points into the arena and its
                // caller owns it now — the same split the textual backend makes
                // between `__vyrn_region_pop` and `__vyrn_region_exit`.
                self.exit_regions_above(b, 0, false);
                b.ins(&Instruction::Br(self.depth));
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                line,
            } => {
                self.cond(m, b, cond, *line)?;
                // RFC-0114 Rule N: the analysis says one branch consumed a
                // binding the other still holds at the join, where nothing may
                // read it again — so the still-owning edge releases it here.
                // An `if` with no else-arm grows one when the implicit edge is
                // the one that owes a release. After a diverged arm the
                // releases are dead code, which wasm validates.
                let ers = self.cx.edge_rows(s as *const Stmt as usize);
                b.ins(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                self.block(m, b, then_block)?;
                self.emit_edge_releases(m, b, &ers, 0, *line)?;
                if let Some(e) = else_block {
                    b.ins(&Instruction::Else);
                    self.block(m, b, e)?;
                    self.emit_edge_releases(m, b, &ers, 1, *line)?;
                } else if ers.iter().any(|(_, t)| *t == 1) {
                    b.ins(&Instruction::Else);
                    self.emit_edge_releases(m, b, &ers, 1, *line)?;
                }
                self.depth -= 1;
                b.ins(&Instruction::End);
            }
            // `if let PAT = e { .. } else { .. }` (RFC-0060). Not sugar the parser
            // removed — it survives to every backend as its own node — but it IS
            // sugar in shape: one tag test, the payload bound on the taken side,
            // and no join at all, since the statement form carries no value. So it
            // is `match_expr` with the arm chain replaced by a single `if`, reusing
            // the same `tag_test` and `bind_payload`.
            Stmt::IfLet {
                pattern,
                scrutinee,
                then_block,
                else_block,
                line,
            } => {
                // An OPTIONAL projection as the scrutinee (RFC-0122): no
                // `Option` is built — prologue, one branch on the miss, and
                // the hit arm's binder bound to the place by an ordinary
                // (synthetic, row-less, so never drop-tracked) `let`.
                if self.optional_if_let(m, b, pattern, scrutinee, then_block, else_block, *line)? {
                    return Ok(());
                }
                // The two-arm switch the core lowers this to (RFC-0125 §3 M5):
                // the pattern's arm, and the `else` under the default arm a
                // user cannot spell. `match_expr` reads that shape and writes
                // the `if`/`else` this arm used to write itself.
                let empty = Block { stmts: Vec::new() };
                let other = Pattern::Other;
                let arms = [
                    ArmRef {
                        pattern,
                        body: BodyRef::Block(then_block),
                    },
                    ArmRef {
                        pattern: &other,
                        body: BodyRef::Block(else_block.as_ref().unwrap_or(&empty)),
                    },
                ];
                let key = s as *const Stmt as usize;
                self.match_expr(m, b, key, scrutinee, &arms, "`if let`", *line)?;
            }
            Stmt::While { cond, body, line } => {
                // RFC-0125 M1: the headers this loop reads and never moves,
                // taken apart once, before the loop.
                let hoisted = self.hoist_walks(b, cond, body, *line)?;
                // `block { loop { br_if 1 (!cond); body; br 0 } }` — the block is
                // where `break` goes, the loop is where `continue` goes, and
                // neither needs a relooper because both are in the AST already.
                let brk = self.depth;
                b.ins(&Instruction::Block(BlockType::Empty));
                self.depth += 1;
                let cont = self.depth;
                b.ins(&Instruction::Loop(BlockType::Empty));
                self.depth += 1;
                self.cond(m, b, cond, *line)?;
                b.ins(&Instruction::I32Eqz);
                let out = self.br_to(brk);
                b.ins(&Instruction::BrIf(out));
                self.loops.push((brk, cont, self.region_depth));
                self.block(m, b, body)?;
                self.loops.pop();
                let back = self.br_to(cont);
                b.ins(&Instruction::Br(back));
                self.depth -= 1;
                b.ins(&Instruction::End);
                self.depth -= 1;
                b.ins(&Instruction::End);
                for (name, prev) in hoisted {
                    match prev {
                        Some(w) => {
                            self.walks.insert(name, w);
                        }
                        None => {
                            self.walks.remove(&name);
                        }
                    }
                }
            }
            Stmt::ForIn {
                var,
                iter,
                body,
                line,
                ..
            } => {
                // RFC-0091 M3: a user container declares how it is iterated. The
                // probe is `&mut self`, so a program that declares no `Iterate`
                // row never reaches it — the same shape of guard `project_at`
                // uses, narrowed to the one protocol this site can dispatch.
                if self.cx.impls.iter().any(|i| i.protocol == ftypes::ITERATE) {
                    if let Some((size_fn, nth)) = self
                        .peek(iter, *line)
                        .ok()
                        .and_then(|t| ftypes::iterate_impl(&self.cx.impls, &t))
                    {
                        let blk = vyrn_frontend::project::iterate_loop(
                            &size_fn, nth, var, iter, body, *line,
                        )?;
                        // RFC-0114 §26: resolve plan queries through the
                        // clone — the textual driver's twin comment.
                        self.cx
                            .plan
                            .alias_clones(vyrn_frontend::project::iterate_aliases(blk));
                        return self.block(m, b, blk);
                    }
                }
                // `block { loop { br_if 1 (i >= len); bind; block { body }; i++;
                // br 0 } }`. The INNER block is what makes `continue` correct:
                // branching to it leaves the body and lands on the increment, so
                // a `continue` steps the index exactly like falling off the end
                // does. Branching to the loop instead would spin on one element.
                let it = self.expr(m, b, iter)?;
                // RFC-0075 M2b: a stream is pulled, not indexed.
                if let Type::Stream(inner) = self.cx.resolve(&it) {
                    return self.for_stream(m, b, var, body, &inner, *line);
                }
                // RFC-0092 M5, census "U4's price": an iterable that is a
                // TEMPORARY owns what it holds and has no name, so `own` gives
                // the STATEMENT the reclamation row — the same row Phase 10a
                // gives an `if let`'s scrutinee. A release frame of its own is
                // what makes the release survive a `return` out of the body, and
                // it is pushed BEFORE the loop's boundary so `break` and
                // `continue` leave it to the fall-through below.
                let key = s as *const Stmt as usize;
                // Is the container this frame's to give back here? A placed
                // row names it at an exit, or — for a consuming loop, whose
                // release is the core's own statement and no row's — the
                // core says so at the loop. Both are the core's answers;
                // this emitter asked the plan's droppable table until
                // RFC-0125 §3 M3's container slice.
                if self.releases_whole(key) || self.cx.loop_gives_back(key) {
                    if let Some(r) = self.rel_for(&it, *line)? {
                        // WHAT the release walks is the other half of the
                        // element sentence. Where every element left through
                        // the loop variable the deep walk `rel_for` builds
                        // would free values somebody else now owns — the trap
                        // that turned round fourteen's blanket downgrade back
                        // — so the buffer, which is the triple's field 0, is
                        // all the loop still owns.
                        let r = if self.cx.loop_buffer_only(key) {
                            Rel::Buffers(vec![0])
                        } else {
                            r
                        };
                        // `expr` leaves one I32 — an aggregate's address or a
                        // String's pointer — and `walk` wants it back, so it is
                        // stashed rather than teed into two shapes.
                        let src = b.local(ValType::I32);
                        b.ins(&Instruction::LocalSet(src));
                        let rr = self.cx.repr(&it, *line)?;
                        let place = self.place_for(b, &rr, *line)?;
                        match (place, &rr) {
                            // A copy of its own, and it has to be one: `expr`
                            // left the aggregate wherever it built it, and the
                            // body can build over that. The copy is by value, so
                            // it holds the same buffer pointers the walk reads
                            // and releasing it releases exactly those.
                            (Place::Slot(own), Repr::Agg(l)) => {
                                b.slot(own);
                                b.ins(&Instruction::LocalGet(src));
                                b.ins(&Instruction::I32Const(l.size as i32));
                                b.ins(&Instruction::MemoryCopy {
                                    src_mem: 0,
                                    dst_mem: 0,
                                });
                            }
                            (Place::Local(l), _) => {
                                b.ins(&Instruction::LocalGet(src));
                                b.ins(&Instruction::LocalSet(l));
                            }
                            _ => return unsupported("a `for` over a Unit value", *line),
                        }
                        self.register_rel(b, key, place, r);
                        b.ins(&Instruction::LocalGet(src));
                    }
                }
                let w = self.walk(b, &it, *line)?;
                let i = b.local(ValType::I64);
                b.ins(&Instruction::I64Const(0));
                b.ins(&Instruction::LocalSet(i));

                let brk = self.depth;
                b.ins(&Instruction::Block(BlockType::Empty));
                self.depth += 1;
                let top = self.depth;
                b.ins(&Instruction::Loop(BlockType::Empty));
                self.depth += 1;
                b.ins(&Instruction::LocalGet(i));
                b.ins(&Instruction::LocalGet(w.len));
                b.ins(&Instruction::I64GeU);
                let out = self.br_to(brk);
                b.ins(&Instruction::BrIf(out));

                // The loop variable is a COPY, so a body that grows the array
                // cannot leave it pointing into a buffer that was abandoned.
                let r = self.cx.repr(&w.elem, *line)?;
                let place = self.place_for(b, &r, *line)?;
                match (place, &r) {
                    (Place::Local(l), _) => {
                        self.elem_addr(b, &w, i);
                        self.load_elem(b, &w, *line)?;
                        b.ins(&Instruction::LocalSet(l));
                    }
                    (Place::Slot(off), Repr::Agg(el)) => {
                        b.slot(off);
                        self.elem_addr(b, &w, i);
                        b.ins(&Instruction::I32Const(el.size as i32));
                        b.ins(&Instruction::MemoryCopy {
                            src_mem: 0,
                            dst_mem: 0,
                        });
                    }
                    _ => return unsupported("an array of Unit", *line),
                }
                let mark = self.scope.len();
                self.scope.push((var.clone(), place, w.elem.clone()));
                // RFC-0125 M3: a variable the body drains a field of keeps
                // the rest of its element, and the placer's rows for it —
                // keyed by the variable's spelling, since it has no `let` —
                // release that rest at every exit of the body.
                let vkey = vyrn_frontend::own::for_var_key(var);
                if self.releases_whole(vkey) {
                    if let Some(r) = self.rel_for(&w.elem, *line)? {
                        self.register_rel(b, vkey, place, r);
                    }
                }

                let cont = self.depth;
                b.ins(&Instruction::Block(BlockType::Empty));
                self.depth += 1;
                self.loops.push((brk, cont, self.region_depth));
                self.block(m, b, body)?;
                self.loops.pop();
                self.depth -= 1;
                b.ins(&Instruction::End);
                self.scope.truncate(mark);

                b.ins(&Instruction::LocalGet(i));
                b.ins(&Instruction::I64Const(1));
                b.ins(&Instruction::I64Add);
                b.ins(&Instruction::LocalSet(i));
                let back = self.br_to(top);
                b.ins(&Instruction::Br(back));
                self.depth -= 1;
                b.ins(&Instruction::End);
                self.depth -= 1;
                b.ins(&Instruction::End);
                // The fall-through release (RFC-0092 M5), after every exit path
                // has rejoined. A body that returned already ran it and branched.
                self.emit_releases(m, b, ExitKind::Scrutinee, key)?;
                // `for x in consume xs` — the loop TOOK the container, so the
                // loop gives it back, and the core says so with a release of
                // its own rather than a row (RFC-0125 §3 M3, the walk's
                // deletion). The take is what the kernel judges at the loop:
                // it is where a consuming loop over a `read` parameter's field
                // or over module state is refused (the census, rows 10, 11 and
                // 29), so the core cannot leave it to an exit row — and a
                // release only the core states is one no row names, which is
                // why the two are exclusive here. The core says WHICH loop
                // gives one back; this pass no longer reads `consume` off the
                // statement to guess (RFC-0125 §3 M3, the event stream's
                // slice).
                if self.cx.loop_gives_back(key) && !self.releases_whole(key) {
                    if let Some(r) = self.rel_slots.get(&key).cloned() {
                        self.emit_rel(m, b, r.place, &r.rel, *line)?;
                        self.rel_slots.remove(&key);
                        self.rel_pending.retain(|(k, _)| *k != key);
                    }
                }
            }
            Stmt::IndexSet {
                name,
                index,
                value,
                line,
            } => {
                let (place, ty) = self.lookup(name, *line)?;
                // The store dispatches exactly as the read does (RFC-0091 M2):
                // `a[i] = v` asks the receiver's type for `place atSet`, and
                // the seeded row yields this binding's own element.
                // A user container's store is its own statement group, lowered
                // by the statements this backend already has.
                if let Some(blk) =
                    vyrn_frontend::project::store_index(&self.cx.impls, name, index, value, &ty)?
                {
                    // The projection's own statements decide the release.
                    // RFC-0125 §3 M3, the store slice: the core judged THIS
                    // statement and this pass walks the expansion, so the
                    // store inside it is pointed back at the node the answer
                    // is filed under. The expansion is memoized and leaked,
                    // so the pair outlives every walk that reads it.
                    if let Some(st) = vyrn_frontend::project::store_node(blk) {
                        self.cx.plan.alias_clones(&[(
                            st as *const Stmt as usize,
                            s as *const Stmt as usize,
                        )]);
                    }
                    return self.block(m, b, blk);
                }
                // RFC-0125 M1: a header a `while` hoisted is already in
                // locals, and an element store moves no header.
                let cached = self.walks.get(name.as_str()).cloned();
                if cached.is_none() {
                    place
                        .addr(b, 0)
                        .ok_or_else(|| gap("an element assignment to a non-array", *line))?;
                }
                // `m[k] = v` (RFC-0028) inserts or updates; it is not a bounded
                // element store and has no index to check.
                if let Type::Map(key_t, val) = self.cx.resolve(&ty) {
                    let l = self.layout_of(&ty, *line)?;
                    let hdr = b.local(ValType::I32);
                    b.ins(&Instruction::LocalSet(hdr));
                    // Rule 4 through an entry. Two questions, not the element
                    // store's three, for the reason [`crate::Gen::gen_stmt`]
                    // states at its own map arm: a map owns its values outright,
                    // so who owns the MAP does not change who owns the value this
                    // store displaces. The arena and aliasing are what is asked.
                    let drop_old = !vyrn_frontend::movecheck::mentions_place(value, name)
                        && !vyrn_frontend::movecheck::mentions_place(index, name);
                    // The entry's release is `map_set`'s own two questions.
                    return self
                        .map_set(m, b, hdr, &l, index, value, &key_t, &val, drop_old, *line);
                }
                let w = match cached {
                    Some(w) => w,
                    None => self.walk(b, &ty, *line)?,
                };
                if w.byte {
                    return unsupported("an element assignment into a String", *line);
                }
                self.expr_as(m, b, index, &Type::Int)?;
                let i = b.local(ValType::I64);
                b.ins(&Instruction::LocalSet(i));
                self.bounds_check(b, &w, i, false);
                self.elem_addr(b, &w, i);
                let elem = w.elem.clone();
                // Rule 4 through an element. The element address is already on the
                // stack, so it is teed rather than recomputed; the snapshot is
                // stack-neutral and the store finds its address where it left it.
                let snap = if self.cx.store_row(s as *const Stmt as usize) {
                    let ea = b.local(ValType::I32);
                    b.ins(&Instruction::LocalTee(ea));
                    self.snap_at(b, ea, &elem, *line)?
                } else {
                    Vec::new()
                };
                match self.cx.repr(&elem, *line)? {
                    Repr::Scalar(_) => {
                        self.expr_as(m, b, value, &elem)?;
                        b.ins(&store_of(&self.cx.ll(&elem)));
                    }
                    // RFC-0125 M1: built in the element when the value cannot
                    // see the binding while it is made. The element address
                    // moves off the stack into a local so the fields can be
                    // addressed from it.
                    Repr::Agg(el) => {
                        let a = b.local(ValType::I32);
                        b.ins(&Instruction::LocalSet(a));
                        let fresh = !observes(value, name);
                        self.agg_into(m, b, Dest::Addr(a, 0), el.size, value, &elem, fresh)?;
                    }
                    Repr::Unit => return unsupported("an array of Unit", *line),
                }
                self.free_snap(b, snap.as_slice());
            }
            Stmt::Break { line } => {
                let &(brk, _, regions) = self
                    .loops
                    .last()
                    .ok_or_else(|| gap("`break` outside a loop", *line))?;
                self.emit_releases(m, b, ExitKind::Break, s as *const Stmt as usize)?;
                self.exit_regions_above(b, regions, true);
                let d = self.br_to(brk);
                b.ins(&Instruction::Br(d));
            }
            Stmt::Continue { line } => {
                let &(_, cont, regions) = self
                    .loops
                    .last()
                    .ok_or_else(|| gap("`continue` outside a loop", *line))?;
                self.emit_releases(m, b, ExitKind::Continue, s as *const Stmt as usize)?;
                self.exit_regions_above(b, regions, true);
                let d = self.br_to(cont);
                b.ins(&Instruction::Br(d));
            }
            // `region { .. }` (RFC-0004 §4). An arena scope, and in this backend
            // that is a counter and its trap — see `region_exit` for why the arena
            // itself is the allocator's ceiling rather than a region-shaped hole.
            //
            // The body is an ordinary block, so its scope and its release frame
            // come free; a region is one more frame the exit edges close, the
            // same shape M2l gave the inferred release. No `if !terminated` guard
            // like the textual backend's: a fall-through exit after a `br` is code
            // wasm has already marked unreachable, which is the same argument
            // `Fn_::block` makes about its own releases.
            Stmt::Region { body, .. } => {
                self.region_enter(b);
                self.region_depth += 1;
                let r = self.block(m, b, body);
                self.region_depth -= 1;
                let mark = self.region_marks.pop().expect("one mark per open region");
                r?;
                self.region_exit(b, mark);
            }
            Stmt::Drop { name, line } => {
                let (place, ty) = self.lookup(name, *line)?;
                // RFC-0095 M1. A task is linear, and `drop t` is the discharge
                // that does not want the result. `rel_for` does not answer for a
                // `Task` — an automatic block-exit row would free what the join
                // already freed — so the release is emitted here, exactly as the
                // textual backend emits it.
                //
                // There is no wait: this target has no threads, so the thunk ran
                // at the spawn point and the box holds a finished result. What is
                // left is the half a `Task<Int64>` makes invisible — the RESULT
                // is released by its type before the box goes, because a dropped
                // `Task<String>` has a String in that box and nothing else will
                // ever free it.
                if let Type::Task(inner) = self.cx.resolve(&ty) {
                    let box_ = b.local(ValType::I32);
                    // A `Task` is one word, so it is a scalar: a local holds the
                    // box address itself, and every other place holds it at an
                    // address (`Rel::Str` reads its own place the same way).
                    match place {
                        Place::Local(l) => {
                            b.ins(&Instruction::LocalGet(l));
                        }
                        _ => {
                            place
                                .addr(b, 0)
                                .ok_or_else(|| gap("a Task with no place", *line))?;
                            b.ins(&Instruction::I32Load(word()));
                        }
                    }
                    b.ins(&Instruction::LocalSet(box_));
                    self.rel_at(m, b, box_, &inner, *line)?;
                    b.ins(&Instruction::LocalGet(box_));
                    b.ins(&Instruction::Call(self.cx.rt.free));
                    return Ok(());
                }
                if let Some(r) = self.rel_for(&ty, *line)? {
                    self.emit_rel(m, b, place, &r, *line)?;
                }
            }
            Stmt::Expr(e) => {
                // A call for its effect leaves its result on the stack; drop it,
                // or the block's type will not check. Round twenty-eight: an
                // OWNED result nothing binds is freed rather than dropped.
                let ty = self.expr(m, b, e)?;
                let line = Expr::line(e);
                match self.cx.repr(&ty, line)? {
                    Repr::Unit => {}
                    _ if self.cx.discarded_row(s as *const Stmt as usize) => {
                        let l = b.local(ValType::I32);
                        b.ins(&Instruction::LocalSet(l));
                        self.free_arg_temp(m, b, l, &ty, line)?;
                    }
                    _ => {
                        b.ins(&Instruction::Drop);
                    }
                }
            }
        }
        Ok(())
    }

    /// A boolean in an `if`/`while` position.
    fn cond(&mut self, m: &mut Module, b: &mut Frame, e: &Expr, line: usize) -> Result<(), String> {
        let t = self.expr(m, b, e)?;
        match self.cx.resolve(&t) {
            Type::Bool => Ok(()),
            _ => unsupported("a non-boolean condition", line),
        }
    }

    /// Where a new binding of representation `r` lives.
    fn place_for(&mut self, b: &mut Frame, r: &Repr, line: usize) -> Result<Place, String> {
        Ok(match r {
            Repr::Scalar(v) => Place::Local(b.local(*v)),
            Repr::Agg(l) => Place::Slot(b.alloc(l.size, l.align)),
            Repr::Unit => return unsupported("a binding of a Unit value", line),
        })
    }

    /// Give the accumulator in wasm local `l` its ownership flag, and start it
    /// unowned.
    ///
    /// This was a `(len, cap)` shadow until RFC-0089 M1a, because a String
    /// carried neither. Both are in the String header now, and the one word left
    /// is the question the header cannot answer: did THIS path allocate the
    /// buffer? `0` means no — a literal in a data segment, a `concat` result, a
    /// call result — so it may not be grown in place, because `s = t` may alias
    /// it and nothing yet forbids that (the conventions do, RFC-0089 M2).
    ///
    /// It goes in the frame rather than in another wasm local because the runtime
    /// helper writes it back and wasm has no way to pass a local by reference —
    /// four bytes of shadow stack against a two-result function type, and the
    /// frame is already per-invocation, so a recursive writer (`emitArr` calling
    /// `emit`) gets its own without anything being said about recursion.
    ///
    /// Emitted at the `let`, so the second trip through an enclosing loop starts
    /// unowned again.
    fn str_append_shadow(&mut self, b: &mut Frame, l: u32, owns: bool) {
        let at = *self.str_append.entry(l).or_insert_with(|| b.alloc(4, 4));
        b.slot(at)
            .ins(&Instruction::I32Const(owns as i32))
            .ins(&Instruction::I32Store(word()));
    }

    /// One in-place append into `place`: `own` is the ownership word's place, and
    /// the helper hands back the pointer to store, because a wasm local has no
    /// address to write through (RFC-0081).
    ///
    /// Two shapes, because the destination has two. A local is set; a global is
    /// stored to a fixed address, which has to go down BEFORE the call, so the
    /// result lands on top of it.
    fn append_once(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        place: Place,
        own: Place,
        part: &Expr,
    ) -> Result<(), String> {
        let line = Expr::line(part);
        match place {
            // The append COPIES the operand into the accumulator, so an operand
            // this statement allocated is released after it (RFC-0096 M3).
            // `s = s + i.toString()` reaches the same `@str` temporary the
            // general `+` path frees, through the fast path instead.
            Place::Local(l) => {
                own.addr(b, 0)
                    .ok_or_else(|| gap("an append flag with no address", line))?;
                b.ins(&Instruction::LocalGet(l));
                self.expr_as(m, b, part, &Type::Str)?;
                let k = self.tee_str_temp(b, part);
                b.ins(&Instruction::Call(self.cx.rt.str_append));
                b.ins(&Instruction::LocalSet(l));
                self.free_str_temp(b, k);
            }
            Place::Static(at) => {
                b.ins(&Instruction::I32Const(at as i32));
                own.addr(b, 0)
                    .ok_or_else(|| gap("an append flag with no address", line))?;
                b.ins(&Instruction::I32Const(at as i32))
                    .ins(&Instruction::I32Load(word()));
                self.expr_as(m, b, part, &Type::Str)?;
                let k = self.tee_str_temp(b, part);
                b.ins(&Instruction::Call(self.cx.rt.str_append));
                b.ins(&Instruction::I32Store(word()));
                self.free_str_temp(b, k);
            }
            Place::Slot(_) => return unsupported("an in-place append into a slot", line),
        }
        Ok(())
    }

    /// Evaluate `value` into an existing place of known type. `fresh` says the
    /// place is storage nothing can name yet (a `let`'s slot), so an aggregate
    /// may be built in it directly — see [`Dest`].
    #[allow(clippy::too_many_arguments)]
    fn store_into(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        place: Place,
        r: &Repr,
        value: &Expr,
        ty: &Type,
        fresh: bool,
    ) -> Result<(), String> {
        match (place, r) {
            (Place::Local(l), _) => {
                self.expr_as(m, b, value, ty)?;
                b.ins(&Instruction::LocalSet(l));
            }
            // Destination-first, exactly as at a join: the address goes down
            // before the value is built, so an aggregate has somewhere to be
            // copied to. A `Static` destination is the same shape with a constant
            // address, which is why module state needed no new store path.
            (Place::Slot(_) | Place::Static(_), Repr::Agg(l)) => match Dest::of(place) {
                Some(d) => self.agg_into(m, b, d, l.size, value, ty, fresh)?,
                None => {
                    place.addr(b, 0);
                    self.expr_as(m, b, value, ty)?;
                    b.ins(&Instruction::I32Const(l.size as i32));
                    b.ins(&Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                }
            },
            (Place::Static(_), Repr::Scalar(_)) => {
                place.addr(b, 0);
                self.expr_as(m, b, value, ty)?;
                b.ins(&store_of(&self.cx.ll(ty)));
            }
            _ => return unsupported("a store of a Unit value", Expr::line(value)),
        }
        Ok(())
    }

    /// Evaluate the aggregate `value`, of type `ty` and `size` bytes, and leave
    /// it at `dest` (RFC-0125 M1). With `in_place`, a literal or a call whose
    /// type is `ty` writes there directly and the copy is skipped; anything
    /// else — a variable, a field read, a coercion — is built where it is built
    /// and copied. Without it, only the copy: the caller could not show the
    /// destination is unnamed while the value is made (see [`Dest`]).
    ///
    /// The destination's address goes down before the value either way, the
    /// order every other store in this file uses; when the value landed in
    /// place, the two addresses on the stack are dropped instead of copied.
    #[allow(clippy::too_many_arguments)]
    fn agg_into(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        size: u32,
        value: &Expr,
        ty: &Type,
        in_place: bool,
    ) -> Result<(), String> {
        dest.addr(b, 0);
        self.dest_hint = in_place.then(|| (dest, ty.clone()));
        self.dest_used = false;
        let r = self.expr_as(m, b, value, ty);
        self.dest_hint = None;
        let used = std::mem::take(&mut self.dest_used);
        r?;
        if used {
            b.ins(&Instruction::Drop);
            b.ins(&Instruction::Drop);
        } else {
            b.ins(&Instruction::I32Const(size as i32));
            b.ins(&Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });
        }
        Ok(())
    }

    /// The offset and type of `field` within `ty`.
    fn field_of(&self, ty: &Type, field: &str, line: usize) -> Result<(u32, Type), String> {
        let fs = self
            .cx
            .fields(ty)
            .ok_or_else(|| gap(&format!("a field of the non-record type `{ty}`"), line))?;
        let i = fs
            .iter()
            .position(|f| f.name == field)
            .ok_or_else(|| gap(&format!("the field `{field}`"), line))?;
        let l = layout::of_ll(&self.cx.ll(ty)).map_err(|e| format!("direct backend: {e}"))?;
        Ok((l.fields[i], fs[i].ty.clone()))
    }

    // -- expressions --------------------------------------------------------

    /// Evaluate `e`, leaving a value of type `want` on the stack.
    fn expr_as(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        e: &Expr,
        want: &Type,
    ) -> Result<(), String> {
        self.expect.push(want.clone());
        let mark = self.arg_frees.len();
        let got = self.expr(m, b, e);
        self.expect.pop();
        let got = got?;
        self.coerce(m, b, Some(e), &got, want, Expr::line(e))?;
        // RFC-0114 §25 round three: a `[..]` argument's recorded temporary
        // (`@heapify`) is teed by `expr` on the FIXED value, before the
        // conversion above builds the heap triple the record is actually
        // about. Freeing the fixed one walks frame memory whose element
        // pointers the triple now shares — so the pending free is retargeted
        // at the triple, exactly as the textual backend's call loops do.
        if self.arg_frees.len() > mark {
            let want_r = self.cx.resolve(want);
            if matches!(self.cx.resolve(&got), Type::ArrayN(..))
                && matches!(want_r, Type::Array(_))
                && self.arg_frees.last().is_some_and(|(_, t)| *t == got)
            {
                let l = b.local(ValType::I32);
                b.ins(&Instruction::LocalTee(l));
                if let Some(last) = self.arg_frees.last_mut() {
                    *last = (l, want_r);
                }
            }
        }
        Ok(())
    }

    /// Reconcile the value on the stack, of type `from`, into `to`, by the rung
    /// [`crate::coerce_plan`] places for the pair.
    ///
    /// **The seam** (RFC-0077 M2d). Before this the backend had no coercion
    /// concept at all: it lowered when `repr()` already agreed on both sides and
    /// [`Cx::ty_gap`] refused everything needing reconciliation — which is why a
    /// validated type, a `modify` parameter, a `SmallArray`, a `Map` index and a
    /// two-word `Option` payload were five gaps rather than one absence wearing
    /// five hats. Every flow site reaches here through [`Fn_::expr_as`]: a typed
    /// `let`, an assignment, a field or element store, a call argument, a return,
    /// a join arm, an enum payload. A reconciliation added here is added at all
    /// of them at once, which is the property the five separate refusals lacked.
    ///
    /// **The decision is not here** — RFC-0125 §2.3, and §3 M6's coercion
    /// census. This emitter used to restate the ladder, in an order the textual
    /// emitter did not share, and the corpus gate existed to say where the two
    /// orders came apart. The guards are gone; what is left is one arm per rung.
    /// The order that mattered — validation before the shape shortcut, and the
    /// integer rung before it as well, because `llt` prints `i8` for `Int8` and
    /// `UInt8` alike — is the plan's now, and is written down there.
    ///
    /// `expr` is the expression that produced the value, when there is one — only
    /// RFC-0020's containment proof needs it, and only for strings.
    fn coerce(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        expr: Option<&Expr>,
        from: &Type,
        to: &Type,
        line: usize,
    ) -> Result<(), String> {
        // Substituted, not resolved. M2d's rule is that a declared spelling must
        // survive to here or the boundary is not a boundary — but a `Param` is a
        // spelling that says nothing until the monomorphization fills it in, so
        // it is the one thing that MUST be reduced before the plan looks at it: a
        // `T` where `T = Age` is an `Age` flow, and a `Param` would silently be
        // neither `Named` nor a boundary.
        let (from, to) = (&self.cx.sub(from), &self.cx.sub(to));
        let rung = crate::coerce_plan(from, to, &self.cx.types);
        crate::observe::note_rung(crate::observe::Site::Wasm, from, to, rung);
        match rung {
            // A `Never` (RFC-0079) reached this seam from a `panic`, which left
            // nothing on the stack and ended the block in `unreachable`. There is
            // no value to reconcile and no validation to owe — the polymorphic
            // stack after `unreachable` satisfies `to` on its own.
            crate::Rung::Never => Ok(()),
            crate::Rung::Validate => {
                let Some(decl) = crate::validation_required(from, to, &self.cx.types).cloned()
                else {
                    return Err(crate::plan_disagrees(from, to, rung));
                };
                // The value has to be in the base's representation before the
                // predicate reads it. The recursion terminates because a base is
                // one step nearer a builtin than the name it backs.
                self.coerce(m, b, expr, from, &decl.base, line)?;
                if !expr.is_some_and(|e| self.proven(e, to)) {
                    self.emit_validation(b, &decl, line)?;
                }
                Ok(())
            }
            // An integer resize. Widening reads the SOURCE's signedness (a
            // `UInt8` zero-extends, an `Int8` sign-extends); narrowing discards
            // bits and renormalizes into the TARGET's. That is the interpreter's
            // `wrap_intn` and the textual backend's `sext`/`zext`/`trunc`, and
            // both stop being separate rules the moment [`Num`]'s invariant is
            // written down.
            crate::Rung::Resize => {
                let (Some(f), Some(t)) = (
                    Num::of(&self.cx.resolve(from)),
                    Num::of(&self.cx.resolve(to)),
                ) else {
                    return Err(crate::plan_disagrees(from, to, rung));
                };
                match (f == t, f.wide(), t.wide()) {
                    (true, ..) => {}
                    (_, false, true) => widen(b, f),
                    (_, true, false) => {
                        b.ins(&Instruction::I32WrapI64);
                        renorm(b, t);
                    }
                    // Both carriers are `i64`, so only the signedness changed and
                    // the bits do not move.
                    (_, true, true) => {}
                    // Both in an `i32`: the source's representation already holds
                    // the bits, and only the target's normalization is owed.
                    (_, false, false) => renorm(b, t),
                }
                Ok(())
            }
            // Across the int/float line, and between the two float widths.
            //
            // `trunc_sat` rather than `trunc`: wasm's plain `i64.trunc_f64_s`
            // TRAPS out of range, where LLVM's `fptosi` is undefined and Rust's
            // `as` saturates — and the interpreter IS Rust's `as`, which is the
            // answer this emitter is compared against.
            //
            // Float → sized int goes through 64 bits FIRST and narrows after,
            // because that is what the interpreter does (`f as i64`, then
            // `wrap_intn`) and the two genuinely disagree: `Int8(1e10)` is 0
            // through an `i64` and -1 through an `i32` whose saturation clamped
            // at `i32::MAX`.
            crate::Rung::FloatCross => {
                let (fr, tr) = (self.cx.resolve(from), self.cx.resolve(to));
                let flt = |t: &Type| match t {
                    Type::Float => Some(true),
                    Type::Float32 => Some(false),
                    _ => None,
                };
                match (Num::of(&fr), flt(&fr), Num::of(&tr), flt(&tr)) {
                    (Some(f), _, _, Some(wide)) => {
                        widen(b, f);
                        b.ins(match (wide, f.signed) {
                            (true, true) => &Instruction::F64ConvertI64S,
                            (true, false) => &Instruction::F64ConvertI64U,
                            (false, true) => &Instruction::F32ConvertI64S,
                            (false, false) => &Instruction::F32ConvertI64U,
                        });
                        Ok(())
                    }
                    (_, Some(wide), Some(t), _) => {
                        b.ins(match (wide, t.signed) {
                            (true, true) => &Instruction::I64TruncSatF64S,
                            (true, false) => &Instruction::I64TruncSatF64U,
                            (false, true) => &Instruction::I64TruncSatF32S,
                            (false, false) => &Instruction::I64TruncSatF32U,
                        });
                        if !t.wide() {
                            b.ins(&Instruction::I32WrapI64);
                            renorm(b, t);
                        }
                        Ok(())
                    }
                    (_, Some(f), _, Some(t)) if f != t => {
                        b.ins(if f {
                            &Instruction::F32DemoteF64
                        } else {
                            &Instruction::F64PromoteF32
                        });
                        Ok(())
                    }
                    _ => Err(crate::plan_disagrees(from, to, rung)),
                }
            }
            // A function value between `fn`-typed spellings. This emitter has no
            // instruction for it and never had a rung of its own: the structural
            // spelling and every named alias share the `{ i64, i64 }` shape, so
            // its shape shortcut used to answer first. It is a rung with an empty
            // arm now, which is the same thing said once (RFC-0125 §3 M6).
            crate::Rung::FnRetag => Ok(()),
            // Fixed arrays whose ELEMENT type changes. The textual emitter
            // unrolls a per-element crossing; this one has no lowering for that,
            // and a pair whose elements share a shape needs none.
            crate::Rung::Elementwise => {
                if self.cx.ll(from) == self.cx.ll(to) {
                    return Ok(());
                }
                unsupported(
                    &format!("an element-wise conversion from `{from}` to `{to}`"),
                    line,
                )
            }
            // A literal is a fixed `[N x T]`; an `Array<T>` slot wants the
            // growable triple. One conversion, so every literal position — a
            // `let`, an argument, a `return`, a field, an element — reaches the
            // heap the same way.
            crate::Rung::Heapify => {
                let Type::ArrayN(inner, n) = self.cx.resolve(from) else {
                    return Err(crate::plan_disagrees(from, to, rung));
                };
                self.heapify(b, &inner, n, to, line)
            }
            // The same literal in a `SmallArray<T, N>` position stays OFF the
            // heap: the elements are copied into the inline buffer and `cap` is
            // set to `N`, which is the state discriminant (RFC-0056). The checker
            // proved `len <= N`.
            crate::Rung::Inline => {
                let (Type::ArrayN(inner, len), Type::SmallArray(_, n)) =
                    (self.cx.resolve(from), self.cx.resolve(to))
                else {
                    return Err(crate::plan_disagrees(from, to, rung));
                };
                self.sa_from_fixed(b, &inner, len, to, n, line)
            }
            // The bits are already right.
            crate::Rung::Identity => Ok(()),
            // RFC-0002's record width subtyping: a wider record used as a
            // narrower one. A rebuild rather than a prefix, because the two field
            // orders need not agree — the shapes are the same length only by
            // coincidence.
            crate::Rung::Rebuild => {
                let (got, want) = (from, to);
                let (Some(ff), Some(tf)) = (self.cx.fields(got), self.cx.fields(want)) else {
                    return Err(crate::plan_disagrees(from, to, rung));
                };
                let src = self.scratch(b, ValType::I32, 0);
                b.ins(&Instruction::LocalSet(src));
                let l = self.cx.repr(want, line)?;
                let Repr::Agg(dl) = &l else {
                    return unsupported("a record that is not an aggregate", line);
                };
                let off = b.alloc(dl.size, dl.align);
                let sl =
                    layout::of_ll(&self.cx.ll(got)).map_err(|e| format!("direct backend: {e}"))?;
                for (i, f) in tf.iter().enumerate() {
                    let j = ff
                        .iter()
                        .position(|g| g.name == f.name)
                        .ok_or_else(|| gap(&format!("the field `{}`", f.name), line))?;
                    if self.cx.ll(&ff[j].ty) != self.cx.ll(&f.ty) {
                        return unsupported(
                            "a record conversion that changes a field's shape",
                            line,
                        );
                    }
                    match self.cx.repr(&f.ty, line)? {
                        Repr::Scalar(_) => {
                            b.slot(off + dl.fields[i]);
                            b.ins(&Instruction::LocalGet(src));
                            b.ins(&load_of(
                                &self.cx.ll(&f.ty),
                                sl.fields[j],
                                self.cx.signed(&f.ty),
                            ));
                            b.ins(&store_of(&self.cx.ll(&f.ty)));
                        }
                        Repr::Agg(fl) => {
                            b.slot(off + dl.fields[i]);
                            b.ins(&Instruction::LocalGet(src));
                            b.ins(&Instruction::I32Const(sl.fields[j] as i32));
                            b.ins(&Instruction::I32Add);
                            b.ins(&Instruction::I32Const(fl.size as i32));
                            b.ins(&Instruction::MemoryCopy {
                                src_mem: 0,
                                dst_mem: 0,
                            });
                        }
                        Repr::Unit => return unsupported("a Unit field", line),
                    }
                }
                b.slot(off);
                Ok(())
            }
            // Two shapes of one sum (RFC-0126 §8.4). The tag is at offset 0 and
            // the slots follow it, so the bytes both shapes have are a prefix:
            // zero the destination, then copy that prefix.
            crate::Rung::Reshape => {
                let src = self.scratch(b, ValType::I32, 0);
                b.ins(&Instruction::LocalSet(src));
                let Repr::Agg(dl) = self.cx.repr(to, line)? else {
                    return unsupported("a sum that is not an aggregate", line);
                };
                let sl =
                    layout::of_ll(&self.cx.ll(from)).map_err(|e| format!("direct backend: {e}"))?;
                let off = b.alloc(dl.size, dl.align);
                b.slot(off);
                b.ins(&Instruction::I32Const(0));
                b.ins(&Instruction::I32Const(dl.size as i32));
                b.ins(&Instruction::MemoryFill(0));
                b.slot(off);
                b.ins(&Instruction::LocalGet(src));
                b.ins(&Instruction::I32Const(dl.size.min(sl.size) as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                b.slot(off);
                Ok(())
            }
            // The end of the ladder, and the textual emitter reaches the same one
            // now (RFC-0101 §1.5 recorded that the two ends differed).
            crate::Rung::Refuse => {
                unsupported(&format!("a conversion from `{from}` to `{to}`"), line)
            }
        }
    }

    /// RFC-0020's containment escape: a string flow the checker proved lands
    /// inside `to`'s language needs no runtime check.
    ///
    /// Both backends run the same frontend predicate over the same AST rather
    /// than agreeing by construction — the consteval precedent, and the reason
    /// `lib.rs::coerce_flow` exists at all. Skipping differently here would show
    /// up as a trap on one target only.
    fn proven(&self, e: &Expr, to: &Type) -> bool {
        let resolve = |x: &Expr| match x {
            Expr::Var { name, .. } => self.lookup(name, 0).ok().map(|(_, t)| t),
            _ => None,
        };
        vyrn_frontend::finite::string_flow_proven(e, to, &self.cx.types, &resolve)
    }

    /// Emit the check that the value on the stack satisfies `decl`'s `where`
    /// predicate: a CALL to the program's own constructor for the type, which
    /// traps with the canonical message when it does not hold.
    ///
    /// RFC-0125 §3 M6, the third judgment's fourth slice. This backend used to
    /// lower the predicate itself — bind what [`crate::predicate_binds`] names,
    /// walk the clause, spell the trap — and so did the LLVM emitter and the
    /// interpreter. The predicate is generated Vyrn now
    /// ([`vyrn_frontend::ctor`]), so the census's `where-scalar` and
    /// `where-record` rows are one body every engine calls.
    ///
    /// The value is LEFT on the stack, because a validation is a check on a
    /// flow and not a step in it. Parked in a local first, since the call
    /// consumes what it is given: three instructions where the old site was a
    /// binding walk.
    fn emit_validation(
        &mut self,
        b: &mut Frame,
        decl: &TypeDecl,
        line: usize,
    ) -> Result<(), String> {
        if decl.predicate.is_none() {
            return Ok(());
        }
        let held = self.park_for_predicate(b, decl, line)?;
        let name = vyrn_frontend::ctor::ctor_name(&decl.name);
        let Some(sig) = self.cx.sigs.get(&name) else {
            return unsupported(
                &format!(
                    "a `where` clause on `{}` with no constructor in the link",
                    decl.name
                ),
                line,
            );
        };
        let index = sig.index;
        b.ins(&Instruction::LocalGet(held));
        b.ins(&Instruction::Call(index));
        b.ins(&Instruction::LocalGet(held));
        Ok(())
    }

    /// Consume the value on the stack and leave `decl`'s `where` predicate's
    /// answer (a Bool) there instead, giving the local the value was parked in —
    /// or `None`, stack untouched, for a type with no refinement.
    ///
    /// Split from [`Fn_::emit_validation`] because a fallible construction wants
    /// the same answer without the trap (RFC-0077 M2k). It is the same generated
    /// function the constructor calls, so `Age?(n)` and `Age(n)` cannot read a
    /// different `value`.
    ///
    /// Scalar bases only, which is what the one caller allows: a record base
    /// binds by field and has no single word to become an `Option`'s payload.
    fn predicate_holds(
        &mut self,
        b: &mut Frame,
        decl: &TypeDecl,
        line: usize,
    ) -> Result<Option<u32>, String> {
        if decl.predicate.is_none() {
            return Ok(None);
        }
        let held = self.park_for_predicate(b, decl, line)?;
        let name = vyrn_frontend::ctor::pred_name(&decl.name);
        let Some(sig) = self.cx.sigs.get(&name) else {
            return unsupported(
                &format!(
                    "a `where` clause on `{}` with no predicate in the link",
                    decl.name
                ),
                line,
            );
        };
        let index = sig.index;
        b.ins(&Instruction::LocalGet(held));
        b.ins(&Instruction::Call(index));
        Ok(Some(held))
    }

    /// Park the value on the stack in a local, so a generated call can be given
    /// it and the flow can carry on with it afterwards.
    ///
    /// An aggregate base is on the stack as its ADDRESS, which is what a `read`
    /// parameter of that type is passed as ([`Fn_::emit_call`]), so one local
    /// holds either shape.
    fn park_for_predicate(
        &mut self,
        b: &mut Frame,
        decl: &TypeDecl,
        line: usize,
    ) -> Result<u32, String> {
        let v = match self.cx.repr(&decl.base, line)? {
            Repr::Scalar(v) => v,
            Repr::Agg(_) => ValType::I32,
            Repr::Unit => {
                return unsupported(
                    &format!("a `where` clause over the Unit base `{}`", decl.base),
                    line,
                )
            }
        };
        let held = b.local(v);
        b.ins(&Instruction::LocalSet(held));
        Ok(held)
    }

    /// Evaluate `e`, leaving its value (a scalar) or its address (an aggregate)
    /// on the stack, and giving the Vyrn type of what it left.
    ///
    /// The wrapper keeps one fact: whether this expression is a call argument
    /// whose value the CALLER releases once the call is done with it
    /// (`rfcs/census-call-arguments.md`). `own` decided that, per argument node;
    /// this tees the pointer into a local so [`Fn_::call`] can hand it back after
    /// the call. The tee is HERE — where the argument is evaluated — rather than
    /// at the call, so the evaluation order stays the one the program wrote.
    ///
    /// It asks nothing about a `region`. It used to stand down inside one, on
    /// the argument that the arena was the single owner there; the arena owns
    /// what [`Fn_::arena_route`] routes into it and nothing else, and `free`
    /// refuses one of its blocks by the class word in its header. So the
    /// temporary is handed back at whatever depth it was made.
    fn expr(&mut self, m: &mut Module, b: &mut Frame, e: &Expr) -> Result<Type, String> {
        if let Some(f) = expr_form(e) {
            count(f, false);
        }
        let t = self.expr_inner(m, b, e)?;
        if self.cx.arg_drop_row(e as *const Expr as usize) {
            let l = b.local(ValType::I32);
            b.ins(&Instruction::LocalTee(l));
            self.arg_frees.push((l, t.clone()));
        }
        if crate::observe::on() {
            crate::observe::record(
                crate::observe::Site::Wasm,
                crate::observe::kind_of(e),
                e as *const Expr as usize,
                &self.cx.subst,
                &t,
            );
        }
        Ok(t)
    }

    /// The walk itself. Every arm leaves exactly one value (or none, for
    /// `Unit`) on the stack, which is what lets the wrapper above tee it.
    fn expr_inner(&mut self, m: &mut Module, b: &mut Frame, e: &Expr) -> Result<Type, String> {
        // RFC-0125 M1: the consumer's storage, for THIS node only. Taken here so
        // that a literal or a call nested anywhere below cannot claim it.
        let hint = self.dest_hint.take();
        Ok(match e {
            // RFC-0093: a take is the load the read already emits. The `.copy()`
            // that used to follow it is what the take removes, so the emitted
            // output is strictly smaller and never a new shape.
            Expr::Consume { place, .. } => self.expr(m, b, place)?,
            Expr::Int(v) => {
                b.ins(&Instruction::I64Const(*v));
                Type::Int
            }
            Expr::Byte(v) => {
                b.ins(&Instruction::I64Const(*v as i64));
                Type::Int
            }
            Expr::Bool(v) => {
                b.ins(&Instruction::I32Const(*v as i32));
                Type::Bool
            }
            // A float literal is `Float64`; a `Float32` position demotes it, which
            // is what the interpreter's `f as f32` does to the same parsed double.
            Expr::Float(v) => {
                b.ins(&Instruction::F64Const((*v).into()));
                Type::Float
            }
            Expr::Str(s) => {
                let at = self.cx.rt.intern(m, s);
                b.ins(&Instruction::I32Const(at as i32));
                Type::Str
            }
            // A lambda in a value position (RFC-0037): the slot's declared
            // signature types it, and there is nothing else that could.
            Expr::Lambda { line, .. } => {
                let Some(sig) = self.expected_fn_sig() else {
                    return unsupported("a lambda with no expected function type", *line);
                };
                self.fnval_lambda(m, b, e, &sig)?
            }
            // A `fn`-typed parameter used as a VALUE rather than called
            // (RFC-0037 × RFC-0023) — stored into a Map, an Array, a record field,
            // or captured. Its target and captures are statically known here, so it
            // materializes the same aggregate a lambda source would.
            Expr::Var { name, line } if self.fn_binds.contains_key(name) => {
                let bnd = self.fn_binds[name].clone();
                self.fnval_binding(m, b, &bnd, *line)?
            }
            // A nullary constructor (`None`, or an enum's `Empty`) parses as a
            // bare name, so it is only distinguishable from a local by failing
            // to be one.
            Expr::Var { name, line }
                if self.lookup(name, *line).is_err()
                    && (name == "None" || self.cx.variants.contains_key(name)) =>
            {
                match self.sum_ctor(m, b, name, &[], *line, hint)? {
                    Some(t) => t,
                    None => return unsupported(&format!("the name `{name}`"), *line),
                }
            }
            // A bare function name as a value (RFC-0037): the empty-payload
            // variant, which is the whole of `let f = double`.
            Expr::Var { name, line }
                if self.lookup(name, *line).is_err() && self.cx.sigs.contains_key(name) =>
            {
                self.fnval_named(m, b, name, *line)?
            }
            Expr::Var { name, line } => {
                let (place, t) = self.lookup(name, *line)?;
                match place {
                    Place::Local(l) => {
                        b.ins(&Instruction::LocalGet(l));
                    }
                    Place::Slot(off) => {
                        b.slot(off);
                    }
                    // A global aggregate IS its address, like a slot; a global
                    // scalar has to be loaded out of memory, which is the one way
                    // module state differs from a local at a read.
                    Place::Static(at) => match self.cx.repr(&t, *line)? {
                        Repr::Scalar(_) => {
                            b.ins(&Instruction::I32Const(at as i32));
                            b.ins(&load_of(&self.cx.ll(&t), 0, self.cx.signed(&t)));
                        }
                        _ => {
                            b.ins(&Instruction::I32Const(at as i32));
                        }
                    },
                }
                t
            }
            Expr::Field { expr, field, line } => {
                let base = self.expr(m, b, expr)?;
                // RFC-0114 R1′: an unnamed String receiver this frame owns is
                // freed right after the header read — the pointer is teed to a
                // local before `length_of` consumes it.
                let row = self.cx.receiver_row(e as *const Expr as usize);
                let rfree = row.is_some();
                let tee = if rfree {
                    let l = b.local(ValType::I32);
                    b.ins(&Instruction::LocalTee(l));
                    Some(l)
                } else {
                    None
                };
                if let Some(t) = self.length_of(b, &base, field, *line)? {
                    // `own` admits only silent kinds into the set, so this
                    // never meets a declared release.
                    if let Some(l) = tee {
                        self.free_arg_temp(m, b, l, &base, *line)?;
                    }
                    return Ok(t);
                }
                let (off, fty) = self.field_of(&base, field, *line)?;
                let frepr = self.cx.repr(&fty, *line)?;
                match &frepr {
                    Repr::Scalar(_) => {
                        b.ins(&load_of(&self.cx.ll(&fty), off, self.cx.signed(&fty)))
                    }
                    Repr::Agg(_) => b
                        .ins(&Instruction::I32Const(off as i32))
                        .ins(&Instruction::I32Add),
                    Repr::Unit => return unsupported("a Unit field", *line),
                };
                // RFC-0114 R1′: a SCALAR field read off an unnamed record this
                // frame owns is the record's last observer — free it whole
                // from the teed address. An aggregate field is an address INTO
                // the record; a heap or `lazy` one is read again later. All
                // three stay out.
                if let Some(l) = tee {
                    // RFC-0125 M3: the read TOOK a heap field (`let sels =
                    // parse(q).sels`), and the placer's row frees the rest of
                    // the receiver around that hole.
                    let rh = row.unwrap_or_default();
                    if !rh.is_empty() {
                        if let Some(Rel::Deep(t, _)) = self.rel_for(&base, *line)? {
                            self.emit_rel(m, b, Place::Local(l), &Rel::Deep(t, rh), *line)?;
                        }
                    } else if matches!(frepr, Repr::Scalar(_))
                        && self.rel_for(&fty, *line)?.is_none()
                        && vyrn_frontend::types::deferred(&fty).is_none()
                    {
                        self.free_arg_temp(m, b, l, &base, *line)?;
                    }
                }
                // RFC-0085 M4a: reading a `lazy T` field FORCES it. The address
                // now on the stack IS a stored nullary closure (`lazy T` lowers
                // as `fn() -> T`), so the force is one call through that
                // signature's dispatcher — no new machinery, which is the whole
                // reason the deferral was given RFC-0037's representation.
                //
                // The copy-into-a-slot-and-name-it dance is `?`'s (RFC-0080 M3)
                // verbatim, and for its reason: a dispatcher argument is emitted
                // from an `Expr`, and an address sitting in a wasm local is the
                // one thing a `Place` cannot name.
                match vyrn_frontend::types::deferred(&fty) {
                    None => fty,
                    Some(inner) => {
                        let sig = crate::normalize_fn_sig(
                            &Type::Fn(Vec::new(), Box::new(inner.clone())),
                            &self.cx.types,
                        );
                        let fl = self.layout_of(&sig, *line)?;
                        let addr = b.local(ValType::I32);
                        b.ins(&Instruction::LocalSet(addr));
                        let slot = b.alloc(fl.size, fl.align);
                        b.slot(slot);
                        b.ins(&Instruction::LocalGet(addr));
                        b.ins(&Instruction::I32Const(fl.size as i32));
                        b.ins(&Instruction::MemoryCopy {
                            src_mem: 0,
                            dst_mem: 0,
                        });
                        let mark = self.scope.len();
                        self.scope
                            .push(("@lazy".to_string(), Place::Slot(slot), sig.clone()));
                        let recv = Expr::Var {
                            name: "@lazy".to_string(),
                            line: *line,
                        };
                        let t = self.fnval_call(m, b, &recv, &sig, &[], *line);
                        self.scope.truncate(mark);
                        t?
                    }
                }
            }
            Expr::StructLit { name, fields, line } => {
                let ty = self.applied_record(name, fields, *line)?;
                let decl = self
                    .cx
                    .fields(&ty)
                    .ok_or_else(|| gap(&format!("the record literal `{name}`"), *line))?;
                let Repr::Agg(l) = self.cx.repr(&ty, *line)? else {
                    return unsupported(&format!("the record literal `{name}`"), *line);
                };
                // RFC-0125 M1: the consumer's storage when it holds this very
                // type, a slot of our own otherwise.
                let (dest, used) = match hint {
                    Some((d, t)) if self.cx.ll(&t) == self.cx.ll(&ty) => (d, true),
                    _ => (Dest::Slot(b.alloc(l.size, l.align)), false),
                };
                for (i, f) in decl.iter().enumerate() {
                    let init = fields
                        .iter()
                        .find(|(n, _)| *n == f.name)
                        .map(|(_, e)| e)
                        .ok_or_else(|| gap(&format!("the missing field `{}`", f.name), *line))?;
                    match self.cx.repr(&f.ty, *line)? {
                        Repr::Scalar(_) => {
                            dest.addr(b, l.fields[i]);
                            self.expr_as(m, b, init, &f.ty)?;
                            b.ins(&store_of(&self.cx.ll(&f.ty)));
                        }
                        Repr::Agg(fl) => {
                            let at = dest.at(l.fields[i]);
                            self.agg_into(m, b, at, fl.size, init, &f.ty, true)?;
                        }
                        Repr::Unit => return unsupported("a Unit field", *line),
                    }
                }
                dest.addr(b, 0);
                self.dest_used = used;
                // A predicated record's cross-field `where` runs on the finished
                // literal. There is no coercion to hang it on — the literal
                // already IS the named type, so `from == to` and
                // `validation_required` correctly says no — which is exactly why
                // the LLVM emitter validates at its construction site too. A
                // wholly constant literal was proven by the checker, so only a
                // dynamic one pays.
                if let Some(d) = self
                    .cx
                    .types
                    .get(name)
                    .filter(|d| d.predicate.is_some())
                    .cloned()
                {
                    let dynamic = fields
                        .iter()
                        .any(|(_, e)| vyrn_frontend::consteval::eval(e, &HashMap::new()).is_none());
                    if dynamic {
                        self.emit_validation(b, &d, *line)?;
                    }
                }
                ty
            }
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                line,
            } => {
                let els = else_branch
                    .as_deref()
                    .ok_or_else(|| gap("an `if` expression with no `else`", *line))?;
                self.join(
                    m,
                    b,
                    e as *const Expr as usize,
                    cond,
                    then_branch,
                    els,
                    *line,
                )?
            }
            Expr::Unary { op, expr, line } => {
                let t = self.expr(m, b, expr)?;
                self.un_ins(b, *op, &t, *line)?
            }
            Expr::ArrayLit { elems, line } => self.array_lit(m, b, hint, elems, *line)?,
            Expr::MapLit { entries, line } => self.map_lit(m, b, entries, *line)?,
            Expr::Match {
                scrutinee,
                arms,
                line,
                ..
            } => self.match_expr(
                m,
                b,
                e as *const Expr as usize,
                scrutinee,
                &ArmRef::of(arms),
                "`match`",
                *line,
            )?,
            Expr::Try {
                expr: operand,
                line,
            } => self.try_(m, b, operand, *line, e as *const Expr as usize)?,
            Expr::TryConstruct { name, args, line } => {
                self.try_construct(m, b, name, args, *line)?
            }
            Expr::Binary { op, lhs, rhs, line } => self.binary(m, b, *op, lhs, rhs, *line)?,
            Expr::Call { name, args, line } => {
                self.call_dest = hint;
                self.call(m, b, name, args, *line)?
            }
            Expr::Spawn { name, args, line } => self.spawn(m, b, name, args, *line)?,
            // No catch-all. The arms above cover `Expr` exhaustively, and the
            // `other => unsupported(..)` that used to sit here was dead — it
            // printed an `unreachable_patterns` warning on every build of the
            // workspace, which is the kind that teaches a reader to stop reading
            // warnings.
            //
            // Deleting it also moves the obligation to where it belongs: a new
            // `Expr` variant now fails to COMPILE here, instead of silently
            // reaching a runtime "unsupported" that says the backend is missing a
            // lowering. RFC-0077's ladder reached 87 of 87 with exactly one such
            // hole (`extern`, excluded from the run comparison so nothing ever
            // built it), and a non-exhaustive match is the cheapest way to not
            // repeat that. `expr_name` keeps its three other callers.
        })
    }

    /// The concrete type a record literal produces.
    ///
    /// For a generic record the type arguments come from the site's own
    /// expectation first ([`crate::expected_type_args`]) and from the FIELD values
    /// for whatever the expectation leaves open, by the same shared rule a call
    /// site uses — and they have to be solved before the literal's slot is
    /// allocated, because `Box<Int64>` and `Box<Bool>` are not the same size.
    /// Non-generic is the overwhelming majority and costs nothing: the name IS
    /// the type.
    ///
    /// The expectation is not a nicety. A field that holds a `fn` under a
    /// parameter — an `Array<fn(P) -> T>` field, an `Option<fn(P) -> T>` field —
    /// peeks at the still-open `fn(P) -> T`, and a value built for that signature
    /// registers an RFC-0037 variant no dispatcher covers.
    fn applied_record(
        &mut self,
        name: &str,
        fields: &[(String, Expr)],
        line: usize,
    ) -> Result<Type, String> {
        let named = Type::Named(name.to_string());
        let Some(decl) = self
            .cx
            .types
            .get(name)
            .filter(|d| !d.type_params.is_empty())
            .cloned()
        else {
            return Ok(named);
        };
        // The declared field types carry the DECLARATION's parameters, not this
        // body's — `Cx::fields` substitutes into its argument, and `Named("Box")`
        // has nothing to substitute.
        let declared = self
            .cx
            .fields(&named)
            .ok_or_else(|| gap(&format!("the record literal `{name}`"), line))?;
        let want = self.expect.last().map(|t| self.cx.sub(t));
        let mut solved = crate::expected_type_args(want.as_ref(), name, Some(&decl));
        for f in &declared {
            let e = fields
                .iter()
                .find(|(n, _)| *n == f.name)
                .map(|(_, e)| e)
                .ok_or_else(|| gap(&format!("the missing field `{}`", f.name), line))?;
            // The declared field type, under this body's substitution and what is
            // solved so far, is what the value is read against: an empty array
            // literal has no element to be typed by, and `vals: []` for a field
            // declared `Array<T>` is how an empty container is built.
            self.expect.push(vyrn_frontend::types::substitute(
                &self.cx.sub(&f.ty),
                &solved,
            ));
            let t = self.peek(e, line);
            self.expect.pop();
            let t = self.cx.sub(&t?);
            if crate::settles_type_args(e) {
                crate::solve_param(&f.ty, &t, &mut solved);
            }
        }
        Ok(crate::applied_type(
            Some(&decl),
            name,
            &declared.iter().map(|f| f.ty.clone()).collect::<Vec<_>>(),
            &declared
                .iter()
                .map(|f| vyrn_frontend::types::substitute(&f.ty, &solved))
                .collect::<Vec<_>>(),
        ))
    }

    /// The fully-applied type an enum-variant construction produces.
    ///
    /// [`Fn_::applied_record`] for a variant instead of a record's fields, and the
    /// same shared [`crate::applied_type`] — a generic enum's arguments come from
    /// its PAYLOAD, because a bare constructor's use site is its payload (M2e).
    /// `Ok(None)` when the name is not a variant, or is one two enums declare: an
    /// ambiguity is the caller's to refuse, and both callers do.
    ///
    /// Naming only the enum, as `peek` used to, leaves the variant's payload the
    /// declaration's own `Type::Param` — which is where "a conversion from `Cargo`
    /// to `T`" came from. It is not solvable at the payload's own coercion, either:
    /// by then the destination slot exists and its type is fixed.
    fn applied_variant(
        &mut self,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<Type>, String> {
        let cands = match self.cx.variants.get(name) {
            Some(c) if c.len() == 1 => c.clone(),
            _ => return Ok(None),
        };
        let (e, _, declared) = cands.into_iter().next().unwrap();
        let actual = self.arg_types(&declared, args, line)?;
        let decl = self.cx.types.get(&e).cloned();
        Ok(Some(crate::applied_type(
            decl.as_ref(),
            &e,
            &declared,
            &actual,
        )))
    }

    /// Two arms meeting at one value — M0's destination-first rule.
    ///
    /// A scalar join is a `block (result T)` and needs nothing special. An
    /// aggregate one has no value to leave on the stack at all, so the slot is
    /// allocated here, BEFORE the branch, and each arm copies into it. The arms
    /// therefore have to agree on a type before either is emitted, which is what
    /// [`Fn_::peek`] is for.
    fn join(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        key: usize,
        cond: &Expr,
        then_e: &Expr,
        else_e: &Expr,
        line: usize,
    ) -> Result<Type, String> {
        let want = self.join_ty(key, line)?;
        let r = self.cx.repr(&want, line)?;
        // RFC-0114 Rule N at an `if`-expression join. The releases are
        // stack-neutral, so in the scalar case they sit under the branch value
        // exactly as `match_expr`'s do.
        let ers = self.cx.edge_rows(key);
        self.cond(m, b, cond, line)?;
        match &r {
            Repr::Agg(l) => {
                let off = b.alloc(l.size, l.align);
                b.ins(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                b.slot(off);
                self.expr_as(m, b, then_e, &want)?;
                b.ins(&Instruction::I32Const(l.size as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                self.emit_edge_releases(m, b, &ers, 0, line)?;
                b.ins(&Instruction::Else);
                b.slot(off);
                self.expr_as(m, b, else_e, &want)?;
                b.ins(&Instruction::I32Const(l.size as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                self.emit_edge_releases(m, b, &ers, 1, line)?;
                self.depth -= 1;
                b.ins(&Instruction::End);
                b.slot(off);
            }
            Repr::Scalar(v) => {
                b.ins(&Instruction::If(BlockType::Result(*v)));
                self.depth += 1;
                self.expr_as(m, b, then_e, &want)?;
                self.emit_edge_releases(m, b, &ers, 0, line)?;
                b.ins(&Instruction::Else);
                self.expr_as(m, b, else_e, &want)?;
                self.emit_edge_releases(m, b, &ers, 1, line)?;
                self.depth -= 1;
                b.ins(&Instruction::End);
            }
            // Both branches diverge (RFC-0079): there is no value to join, no
            // destination to allocate, and nothing for the enclosing block to
            // read — so the branches are emitted as statements and the stack is
            // taken polymorphic afterwards, which is the shape `panic` itself
            // takes. A plain Unit `if` in value position is still a gap.
            Repr::Unit if matches!(want, Type::Never) => {
                b.ins(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                self.expr_as(m, b, then_e, &want)?;
                b.ins(&Instruction::Else);
                self.expr_as(m, b, else_e, &want)?;
                self.depth -= 1;
                b.ins(&Instruction::End);
            }
            Repr::Unit => return unsupported("an `if` expression yielding Unit", line),
        }
        self.diverged(b, &want);
        Ok(want)
    }

    /// A join whose every arm diverged (RFC-0079) leaves the enclosing stack with
    /// nothing on it, and unlike a bare `panic` the `end` of its own block has
    /// already restored a non-polymorphic stack. One `unreachable` says so.
    ///
    /// M1 pinned every join shape with the panic NOT taken, which is the case
    /// where the surviving arm supplies the value. `std/strings`'s `substring` is
    /// the other one — a nested `match` with a `panic` in BOTH arms, in value
    /// position — and it read as "expected i32 but nothing on stack" in wasmtime
    /// and as an empty `phi` operand on the textual path.
    fn diverged(&self, b: &mut Frame, want: &Type) {
        if matches!(want, Type::Never) {
            b.ins(&Instruction::Unreachable);
        }
    }

    /// The type a join carries — the checker's, at the node, once.
    ///
    /// RFC-0125 §3 M5: a merge holds ONE value and the checker states which
    /// type it has. This backend used to reconcile one from the ARMS, and an
    /// arm can only report the type it happens to have PRODUCED: the same
    /// type in whichever shape that arm built it. The textual backend fed its
    /// `phi` the other shape for exactly that reason.
    ///
    /// A join whose type the record does not carry is a gap, not a guess:
    /// `compile` asks `vyrn_lower::core::decide` for the record before it
    /// emits a byte, so every program this reaches has one.
    ///
    /// A REFINED type decays to its base here, and only here, because a join is
    /// not a value boundary. The checker unifies `Some(a) => a` (an `Age`) with
    /// `None => 0 - 1` (an `Int64`) at the base and asks nothing of the second
    /// arm; a lowering that made `Age` the arms' target instead would send that
    /// arm through M2d's seam and validate it against a refinement the language
    /// never required, which is `error: validation failed for `Age`` here and
    /// `-1` on the other two engines. The checker's own answer is already the
    /// base, so this only holds the base where a substitution reintroduces a
    /// refined alias.
    ///
    /// Found by `validate.vyrn` becoming compilable in M2k, but the hole is
    /// M2b's: a plain `match` on an `Option<Age>` had it all along, and no
    /// example held one. The boundary the value really crosses — the `let`, the
    /// `return`, the field — still validates, because that coercion is a
    /// separate one outside the join.
    fn join_ty(&self, key: usize, line: usize) -> Result<Type, String> {
        let Some(t) = vyrn_lower::core::join_ty(key) else {
            return unsupported("a join the checker did not type", line);
        };
        let t = self.cx.sub(&t);
        Ok(match &t {
            Type::Named(n) if self.cx.types.get(n).is_some_and(|d| d.predicate.is_some()) => {
                self.cx.resolve(&t)
            }
            _ => t,
        })
    }

    /// The type an expression has — the checker's, read by node.
    ///
    /// Needed at a join, where the destination has to exist before either arm
    /// runs, and at every emit site that has to know an operand's type before
    /// it emits one. It used to DERIVE the answer from the operands, in
    /// twenty arms covering every expression kind, which is the second copy
    /// of the checker's rule that RFC-0125 §3 M5 exists to delete.
    ///
    /// [`Fn_::peek_inner`] is what is left, and it answers only for the AST
    /// this backend builds itself.
    fn peek(&mut self, e: &Expr, line: usize) -> Result<Type, String> {
        // Through the plan's own clone→original alias, for the trees this
        // backend copies to specialize a higher-order call or to name an
        // impl's method: a clone's node has no record of its own, and the
        // node it copies has the answer. RFC-0114 §26 built that map so a
        // release row would survive the copy; a type survives it the same way,
        // and reading one map rather than two is why the alias is registered
        // where the clone is made and not here.
        let at = e as *const Expr as usize;
        let t = match vyrn_lower::core::node_ty(at)
            .or_else(|| vyrn_lower::core::node_ty(self.cx.plan.key_of(at)))
        {
            Some(t) => self.cx.sub(&t),
            None => self.peek_inner(e, line)?,
        };
        if crate::observe::on() {
            crate::observe::record(
                crate::observe::Site::Peek,
                crate::observe::kind_of(e),
                e as *const Expr as usize,
                &self.cx.subst,
                &t,
            );
        }
        Ok(t)
    }

    /// The stand-down: a type for AST the CHECKER never saw.
    ///
    /// This was twenty arms and every expression kind, which is what made it
    /// the whole of the class RFC-0125 §3 M5 deletes — a second statement of
    /// the rule that decides a node's type, in an emitter, able to disagree
    /// with the first. [`Fn_::peek`] reads the checker's answer now, and what
    /// is left here answers only for the trees this backend BUILDS at an emit
    /// site: the `@rel` receiver of an implicit release, the index of a
    /// desugared element read, a dispatched call it writes to reach an impl.
    /// The checker never typed those, so nothing it says can be disagreed
    /// with, and RFC-0101 §2.3 already assigns the class to the backend.
    ///
    /// Measured over `vyrn-cli`'s whole suite: 360 answers, all of them one
    /// of a local this backend named, a field read of one, an element read it
    /// desugared, a call it wrote, and a literal beside them. A kind that is
    /// not one of those is a gap rather than a guess, which is the property
    /// the twenty arms had and the reason a catch-all is safe to write now.
    fn peek_inner(&mut self, e: &Expr, line: usize) -> Result<Type, String> {
        Ok(match e {
            // A literal the emitter minted — a length, an index, a tag.
            Expr::Int(_) | Expr::Byte(_) => Type::Int,
            Expr::Float(_) => Type::Float,
            Expr::Bool(_) => Type::Bool,
            Expr::Str(_) => Type::Str,
            // A local it just created and named. The scope is the only source
            // there could be: the name is one this backend chose.
            Expr::Var { name, .. } => self.lookup(name, line)?.1,
            Expr::Field { expr, field, .. } => {
                let base = self.peek(expr, line)?;
                match length_ty(field, &self.cx.resolve(&base)) {
                    Some(t) => t,
                    // A read of a `lazy T` field is a `T` (RFC-0085 M4a) — it has
                    // been forced by the time anything asks what it is.
                    None => vyrn_frontend::types::forced(&self.field_of(&base, field, line)?.1),
                }
            }
            Expr::Call { name, args, .. } => match name.as_str() {
                // `@at` is `vyrn_frontend::project::AT` and `@slot` is
                // `vyrn_frontend::project::ELEM`, both spelled out because a
                // match pattern cannot name them through the path.
                "@at" | "@slot" | "@swapRemove" if args.len() == 2 => {
                    let a = self.peek(&args[0], line)?;
                    match self.cx.resolve(&a) {
                        Type::Array(i) | Type::ArrayN(i, _) | Type::SmallArray(i, _) => *i,
                        Type::Str => Type::IntN {
                            bits: 8,
                            signed: false,
                        },
                        // `m[k]` is an honest lookup, so it is an `Option` where
                        // an array index is the element (RFC-0028).
                        Type::Map(_, v) if name != "@swapRemove" => Type::option(*v),
                        // A user container answers with its `place at` — the
                        // same row `a[i]` resolves through (RFC-0091 M3). The
                        // DECLARED type keys it, because an impl head names the
                        // alias and `resolve` above has already lost it.
                        other => match self.user_elem(&a) {
                            Some(t) => t,
                            None => {
                                return unsupported(&format!("a branch indexing `{other}`"), line)
                            }
                        },
                    }
                }
                // A call this backend WROTE: an implicitly dispatched method
                // (RFC-0084), or a builtin whose implementation is a Vyrn
                // function (RFC-0078 M4c). Both are answered by the callee's
                // own signature, which is what `call` routes to when it emits.
                _ => match vyrn_frontend::loader::routed_builtin(name)
                    .and_then(|rt| self.cx.sigs.get(rt))
                    .or_else(|| self.cx.sigs.get(name))
                {
                    Some(s) => s.ret_ty.clone(),
                    None => return unsupported(&format!("a branch yielding `{name}`"), line),
                },
            },
            other => {
                return unsupported(
                    &format!("a synthesized {}", crate::observe::kind_of(other)),
                    line,
                )
            }
        })
    }

    /// A `=~` pattern's DFA in the data segment: `(table, accept, start)`.
    ///
    /// Interned at the USE site rather than collected in a pre-pass. The textual
    /// backend needs a pre-pass because an LLVM global has a name that has to
    /// exist before the reference to it; a data address does not, and
    /// [`Module::data`] already shares identical contents — so the two sites of
    /// `value =~ "[a-z]+"` in `regex.vyrn` get one table because their bytes are
    /// equal, not because something went looking for them. That also means the
    /// generated code this backend compiles (RFC-0021 generators, RFC-0078's
    /// rewrites) needs no walker of its own to be reachable.
    ///
    /// `compile` is the one source of the table — the same function the checker
    /// proved the pattern with and the interpreter runs — so the three engines can
    /// only disagree about the WALK, never about the language.
    ///
    /// ponytail: a complete 256-wide table of `u32` is **1 KB per state**, and
    /// `twdemo`'s `Tw` is 781 states — 799,744 bytes, the largest static this
    /// backend emits anywhere. That is the shape RFC-0046 chose and what the
    /// textual backend emits too, so it is not a regression; if it ever matters,
    /// byte equivalence classes (a 256-byte class map plus a `nclasses`-wide row)
    /// would cut it by whatever the alphabet actually distinguishes, which for a
    /// finite key set is a factor of ten or more. Interning at the use site already
    /// takes the easy half: a pattern whose every boundary was proven at compile
    /// time costs nothing at all, which is why `TwClass` is not in the module.
    fn regex_dfa(
        &mut self,
        m: &mut Module,
        pat: &str,
        line: usize,
    ) -> Result<(u32, u32, u32), String> {
        // The checker compiled every pattern already; a failure here would be the
        // two disagreeing, which is a gap rather than a panic.
        let dfa = vyrn_frontend::regex::compile(pat)
            .map_err(|e| gap(&format!("the pattern `{pat}` ({e})"), line))?;
        let mut table = Vec::with_capacity(dfa.table.len() * 4);
        for n in &dfa.table {
            table.extend_from_slice(&n.to_le_bytes());
        }
        let accept: Vec<u8> = dfa.accepting.iter().map(|a| u8::from(*a)).collect();
        Ok((m.data(&table, 4), m.data(&accept, 1), dfa.start))
    }

    /// `a + b` on Strings is `@concat` written as an operator, so its operands
    /// are call arguments and take the call-argument rule
    /// (`rfcs/census-call-arguments.md` §9, finding 3): `"n" + label(i)` reaches
    /// this lowering rather than [`Fn_::call`], so it was in neither that
    /// census's count nor RFC-0096 M3's operand class, and leaked the same 48
    /// bytes a turn.
    ///
    /// The mark is [`Fn_::call`]'s, for its reason: an operand that is itself a
    /// call takes back only what was teed after its own mark. Every other
    /// operator reaches the drain with nothing teed — `own` records a row only
    /// where the `+` builds a String. The concatenation's own result stays on
    /// the stack while the frees run, exactly as it does at a call.
    fn binary(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        line: usize,
    ) -> Result<Type, String> {
        let mark = self.arg_frees.len();
        let r = self.binary_inner(m, b, op, lhs, rhs, line);
        for (l, ty) in self.arg_frees.split_off(mark) {
            self.free_arg_temp(m, b, l, &ty, line)?;
        }
        r
    }

    /// Release one argument temporary, by its TYPE (RFC-0114 M1).
    ///
    /// The String case is the historical fast path: the local holds the char
    /// pointer and [`Fn_::free_str_temp`] adjusts to the block start. Every
    /// other owning kind's local holds the ADDRESS of the value's storage —
    /// aggregates travel by pointer in this backend — which is exactly what
    /// [`Fn_::emit_rel`] takes as a `Place::Local`, so the release is the same
    /// walk block exit uses and this adapter adds none. The kind comes off the
    /// type through [`Fn_::rel_for`], the same table the analysis consulted
    /// when it recorded the temporary.
    /// RFC-0114 Rule N: release the bindings the OTHER branch of this `if`
    /// consumed, on the edge where they are still this frame's. A declared
    /// `impl Owned` release is skipped — its body is user code whose timing
    /// all three engines must agree on, and the RFC refuses to put it on an
    /// edge. A `region` is not asked: the arena refuses its own blocks at
    /// `free`, and an `Array` or a `Map` bound inside one was never the
    /// arena's — this edge used to hand both to nobody.
    fn emit_edge_releases(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        ers: &[(String, u32)],
        edge: u32,
        line: usize,
    ) -> Result<(), String> {
        for (name, t) in ers {
            if *t != edge {
                continue;
            }
            // `d.line` (RFC-0125 M3): the sub-place the other edge took,
            // released here from its address inside the binding.
            let mut parts = name.split('.');
            let root = parts.next().unwrap_or_default();
            let Ok((place, mut ty)) = self.lookup(root, line) else {
                continue;
            };
            let mut off = 0u32;
            let mut sub = false;
            for f in parts {
                let (o, fty) = self.field_of(&ty, f, line)?;
                off += o;
                ty = fty;
                sub = true;
            }
            if sub {
                if self.rel_for(&ty, line)?.is_some() {
                    let a = self.addr_local(b, place, off);
                    self.rel_at(m, b, a, &ty, line)?;
                }
                continue;
            }
            match self.rel_for(&ty, line)? {
                Some(Rel::Call(..)) | None => {}
                Some(rel) => self.emit_rel(m, b, place, &rel, line)?,
            }
        }
        Ok(())
    }

    fn free_arg_temp(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        l: u32,
        ty: &Type,
        line: usize,
    ) -> Result<(), String> {
        match self.rel_for(ty, line)? {
            Some(Rel::Str) => {
                self.free_str_temp(b, Some(l));
                Ok(())
            }
            Some(rel) => self.emit_rel(m, b, Place::Local(l), &rel, line),
            None => Ok(()),
        }
    }

    #[allow(clippy::too_many_arguments)]
    fn binary_inner(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        op: BinOp,
        lhs: &Expr,
        rhs: &Expr,
        line: usize,
    ) -> Result<Type, String> {
        // `&&` and `||` are control flow, not arithmetic: the right operand must
        // not run when the left decides the answer.
        if matches!(op, BinOp::And | BinOp::Or) {
            self.cond(m, b, lhs, line)?;
            b.ins(&Instruction::If(BlockType::Result(ValType::I32)));
            self.depth += 1;
            if op == BinOp::And {
                self.cond(m, b, rhs, line)?;
                b.ins(&Instruction::Else);
                b.ins(&Instruction::I32Const(0));
            } else {
                b.ins(&Instruction::I32Const(1));
                b.ins(&Instruction::Else);
                self.cond(m, b, rhs, line)?;
            }
            self.depth -= 1;
            b.ins(&Instruction::End);
            return Ok(Type::Bool);
        }

        // `s =~ "pat"` (RFC-0046): the pattern is a compile-time DFA, so the right
        // operand is not a value and must not be evaluated. Handled before the
        // operands for that reason alone.
        if op == BinOp::Match {
            let Expr::Str(pat) = rhs else {
                // The checker requires a literal; this says so rather than
                // evaluating a `String` no DFA was compiled for.
                return unsupported("a `=~` pattern that is not a string literal", line);
            };
            let s = self.expr(m, b, lhs)?;
            if self.cx.resolve(&s) != Type::Str {
                return unsupported(&format!("`=~` on `{s}`"), line);
            }
            // An allocated left operand is this operator's to free (round
            // thirty), through the same tee the comparisons use.
            let k = self.tee_str_temp(b, lhs);
            let (table, accept, start) = self.regex_dfa(m, pat, line)?;
            b.ins(&Instruction::I32Const(table as i32));
            b.ins(&Instruction::I32Const(start as i32));
            b.ins(&Instruction::I32Const(accept as i32));
            b.ins(&Instruction::Call(self.cx.rt.regex_run));
            let flag = b.local(ValType::I32);
            b.ins(&Instruction::LocalSet(flag));
            self.free_str_temp(b, k);
            b.ins(&Instruction::LocalGet(flag));
            return Ok(Type::Bool);
        }

        let l = self.expr(m, b, lhs)?;
        let lt = self.cx.resolve(&l);
        // A string `+` is a concatenation and a string comparison is a byte
        // compare; both are calls, so they are handled before the numeric table.
        if lt == Type::Str {
            // The concatenation copies both halves, so a half this expression
            // allocated is released once it has (RFC-0096 M3). The left one is
            // kept BEFORE the right is lowered, because lowering the right can
            // be a whole nested concatenation of its own.
            // Comparisons read both halves and keep neither, so their
            // operand temporaries are this site's to free too (RFC-0096 M3,
            // exit-residue round twelve) — the textual backend's strcmp arm
            // is the twin.
            let kl = match op {
                BinOp::Add
                | BinOp::Eq
                | BinOp::NotEq
                | BinOp::Lt
                | BinOp::LtEq
                | BinOp::Gt
                | BinOp::GtEq => self.tee_str_temp(b, lhs),
                _ => None,
            };
            let r = self.expr(m, b, rhs)?;
            if self.cx.resolve(&r) != Type::Str {
                return unsupported("a string operator with a non-string operand", line);
            }
            if op == BinOp::Add {
                let kr = self.tee_str_temp(b, rhs);
                self.arena_route(b, true);
                b.ins(&Instruction::Call(self.cx.rt.concat));
                self.arena_route(b, false);
                self.free_str_temp(b, kl);
                self.free_str_temp(b, kr);
                return Ok(Type::Str);
            }
            let kr = self.tee_str_temp(b, rhs);
            b.ins(&Instruction::Call(self.cx.rt.strcmp));
            b.ins(&Instruction::I32Const(0));
            b.ins(&cmp_i32(op).ok_or_else(|| gap(&format!("`{op:?}` on strings"), line))?);
            self.free_str_temp(b, kl);
            self.free_str_temp(b, kr);
            return Ok(Type::Bool);
        }
        // `Code + Code` concatenates fragments with their origins carried
        // (RFC-0054). Both sides are handles, so the concatenation happens in the
        // HOST's arena and this is one import call (RFC-0076 M3a). Equality needs no
        // import: the checker permits only `+`.
        if let (Some(g), Type::Named(n)) = (self.cx.gen, &lt) {
            if n == "Code" {
                if op != BinOp::Add {
                    return unsupported(&format!("`{op:?}` on a code quote"), line);
                }
                self.expr_as(m, b, rhs, &lt)?;
                b.ins(&Instruction::Call(g.concat));
                return Ok(l);
            }
        }
        // The width the operator RUNS at, which may be the right operand's
        // (`op_width`), and the left operand already on the stack moving to it
        // through the M2d seam like any other flow.
        let opty = match Num::of(&lt) {
            // A LITERAL is not the sibling the rule means, and `peek` is the
            // checker's answer for every other right operand.
            Some(n) if n == Num::PLAIN && !matches!(rhs, Expr::Int(_) | Expr::Byte(_)) => {
                let rt = self.peek(rhs, line).ok();
                self.op_width(&lt, rt.as_ref())
            }
            _ => lt.clone(),
        };
        if opty != lt {
            self.coerce(m, b, None, &lt, &opty, line)?;
        }
        // The RESOLVED operand type — arithmetic runs on the base
        // representation, so `age + 1` must not validate `1` against `Age`'s
        // predicate. It is the *assignment* that re-validates the sum, which is
        // why the LLVM emitter returns its `numty` rather than `lty`.
        self.expr_as(m, b, rhs, &opty)?;
        self.bin_ins(b, op, &opty, &l, line)
    }

    /// The instruction a unary operator IS, once its operand stands on the
    /// stack — RFC-0125 §2.3's "maps `prim` rows to wasm instructions".
    ///
    /// Stated once, for the two walks that reach it: [`Fn_::expr`], which
    /// reads an `Expr::Unary`, and [`Fn_::core_prim`], which reads the
    /// operator off [`vyrn_lower::core::Op`]. Nothing here interleaves the
    /// operand with anything, so unlike the binary table this one has no
    /// family left behind at its caller.
    fn un_ins(&mut self, b: &mut Frame, op: UnOp, t: &Type, line: usize) -> Result<Type, String> {
        let rt = self.cx.resolve(t);
        match (op, Num::of(&rt)) {
            // `x * -1`, which is also what makes the width's minimum
            // negate to itself — the wrapping the interpreter does, for
            // free. `~x` is `x ^ -1`, and both then renormalize because a
            // narrow carrier holds more bits than the width.
            (UnOp::Neg | UnOp::BitNot, Some(n)) => {
                if n.wide() {
                    b.ins(&Instruction::I64Const(-1));
                    b.ins(if op == UnOp::Neg {
                        &Instruction::I64Mul
                    } else {
                        &Instruction::I64Xor
                    });
                } else {
                    b.ins(&Instruction::I32Const(-1));
                    b.ins(if op == UnOp::Neg {
                        &Instruction::I32Mul
                    } else {
                        &Instruction::I32Xor
                    });
                }
                renorm(b, n);
            }
            (UnOp::Neg, None) if matches!(rt, Type::Float | Type::Float32) => {
                b.ins(if rt == Type::Float32 {
                    &Instruction::F32Neg
                } else {
                    &Instruction::F64Neg
                });
            }
            // `-v` (RFC-0083 M2) is the sign-bit flip, not a subtraction
            // from zero — `f32x4.neg` keeps the sign of a zero where
            // `splat(0.0) - v` does not.
            (UnOp::Neg, None) if rt == Type::F32x4 => {
                b.ins(&Instruction::F32x4Neg);
            }
            (UnOp::Neg, None) if rt == Type::F64x2 => {
                b.ins(&Instruction::F64x2Neg);
            }
            // Two's-complement negation, four lanes (RFC-0083 M3):
            // `-Int32.min` is `Int32.min`, the same wrap `i32.sub` from
            // zero has at scalar width.
            (UnOp::Neg, None) if rt == Type::I32x4 => {
                b.ins(&Instruction::I32x4Neg);
            }
            // `~m` complements all 128 bits, which is the lane-wise
            // complement because a mask lane is all-ones or all-zeros —
            // and the lane-wise complement of an `I32x4` for the simpler
            // reason that `v128.not` has no lane width to get wrong.
            (UnOp::BitNot, None) if matches!(rt, Type::Mask32x4 | Type::Mask64x2 | Type::I32x4) => {
                b.ins(&Instruction::V128Not);
            }
            (UnOp::Not, _) if rt == Type::Bool => {
                b.ins(&Instruction::I32Eqz);
            }
            _ => return unsupported("a unary operator on this type", line),
        }
        Ok(t.clone())
    }

    /// The width an integer operator runs at: EITHER operand's.
    ///
    /// A plain-`Int` operand adopts a sized sibling's width, which is the
    /// textual backend's `numty` rule. Taking it from the left alone would
    /// compute `0 - eight` (an `Int32`) in 64 bits — the same answer for
    /// `+`/`-`/`*` and a different one for `/`, `>>` and every comparison.
    ///
    /// `rt` is the right operand's type where the right operand is a VALUE,
    /// and `None` where it is a literal: a byte literal adapts to the position
    /// it is in, the checker types `'A' - 'a'` `Int64`, and computing it at
    /// eight bits makes it 224 where the other two engines say -32. The same
    /// is true of `b >= 'a'`, a signed 64-bit comparison in all three engines
    /// that would become an unsigned byte one here.
    ///
    /// Stated once for the two walks that ask it (RFC-0125 §3 M3, the driver
    /// slice): the AST walk peeks the right operand's node, and the core walk
    /// reads the type off the name the row carries.
    fn op_width(&self, lt: &Type, rt: Option<&Type>) -> Type {
        let Some(rt) = rt else { return lt.clone() };
        let rt = self.cx.resolve(rt);
        match Num::of(&rt) {
            Some(rn) if rn != Num::PLAIN => rt,
            _ => lt.clone(),
        }
    }

    /// The instruction an operator IS, once both its operands stand on the
    /// stack at `opty` — RFC-0125 §2.3's "maps `prim` rows to wasm
    /// instructions".
    ///
    /// Stated once, for the two walks that reach it: [`Fn_::binary_inner`],
    /// which reads an `Expr::Binary`, and [`Fn_::core_prim`], which reads the
    /// operator off [`vyrn_lower::core::Op`] and never looks at the source.
    /// The families NOT here are the ones that interleave the operands with
    /// something else, so an operand-first seam cannot hold them: `&&` and
    /// `||` branch, `=~` compiles its right operand to a DFA rather than
    /// evaluating it, and a `String` or a `Code` operator releases what each
    /// operand allocated between the two evaluations.
    ///
    /// `spell` is the left operand's type AS WRITTEN, which is what a gap's
    /// wording names; `opty` is the resolved type the operator runs at.
    fn bin_ins(
        &mut self,
        b: &mut Frame,
        op: BinOp,
        opty: &Type,
        spell: &Type,
        line: usize,
    ) -> Result<Type, String> {
        let lt = opty.clone();
        let l = spell;
        if lt == Type::Bool {
            b.ins(&cmp_i32(op).ok_or_else(|| gap(&format!("`{op:?}` on booleans"), line))?);
            return Ok(Type::Bool);
        }
        // Floats have their own small table and no width bookkeeping: `f32` and
        // `f64` are wasm value types, so nothing needs renormalizing and a
        // `Float32` operation rounds to single precision because the opcode does.
        // `%` and the bitwise family are not valid on a float — the checker says
        // so, and the interpreter and the textual backend both call that a type
        // error rather than lowering it.
        // Lane-wise arithmetic (RFC-0083). One instruction, four independent
        // single-precision operations — nothing renormalizes and nothing
        // reassociates, so this needs no more bookkeeping than the scalar floats
        // below. The checker admits ten operators on a vector; the six relational
        // ones yield a `Mask32x4`, and wasm's `f32x4.lt`..`f32x4.ge` and
        // `f32x4.eq` are already the ORDERED comparisons (false on a NaN operand)
        // while `f32x4.ne` is the unordered one — the same pairing the textual
        // backend's `fcmp olt`/`fcmp une` makes, which is what RFC-0081 had to
        // correct at scalar width and is written down here for that reason.
        if lt == Type::F32x4 {
            let mask = !matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div);
            b.ins(&match op {
                BinOp::Add => Instruction::F32x4Add,
                BinOp::Sub => Instruction::F32x4Sub,
                BinOp::Mul => Instruction::F32x4Mul,
                BinOp::Div => Instruction::F32x4Div,
                BinOp::Lt => Instruction::F32x4Lt,
                BinOp::LtEq => Instruction::F32x4Le,
                BinOp::Gt => Instruction::F32x4Gt,
                BinOp::GtEq => Instruction::F32x4Ge,
                BinOp::Eq => Instruction::F32x4Eq,
                BinOp::NotEq => Instruction::F32x4Ne,
                _ => return unsupported(&format!("`{op:?}` on `{l}`"), line),
            });
            return Ok(if mask { Type::Mask32x4 } else { lt });
        }
        // The wide float width (RFC-0083 M4): the same ten operators, one lane
        // wider and two lanes fewer, and `f64x2.div` exists so nothing is lost.
        // The comparisons are wasm's ORDERED ones with `ne` unordered, the same
        // pairing the narrow width states above.
        if lt == Type::F64x2 {
            let mask = !matches!(op, BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div);
            b.ins(&match op {
                BinOp::Add => Instruction::F64x2Add,
                BinOp::Sub => Instruction::F64x2Sub,
                BinOp::Mul => Instruction::F64x2Mul,
                BinOp::Div => Instruction::F64x2Div,
                BinOp::Lt => Instruction::F64x2Lt,
                BinOp::LtEq => Instruction::F64x2Le,
                BinOp::Gt => Instruction::F64x2Gt,
                BinOp::GtEq => Instruction::F64x2Ge,
                BinOp::Eq => Instruction::F64x2Eq,
                BinOp::NotEq => Instruction::F64x2Ne,
                _ => return unsupported(&format!("`{op:?}` on `{l}`"), line),
            });
            return Ok(if mask { Type::Mask64x2 } else { lt });
        }
        // Combining masks (RFC-0083 M2). The `v128.*` opcodes are width-agnostic —
        // they are bit operations on 128 bits — which costs nothing here because a
        // `Mask32x4` lane is all-ones or all-zeros and no program can build one
        // that is neither. That is the same closed set of inhabitants `any_true`
        // already leans on. `v128.andnot` exists and has no Vyrn spelling: `a & ~b`
        // is one instruction more and nothing measured wanted it.
        if matches!(lt, Type::Mask32x4 | Type::Mask64x2) {
            b.ins(&match op {
                BinOp::BitAnd => Instruction::V128And,
                BinOp::BitOr => Instruction::V128Or,
                BinOp::BitXor => Instruction::V128Xor,
                _ => return unsupported(&format!("`{op:?}` on `{l}`"), line),
            });
            return Ok(lt);
        }
        // Lane-wise integer arithmetic, comparison and bitwise (RFC-0083 M3).
        // Three ways this is not the float table with different opcodes: there is
        // no `Div` arm, because the encoder has no `I32x4Div` and no hardware has
        // SIMD integer divide; the comparisons are the SIGNED ones, chosen by the
        // `Int32` lane type, where `lt_u` is what a `U32x4` would reach and the
        // two disagree exactly at `Int32.min`; and `& | ^` are reached DIRECTLY
        // rather than through a mask — `v128.and` has no lane width, so the
        // integers get for free what `F32x4` can only spell on a comparison's
        // result. The three arithmetic opcodes WRAP, matching the scalar `Int32`;
        // wasm has saturating adds only at i8 and i16, so there is nothing here to
        // pick wrongly.
        if lt == Type::I32x4 {
            let mask = matches!(
                op,
                BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq | BinOp::Eq | BinOp::NotEq
            );
            b.ins(&match op {
                BinOp::Add => Instruction::I32x4Add,
                BinOp::Sub => Instruction::I32x4Sub,
                BinOp::Mul => Instruction::I32x4Mul,
                BinOp::Lt => Instruction::I32x4LtS,
                BinOp::LtEq => Instruction::I32x4LeS,
                BinOp::Gt => Instruction::I32x4GtS,
                BinOp::GtEq => Instruction::I32x4GeS,
                BinOp::Eq => Instruction::I32x4Eq,
                BinOp::NotEq => Instruction::I32x4Ne,
                BinOp::BitAnd => Instruction::V128And,
                BinOp::BitOr => Instruction::V128Or,
                BinOp::BitXor => Instruction::V128Xor,
                _ => return unsupported(&format!("`{op:?}` on `{l}`"), line),
            });
            return Ok(if mask { Type::Mask32x4 } else { lt });
        }
        if matches!(lt, Type::Float | Type::Float32) {
            let wide = lt == Type::Float;
            let ins = match (op, wide) {
                (BinOp::Add, true) => Instruction::F64Add,
                (BinOp::Add, false) => Instruction::F32Add,
                (BinOp::Sub, true) => Instruction::F64Sub,
                (BinOp::Sub, false) => Instruction::F32Sub,
                (BinOp::Mul, true) => Instruction::F64Mul,
                (BinOp::Mul, false) => Instruction::F32Mul,
                // IEEE division: `/0.0` is an infinity or a NaN, never a trap,
                // which is why the div-by-zero guard is in the integer table only.
                (BinOp::Div, true) => Instruction::F64Div,
                (BinOp::Div, false) => Instruction::F32Div,
                (BinOp::Eq, true) => Instruction::F64Eq,
                (BinOp::Eq, false) => Instruction::F32Eq,
                (BinOp::NotEq, true) => Instruction::F64Ne,
                (BinOp::NotEq, false) => Instruction::F32Ne,
                (BinOp::Lt, true) => Instruction::F64Lt,
                (BinOp::Lt, false) => Instruction::F32Lt,
                (BinOp::LtEq, true) => Instruction::F64Le,
                (BinOp::LtEq, false) => Instruction::F32Le,
                (BinOp::Gt, true) => Instruction::F64Gt,
                (BinOp::Gt, false) => Instruction::F32Gt,
                (BinOp::GtEq, true) => Instruction::F64Ge,
                (BinOp::GtEq, false) => Instruction::F32Ge,
                _ => return unsupported(&format!("`{op:?}` on `{l}`"), line),
            };
            b.ins(&ins);
            return Ok(match op {
                BinOp::Add | BinOp::Sub | BinOp::Mul | BinOp::Div => lt,
                _ => Type::Bool,
            });
        }
        let Some(n) = Num::of(opty) else {
            return unsupported(&format!("`{op:?}` on `{l}`"), line);
        };
        // Division and the shifts are the operators with control flow in them.
        // Both operands come off the stack into scratch first, because the checks
        // have to look at them and then hand them back; and every case is checked
        // rather than left to wasm, whose own `div_s` trap would put wasmtime's
        // wording on stderr where parity compares ours.
        if matches!(op, BinOp::Div | BinOp::Rem | BinOp::Shl | BinOp::Shr) {
            let c = if n.wide() { ValType::I64 } else { ValType::I32 };
            let (d, num) = (self.scratch(b, c, 0), self.scratch(b, c, 1));
            let rule = match op {
                BinOp::Div => vyrn_frontend::trap::Rule::DivZero,
                BinOp::Rem => vyrn_frontend::trap::Rule::RemZero,
                _ => vyrn_frontend::trap::Rule::ShiftRange,
            };
            b.ins(&Instruction::LocalSet(d));
            b.ins(&Instruction::LocalSet(num));
            b.ins(&Instruction::LocalGet(d));
            if matches!(op, BinOp::Shl | BinOp::Shr) {
                // RFC-0045: a shift by `>= the width`, or a negative amount,
                // traps. ONE unsigned `>=` covers both — a negative amount reads
                // as a huge unsigned — which is exactly the interpreter's
                // `y < 0 || y >= bits` and the textual backend's `icmp uge`.
                if n.wide() {
                    b.ins(&Instruction::I64Const(i64::from(n.bits)));
                    b.ins(&Instruction::I64GeU);
                } else {
                    b.ins(&Instruction::I32Const(i32::from(n.bits)));
                    b.ins(&Instruction::I32GeU);
                }
            } else if n.wide() {
                b.ins(&Instruction::I64Eqz);
            } else {
                b.ins(&Instruction::I32Eqz);
            }
            b.ins(&Instruction::If(BlockType::Empty));
            self.depth += 1;
            self.trap_row(b, rule, None);
            self.depth -= 1;
            b.ins(&Instruction::End);
            if op == BinOp::Div && n.signed {
                // The width's minimum over -1 has no representable answer.
                // (`%` is exempt: wasm defines `rem_s` there as 0, which is what
                // LLVM's rewritten `srem` and the interpreter both produce. An
                // unsigned divide is exempt because it has no minimum.)
                let min = i64::MIN >> (64 - n.bits);
                b.ins(&Instruction::LocalGet(d));
                if n.wide() {
                    b.ins(&Instruction::I64Const(-1)).ins(&Instruction::I64Eq);
                    b.ins(&Instruction::LocalGet(num));
                    b.ins(&Instruction::I64Const(min)).ins(&Instruction::I64Eq);
                } else {
                    b.ins(&Instruction::I32Const(-1)).ins(&Instruction::I32Eq);
                    b.ins(&Instruction::LocalGet(num));
                    b.ins(&Instruction::I32Const(min as i32))
                        .ins(&Instruction::I32Eq);
                }
                b.ins(&Instruction::I32And);
                b.ins(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                self.trap_row(b, vyrn_frontend::trap::Rule::DivOverflow, None);
                self.depth -= 1;
                b.ins(&Instruction::End);
            }
            b.ins(&Instruction::LocalGet(num));
            b.ins(&Instruction::LocalGet(d));
        }
        b.ins(&int_op(op, n).ok_or_else(|| gap(&format!("`{op:?}` on `{opty}`"), line))?);
        Ok(match op {
            // Arithmetic and bitwise keep the operand's integer type; a
            // comparison is a `Bool`. `&`/`|`/`^` and `>>` would preserve the
            // representation invariant on their own — two values whose high bits
            // already agree still agree — but `<<` shifts foreign bits in, so
            // renormalizing the whole group is one rule rather than five.
            BinOp::Add
            | BinOp::Sub
            | BinOp::Mul
            | BinOp::Div
            | BinOp::Rem
            | BinOp::BitAnd
            | BinOp::BitOr
            | BinOp::BitXor
            | BinOp::Shl
            | BinOp::Shr => {
                renorm(b, n);
                lt
            }
            _ => Type::Bool,
        })
    }

    /// Whether a user function claims `name`, so a builtin spelled the same way
    /// must NOT be lowered as one.
    ///
    /// `render`, `raw`, `rawAt` and `lex` are ordinary words, and the checker's rule
    /// (RFC-0054, RFC-0076 M3b) is that a user function of the same name wins —
    /// `examples/templates.vyrn` has a `render`. The three lists are the three
    /// places a definition can be: an ordinary body, a generic, an RFC-0023 shell.
    fn user_claims(&self, name: &str) -> bool {
        self.cx.sigs.contains_key(name)
            || self.cx.generics.contains_key(name)
            || self.cx.higher_order.contains_key(name)
    }

    /// The M3b entry the engine synthesized for a structured builtin, if it did.
    ///
    /// `lex`, `moduleInterface` and `contractOf` each return a value of a known
    /// named type, and the engine appends an ordinary Vyrn function that asks the
    /// host for it and DECODES it by walking that type. So there is nothing to lower
    /// here: the call site is redirected, and the decode is compiled by the same
    /// emitter every other Vyrn function gets — which is what makes the two walks
    /// unable to disagree about a record's field order.
    ///
    /// Conditional on the entry existing, which is how the shadowing rule survives
    /// without being restated: the engine emits one for `lex` only when no user
    /// function claims the name.
    fn gen_entry(&self, name: &str, args: &[Expr]) -> Option<String> {
        let e = match name {
            "moduleInterface" => crate::GEN_ENTRY_MODULE_INTERFACE.to_string(),
            "lex" => crate::GEN_ENTRY_LEX.to_string(),
            // A contract name is a declaration, not an argument, so the entry is
            // per-contract and nullary.
            "contractOf" => match args.first() {
                Some(Expr::Var { name: c, .. }) => format!("{}{c}", crate::GEN_ENTRY_CONTRACT_OF),
                _ => return None,
            },
            _ => return None,
        };
        self.cx.sigs.contains_key(&e).then_some(e)
    }

    /// The builtins that exist only while a generator runs (RFC-0076 M7), or `None`
    /// if `name` is not one of them.
    ///
    /// Everything here is a `vyrn_gen` import or a redirect to one. The RFC-0054
    /// piece arena, the lexer, the linker and the contract table all stay in the
    /// HOST — the interpreter's own code — so the splice rules, the identifier
    /// validation and the shortest-roundtrip float formatting are byte-identical by
    /// construction rather than by testing. Nothing guest-side knows what a piece
    /// is: a `Code` is an `i64` index into that arena, which `llt_of` says and this
    /// file therefore does not.
    ///
    /// `readFile` and `readFileBytes` are NOT here. They have a row in the table
    /// below on every path; what differs is the runtime function behind it, and
    /// `std/runtime`'s `readFileGen` and `readFileBytesGen` are that difference
    /// — one mediated import (`std/mem`'s `genRead`) in place of `path_open`.
    fn gen_builtin(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<Type>, String> {
        let g = self
            .cx
            .gen
            .expect("the caller checked there is a generator host");
        let code = Type::Named("Code".to_string());
        if let Some(e) = self.gen_entry(name, args) {
            let fwd: &[Expr] = if name == "contractOf" { &[] } else { args };
            return self.call(m, b, &e, fwd, line).map(Some);
        }
        match (name, args.len()) {
            // `raw(s)` IS `@codeText(s)` in the interpreter — one verbatim piece,
            // no origin — so it is the same import.
            ("@codeText", 1) | ("raw", 1) if !self.user_claims(name) => {
                self.expr_as(m, b, &args[0], &Type::Str)?;
                b.ins(&Instruction::Call(g.text));
                Ok(Some(code))
            }
            ("rawAt", 4) if !self.user_claims(name) => {
                self.expr_as(m, b, &args[0], &Type::Str)?;
                self.expr_as(m, b, &args[1], &Type::Str)?;
                self.expr_as(m, b, &args[2], &Type::Int)?;
                self.expr_as(m, b, &args[3], &Type::Int)?;
                b.ins(&Instruction::Call(g.raw_at));
                Ok(Some(code))
            }
            // The host renders and stashes; the guest asks for the length,
            // allocates, and fetches — the same protocol every host result uses,
            // because the host must not allocate inside guest memory.
            ("render", 1) if !self.user_claims(name) => {
                self.expr_as(m, b, &args[0], &code)?;
                b.ins(&Instruction::Call(g.render));
                self.fetch_str(b, g);
                Ok(Some(Type::Str))
            }
            // The spliced value crosses as a TAG plus one 64-bit word (plus a
            // pointer when it is a String), because the host needs the value itself
            // and cannot chase a guest pointer to anything else. The tag names the
            // interpreter `Val` the host rebuilds and is a COMPILE-TIME constant —
            // the static type is known here — so there is no runtime dispatch.
            //
            // Peeked before it is evaluated, unlike the textual emitter's version:
            // the tag is the FIRST argument on the stack and the value is the
            // second, so the type has to be known before the value is pushed.
            ("@codeSplice", 2) => {
                let vty = self.cx.resolve(&self.peek(&args[0], line)?);
                let tag = match &vty {
                    Type::Str => crate::TAG_STR,
                    Type::Named(n) if n == "Code" => crate::TAG_CODE,
                    Type::Bool => crate::TAG_BOOL,
                    Type::Float => crate::TAG_F64,
                    Type::Float32 => crate::TAG_F32,
                    _ => match Num::of(&vty) {
                        Some(n) if n.signed => crate::TAG_INT,
                        Some(_) => crate::TAG_UINT,
                        None => {
                            return unsupported(
                                &format!("splicing `{vty}` into a code quote"),
                                line,
                            )
                        }
                    },
                };
                b.ins(&Instruction::I32Const(tag));
                // `bits` then `ptr`: a String travels as the pointer and a zero
                // word, everything else as the word and a null pointer.
                if vty == Type::Str {
                    b.ins(&Instruction::I64Const(0));
                    self.expr_as(m, b, &args[0], &Type::Str)?;
                } else {
                    self.expr_as(m, b, &args[0], &vty)?;
                    match &vty {
                        // Lossless, and it leaves the formatting where it belongs.
                        Type::Float => {
                            b.ins(&Instruction::I64ReinterpretF64);
                        }
                        Type::Float32 => {
                            b.ins(&Instruction::I32ReinterpretF32)
                                .ins(&Instruction::I64ExtendI32U);
                        }
                        Type::Bool => {
                            b.ins(&Instruction::I64ExtendI32U);
                        }
                        // A sized integer's carrier is already correctly extended
                        // for its signedness (the M2h invariant), so widening it
                        // agrees with the tag by construction. A `Code` handle is
                        // already the word.
                        _ => {
                            if let Some(n) = Num::of(&vty) {
                                widen(b, n);
                            }
                        }
                    }
                    b.ins(&Instruction::I32Const(0));
                }
                self.expr_as(m, b, &args[1], &Type::Int)?;
                b.ins(&Instruction::Call(g.splice));
                Ok(Some(code))
            }
            // M3b's atom stream. `reflect` computes the value host-side and leaves
            // it as atoms; the two `next` calls pull them back. Nothing about the
            // value's SHAPE is encoded here — the synthesized decoder walks the
            // type, and so does the host.
            (crate::GEN_REFLECT, 2) => {
                self.expr_as(m, b, &args[0], &Type::Int)?;
                self.expr_as(m, b, &args[1], &Type::Str)?;
                b.ins(&Instruction::Call(g.reflect));
                Ok(Some(Type::Unit))
            }
            (crate::GEN_NEXT_INT, 0) => {
                b.ins(&Instruction::Call(g.next_int));
                Ok(Some(Type::Int))
            }
            (crate::GEN_NEXT_STR, 0) => {
                b.ins(&Instruction::Call(g.next_str));
                self.fetch_str(b, g);
                Ok(Some(Type::Str))
            }
            _ => Ok(None),
        }
    }

    /// The stash protocol's guest half: a length is on the stack, so allocate,
    /// fetch and NUL-terminate, leaving a `String`.
    ///
    /// Shared by `render` and `nextStr` because it is one protocol, not two — the
    /// host stashes and answers with a length precisely so it never writes into
    /// guest memory the guest did not hand it.
    fn fetch_str(&mut self, b: &mut Frame, g: Gen) {
        // `str_new` allocates the header, the room and the terminator, so the
        // result is a `String` the moment the host has filled it (RFC-0089 M1a).
        // The length is 64-bit on the way in because the host names the size —
        // this is the one length in the module that is not bounded by the memory
        // it has to fit in — and `str_new` is where it is judged.
        let len = b.local(ValType::I32);
        let buf = b.local(ValType::I32);
        b.ins(&Instruction::I32WrapI64)
            .ins(&Instruction::LocalTee(len))
            .ins(&Instruction::LocalGet(len))
            .ins(&Instruction::Call(self.cx.rt.str_new))
            .ins(&Instruction::LocalTee(buf))
            .ins(&Instruction::Call(g.fetch))
            .ins(&Instruction::LocalGet(buf));
    }

    /// Format the `Float64` on the stack with `std/num`'s `f64Str`, leaving the
    /// String — the six decimal places, and since RFC-0081 M2 the only float
    /// formatter this backend has. `print` and `@str` both come here, so the two
    /// cannot drift apart.
    ///
    /// A `Float32` promotes first, because the interpreter formats `*f as f64`.
    ///
    /// A call by INDEX rather than by name through [`Fn_::call`]: the value is
    /// already on the stack, which is the whole of a wasm call's argument passing,
    /// and `f64Str` takes one scalar and returns one. The 511 hand-written lines
    /// this replaced are the reason — they were the largest single thing in this
    /// backend and they were the third of three implementations of `%f` that had
    /// to agree byte for byte.
    fn f64_str(&mut self, b: &mut Frame, ty: &Type, line: usize) -> Result<(), String> {
        if *ty == Type::Float32 {
            b.ins(&Instruction::F64PromoteF32);
        }
        let f = vyrn_frontend::loader::F64_STR;
        let Some(sig) = self.cx.sigs.get(f) else {
            // `std/num` is injected into any program that mentions `print` or
            // `@str`, so reaching this means a program built without a std root.
            return unsupported("formatting a `Float64` with no `std/num` in the link", line);
        };
        b.ins(&Instruction::Call(sig.index));
        Ok(())
    }

    /// A call, then the release of every argument temporary it is finished with.
    ///
    /// The census's rule (`rfcs/census-call-arguments.md` §8): a heap-owning
    /// value the ARGUMENT EXPRESSION built has no binding, so `own` — which keys
    /// every release on a `let` — has nothing to write a row against, and
    /// `width(label(i))` leaked 48 bytes a turn where `let s = label(i)` on the
    /// line above did not. Which arguments those are is `own`'s answer and not
    /// this backend's: it stands aside at a `consume` position, at a
    /// constructor, at a position `movecheck::note_retention` recorded, and
    /// wherever no signature is visible.
    ///
    /// The mark is what makes it nest. `f(g(h(x)))` frees `h`'s result at `g`
    /// and `g`'s at `f`, because the inner call takes back only what was teed
    /// after its own mark. The call's own result stays on the stack: a local
    /// read and a call push and pop above it, which is what
    /// [`Fn_::free_str_temp`] already relies on.
    fn call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        // RFC-0114 §25: the instrument's hooks are calls only in an audited
        // build. The arguments are locals and constants at every one of the
        // four sites, so dropping the statement drops nothing else with it.
        if !self.cx.audit && name.starts_with(AUDIT_PREFIX) {
            return Ok(Type::Unit);
        }
        let mark = self.arg_frees.len();
        let r = self.call_inner(m, b, name, args, line);
        // RFC-0125 M3, third slice: a lending call's result points into an
        // argument (`a[i]`, a projection). A temporary its arguments teed —
        // the receiver `weekdayLetters()[1]` reads its element out of — must
        // outlive the call when the result owns heap, so the call or operator
        // that consumes the result drains it instead.
        if let Ok(t) = &r {
            if self.lends(name) && self.rel_for(t, line)?.is_some() {
                return r;
            }
        }
        for (l, ty) in self.arg_frees.split_off(mark) {
            self.free_arg_temp(m, b, l, &ty, line)?;
        }
        r
    }

    /// Whether a call by this name lends: `a[i]` and the seeded element row
    /// it dispatches to, a lending prelude row, the `value` box, a projection.
    fn lends(&self, name: &str) -> bool {
        name == vyrn_frontend::project::AT
            || name == vyrn_frontend::project::ELEM
            || vyrn_frontend::prelude::lends(name)
            || name == "value"
            || self
                .cx
                .impls
                .iter()
                .any(|i| i.places.iter().any(|p| p.name == name))
    }

    fn call_inner(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        // RFC-0125 M1: the consumer's storage for THIS call's result, taken
        // before any argument is lowered.
        let hint = self.call_dest.take();
        // PLAN-0125-runtime §2.1: a `std/mem` primitive is one instruction,
        // never a call. Before every other resolution, because the loader's
        // prefix is the whole of the identity and no table below has a row.
        if let Some(prim) = name.strip_prefix(vyrn_frontend::loader::MEM_PREFIX) {
            return self.mem_prim(m, b, prim, args, line);
        }
        // RFC-0078 M4c: a builtin whose implementation IS a Vyrn function needs no
        // lowering here at all — it is a call to the reserved spelling the loader
        // injected. This backend had no lowering for any of the ten (the six
        // codecs, `chars`, and the three string predicates), so routing them is the
        // RFC-0077 relationship in its cleanest form: ten rows that would each have
        // to be hand-emitted become a library this backend already compiles.
        if let Some(rt) = vyrn_frontend::loader::routed_builtin(name) {
            if self.cx.sigs.contains_key(rt) {
                return self.call(m, b, rt, args, line);
            }
            // Otherwise fall through to `unsupported("the call \`{name}\`")` below,
            // which is this backend's own wording for something it cannot reach.
        }
        // RFC-0094 M3: a type the language cannot render renders itself. `@str`
        // BECOMES the `show` call; `print` and `value` take the String it hands
        // back, so each keeps the one lowering below. Dispatched on `peek`
        // rather than on an emitted type, exactly as `@copy` is, because the
        // choice has to be made before anything reaches the stack.
        if matches!(name, "print" | "@str" | "value") && args.len() == 1 {
            if let Some(f) = self
                .peek(&args[0], line)
                .ok()
                .and_then(|t| self.show_dispatch(&t))
            {
                if name == "@str" {
                    return self.call(m, b, &f, args, line);
                }
                // The textual backend's twin (round thirty-five): a printed
                // render is freed at the print, because the synthesized call
                // node has no plan row.
                if name == "print" {
                    self.call(m, b, &f, args, line)?;
                    let sv = b.local(ValType::I32);
                    b.ins(&Instruction::LocalTee(sv));
                    b.ins(&Instruction::Call(self.cx.rt.print_str));
                    b.ins(&Instruction::LocalGet(sv));
                    str_hdr(b);
                    b.ins(&Instruction::Call(self.cx.rt.free));
                    return Ok(Type::Unit);
                }
                let rendered = [Expr::Call {
                    name: f,
                    args: args.to_vec(),
                    line,
                }];
                return self.call(m, b, name, &rendered, line);
            }
        }
        // RFC-0076 M7: the builtins that exist only while a generator runs — the
        // `Code` handle operations and M3b's atom stream, none of which has a
        // row in the table below because none of them has a runtime meaning
        // outside generation.
        if self.cx.gen.is_some() {
            if let Some(t) = self.gen_builtin(m, b, name, args, line)? {
                return Ok(t);
            }
        }
        // `listDir` (RFC-0021) and `listDirKinds` (RFC-0119): one path in, a
        // `Result<Array<String>, String>` out through a destination slot, the
        // shape `readFile` has. Where the listing comes from is the twin the
        // emitter calls: the generator host's resolver under a generation,
        // told the host's list mode, and WASI's `fd_readdir` on an ordinary
        // build (RFC-0125 §3 M5), told whether names carry kinds, so `vyrn
        // run` lists the real filesystem.
        if matches!(name, "listDir" | "listDirKinds") && args.len() == 1 {
            let ty = gen_list_dir_ty();
            let l = self.layout_of(&ty, line)?;
            let off = b.alloc(l.size, l.align);
            b.slot(off);
            self.expr_as(m, b, &args[0], &Type::Str)?;
            let rt = self.cx.rt;
            let kinds = name == "listDirKinds";
            let f = if self.cx.gen.is_some() {
                b.ins(&Instruction::I32Const(if kinds {
                    crate::GEN_MODE_LIST_KINDS
                } else {
                    crate::GEN_MODE_LIST
                }));
                rt.list_dir_gen
            } else {
                b.ins(&Instruction::I32Const(kinds as i32));
                rt.list_dir
            };
            b.ins(&Instruction::I32Const(rt.listerr.0 as i32));
            b.ins(&Instruction::I32Const(rt.listerr.1 as i32));
            b.ins(&Instruction::Call(f));
            b.slot(off);
            return Ok(ty);
        }
        match name {
            // RFC-0079: `panic(msg)` — `error: `, the caller's message, a
            // newline, exit 1, in three `write_all`s for the reason `log_write`
            // takes five (the pieces are already where they need to be, and
            // concatenating first would cost a `malloc` out of an allocator that
            // never frees). The LAST piece is handed to `trap`, which writes its
            // argument and `proc_exit(1)`s — so the exit path is the one every
            // trap already takes, and this lowering adds no runtime function.
            //
            // Census U5 costs this site NOTHING. The site the loader stamped is
            // fused into the constant `trap` already receives — `"\n"` becomes
            // `" (std/slots.vyrn:189)\n"` — so the code is the same three calls
            // with one different immediate, and only the data segment grows.
            // `blackBox(v)` (RFC-0055): the value, round-tripped through a slot
            // of linear memory that nothing else names.
            //
            // It WAS the identity, and the comment said why — this backend never
            // optimizes (RFC-0125 §2.3), so there was nothing to hide the value
            // from. That stopped being true when the native route became this
            // module through wasm2c and clang at `-O2` (RFC-0125 §2.5): the
            // optimizer is downstream of the emitter now. With the identity in
            // place `examples/benching.vyrn`'s "hash to 1000" read 1 ns against
            // the textual route's 1.35 µs, because clang folded a data-dependent
            // loop the barrier existed to keep.
            //
            // A store and a load rather than a global, and a `reserve` rather
            // than `data`: a global becomes an instance field wasm2c's C forwards
            // through in one step, and `data` SHARES identical contents, so two
            // barriers would be one address. Each site takes its own sixteen
            // bytes, which is enough for a `v128` and is reserved only by a
            // program that has a `blackBox` — every other module's bytes are
            // unchanged.
            "blackBox" if args.len() == 1 => {
                let r = self.expr(m, b, &args[0])?;
                let Some(t) = self.cx.repr(&r, line)?.val() else {
                    return Ok(r);
                };
                let addr = m.reserve(16, 16) as i32;
                let tmp = self.scratch(b, t, 9);
                let at = |align: u32| MemArg {
                    offset: 0,
                    align,
                    memory_index: 0,
                };
                b.ins(&Instruction::LocalSet(tmp))
                    .ins(&Instruction::I32Const(addr))
                    .ins(&Instruction::LocalGet(tmp));
                match t {
                    ValType::I32 => b.ins(&Instruction::I32Store(at(2))),
                    ValType::I64 => b.ins(&Instruction::I64Store(at(3))),
                    ValType::F32 => b.ins(&Instruction::F32Store(at(2))),
                    ValType::F64 => b.ins(&Instruction::F64Store(at(3))),
                    _ => b.ins(&Instruction::V128Store(at(4))),
                };
                b.ins(&Instruction::I32Const(addr));
                match t {
                    ValType::I32 => b.ins(&Instruction::I32Load(at(2))),
                    ValType::I64 => b.ins(&Instruction::I64Load(at(3))),
                    ValType::F32 => b.ins(&Instruction::F32Load(at(2))),
                    ValType::F64 => b.ins(&Instruction::F64Load(at(3))),
                    _ => b.ins(&Instruction::V128Load(at(4))),
                };
                return Ok(r);
            }
            // `assert(c)` (RFC-0015): the interpreter's trap, in its words. Lowered
            // here rather than rewritten into `panic` by the CLI before the compile
            // (RFC-0125 §3 M5), so the rule is stated once.
            "assert" if args.len() == 1 => {
                self.expr_as(m, b, &args[0], &Type::Bool)?;
                let msg = self.cx.rt.intern(
                    m,
                    &vyrn_frontend::trap::line(&format!("assertion failed at line {line}")),
                );
                b.ins(&Instruction::I32Eqz)
                    .ins(&Instruction::If(BlockType::Empty))
                    .ins(&Instruction::I32Const(msg as i32))
                    .ins(&Instruction::Call(self.cx.rt.trap))
                    .ins(&Instruction::End);
                return Ok(Type::Unit);
            }
            // `assertEq(a, b)` (RFC-0015): each operand evaluated once into a
            // local, the two compared by their type — the checker allows one
            // equatable scalar type for both — and on a mismatch rendered the way
            // `toString` renders them, around ` != `, after the interpreter's
            // `scalar_to_string`. The line is written in pieces the way `panic`
            // writes its message, and `trap` writes the last piece and exits. An
            // operand that allocated is released by [`Fn_::call`] after this
            // returns, as every call argument is (`rfcs/census-call-arguments.md`).
            "assertEq" if args.len() == 2 => {
                let t = self.expr(m, b, &args[0])?;
                let t = self.cx.resolve(&t);
                let Some(vt) = self.cx.repr(&t, line)?.val() else {
                    return unsupported("`assertEq` on a non-scalar", line);
                };
                let la = b.local(vt);
                b.ins(&Instruction::LocalSet(la));
                self.expr_as(m, b, &args[1], &t)?;
                let lb = b.local(vt);
                b.ins(&Instruction::LocalSet(lb));
                b.ins(&Instruction::LocalGet(la))
                    .ins(&Instruction::LocalGet(lb));
                match &t {
                    Type::Str => {
                        b.ins(&Instruction::Call(self.cx.rt.strcmp))
                            .ins(&Instruction::I32Const(0))
                            .ins(&Instruction::I32Ne);
                    }
                    Type::Float => {
                        b.ins(&Instruction::F64Ne);
                    }
                    Type::Float32 => {
                        b.ins(&Instruction::F32Ne);
                    }
                    Type::Bool => {
                        b.ins(&Instruction::I32Ne);
                    }
                    it => match Num::of(it) {
                        Some(n) if n.wide() => {
                            b.ins(&Instruction::I64Ne);
                        }
                        Some(_) => {
                            b.ins(&Instruction::I32Ne);
                        }
                        None => return unsupported(&format!("`assertEq` on `{t}`"), line),
                    },
                }
                let (write_all, trap, strlen) =
                    (self.cx.rt.write_all, self.cx.rt.trap, self.cx.rt.strlen);
                let head = format!("error: assertion failed at line {line}: ");
                let (head_at, sep_at, nl_at) = (
                    self.cx.rt.intern(m, &head),
                    self.cx.rt.intern(m, " != "),
                    self.cx.rt.intern(m, "\n"),
                );
                let rendered = self.scratch(b, ValType::I32, 7);
                b.ins(&Instruction::If(BlockType::Empty))
                    .ins(&Instruction::I32Const(2))
                    .ins(&Instruction::I32Const(head_at as i32))
                    .ins(&Instruction::I32Const(head.len() as i32))
                    .ins(&Instruction::Call(write_all))
                    .ins(&Instruction::Drop);
                for (side, local) in [(0, la), (1, lb)] {
                    if side == 1 {
                        b.ins(&Instruction::I32Const(2))
                            .ins(&Instruction::I32Const(sep_at as i32))
                            .ins(&Instruction::I32Const(4))
                            .ins(&Instruction::Call(write_all))
                            .ins(&Instruction::Drop);
                    }
                    // The same three renderings `@str` uses, on a value that is
                    // about to be the last thing the program prints, so nothing
                    // rendered here is released.
                    b.ins(&Instruction::LocalGet(local));
                    match &t {
                        Type::Str => {}
                        Type::Float | Type::Float32 => self.f64_str(b, &t, line)?,
                        Type::Bool => {
                            b.ins(&Instruction::I32Const(self.cx.rt.str_true as i32))
                                .ins(&Instruction::I32Const(self.cx.rt.str_false as i32))
                                .ins(&Instruction::Call(self.cx.rt.bool_str));
                        }
                        it => {
                            let n = Num::of(it).expect("compared as a number above");
                            widen(b, n);
                            b.ins(&Instruction::I32Const(n.signed as i32));
                            b.ins(&Instruction::Call(self.cx.rt.int_str));
                        }
                    }
                    b.ins(&Instruction::LocalSet(rendered))
                        .ins(&Instruction::I32Const(2))
                        .ins(&Instruction::LocalGet(rendered))
                        .ins(&Instruction::LocalGet(rendered))
                        .ins(&Instruction::Call(strlen))
                        .ins(&Instruction::Call(write_all))
                        .ins(&Instruction::Drop);
                }
                b.ins(&Instruction::I32Const(nl_at as i32))
                    .ins(&Instruction::Call(trap))
                    .ins(&Instruction::End);
                return Ok(Type::Unit);
            }
            "panic" | vyrn_frontend::ast::PANIC_AT => {
                if args.is_empty() || args.len() > 2 {
                    return unsupported("`panic` with other than one argument", line);
                }
                let write_all = self.cx.rt.write_all;
                let tail = match args.get(1) {
                    Some(Expr::Str(at)) => format!(" ({at})\n"),
                    _ => "\n".to_string(),
                };
                let (pre, nl) = (self.cx.rt.intern(m, "error: "), self.cx.rt.intern(m, &tail));
                // Parked in a local because `write_all` consumes three operands,
                // so the message cannot wait on the stack under the prefix's
                // call. Evaluated FIRST, since the other two engines evaluate the
                // argument before any byte of the line is written.
                let msg = self.scratch(b, ValType::I32, 7);
                self.expr_as(m, b, &args[0], &Type::Str)?;
                b.ins(&Instruction::LocalSet(msg));
                b.ins(&Instruction::I32Const(2))
                    .ins(&Instruction::I32Const(pre as i32))
                    .ins(&Instruction::I32Const(7))
                    .ins(&Instruction::Call(write_all))
                    .ins(&Instruction::Drop);
                b.ins(&Instruction::I32Const(2))
                    .ins(&Instruction::LocalGet(msg));
                b.ins(&Instruction::LocalGet(msg));
                str_len(b);
                b.ins(&Instruction::Call(write_all)).ins(&Instruction::Drop);
                b.ins(&Instruction::I32Const(nl as i32))
                    .ins(&Instruction::Call(self.cx.rt.trap));
                // The stack goes polymorphic here, which is what lets a `panic`
                // arm sit inside a `block (result T)` owing no value.
                b.ins(&Instruction::Unreachable);
                return Ok(Type::Never);
            }
            // RFC-0074 M3a. The same runtime trap the LLVM emitter writes, from
            // the same constant: a compiled wasm module is not `vyrn serve`, and
            // `std/http`'s `mount` reaches this arm whether or not the program
            // mounts a live route. The argument is not emitted — the producer it
            // names has nobody to pull it here.
            "serveStream" => {
                let msg = self.cx.rt.intern(m, &crate::serve_stream_trap());
                b.ins(&Instruction::I32Const(msg as i32))
                    .ins(&Instruction::Call(self.cx.rt.trap));
                b.ins(&Instruction::Unreachable);
                return Ok(Type::Never);
            }
            "print" => {
                if args.len() != 1 {
                    return unsupported("`print` with other than one argument", line);
                }
                let t = self.expr(m, b, &args[0])?;
                match self.cx.resolve(&t) {
                    // Every width goes through one `i64` printer: widened by its
                    // own signedness, and then told whether to look for a sign.
                    // An unsigned type prints its magnitude, which is the
                    // interpreter's `*v as u64`.
                    ref it if Num::of(it).is_some() => {
                        let n = Num::of(it).unwrap();
                        widen(b, n);
                        b.ins(&Instruction::I32Const(n.signed as i32));
                        b.ins(&Instruction::Call(self.cx.rt.print_i64));
                    }
                    // Fixed six decimals, which `std/num`'s `f64Str` owns. Its
                    // answer is a fresh allocation always — the doc on `f64Str`
                    // pins that, non-finite words included — and the write was
                    // its whole life, so it is freed here: one block per float
                    // print, simd's entire residue table (exit-residue round
                    // seventeen).
                    ref f if matches!(f, Type::Float | Type::Float32) => {
                        self.f64_str(b, f, line)?;
                        let s = b.local(ValType::I32);
                        b.ins(&Instruction::LocalTee(s));
                        b.ins(&Instruction::Call(self.cx.rt.print_str));
                        b.ins(&Instruction::LocalGet(s));
                        str_hdr(b);
                        b.ins(&Instruction::Call(self.cx.rt.free));
                    }
                    Type::Str => {
                        b.ins(&Instruction::Call(self.cx.rt.print_str));
                    }
                    Type::Bool => {
                        b.ins(&Instruction::I32Const(self.cx.rt.str_true as i32))
                            .ins(&Instruction::I32Const(self.cx.rt.str_false as i32))
                            .ins(&Instruction::Call(self.cx.rt.bool_str));
                        b.ins(&Instruction::Call(self.cx.rt.print_str));
                    }
                    _ => return unsupported(&format!("`print` of `{t}`"), line),
                }
                return Ok(Type::Unit);
            }
            // RFC-0008's facade. A `Logger` IS its name string — the handle has no
            // other content — so `logger(name)` is the identity on a `ptr` and
            // costs nothing at all.
            "logger" if args.len() == 1 => {
                self.expr_as(m, b, &args[0], &Type::Str)?;
                return Ok(Type::Logger);
            }
            // The five levels, written subject-first, so `args` is the logger then
            // the message (the parser's method sugar).
            //
            // Both arguments are evaluated WHATEVER the threshold says, because the
            // interpreter evaluates them before it checks (RFC-0008 Q4, pinned), and
            // then the write is emitted only if the level clears it. That test is
            // the whole feature: with `logging { level: warn }` a `.debug(..)` call
            // emits no `write_all` at all, which is why a disabled log site costs
            // nothing on any engine. Making it a runtime comparison would turn a
            // deleted call into a branch — RFC-0078's census names that mistake.
            // (The five spellings are the parser's own, so no user function can
            // reach this arm: `log.info(m)` carries `@info` and a module that
            // declares `info` gets the surface word back before this.)
            // Literal rather than [`vyrn_frontend::ast::log_internal`]:
            // `primitives.rs` greps THIS FILE for each census name to decide
            // whether the direct backend covers it, and a predicate is
            // invisible to a text scan.
            "@trace" | "@debug" | "@info" | "@warn" | "@error" if args.len() == 2 => {
                self.expr_as(m, b, &args[0], &Type::Logger)?;
                self.expr_as(m, b, &args[1], &Type::Str)?;
                if log_internal(name).unwrap_or(0) < self.cx.log_level {
                    // Below the threshold: the two values are the only thing this
                    // site leaves behind, and `Unit` means nobody consumes them.
                    b.ins(&Instruction::Drop);
                    b.ins(&Instruction::Drop);
                    return Ok(Type::Unit);
                }
                self.log_write(m, b, name, line)?;
                return Ok(Type::Unit);
            }
            // String interpolation desugars to these two (parser), so they are
            // the whole of `"a \{b}"`.
            "@str" => {
                if args.len() != 1 {
                    return unsupported("`toString` with other than one argument", line);
                }
                let t = self.expr(m, b, &args[0])?;
                match self.cx.resolve(&t) {
                    // Copy, so the rendered value owns its storage. This arm was
                    // the IDENTITY until RFC-0096 M3 — the pointer passed
                    // straight through, and the textual backend has strdup'd
                    // here since it was written. That divergence was a latent
                    // double free on this backend alone: `let t = "\{s}"` has a
                    // single hole and no literal piece, so the whole
                    // interpolation IS `@str(s)` with no `@concat` above it, and
                    // `t` and `s` then released one buffer twice. The two
                    // engines now say the same thing about who owns a rendered
                    // String, which is what lets one rule
                    // ([`vyrn_frontend::declared::str_temporary`]) answer for both.
                    Type::Str => {
                        let k = self.tee_str_temp(b, &args[0]);
                        self.str_dup(b);
                        self.free_str_temp(b, k);
                    }
                    // The same two steps `print` takes, for the same reason: the
                    // digits of a sized int are the digits of the `i64` its own
                    // signedness widens it to.
                    ref it if Num::of(it).is_some() => {
                        let n = Num::of(it).unwrap();
                        widen(b, n);
                        b.ins(&Instruction::I32Const(n.signed as i32));
                        self.arena_route(b, true);
                        b.ins(&Instruction::Call(self.cx.rt.int_str));
                        self.arena_route(b, false);
                    }
                    ref f if matches!(f, Type::Float | Type::Float32) => {
                        self.f64_str(b, f, line)?;
                    }
                    // Copy, for the reason the `Str` arm above copies: a rendered
                    // value owns its storage. `bool_str` hands back the interned
                    // `"true"`/`"false"` itself, and a caller that owns a
                    // data-segment pointer is a caller that writes into the data
                    // segment — `var s = "\{flag}"` then `s = s + ".."` took
                    // `str_append`'s ours-branch, read the literal's `cap` of
                    // `u32::MAX`, never grew, and copied past the literal's end.
                    // The copy is here rather than in `bool_str` because `print`
                    // is the other caller and it frees nothing: duplicating there
                    // would leak a block per `print(flag)`. The textual backend
                    // splits the same way — `@.str.true` is strdup'd by `str(..)`
                    // and printed straight by `print`.
                    Type::Bool => {
                        b.ins(&Instruction::I32Const(self.cx.rt.str_true as i32))
                            .ins(&Instruction::I32Const(self.cx.rt.str_false as i32))
                            .ins(&Instruction::Call(self.cx.rt.bool_str));
                        self.str_dup(b);
                    }
                    _ => return unsupported(&format!("`toString` of `{t}`"), line),
                }
                return Ok(Type::Str);
            }
            "@concat" => {
                if args.len() != 2 {
                    return unsupported("`@concat` with other than two arguments", line);
                }
                self.expr_as(m, b, &args[0], &Type::Str)?;
                let ka = self.tee_str_temp(b, &args[0]);
                self.expr_as(m, b, &args[1], &Type::Str)?;
                let kb = self.tee_str_temp(b, &args[1]);
                self.arena_route(b, true);
                b.ins(&Instruction::Call(self.cx.rt.concat));
                self.arena_route(b, false);
                // The interpolation spine (RFC-0096 M3): `"a\{x}b\{y}"` folds
                // left into nested `@concat`s, so every hole's `@str` and every
                // inner join is released by the `@concat` above it.
                self.free_str_temp(b, ka);
                self.free_str_temp(b, kb);
                return Ok(Type::Str);
            }
            // Not calls at all: RFC-0021-family COMPILE-TIME reflection, which the
            // textual emitter rewrites into an ordinary expression built from the
            // type declaration. Same rewrite, from the same two frontend functions,
            // so neither backend has a runtime lowering to get wrong and the bytes
            // cannot disagree. `jsonSchema` is one string; `schemaOf` is a `Schema`
            // record literal that then lowers like any other.
            "jsonSchema" | "schemaOf" if args.len() == 1 => {
                let e = self.reflected(name, &args[0], line)?;
                return self.expr(m, b, &e);
            }
            // `toJson(x)` is the same shape one size up (RFC-0078 M2b): the
            // type-directed walk is a shared AST builder in the frontend and the
            // serializer is `std/json`'s `emit`, injected into the link. So this
            // backend gets `toJson` for the price of typing the argument — no DOM,
            // no escaping table, no number formatter of its own.
            "toJson" if args.len() == 1 => {
                let ty = self.peek(&args[0], line)?;
                let e = vyrn_frontend::jsonenc::encode_expr(args[0].clone(), &ty, line);
                // RFC-0114 §26: the rewrite EMBEDS a clone of the argument —
                // pair the whole synthesized tree's occurrences of it with
                // the original for the emission, then unwind: the tree dies
                // here.
                let mark = self.cx.plan.alias_scope();
                let mut pairs = Vec::new();
                vyrn_frontend::ast::alias_embedded(&e, &args[0], &mut pairs);
                self.cx.plan.alias_clones_scoped(&pairs);
                let r = self.expr(m, b, &e);
                self.cx.plan.alias_unwind(mark);
                return r;
            }
            // `fromJson(T, s)` is the mirror (RFC-0078 M3), and it needs less: the
            // target is a type NAME, so there is nothing to peek. The reader is
            // `std/jsonread` and the walk is generated per target, so this backend
            // gets `fromJson` without a DOM, a number parser or a message
            // assembler — the two rows RFC-0077 had left unlowered.
            "fromJson" if args.len() == 2 => {
                let Expr::Var { name: tn, .. } = &args[0] else {
                    return unsupported("`fromJson` without a type name", line);
                };
                let target = vyrn_frontend::ast::Type::Named(tn.clone());
                if !self
                    .cx
                    .sigs
                    .contains_key(&vyrn_frontend::jsondec::top_name(&target))
                {
                    return unsupported("`fromJson` without the JSON runtime linked", line);
                }
                let e = vyrn_frontend::jsondec::decode_expr(&target, args[1].clone(), line);
                // RFC-0114 §26: the rewrite embeds a clone of the payload
                // argument — `toJson`'s twin, treated identically.
                let mark = self.cx.plan.alias_scope();
                let mut pairs = Vec::new();
                vyrn_frontend::ast::alias_embedded(&e, &args[1], &mut pairs);
                self.cx.plan.alias_clones_scoped(&pairs);
                let r = self.expr(m, b, &e);
                self.cx.plan.alias_unwind(mark);
                return r;
            }
            // `value(x)` boxes a scalar into the built-in `Value` enum. Its variant
            // is picked by the argument's type and built by the ordinary enum path,
            // so the tag and the payload encoding are the same ones a user's
            // `IntVal(3)` would get.
            "value" if args.len() == 1 => {
                let name = self.value_variant(&args[0], line)?;
                return match self.sum_ctor(m, b, name, args, line, hint)? {
                    Some(t) => Ok(t),
                    None => unsupported("the built-in `Value` enum", line),
                };
            }
            // The IEEE-754 bit views (RFC-0078 M4a). One instruction each, and
            // the whole reason they are primitives: `f64` and `i64` are the same
            // 64 bits in this backend's value stack, so a reinterpretation is
            // free while a conversion rounds.
            "floatBits" if args.len() == 1 => {
                self.expr_as(m, b, &args[0], &Type::Float)?;
                b.ins(&Instruction::I64ReinterpretF64);
                return Ok(Type::IntN {
                    bits: 64,
                    signed: false,
                });
            }
            "floatFromBits" if args.len() == 1 => {
                self.expr_as(
                    m,
                    b,
                    &args[0],
                    &Type::IntN {
                        bits: 64,
                        signed: false,
                    },
                )?;
                b.ins(&Instruction::F64ReinterpretI64);
                return Ok(Type::Float);
            }
            // `stringFromBytes(b)` (RFC-0014): the bytes checked by `std/text`'s
            // `stringFault` and then copied into a fresh NUL-terminated buffer, as
            // a `Result<String, String>`. The result is an aggregate, so the slot
            // is allocated here and the runtime writes through it — the same
            // hidden destination an aggregate-returning Vyrn call gets.
            //
            // RFC-0125 §3 M6 (the third judgment's fifth slice): the check is the
            // call this arm makes first, and its answer travels into
            // `strFromBytes` where the DFA table used to go. This backend was
            // never a carrier of the two `String` rows — it called the runtime —
            // and now the runtime is not one either.
            "stringFromBytes" if args.len() == 1 => {
                let ty = Type::result(Type::Str, Type::Str);
                let Repr::Agg(l) = self.cx.repr(&ty, line)? else {
                    return unsupported("`stringFromBytes` returning a non-aggregate", line);
                };
                // Through `expr_as`, so a literal argument is typed by the position
                // rather than by its first element: `['h', 'i']` is bytes because
                // this is where bytes are wanted, and an empty one has nothing else
                // to be typed by at all.
                let bytes = Type::Array(Box::new(Type::IntN {
                    bits: 8,
                    signed: false,
                }));
                self.expr_as(m, b, &args[0], &bytes)?;
                let src = self.scratch(b, ValType::I32, 0);
                let al = self.layout_of(&bytes, line)?;
                b.ins(&Instruction::LocalSet(src));
                let off = b.alloc(l.size, l.align);
                self.str_from_bytes(b, off, src, &al, line)?;
                b.slot(off);
                return Ok(ty);
            }
            // `bytes(s)` — the string's UTF-8 bytes as an `Array<UInt8>`, i8 stride.
            // A copy, because the array is growable and the string is not: a `push`
            // on the result must not write into the string's storage.
            // `bytes(s)` and `bytes(s, start, end)` (RFC-0113). One arm: the
            // three-argument form differs only in where the copy starts and how
            // long it is, and `MemoryCopy` does not care which.
            "bytes" if args.len() == 1 || args.len() == 3 => {
                let ty = Type::Array(Box::new(Type::IntN {
                    bits: 8,
                    signed: false,
                }));
                let l = self.layout_of(&ty, line)?;
                self.expr_as(m, b, &args[0], &Type::Str)?;
                let s = self.scratch(b, ValType::I32, 0);
                let n = self.scratch(b, ValType::I32, 1);
                let buf = self.scratch(b, ValType::I32, 2);
                let from = self.scratch(b, ValType::I32, 3);
                let malloc = self.cx.rt.malloc;
                b.ins(&Instruction::LocalTee(s));
                if args.len() == 3 {
                    // `start` and `end` as i32 offsets, bounds checked against
                    // the string's length before either is used. The wording is
                    // `s[i]`'s, so the trap catalogue does not grow.
                    str_len(b);
                    let len = self.scratch(b, ValType::I32, 4);
                    b.ins(&Instruction::LocalSet(len));
                    self.expr_as(m, b, &args[1], &Type::Int)?;
                    b.ins(&Instruction::I32WrapI64);
                    b.ins(&Instruction::LocalSet(from));
                    self.expr_as(m, b, &args[2], &Type::Int)?;
                    b.ins(&Instruction::I32WrapI64);
                    let to = self.scratch(b, ValType::I32, 5);
                    b.ins(&Instruction::LocalSet(to));
                    // start < 0 || end < start || end > len — one unsigned
                    // compare would miss the ordering, so all three are written.
                    b.ins(&Instruction::LocalGet(from));
                    b.ins(&Instruction::I32Const(0));
                    b.ins(&Instruction::I32LtS);
                    b.ins(&Instruction::LocalGet(to));
                    b.ins(&Instruction::LocalGet(from));
                    b.ins(&Instruction::I32LtS);
                    b.ins(&Instruction::I32Or);
                    b.ins(&Instruction::LocalGet(to));
                    b.ins(&Instruction::LocalGet(len));
                    b.ins(&Instruction::I32GtS);
                    b.ins(&Instruction::I32Or);
                    b.ins(&Instruction::If(BlockType::Empty));
                    self.depth += 1;
                    // The offset the other two engines name: the low one when it
                    // is negative or out of order, otherwise the high one.
                    let at = b.local(ValType::I64);
                    b.ins(&Instruction::LocalGet(from));
                    b.ins(&Instruction::I64ExtendI32S);
                    b.ins(&Instruction::LocalGet(to));
                    b.ins(&Instruction::I64ExtendI32S);
                    b.ins(&Instruction::LocalGet(from));
                    b.ins(&Instruction::I32Const(0));
                    b.ins(&Instruction::I32LtS);
                    b.ins(&Instruction::LocalGet(to));
                    b.ins(&Instruction::LocalGet(from));
                    b.ins(&Instruction::I32LtS);
                    b.ins(&Instruction::I32Or);
                    b.ins(&Instruction::Select);
                    b.ins(&Instruction::LocalSet(at));
                    self.trap_row(b, vyrn_frontend::trap::Rule::StringIndex, Some(at));
                    self.depth -= 1;
                    b.ins(&Instruction::End);
                    b.ins(&Instruction::LocalGet(to));
                    b.ins(&Instruction::LocalGet(from));
                    b.ins(&Instruction::I32Sub);
                } else {
                    b.ins(&Instruction::I32Const(0));
                    b.ins(&Instruction::LocalSet(from));
                    str_len(b);
                }
                b.ins(&Instruction::LocalTee(n));
                // A zero-length string still gets a buffer, so the triple's pointer
                // is never null — `push` reallocs from it either way.
                b.ins(&Instruction::I32Const(1));
                b.ins(&Instruction::I32Add);
                b.ins(&Instruction::I64ExtendI32U);
                b.ins(&Instruction::Call(malloc));
                b.ins(&Instruction::LocalTee(buf));
                b.ins(&Instruction::LocalGet(s));
                b.ins(&Instruction::LocalGet(from));
                b.ins(&Instruction::I32Add);
                b.ins(&Instruction::LocalGet(n));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                b.ins(&Instruction::LocalGet(buf));
                b.ins(&Instruction::I32Store(word_at(l.fields[0])));
                for f in [l.fields[1], l.fields[2]] {
                    b.slot(off + f);
                    b.ins(&Instruction::LocalGet(n));
                    b.ins(&Instruction::I64ExtendI32U);
                    b.ins(&Instruction::I64Store(word8()));
                }
                b.slot(off);
                return Ok(ty);
            }
            // (`slice` was here, three `expr_as` and a call into `rt.slice`. The
            // arm was cheap; the RUNTIME FUNCTION behind it was a third copy of the
            // range check, and RFC-0079 M3 deleted both — `slice` routes into
            // `std/strpred`'s `sliceV` at the top of this dispatch now.)
            // RFC-0014's input I/O. Every one of these is a runtime function that
            // writes its whole result through a slot allocated here — the same
            // hidden destination an aggregate-returning Vyrn call gets, which is
            // why none of them needed a case outside M2b's four ABI rules.
            "args" if args.is_empty() => {
                let ty = io_builtin_ty(name, 0).expect("`args` is an I/O builtin");
                let l = self.layout_of(&ty, line)?;
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                b.ins(&Instruction::Call(self.cx.rt.args));
                b.slot(off);
                return Ok(ty);
            }
            "readLine" if args.is_empty() => {
                let ty = io_builtin_ty(name, 0).expect("`readLine` is an I/O builtin");
                let l = self.layout_of(&ty, line)?;
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                b.ins(&Instruction::I32Const(self.cx.rt.utf8d as i32));
                b.ins(&Instruction::Call(self.cx.rt.read_line));
                b.slot(off);
                return Ok(ty);
            }
            // One path in, a `Result<_, String>` out through a destination slot.
            // RFC-0044's `fsyncFile` is the same shape as the two readers and
            // differs only in the runtime function — which is exactly why it was
            // missed: it reads as a writer, so the arm it belonged in was the one
            // keyed on TWO arguments.
            //
            // The destination leads, as it does for every aggregate a
            // `std/runtime` function answers; the path follows, then what the
            // function needs after it. Under a generation (`Cx::gen`) the two
            // readers are their host twins, which take the host's read mode
            // after the path and read through the loader's resolver rather
            // than `path_open` (RFC-0076 M7).
            "readFile" | "readFileBytes" | "fsyncFile" if args.len() == 1 => {
                let ty = io_builtin_ty(name, 1).expect("all three are I/O builtins");
                let l = self.layout_of(&ty, line)?;
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                self.expr_as(m, b, &args[0], &Type::Str)?;
                let rt = self.cx.rt;
                let gen = self.cx.gen.is_some();
                let halves = |b: &mut Frame, (pre, post): (u32, u32)| {
                    b.ins(&Instruction::I32Const(pre as i32))
                        .ins(&Instruction::I32Const(post as i32));
                };
                let f = match name {
                    "readFile" => {
                        if gen {
                            b.ins(&Instruction::I32Const(crate::GEN_MODE_READ));
                        }
                        b.ins(&Instruction::I32Const(rt.utf8d as i32));
                        halves(b, rt.readerr);
                        halves(b, rt.nulerr);
                        halves(b, rt.utf8err);
                        if gen {
                            rt.read_file_gen
                        } else {
                            rt.read_file
                        }
                    }
                    "readFileBytes" => {
                        if gen {
                            b.ins(&Instruction::I32Const(crate::GEN_MODE_READ_BYTES));
                        }
                        halves(b, rt.readerr);
                        if gen {
                            halves(b, rt.nulerr);
                            rt.read_file_bytes_gen
                        } else {
                            rt.read_file_bytes
                        }
                    }
                    _ => {
                        halves(b, rt.writeerr);
                        rt.fsync_file
                    }
                };
                b.ins(&Instruction::Call(f));
                b.slot(off);
                return Ok(ty);
            }
            // RFC-0111: a path and an `Array<UInt8>`, into the same destination
            // slot `writeFile` uses. The array arrives as a POINTER to its
            // `{ ptr, len, cap }` record, so the two words are loaded out of it
            // and pushed separately — the buffer may hold NULs, which is exactly
            // what the String writer's `strlen` could not have measured.
            "writeFileBytes" if args.len() == 2 => {
                let ty = io_builtin_ty(name, 2).expect("writeFileBytes is an I/O builtin");
                let l = self.layout_of(&ty, line)?;
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                self.expr_as(m, b, &args[0], &Type::Str)?;
                let bytes = Type::Array(Box::new(Type::IntN {
                    bits: 8,
                    signed: false,
                }));
                self.expr_as(m, b, &args[1], &bytes)?;
                let src = self.scratch(b, ValType::I32, 0);
                let al = self.layout_of(&bytes, line)?;
                b.ins(&Instruction::LocalSet(src));
                b.ins(&Instruction::LocalGet(src));
                b.ins(&Instruction::I32Load(word_at(al.fields[0])));
                b.ins(&Instruction::LocalGet(src));
                b.ins(&Instruction::I64Load(at(al.fields[1])));
                b.ins(&Instruction::I32WrapI64);
                let (pre, post) = self.cx.rt.writeerr;
                b.ins(&Instruction::I32Const(pre as i32));
                b.ins(&Instruction::I32Const(post as i32));
                b.ins(&Instruction::Call(self.cx.rt.write_file_bytes));
                b.slot(off);
                return Ok(ty);
            }
            // RFC-0111: `print` for bytes. `write_all` is already the gathered
            // stdout writer every printed line goes through, so this is that call
            // with the caller's buffer — same buffering, same ordering against
            // `print` and against standard error. Its status is dropped, for the
            // reason `print` drops it.
            "writeStdout" if args.len() == 1 => {
                let bytes = Type::Array(Box::new(Type::IntN {
                    bits: 8,
                    signed: false,
                }));
                self.expr_as(m, b, &args[0], &bytes)?;
                let src = self.scratch(b, ValType::I32, 0);
                let al = self.layout_of(&bytes, line)?;
                b.ins(&Instruction::LocalSet(src));
                b.ins(&Instruction::I32Const(1));
                b.ins(&Instruction::LocalGet(src));
                b.ins(&Instruction::I32Load(word_at(al.fields[0])));
                b.ins(&Instruction::LocalGet(src));
                b.ins(&Instruction::I64Load(at(al.fields[1])));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::Call(self.cx.rt.write_all));
                b.ins(&Instruction::Drop);
                return Ok(Type::Unit);
            }
            // Two strings in, a `Result<Bool, String>` out, through a destination
            // slot — the same shape, so one arm. RFC-0044's `renameFile` differs
            // from `writeFile` only in which runtime function it calls.
            "writeFile" | "renameFile" if args.len() == 2 => {
                let ty = io_builtin_ty(name, 2).expect("both writers are I/O builtins");
                let l = self.layout_of(&ty, line)?;
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                self.expr_as(m, b, &args[0], &Type::Str)?;
                self.expr_as(m, b, &args[1], &Type::Str)?;
                let rt = self.cx.rt;
                b.ins(&Instruction::I32Const(rt.writeerr.0 as i32));
                b.ins(&Instruction::I32Const(rt.writeerr.1 as i32));
                if name == "renameFile" {
                    b.ins(&Instruction::I32Const(rt.xdeverr.0 as i32));
                    b.ins(&Instruction::I32Const(rt.xdeverr.1 as i32));
                }
                b.ins(&Instruction::Call(if name == "writeFile" {
                    rt.write_file
                } else {
                    rt.rename_file
                }));
                b.slot(off);
                return Ok(ty);
            }
            // The two builtins RFC-0078 refused to route, and therefore the two
            // this backend owes a loop. `text_runtime` is where those loops are and
            // why they are not `std/num` and `std/text`.
            //
            // `parse` writes its `Option<Int64>` through a slot allocated here,
            // which is the same hidden destination `readLine` gets — the M2b
            // aggregate rule rather than a case of its own. `parseI64` is a
            // `std/runtime` function returning the `Option`, so the destination
            // leads, as it does for every aggregate result (`wasm_sig`).
            "parse" if args.len() == 1 => {
                let ty = Type::option(Type::Int);
                let l = self.layout_of(&ty, line)?;
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                self.expr_as(m, b, &args[0], &Type::Str)?;
                b.ins(&Instruction::Call(self.cx.rt.parse_i64));
                b.slot(off);
                return Ok(ty);
            }
            // `lineAt(bytes, off)` / `colAt(bytes, off)`. The buffer goes through
            // `walk`, so an `Array`, a fixed `ArrayN` and a `SmallArray` all arrive
            // as one base-and-count — the same three the checker accepts — and the
            // helper is handed exactly what the C one is handed natively.
            //
            // The offset lands in a FRESH local rather than a scratch: the two
            // pushes below it have to survive whatever evaluating it does, and a
            // scratch key is shared (the M2g `box_value` bug).
            "lineAt" | "colAt" if args.len() == 2 => {
                let bty = self.expr(m, b, &args[0])?;
                let w = self.walk(b, &bty, line)?;
                // A byte buffer, i.e. stride 1. The interpreter takes `v as u8` of
                // whatever the elements are and the native helper reads
                // `unsigned char*` off the data pointer, so a wider element would
                // have three engines reading three different things — and nothing
                // but `bytes(s)` ever reaches here.
                if w.stride != 1 {
                    return unsupported(&format!("`{name}` over a buffer of `{}`", w.elem), line);
                }
                self.expr_as(m, b, &args[1], &Type::Int)?;
                let o = b.local(ValType::I64);
                b.ins(&Instruction::LocalSet(o));
                b.ins(&Instruction::LocalGet(w.data));
                b.ins(&Instruction::LocalGet(w.len));
                b.ins(&Instruction::LocalGet(o));
                let f = if name == "lineAt" {
                    self.cx.rt.line_at
                } else {
                    self.cx.rt.col_at
                };
                b.ins(&Instruction::Call(f));
                return Ok(Type::Int);
            }
            // RFC-0083 M1. Construction starts from `v128.const 0` and replaces
            // each lane in written order; a splat is the one opcode. No lane is
            // read back before it is written, so the zero start costs nothing that
            // an undefined one would have saved.
            //
            // M3's integer width is the same two shapes with the lane-typed
            // opcodes swapped, which is what M1 meant by "one internal name per
            // width": nothing here decodes a receiver.
            "F32x4" | "@f32x4Splat" | "I32x4" | "@i32x4Splat" | "F64x2" | "@f64x2Splat"
                if !args.is_empty() =>
            {
                let wide = name.starts_with("@f64x2") || name == "F64x2";
                let int = name.starts_with("@i32x4") || name == "I32x4";
                let (vec, lane) = if int {
                    (Type::I32x4, INT32)
                } else if wide {
                    (Type::F64x2, Type::Float)
                } else {
                    (Type::F32x4, Type::Float32)
                };
                if name.ends_with("Splat") {
                    self.expr_as(m, b, &args[0], &lane)?;
                    b.ins(if int {
                        &Instruction::I32x4Splat
                    } else if wide {
                        &Instruction::F64x2Splat
                    } else {
                        &Instruction::F32x4Splat
                    });
                } else {
                    b.ins(&Instruction::V128Const(0));
                    for (i, a) in args.iter().enumerate() {
                        self.expr_as(m, b, a, &lane)?;
                        b.ins(&if int {
                            Instruction::I32x4ReplaceLane(i as u8)
                        } else if wide {
                            Instruction::F64x2ReplaceLane(i as u8)
                        } else {
                            Instruction::F32x4ReplaceLane(i as u8)
                        });
                    }
                }
                return Ok(vec);
            }
            // The lane index was proven constant and in range by the checker, so
            // this is a plain immediate and there is no bounds check to emit.
            "@lane" if args.len() == 2 => {
                let vt = self.expr(m, b, &args[0])?;
                let vt = self.cx.resolve(&vt);
                let lanes = if matches!(vt, Type::F64x2 | Type::Mask64x2) {
                    2
                } else {
                    4
                };
                let Some(k) = ftypes::const_lane(&args[1], lanes) else {
                    return unsupported("a lane index that is not a constant", line);
                };
                // A mask lane is all-ones or all-zeros; `Bool` rides an `i32` that
                // must be 0 or 1, so the extract is followed by a test against
                // zero rather than being handed over raw — `-1` where `1` is
                // expected would print `true` and compare unequal to `true`. The
                // wide mask extracts an `i64`, so its `eqz` is the 64-bit one and
                // the second `eqz` — the one that puts the sense back — is the
                // 32-bit one, because the first already left an `i32` behind.
                if vt == Type::Mask64x2 {
                    b.ins(&Instruction::I64x2ExtractLane(k));
                    b.ins(&Instruction::I64Eqz);
                    b.ins(&Instruction::I32Eqz);
                    return Ok(Type::Bool);
                }
                if vt == Type::Mask32x4 {
                    b.ins(&Instruction::I32x4ExtractLane(k));
                    b.ins(&Instruction::I32Eqz);
                    b.ins(&Instruction::I32Eqz);
                    return Ok(Type::Bool);
                }
                // An `Int32` lane needs no normalising: `i32x4.extract_lane` is
                // already the whole 32-bit value, and `Int32` rides an `i32`.
                if vt == Type::I32x4 {
                    b.ins(&Instruction::I32x4ExtractLane(k));
                    return Ok(INT32);
                }
                if vt == Type::F64x2 {
                    b.ins(&Instruction::F64x2ExtractLane(k));
                    return Ok(Type::Float);
                }
                b.ins(&Instruction::F32x4ExtractLane(k));
                return Ok(Type::Float32);
            }
            // `v.replaceLane(k, x)` — the same immediate as the read, and the same
            // opcode the four-argument constructor above already uses one lane at a
            // time. Vectors only: the checker refuses a mask receiver.
            "@replaceLane" if args.len() == 3 => {
                let vt = self.cx.resolve(&self.peek(&args[0], line)?);
                let int = vt == Type::I32x4;
                let wide = vt == Type::F64x2;
                self.expr_as(m, b, &args[0], &vt)?;
                let Some(k) = ftypes::const_lane(&args[1], if wide { 2 } else { 4 }) else {
                    return unsupported("a lane index that is not a constant", line);
                };
                let lane = if int {
                    &INT32
                } else if wide {
                    &Type::Float
                } else {
                    &Type::Float32
                };
                self.expr_as(m, b, &args[2], lane)?;
                b.ins(&if int {
                    Instruction::I32x4ReplaceLane(k)
                } else if wide {
                    Instruction::F64x2ReplaceLane(k)
                } else {
                    Instruction::F32x4ReplaceLane(k)
                });
                return Ok(vt);
            }
            // Mask reductions (RFC-0083 M2). Both push an `i32` that is already 0
            // or 1, so unlike the mask lane read there is no normalising `i32.eqz`
            // pair to add.
            //
            // `v128.any_true` is whole-vector — any bit set anywhere — where
            // `i32x4.all_true` is per lane. They coincide here because a
            // `Mask32x4` lane is all-ones or all-zeros and nothing else can build
            // one; that is the same closed-inhabitants argument that let the mask
            // be its own type. There is no `i32x4.any_true` to reach for instead:
            // the encoder carries exactly one any-true, at v128 width.
            //
            // `all_true` is the one that has to know the width — `i64x2.all_true`
            // is a different opcode reading the same 128 bits as two lanes instead
            // of four, and reading a `Mask64x2` with the 32-bit one would answer
            // correctly for all-true and all-false and diverge only on a mixed
            // mask. `any_true` is unchanged because it never had a lane width.
            "@anyTrue" | "@allTrue" => {
                let mt = self.cx.resolve(&self.peek(&args[0], line)?);
                let wide = mt == Type::Mask64x2;
                self.expr_as(m, b, &args[0], &mt)?;
                b.ins(&if name == "@anyTrue" {
                    Instruction::V128AnyTrue
                } else if wide {
                    Instruction::I64x2AllTrue
                } else {
                    Instruction::I32x4AllTrue
                });
                return Ok(Type::Bool);
            }
            // RFC-0083 M2. `min`/`max` are wasm's own, which is the rule the other
            // two engines were pointed AT rather than the one they fell into: NaN
            // in either operand propagates and `-0.0` orders below `+0.0`.
            //
            // `f32x4.nearest` is roundTiesToEven, and it is the engine with no
            // choice again: the other two were pointed at it (`llvm.roundeven`,
            // `round_ties_even`) rather than at their `round`, which is ties-away
            // and answers 3 for 2.5.
            "@f32x4Min" | "@f32x4Max" | "@f32x4Sqrt" | "@f32x4Ceil" | "@f32x4Floor"
            | "@f32x4Trunc" | "@f32x4Nearest" => {
                self.expr_as(m, b, &args[0], &Type::F32x4)?;
                if args.len() == 2 {
                    self.expr_as(m, b, &args[1], &Type::F32x4)?;
                }
                b.ins(&match name {
                    "@f32x4Min" => Instruction::F32x4Min,
                    "@f32x4Max" => Instruction::F32x4Max,
                    "@f32x4Ceil" => Instruction::F32x4Ceil,
                    "@f32x4Floor" => Instruction::F32x4Floor,
                    "@f32x4Trunc" => Instruction::F32x4Trunc,
                    "@f32x4Nearest" => Instruction::F32x4Nearest,
                    _ => Instruction::F32x4Sqrt,
                });
                return Ok(Type::F32x4);
            }
            // The wide width's three (RFC-0083 M4). Same rule, same reason: wasm's
            // `f64x2.min` is IEEE-754-2019 `minimum` and the other two engines were
            // pointed at it rather than at their own default.
            "@f64x2Min" | "@f64x2Max" | "@f64x2Sqrt" => {
                self.expr_as(m, b, &args[0], &Type::F64x2)?;
                if args.len() == 2 {
                    self.expr_as(m, b, &args[1], &Type::F64x2)?;
                }
                b.ins(&match name {
                    "@f64x2Min" => Instruction::F64x2Min,
                    "@f64x2Max" => Instruction::F64x2Max,
                    _ => Instruction::F64x2Sqrt,
                });
                return Ok(Type::F64x2);
            }
            // (`@f32x4Abs` was here as `f32x4.abs`, deleted in M4 — and this is the
            // column that kept it two milestones too long. Its census row claimed
            // 3.5x HERE, which was four calls Cranelift declined to inline and not
            // the instruction; written inline the walk is 54 ms against 58 ms over
            // 102 M lanes — 1.07x, `select`'s bar. See RFC-0083's M4 note.)
            //
            // (`@i32x4Min`/`Max`/`Abs` were here, as `i32x4.min_s`/`max_s`/`abs`,
            // and were deleted on their measurement. This is the column that came
            // CLOSEST to keeping them and still did not: over 200 M lanes the
            // builtin walk is 139 ms against the Vyrn one's 146 ms — 1.05x, and
            // `select` was refused at 1.06x. The 273 ms the same walk shows with
            // the Vyrn version behind a helper function is Cranelift not inlining
            // a call, not the operation. See RFC-0083's M3 note.)
            //
            // Four consecutive elements of an `Array<Float32>` / `Array<Int32>` as
            // one 16-byte access, behind ONE bounds check rather than four. Both
            // widths are the same `v128.load`: the element stride is 4 either way,
            // which is why `walk`/`elem_addr` need no lane knowledge.
            "@f32x4Load" | "@f32x4Store" | "@i32x4Load" | "@i32x4Store" | "@f64x2Load"
            | "@f64x2Store" => {
                let (vec, span) = if name.starts_with("@i32x4") {
                    (Type::I32x4, 4)
                } else if name.starts_with("@f64x2") {
                    // Two lanes, an 8-byte stride — and `elem_addr` still needs no
                    // lane knowledge, because it scales by the ELEMENT size the
                    // array already carries. Only the check's span is ours.
                    (Type::F64x2, 2)
                } else {
                    (Type::F32x4, 4)
                };
                let aty = self.expr(m, b, &args[0])?;
                let w = self.walk(b, &aty, line)?;
                self.expr_as(m, b, &args[1], &Type::Int)?;
                let idx = b.local(ValType::I64);
                b.ins(&Instruction::LocalSet(idx));
                self.bounds_check_span(b, &w, idx, span);
                if name.ends_with("Load") {
                    self.elem_addr(b, &w, idx);
                    // `align: 0` — a log2 exponent, so one byte. The buffer is an
                    // array of elements, so nothing guarantees the 16 a
                    // `v128.load` would like, and an overstated hint is a
                    // validation-legal lie the engine may act on. The textual
                    // backend understates for the same reason, in the other unit:
                    // its `align 4` is a BYTE count, not this exponent.
                    b.ins(&Instruction::V128Load(MemArg {
                        offset: 0,
                        align: 0,
                        memory_index: 0,
                    }));
                    return Ok(vec);
                }
                self.elem_addr(b, &w, idx);
                self.expr_as(m, b, &args[2], &vec)?;
                b.ins(&Instruction::V128Store(MemArg {
                    offset: 0,
                    align: 0,
                    memory_index: 0,
                }));
                return Ok(Type::Unit);
            }
            // `Int64(x)` / `UInt16(x)` — a conversion, not a call. Which names are
            // conversions is the frontend's answer (`numeric_conv_target`), so the
            // two backends cannot disagree about whether `Int64` is a cast, and the
            // conversion itself is the M2d seam rather than a second truncation
            // rule: an out-of-range width is refused there, once, for every flow.
            _ if args.len() == 1 && ftypes::numeric_conv_target(name).is_some() => {
                let to = ftypes::numeric_conv_target(name).unwrap();
                self.expr_as(m, b, &args[0], &to)?;
                return Ok(to);
            }
            // `t.join()` (RFC-0025). The task already ran, at the spawn point, so
            // there is nothing to wait for: this is a read out of its heap box.
            //
            // Since RFC-0095 M1 the join CONSUMES the task, so the box goes back
            // here — the wasm half of "free the frame, free the record, close the
            // handle", of which this target has only the first: there are no
            // threads, so `VTask` is the box and there is no handle. The read
            // happens before the free, and a second `t.join()` is a compile
            // error, so nothing reads the box afterwards.
            "@join" if args.len() == 1 => {
                let t = self.expr(m, b, &args[0])?;
                let Type::Task(inner) = self.cx.resolve(&t) else {
                    // The checker admits nothing else; keep the textual backend's
                    // defensive identity rather than inventing a diagnostic.
                    return Ok(t);
                };
                // The box's address, kept: every arm below consumes it off the
                // stack, and the free needs it again.
                let box_ = b.local(ValType::I32);
                b.ins(&Instruction::LocalTee(box_));
                match self.cx.repr(&inner, line)? {
                    Repr::Scalar(_) => {
                        b.ins(&load_of(&self.cx.ll(&inner), 0, self.cx.signed(&inner)));
                    }
                    // A copy, where the LLVM backend emits `load {ll}`. Handing
                    // out the box's own address would make a joined aggregate an
                    // alias into the task's result — M2l's `get` hazard, one
                    // container along. Since M1 it would also be a read of freed
                    // memory, because the box goes back three instructions later.
                    Repr::Agg(l) => {
                        let src = b.local(ValType::I32);
                        b.ins(&Instruction::LocalSet(src));
                        let off = b.alloc(l.size, l.align);
                        b.slot(off);
                        b.ins(&Instruction::LocalGet(src));
                        b.ins(&Instruction::I32Const(l.size as i32));
                        b.ins(&Instruction::MemoryCopy {
                            src_mem: 0,
                            dst_mem: 0,
                        });
                        b.slot(off);
                    }
                    // A Unit task has no result to read, but it still has a box:
                    // the `Task` was a value and has to be consumed.
                    Repr::Unit => {
                        b.ins(&Instruction::Drop);
                    }
                }
                // The result is already an operand (or a slot address of this
                // frame's own), so freeing the box now cannot invalidate it.
                b.ins(&Instruction::LocalGet(box_));
                b.ins(&Instruction::Call(self.cx.rt.free));
                return Ok(*inner);
            }
            "@has" | "@remove" if args.len() == 2 => {
                return self.map_method(m, b, name, args, line)
            }
            "@keys" if args.len() == 1 => return self.map_method(m, b, name, args, line),
            // `a[i]` dispatches (RFC-0091 M2): it asks the receiver's type for a
            // `place at` projection and inlines its body here. A builtin
            // container takes the seeded row, whose body is `yield @slot(self,
            // i)`, so the element lowering below is reached through the same
            // table a user container reaches its own through.
            "@at" if args.len() == 2 => return self.project_at(m, b, args, line),
            n if n == vyrn_frontend::project::ELEM && args.len() == 2 => {
                return self.at(m, b, args, line)
            }
            // `x.copy()` (RFC-0089 M1b) — the receiver's value, with heap of its
            // own. The reported type is the receiver's own.
            "@copy" if args.len() == 1 => {
                // RFC-0091 M1: a type that declares `impl Copy for T` says what
                // duplicating it means, so the call goes there instead. The
                // receiver is named by `peek` rather than emitted first, because
                // the dispatch has to choose before anything is emitted.
                if let Some(f) = self
                    .peek(&args[0], line)
                    .ok()
                    .and_then(|t| ftypes::copy_impl(&self.cx.impls, &t))
                {
                    return self.call(m, b, &f, args, line);
                }
                let ty = self.expr(m, b, &args[0])?;
                self.copy_stack(m, b, &ty, line)?;
                return Ok(ty);
            }
            "@push" if args.len() == 2 => return self.push(m, b, args, line),
            "@reserve" if args.len() == 2 => return self.reserve_arr(m, b, args, line),
            "@clear" if args.len() == 1 => return self.clear_arr(m, b, args, line),
            "@append" if args.len() == 2 => return self.append_arr(m, b, args, line),
            "@copyFrom" if args.len() == 2 => return self.copy_from_arr(m, b, args, line),
            "@tally" if args.len() == 3 => return self.map_tally(m, b, args, line),
            "@tallyBytes" if args.len() == 3 => return self.map_tally_bytes(m, b, args, line),
            // A `SmallArray` receiver takes the four-field path. Dispatched on
            // `peek` rather than on an emitted type, because the receiver must not
            // be evaluated twice — `sa_method` evaluates it itself, and for `pop`
            // and `swapRemove` it needs the BINDING rather than a value.
            "@pop" | "@swapRemove" | "@toArray"
                if !args.is_empty()
                    && matches!(
                        self.peek(&args[0], line).map(|t| self.cx.resolve(&t)),
                        Ok(Type::SmallArray(..))
                    ) =>
            {
                let Ok(Type::SmallArray(inner, n)) =
                    self.peek(&args[0], line).map(|t| self.cx.resolve(&t))
                else {
                    return unsupported(&format!("`{name}` on a non-SmallArray"), line);
                };
                let aty = Type::SmallArray(inner.clone(), n);
                return self.sa_method(m, b, name, args, &aty, &inner, n, line);
            }
            // RFC-0075 M2b. `fromArray` is no longer a retype: a stream is a
            // six-word header now and the array's three words go into it, with
            // the read cursor at 0 and the producer tag at -1.
            "fromArray" if args.len() == 1 => {
                let got = self.expr(m, b, &args[0])?;
                let inner = match self.cx.resolve(&got) {
                    Type::Array(i) => *i,
                    other => return unsupported(&format!("`fromArray` of `{other}`"), line),
                };
                return self.stream_from_array(b, &inner, line);
            }
            "fromStep" if args.len() == 3 => return self.stream_from_step(m, b, args, line),
            "boxStream" if args.len() == 1 => return self.stream_box(m, b, args, line),
            "unboxStream" if args.len() == 1 => return self.stream_unbox(m, b, args, line),
            "pullAt" if args.len() == 1 => return self.stream_pull_at(m, b, args, line),
            // `close` reclaims what this backend CAN reclaim. Its `malloc` is a
            // bump pointer that never frees, so a buffer stream's teardown is
            // still nothing — but a stepped one owns a cell, and cells come from
            // a fixed slab of 65536 that a leak would exhaust. Which of the two
            // it is, is the tag.
            "close" if args.len() == 1 => {
                let got = self.expr(m, b, &args[0])?;
                let elem = match self.cx.resolve(&got) {
                    Type::Stream(i) => *i,
                    other => return unsupported(&format!("`close` of `{other}`"), line),
                };
                let s = b.local(ValType::I32);
                b.ins(&Instruction::LocalSet(s));
                self.stream_release(m, b, Place::Local(s), &elem, line)?;
                return Ok(Type::Unit);
            }
            "@pop" if args.len() == 1 => return self.pop(b, args, line),
            "@swapRemove" if args.len() == 2 => return self.swap_remove(m, b, args, line),
            // `list([..])` is the explicit spelling of the contextual literal;
            // both land on the same `ArrayN → Array` conversion.
            "@list" if args.len() == 1 => {
                let got = self.expr(m, b, &args[0])?;
                return match self.cx.resolve(&got) {
                    Type::Array(_) => Ok(got),
                    Type::ArrayN(inner, n) => {
                        let want = Type::Array(inner.clone());
                        self.heapify(b, &inner, n, &want, line)?;
                        Ok(want)
                    }
                    other => unsupported(&format!("`list` of `{other}`"), line),
                };
            }
            _ => {}
        }
        // Calling a `fn`-typed PARAMETER inside a specialization (RFC-0023): a
        // direct call to the resolved target with this instance's own capture
        // parameters prepended. No function pointer exists — the target is an
        // index the discovering call site handed out, and the captures are values
        // fixed at that site.
        if let Some(bnd) = self.fn_binds.get(name).cloned() {
            return self.target_call(m, b, &bnd, args, line);
        }
        // A call through a stored function value (RFC-0037): one direct call to
        // the signature's dispatcher. The receiver is always a NAME — a `let`, a
        // `for` variable, a `match` binding, a field read into a local — which is
        // the surface RFC-0037 defines.
        if let Ok((_, ty)) = self.lookup(name, line) {
            let norm = crate::normalize_fn_sig(&self.cx.sub(&ty), &self.cx.types);
            if matches!(norm, Type::Fn(..)) {
                let recv = Expr::Var {
                    name: name.to_string(),
                    line,
                };
                return self.fnval_call(m, b, &recv, &norm, args, line);
            }
        }
        if let Some(t) = self.sum_ctor(m, b, name, args, line, hint.clone())? {
            return Ok(t);
        }
        // `Age(n)` — the explicit spelling of what a boundary now does by itself
        // (RFC-0003). Same rule as the record literal above: a constant was
        // proven by the checker, so only a dynamic value pays for a check.
        if let Some(d) = self
            .cx
            .types
            .get(name)
            .filter(|d| d.predicate.is_some())
            .cloned()
        {
            if args.len() != 1 {
                return unsupported(&format!("`{name}` at this arity"), line);
            }
            self.expr_as(m, b, &args[0], &d.base)?;
            if vyrn_frontend::consteval::eval(&args[0], &HashMap::new()).is_none() {
                self.emit_validation(b, &d, line)?;
            }
            return Ok(Type::Named(name.to_string()));
        }
        // A protocol method (RFC-0002 §5): `x.show()` parses as `show(x)` and
        // dispatches statically on the receiver's concrete type — which inside a
        // bounded generic is concrete only because `subst` says so. The same
        // mangled impl the textual emitter calls, so there is one naming scheme.
        if let Some(proto) = self.cx.protocol_methods.get(name).cloned() {
            let recv = args.first().ok_or_else(|| {
                gap(
                    &format!("the protocol method `{name}` with no receiver"),
                    line,
                )
            })?;
            let rty = self.peek(recv, line)?;
            let rty = self.cx.sub(&rty);
            let key = ftypes::type_key(&rty)
                .ok_or_else(|| gap(&format!("`{name}` dispatched on `{rty}`"), line))?;
            let mangled = ftypes::impl_method_name(&proto, &key, name);
            return self.call(m, b, &mangled, args, line);
        }
        // A function with `fn`-typed parameters (RFC-0023): resolve each
        // function-value argument to a direct-call target, specialize the callee
        // per those targets, and call the specialization with the captures
        // appended. The shell itself was never emitted.
        if let Some(f) = self.cx.higher_order.get(name).copied() {
            if f.params.len() != args.len() {
                return unsupported(&format!("the call `{name}` at this arity"), line);
            }
            return self.ho_call(m, b, f, args, line);
        }
        // A generic callee: solve its type arguments, discover the specialization
        // (which is what hands out its function index), then call it like any
        // other function.
        if let Some(f) = self.cx.generics.get(name).copied() {
            if f.params.len() != args.len() {
                return unsupported(&format!("the call `{name}` at this arity"), line);
            }
            let arg_tys = self.arg_types(
                &f.params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>(),
                args,
                line,
            )?;
            let want = self.expect.last().cloned();
            let (subst, solved) = crate::solve_with_expected(
                &f.type_params,
                &f.params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>(),
                &arg_tys,
                &f.ret,
                want.as_ref(),
            );
            let mut type_args = Vec::new();
            for (tp, got) in f.type_params.iter().zip(solved) {
                match got {
                    Some(t) => type_args.push(t),
                    // The textual emitter substitutes `Unit` and lowers it to
                    // `void`; in wasm that is a signature with one fewer
                    // parameter, which is a different function rather than a
                    // diagnostic.
                    None => {
                        return unsupported(
                            &format!(
                                "a generic type parameter `{tp}` the call `{name}` does not fix"
                            ),
                            line,
                        )
                    }
                }
            }
            let sig = self.cx.instantiate(m, &f, type_args, subst)?;
            return self.emit_call(m, b, &sig, args, hint);
        }
        // RFC-0043's host boundary. These three are not `vyrn` host imports like
        // an ordinary RFC-0012 `extern`: the C shim defines them on every target,
        // honouring `VYRN_FIXED_TIME`/`VYRN_FIXED_SEED`, which is what makes a
        // clock example a three-way parity citizen instead of a browser-only one.
        //
        // M2i got them by reaching that shim, and M2j took it back out: a shape
        // that only works linked cannot be what `vyrn build --target wasm` does,
        // because M5's criterion is no clang. WASI has `clock_time_get` and
        // `random_get`, the env injection is `environ_get`, and wasi-libc's
        // `timespec_get`/`getentropy` are thin wrappers over the first two — so
        // the emitted runtime reads the same syscalls by a shorter route.
        if let Some(sym) = crate::host_boundary_extern(name) {
            // Each reader takes the interned name of its injected value
            // (`VYRN_FIXED_TIME=`, `VYRN_FIXED_SEED=`), which is how the
            // harness fixes a clock example (RFC-0043).
            let (f, key) = match sym {
                "__vyrn_now_millis" => (self.cx.rt.now_millis, self.cx.rt.fixed_time),
                "__vyrn_monotonic_nanos" => (self.cx.rt.mono_nanos, self.cx.rt.fixed_time),
                _ => (self.cx.rt.random_seed, self.cx.rt.fixed_seed),
            };
            if !args.is_empty() {
                return unsupported(&format!("the call `{name}` at this arity"), line);
            }
            b.ins(&Instruction::I32Const(key as i32));
            // What it returns is the declaration's business, not this file's, and
            // the boundary hands back an `i64`. Anything else spelled over one of
            // these reserved names would read the wrong bytes silently.
            let ret = self
                .cx
                .externs
                .get(name)
                .map(|e| e.ret.clone())
                .unwrap_or(Type::Unit);
            if self.cx.repr(&ret, line)? != Repr::Scalar(ValType::I64) {
                return unsupported(&format!("`{name}` declared as returning `{ret}`"), line);
            }
            b.ins(&Instruction::Call(f));
            return Ok(ret);
        }
        // RFC-0012 M1: a real call into the host, through the `vyrn` import
        // declared from this `extern fn`'s own signature.
        //
        // Every ABI conversion the textual backend's `to_extern_abi` performs is
        // already done here by the carrier invariant (M2h): a `Bool` and every
        // sub-64-bit int ride an `i32`, correctly extended, which is exactly what
        // the ABI widens them to. `String` is the one shape that is not one word,
        // and it is the one thing this loop does.
        if let Some(ext) = self.cx.externs.get(name).cloned() {
            let Some(index) = ext.index else {
                // A host-boundary name handled above; anything else here is a
                // declaration this backend has no route for.
                return unsupported(&format!("the call `{name}`"), line);
            };
            if ext.params.len() != args.len() {
                return unsupported(&format!("the call `{name}` at this arity"), line);
            }
            for (i, (a, p)) in args.iter().zip(&ext.params).enumerate() {
                self.expr_as(m, b, a, p)?;
                if matches!(self.cx.resolve(p), Type::Str) {
                    // (ptr, len): the host decodes UTF-8 out of linear memory, so
                    // it needs the length a NUL-terminated pointer does not carry.
                    // Its own scratch number per argument — one local for two live
                    // values is the M2g bug, and here it would send the host a
                    // length taken from the wrong string.
                    let s = self.scratch(b, ValType::I32, 20 + i as u8);
                    b.ins(&Instruction::LocalTee(s))
                        .ins(&Instruction::LocalGet(s));
                    str_len(b);
                    b.ins(&Instruction::I64ExtendI32U);
                }
            }
            b.ins(&Instruction::Call(index));
            // The host returns an `i32` for every narrow width, and a JS number
            // out of range would otherwise be a carrier the rest of this backend
            // reads as in-range. `from_extern_abi`'s `trunc` on the other backend.
            if let Some(n) = Num::of(&self.cx.resolve(&ext.ret)) {
                renorm(b, n);
            }
            return Ok(ext.ret.clone());
        }
        // RFC-0120: a named projection dispatches here exactly as `a[i]` does —
        // the same table, its own method name. Last, so every callable of the
        // same name won above, which is the checker's resolution order too.
        if !args.is_empty()
            && self.cx.sigs.get(name).is_none()
            && self
                .cx
                .impls
                .iter()
                .any(|i| i.places.iter().any(|p| p.name == *name))
        {
            let recv = self.peek(&args[0], line).ok();
            if let Some(p) = vyrn_frontend::project::site(
                &self.cx.impls,
                recv.as_ref(),
                name,
                &args[0],
                &args[1..],
                line,
            )? {
                for s in &p.prologue {
                    self.stmt(m, b, s)?;
                }
                return self.expr(m, b, &p.place);
            }
        }
        let Some(sig) = self.cx.sigs.get(name).cloned() else {
            return unsupported(&format!("the call `{name}`"), line);
        };
        if sig.params.len() != args.len() {
            return unsupported(&format!("the call `{name}` at this arity"), line);
        }
        self.emit_call(m, b, &sig, args, hint)
    }

    /// One `std/mem` primitive (PLAN-0125-runtime §2.1 to §2.3) as its
    /// instruction, or one host import as its `call`. `std/mem.vyrn` holds the
    /// signatures and this holds the whole of their lowering; the bodies there
    /// are never read by this emitter.
    ///
    /// The argument types are spelled here as well as in the module because
    /// `expr_as` needs the target type to coerce a literal, and the row is the
    /// one place the two lists meet. A mismatch is a checker error at the call
    /// in `std/runtime` before it is anything here.
    fn mem_prim(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        prim: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let u = |bits: u8| Type::IntN {
            bits,
            signed: false,
        };
        let at = |align: u32| MemArg {
            offset: 0,
            align,
            memory_index: 0,
        };
        // PLAN-0125-runtime §2.2: a host import is one `call` of the import
        // `wasi_imports` declared, with the witx signature. The `vyrn_gen`
        // pair exists only under a generation (`Cx::gen`); an ordinary build
        // lowers a call to either as `unreachable`, and the runtime functions
        // that make one (`readFileGen`, `listDirGen`) are reached by nothing
        // there, so the sweep drops them with the branch.
        let w = self.cx.rt.wasi;
        let host: Option<(Option<u32>, Vec<Type>, Type)> = match prim {
            "fdWrite" => Some((Some(w.fd_write), vec![INT32; 4], INT32)),
            "fdRead" => Some((Some(w.fd_read), vec![INT32; 4], INT32)),
            "fdClose" => Some((Some(w.fd_close), vec![INT32], INT32)),
            "procExit" => Some((Some(w.proc_exit), vec![INT32], Type::Unit)),
            "pathOpen" => Some((
                Some(w.path_open),
                vec![
                    INT32,
                    INT32,
                    INT32,
                    INT32,
                    INT32,
                    Type::Int,
                    Type::Int,
                    INT32,
                    INT32,
                ],
                INT32,
            )),
            "pathRename" => Some((Some(w.path_rename), vec![INT32; 6], INT32)),
            "fdSync" => Some((Some(w.fd_sync), vec![INT32], INT32)),
            "fdPrestatGet" => Some((Some(w.fd_prestat_get), vec![INT32; 2], INT32)),
            "argsSizesGet" => Some((Some(w.args_sizes_get), vec![INT32; 2], INT32)),
            "argsGet" => Some((Some(w.args_get), vec![INT32; 2], INT32)),
            "environSizesGet" => Some((Some(w.environ_sizes_get), vec![INT32; 2], INT32)),
            "environGet" => Some((Some(w.environ_get), vec![INT32; 2], INT32)),
            "clockTimeGet" => Some((Some(w.clock_time_get), vec![INT32, Type::Int, INT32], INT32)),
            "randomGet" => Some((Some(w.random_get), vec![INT32; 2], INT32)),
            "fdReaddir" => Some((
                Some(w.fd_readdir),
                vec![INT32, INT32, INT32, Type::Int, INT32],
                INT32,
            )),
            "genRead" => Some((self.cx.gen.map(|g| g.read), vec![INT32, INT32], Type::Int)),
            "genFetch" => Some((self.cx.gen.map(|g| g.fetch), vec![INT32], Type::Unit)),
            _ => None,
        };
        if let Some((index, params, ret)) = host {
            if args.len() != params.len() {
                return unsupported(&format!("`std/mem.{prim}` at this arity"), line);
            }
            for (a, p) in args.iter().zip(&params) {
                self.expr_as(m, b, a, p)?;
            }
            match index {
                Some(i) => b.ins(&Instruction::Call(i)),
                None => b.ins(&Instruction::Unreachable),
            };
            return Ok(ret);
        }
        let (params, ret): (Vec<Type>, Type) = match prim {
            "load8" => (vec![INT32], u(8)),
            "load16" => (vec![INT32], u(16)),
            "load32" => (vec![INT32], u(32)),
            "load64" => (vec![INT32], u(64)),
            "loadF32" => (vec![INT32], Type::Float32),
            "loadF64" => (vec![INT32], Type::Float),
            "store8" => (vec![INT32, u(8)], Type::Unit),
            "store16" => (vec![INT32, u(16)], Type::Unit),
            "store32" => (vec![INT32, u(32)], Type::Unit),
            "store64" => (vec![INT32, u(64)], Type::Unit),
            "storeF32" => (vec![INT32, Type::Float32], Type::Unit),
            "storeF64" => (vec![INT32, Type::Float], Type::Unit),
            "copy" => (vec![INT32, INT32, INT32], Type::Unit),
            "fill" => (vec![INT32, u(8), INT32], Type::Unit),
            "memorySize" => (vec![], INT32),
            "grow" => (vec![INT32], INT32),
            "heapBase" => (vec![], INT32),
            "trap" => (vec![INT32, INT32], Type::Unit),
            _ => return unsupported(&format!("the `std/mem` primitive `{prim}`"), line),
        };
        if args.len() != params.len() {
            return unsupported(&format!("`std/mem.{prim}` at this arity"), line);
        }
        if prim == "trap" {
            // The descriptor, under the message and its length.
            b.ins(&Instruction::I32Const(2));
        }
        for (a, p) in args.iter().zip(&params) {
            self.expr_as(m, b, a, p)?;
        }
        match prim {
            "load8" => b.ins(&Instruction::I32Load8U(at(0))),
            "load16" => b.ins(&Instruction::I32Load16U(at(1))),
            "load32" => b.ins(&Instruction::I32Load(at(2))),
            "load64" => b.ins(&Instruction::I64Load(at(3))),
            "loadF32" => b.ins(&Instruction::F32Load(at(2))),
            "loadF64" => b.ins(&Instruction::F64Load(at(3))),
            "store8" => b.ins(&Instruction::I32Store8(at(0))),
            "store16" => b.ins(&Instruction::I32Store16(at(1))),
            "store32" => b.ins(&Instruction::I32Store(at(2))),
            "store64" => b.ins(&Instruction::I64Store(at(3))),
            "storeF32" => b.ins(&Instruction::F32Store(at(2))),
            "storeF64" => b.ins(&Instruction::F64Store(at(3))),
            "copy" => b.ins(&Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            }),
            "fill" => b.ins(&Instruction::MemoryFill(0)),
            "memorySize" => b.ins(&Instruction::MemorySize(0)),
            "grow" => b.ins(&Instruction::MemoryGrow(0)),
            "heapBase" => b.ins(&Instruction::GlobalGet(HEAP_BASE)),
            // `write_all` first, so the stdout buffer is flushed ahead of the
            // message — the order `trap` keeps.
            "trap" => b
                .ins(&Instruction::Call(self.cx.rt.write_all))
                .ins(&Instruction::Drop)
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::Call(self.cx.rt.proc_exit))
                .ins(&Instruction::Unreachable),
            _ => unreachable!("matched above"),
        };
        Ok(ret)
    }

    /// One log line: `[LEVEL] name: message\n`, to the configured descriptor.
    ///
    /// Reached only when the level clears the threshold — a suppressed call never
    /// gets here, which is what makes RFC-0008's fold a fold.
    ///
    /// Five `write_all`s rather than one assembled string, because `write_all` is
    /// the ONE place bytes leave this module and the pieces are already where they
    /// need to be: three are interned constants of known length, and the other two
    /// are the `ptr`s a `String` is. Concatenating first would cost three `malloc`s
    /// out of an allocator that never frees, to save four calls that are the same
    /// syscall either way. There is nothing to interleave with — RFC-0008 bars
    /// logging from a spawned task.
    ///
    /// The two `String`s are parked in scratch locals because each `write_all`
    /// consumes three operands, so the second value cannot wait on the stack under
    /// the first one's call.
    fn log_write(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        level: &str,
        line: usize,
    ) -> Result<(), String> {
        // Interned AT the use site, the way M2m interns a DFA table: `Module::data`
        // shares identical contents, so five sites at one level get one string
        // without anything having gone looking for them.
        let prefix = format!("[{}] ", level.trim_start_matches('@').to_uppercase());
        let (at, plen) = (self.cx.rt.intern(m, &prefix), prefix.len() as i32);
        let colon = self.cx.rt.intern(m, ": ");
        let nl = self.cx.rt.intern(m, "\n");
        let (name, msg) = (
            self.scratch(b, ValType::I32, 7),
            self.scratch(b, ValType::I32, 8),
        );
        b.ins(&Instruction::LocalSet(msg));
        b.ins(&Instruction::LocalSet(name));
        let write_all = self.cx.rt.write_all;
        // The descriptor, decided at compile time: 2 and 1 are WASI's own stderr
        // and stdout, and a file sink reads the one `_start` opened. There is no
        // fourth case, and a sink this backend could not serve would be a gap
        // rather than a default.
        let fd = |b: &mut Frame| match self.cx.log_fd {
            Some(at) => {
                b.ins(&Instruction::I32Const(at as i32));
                b.ins(&Instruction::I32Load(word()));
            }
            None => {
                b.ins(&Instruction::I32Const(match self.cx.log_sink {
                    LogSink::Stdout => 1,
                    _ => 2,
                }));
            }
        };
        if self.cx.log_fd.is_none() && matches!(self.cx.log_sink, LogSink::File(_)) {
            return unsupported("a `file(..)` log sink with no descriptor", line);
        }
        // A constant piece knows its own length. A `String` is a NUL-terminated
        // `ptr`, so its length is a `strlen` of the same pointer — the pair
        // `print_str` writes with, one level up.
        let konst = |b: &mut Frame, p: u32, n: i32| {
            fd(b);
            b.ins(&Instruction::I32Const(p as i32))
                .ins(&Instruction::I32Const(n))
                .ins(&Instruction::Call(write_all))
                .ins(&Instruction::Drop);
        };
        konst(b, at, plen);
        let string = |b: &mut Frame, l: u32| {
            fd(b);
            b.ins(&Instruction::LocalGet(l))
                .ins(&Instruction::LocalGet(l));
            str_len(b);
            b.ins(&Instruction::Call(write_all)).ins(&Instruction::Drop);
        };
        string(b, name);
        konst(b, colon, 2);
        string(b, msg);
        konst(b, nl, 1);
        Ok(())
    }

    /// The expression `jsonSchema(T)` / `schemaOf(T)` stands for.
    ///
    /// Both are compile-time reflection over a *declaration*, so the argument is a
    /// type name rather than a value — which is also why this is a rewrite rather
    /// than a call: there is nothing to evaluate at runtime.
    fn reflected(&self, which: &str, arg: &Expr, line: usize) -> Result<Expr, String> {
        let Expr::Var { name: tn, .. } = arg else {
            return unsupported(
                &format!("`{which}` of something other than a type name"),
                line,
            );
        };
        let Some(decl) = self.cx.types.get(tn) else {
            return unsupported(&format!("`{which}` of the undeclared type `{tn}`"), line);
        };
        Ok(if which == "jsonSchema" {
            Expr::Str(ftypes::json_schema_string(decl, &self.cx.types))
        } else {
            ftypes::schema_struct_lit(decl)
        })
    }

    /// Which `Value` variant `value(x)` builds. The three the interpreter and the
    /// textual emitter box, and nothing else.
    fn value_variant(&mut self, arg: &Expr, line: usize) -> Result<&'static str, String> {
        let t = self.peek(arg, line)?;
        Ok(match self.cx.resolve(&t) {
            Type::Int
            | Type::IntN {
                bits: 64,
                signed: true,
            } => "IntVal",
            Type::Bool => "BoolVal",
            Type::Str => "StrVal",
            // RFC-0094 M3: a type that says how it renders boxes as the String
            // it renders to. The emitting path rewrites the argument into that
            // `show` call, so both halves name one variant.
            _ if self.show_dispatch(&t).is_some() => "StrVal",
            other => return unsupported(&format!("`value` of `{other}`"), line),
        })
    }

    /// The `impl Show for T` a value of type `ty` renders through (RFC-0094 M3),
    /// or `None` where the language renders it itself.
    /// The key is taken from the SUBSTITUTED type, not the written one: inside a
    /// `<T: Show>` specialization the parameter is still spelled `T` here, and
    /// `T` names no impl. Substituting is what selects the impl per instance,
    /// which is what the checker deferred to this point.
    fn show_dispatch(&self, ty: &Type) -> Option<String> {
        let t = self.cx.sub(ty);
        match ftypes::renders(&self.cx.resolve(&t)) {
            true => None,
            false => ftypes::show_impl(&self.cx.impls, &t),
        }
    }

    /// The concrete type of each argument, WITHOUT emitting it.
    ///
    /// A generic call needs these before the first argument is lowered: the
    /// specialization's parameter types are what the arguments get coerced to,
    /// and an aggregate return's destination is a hidden LEADING argument, so
    /// nothing can go on the stack until the substitution is solved. Same bind a
    /// join is in, and the same answer — [`Fn_::peek`] predicts and `expr_as`
    /// re-checks, so a wrong prediction is a compile error rather than a wrong
    /// specialization.
    ///
    /// `declared` is only consulted where `peek` needs a position to type an
    /// argument that has none of its own (an empty array literal, a bare `None`).
    fn arg_types(
        &mut self,
        declared: &[Type],
        args: &[Expr],
        line: usize,
    ) -> Result<Vec<Type>, String> {
        let mut out = Vec::new();
        for (i, a) in args.iter().enumerate() {
            if let Some(d) = declared.get(i) {
                self.expect.push(d.clone());
            }
            let t = self.peek(a, line);
            if declared.get(i).is_some() {
                self.expect.pop();
            }
            out.push(self.cx.sub(&t?));
        }
        Ok(out)
    }

    /// The call itself, once the callee's signature is known.
    fn emit_call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        sig: &Sig,
        args: &[Expr],
        hint: Option<(Dest, Type)>,
    ) -> Result<Type, String> {
        // An aggregate result is written through a hidden leading pointer into a
        // slot of ours, so the destination goes on the stack before the
        // arguments and is pushed again afterwards as the value. RFC-0125 M1:
        // the consumer's own storage when it holds this very type, so the
        // callee's `return` lands there and no slot is taken here.
        let dest = match sig.ret.agg() {
            Some(l) => Some(match hint {
                Some((d, t)) if self.cx.ll(&t) == self.cx.ll(&sig.ret_ty) => (d, true),
                _ => (Dest::Slot(b.alloc(l.size, l.align)), false),
            }),
            None => None,
        };
        if let Some((d, _)) = dest {
            d.addr(b, 0);
        }
        // A `modify` argument is the caller's binding by ADDRESS. Reloads are the
        // one case that needs a fixup after the call: a scalar in a wasm local has
        // no address at all, so it is spilled to a slot for the callee to write
        // through and read back afterwards.
        let mut reload: Vec<(u32, u32, String, bool)> = Vec::new();
        for (i, (a, p)) in args.iter().zip(&sig.params).enumerate() {
            if sig.modify.get(i) != Some(&true) {
                self.expr_as(m, b, a, p)?;
                continue;
            }
            let line = Expr::line(a);
            let Expr::Var { name, .. } = a else {
                return unsupported("a `modify` argument that is not a variable", line);
            };
            let (place, ty) = self.lookup(name, line)?;
            match place {
                Place::Local(l) => {
                    let Repr::Scalar(_) = self.cx.repr(&ty, line)? else {
                        return unsupported("a `modify` argument in a local", line);
                    };
                    let ll = self.cx.ll(&ty);
                    let l2 = layout::of_ll(&ll).map_err(|e| format!("direct backend: {e}"))?;
                    let off = b.alloc(l2.size, l2.align);
                    b.slot(off);
                    b.ins(&Instruction::LocalGet(l));
                    b.ins(&store_of(&ll));
                    b.slot(off);
                    reload.push((off, l, ll, self.cx.signed(&ty)));
                }
                // A frame slot or module state: hand over the address itself, so
                // the callee's copy-out lands in the caller's own storage.
                _ => {
                    place
                        .addr(b, 0)
                        .ok_or_else(|| gap("a `modify` argument with no address", line))?;
                }
            }
        }
        b.ins(&Instruction::Call(sig.index));
        for (off, l, ll, signed) in &reload {
            b.slot(*off);
            b.ins(&load_of(ll, 0, *signed));
            b.ins(&Instruction::LocalSet(*l));
        }
        if let Some((d, used)) = dest {
            d.addr(b, 0);
            self.dest_used = used;
        }
        // The DECLARED return type, not its structural form. Resolving here threw
        // away exactly the information a caller needs to solve a further generic:
        // a `Pair<Int64, Int64>` reduced to its record shape no longer matches
        // `Pair<A, B>`, so `firstOf(twice(41))` could not fix `A`. The textual
        // emitter returns the declared type for the same reason.
        Ok(sig.ret_ty.clone())
    }

    // ---- RFC-0023 higher-order specialization -----------------------------

    /// A call to a function taking one or more `fn`-typed parameters.
    ///
    /// Every function-value argument is resolved to a **target** — a lifted
    /// lambda, a named function, or a forwarded `fn` parameter — with its captures
    /// materialized HERE, at the outer call site, which is RFC-0023's
    /// capture-timing lock. The callee is then specialized per those targets and
    /// called directly.
    ///
    /// Nothing is emitted until the specialization's signature exists, and that is
    /// not fastidiousness: an aggregate return crosses as a hidden LEADING
    /// pointer, so the first thing that goes on the operand stack depends on a
    /// substitution the arguments have to be typed to solve. So the arguments are
    /// PEEKED here and emitted once, by [`Fn_::emit_call`], from a synthesized
    /// argument list — which is also what keeps the aggregate convention,
    /// `modify`, and the M2d coercion seam from having a second implementation to
    /// disagree with.
    fn ho_call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        f: &'p Function,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let generic = !f.type_params.is_empty();
        let mut subst: HashMap<String, Type> = HashMap::new();
        // Pass 1: the ordinary arguments, so a `map<T, U>` lambda sees a concrete
        // `T`. Peek only — see the note above about emission order.
        for (i, p) in f.params.iter().enumerate() {
            if matches!(p.ty, Type::Fn(..)) {
                continue;
            }
            self.expect.push(p.ty.clone());
            let t = self.peek(&args[i], line);
            self.expect.pop();
            let aty = self.cx.sub(&t?);
            if generic {
                crate::solve_param(&p.ty, &aty, &mut subst);
            }
        }
        // Pass 1.5: a type parameter may occur ONLY inside a `fn` parameter's own
        // parameter list (`paramQuery(run: fn(P) -> T)` — RFC-0071 M2b), with no
        // ordinary argument to pin it. Solve those from the target's DECLARED
        // parameters, or `P` survives into the instance as a `Type::Param` — which
        // `llt_of` prints as `void`, i.e. a signature with one fewer parameter
        // rather than a diagnostic. The checker's `check_fn_arg` learned the same
        // rule; this is its codegen half, and the textual backend's too.
        if generic {
            for (i, p) in f.params.iter().enumerate() {
                let Type::Fn(dptys, _) = &p.ty else { continue };
                if let Some(tptys) = self.fn_arg_param_types(&args[i], line) {
                    for (d, t) in dptys.iter().zip(&tptys) {
                        crate::solve_param(d, t, &mut subst);
                    }
                }
            }
        }
        // Pass 2: resolve each `fn`-typed argument to its target, and solve the
        // outbound parameter (`U` in `map<T, U>`) from the target's own return.
        let mut targets: Vec<FnTarget> = Vec::new();
        let mut cap_tys: Vec<Vec<Type>> = Vec::new();
        let mut cap_srcs: Vec<Vec<Expr>> = Vec::new();
        for (i, p) in f.params.iter().enumerate() {
            let Type::Fn(dptys, dret) = &p.ty else {
                continue;
            };
            let ptys: Vec<Type> = dptys
                .iter()
                .map(|t| ftypes::substitute(t, &subst))
                .collect();
            let dret_sub = ftypes::substitute(dret, &subst);
            let (target, srcs, tys) = self.resolve_fn_arg(m, &args[i], &ptys, &dret_sub, line)?;
            if generic {
                crate::solve_param(dret, &target.sig.ret_ty, &mut subst);
            }
            targets.push(target);
            cap_srcs.push(srcs);
            cap_tys.push(tys);
        }
        let mut type_args = Vec::new();
        for tp in &f.type_params {
            match subst.get(tp) {
                Some(t) => type_args.push(t.clone()),
                None => {
                    return unsupported(
                        &format!(
                            "a generic type parameter `{tp}` the call `{}` does not fix",
                            f.name
                        ),
                        line,
                    )
                }
            }
        }
        // The specialization's own signature, and the argument list to call it
        // with, built in ONE walk of the callee's parameters: an ordinary
        // parameter keeps its place, and a `fn` parameter becomes its captures at
        // that same place. A synthesized `Function` rather than a hand-built
        // signature, so `lower_fn` lowers it with no case of its own — the
        // prologue's by-value copy of an aggregate parameter is exactly what a
        // captured record wants.
        //
        // Interleaved rather than ordinary-then-captures, because a wasm argument
        // is evaluated where its operand is pushed, and a `fn`-typed argument can
        // be an expression that prints or traps (a stored value read from a
        // place). Collecting the captures at the end evaluated that expression
        // after every ordinary argument, which the other two engines do not do.
        let mut sf = shell_of(f);
        let mut params: Vec<Param> = Vec::new();
        let mut call_args: Vec<Expr> = Vec::new();
        let mut binds: HashMap<String, FnBinding> = HashMap::new();
        let mut fi = 0usize;
        let mark = self.cx.plan.alias_scope();
        // RFC-0114 §26: `call_args` holds CLONES of the caller's argument
        // expressions, so plan rows on the originals would go undischarged —
        // each element remembers its source's node addresses, and the pairs
        // are registered once the vector stops growing (elements only sit
        // still after the last push).
        let mut src_lists: Vec<Vec<usize>> = Vec::new();
        for (i, p) in f.params.iter().enumerate() {
            if !matches!(p.ty, Type::Fn(..)) {
                params.push(Param {
                    name: p.name.clone(),
                    capability: p.capability,
                    ty: ftypes::substitute(&p.ty, &subst),
                });
                call_args.push(args[i].clone());
                let mut v = Vec::new();
                vyrn_frontend::ast::node_addrs_val(&args[i], &mut v);
                src_lists.push(v);
                continue;
            }
            let mut srcs = Vec::new();
            for t in &cap_tys[fi] {
                // A reserved spelling: no Vyrn identifier can contain `@`, so an
                // instance's capture parameter cannot shadow or be shadowed by
                // anything the callee's body names.
                let n = format!("@cap{}", params.len());
                params.push(Param {
                    name: n.clone(),
                    capability: Capability::Read,
                    ty: t.clone(),
                });
                srcs.push(n);
            }
            binds.insert(
                p.name.clone(),
                FnBinding {
                    target: targets[fi].clone(),
                    cap_srcs: srcs,
                },
            );
            // The capture values, read from the caller's own scope — which is
            // what fixes them at this site.
            for src in &cap_srcs[fi] {
                call_args.push(src.clone());
                let mut v = Vec::new();
                vyrn_frontend::ast::node_addrs_val(src, &mut v);
                src_lists.push(v);
            }
            fi += 1;
        }
        {
            let mut pairs: Vec<(usize, usize)> = Vec::new();
            for (e, srcs) in call_args.iter().zip(&src_lists) {
                let mut c = Vec::new();
                vyrn_frontend::ast::node_addrs_val(e, &mut c);
                pairs.extend(c.into_iter().zip(srcs.iter().copied()));
            }
            self.cx.plan.alias_clones_scoped(&pairs);
        }
        sf.params = params;
        sf.ret = ftypes::substitute(&f.ret, &subst);
        let sig = self.cx.enqueue(
            m,
            Key::Ho(f.name.clone(), type_args, targets),
            Rc::new(sf),
            Body::Block(&f.body),
            subst,
            binds,
        )?;
        let r = self.emit_call(m, b, &sig, &call_args, None);
        // The clones die with this frame; their aliases must die first, or a
        // later node at a recycled address would resolve to somebody's row.
        self.cx.plan.alias_unwind(mark);
        r
    }

    /// A call through a `fn`-typed parameter: the target, with this instance's
    /// capture parameters prepended to the argument list.
    ///
    /// The prepend is why there is one call path: a target's signature is
    /// captures-then-parameters, so `emit_call` sees an ordinary call to an
    /// ordinary function and every convention it already implements applies. The
    /// arguments coerce into the TARGET's declared parameter types, not the `fn`
    /// type's — a named target declaring `Age` where the signature says `Int64`
    /// re-validates, which is what the textual backend's dispatcher does at the
    /// same boundary.
    fn target_call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        bnd: &FnBinding,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let mut all: Vec<Expr> = bnd
            .cap_srcs
            .iter()
            .map(|s| Expr::Var {
                name: s.clone(),
                line,
            })
            .collect();
        all.extend(args.iter().cloned());
        if all.len() != bnd.target.sig.params.len() {
            return unsupported("a call through a `fn` parameter at another arity", line);
        }
        // RFC-0114 §26: the tail is a clone of the caller's arguments — pair
        // it with the originals so their plan rows discharge (the capture
        // reads ahead of it are synthesized and carry no rows).
        let mark = self.cx.plan.alias_scope();
        {
            let mut pairs: Vec<(usize, usize)> = Vec::new();
            for (e, src) in all[bnd.cap_srcs.len()..].iter().zip(args) {
                let (mut c, mut o) = (Vec::new(), Vec::new());
                vyrn_frontend::ast::node_addrs_val(e, &mut c);
                vyrn_frontend::ast::node_addrs_val(src, &mut o);
                pairs.extend(c.into_iter().zip(o));
            }
            self.cx.plan.alias_clones_scoped(&pairs);
        }
        let r = self.emit_call(m, b, &bnd.target.sig, &all, None);
        self.cx.plan.alias_unwind(mark);
        r
    }

    /// Resolve one `fn`-typed argument to a call target, giving the EXPRESSIONS to
    /// read its capture values from at THIS site and their types.
    ///
    /// Nothing is emitted: the captures are named, not loaded, because the call
    /// they become arguments to has not started pushing operands yet.
    fn resolve_fn_arg(
        &mut self,
        m: &mut Module,
        arg: &Expr,
        ptys: &[Type],
        expected_ret: &Type,
        line: usize,
    ) -> Result<(FnTarget, Vec<Expr>, Vec<Type>), String> {
        let var = |n: &String| Expr::Var {
            name: n.clone(),
            line,
        };
        match arg {
            Expr::Lambda { params, body, line } => {
                self.lift_lambda(m, arg, params, body, ptys, expected_ret, *line)
            }
            Expr::Var { name, .. } => {
                // A pass-through `fn`-typed parameter: forward the target AND the
                // captures, which are this instance's own capture parameters. The
                // monomorphization threads through transitively and the inner
                // instance never learns there was an outer one.
                if let Some(bnd) = self.fn_binds.get(name) {
                    let tys = bnd.target.sig.params[..bnd.target.ncaps].to_vec();
                    let srcs = bnd.cap_srcs.iter().map(&var).collect();
                    return Ok((bnd.target.clone(), srcs, tys));
                }
                // A stored function value (RFC-0037) flowing into a `fn`-typed
                // parameter: the target is the signature's DISPATCHER and the
                // "capture" is the enum itself, which is why this needs no third
                // mechanism — a dispatcher is a target with one capture, and the
                // specialization dispatches internally. v1's zero-cost path for a
                // direct lambda or named argument is untouched; those never reach
                // this arm.
                if let Ok((_, ty)) = self.lookup(name, line) {
                    let norm = crate::normalize_fn_sig(&self.cx.sub(&ty), &self.cx.types);
                    if matches!(norm, Type::Fn(..)) {
                        let dsig = self.dispatcher(m, &norm, line)?;
                        return Ok((
                            FnTarget {
                                sig: dsig,
                                ncaps: 1,
                            },
                            vec![var(name)],
                            vec![norm],
                        ));
                    }
                    return unsupported(&format!("`{name}` as a function value"), line);
                }
                // A named top-level function: called directly, no captures. A
                // GENERIC or itself higher-order target is refused — the first has
                // no index until something fixes its type arguments, and the second
                // has no first-order definition at all.
                match self.cx.sigs.get(name) {
                    Some(sig) if sig.modify.iter().any(|m| *m) => unsupported(
                        &format!("`{name}` as a function value (it takes a `modify` parameter)"),
                        line,
                    ),
                    Some(sig) => Ok((
                        FnTarget {
                            sig: sig.clone(),
                            ncaps: 0,
                        },
                        Vec::new(),
                        Vec::new(),
                    )),
                    None => unsupported(&format!("`{name}` as a function value"), line),
                }
            }
            // Any other expression of `fn` type (RFC-0037): a field read, an
            // element, a call's result. It produces the same defunctionalized
            // value a binding holds, so it takes the arm above — the target is
            // the signature's dispatcher and the "capture" is the value itself,
            // read at this site by the expression rather than by a name.
            other => {
                let ty = self.peek(other, line)?;
                let norm = crate::normalize_fn_sig(&self.cx.sub(&ty), &self.cx.types);
                if !matches!(norm, Type::Fn(..)) {
                    return unsupported(
                        &format!("a `fn`-typed argument that is {}", expr_name(other)),
                        Expr::line(other),
                    );
                }
                let dsig = self.dispatcher(m, &norm, line)?;
                // RFC-0114 §26: the capture source is a clone of the
                // argument expression — pair it with the original so its
                // plan rows discharge. A `Vec` never pushed again keeps its
                // buffer, so the element's addresses are already final.
                let srcs = vec![other.clone()];
                {
                    let (mut c, mut o) = (Vec::new(), Vec::new());
                    vyrn_frontend::ast::node_addrs_val(&srcs[0], &mut c);
                    vyrn_frontend::ast::node_addrs_val(other, &mut o);
                    let pairs: Vec<(usize, usize)> = c.into_iter().zip(o).collect();
                    // Scoped: the vector lives until the enclosing `ho_call`
                    // returns, whose unwind removes these with its own.
                    self.cx.plan.alias_clones_scoped(&pairs);
                }
                Ok((
                    FnTarget {
                        sig: dsig,
                        ncaps: 1,
                    },
                    srcs,
                    vec![norm],
                ))
            }
        }
    }

    /// What a lambda literal returns: concrete when the `fn` type named it, and
    /// otherwise what the body produces — which is the outbound `U` a generic
    /// higher-order call solves from. A block body carries no expression to peek, so
    /// it is `Unit`, the textual backend's rule and the same one.
    ///
    /// One function because both the lifting path and [`Fn_::peek`] need the answer
    /// and a `peek` that guessed differently would size a join's destination for a
    /// type the arm does not produce.
    fn lambda_ret(
        &mut self,
        params: &[String],
        body: &LambdaBody,
        ptys: &[Type],
        expected_ret: &Type,
        line: usize,
    ) -> Result<Type, String> {
        Ok(match (expected_ret, body) {
            (Type::Param(_), LambdaBody::Expr(e)) => {
                let mark = self.scope.len();
                for (pn, pt) in params.iter().zip(ptys) {
                    self.scope
                        .push((pn.clone(), Place::Local(u32::MAX), pt.clone()));
                }
                let got = self.peek(e, line);
                self.scope.truncate(mark);
                self.cx.sub(&got?)
            }
            (Type::Param(_), LambdaBody::Block(_)) => Type::Unit,
            (t, _) => t.clone(),
        })
    }

    /// The DECLARED parameter types of a `fn`-typed argument's target, when the
    /// argument names one or is an expression of `fn` type. `None` for a lambda
    /// literal, whose parameters take their types from the signature they flow
    /// into and so can solve nothing.
    fn fn_arg_param_types(&mut self, arg: &Expr, line: usize) -> Option<Vec<Type>> {
        let Expr::Var { name, .. } = arg else {
            return match self.fn_expr_sig(arg, line) {
                Ok(Type::Fn(ptys, _)) => Some(ptys),
                _ => None,
            };
        };
        if let Some(bnd) = self.fn_binds.get(name) {
            return Some(bnd.target.sig.params[bnd.target.ncaps..].to_vec());
        }
        if let Ok((_, ty)) = self.lookup(name, 0) {
            return match self.cx.resolve(&ty) {
                Type::Fn(ptys, _) => Some(ptys),
                _ => None,
            };
        }
        self.cx.sigs.get(name).map(|s| s.params.clone())
    }

    /// The normalized `fn` signature an expression produces — a lambda literal
    /// excepted, since it has none of its own. Peeked, so nothing is emitted.
    fn fn_expr_sig(&mut self, arg: &Expr, line: usize) -> Result<Type, String> {
        if matches!(arg, Expr::Lambda { .. }) {
            return Ok(Type::Unit);
        }
        let ty = self.peek(arg, line)?;
        Ok(crate::normalize_fn_sig(&self.cx.sub(&ty), &self.cx.types))
    }

    /// Lift a lambda literal to a top-level function: `(captures.., params..) ->
    /// ret`, discovered and indexed here, its body lowered when the queue reaches
    /// it.
    ///
    /// A synthesized `Function` rather than a bespoke lowering, so the captures are
    /// ordinary read parameters — which is exactly the by-value snapshot RFC-0023
    /// specifies, since `lower_fn`'s prologue already copies an aggregate parameter
    /// into a slot of its own.
    #[allow(clippy::too_many_arguments)]
    fn lift_lambda(
        &mut self,
        m: &mut Module,
        at: &Expr,
        params: &[String],
        body: &LambdaBody,
        ptys: &[Type],
        expected_ret: &Type,
        line: usize,
    ) -> Result<(FnTarget, Vec<Expr>, Vec<Type>), String> {
        if params.len() != ptys.len() {
            return unsupported("a lambda with the wrong number of parameters", line);
        }
        // The free locals, in first-seen order — the SHARED walk (`lib.rs`),
        // because a capture list is part of the lifted function's signature and two
        // backends disagreeing about its length would emit calls with the wrong
        // number of arguments.
        let cap_names = crate::lambda_captures(body, params.iter().cloned().collect(), &|n| {
            self.scope.iter().any(|(s, _, _)| s == n) || self.fn_binds.contains_key(n)
        });
        let mut cap_tys = Vec::new();
        for cn in &cap_names {
            // A `fn`-typed PARAMETER captured by the lambda has no slot: inside a
            // specialization it lives in `fn_binds`. It is captured as its own
            // `fn` TYPE, so the lifted function takes an ordinary function value
            // and calls it through the dispatcher — the site reads it with
            // `fnval_binding`, which is the same aggregate storing it anywhere
            // else builds. Without this the name fell through to a direct call to
            // a symbol no module defines.
            if let Some(bnd) = self.fn_binds.get(cn) {
                let t = &bnd.target;
                cap_tys.push(crate::normalize_fn_sig(
                    &Type::Fn(
                        t.sig.params[t.ncaps..].to_vec(),
                        Box::new(t.sig.ret_ty.clone()),
                    ),
                    &self.cx.types,
                ));
                continue;
            }
            let (_, t) = self.lookup(cn, line)?;
            cap_tys.push(self.cx.sub(&t));
        }
        let ret = self.lambda_ret(params, body, ptys, expected_ret, line)?;
        // The literal's OWN nodes, which is the whole of RFC-0101 M6's third
        // phase. This used to be `b.clone()` and `(**e).clone()`: a deep copy of
        // the body, made so the worklist had something to hold, and every answer
        // given while walking one was about a node the program does not have.
        // The textual emitter never copied — `emit_lifted_lambda` walks the
        // literal's own `LambdaBody`, and capture prepending is a fact about the
        // SIGNATURE, which is `sf.params` below either way.
        //
        // A literal the program does not hold keeps the copy, and the shell
        // carries it exactly as before.
        let queued = match self.cx.lambda(at) {
            Some(LambdaBody::Block(b)) => Body::Block(b),
            Some(LambdaBody::Expr(e)) => Body::Value(e),
            None => Body::Shell,
        };
        let mut sf = f_shell(line);
        sf.name = format!("{LAMBDA} {}", self.owner);
        sf.params = cap_names
            .iter()
            .zip(&cap_tys)
            .map(|(n, t)| Param {
                name: n.clone(),
                capability: Capability::Read,
                ty: t.clone(),
            })
            .chain(params.iter().zip(ptys).map(|(n, t)| Param {
                name: n.clone(),
                capability: Capability::Read,
                ty: t.clone(),
            }))
            .collect();
        sf.ret = ret.clone();
        if let Body::Shell = queued {
            // `LambdaBody::Expr` is a `return` of that expression — the same
            // thing the block form writes by hand — and a Unit-returning
            // signature makes it a statement instead. [`Fn_::lambda_value`] is
            // the same split, made where the body is lowered rather than by
            // building a statement to hold the copy.
            sf.body = match body {
                LambdaBody::Block(b) => b.clone(),
                LambdaBody::Expr(e) if self.cx.repr(&ret, line)? == Repr::Unit => Block {
                    stmts: vec![Stmt::Expr((**e).clone())],
                },
                LambdaBody::Expr(e) => Block {
                    stmts: vec![Stmt::Return {
                        value: Some((**e).clone()),
                        line,
                    }],
                },
            };
        }
        // RFC-0114 §26: a SHELL's body is the one clone left (RFC-0101 M6),
        // so plan rows inside the literal would go undischarged — the same
        // hole the user-container `for` had, cured the same way: pair the
        // clone's nodes with the source's and let every plan query resolve
        // through them. Statement addresses live in the Vec's buffer and
        // expression nodes behind boxes, so the pairs survive the move into
        // the queue's `Rc`.
        if let Body::Shell = queued {
            let (mut orig, mut clone) = (Vec::new(), Vec::new());
            match body {
                LambdaBody::Block(src) => {
                    vyrn_frontend::ast::node_addrs(src, &mut orig);
                    vyrn_frontend::ast::node_addrs(&sf.body, &mut clone);
                }
                LambdaBody::Expr(src) => {
                    vyrn_frontend::ast::node_addrs_val(src, &mut orig);
                    match sf.body.stmts.first() {
                        Some(Stmt::Expr(e)) | Some(Stmt::Return { value: Some(e), .. }) => {
                            vyrn_frontend::ast::node_addrs_val(e, &mut clone)
                        }
                        _ => {}
                    }
                }
            }
            let pairs: Vec<(usize, usize)> = clone.into_iter().zip(orig).collect();
            self.cx.plan.alias_clones(&pairs);
        }
        // The key: the literal's node address, the concrete shape, AND the
        // substitution the body is under. One literal inside a generic body lifts a
        // distinct copy per instantiation, and the shape alone does not say so when
        // the type parameter appears only in a statement.
        let mut shape: Vec<Type> = cap_tys.clone();
        shape.extend(ptys.iter().cloned());
        shape.push(ret);
        let mut under: Vec<(String, Type)> = self
            .cx
            .subst
            .iter()
            .map(|(k, v)| (k.clone(), v.clone()))
            .collect();
        under.sort_by(|a, b| a.0.cmp(&b.0));
        let key = Key::Lambda(at as *const Expr as usize, shape, under);
        let sig = self.cx.enqueue(
            m,
            key,
            Rc::new(sf),
            queued,
            self.cx.subst.clone(),
            HashMap::new(),
        )?;
        let srcs = cap_names
            .iter()
            .map(|n| Expr::Var {
                name: n.clone(),
                line,
            })
            .collect();
        Ok((
            FnTarget {
                sig,
                ncaps: cap_names.len(),
            },
            srcs,
            cap_tys,
        ))
    }

    // ---- RFC-0037 stored function values ----------------------------------

    /// The `fn` type a value is being built FOR, normalized. `None` when the
    /// position does not name one, which is what makes a bare `let f = double`
    /// take the function's own signature instead.
    fn expected_fn_sig(&self) -> Option<Type> {
        let top = self.expect.last()?;
        match crate::normalize_fn_sig(&self.cx.sub(top), &self.cx.types) {
            t @ Type::Fn(..) => Some(t),
            _ => None,
        }
    }

    /// Register a variant (deduped on signature + target) and give its tag.
    ///
    /// The tag is an index into the MODULE-GLOBAL list, matching the textual
    /// backend: a tag has to mean the same thing in every body that builds one,
    /// and the dispatcher filters by signature rather than renumbering.
    fn register_fnval(&self, sig: &Type, target: &FnTarget) -> i64 {
        let mut v = self.cx.fnvals.borrow_mut();
        if let Some(i) = v.iter().position(|x| x.sig == *sig && x.target == *target) {
            return i as i64;
        }
        v.push(FnVal {
            sig: sig.clone(),
            target: target.clone(),
        });
        (v.len() - 1) as i64
    }

    /// The LLVM shape of a capture block: the captures packed by value, in order.
    fn cap_block(&self, cap_tys: &[Type]) -> Result<Layout, String> {
        let ll = format!(
            "{{ {} }}",
            cap_tys
                .iter()
                .map(|t| self.cx.ll(t))
                .collect::<Vec<_>>()
                .join(", ")
        );
        layout::of_ll(&ll).map_err(|e| format!("direct backend: {e}"))
    }

    /// Build a stored function value: `{ i64 tag, i64 payload }` in a frame slot.
    ///
    /// The payload is 0 when there are no captures, and otherwise a heap block
    /// holding them BY VALUE — read here, at the construction site, which is
    /// RFC-0023's capture-timing lock applied to a value that outlives its scope.
    /// The block is never freed, the same safe leak every boxed enum payload
    /// already is.
    fn build_fnval(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        sig_ty: &Type,
        target: FnTarget,
        cap_srcs: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let cap_tys = target.sig.params[..target.ncaps].to_vec();
        if cap_tys.len() != cap_srcs.len() {
            return unsupported(
                "a function value whose captures do not match its target",
                line,
            );
        }
        let tag = self.register_fnval(sig_ty, &target);
        let Repr::Agg(l) = self.cx.repr(sig_ty, line)? else {
            return unsupported("a function value that is not an aggregate", line);
        };
        let off = b.alloc(l.size, l.align);
        // The payload first, because building the block needs scratch the tag
        // store would otherwise be sitting on top of.
        let payload = if cap_tys.is_empty() {
            None
        } else {
            let bl = self.cap_block(&cap_tys)?;
            let p = b.local(ValType::I32);
            b.ins(&Instruction::I64Const(bl.size as i64));
            b.ins(&Instruction::Call(self.cx.rt.malloc));
            b.ins(&Instruction::LocalSet(p));
            for (i, (src, ty)) in cap_srcs.iter().zip(&cap_tys).enumerate() {
                b.ins(&Instruction::LocalGet(p));
                if bl.fields[i] != 0 {
                    b.ins(&Instruction::I32Const(bl.fields[i] as i32));
                    b.ins(&Instruction::I32Add);
                }
                self.expr_as(m, b, src, ty)?;
                // The snapshot OWNS its heap (RFC-0114 §25 round three, the
                // textual backend's rule, mirrored here in round fifty-seven):
                // a heap capture is DUPLICATED into the block, which is what
                // lets the release twin walk it and the captured binding keep
                // releasing its own value at block exit.
                match self.cx.repr(ty, line)? {
                    Repr::Scalar(_) => {
                        if self.owns_heap(ty) {
                            self.copy_stack(m, b, ty, line)?;
                        }
                        b.ins(&store_of(&self.cx.ll(ty)));
                    }
                    Repr::Agg(fl) => {
                        b.ins(&Instruction::I32Const(fl.size as i32));
                        b.ins(&Instruction::MemoryCopy {
                            src_mem: 0,
                            dst_mem: 0,
                        });
                        if self.owns_heap(ty) {
                            let a = b.local(ValType::I32);
                            b.ins(&Instruction::LocalGet(p));
                            if bl.fields[i] != 0 {
                                b.ins(&Instruction::I32Const(bl.fields[i] as i32));
                                b.ins(&Instruction::I32Add);
                            }
                            b.ins(&Instruction::LocalSet(a));
                            self.copy_at(m, b, a, ty, line)?;
                        }
                    }
                    Repr::Unit => return unsupported("a captured Unit value", line),
                }
            }
            Some(p)
        };
        b.slot(off + l.fields[0]);
        b.ins(&Instruction::I64Const(tag));
        b.ins(&Instruction::I64Store(word8()));
        b.slot(off + l.fields[1]);
        match payload {
            Some(p) => {
                b.ins(&Instruction::LocalGet(p));
                b.ins(&Instruction::I64ExtendI32U);
            }
            None => {
                b.ins(&Instruction::I64Const(0));
            }
        }
        b.ins(&Instruction::I64Store(word8()));
        b.slot(off);
        Ok(sig_ty.clone())
    }

    /// A stored value from a lambda literal: lift the body through the SAME
    /// [`Fn_::lift_lambda`] the RFC-0023 argument path uses, typed exactly by the
    /// slot's signature. One lifting rule, so storing a lambda and passing one
    /// cannot disagree about its captures or its parameter types.
    fn fnval_lambda(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        e: &Expr,
        sig_ty: &Type,
    ) -> Result<Type, String> {
        let Expr::Lambda { params, body, line } = e else {
            return unsupported("a function value from a non-lambda", Expr::line(e));
        };
        let Type::Fn(ptys, ret) = sig_ty else {
            return unsupported("a lambda in a non-function position", *line);
        };
        // The expected-type stack must not leak into the lifted body: its own
        // storage boundaries push their own types.
        let saved = std::mem::take(&mut self.expect);
        let r = self.lift_lambda(m, e, params, body, ptys, ret, *line);
        self.expect = saved;
        let (target, srcs, _) = r?;
        self.build_fnval(m, b, sig_ty, target, &srcs, *line)
    }

    /// A stored value from a bare function name: the empty-payload variant. The
    /// signature is the slot's when a position names one, else the function's own.
    fn fnval_named(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        line: usize,
    ) -> Result<Type, String> {
        let Some(sig) = self.cx.sigs.get(name).cloned() else {
            return unsupported(&format!("`{name}` as a function value"), line);
        };
        if sig.modify.iter().any(|x| *x) {
            return unsupported(
                &format!("`{name}` as a function value (it takes a `modify` parameter)"),
                line,
            );
        }
        let own = crate::normalize_fn_sig(
            &Type::Fn(sig.params.clone(), Box::new(sig.ret_ty.clone())),
            &self.cx.types,
        );
        let sig_ty = self.expected_fn_sig().unwrap_or(own);
        self.build_fnval(m, b, &sig_ty, FnTarget { sig, ncaps: 0 }, &[], line)
    }

    /// A stored value from a `fn`-typed PARAMETER (RFC-0037 × RFC-0023): inside a
    /// specialization the parameter's target and captures are both statically
    /// known, so storing it materializes exactly the aggregate a lambda or named
    /// source builds. Storing a `fn` parameter therefore behaves exactly as calling
    /// one — for any signature, scalar or aggregate captures alike.
    fn fnval_binding(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        bnd: &FnBinding,
        line: usize,
    ) -> Result<Type, String> {
        let t = &bnd.target;
        let own = crate::normalize_fn_sig(
            &Type::Fn(
                t.sig.params[t.ncaps..].to_vec(),
                Box::new(t.sig.ret_ty.clone()),
            ),
            &self.cx.types,
        );
        let sig_ty = self.expected_fn_sig().unwrap_or(own);
        let srcs: Vec<Expr> = bnd
            .cap_srcs
            .iter()
            .map(|n| Expr::Var {
                name: n.clone(),
                line,
            })
            .collect();
        self.build_fnval(m, b, &sig_ty, t.clone(), &srcs, line)
    }

    /// The dispatcher for one signature, reserving its index the first time
    /// anything calls through a value of that signature.
    ///
    /// Its Vyrn-level shape is `fn(fv: <sig>, a0: P0, ..) -> R`, i.e. the fn value
    /// prepended to the signature's own parameters — which is exactly
    /// captures-then-parameters with one capture. So a dispatcher IS a [`FnTarget`]
    /// with `ncaps == 1`, and a stored value flowing into a `fn`-typed parameter
    /// needs no third mechanism: the "capture" the RFC-0023 instance receives is
    /// the enum itself.
    fn dispatcher(&mut self, m: &mut Module, sig_ty: &Type, line: usize) -> Result<Sig, String> {
        if let Some((_, s)) = self
            .cx
            .dispatch
            .borrow()
            .sigs
            .iter()
            .find(|(t, _)| t == sig_ty)
        {
            return Ok(s.clone());
        }
        let Type::Fn(ptys, ret) = sig_ty else {
            return unsupported("a dispatcher for a non-function type", line);
        };
        let mut params = vec![sig_ty.clone()];
        params.extend(ptys.iter().cloned());
        let s = Sig {
            index: 0,
            modify: vec![false; params.len()],
            params,
            ret: self.cx.repr(ret, line)?,
            ret_ty: (**ret).clone(),
        };
        let (wp, wr) = self.cx.wasm_sig(&s, line)?;
        let sig = Sig {
            index: m.reserve_func(&wp, &wr),
            ..s
        };
        self.cx
            .dispatch
            .borrow_mut()
            .sigs
            .push((sig_ty.clone(), sig.clone()));
        Ok(sig)
    }

    /// A call through a stored function value: ONE direct call to the signature's
    /// dispatcher, with the value as its leading argument. The switch and the
    /// direct calls live inside it, so the RFC-0037 invariant holds verbatim — no
    /// function pointer exists anywhere in the module.
    fn fnval_call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        recv: &Expr,
        sig_ty: &Type,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let Type::Fn(ptys, _) = sig_ty else {
            return unsupported("a call through a non-function value", line);
        };
        if args.len() != ptys.len() {
            return unsupported("a call through a stored `fn` value at another arity", line);
        }
        let dsig = self.dispatcher(m, sig_ty, line)?;
        let mut all = vec![recv.clone()];
        all.extend(args.iter().cloned());
        self.emit_call(m, b, &dsig, &all, None)
    }

    /// `.length` / `.byteLength`, neither of which is a field: the receiver's
    /// value is on the stack already, so each has to consume it — including the
    /// fixed array, whose length is a constant and whose address is therefore
    /// dropped.
    fn length_of(
        &mut self,
        b: &mut Frame,
        base: &Type,
        field: &str,
        line: usize,
    ) -> Result<Option<Type>, String> {
        // [`length_ty`] decides, here and in `peek`. The two paths kept the list
        // by hand and it drifted, so the emitting path now asks the same table
        // the predicting one does.
        if length_ty(field, &self.cx.resolve(base)).is_none() {
            return Ok(None);
        }
        match (field, self.cx.resolve(base)) {
            // One load off the String header, not a scan (RFC-0089 M1a).
            ("byteLength", Type::Str) => {
                str_len(b);
                b.ins(&Instruction::I64ExtendI32U);
            }
            ("length", Type::Array(_)) => {
                let l = self.layout_of(base, line)?;
                b.ins(&Instruction::I64Load(at(l.fields[1])));
            }
            ("length", Type::ArrayN(_, n)) => {
                b.ins(&Instruction::Drop);
                b.ins(&Instruction::I64Const(n as i64));
            }
            // A `SmallArray` keeps its length in field 0 (RFC-0056) — the one that
            // would have been read as a data pointer.
            ("length", Type::SmallArray(..)) => {
                let l = self.layout_of(base, line)?;
                b.ins(&Instruction::I64Load(at(l.fields[0])));
            }
            // `m.length` is the shared length of a Map's two parallel buffers,
            // field 2 — not field 1, which is where an Array keeps its own.
            ("length", Type::Map(..)) => {
                let l = self.layout_of(base, line)?;
                b.ins(&Instruction::I64Load(at(l.fields[2])));
            }
            _ => return Ok(None),
        }
        Ok(Some(Type::Int))
    }

    /// `spawn f(args)` (RFC-0025) — and the whole reason this backend needs no
    /// function table after all.
    ///
    /// M2a's pre-flight measured nine function-addresses-as-values over the
    /// corpus and concluded "there IS a function table, and it is `spawn`". All
    /// nine are the *textual* emitter's `call @__vyrn_spawn(ptr @__vyrn_task_*,
    /// ptr)`, and the half of that finding which is about wasm is wrong. Read what
    /// the retired C shim did with the pointer on this target: wasm has no
    /// threads, so its `__vyrn_spawn` called
    /// `thunk(frame)` **inline** and returns a `VTask` holding the frame. The
    /// pointer is formed and consumed in one C statement, and it exists only
    /// because the LLVM path routes an eager call through a C function that cannot
    /// know the callee.
    ///
    /// A spawn site names its callee statically, so emitting that eager path here
    /// forms no pointer at all: no table, no element segment, no `ref.func`, no
    /// `call_indirect`. `spawn f(a)` IS `f(a)`, at the spawn point, in argument
    /// order — which is also literally what the interpreter does (`interp.rs`,
    /// `Expr::Spawn`), so all three engines run one schedule.
    ///
    /// What survives of the machinery is the **frame**: a `Task<T>` outlives the
    /// shadow-stack frame that made it, so the result is boxed on the heap and
    /// the `Task` is that address — the shim's `VTask { frame }` minus the thunk
    /// field it no longer needs. Since RFC-0095 M1 the box is freed by whichever
    /// construct discharges the task, `t.join()` or `drop t`, because a task is
    /// linear and there is exactly one of them.
    ///
    /// Isolation is NOT enforced here, and must not be: the checker proves it
    /// transitively (`checker.rs`, `spawn_safe`) for every engine, so a second
    /// opinion in one backend would be a rule free to disagree with itself.
    /// `__vyrn_join_all` has nothing to do either — eager means every spawned task
    /// has already run by the time `main` returns, which is why the shim's wasm
    /// arm defines it empty.
    fn spawn(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        // A spawn callee is always a user function — the checker resolves the name
        // in its own `sigs` and admits nothing else. Guarded before routing
        // through [`Fn_::call`], which matches builtin spellings first: without
        // this, a user function named like a builtin would spawn the builtin while
        // the textual backend (whose `prep_spawn_target` looks only at `funcs`)
        // spawned the function.
        if !self.cx.sigs.contains_key(name) && !self.cx.generics.contains_key(name) {
            return unsupported(
                &format!("`spawn {name}(..)` of something not a function"),
                line,
            );
        }
        // Everything a call needs — argument coercion, generic instantiation, the
        // hidden destination for an aggregate return — is `call`'s, so a spawned
        // call and a plain one cannot diverge in how they pass arguments.
        let ret = self.call(m, b, name, args, line)?;
        let boxed = b.local(ValType::I32);
        match self.cx.repr(&ret, line)? {
            Repr::Scalar(v) => {
                let l = self.layout_of(&ret, line)?;
                let held = b.local(v);
                b.ins(&Instruction::LocalSet(held));
                b.ins(&Instruction::I64Const(l.size as i64));
                b.ins(&Instruction::Call(self.cx.rt.malloc));
                b.ins(&Instruction::LocalTee(boxed));
                b.ins(&Instruction::LocalGet(held));
                b.ins(&store_of(&self.cx.ll(&ret)));
            }
            Repr::Agg(l) => {
                let src = b.local(ValType::I32);
                b.ins(&Instruction::LocalSet(src));
                b.ins(&Instruction::I64Const(l.size as i64));
                b.ins(&Instruction::Call(self.cx.rt.malloc));
                b.ins(&Instruction::LocalTee(boxed));
                b.ins(&Instruction::LocalGet(src));
                b.ins(&Instruction::I32Const(l.size as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }
            // A `Task<Unit>` still has to be a value: one word, so `join` has a
            // pointer to drop rather than a `Task` with no representation.
            Repr::Unit => {
                b.ins(&Instruction::I64Const(8));
                b.ins(&Instruction::Call(self.cx.rt.malloc));
                b.ins(&Instruction::LocalSet(boxed));
            }
        }
        b.ins(&Instruction::LocalGet(boxed));
        Ok(Type::Task(Box::new(ret)))
    }
}

// ---------------------------------------------------------------------------
// `Array<T>`, `Array<T, N>`, and walking either of them (RFC-0077 M2c)
// ---------------------------------------------------------------------------

/// What an indexable value is made of, once its parts are in locals: where its
/// elements start, how many there are, and what one is.
///
/// The parts are SNAPSHOTTED — the same thing the LLVM backend does by taking
/// them out of an SSA aggregate, and the reason a `for` that grows its own array
/// keeps walking the buffer it started on rather than following a `realloc` to a
/// new one. Both backends agree with the interpreter, which iterates a copy.
#[derive(Clone)]
struct Walk {
    /// `i32` local: the address of element 0.
    data: u32,
    /// `i64` local: the element count.
    len: u32,
    elem: Type,
    stride: u32,
    /// A `String`'s elements are bytes widened to `Int`, not stored values —
    /// which is what the LLVM backend's `for` over a String produces too.
    byte: bool,
}

impl<'p> Fn_<'_, 'p> {
    fn layout_of(&self, ty: &Type, line: usize) -> Result<Layout, String> {
        layout::of_ll(&self.cx.ll(ty))
            .map_err(|e| gap(&format!("the layout of `{ty}` ({e})"), line))
    }

    /// The distance between consecutive elements. `of_ll` already rounds a
    /// shape's size up to its own alignment, so a size IS a stride.
    fn stride(&self, elem: &Type, line: usize) -> Result<u32, String> {
        Ok(self.layout_of(elem, line)?.size)
    }

    /// `n` elements of `elem`, in bytes — every allocation size and every
    /// `memory.copy` length in this file that is a count times a stride.
    ///
    /// Checked against `i32::MAX` rather than `u32::MAX`, and the difference is
    /// the whole defect. Every consumer of this number is an `i32`: a
    /// `memory.copy` length, a frame offset, a `malloc` argument. A product in
    /// `[2^31, 2^32)` does not wrap — it goes NEGATIVE, and the consumers then
    /// disagree about what it means. `malloc` is handed `bytes.max(1)`, so it
    /// returns a one-byte block; `memory.copy` reads the same bits as an
    /// unsigned length and copies two billion bytes over the heap behind it.
    /// That is corruption rather than a trap, which is the worst answer
    /// available. Nothing is lost by the tighter bound: a frame this big is
    /// already past `FRAME_LIMIT` by five orders of magnitude.
    ///
    /// [`layout::of_ll`] bounds ONE shape; this bounds a count times one.
    fn extent(&self, elem: &Type, n: usize, line: usize) -> Result<u32, String> {
        let bytes = self.stride(elem, line)? as u64 * n as u64;
        if bytes > i32::MAX as u64 {
            return Err(too_big(&format!("{n} × `{elem}`"), bytes, line));
        }
        Ok(bytes as u32)
    }

    // ---- RFC-0075 M2b: `Stream<T>` as a producer ---------------------------

    /// The six-word header, whatever the element type — the layout is a function
    /// of the SHAPE, and every stream shares it.
    fn stream_layout(&self, line: usize) -> Result<Layout, String> {
        self.layout_of(&Type::Stream(Box::new(Type::Int)), line)
    }

    /// `fromArray(xs)`: the array's three words into a buffer-tagged header.
    fn stream_from_array(
        &mut self,
        b: &mut Frame,
        inner: &Type,
        line: usize,
    ) -> Result<Type, String> {
        let arr = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(arr));
        let al = self.layout_of(&Type::Array(Box::new(inner.clone())), line)?;
        let sl = self.stream_layout(line)?;
        let off = b.alloc(sl.size, sl.align);
        b.slot(off + sl.fields[0]);
        b.ins(&Instruction::LocalGet(arr));
        b.ins(&Instruction::I32Load(word_at(al.fields[0])));
        b.ins(&Instruction::I32Store(word()));
        b.slot(off + sl.fields[1]);
        b.ins(&Instruction::LocalGet(arr));
        b.ins(&Instruction::I64Load(at(al.fields[1])));
        b.ins(&Instruction::I64Store(word8()));
        // tag = -1 (a buffer, for the rest of this stream's life), and the three
        // words a buffer does not use.
        for (i, v) in [(2usize, -1i64), (3, 0), (4, 0), (5, 0)] {
            b.slot(off + sl.fields[i]);
            b.ins(&Instruction::I64Const(v));
            b.ins(&Instruction::I64Store(word8()));
        }
        b.slot(off);
        Ok(Type::Stream(Box::new(inner.clone())))
    }

    /// `fromStep(slot, gen, step)`: the step's two words and the cursor's two,
    /// each written straight into the pair of header fields that IS that value.
    /// The cursor arrives from the caller since RFC-0090 M3 — `std/stream` minted
    /// it out of its own `Slots` — so nothing is allocated here.
    fn stream_from_step(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        // Written argument order — the interpreter evaluates `slot`, `gen` and
        // the step left to right, so their effects (and any trap) happen in
        // that order here too. The step's SIGNATURE names the element type, but
        // its value is not needed until the header exists, so the two cursor
        // words wait in locals while the step is evaluated.
        self.expr_as(m, b, &args[0], &Type::Int)?;
        let c0 = b.local(ValType::I64);
        b.ins(&Instruction::LocalSet(c0));
        self.expr_as(m, b, &args[1], &Type::Int)?;
        let c1 = b.local(ValType::I64);
        b.ins(&Instruction::LocalSet(c1));
        let fty = self.expr(m, b, &args[2])?;
        let fv = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(fv));
        let sig = self.cx.resolve(&fty);
        let elem = match &sig {
            Type::Fn(_, r) => {
                let rr = self.cx.resolve(r);
                match ftypes::option_payload(&rr) {
                    Some(i) => i.clone(),
                    None => return unsupported(&format!("a step returning `{rr}`"), line),
                }
            }
            other => return unsupported(&format!("`fromStep` of `{other}`"), line),
        };
        // The loop reconstructs this signature from the element type alone, so a
        // step registered under any other spelling would dispatch through a
        // table it is not in. Refuse rather than miscompile.
        if crate::normalize_fn_sig(&sig, &self.cx.types)
            != crate::normalize_fn_sig(&stream_step_sig(&elem), &self.cx.types)
        {
            return unsupported(&format!("a step of type `{sig}`"), line);
        }
        let Repr::Agg(fl) = self.cx.repr(&sig, line)? else {
            return unsupported("a step value that is not an aggregate", line);
        };
        let sl = self.stream_layout(line)?;
        let off = b.alloc(sl.size, sl.align);
        b.slot(off + sl.fields[0]);
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32Store(word()));
        b.slot(off + sl.fields[1]);
        b.ins(&Instruction::I64Const(0));
        b.ins(&Instruction::I64Store(word8()));
        b.slot(off + sl.fields[2]);
        b.ins(&Instruction::LocalGet(fv));
        b.ins(&Instruction::I32Const(fl.size as i32));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        b.slot(off + sl.fields[4]);
        b.ins(&Instruction::LocalGet(c0));
        b.ins(&Instruction::I64Store(word8()));
        b.slot(off + sl.fields[5]);
        b.ins(&Instruction::LocalGet(c1));
        b.ins(&Instruction::I64Store(word8()));
        b.slot(off);
        Ok(Type::Stream(Box::new(elem)))
    }

    /// The word every boxed stream starts with (RFC-0090 M3). An address is an
    /// ordinary `Int64` a program can spell, so `unboxStream` and `pullAt` check this
    /// before they read a `Stream` out of it, and `unboxStream` clears it before it
    /// frees — a second `unboxStream` of one address is the trap rather than a second
    /// owner of one stream.
    const BOX_MAGIC: i64 = 3735928559;

    /// Leave the address of a checked box's `Stream` on the stack, or trap.
    fn stream_box_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        _line: usize,
    ) -> Result<u32, String> {
        let a = b.local(ValType::I32);
        self.expr_as(m, b, &args[0], &Type::Int)?;
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::LocalTee(a));
        b.ins(&Instruction::I32Eqz);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        let msg = self.cx.rt.intern(
            m,
            &vyrn_frontend::trap::line(vyrn_frontend::trap::NO_STREAM),
        );
        b.ins(&Instruction::I32Const(msg as i32));
        b.ins(&Instruction::Call(self.cx.rt.trap));
        self.depth -= 1;
        b.ins(&Instruction::End);
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I64Load(word8()));
        b.ins(&Instruction::I64Const(Self::BOX_MAGIC));
        b.ins(&Instruction::I64Ne);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        let msg = self.cx.rt.intern(
            m,
            &vyrn_frontend::trap::line(vyrn_frontend::trap::NO_STREAM),
        );
        b.ins(&Instruction::I32Const(msg as i32));
        b.ins(&Instruction::Call(self.cx.rt.trap));
        self.depth -= 1;
        b.ins(&Instruction::End);
        Ok(a)
    }

    /// `boxStream(s)` (RFC-0090 M3): the stream moves into one heap box and the
    /// call answers its address. A `Stream<T>` may not be a field of anything —
    /// M1 refuses it, because a field erases the disposal obligation — so this is
    /// where a lazy combinator's source lives, with `std/stream` holding the
    /// address in its own cursor slot.
    fn stream_box(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let sl = self.stream_layout(line)?;
        let p = b.local(ValType::I32);
        b.ins(&Instruction::I64Const((8 + sl.size) as i64));
        b.ins(&Instruction::Call(self.cx.rt.malloc));
        b.ins(&Instruction::LocalTee(p));
        b.ins(&Instruction::I64Const(Self::BOX_MAGIC));
        b.ins(&Instruction::I64Store(word8()));
        b.ins(&Instruction::LocalGet(p));
        b.ins(&Instruction::I32Const(8));
        b.ins(&Instruction::I32Add);
        let got = self.expr(m, b, &args[0])?;
        if !matches!(self.cx.resolve(&got), Type::Stream(_)) {
            return unsupported(&format!("`boxStream` of `{got}`"), line);
        }
        b.ins(&Instruction::I32Const(sl.size as i32));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        b.ins(&Instruction::LocalGet(p));
        b.ins(&Instruction::I64ExtendI32U);
        Ok(Type::Int)
    }

    /// `unboxStream(a)`: the stream comes back out and the box is freed.
    fn stream_unbox(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let elem = match self.expect.last().map(|t| self.cx.resolve(t)) {
            Some(Type::Stream(i)) => *i,
            _ => return unsupported("an `unboxStream` with no expected Stream type", line),
        };
        let sl = self.stream_layout(line)?;
        let a = self.stream_box_at(m, b, args, line)?;
        let off = b.alloc(sl.size, sl.align);
        b.slot(off);
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I32Const(8));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::I32Const(sl.size as i32));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I64Const(0));
        b.ins(&Instruction::I64Store(word8()));
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::Call(self.cx.rt.free));
        b.slot(off);
        Ok(Type::Stream(Box::new(elem)))
    }

    /// `pullAt(a)`: one element from the stream in that box (RFC-0075 M2c),
    /// which is the whole of what a wrapper's step can do that an ordinary
    /// producer's cannot. The element type is the annotation's: an address is an
    /// `Int64` whatever it addresses.
    fn stream_pull_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let want = self.expect.last().map(|t| self.cx.resolve(t));
        let elem = match want.as_ref().and_then(ftypes::option_payload) {
            Some(i) => i.clone(),
            None => return unsupported("a `pullAt` with no expected Option type", line),
        };
        let opt = Type::option(elem.clone());
        let Repr::Agg(ol) = self.cx.repr(&opt, line)? else {
            return unsupported("an Option that is not an aggregate", line);
        };
        let a = self.stream_box_at(m, b, args, line)?;
        let src = b.local(ValType::I32);
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I32Const(8));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalSet(src));

        // The element, then the `Option` this call's own signature owes.
        let r = self.cx.repr(&elem, line)?;
        let place = self.place_for(b, &r, line)?;
        let has = self.stream_next(m, b, src, place, &elem, line)?;
        let ooff = b.alloc(ol.size, ol.align);
        b.slot(ooff);
        b.ins(&Instruction::LocalGet(has));
        b.ins(&Instruction::I64ExtendI32U);
        b.ins(&Instruction::I64Store(word8()));
        for f in &ol.fields[1..] {
            b.slot(ooff + f);
            b.ins(&Instruction::I64Const(0));
            b.ins(&Instruction::I64Store(word8()));
        }
        b.ins(&Instruction::LocalGet(has));
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        self.store_payload(b, place, &elem, ooff + ol.fields[1], line)?;
        self.depth -= 1;
        b.ins(&Instruction::End);
        b.slot(ooff);
        Ok(opt)
    }

    /// Write a value of type `t`, held in `place`, into a sum's payload words at
    /// `w0` (RFC-0075 M2c). The encoding is [`Fn_::build_sum2`]'s, from a place
    /// rather than from an expression — which is what `pull` has.
    fn store_payload(
        &mut self,
        b: &mut Frame,
        place: Place,
        t: &Type,
        w0: u32,
        line: usize,
    ) -> Result<(), String> {
        if self.word2(t)? == Word::Inline2 {
            b.slot(w0);
            place
                .addr(b, 0)
                .ok_or_else(|| gap("a two-word payload with no address", line))?;
            b.ins(&Instruction::I32Const(16));
            b.ins(&Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });
            return Ok(());
        }
        // A scalar lives in a local and an aggregate in a slot, so "push the
        // value" is two spellings of one thing.
        let ll = self.cx.ll(t);
        let signed = self.cx.signed(t);
        let push = |b: &mut Frame| -> Result<(), String> {
            match place {
                Place::Local(l) => b.ins(&Instruction::LocalGet(l)),
                _ => {
                    place
                        .addr(b, 0)
                        .ok_or_else(|| gap("a payload with no address", line))?;
                    b.ins(&load_of(&ll, 0, signed))
                }
            };
            Ok(())
        };
        b.slot(w0);
        match self.word2(t)? {
            Word::Direct => {
                push(b)?;
            }
            Word::Ext(_) => {
                push(b)?;
                b.ins(&Instruction::I64ExtendI32U);
            }
            Word::Float(v) => {
                push(b)?;
                float_into_word(b, v);
            }
            _ => {
                place
                    .addr(b, 0)
                    .ok_or_else(|| gap("a boxed payload with no address", line))?;
                self.box_value(b, t, line)?;
                b.ins(&Instruction::I64ExtendI32U);
            }
        }
        b.ins(&Instruction::I64Store(word8()));
        Ok(())
    }

    /// A stream's release (RFC-0075 M2b, re-hosted by RFC-0090 M3): a buffer
    /// hands back the array data it was given, a producer gives its cursor slot
    /// back by CALLING its own step.
    ///
    /// The slab is `std/stream`'s now, so nothing here can name it. The step can:
    /// it is asked once with `closing` true, releases its slot and — if it is a
    /// wrapper — takes its source out of the box and `close`s it, which is an
    /// ordinary Vyrn `close` that `movecheck` checks. So the M2c walk down a
    /// chain is recursion in Vyrn rather than a loop here.
    fn stream_release(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        place: Place,
        elem: &Type,
        line: usize,
    ) -> Result<(), String> {
        // A stream is an aggregate, so a `Place::Local` holding one holds its
        // ADDRESS — the opposite of what it means for a scalar, and the reason
        // this does not just call `place.addr`.
        if let Place::Local(a) = place {
            return self.stream_release_at(m, b, a, elem, line);
        }
        let a = b.local(ValType::I32);
        place
            .addr(b, 0)
            .ok_or_else(|| gap("a stream with no address", line))?;
        b.ins(&Instruction::LocalSet(a));
        self.stream_release_at(m, b, a, elem, line)
    }

    fn stream_release_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        a: u32,
        elem: &Type,
        line: usize,
    ) -> Result<(), String> {
        let sl = self.stream_layout(line)?;
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I64Load(at(sl.fields[2])));
        b.ins(&Instruction::I64Const(0));
        b.ins(&Instruction::I64LtS);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        // A buffer owns the array data it was handed.
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I32Load(word_at(sl.fields[0])));
        b.ins(&Instruction::Call(self.cx.rt.free));
        self.depth -= 1;
        b.ins(&Instruction::Else);
        self.depth += 1;
        let opt = Type::option(elem.clone());
        let Repr::Agg(ol) = self.cx.repr(&opt, line)? else {
            return unsupported("an Option that is not an aggregate", line);
        };
        // Normalized, as every other dispatcher key is: a step's tag is
        // registered under `normalize_fn_sig`, and since RFC-0126 §8.11's M4b
        // that turns the `Option<T>` return into its variant list. A raw key
        // looked the tag up in a table it was not in.
        let step = crate::normalize_fn_sig(&stream_step_sig(elem), &self.cx.types);
        let dsig = self.dispatcher(m, &step, line)?;
        let ooff = b.alloc(ol.size, ol.align);
        b.slot(ooff);
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I32Const(sl.fields[2] as i32));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I64Load(at(sl.fields[4])));
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I64Load(at(sl.fields[5])));
        b.ins(&Instruction::I32Const(1));
        b.ins(&Instruction::Call(dsig.index));
        // The step's capture block. A stream owns the fn value it was built
        // with, so this is the one place that can hand it back; an empty capture
        // set is payload 0, which `free` refuses.
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I32Load(word_at(sl.fields[3])));
        b.ins(&Instruction::Call(self.cx.rt.free));
        self.depth -= 1;
        b.ins(&Instruction::End);
        Ok(())
    }

    /// One element from the stream at `s`, into `place`: the answer is the `i32`
    /// local returned, 1 if there was one (RFC-0075 M2c).
    ///
    /// Both readers go through here — `for … in` below and `pull`, which is what
    /// a lazy combinator's step is written in terms of. The two asked the same
    /// two questions in two spellings until M2c needed the second reader; the
    /// buffer arm's cursor advance and the producer arm's "a stream that ended
    /// stays ended" latch are exactly the kind of agreement that stops being
    /// true in one of two copies.
    ///
    /// It answers into a place rather than as an `Option<T>`, because an Option
    /// payload wider than a word is boxed — an emitter that answered one would
    /// have put an allocation in every `for r in fromArray(rs)` over a record.
    /// `pull` builds the Option its own signature owes and pays for it there.
    ///
    /// Neither arm branches OUT of itself, which is what keeps `self.depth` — and
    /// therefore every `break` in a caller's body — honest.
    fn stream_next(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        s: u32,
        place: Place,
        elem: &Type,
        line: usize,
    ) -> Result<u32, String> {
        let sl = self.stream_layout(line)?;
        let r = self.cx.repr(elem, line)?;
        let stride = self.stride(elem, line)?;
        let has = b.local(ValType::I32);
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::LocalSet(has));
        // Which producer? A negative tag is a buffer.
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::I64Load(at(sl.fields[2])));
        b.ins(&Instruction::I64Const(0));
        b.ins(&Instruction::I64LtS);
        b.ins(&Instruction::If(BlockType::Empty));

        // Buffer: cursor < len yields data[cursor] and steps the cursor.
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::I64Load(at(sl.fields[4])));
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::I64Load(at(sl.fields[1])));
        b.ins(&Instruction::I64LtU);
        b.ins(&Instruction::If(BlockType::Empty));
        let addr = b.local(ValType::I32);
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::I32Load(word_at(sl.fields[0])));
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::I64Load(at(sl.fields[4])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::I32Const(stride as i32));
        b.ins(&Instruction::I32Mul);
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalSet(addr));
        match (place, &r) {
            (Place::Local(l), _) => {
                b.ins(&Instruction::LocalGet(addr));
                b.ins(&load_of(&self.cx.ll(elem), 0, self.cx.signed(elem)));
                b.ins(&Instruction::LocalSet(l));
            }
            (Place::Slot(off), Repr::Agg(el)) => {
                b.slot(off);
                b.ins(&Instruction::LocalGet(addr));
                b.ins(&Instruction::I32Const(el.size as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }
            _ => return unsupported("a stream of Unit", line),
        }
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::I64Load(at(sl.fields[4])));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Add);
        b.ins(&Instruction::I64Store(at(sl.fields[4])));
        b.ins(&Instruction::I32Const(1));
        b.ins(&Instruction::LocalSet(has));
        b.ins(&Instruction::End);

        b.ins(&Instruction::Else);

        // Producer: a stream that ended stays ended, so `len` latches at 1 and
        // the step is never called again.
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::I64Load(at(sl.fields[1])));
        b.ins(&Instruction::I64Eqz);
        b.ins(&Instruction::If(BlockType::Empty));
        let opt = Type::option(elem.clone());
        let Repr::Agg(ol) = self.cx.repr(&opt, line)? else {
            return unsupported("an Option that is not an aggregate", line);
        };
        // Normalized, as every other dispatcher key is: a step's tag is
        // registered under `normalize_fn_sig`, and since RFC-0126 §8.11's M4b
        // that turns the `Option<T>` return into its variant list. A raw key
        // looked the tag up in a table it was not in.
        let step = crate::normalize_fn_sig(&stream_step_sig(elem), &self.cx.types);
        let dsig = self.dispatcher(m, &step, line)?;
        let ooff = b.alloc(ol.size, ol.align);
        b.slot(ooff);
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::I32Const(sl.fields[2] as i32));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::I64Load(at(sl.fields[4])));
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::I64Load(at(sl.fields[5])));
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::Call(dsig.index));
        let oaddr = b.local(ValType::I32);
        b.slot(ooff);
        b.ins(&Instruction::LocalSet(oaddr));
        let sum = crate::sum_variants_of(&opt, &self.cx.types).unwrap_or_default();
        let some = Pattern::Variant("Some".into(), vec![String::new()]);
        self.tag_test(b, oaddr, &sum, &some, line)?;
        b.ins(&Instruction::If(BlockType::Empty));
        let got = self.bind_payload(
            b,
            oaddr,
            &ol,
            std::slice::from_ref(elem),
            0,
            elem,
            line,
            false,
        )?;
        match (place, got, &r) {
            (Place::Local(d), Place::Local(v), _) => {
                b.ins(&Instruction::LocalGet(v));
                b.ins(&Instruction::LocalSet(d));
            }
            (Place::Slot(d), src, Repr::Agg(el)) => {
                b.slot(d);
                src.addr(b, 0)
                    .ok_or_else(|| gap("a stream payload in a local", line))?;
                b.ins(&Instruction::I32Const(el.size as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }
            _ => return unsupported("a stream element of this shape", line),
        }
        // The step built this Some's payload in a box of its own when the
        // element does not ride in two words; the binding above copied its own
        // out, so the box has no owner left. The mirror of `rel_word`'s boxed
        // arm, minus the walk — the contents now belong to `place`.
        if self.word2(elem)? == Word::Boxed {
            let p = b.local(ValType::I32);
            b.slot(ooff);
            b.ins(&Instruction::I64Load(at(ol.fields[1])));
            b.ins(&Instruction::I32WrapI64);
            b.ins(&Instruction::LocalSet(p));
            b.ins(&Instruction::LocalGet(p))
                .ins(&Instruction::Call(self.cx.rt.free));
        }
        b.ins(&Instruction::I32Const(1));
        b.ins(&Instruction::LocalSet(has));
        b.ins(&Instruction::Else);
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Store(at(sl.fields[1])));
        b.ins(&Instruction::End);
        b.ins(&Instruction::End);

        b.ins(&Instruction::End);
        Ok(has)
    }

    /// `for x in <stream>` — the pull loop.
    ///
    /// One iteration asks the stream for an element and gets a yes/no back in
    /// `has`; the two producers answer differently and nothing after the join
    /// knows which one did. Neither arm branches OUT of itself, which is what
    /// keeps `self.depth` — and therefore every `break` in the body — honest.
    fn for_stream(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        var: &str,
        body: &Block,
        elem: &Type,
        line: usize,
    ) -> Result<(), String> {
        let s = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(s));
        let r = self.cx.repr(elem, line)?;
        let place = self.place_for(b, &r, line)?;

        let brk = self.depth;
        b.ins(&Instruction::Block(BlockType::Empty));
        self.depth += 1;
        let top = self.depth;
        b.ins(&Instruction::Loop(BlockType::Empty));
        self.depth += 1;

        let has = self.stream_next(m, b, s, place, elem, line)?;

        // The join: no element means the loop is over, and the release below is
        // the one path out that still owns the stream.
        b.ins(&Instruction::LocalGet(has));
        b.ins(&Instruction::I32Eqz);
        let out = self.br_to(brk);
        b.ins(&Instruction::BrIf(out));

        let mark = self.scope.len();
        self.scope.push((var.to_string(), place, elem.clone()));
        // The one entry the placement has nothing for — see [`Fn_::cursors`].
        // A `break` leaves the loop through `fend`, which releases it, so only
        // an early `return` or `?` reaches this one.
        self.cursors
            .push((Place::Local(s), elem.clone(), self.rel_seq));
        let cont = self.depth;
        b.ins(&Instruction::Block(BlockType::Empty));
        self.depth += 1;
        self.loops.push((brk, cont, self.region_depth));
        self.block(m, b, body)?;
        self.loops.pop();
        self.depth -= 1;
        b.ins(&Instruction::End);
        self.cursors.pop();
        self.scope.truncate(mark);

        let back = self.br_to(top);
        b.ins(&Instruction::Br(back));
        self.depth -= 1;
        b.ins(&Instruction::End);
        self.depth -= 1;
        b.ins(&Instruction::End);

        // Normal end and `break` both land here.
        self.stream_release_at(m, b, s, elem, line)
    }

    /// Take the indexable value on the stack apart into locals.
    ///
    /// Fresh locals rather than scratch: a [`Walk`] outlives the expression that
    /// produced it — a `for` holds one across its whole body — so sharing would
    /// be a miscompile the moment two of them nested.
    fn walk(&mut self, b: &mut Frame, ty: &Type, line: usize) -> Result<Walk, String> {
        let addr = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(addr));
        let len = b.local(ValType::I64);
        Ok(match self.cx.resolve(ty) {
            // A `Stream<T>` used to share this arm (RFC-0075 M1) and does not any
            // more: it is a producer now, so it is pulled by `for_stream` rather
            // than indexed, and it reaches none of `walk`'s other six callers —
            // nothing indexes, pops or slices a stream.
            Type::Array(inner) => {
                let l = self.layout_of(ty, line)?;
                let data = b.local(ValType::I32);
                b.ins(&Instruction::LocalGet(addr));
                b.ins(&Instruction::I32Load(word_at(l.fields[0])));
                b.ins(&Instruction::LocalSet(data));
                b.ins(&Instruction::LocalGet(addr));
                b.ins(&Instruction::I64Load(at(l.fields[1])));
                b.ins(&Instruction::LocalSet(len));
                let stride = self.stride(&inner, line)?;
                Walk {
                    data,
                    len,
                    stride,
                    elem: *inner,
                    byte: false,
                }
            }
            // A fixed array is its own buffer: the slot address IS element 0,
            // and the length is in the type.
            Type::ArrayN(inner, n) => {
                b.ins(&Instruction::I64Const(n as i64));
                b.ins(&Instruction::LocalSet(len));
                let stride = self.stride(&inner, line)?;
                Walk {
                    data: addr,
                    len,
                    stride,
                    elem: *inner,
                    byte: false,
                }
            }
            Type::Str => {
                b.ins(&Instruction::LocalGet(addr));
                str_len(b);
                b.ins(&Instruction::I64ExtendI32U);
                b.ins(&Instruction::LocalSet(len));
                Walk {
                    data: addr,
                    len,
                    stride: 1,
                    elem: Type::Int,
                    byte: true,
                }
            }
            // A `SmallArray` is a four-field header with an inline buffer and two
            // live states (RFC-0056), and reading it as a triple would be a silent
            // miscompile rather than a missing one — its FIRST field is a length
            // where a growable array keeps a pointer. So the state branch happens
            // here, once, and what comes out is an ordinary base-and-count: every
            // element access downstream is indifferent to which buffer is live.
            Type::SmallArray(inner, n) => {
                let ty = self.cx.resolve(ty);
                let l = self.layout_of(&ty, line)?;
                let (sl, _cap, base) = self.sa_parts(b, addr, &l, n);
                let stride = self.stride(&inner, line)?;
                Walk {
                    data: base,
                    len: sl,
                    stride,
                    elem: *inner,
                    byte: false,
                }
            }
            other => return unsupported(&format!("indexing `{other}`"), line),
        })
    }

    /// Push the address of element `idx` (an `i64` local).
    fn elem_addr(&mut self, b: &mut Frame, w: &Walk, idx: u32) {
        b.ins(&Instruction::LocalGet(w.data));
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I32WrapI64);
        if w.stride != 1 {
            b.ins(&Instruction::I32Const(w.stride as i32));
            b.ins(&Instruction::I32Mul);
        }
        b.ins(&Instruction::I32Add);
    }

    /// Take a trap-table row — RFC-0125 §2.3, and the one way this backend
    /// refuses at any of the eight rows the census of §3 M6 gives the emitter.
    ///
    /// The site pushes the row's NUMBER and the value the row names; the
    /// wording is the table's, which `std/runtime`'s `trapAt` reads. Before
    /// this, each site pushed its own interned sentence and the two index rows
    /// pushed three pieces through a second helper.
    ///
    /// A function with a trap site (M1) parks the pair and branches OUT to it,
    /// so a check costs a compare and a branch rather than a call — the 3.56 s
    /// against 1.71 s the prologue's note records. The caller has already
    /// counted its own `if` in `depth`, so the trap block is `depth - 1`, one
    /// label inside the function block a `return` targets.
    ///
    /// `val` names the row's value as an `i64` local; a row without one passes
    /// zero, which `trapAt` never reads.
    fn trap_row(&mut self, b: &mut Frame, rule: vyrn_frontend::trap::Rule, val: Option<u32>) {
        let push_val = |b: &mut Frame| {
            match val {
                Some(v) => b.ins(&Instruction::LocalGet(v)),
                None => b.ins(&Instruction::I64Const(0)),
            };
        };
        match self.trap_site {
            Some((trule, tval)) => {
                b.ins(&Instruction::I32Const(rule.index() as i32));
                b.ins(&Instruction::LocalSet(trule));
                push_val(b);
                b.ins(&Instruction::LocalSet(tval));
                b.ins(&Instruction::Br(self.depth - 1));
            }
            None => {
                b.ins(&Instruction::I32Const(rule.index() as i32));
                push_val(b);
                b.ins(&Instruction::I32Const(self.cx.rt.trap_table as i32));
                b.ins(&Instruction::Call(self.cx.rt.trap_at));
            }
        }
    }

    /// Trap unless `idx` is in `0..len`.
    ///
    /// Unsigned, so a negative index is caught by the same compare.
    fn bounds_check(&mut self, b: &mut Frame, w: &Walk, idx: u32, string: bool) {
        let rule = if string {
            vyrn_frontend::trap::Rule::StringIndex
        } else {
            vyrn_frontend::trap::Rule::ArrayIndex
        };
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::LocalGet(w.len));
        b.ins(&Instruction::I64GeU);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        self.trap_row(b, rule, Some(idx));
        self.depth -= 1;
        b.ins(&Instruction::End);
    }

    /// Trap unless all `span` of `idx..idx+span-1` are in `0..len` (RFC-0083 M2).
    ///
    /// `span` is 4 for the four-lane shapes and 2 for `@f64x2` — the check, the
    /// address arithmetic and the trap are otherwise identical, which is why the
    /// widths share one arm here as they do in the textual backend.
    ///
    /// ONE branch for the whole vector — the amortisation that is the point of a
    /// vector load, and what a scalar loop cannot express. Two compares rather
    /// than [`bounds_check`]'s one because the unsigned trick does not survive a
    /// span: `idx + span` wraps for a huge `idx` and would let the access through,
    /// while `len - span` cannot wrap because `len >= 0`.
    fn bounds_check_span(&mut self, b: &mut Frame, w: &Walk, idx: u32, span: i64) {
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I64Const(0));
        b.ins(&Instruction::I64LtS);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::LocalGet(w.len));
        b.ins(&Instruction::I64Const(span));
        b.ins(&Instruction::I64Sub);
        b.ins(&Instruction::I64GtS);
        b.ins(&Instruction::I32Or);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        // The first lane of `idx..idx+span-1` actually out of range: `idx` when it
        // is negative, `idx + span - 1` when the tail overruns. Reporting `idx`
        // alone would name an in-range element in the common case, and this is the
        // cold path. A local rather than the stack, because the row's value is
        // what the trap site parks.
        let at = b.local(ValType::I64);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I64Const(span - 1));
        b.ins(&Instruction::I64Add);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I64Const(0));
        b.ins(&Instruction::I64LtS);
        b.ins(&Instruction::Select);
        b.ins(&Instruction::LocalSet(at));
        self.trap_row(b, vyrn_frontend::trap::Rule::ArrayIndex, Some(at));
        self.depth -= 1;
        b.ins(&Instruction::End);
    }

    /// Turn the element address on the stack into the element itself — a value
    /// for a scalar, and the address unchanged for an aggregate, which is the
    /// aggregate convention rather than an exception to it.
    fn load_elem(&mut self, b: &mut Frame, w: &Walk, line: usize) -> Result<(), String> {
        if w.byte {
            b.ins(&Instruction::I32Load8U(byte()));
            b.ins(&Instruction::I64ExtendI32U);
            return Ok(());
        }
        match self.cx.repr(&w.elem, line)? {
            Repr::Scalar(_) => {
                b.ins(&load_of(&self.cx.ll(&w.elem), 0, self.cx.signed(&w.elem)));
            }
            Repr::Agg(_) => {}
            Repr::Unit => return unsupported("an array of Unit", line),
        }
        Ok(())
    }

    /// `[a, b, c]`, and the empty `[]`.
    ///
    /// A literal is always the FIXED `[N x T]` shape, exactly as the LLVM
    /// backend builds it; the growable triple is reached from there through the
    /// same `ArrayN → Array` conversion, so there is one heap-wrapping path
    /// rather than one per literal position. The empty literal is the exception,
    /// because there is no element to take a type from — it can only be the
    /// empty triple its expected type names.
    fn array_lit(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hint: Option<(Dest, Type)>,
        elems: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let want = self.expect.last().map(|t| self.cx.resolve(t));
        let elem_want = match &want {
            Some(Type::Array(i)) | Some(Type::ArrayN(i, _)) | Some(Type::SmallArray(i, _)) => {
                Some((**i).clone())
            }
            _ => None,
        };
        // RFC-0125 M1: in an `Array<T>` position the elements are built straight
        // into the heap buffer and the triple is written where the consumer
        // wants it — its own storage when it handed one over, a slot the size
        // of the triple otherwise — so the fixed form, a frame extent the size
        // of the whole literal copied once more by `heapify`, never exists. In
        // a fixed position of the literal's own length the elements land in
        // the consumer's storage.
        if let Some((dest, hty)) = hint {
            match self.cx.resolve(&hty) {
                Type::Array(inner) if !matches!(*inner, Type::Param(_)) => {
                    return self.array_lit_heap(m, b, dest, &inner, elems, line, true);
                }
                Type::ArrayN(inner, n)
                    if n == elems.len() && n > 0 && !matches!(*inner, Type::Param(_)) =>
                {
                    self.fixed_elems(m, b, dest, &inner, elems, line)?;
                    dest.addr(b, 0);
                    self.dest_used = true;
                    return Ok(Type::ArrayN(inner, n));
                }
                _ => {}
            }
        }
        if let Some(Type::Array(inner)) = &want {
            if !matches!(**inner, Type::Param(_)) {
                let l = self.layout_of(&Type::Array(inner.clone()), line)?;
                let dest = Dest::Slot(b.alloc(l.size, l.align));
                return self.array_lit_heap(m, b, dest, inner, elems, line, false);
            }
        }
        // An empty `[]` in a `SmallArray<T, N>` position is the inline empty state,
        // not the empty triple: `len` 0, `cap` N, `data` null (RFC-0056). Built here
        // rather than through the `ArrayN` conversion because there is no fixed
        // literal to convert — `[N x T]` with N = 0 is not a shape `llt` prints.
        if elems.is_empty() {
            if let Some(Type::SmallArray(inner, n)) = want.clone() {
                let sa = Type::SmallArray(inner.clone(), n);
                self.sa_from_fixed(b, &inner, 0, &sa, n, line)?;
                return Ok(sa);
            }
            let Some(Type::Array(inner)) = want else {
                return unsupported(
                    "an empty array literal with no expected `Array<T>` type",
                    line,
                );
            };
            let ty = Type::Array(inner);
            let l = self.layout_of(&ty, line)?;
            let off = b.alloc(l.size, l.align);
            b.slot(off + l.fields[0]);
            b.ins(&Instruction::I32Const(0));
            b.ins(&Instruction::I32Store(word()));
            for f in [l.fields[1], l.fields[2]] {
                b.slot(off + f);
                b.ins(&Instruction::I64Const(0));
                b.ins(&Instruction::I64Store(word8()));
            }
            b.slot(off);
            return Ok(ty);
        }
        // An element type that IS an unsolved parameter names no type — `Deque {
        // front: [2, 1] }` reaches here with `Array<T>` expected and `T` open,
        // and there is no lowering for `T`. The elements answer for it, and the
        // enclosing literal's `solve_param` reads the parameter back off the
        // result. Only a BARE parameter, matching the checker and the textual
        // backend: an `Array<Array<T>>` field is refused in the checker.
        let elem = match elem_want.filter(|t| !matches!(t, Type::Param(_))) {
            Some(t) => t,
            None => self.peek(&elems[0], line)?,
        };
        let el = self.layout_of(&elem, line)?;
        let off = b.alloc(self.extent(&elem, elems.len(), line)?, el.align);
        self.fixed_elems(m, b, Dest::Slot(off), &elem, elems, line)?;
        b.slot(off);
        Ok(Type::ArrayN(Box::new(elem), elems.len()))
    }

    /// The elements of a literal, one after another from `dest` (RFC-0125 M1).
    /// An aggregate element is built in its own place, so a nested literal
    /// costs no frame.
    fn fixed_elems(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        elem: &Type,
        elems: &[Expr],
        line: usize,
    ) -> Result<(), String> {
        let stride = self.stride(elem, line)?;
        let r = self.cx.repr(elem, line)?;
        for (i, e) in elems.iter().enumerate() {
            let at = stride * i as u32;
            match &r {
                Repr::Scalar(_) => {
                    dest.addr(b, at);
                    self.expr_as(m, b, e, elem)?;
                    b.ins(&store_of(&self.cx.ll(elem)));
                }
                Repr::Agg(_) => self.agg_into(m, b, dest.at(at), stride, e, elem, true)?,
                Repr::Unit => return unsupported("an array of Unit", line),
            }
        }
        Ok(())
    }

    /// `[a, b, c]` in an `Array<T>` position, built on the heap at once
    /// (RFC-0125 M1): the buffer is taken first, the elements are built in it,
    /// and the `{ptr, len, cap}` triple is written at `dest`. `len` and `cap`
    /// are both N, the schedule [`Fn_::heapify`] gives a literal. The empty
    /// literal is the empty triple, `data` null, as it always was.
    #[allow(clippy::too_many_arguments)]
    fn array_lit_heap(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        inner: &Type,
        elems: &[Expr],
        line: usize,
        used: bool,
    ) -> Result<Type, String> {
        let ty = Type::Array(Box::new(inner.clone()));
        let l = self.layout_of(&ty, line)?;
        let n = elems.len();
        let buf = b.local(ValType::I32);
        if n == 0 {
            b.ins(&Instruction::I32Const(0));
            b.ins(&Instruction::LocalSet(buf));
        } else {
            let bytes = self.extent(inner, n, line)? as i32;
            b.ins(&Instruction::I64Const(bytes.max(1) as i64));
            b.ins(&Instruction::Call(self.cx.rt.malloc));
            b.ins(&Instruction::LocalSet(buf));
            self.fixed_elems(m, b, Dest::Addr(buf, 0), inner, elems, line)?;
        }
        dest.addr(b, l.fields[0]);
        b.ins(&Instruction::LocalGet(buf));
        b.ins(&Instruction::I32Store(word()));
        for f in [l.fields[1], l.fields[2]] {
            dest.addr(b, f);
            b.ins(&Instruction::I64Const(n as i64));
            b.ins(&Instruction::I64Store(word8()));
        }
        dest.addr(b, 0);
        self.dest_used = used;
        Ok(ty)
    }

    /// `[N x T]` → the growable `{ptr, len, cap}` triple: a heap buffer with a
    /// COPY of the elements in it.
    ///
    /// Copying rather than pointing at the frame slot is what makes the
    /// conversion sound — the triple outlives the frame, and `push` will
    /// reallocate the buffer it is handed.
    fn heapify(
        &mut self,
        b: &mut Frame,
        from: &Type,
        n: usize,
        want: &Type,
        line: usize,
    ) -> Result<(), String> {
        let src = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(src));
        let bytes = self.extent(from, n, line)? as i32;
        let buf = b.local(ValType::I32);
        b.ins(&Instruction::I64Const(bytes.max(1) as i64));
        b.ins(&Instruction::Call(self.cx.rt.malloc));
        b.ins(&Instruction::LocalTee(buf));
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::I32Const(bytes));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        let l = self.layout_of(want, line)?;
        let off = b.alloc(l.size, l.align);
        b.slot(off + l.fields[0]);
        b.ins(&Instruction::LocalGet(buf));
        b.ins(&Instruction::I32Store(word()));
        // len and cap are both N: a literal's buffer is exactly full, so the
        // first `push` grows it — the same schedule the LLVM path produces.
        for f in [l.fields[1], l.fields[2]] {
            b.slot(off + f);
            b.ins(&Instruction::I64Const(n as i64));
            b.ins(&Instruction::I64Store(word8()));
        }
        b.slot(off);
        Ok(())
    }

    /// The receiver of an array operation `std/runtime` rebuilds
    /// (PLAN-0125-runtime §6 step 6): the element type, the header layout, the
    /// element stride, the receiver's address parked in a local, and a fresh
    /// slot the runtime writes the result triple into. `xs.push(v)` is
    /// `xs = @push(xs, v)`, so the result is a NEW triple and the write-back is
    /// an ordinary assignment; `reserve`, `clear`, `append` and `copyFrom`
    /// (RFC-0115) rebuild the same way.
    fn arr_recv(
        &mut self,
        b: &mut Frame,
        aty: &Type,
        verb: &str,
        line: usize,
    ) -> Result<(Type, Layout, i32, u32, u32), String> {
        let Type::Array(elem) = self.cx.resolve(aty) else {
            return unsupported(&format!("`{verb}` on `{aty}`"), line);
        };
        let l = self.layout_of(aty, line)?;
        let stride = self.stride(&elem, line)? as i32;
        let src = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(src));
        let off = b.alloc(l.size, l.align);
        Ok((*elem, l, stride, src, off))
    }

    /// `xs.clear()` (RFC-0115 addendum): `std/runtime`'s `arrClear`.
    fn clear_arr(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let aty = self.expr(m, b, &args[0])?;
        let (_, _, _, src, off) = self.arr_recv(b, &aty, "clear", line)?;
        b.slot(off);
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::Call(self.cx.rt.arr_clear));
        b.slot(off);
        Ok(aty)
    }

    /// `xs.reserve(n)` (RFC-0115): `std/runtime`'s `arrReserve`. The count is
    /// evaluated into a local first, so no operand of the call is on the stack
    /// while a user expression runs.
    fn reserve_arr(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let aty = self.expr(m, b, &args[0])?;
        let (elem, _, stride, src, off) = self.arr_recv(b, &aty, "reserve", line)?;
        let n = b.local(ValType::I64);
        self.expr_as(m, b, &args[1], &Type::Int)?;
        b.ins(&Instruction::LocalSet(n));
        b.slot(off);
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::I32Const(stride));
        b.ins(&Instruction::LocalGet(n));
        b.ins(&Instruction::Call(self.cx.rt.arr_reserve));
        b.slot(off);
        Ok(Type::Array(Box::new(elem)))
    }

    /// `xs.append(ys)` and `dst.copyFrom(src)` (RFC-0115): `std/runtime`'s
    /// `arrAppend` and `arrCopyFrom`. The checker held the element type to
    /// heapless ones, so the runtime moves bytes and is handed no type.
    fn append_arr(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        self.arr_bulk(m, b, args, "append", line)
    }

    fn copy_from_arr(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        self.arr_bulk(m, b, args, "copyFrom", line)
    }

    fn arr_bulk(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        verb: &str,
        line: usize,
    ) -> Result<Type, String> {
        let aty = self.expr(m, b, &args[0])?;
        let (elem, _, stride, src, off) = self.arr_recv(b, &aty, verb, line)?;
        let xs = b.local(ValType::I32);
        self.expr_as(m, b, &args[1], &aty)?;
        b.ins(&Instruction::LocalSet(xs));
        b.slot(off);
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::I32Const(stride));
        b.ins(&Instruction::LocalGet(xs));
        b.ins(&Instruction::Call(if verb == "append" {
            self.cx.rt.arr_append
        } else {
            self.cx.rt.arr_copy_from
        }));
        b.slot(off);
        Ok(Type::Array(Box::new(elem)))
    }

    /// `xs.push(v)`: `std/runtime`'s `arrPush` grows and writes the new triple
    /// with `len + 1`; the element is stored here, because the runtime knows
    /// the stride and not the type.
    ///
    /// The old buffer comes back from the call and is released only after the
    /// element is stored, and that is not tidiness. The value expression is
    /// evaluated BELOW, and it may read the array being pushed onto —
    /// `w.push(rot1(w[t - 3] ^ w[t - 8] …))` in `std/hash` does, through the
    /// caller's header, which still names the OLD buffer. Freeing at the
    /// growth made that a read of a block already on a free list, and SHA-1
    /// came out wrong from the seventeenth word.
    fn push(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let aty = self.expr(m, b, &args[0])?;
        if let Type::SmallArray(inner, n) = self.cx.resolve(&aty) {
            let ty = self.cx.resolve(&aty);
            return self.sa_push(m, b, &ty, &inner, n, &args[1], line);
        }
        let (elem, l, stride, src, off) = self.arr_recv(b, &aty, "push", line)?;
        let stale = b.local(ValType::I32);
        b.slot(off);
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::I32Const(stride));
        b.ins(&Instruction::Call(self.cx.rt.arr_push));
        b.ins(&Instruction::LocalSet(stale));
        // The element goes at the old length, which the new triple holds plus one.
        let (data, last) = (b.local(ValType::I32), b.local(ValType::I64));
        b.slot(off);
        b.ins(&Instruction::I32Load(word_at(l.fields[0])));
        b.ins(&Instruction::LocalSet(data));
        b.slot(off);
        b.ins(&Instruction::I64Load(at(l.fields[1])));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Sub);
        b.ins(&Instruction::LocalSet(last));
        let w = Walk {
            data,
            len: last,
            stride: stride as u32,
            elem: elem.clone(),
            byte: false,
        };
        self.elem_addr(b, &w, last);
        let r = self.cx.repr(&elem, line)?;
        self.expr_as(m, b, &args[1], &elem)?;
        match &r {
            Repr::Scalar(_) => {
                b.ins(&store_of(&self.cx.ll(&elem)));
            }
            Repr::Agg(_) => {
                b.ins(&Instruction::I32Const(stride));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }
            Repr::Unit => return unsupported("an array of Unit", line),
        }
        // Now nothing can read the old buffer through the caller's header.
        b.ins(&Instruction::LocalGet(stale));
        b.ins(&Instruction::Call(self.cx.rt.free));
        b.slot(off);
        Ok(Type::Array(Box::new(elem)))
    }

    /// The element type of a user container: what its `place at` yields, with
    /// the impl head solved against this receiver (RFC-0091 M3).
    ///
    /// `peek` answers a type without emitting, and a projection's yielded place
    /// is a body it would have to walk. The declared return type says the same
    /// thing and says it in one substitution — `impl<T> Index for Slots<T>` with
    /// a receiver of `Slots<Node>` yields a `Node`.
    fn user_elem(&self, ty: &Type) -> Option<Type> {
        let (imp, f) = vyrn_frontend::project::lookup_impl(&self.cx.impls, ty, "at")?;
        let mut subst = HashMap::new();
        crate::solve_param(&imp.ty, ty, &mut subst);
        Some(self.cx.sub(&ftypes::substitute(&f.ret, &subst)))
    }

    /// `xs[i]` — bounds-checked, and a String's `s[i]` with it.
    /// `a[i]` (RFC-0091 M2): resolve the `place at` projection for the
    /// receiver's type, inline its body here, and read the place it yields.
    ///
    /// A builtin container resolves to the seeded row, which yields
    /// `@slot(self, i)` with an empty prologue — the substitution is the
    /// identity, so [`Fn_::at`] below emits exactly the bytes it emitted when
    /// it was reached by name.
    fn project_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        // Naming the receiver's type here costs a type probe, and the probe is
        // `&mut self`. A program with no projection at all never needs it.
        let recv = if vyrn_frontend::project::any(&self.cx.impls) {
            self.peek(&args[0], line).ok()
        } else {
            None
        };
        // `None` is the seeded row, whose expansion is the identity: `Fn_::at`
        // below reads the ORIGINAL nodes. `project::site` decides that once.
        let Some(p) = vyrn_frontend::project::site(
            &self.cx.impls,
            recv.as_ref(),
            "at",
            &args[0],
            &args[1..],
            line,
        )?
        else {
            return self.at(m, b, args, line);
        };
        for s in &p.prologue {
            self.stmt(m, b, s)?;
        }
        self.expr(m, b, &p.place)
    }

    fn at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        // RFC-0125 M1: a header a `while` hoisted is already in locals.
        let (w, string) = match self.cached_walk(&args[0]) {
            Some(w) => {
                let string = w.byte;
                (w, string)
            }
            None => {
                let aty = self.expr(m, b, &args[0])?;
                // A Map is not walkable and must not be reached as one: its
                // length is field 2 where an Array's is field 1, so a `Walk`
                // over it would index off the value pointer (M2c's refusal,
                // now a branch instead).
                if let Type::Map(_, val) = self.cx.resolve(&aty) {
                    let mty = self.cx.resolve(&aty);
                    return self.map_at(m, b, &mty, &val, &args[1], line);
                }
                let string = self.cx.resolve(&aty) == Type::Str;
                (self.walk(b, &aty, line)?, string)
            }
        };
        self.expr_as(m, b, &args[1], &Type::Int)?;
        let idx = b.local(ValType::I64);
        b.ins(&Instruction::LocalSet(idx));
        self.bounds_check(b, &w, idx, string);
        self.elem_addr(b, &w, idx);
        // `s[i]` is a `UInt8` (RFC-0022), not the `Int` a `for` over the same
        // String yields — the two really do differ, and the LLVM backend has the
        // same pair.
        if string {
            b.ins(&Instruction::I32Load8U(byte()));
            return Ok(Type::IntN {
                bits: 8,
                signed: false,
            });
        }
        self.load_elem(b, &w, line)?;
        Ok(w.elem)
    }

    /// `xs.pop()` → `Option<T>`, shrinking the binding in place. Variable-only,
    /// which is the checker's rule too: it returns a value AND mutates, so there
    /// is no assignment the parser could have desugared it into.
    fn pop(&mut self, b: &mut Frame, args: &[Expr], line: usize) -> Result<Type, String> {
        let (place, aty) = self.receiver(args, "pop", line)?;
        // The binding's ADDRESS, taken once: `pop` shrinks the triple in place, so
        // it needs the storage rather than the value — and module state is storage
        // at a fixed address exactly as a frame slot is at a moving one.
        let slot = b.local(ValType::I32);
        place
            .addr(b, 0)
            .ok_or_else(|| gap("`pop` on a non-array binding", line))?;
        b.ins(&Instruction::LocalSet(slot));
        let Type::Array(elem) = self.cx.resolve(&aty) else {
            return unsupported(&format!("`pop` on `{aty}`"), line);
        };
        let elem = *elem;
        let al = self.layout_of(&aty, line)?;
        let opt = Type::option(elem.clone());
        let ol = self.layout_of(&opt, line)?;
        let out = b.alloc(ol.size, ol.align);
        // `None` first, then the `Some` arm overwrites the tag and the payload:
        // one destination, filled in place, which is destination-first with the
        // trivial arm pre-applied.
        b.slot(out + ol.fields[0]);
        b.ins(&Instruction::I64Const(0));
        b.ins(&Instruction::I64Store(word8()));
        for f in &ol.fields[1..] {
            b.slot(out + f);
            b.ins(&Instruction::I64Const(0));
            b.ins(&Instruction::I64Store(word8()));
        }
        b.ins(&Instruction::LocalGet(slot));
        let w = self.walk(b, &aty, line)?;
        b.ins(&Instruction::LocalGet(w.len));
        b.ins(&Instruction::I64Eqz);
        b.ins(&Instruction::I32Eqz);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        let last = b.local(ValType::I64);
        b.ins(&Instruction::LocalGet(slot));
        b.ins(&Instruction::I32Const(al.fields[1] as i32));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalGet(w.len));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Sub);
        b.ins(&Instruction::LocalTee(last));
        b.ins(&Instruction::I64Store(word8()));
        b.slot(out + ol.fields[0]);
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Store(word8()));
        b.slot(out + ol.fields[1]);
        self.elem_addr(b, &w, last);
        self.load_elem(b, &w, line)?;
        self.encode_word2(b, &elem, line)?;
        b.ins(&Instruction::I64Store(word8()));
        self.depth -= 1;
        b.ins(&Instruction::End);
        b.slot(out);
        Ok(opt)
    }

    /// `xs.swapRemove(i)` → the element, with the last one moved into its slot.
    /// O(1) and unordered, which is the whole point of it (RFC-0011).
    fn swap_remove(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let (place, aty) = self.receiver(args, "swapRemove", line)?;
        let slot = b.local(ValType::I32);
        place
            .addr(b, 0)
            .ok_or_else(|| gap("`swapRemove` on a non-array binding", line))?;
        b.ins(&Instruction::LocalSet(slot));
        let Type::Array(elem) = self.cx.resolve(&aty) else {
            return unsupported(&format!("`swapRemove` on `{aty}`"), line);
        };
        let elem = *elem;
        let al = self.layout_of(&aty, line)?;
        b.ins(&Instruction::LocalGet(slot));
        let w = self.walk(b, &aty, line)?;
        self.expr_as(m, b, &args[1], &Type::Int)?;
        let idx = b.local(ValType::I64);
        b.ins(&Instruction::LocalSet(idx));
        self.bounds_check(b, &w, idx, false);
        // The removed element goes to a slot of its own before the last one
        // lands on top of it — for `i == len-1` those are the same address.
        let r = self.cx.repr(&elem, line)?;
        let taken = self.place_for(b, &r, line)?;
        match (taken, &r) {
            (Place::Local(l), _) => {
                self.elem_addr(b, &w, idx);
                self.load_elem(b, &w, line)?;
                b.ins(&Instruction::LocalSet(l));
            }
            (Place::Slot(off), Repr::Agg(el)) => {
                b.slot(off);
                self.elem_addr(b, &w, idx);
                b.ins(&Instruction::I32Const(el.size as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }
            _ => return unsupported("an array of Unit", line),
        }
        let last = b.local(ValType::I64);
        b.ins(&Instruction::LocalGet(slot));
        b.ins(&Instruction::I32Const(al.fields[1] as i32));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalGet(w.len));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Sub);
        b.ins(&Instruction::LocalTee(last));
        b.ins(&Instruction::I64Store(word8()));
        self.elem_addr(b, &w, idx);
        self.elem_addr(b, &w, last);
        b.ins(&Instruction::I32Const(w.stride as i32));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        match taken {
            Place::Local(l) => {
                b.ins(&Instruction::LocalGet(l));
            }
            // `place_for` hands out a local or a frame slot, never module state.
            p => {
                p.addr(b, 0);
            }
        }
        Ok(elem)
    }

    /// The binding a mutating array method is applied to. Anything else is a gap
    /// rather than a silent no-op: a `pop` whose shrink went nowhere is a wrong
    /// program.
    fn receiver(
        &mut self,
        args: &[Expr],
        what: &str,
        line: usize,
    ) -> Result<(Place, Type), String> {
        match args.first() {
            Some(Expr::Var { name, .. }) => self.lookup(name, line),
            _ => unsupported(
                &format!("`{what}` on something that is not a variable"),
                line,
            ),
        }
    }

    /// Encode the value on the stack into an `Option`'s first payload word.
    fn encode_word2(&mut self, b: &mut Frame, t: &Type, line: usize) -> Result<(), String> {
        match self.word2(t)? {
            Word::Direct => {}
            Word::Ext(_) => {
                b.ins(&Instruction::I64ExtendI32U);
            }
            Word::Float(v) => float_into_word(b, v),
            Word::Boxed => self.box_value(b, t, line)?,
            // A two-word payload is copied whole by `build_sum2`, not encoded
            // into one word; doing it here would need the second word too.
            Word::Inline2 => return unsupported("an Option of a two-word payload", line),
        }
        Ok(())
    }
}

// ---------------------------------------------------------------------------
// Sum types: Option, Result, and user enums
// ---------------------------------------------------------------------------

/// The tag-and-payload shape behind a sum type.
///
/// Two conventions, both inherited from the LLVM emitter rather than invented
/// here: `Option`/`Result` are `{ i1 tag, i64 w0, i64 w1 }` with two payload
/// words (so a `Ref` fits inline, unboxed), while a user enum is
/// `{ i64 tag, i64 p0, .. }` with one word per payload slot of its widest
/// variant. Inheriting them is not politeness — parity compares this backend's
/// output against a build that uses the other one.
type Sum = Vec<EnumVariant>;

/// The tag an arm tests, or `None` for the arm that tests nothing — RFC-0121's
/// `Pattern::Other`, which a user cannot spell and which the switch's shape is
/// read off ([`crate::two_way`]).
fn tag_of(sum: &Sum, pat: &Pattern, line: usize) -> Result<Option<usize>, String> {
    Ok(match pat {
        Pattern::Other => None,
        Pattern::Variant(name, _) => Some(
            sum.iter()
                .position(|v| v.name == *name)
                .ok_or_else(|| gap(&format!("the variant `{name}`"), line))?,
        ),
        // `??`'s pair (RFC-0079) names a TAG and not a variant: the desugar
        // runs in the parser, where there is no type to name one. Tag 1 is the
        // success side of every built-in sum (RFC-0126 §8.1).
        _ => Some(usize::from(matches!(pat, Pattern::Success(_)))),
    })
}

/// How one payload travels inside a sum's `i64` word.
#[derive(PartialEq)]
enum Word {
    /// It IS the word.
    Direct,
    /// A narrower INTEGER scalar, zero-extended into the word.
    Ext(ValType),
    /// A float, whose BITS ride in the word — an `f64` reinterpreted, an `f32`
    /// reinterpreted and then zero-extended. `Ext` cannot serve here: it emits
    /// `i64.extend_i32_u`, which is a validation error against an `f64` on the
    /// stack, so `Option<Float64>` produced a module wasmtime refused to load
    /// rather than a diagnostic (RFC-0078 M4a found it; nothing in the corpus had
    /// ever put a float in a sum payload).
    Float(ValType),
    /// Two words, side by side, no heap (a `Ref` or a stored `fn`).
    Inline2,
    /// The word is a pointer to it.
    Boxed,
}

/// A float on the stack, as the `i64` word a sum payload holds. Its BITS, not its
/// value: the round trip has to be exact, and an `f32` widened to `f64` and back
/// would be too, but reinterpreting is one instruction either way.
fn float_into_word(b: &mut Frame, v: ValType) {
    if v == ValType::F32 {
        b.ins(&Instruction::I32ReinterpretF32);
        b.ins(&Instruction::I64ExtendI32U);
    } else {
        b.ins(&Instruction::I64ReinterpretF64);
    }
}

// ---- RFC-0089 M1b: `x.copy()` ---------------------------------------------

impl<'p> Fn_<'_, 'p> {
    /// Whether a value of `ty` transitively owns heap — the frontend's own
    /// predicate, so this backend copies exactly what the textual one does.
    fn owns_heap(&self, ty: &Type) -> bool {
        vyrn_frontend::declared::owns_heap(&self.cx.sub(ty), &self.cx.types)
    }

    /// `x.copy()`: the receiver's value is on the stack; replace it with one
    /// that shares no heap with it.
    ///
    /// A `String` is the only owning value this backend keeps in a wasm local;
    /// everything else is an aggregate in the frame, so the copy is a byte copy
    /// of the shape followed by [`Fn_::copy_at`] over what the bytes point at.
    fn copy_stack(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        ty: &Type,
        line: usize,
    ) -> Result<(), String> {
        if !self.owns_heap(ty) {
            return Ok(());
        }
        match self.cx.repr(ty, line)? {
            Repr::Scalar(ValType::I32) if matches!(self.cx.resolve(ty), Type::Str) => {
                self.str_dup(b);
                Ok(())
            }
            Repr::Agg(l) => {
                let src = b.local(ValType::I32);
                b.ins(&Instruction::LocalSet(src));
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                b.ins(&Instruction::LocalGet(src));
                b.ins(&Instruction::I32Const(l.size as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                let a = b.local(ValType::I32);
                b.slot(off);
                b.ins(&Instruction::LocalSet(a));
                self.copy_at(m, b, a, ty, line)?;
                b.ins(&Instruction::LocalGet(a));
                Ok(())
            }
            _ => unsupported(&format!("`copy` of `{ty}`"), line),
        }
    }

    /// A `String` pointer on the stack, replaced by a fresh buffer holding the
    /// same bytes. The length is a header load since RFC-0089 M1a, so nothing
    /// scans.
    ///
    /// The funnel `copy_stack`, `copy_at` and `copy_word` share, which is what
    /// makes one [`Fn_::str_owned`] here answer for every `String` a `copy`
    /// reaches — an element of an `Array<String>`, a record field, a `Map` key —
    /// the way one `Gen::str_alloc` inside `Gen::deep_copy` answers for all of
    /// them on the textual backend. `@str` of a `String` and of a `Bool` come
    /// here too, and `Gen`'s two arms for those are `str_alloc` as well.
    fn str_dup(&mut self, b: &mut Frame) {
        let (s, n, d) = (
            b.local(ValType::I32),
            b.local(ValType::I32),
            b.local(ValType::I32),
        );
        b.ins(&Instruction::LocalSet(s));
        b.ins(&Instruction::LocalGet(s));
        str_len(b);
        b.ins(&Instruction::LocalSet(n));
        b.ins(&Instruction::LocalGet(n));
        b.ins(&Instruction::LocalGet(n));
        self.arena_route(b, true);
        b.ins(&Instruction::Call(self.cx.rt.str_new));
        self.arena_route(b, false);
        b.ins(&Instruction::LocalSet(d));
        b.ins(&Instruction::LocalGet(d));
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::LocalGet(n));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        b.ins(&Instruction::LocalGet(d));
    }

    /// A fresh heap block of `bytes` holding a copy of `live` bytes from `src`,
    /// its address left in a new local. One byte of slack, so copying an empty
    /// container never asks the allocator for nothing.
    fn dup_buf(&mut self, b: &mut Frame, src: u32, live: u32, bytes: u32) -> u32 {
        let nb = b.local(ValType::I32);
        b.ins(&Instruction::LocalGet(bytes));
        b.ins(&Instruction::I32Const(1));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::I64ExtendI32U);
        b.ins(&Instruction::Call(self.cx.rt.malloc));
        b.ins(&Instruction::LocalSet(nb));
        b.ins(&Instruction::LocalGet(nb));
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::LocalGet(live));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        nb
    }

    /// Release each of the first `count` elements of `buf` — the mirror of
    /// [`Fn_::copy_each`], and RFC-0092 M2's half of census U4.
    ///
    /// The gate is the element's own release ROW, not whether it reaches heap. A
    /// record reaches two Strings and has no row until M3, and walking into one
    /// here would free fields no rule says the array owns. A row is the proof;
    /// `owns_heap` is only a reachability question.
    fn rel_each(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        buf: u32,
        count: u32,
        stride: u32,
        elem: &Type,
        line: usize,
    ) -> Result<(), String> {
        if self.cx.owned.release_kind(elem).is_none() {
            return Ok(());
        }
        let i = b.local(ValType::I32);
        let p = b.local(ValType::I32);
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::LocalSet(i));
        let out = self.depth;
        b.ins(&Instruction::Block(BlockType::Empty));
        self.depth += 1;
        let again = self.depth;
        b.ins(&Instruction::Loop(BlockType::Empty));
        self.depth += 1;
        b.ins(&Instruction::LocalGet(i));
        b.ins(&Instruction::LocalGet(count));
        b.ins(&Instruction::I32GeU);
        let leave = self.br_to(out);
        b.ins(&Instruction::BrIf(leave));
        b.ins(&Instruction::LocalGet(buf));
        b.ins(&Instruction::LocalGet(i));
        if stride != 1 {
            b.ins(&Instruction::I32Const(stride as i32));
            b.ins(&Instruction::I32Mul);
        }
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalSet(p));
        self.rel_at(m, b, p, elem, line)?;
        b.ins(&Instruction::LocalGet(i));
        b.ins(&Instruction::I32Const(1));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalSet(i));
        let back = self.br_to(again);
        b.ins(&Instruction::Br(back));
        b.ins(&Instruction::End);
        self.depth -= 1;
        b.ins(&Instruction::End);
        self.depth -= 1;
        Ok(())
    }

    /// Replace each of the first `count` elements of `buf` with a deep copy of
    /// itself. No loop is emitted at all when the element owns no heap.
    fn copy_each(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        buf: u32,
        count: u32,
        stride: u32,
        elem: &Type,
        line: usize,
    ) -> Result<(), String> {
        if !self.owns_heap(elem) {
            return Ok(());
        }
        let i = b.local(ValType::I32);
        let p = b.local(ValType::I32);
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::LocalSet(i));
        let out = self.depth;
        b.ins(&Instruction::Block(BlockType::Empty));
        self.depth += 1;
        let again = self.depth;
        b.ins(&Instruction::Loop(BlockType::Empty));
        self.depth += 1;
        b.ins(&Instruction::LocalGet(i));
        b.ins(&Instruction::LocalGet(count));
        b.ins(&Instruction::I32GeU);
        let leave = self.br_to(out);
        b.ins(&Instruction::BrIf(leave));
        b.ins(&Instruction::LocalGet(buf));
        b.ins(&Instruction::LocalGet(i));
        if stride != 1 {
            b.ins(&Instruction::I32Const(stride as i32));
            b.ins(&Instruction::I32Mul);
        }
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalSet(p));
        self.copy_at(m, b, p, elem, line)?;
        b.ins(&Instruction::LocalGet(i));
        b.ins(&Instruction::I32Const(1));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalSet(i));
        let back = self.br_to(again);
        b.ins(&Instruction::Br(back));
        b.ins(&Instruction::End);
        self.depth -= 1;
        b.ins(&Instruction::End);
        self.depth -= 1;
        Ok(())
    }

    /// The bytes at `a` already hold a copy of a value of `ty`. Give that copy
    /// its own heap — as a CALL, the same shape a release takes
    /// ([`Fn_::rel_at`]). The walk is [`Fn_::copy_body`], written once per type.
    fn copy_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        a: u32,
        ty: &Type,
        line: usize,
    ) -> Result<(), String> {
        if !self.owns_heap(ty) {
            return Ok(());
        }
        let f = self.shape_fn(m, false, ty, line);
        b.ins(&Instruction::LocalGet(a)).ins(&Instruction::Call(f));
        Ok(())
    }

    /// What copying a value of `ty` MEANS: the storage it owns, duplicated.
    fn copy_body(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        a: u32,
        ty: &Type,
        line: usize,
    ) -> Result<(), String> {
        match self.cx.resolve(ty) {
            Type::Str => {
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I32Load(word()));
                self.str_dup(b);
                b.ins(&Instruction::I32Store(word()));
                Ok(())
            }
            Type::Array(inner) => {
                let l = self.layout_of(ty, line)?;
                let stride = self.stride(&inner, line)?;
                let (n, bytes) = (b.local(ValType::I32), b.local(ValType::I32));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[1])));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::LocalTee(n));
                b.ins(&Instruction::I32Const(stride as i32));
                b.ins(&Instruction::I32Mul);
                b.ins(&Instruction::LocalSet(bytes));
                let src = b.local(ValType::I32);
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I32Load(word_at(l.fields[0])));
                b.ins(&Instruction::LocalSet(src));
                let nb = self.dup_buf(b, src, bytes, bytes);
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::LocalGet(nb));
                b.ins(&Instruction::I32Store(word_at(l.fields[0])));
                // The copy's capacity is its length: a copy is a fresh buffer,
                // and the room the original had spare is not part of its value.
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[1])));
                b.ins(&Instruction::I64Store(at(l.fields[2])));
                self.copy_each(m, b, nb, n, stride, &inner, line)
            }
            // A `SmallArray<T, N>` that has not spilled owns no buffer, so the
            // header copy is the whole copy of its storage.
            Type::SmallArray(inner, cap_n) => {
                let l = self.layout_of(ty, line)?;
                let stride = self.stride(&inner, line)?;
                let (n, base) = (b.local(ValType::I32), b.local(ValType::I32));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[0])));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::LocalSet(n));
                // Inline while `cap == N`; the data pointer is live otherwise.
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I32Const(l.fields[3] as i32));
                b.ins(&Instruction::I32Add);
                b.ins(&Instruction::LocalSet(base));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[1])));
                b.ins(&Instruction::I64Const(cap_n as i64));
                b.ins(&Instruction::I64Ne);
                b.ins(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                let (src, bytes) = (b.local(ValType::I32), b.local(ValType::I32));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I32Load(word_at(l.fields[2])));
                b.ins(&Instruction::LocalSet(src));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[1])));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::I32Const(stride as i32));
                b.ins(&Instruction::I32Mul);
                b.ins(&Instruction::LocalSet(bytes));
                let live = b.local(ValType::I32);
                b.ins(&Instruction::LocalGet(n));
                b.ins(&Instruction::I32Const(stride as i32));
                b.ins(&Instruction::I32Mul);
                b.ins(&Instruction::LocalSet(live));
                let nb = self.dup_buf(b, src, live, bytes);
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::LocalGet(nb));
                b.ins(&Instruction::I32Store(word_at(l.fields[2])));
                b.ins(&Instruction::LocalGet(nb));
                b.ins(&Instruction::LocalSet(base));
                self.depth -= 1;
                b.ins(&Instruction::End);
                self.copy_each(m, b, base, n, stride, &inner, line)
            }
            Type::Map(kt, vt) => {
                // String keys are dup'd per entry; Int64 keys copy with the
                // buffer (RFC-0117) — 8-byte stride, no per-element walk.
                let ik = self.cx.resolve(&kt) == Type::Int;
                let l = self.layout_of(ty, line)?;
                let vstride = self.stride(&vt, line)?;
                let (n, cap) = (b.local(ValType::I32), b.local(ValType::I32));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[2])));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::LocalSet(n));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[3])));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::LocalSet(cap));
                let kstride = if ik { 8u32 } else { 4u32 };
                for (i, (stride, elem)) in [(kstride, Type::Str), (vstride, (*vt).clone())]
                    .into_iter()
                    .enumerate()
                {
                    let (src, live, room) = (
                        b.local(ValType::I32),
                        b.local(ValType::I32),
                        b.local(ValType::I32),
                    );
                    b.ins(&Instruction::LocalGet(a));
                    b.ins(&Instruction::I32Load(word_at(l.fields[i])));
                    b.ins(&Instruction::LocalSet(src));
                    b.ins(&Instruction::LocalGet(n));
                    b.ins(&Instruction::I32Const(stride as i32));
                    b.ins(&Instruction::I32Mul);
                    b.ins(&Instruction::LocalSet(live));
                    b.ins(&Instruction::LocalGet(cap));
                    b.ins(&Instruction::I32Const(stride as i32));
                    b.ins(&Instruction::I32Mul);
                    b.ins(&Instruction::LocalSet(room));
                    let nb = self.dup_buf(b, src, live, room);
                    b.ins(&Instruction::LocalGet(a));
                    b.ins(&Instruction::LocalGet(nb));
                    b.ins(&Instruction::I32Store(word_at(l.fields[i])));
                    if !(ik && i == 0) {
                        self.copy_each(m, b, nb, n, stride, &elem, line)?;
                    }
                }
                // The index is copied rather than rebuilt: it holds POSITIONS,
                // and a copy keeps the capacity as well as the order, so every
                // bucket still names the entry it named. `cap * 2` buckets of
                // eight bytes, all of them live.
                let (isrc, ibytes) = (b.local(ValType::I32), b.local(ValType::I32));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I32Load(word_at(l.fields[4])));
                b.ins(&Instruction::LocalSet(isrc));
                b.ins(&Instruction::LocalGet(cap));
                b.ins(&Instruction::I32Const(16));
                b.ins(&Instruction::I32Mul);
                b.ins(&Instruction::LocalSet(ibytes));
                let ib = self.dup_buf(b, isrc, ibytes, ibytes);
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::LocalGet(ib));
                b.ins(&Instruction::I32Store(word_at(l.fields[4])));
                Ok(())
            }
            Type::Record(_) => {
                let l = self.layout_of(ty, line)?;
                let fields = self
                    .cx
                    .fields(ty)
                    .ok_or_else(|| gap(&format!("the fields of `{ty}`"), line))?;
                for (i, f) in fields.iter().enumerate() {
                    if !self.owns_heap(&f.ty) {
                        continue;
                    }
                    let p = b.local(ValType::I32);
                    b.ins(&Instruction::LocalGet(a));
                    b.ins(&Instruction::I32Const(l.fields[i] as i32));
                    b.ins(&Instruction::I32Add);
                    b.ins(&Instruction::LocalSet(p));
                    self.copy_at(m, b, p, &f.ty, line)?;
                }
                Ok(())
            }
            Type::ArrayN(inner, n) => {
                let stride = self.stride(&inner, line)?;
                let count = b.local(ValType::I32);
                b.ins(&Instruction::I32Const(n as i32));
                b.ins(&Instruction::LocalSet(count));
                self.copy_each(m, b, a, count, stride, &inner, line)
            }
            // ANY sum: the payload slots of the live variant, and only the ones
            // whose declared type owns something. The tag is the variant's
            // position, exactly as `match` reads it. The mirror of `rel_at`'s own
            // arm, and one walk for the same reason (RFC-0126 §8.11, M4a).
            Type::Enum(_) => {
                let vs = self.cx.sum_vs(ty).unwrap_or_default();
                let l = self.layout_of(ty, line)?;
                for (tag, var) in vs.iter().enumerate() {
                    if !var.payload.iter().any(|p| self.owns_heap(p)) {
                        continue;
                    }
                    tag_eq(b, a, tag as i64);
                    b.ins(&Instruction::If(BlockType::Empty));
                    self.depth += 1;
                    for (j, pty) in var.payload.clone().iter().enumerate() {
                        if !self.owns_heap(pty) {
                            continue;
                        }
                        let at = self.cx.payload_slot(&var.payload, j);
                        let w = self.word2(pty)?;
                        self.copy_word(m, b, a, l.fields[at], pty, w, line)?;
                    }
                    self.depth -= 1;
                    b.ins(&Instruction::End);
                }
                Ok(())
            }
            // A stored `fn` value (RFC-0037, Phase 10b): `{ tag, captures }`, and
            // the copy is a fresh capture block. The block's SIZE is per tag, so
            // the walk cannot be written here — it is one call to the module's
            // derived copy, which holds the registry the tags index.
            Type::Fn(..) => {
                let l = self.layout_of(ty, line)?;
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[0])));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[1])));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::Call(self.cx.fnval_copy));
                b.ins(&Instruction::I64ExtendI32U);
                b.ins(&Instruction::I64Store(at(l.fields[1])));
                Ok(())
            }
            // A handle names something; copying it names the same thing.
            Type::Task(_) | Type::Lazy(_) => Ok(()),
            other => unsupported(&format!("`copy` of `{other}`"), line),
        }
    }

    /// Give the sum payload word at `a + off` its own heap.
    ///
    /// Only two encodings can own anything: a `String` rides in the word itself,
    /// and everything wider is a pointer to a block this copies and then walks.
    fn copy_word(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        a: u32,
        off: u32,
        pty: &Type,
        w: Word,
        line: usize,
    ) -> Result<(), String> {
        match w {
            Word::Ext(ValType::I32) if matches!(self.cx.resolve(pty), Type::Str) => {
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(off)));
                b.ins(&Instruction::I32WrapI64);
                self.str_dup(b);
                b.ins(&Instruction::I64ExtendI32U);
                b.ins(&Instruction::I64Store(at(off)));
                Ok(())
            }
            Word::Boxed => {
                let size = self.layout_of(pty, line)?.size;
                let (src, bytes) = (b.local(ValType::I32), b.local(ValType::I32));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(off)));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::LocalSet(src));
                b.ins(&Instruction::I32Const(size as i32));
                b.ins(&Instruction::LocalSet(bytes));
                let nb = self.dup_buf(b, src, bytes, bytes);
                self.copy_at(m, b, nb, pty, line)?;
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::LocalGet(nb));
                b.ins(&Instruction::I64ExtendI32U);
                b.ins(&Instruction::I64Store(at(off)));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    fn sum_of(&self, ty: &Type) -> Option<Sum> {
        self.cx.sum_vs(ty)
    }

    /// How an `Option`/`Result` payload of type `t` fills its two words.
    fn word2(&self, t: &Type) -> Result<Word, String> {
        Ok(match self.cx.repr(t, 0)? {
            Repr::Scalar(ValType::I64) => Word::Direct,
            Repr::Scalar(v @ (ValType::F64 | ValType::F32)) => Word::Float(v),
            // A vector is 128 bits — the two payload words exactly, but as a
            // wasm VALUE and not an address, so `Inline2`'s `memory.copy` has
            // nothing to copy from. It boxes instead, which is what every other
            // payload wider than a word does. `Ext` was the arm it used to fall
            // into, and `i64.extend_i32_u` on a `v128` is a module wasmtime
            // refuses to load rather than a diagnostic — the same shape of bug
            // `Word::Float` was added for.
            Repr::Scalar(ValType::V128) => Word::Boxed,
            Repr::Scalar(v) => Word::Ext(v),
            // `words(t) == 2` and not the shape STRING: since RFC-0126 §8.4 a
            // one-slot sum prints `{ i64, i64 }` too, and a payload that is
            // itself a sum rides in one slot, boxed. Reading the string here
            // gave a nested `Option` two slots where the shape gave it one, and
            // the second word was read as the payload.
            Repr::Agg(_) if self.cx.words(t) == 2 => Word::Inline2,
            _ => Word::Boxed,
        })
    }

    /// Copy the value on the stack (a scalar, or an aggregate's address) onto
    /// the heap, leaving its address as an `i64` word.
    fn box_value(&mut self, b: &mut Frame, t: &Type, line: usize) -> Result<(), String> {
        let malloc = self.cx.rt.malloc;
        let ll = self.cx.ll(t);
        match self.cx.repr(t, line)? {
            Repr::Scalar(v) => {
                let size = layout::of_ll(&ll)
                    .map_err(|e| format!("direct backend: {e}"))?
                    .size;
                // Two DIFFERENT scratch slots, and it matters: `scratch` is keyed on
                // (type, n), so for an i32-shaped scalar — a `String`, a `Bool`, a
                // `UInt8` — the same `n` would hand out one local for both, the
                // `LocalTee` below would clobber the value with the box's address,
                // and the box would end up holding a pointer to itself. `print` of
                // one showed the pointer's bytes where the string belonged.
                let val = self.scratch(b, v, 2);
                let p = self.scratch(b, ValType::I32, 3);
                b.ins(&Instruction::LocalSet(val));
                b.ins(&Instruction::I64Const(size as i64));
                b.ins(&Instruction::Call(malloc));
                b.ins(&Instruction::LocalTee(p));
                b.ins(&Instruction::LocalGet(val));
                b.ins(&store_of(&ll));
                b.ins(&Instruction::LocalGet(p));
            }
            Repr::Agg(l) => {
                let src = self.scratch(b, ValType::I32, 1);
                let p = self.scratch(b, ValType::I32, 2);
                b.ins(&Instruction::LocalSet(src));
                b.ins(&Instruction::I64Const(l.size as i64));
                b.ins(&Instruction::Call(malloc));
                b.ins(&Instruction::LocalTee(p));
                b.ins(&Instruction::LocalGet(src));
                b.ins(&Instruction::I32Const(l.size as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                b.ins(&Instruction::LocalGet(p));
            }
            Repr::Unit => return unsupported("a Unit payload", line),
        }
        b.ins(&Instruction::I64ExtendI32U);
        Ok(())
    }

    /// Build an `Option`/`Result` value: the tag, then the two payload words.
    #[allow(clippy::too_many_arguments)]
    fn build_sum2(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        ty: &Type,
        tag: i32,
        payload: Option<(&Expr, Type)>,
        line: usize,
        hint: Option<(Dest, Type)>,
    ) -> Result<Type, String> {
        let args: Vec<&Expr> = payload.iter().map(|(e, _)| *e).collect();
        let tys: Vec<Type> = payload.iter().map(|(_, t)| t.clone()).collect();
        self.build_variant(m, b, ty, tag as u64, &args, &tys, line, hint)
    }

    /// Build a sum value: the tag, then the live variant's payloads in the slots
    /// they occupy. ONE builder since M2 — a built-in sum and a declared enum
    /// have one tag width and one payload encoding (RFC-0126 §8.4), so the two
    /// that stood here differed only in which one they refused.
    #[allow(clippy::too_many_arguments)]
    fn build_variant(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        ty: &Type,
        tag: u64,
        args: &[&Expr],
        payload: &[Type],
        line: usize,
        hint: Option<(Dest, Type)>,
    ) -> Result<Type, String> {
        if args.len() != payload.len() {
            return unsupported("an enum variant at this arity", line);
        }
        let Repr::Agg(l) = self.cx.repr(ty, line)? else {
            return unsupported("a sum that is not an aggregate", line);
        };
        // RFC-0125 M1: the consumer's storage when it holds this very type.
        let (dest, used) = match hint {
            Some((d, t)) if self.cx.ll(&t) == self.cx.ll(ty) => (d, true),
            _ => (Dest::Slot(b.alloc(l.size, l.align)), false),
        };
        dest.addr(b, 0);
        b.ins(&Instruction::I64Const(tag as i64));
        b.ins(&Instruction::I64Store(word8()));
        // Every slot this variant does not fill is zeroed: a `None` and a
        // narrower variant must not leave the widest one's words behind.
        let mut filled = 1;
        for (i, (a, t)) in args.iter().zip(payload).enumerate() {
            let at = self.cx.payload_slot(payload, i);
            if self.word2(t)? == Word::Inline2 {
                // Two words already side by side: one copy, no encoding.
                dest.addr(b, l.fields[at]);
                self.expr_as(m, b, a, t)?;
                b.ins(&Instruction::I32Const(16));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            } else {
                dest.addr(b, l.fields[at]);
                self.expr_as(m, b, a, t)?;
                self.encode_word2(b, t, line)?;
                b.ins(&Instruction::I64Store(word8()));
            }
            filled = at + self.cx.words(t);
        }
        for slot in filled..l.fields.len() {
            dest.addr(b, l.fields[slot]);
            b.ins(&Instruction::I64Const(0));
            b.ins(&Instruction::I64Store(word8()));
        }
        dest.addr(b, 0);
        self.dest_used = used;
        Ok(ty.clone())
    }

    /// The sum type an expectation names, if it names one.
    fn expected_sum(&self) -> Option<Type> {
        self.expect
            .last()
            .filter(|t| self.sum_of(t).is_some())
            .cloned()
    }

    /// The sum type a `Some`/`Ok`/`Err` is built at, with the payload's own type.
    /// `None` when the position names no sum and the constructor cannot type
    /// itself.
    ///
    /// The position decides both — a `Some(0)` in an `Option<UInt8>` slot is a
    /// UInt8. Except where the position has not solved its own parameter yet:
    /// `Bag { one: Some(5) }` reaches here with `Option<T>` expected, `T` has no
    /// lowering, and the payload is the only thing that knows. Solving the
    /// parameter from the payload rebuilds the whole sum, so an `Ok(5)` under
    /// `Result<T, String>` keeps the error half the position named.
    ///
    /// Shared by `peek` and by `sum_ctor` for `expected_type_args`'s reason: the
    /// two must report one type for one constructor, or the field is built at
    /// one and read at the other.
    fn sum_ctor_types(
        &mut self,
        name: &str,
        arg: &Expr,
        line: usize,
    ) -> Result<Option<(Type, Type)>, String> {
        let want = self.expected_sum();
        let picked = want
            .as_ref()
            .and_then(|t| self.sum_of(t).map(|s| (t.clone(), s)));
        let (ty, payload) = match picked {
            // The position's sum names the payload the constructor carries: the
            // variant with that name, in the variant list `sum_of` answers for
            // every sum since RFC-0126 §8.11's M4b.
            Some((t, vs)) if vs.iter().any(|v| v.name == name && v.payload.len() == 1) => {
                let p = vs
                    .iter()
                    .find(|v| v.name == name)
                    .expect("the guard just found it")
                    .payload[0]
                    .clone();
                (t, p)
            }
            // An unexpected `Some` still types itself from its payload;
            // `Ok`/`Err` cannot, because the other half is unknowable.
            _ if name == "Some" => {
                let p = self.peek(arg, line)?;
                return Ok(Some((Type::option(p.clone()), p)));
            }
            _ => return Ok(None),
        };
        if !matches!(payload, Type::Param(_)) {
            return Ok(Some((ty, payload)));
        }
        let p = self.peek(arg, line)?;
        let mut sub = HashMap::new();
        crate::solve_param(&payload, &p, &mut sub);
        Ok(Some((
            ftypes::substitute(&ty, &sub),
            ftypes::substitute(&payload, &sub),
        )))
    }

    /// `Some(x)` / `Ok(x)` / `Err(e)` / `Circle(r)` / `None`, or `Ok(None)` if
    /// `name` is not a constructor at all.
    fn sum_ctor(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[Expr],
        line: usize,
        hint: Option<(Dest, Type)>,
    ) -> Result<Option<Type>, String> {
        let want = self.expected_sum();
        match name {
            "None" => {
                let ty = want.ok_or_else(|| gap("a `None` with no expected Option type", line))?;
                return self.build_sum2(m, b, &ty, 0, None, line, hint).map(Some);
            }
            "Some" | "Ok" | "Err" => {
                if args.len() != 1 {
                    return unsupported(&format!("`{name}` at this arity"), line);
                }
                // The payload's type is the position's, not the argument's — a
                // `Some(0)` in an `Option<UInt8>` slot is a UInt8.
                let Some((ty, payload)) = self.sum_ctor_types(name, &args[0], line)? else {
                    return unsupported(&format!("`{name}` with no expected Result type"), line);
                };
                let tag = i32::from(name != "Err");
                return self
                    .build_sum2(m, b, &ty, tag, Some((&args[0], payload)), line, hint)
                    .map(Some);
            }
            _ => {}
        }
        if !self.cx.variants.contains_key(name) {
            return Ok(None);
        }
        // Two enums may declare the same variant name; the expectation decides,
        // and an ambiguity with nothing to decide it is a gap, not a coin toss.
        let pick = want.as_ref().and_then(|t| match self.cx.resolve(t) {
            Type::Enum(vs) => vs
                .iter()
                .position(|v| v.name == name)
                .map(|i| (t.clone(), i as u64, vs[i].payload.clone())),
            _ => None,
        });
        let (ty, tag, payload) = match pick {
            Some(p) => p,
            None => {
                let tag = match self.cx.variants.get(name) {
                    Some(c) if c.len() == 1 => c[0].1,
                    _ => return unsupported(&format!("the ambiguous variant `{name}`"), line),
                };
                // A generic enum has no type until a use site fixes it, and a
                // bare constructor's use site is its PAYLOAD — the rule
                // `Gen::applied_enum_type` gives the textual emitter, shared.
                let Some(ty) = self.applied_variant(name, args, line)? else {
                    return unsupported(&format!("the ambiguous variant `{name}`"), line);
                };
                // The payloads the APPLIED type declares, which for a generic
                // enum are the solved ones rather than its parameters.
                let payload = match self.cx.resolve(&ty) {
                    Type::Enum(vs) => vs
                        .iter()
                        .find(|v| v.name == name)
                        .map(|v| v.payload.clone())
                        .unwrap_or_default(),
                    _ => return unsupported(&format!("the variant `{name}` of `{ty}`"), line),
                };
                (ty, tag, payload)
            }
        };
        let refs: Vec<&Expr> = args.iter().collect();
        self.build_variant(m, b, &ty, tag, &refs, &payload, line, hint)
            .map(Some)
    }

    /// What a pattern binds, and to what — without emitting anything, because a
    /// join needs the arm's type before the arm exists.
    fn pattern_binds(
        &self,
        sum: &Sum,
        pat: &Pattern,
        line: usize,
    ) -> Result<Vec<(String, Type)>, String> {
        // The variant the pattern names, in the list `sum_of` answers for every
        // sum since RFC-0126 §8.11's M4b. `??`'s pair (RFC-0079) names a TAG
        // rather than a variant — variant 1 succeeds, variant 0 fails — which is
        // the one thing the parser could not do; a sum wider than two has no
        // success side as a pattern.
        let (at, binds): (usize, &[String]) = match pat {
            // The refutable-`let` desugar's default arm (RFC-0121): any
            // variant, nothing bound.
            Pattern::Other => return Ok(Vec::new()),
            Pattern::Variant(name, binds) => (
                sum.iter()
                    .position(|v| v.name == *name)
                    .ok_or_else(|| gap(&format!("the variant `{name}`"), line))?,
                binds,
            ),
            Pattern::Success(n) | Pattern::Failure(n) if sum.len() == 2 => (
                usize::from(matches!(pat, Pattern::Success(_))),
                std::slice::from_ref(n),
            ),
            _ => return unsupported("a pattern of the wrong shape for its scrutinee", line),
        };
        let v = &sum[at];
        // A `Failure` over a nullary variant binds nothing — an `Option`'s tag 0
        // has no payload, and that is exactly what this per-arm seam is for.
        if matches!(pat, Pattern::Success(_) | Pattern::Failure(_)) {
            return Ok(v
                .payload
                .first()
                .map(|t| vec![(binds[0].clone(), t.clone())])
                .unwrap_or_default());
        }
        if v.payload.len() != binds.len() {
            return unsupported(&format!("the variant `{}` at this arity", v.name), line);
        }
        Ok(binds
            .iter()
            .cloned()
            .zip(v.payload.iter().cloned())
            .collect())
    }

    /// `match` — the n-way join M0 warned about, lowered destination-first.
    ///
    /// The arms are a chain of `if`s inside one `block`, and each arm leaves by
    /// branching to it: a scalar result rides the branch, an aggregate one is
    /// copied into a slot allocated BEFORE the first test. Nothing here counts
    /// arms, which is the property that makes 46 four-to-seven-way joins cost
    /// exactly what 103 diamonds cost.
    #[allow(clippy::too_many_arguments)]
    fn match_expr(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        key: usize,
        scrutinee: &Expr,
        arms: &[ArmRef],
        form: &str,
        line: usize,
    ) -> Result<Type, String> {
        let st = self.expr(m, b, scrutinee)?;
        let sum = self
            .sum_of(&st)
            .ok_or_else(|| gap(&format!("a {form} on `{st}`"), line))?;
        // The shape, asked once, before anything is emitted: two arms, one
        // tag and a default is a two-way branch, and every other shape is the
        // chain (RFC-0125 §3 M5, and [`crate::two_way`]).
        let tags: Vec<Option<usize>> = arms
            .iter()
            .map(|a| tag_of(&sum, a.pattern, line))
            .collect::<Result<_, _>>()?;
        let two = crate::two_way(&tags);
        // One scratch slot for every switch. `if let` took a local of its
        // own, on the argument that "an inner one's scrutinee would take the
        // same scratch slot back" — which the emitted order says is not so:
        // nothing reads the address after the arm's binds, and an inner
        // construct is inside an arm. So the two forms share the slot, and a
        // function that writes both now declares one local where it declared
        // two.
        let addr = self.scratch(b, ValType::I32, 3);
        b.ins(&Instruction::LocalSet(addr));
        let Repr::Agg(sl) = self.cx.repr(&st, line)? else {
            return unsupported(&format!("a {form} on a non-aggregate"), line);
        };
        // The scrutinee's release, where `own` says this match is its last owner
        // — the `if let` release above, at the construct that also carries a
        // value out. A row exists only where no arm handed the payload on, so
        // the copy released here holds nothing anything else still names.
        //
        // A frame of its own, and a slot of its own, for the two reasons the
        // `if let` states: an arm that returns walks the frames, and an arm may
        // build over the scratch the scrutinee was left in.

        // A ROW, and no per-binding table: a construct that TOOK its scrutinee
        // has no row, and a slot registered for a release nobody emits is a
        // slot no later statement can reuse — the frame then grows once per
        // construct, and one generated `main` of 316 matches went past the
        // 8 KB limit (RFC-0125 §3 M3, the walk's deletion).
        if self.releases_whole(key) {
            if let Some(r) = self.rel_for(&st, line)? {
                let own = b.alloc(sl.size, sl.align);
                b.slot(own);
                b.ins(&Instruction::LocalGet(addr));
                b.ins(&Instruction::I32Const(sl.size as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                self.register_rel(b, key, Place::Slot(own), r);
            }
        }
        // A block arm (RFC-0118) makes this a STATEMENT match: the merge
        // carries nothing, whatever the expression arms beside it compute is
        // dropped, and the checker's answer for the node is about a value the
        // construct does not hand out. Every other match is one value, and the
        // checker said which type it has — `expr_as` then coerces each arm's
        // own shape into it.
        let want = if arms.iter().any(ArmRef::is_block) {
            Type::Unit
        } else {
            self.join_ty(key, line)?
        };
        let r = self.cx.repr(&want, line)?;

        let dest = match &r {
            Repr::Agg(l) => Some((b.alloc(l.size, l.align), l.size)),
            _ => None,
        };
        // A two-way branch is an `if`/`else` and joins where it ends, so it
        // needs no block to leave and no `br` to leave it by. Every other
        // shape is a chain of `if`s inside one block, and each arm branches
        // out of it.
        let bt = match &r {
            Repr::Scalar(v) => BlockType::Result(*v),
            _ => BlockType::Empty,
        };
        let out = self.depth;
        if two.is_none() {
            b.ins(&Instruction::Block(bt));
            self.depth += 1;
        }

        // RFC-0114 Rule N at a match join, keyed by this expression's address.
        let ers = self.cx.edge_rows(key);
        let free_box = self.frees_boxes(scrutinee, key);
        // The tagged arm goes first in a two-way branch, whichever side the
        // source wrote it on: the `if` tests a tag and the `else` is what is
        // left.
        let order: Vec<usize> = match two {
            Some((at, _)) => vec![at, 1 - at],
            None => (0..arms.len()).collect(),
        };
        // Where the `else` went, so an arm that writes nothing can take it
        // back — `if let` with no `else` is the whole of that case.
        let mut els = None;
        for (slot, arm_ix) in order.into_iter().enumerate() {
            let arm = &arms[arm_ix];
            match two {
                Some(_) if slot == 1 => {
                    els = Some(b.here());
                    b.ins(&Instruction::Else);
                }
                Some(_) => {
                    self.tag_test(b, addr, &sum, arm.pattern, line)?;
                    b.ins(&Instruction::If(bt));
                    self.depth += 1;
                }
                None => {
                    self.tag_test(b, addr, &sum, arm.pattern, line)?;
                    b.ins(&Instruction::If(BlockType::Empty));
                    self.depth += 1;
                }
            }

            let mark = self.scope.len();
            let binds = self.pattern_binds(&sum, arm.pattern, line)?;
            let ptys: Vec<Type> = binds.iter().map(|(_, t)| t.clone()).collect();
            let mut bound: Vec<(String, Place, Type)> = Vec::new();
            // The binders' own plan keys, taken off the pattern the reader
            // wrote — the same address the core keys them by
            // ([`vyrn_frontend::own::binder_key`]). A synthetic pattern this
            // backend builds has none in the plan and registers nothing.
            let keys: Vec<usize> = match arm.pattern {
                Pattern::Variant(_, ns) => ns
                    .iter()
                    .map(|n| vyrn_frontend::own::binder_key(n))
                    .collect(),
                Pattern::Success(n) | Pattern::Failure(n) => {
                    vec![vyrn_frontend::own::binder_key(n)]
                }
                Pattern::Other => Vec::new(),
            };
            for (i, (n, t)) in binds.into_iter().enumerate() {
                let place = self.bind_payload(b, addr, &sl, &ptys, i, &t, line, free_box)?;
                // A binder the arm owns is released at every exit INSIDE the
                // arm, and the placer's row names it there (RFC-0125 §3 M3,
                // the walk's deletion). The arm's END is the other half and
                // `arm_row` below states it; the two are exclusive, since a
                // binder handed out or already freed is held at neither.
                if let Some(key) = keys.get(i).copied() {
                    if self.releases_whole(key) {
                        if let Some(rel) = self.rel_for(&t, line)? {
                            self.register_rel(b, key, place.clone(), rel);
                        }
                    }
                }
                bound.push((n.clone(), place.clone(), t.clone()));
                self.scope.push((n, place, t));
            }
            match (&arm.body, dest) {
                (BodyRef::Expr(body), Some((off, size))) => {
                    b.slot(off);
                    self.expr_as(m, b, body, &want)?;
                    b.ins(&Instruction::I32Const(size as i32));
                    b.ins(&Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                }
                // A statement match (RFC-0118): a block arm is its statements;
                // an expression arm beside one computes and drops — `want` is
                // Unit whenever any arm is a block, so a dest never exists on
                // this path.
                (BodyRef::Expr(body), None) if matches!(want, Type::Unit) => {
                    let got = self.expr(m, b, body)?;
                    if !matches!(self.cx.repr(&got, line)?, Repr::Unit) {
                        b.ins(&Instruction::Drop);
                    }
                }
                (BodyRef::Expr(body), None) => self.expr_as(m, b, body, &want)?,
                (BodyRef::Block(blk), _) => self.block(m, b, blk)?,
            }
            // Round forty: the unmoved payload binders the row names — the
            // textual backend's `gen_arm_body` twin.
            let owed = self.cx.arm_row(key, arm_ix as u32);
            if let Some(rows) = owed {
                for (n, place, ty) in &bound {
                    let Some((_, holes)) = rows.iter().find(|(r, _)| r == n) else {
                        continue;
                    };
                    let (place, ty) = (place.clone(), ty.clone());
                    if let Some(mut rel) = self.rel_for(&ty, line)? {
                        // RFC-0125 M3: the arm handed part of the binder out.
                        if let Rel::Deep(t, _) = rel {
                            rel = Rel::Deep(t, holes.clone());
                        }
                        self.emit_rel(m, b, place, &rel, line)?;
                    }
                }
            }
            // A binder's scope is its arm, and so is the row that names it:
            // no exit past this point can reach one. Un-pinning the slot here
            // is what keeps the frame the size it was — a row held to the
            // function's `return` would hold its slot with it, and one
            // generated `main` grew past the 8 KB frame limit
            // (`numbers.rs`'s differential parser).
            for key in &keys {
                self.rel_slots.remove(key);
                self.rel_pending.retain(|(k, _)| k != key);
            }
            self.scope.truncate(mark);
            self.emit_edge_releases(m, b, &ers, arm_ix as u32, line)?;
            match two {
                Some(_) if slot == 0 => {}
                Some(_) => {
                    // An `else` that wrote nothing is no `else` at all. A
                    // branch that carries a value always writes one, so only
                    // an empty-result `if` can lose it.
                    if let Some(at) =
                        els.filter(|at| b.here() == at + 1 && matches!(bt, BlockType::Empty))
                    {
                        b.rewind(at);
                    }
                    self.depth -= 1;
                    b.ins(&Instruction::End);
                }
                None => {
                    let d = self.br_to(out);
                    b.ins(&Instruction::Br(d));
                    self.depth -= 1;
                    b.ins(&Instruction::End);
                }
            }
        }
        if two.is_none() {
            // The checker proves the arms exhaustive; the validator cannot see
            // the proof, so it is told instead.
            b.ins(&Instruction::Unreachable);
            self.depth -= 1;
            b.ins(&Instruction::End);
        }
        // The fall-through release, after the arms have rejoined and before the
        // aggregate result's address is pushed. A scalar result is already on
        // the stack here and the release is stack-neutral, so it sits under it.
        self.emit_releases(m, b, ExitKind::Scrutinee, key)?;

        if let Some((off, _)) = dest {
            b.slot(off);
        }
        self.diverged(b, &want);
        Ok(want)
    }

    /// `e?` — unwrap `Some`/`Ok`, or carry the whole sum out of the function
    /// (RFC-0005).
    ///
    /// The propagation is a `return` in everything but the instruction, and M1's
    /// rule is that a body must not emit one: the epilogue that releases the
    /// shadow-stack frame, and since M2f copies `modify` parameters back, sits
    /// AFTER the block every exit branches to. So this writes the sum through
    /// `dest` exactly as [`Stmt::Return`] does and takes the same `br` to the same
    /// block — which is why `?` needs no reclamation of its own and cannot leak a
    /// frame or skip a copy-back.
    ///
    /// The success path is the FALL-THROUGH, not an arm: the failing side branches
    /// away, so there is nothing to join and no `peek` to get wrong. The `if` is
    /// stack-neutral, so a destination address already sitting under the operand
    /// stack — `let r: Rec = f(g()?)` puts one there — survives it, the same
    /// property M2d needed for a validation.
    fn try_(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        e: &Expr,
        line: usize,
        at: usize,
    ) -> Result<Type, String> {
        let st = self.expr(m, b, e)?;
        // Tag 1 is the success side of the two BUILT-IN sums (§8.1); every other
        // sum asks `Fallible` (RFC-0080 M3) instead of the tag. The test is the
        // variant NAMES, not the arity: a declared `| Full(T) | Gone(String)`
        // has two variants and is not a `Result` (RFC-0126 §8.16).
        let (sum, ok_ty) = match self.sum_of(&st) {
            Some(vs) if ftypes::is_builtin_sum(&vs) && vs[1].payload.len() == 1 => {
                let ok_ty = vs[1].payload[0].clone();
                (vs, ok_ty)
            }
            _ => return self.try_fallible(m, b, &st, line, at),
        };
        // "Tag 1 succeeds" is [`Pattern::Success`], which `ast.rs` says was added
        // for exactly this — "the same trick `Expr::Try` plays for `?`, moved
        // into `Pattern`". A `Variant` rebuilt here from `vs[1].name` said it a
        // second time. The binder is unread either way: `tag_test` takes the tag
        // from the pattern and `bind_payload` the type from the sum.
        let ok_pat = Pattern::Success(String::new());
        let Repr::Agg(sl) = self.cx.repr(&st, line)? else {
            return unsupported("`?` on a non-aggregate sum", line);
        };
        // The propagated value is the WHOLE sum, byte for byte, which is only
        // sound if the two are the same shape. Since RFC-0126 §8.4 a sum's slot
        // count follows its widest payload, so the two really can differ, and a
        // memcpy has a width — so the width is checked rather than assumed.
        //
        // The checker's rule does not guarantee it: `check_try`'s `Option` arm
        // reads the return as `Some(_)` and never compares the payloads, because
        // only the FAILING variant travels. So `Option<fn(Int64, Int64) -> Int64>`
        // unwrapped in a function returning `Option<Int64>` types, and arrives
        // here two words wide against one. A width is a LAYOUT, so it is refused
        // as a gap rather than diagnosed (RFC-0125 §3 M3, the `?` census).
        let ret_ty = self.ret_ty.clone();
        if self.sum_of(&ret_ty).is_none() || self.cx.ll(&ret_ty) != self.cx.ll(&st) {
            return unsupported(
                &format!("`?` on `{st}` in a function returning `{}`", self.ret_ty),
                line,
            );
        }
        let Repr::Agg(rl) = self.ret.clone() else {
            return unsupported("`?` in a function whose return is not an aggregate", line);
        };
        let addr = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(addr));
        self.tag_test(b, addr, &sum, &ok_pat, line)?;
        b.ins(&Instruction::I32Eqz);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        b.ins(&Instruction::LocalGet(
            self.dest.expect("an aggregate return has a destination"),
        ));
        b.ins(&Instruction::LocalGet(addr));
        b.ins(&Instruction::I32Const(rl.size as i32));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        // `?` is `Stmt::Return` minus the keyword, so it owes the same two
        // unwinds. It did not pay them: a `?` out of a `region` left the counter
        // raised, and the 65th such call aborted where the interpreter kept
        // going. The value is already copied through `dest`, so neither of these
        // can disturb it — the same reason the `return` arm does them here.
        self.emit_releases(m, b, ExitKind::Try, at)?;
        self.exit_regions_above(b, 0, false);
        b.ins(&Instruction::Br(self.depth));
        self.depth -= 1;
        b.ins(&Instruction::End);
        // Reusing `bind_payload` costs one local or slot that nothing else reads,
        // and buys the four payload shapes (direct, extended, inline pair, boxed)
        // already being right here because they are right in `match`.
        //
        // The box question is `match`'s, so it is asked in `match`'s words: a
        // `?` IS a switch, the core lowers it as one, and the success arm reads
        // the payload out of the box exactly as an arm binder does. Passing
        // `false` here was the whole of the `?` residue — every successful
        // `parseJson` left the box its `Ok` payload came in.
        let free_box = self.frees_boxes(e, at);
        let place = self.bind_payload(
            b,
            addr,
            &sl,
            std::slice::from_ref(&ok_ty),
            0,
            &ok_ty,
            line,
            free_box,
        )?;
        match place {
            Place::Local(l) => {
                b.ins(&Instruction::LocalGet(l));
            }
            Place::Slot(off) => {
                b.slot(off);
            }
            Place::Static(_) => return unsupported("`?` yielding module state", line),
        }
        Ok(ok_ty)
    }

    /// `?` on a type that implements `Fallible` (RFC-0080 M3), with the operand's
    /// aggregate address already on the stack.
    ///
    /// Same three moves as `try_` above — test, propagate the whole value, read
    /// the success payload — with the first and third answered by impl methods.
    /// The propagation is still the `memory.copy` of the entire aggregate, which
    /// is the claim the milestone exists to execute: a failing variant reaches the
    /// caller intact because nothing looks inside it.
    ///
    /// The value is copied into a frame slot and given a reserved name so the two
    /// calls can be spelled as `Expr::Var` and go through `call` whole, including
    /// its generic path. Passing the raw address instead would mean a second
    /// argument-passing convention beside the one `emit_call` already has.
    fn try_fallible(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        st: &Type,
        line: usize,
        at: usize,
    ) -> Result<Type, String> {
        let key = ftypes::type_key(&self.cx.sub(st))
            .ok_or_else(|| gap(&format!("`?` dispatched on `{st}`"), line))?;
        let Repr::Agg(sl) = self.cx.repr(st, line)? else {
            return unsupported("`?` on a non-aggregate Fallible value", line);
        };
        // Whole-value propagation is only sound if the two sides are the same
        // shape. The checker requires the same type outright here (there is no
        // error half to compare separately), so this is the width check the
        // `memory.copy` below needs rather than a second type rule.
        let ret_ty = self.ret_ty.clone();
        if self.cx.ll(&ret_ty) != self.cx.ll(st) {
            return unsupported(
                &format!("`?` on `{st}` in a function returning `{ret_ty}`"),
                line,
            );
        }
        let addr = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(addr));
        let off = b.alloc(sl.size, sl.align);
        b.slot(off);
        b.ins(&Instruction::LocalGet(addr));
        b.ins(&Instruction::I32Const(sl.size as i32));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        let mark = self.scope.len();
        self.scope
            .push(("@try".to_string(), Place::Slot(off), st.clone()));
        let recv = [Expr::Var {
            name: "@try".to_string(),
            line,
        }];

        self.call(
            m,
            b,
            &ftypes::impl_method_name(ftypes::FALLIBLE, &key, "isSuccess"),
            &recv,
            line,
        )?;
        b.ins(&Instruction::I32Eqz);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        b.ins(&Instruction::LocalGet(
            self.dest.expect("an aggregate return has a destination"),
        ));
        b.slot(off);
        b.ins(&Instruction::I32Const(sl.size as i32));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        // The same two unwinds `?` owes as `return`-minus-the-keyword.
        self.emit_releases(m, b, ExitKind::Try, at)?;
        self.exit_regions_above(b, 0, false);
        b.ins(&Instruction::Br(self.depth));
        self.depth -= 1;
        b.ins(&Instruction::End);

        let out = self.call(
            m,
            b,
            &ftypes::impl_method_name(ftypes::FALLIBLE, &key, "success"),
            &recv,
            line,
        );
        self.scope.truncate(mark);
        out
    }

    /// `Age?(n)` — a validated construction whose refinement answers with a tag
    /// instead of a trap, yielding `Option<Age>` (RFC-0003).
    ///
    /// This is the one flow that deliberately steps AROUND the M2d coercion seam,
    /// and the reason is the whole point of the form: `expr_as(n, Age)` would emit
    /// the validation that aborts. So the argument is evaluated at the refinement's
    /// BASE type and the predicate's own answer becomes the tag. The predicate
    /// itself is stated once, in `predicate_holds`, and this is the one caller
    /// that reads its answer as a value instead of letting it trap — a form the
    /// interpreter runs from the same declaration, because a value the two
    /// disagree about is a diverging `None`.
    fn try_construct(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let decl = self
            .cx
            .types
            .get(name)
            .cloned()
            .ok_or_else(|| gap(&format!("a fallible construction of `{name}`"), line))?;
        if args.len() != 1 {
            return unsupported(&format!("`{name}?` at this arity"), line);
        }
        let ty = Type::option(Type::Named(name.to_string()));
        let Repr::Agg(l) = self.cx.repr(&ty, line)? else {
            return unsupported("a fallible construction of a non-aggregate Option", line);
        };
        let base = decl.base.clone();
        self.expr_as(m, b, &args[0], &base)?;
        // `predicate_holds` parks the value where the `where` clause binds it, so
        // both halves of the answer are in locals before either store.
        let (held, base_v) = match self.cx.repr(&base, line)? {
            Repr::Scalar(v) => (self.predicate_holds(b, &decl, line)?, v),
            // Only the record arm of `emit_validation` binds an aggregate base, and
            // it binds by field — there is no single local to become the payload
            // word. No corpus has one, and a guess here would be a silent `None`.
            _ => {
                return unsupported(
                    &format!("a fallible construction over the aggregate base `{base}`"),
                    line,
                )
            }
        };
        let held = match held {
            Some(h) => h,
            // No `where` clause at all: every value satisfies it, so the tag is a
            // constant and the value still has to be parked to be stored.
            None => {
                let h = b.local(base_v);
                b.ins(&Instruction::LocalSet(h));
                b.ins(&Instruction::I32Const(1));
                h
            }
        };
        let tag = self.scratch(b, ValType::I32, 0);
        b.ins(&Instruction::LocalSet(tag));
        let off = b.alloc(l.size, l.align);
        b.slot(off + l.fields[0]);
        b.ins(&Instruction::LocalGet(tag));
        b.ins(&Instruction::I64ExtendI32U);
        b.ins(&Instruction::I64Store(word8()));
        b.slot(off + l.fields[1]);
        b.ins(&Instruction::LocalGet(held));
        self.encode_word2(b, &base, line)?;
        b.ins(&Instruction::I64Store(word8()));
        for f in &l.fields[2..] {
            b.slot(off + f);
            b.ins(&Instruction::I64Const(0));
            b.ins(&Instruction::I64Store(word8()));
        }
        b.slot(off);
        Ok(ty)
    }

    /// `if let Some(x) = s.tryAt(h)` (RFC-0122): lower an OPTIONAL projection
    /// where it is tested. Answers `false` when the scrutinee is not one, and
    /// the caller keeps the ordinary path.
    #[allow(clippy::too_many_arguments)]
    fn optional_if_let(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        pattern: &Pattern,
        scrutinee: &Expr,
        then_block: &vyrn_frontend::ast::Block,
        else_block: &Option<vyrn_frontend::ast::Block>,
        line: usize,
    ) -> Result<bool, String> {
        let Expr::Call { name, args, .. } = scrutinee else {
            return Ok(false);
        };
        if args.is_empty()
            || self.cx.sigs.contains_key(name.as_str())
            || !self
                .cx
                .impls
                .iter()
                .any(|i| i.places.iter().any(|p| p.name == *name))
        {
            return Ok(false);
        }
        let recv = self.peek(&args[0], line).ok();
        let Some(p) = vyrn_frontend::project::optional_site(
            &self.cx.impls,
            recv.as_ref(),
            name,
            &args[0],
            &args[1..],
            line,
        )?
        else {
            return Ok(false);
        };
        let mark = self.scope.len();
        for s in &p.prologue {
            self.stmt(m, b, s)?;
        }
        self.cond(m, b, &p.miss, line)?;
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        // miss: the else arm (the wasm `if` takes the true edge).
        if let Some(eb) = else_block {
            self.block(m, b, eb)?;
        }
        b.ins(&Instruction::Else);
        // hit: run the hit prologue (RFC-0123 M1), then bind the pattern's
        // binder to the place by a synthetic `let` — no analysis row exists
        // for it, so nothing drop-tracks the alias.
        let inner = self.scope.len();
        for s in &p.hit {
            self.stmt(m, b, s)?;
        }
        if let Pattern::Variant(_, binds) = pattern {
            let bind = &binds[0];
            let synth = Stmt::Let {
                name: bind.clone(),
                mutable: false,
                ty: None,
                value: p.place.clone(),
                line,
            };
            self.stmt(m, b, &synth)?;
        }
        self.block(m, b, then_block)?;
        self.scope.truncate(inner);
        self.depth -= 1;
        b.ins(&Instruction::End);
        self.scope.truncate(mark);
        Ok(true)
    }

    /// Push whether the sum at `addr` is `pat`'s variant.
    ///
    /// `Option`/`Result` carry a one-byte tag; a user enum carries an i64 one.
    /// Shared by `match`, `if let` and `?` because all three are the same probe —
    /// a second spelling of it would be a second chance to read the tag at the
    /// wrong width, which is silent rather than loud.
    fn tag_test(
        &self,
        b: &mut Frame,
        addr: u32,
        sum: &Sum,
        pat: &Pattern,
        line: usize,
    ) -> Result<(), String> {
        b.ins(&Instruction::LocalGet(addr));
        // The refutable-`let` desugar's default arm (RFC-0121): the probe is
        // constant truth — the address read above is discarded, and the one
        // `i32` every caller expects is pushed in its place.
        let Some(tag) = tag_of(sum, pat, line)? else {
            b.ins(&Instruction::Drop);
            b.ins(&Instruction::I32Const(1));
            return Ok(());
        };
        // One tag read for every sum since RFC-0126 §8.11's M4b, where the two
        // built-in ones stopped having a variant list of their own. A second
        // spelling of this probe would be a second chance to read the tag at the
        // wrong width, which is silent rather than loud.
        b.ins(&Instruction::I64Load(word8()));
        b.ins(&Instruction::I64Const(tag as i64));
        b.ins(&Instruction::I64Eq);
        Ok(())
    }

    /// Bind payload `i` of the matched variant out of the sum at `addr`.
    /// `ptys` is the whole variant's payload list, because a payload's slot is
    /// the width of the ones before it (RFC-0126 §8.4).
    #[allow(clippy::too_many_arguments)]
    fn bind_payload(
        &mut self,
        b: &mut Frame,
        addr: u32,
        sl: &Layout,
        ptys: &[Type],
        i: usize,
        t: &Type,
        line: usize,
        free_box: bool,
    ) -> Result<Place, String> {
        let off = sl.fields[self.cx.payload_slot(ptys, i)];
        let kind = self.word2(t)?;
        let ll = self.cx.ll(t);
        Ok(match kind {
            Word::Direct => {
                let l = b.local(ValType::I64);
                b.ins(&Instruction::LocalGet(addr));
                b.ins(&Instruction::I64Load(at(off)));
                b.ins(&Instruction::LocalSet(l));
                Place::Local(l)
            }
            Word::Ext(v) => {
                let l = b.local(v);
                b.ins(&Instruction::LocalGet(addr));
                b.ins(&Instruction::I64Load(at(off)));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::LocalSet(l));
                Place::Local(l)
            }
            Word::Float(v) => {
                let l = b.local(v);
                b.ins(&Instruction::LocalGet(addr));
                b.ins(&Instruction::I64Load(at(off)));
                if v == ValType::F32 {
                    b.ins(&Instruction::I32WrapI64);
                    b.ins(&Instruction::F32ReinterpretI32);
                } else {
                    b.ins(&Instruction::F64ReinterpretI64);
                }
                b.ins(&Instruction::LocalSet(l));
                Place::Local(l)
            }
            // Both words at once, and they are contiguous.
            Word::Inline2 => {
                let slot = b.alloc(16, 8);
                b.slot(slot);
                b.ins(&Instruction::LocalGet(addr));
                b.ins(&Instruction::I32Const(off as i32));
                b.ins(&Instruction::I32Add);
                b.ins(&Instruction::I32Const(16));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                Place::Slot(slot)
            }
            // The word is a heap pointer; the binding gets its own copy, so an
            // arm's value is as independent as every other binding's.
            Word::Boxed => {
                let p = self.scratch(b, ValType::I32, 1);
                b.ins(&Instruction::LocalGet(addr));
                b.ins(&Instruction::I64Load(at(off)));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::LocalSet(p));
                let place = match self.cx.repr(t, line)? {
                    Repr::Scalar(v) => {
                        let l = b.local(v);
                        b.ins(&Instruction::LocalGet(p));
                        b.ins(&load_of(&ll, 0, self.cx.signed(t)));
                        b.ins(&Instruction::LocalSet(l));
                        Place::Local(l)
                    }
                    Repr::Agg(l) => {
                        let slot = b.alloc(l.size, l.align);
                        b.slot(slot);
                        b.ins(&Instruction::LocalGet(p));
                        b.ins(&Instruction::I32Const(l.size as i32));
                        b.ins(&Instruction::MemoryCopy {
                            src_mem: 0,
                            dst_mem: 0,
                        });
                        Place::Slot(slot)
                    }
                    Repr::Unit => return unsupported("a Unit payload", line),
                };
                // The value is out; a consumed scrutinee's box is this
                // construct's to give back (the textual backend's
                // `free_boxes`, RFC-0125 M3 for this backend). It used to be
                // the safe leak every boxed payload was here.
                if free_box {
                    b.ins(&Instruction::LocalGet(p));
                    b.ins(&Instruction::Call(self.cx.rt.free));
                }
                place
            }
        })
    }

    /// Whether a `match`, an `if let` or a `?` at `key` frees the boxes its
    /// binders were read out of: the construct owns the value it switches on,
    /// no drop row walks that value whole after it, the memory is not an
    /// arena's, and this is not a declared `release` destructuring its own
    /// receiver, whose caller walks the boxes.
    ///
    /// The first clause is the CORE's, whole ([`Cx::owns_scrutinee`]). It was
    /// four clauses here — a `consume`, a scrutinee naming no place, a `Map`
    /// lookup, and the core's own take — and the first three read the SOURCE
    /// beside a core row that answered the fourth. The core states all four
    /// as one row now (`St::Switch`'s `owns`), which is why this backend has
    /// no `Expr` left to look at (RFC-0125 §3 M3, the box slice).
    fn frees_boxes(&self, scrutinee: &Expr, key: usize) -> bool {
        use vyrn_frontend::movecheck::place_path;
        let consumed = self.cx.owns_scrutinee(key);
        let own_receiver = self.is_release
            && match scrutinee {
                Expr::Consume { place, .. } => {
                    place_path(place).is_none_or(|(root, _)| root == "self")
                }
                _ => true,
            };
        consumed && !self.releases_whole(key) && !own_receiver
    }

    /// Whether a placed row releases the value at `key` WHOLE — the question
    /// [`Fn_::frees_boxes`] used to ask of the plan's own per-binding table
    /// (RFC-0125 §3 M3, the walk's deletion).
    ///
    /// The two are not the same question. The table says the type of the
    /// value has a release; a ROW says one runs here. While the walk placed a
    /// row at every frame exit the two agreed, and once the take is the
    /// core's own answer they part: a construct that TOOK its scrutinee has
    /// no row, and the boxes its binders came out of are then its to give
    /// back. `releaseacrossexit`'s `overIfLet` is the reading — its `Option`
    /// box outlived the arm that took the payload.
    fn releases_whole(&self, key: usize) -> bool {
        self.placed
            .values()
            .any(|rows| rows.iter().any(|(binding, _)| *binding == key))
    }
}

// ---------------------------------------------------------------------------
// `Map<String, V>` (RFC-0028, RFC-0077 M2l)
// ---------------------------------------------------------------------------

/// A Map is `{ ptr keys, ptr vals, i64 len, i64 cap }` — two parallel growable
/// buffers sharing one length, in first-insertion order. Field 2 is the length
/// where an `Array`'s is field 1, which is the whole reason M2c refused to reach
/// for either shape by position: read as a triple, a Map's `vals` pointer would
/// be its length.
///
/// Everything here works through the header's ADDRESS rather than a snapshot,
/// because an insert may reallocate both buffers and every later read has to see
/// the new ones. That is the opposite of [`Walk`], and deliberately so: an
/// `Array` is snapshotted to match a `for` that grows what it walks, and a Map has
/// no iteration form at all (`m.keys()` hands out a copy).
impl<'p> Fn_<'_, 'p> {
    /// The value type a map literal builds, and the map type it produces.
    ///
    /// The position decides, not the first entry: `["k": [[5], [6, 7]]]` in a
    /// `Map<String, Array<Array<Int64>>>` slot has to store growable arrays, and
    /// a nested literal on its own lowers as a fixed `[N x T]`. Storing it at the
    /// literal's own width and reading it back as a triple is the M2c hazard.
    fn map_lit(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        entries: &[(Expr, Expr)],
        line: usize,
    ) -> Result<Type, String> {
        let (want_k, want) = match self.expect.last().map(|t| self.cx.resolve(t)) {
            Some(Type::Map(k, v)) => (Some(*k), Some(*v)),
            _ => (None, None),
        };
        // A value type that IS an unsolved parameter names no type (see
        // `array_lit`) — the first value answers. The key type comes from the
        // position too, else from the first key (RFC-0117: `String` or
        // `Int64` — the checker made every key the same one).
        let val = match (
            want.filter(|t| !matches!(t, Type::Param(_))),
            entries.first(),
        ) {
            (Some(v), _) => v,
            (None, Some((_, ve))) => self.peek(ve, line)?,
            // An empty literal in no map position at all. `Map<String, Int64>` is
            // what the textual backend defaults to, and the two have to agree
            // because the header is the same 32 bytes either way.
            (None, None) => Type::Int,
        };
        let key_t = match (want_k, entries.first()) {
            (Some(k), _) => k,
            (None, Some((ke, _))) => self.peek(ke, line)?,
            (None, None) => Type::Str,
        };
        let mty = Type::Map(Box::new(key_t.clone()), Box::new(val.clone()));
        let l = self.layout_of(&mty, line)?;
        let off = b.alloc(l.size, l.align);
        b.slot(off);
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32Const(l.size as i32));
        b.ins(&Instruction::MemoryFill(0));
        let hdr = b.local(ValType::I32);
        b.slot(off);
        b.ins(&Instruction::LocalSet(hdr));
        // Written order, so a duplicate key updates in place and keeps its slot —
        // `["usd": 1, "eur": 2, "usd": 3]` is length 2 with `usd` first.
        // A repeated key updates in place, so the value it shadows has no owner
        // left — `["usd": 1, "usd": 3]`. Inside a `region` the arena owns it.
        let drop_old = true;
        for (ke, ve) in entries {
            self.map_set(m, b, hdr, &l, ke, ve, &key_t, &val, drop_old, line)?;
        }
        b.slot(off);
        Ok(mty)
    }

    /// `m[k] = v` — update in place on a hit, append on a miss.
    ///
    /// `hdr` is a local holding the header's address. `drop_old` is rule 4's own
    /// question — may this store release what the slot holds now — answered by
    /// the caller, because a new value that names the map could name the very
    /// bytes this frees. The map takes the value as well as the key, so a hit
    /// that only stored over the old value leaked it.
    /// `m.tallyBytes(w, n)` (RFC-0116): the byte-keyed probe, on this backend
    /// too. `mapFind`'s kind 3 compares the window where it lies, so a hit — the
    /// hot path in a counting loop — builds no String, validates nothing, and
    /// allocates nothing. Only a miss goes through `str_from_bytes` (whose Err
    /// is the trap) and the insert path, where the fresh key is stored, not
    /// copied.
    fn map_tally_bytes(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let mty = self.expr(m, b, &args[0])?;
        let Type::Map(..) = self.cx.resolve(&mty) else {
            return unsupported(&format!("`tallyBytes` on `{mty}`"), line);
        };
        let l = self.layout_of(&mty, line)?;
        let hdr = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(hdr));
        let bytes = Type::Array(Box::new(Type::IntN {
            bits: 8,
            signed: false,
        }));
        let wsrc = b.local(ValType::I32);
        self.expr_as(m, b, &args[1], &bytes)?;
        b.ins(&Instruction::LocalSet(wsrc));
        let al = self.layout_of(&bytes, line)?;
        let (wdata, wlen) = (b.local(ValType::I32), b.local(ValType::I32));
        b.ins(&Instruction::LocalGet(wsrc));
        b.ins(&Instruction::I32Load(word_at(al.fields[0])));
        b.ins(&Instruction::LocalSet(wdata));
        b.ins(&Instruction::LocalGet(wsrc));
        b.ins(&Instruction::I64Load(at(al.fields[1])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::LocalSet(wlen));
        let n = b.local(ValType::I64);
        self.expr_as(m, b, &args[2], &Type::Int)?;
        b.ins(&Instruction::LocalSet(n));
        // One probe, before any key exists: kind 3, the window's length as
        // `klen`, the window's address as the key.
        let idx = b.local(ValType::I32);
        b.ins(&Instruction::I32Const(3));
        b.ins(&Instruction::LocalGet(wlen));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[0])));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[2])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::LocalGet(wdata));
        b.ins(&Instruction::I64ExtendI32U);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[4])));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[3])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::Call(self.cx.rt.map_find));
        b.ins(&Instruction::LocalSet(idx));
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32LtS);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        // miss: NOW the key exists — str_from_bytes, whose Err is the trap.
        let rty = Type::result(Type::Str, Type::Str);
        let rl = layout::of_ll(&self.cx.ll(&rty)).expect("the Result shape");
        let dest = b.alloc(rl.size, rl.align);
        self.str_from_bytes(b, dest, wsrc, &al, line)?;
        b.slot(dest + rl.fields[0]);
        b.ins(&Instruction::I64Load(word8()));
        b.ins(&Instruction::I64Eqz);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        let msg = self.cx.rt.intern(
            m,
            &vyrn_frontend::trap::line(vyrn_frontend::trap::io("tbytes")),
        );
        b.ins(&Instruction::I32Const(msg as i32));
        b.ins(&Instruction::Call(self.cx.rt.trap));
        b.ins(&Instruction::Unreachable);
        self.depth -= 1;
        b.ins(&Instruction::End);
        let k = b.local(ValType::I32);
        b.slot(dest + rl.fields[1]);
        b.ins(&Instruction::I64Load(at(0)));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::LocalSet(k));
        // The insert path — the probe already said the key is absent, and the
        // fresh key is ours, so it is stored, not copied.
        self.map_reserve(b, hdr, &l, 8, MapKey::Str);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[0])));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[2])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::LocalTee(idx));
        b.ins(&Instruction::I32Const(4));
        b.ins(&Instruction::I32Mul);
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalGet(k));
        b.ins(&Instruction::I32Store(word()));
        self.map_put(b, hdr, &l, idx, MapKey::Str);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[2])));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Add);
        b.ins(&Instruction::I64Store(at(l.fields[2])));
        self.map_val_addr(b, hdr, &l, idx, 8);
        b.ins(&Instruction::LocalGet(n));
        b.ins(&Instruction::I64Store(at(0)));
        b.ins(&Instruction::Else);
        // hit: add in place. Nothing was built, so nothing frees.
        self.map_val_addr(b, hdr, &l, idx, 8);
        let vp = b.local(ValType::I32);
        b.ins(&Instruction::LocalTee(vp));
        b.ins(&Instruction::LocalGet(vp));
        b.ins(&Instruction::I64Load(at(0)));
        b.ins(&Instruction::LocalGet(n));
        b.ins(&Instruction::I64Add);
        b.ins(&Instruction::I64Store(at(0)));
        self.depth -= 1;
        b.ins(&Instruction::End);
        b.ins(&Instruction::LocalGet(hdr));
        Ok(mty)
    }

    /// `m.tally(k, n)` (RFC-0116): insert-or-add, ONE probe. The callee never
    /// takes the key — a hit adds in place and touches nothing else, a miss
    /// stores a COPY — so the caller's ownership is the same on both paths.
    fn map_tally(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let mty = self.expr(m, b, &args[0])?;
        let key_t = match self.cx.resolve(&mty) {
            Type::Map(k, _) => *k,
            _ => return unsupported(&format!("`tally` on `{mty}`"), line),
        };
        let mk = self.map_key(&key_t, line)?;
        let l = self.layout_of(&mty, line)?;
        let hdr = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(hdr));
        let k = match mk {
            MapKey::I64 => {
                let k = b.local(ValType::I64);
                self.expr_as(m, b, &args[1], &Type::Int)?;
                b.ins(&Instruction::LocalSet(k));
                k
            }
            MapKey::Pack(_) => {
                let raw = b.local(ValType::I32);
                self.expr_as(m, b, &args[1], &key_t)?;
                b.ins(&Instruction::LocalSet(raw));
                self.pack_key(b, raw, &key_t, line)?
            }
            MapKey::Str => {
                let k = b.local(ValType::I32);
                self.expr_as(m, b, &args[1], &Type::Str)?;
                b.ins(&Instruction::LocalSet(k));
                k
            }
        };
        let n = b.local(ValType::I64);
        self.expr_as(m, b, &args[2], &Type::Int)?;
        b.ins(&Instruction::LocalSet(n));
        let idx = b.local(ValType::I32);
        self.map_scan(b, hdr, &l, k, idx, mk);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32LtS);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        // miss: reserve, append the key, index it, count starts at `n`. An
        // Int64 key is stored by value, a packed user key by its canonical
        // bytes — neither dup'd nor freed (RFC-0117).
        self.map_reserve(b, hdr, &l, 8, mk);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[0])));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[2])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::LocalTee(idx));
        b.ins(&Instruction::I32Const(mk.stride()));
        b.ins(&Instruction::I32Mul);
        b.ins(&Instruction::I32Add);
        match mk {
            MapKey::I64 => {
                b.ins(&Instruction::LocalGet(k));
                b.ins(&Instruction::I64Store(word8()));
            }
            MapKey::Pack(stride) => {
                b.ins(&Instruction::LocalGet(k));
                b.ins(&Instruction::I32Const(stride as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }
            MapKey::Str => {
                b.ins(&Instruction::LocalGet(k));
                self.str_dup(b);
                b.ins(&Instruction::I32Store(word()));
            }
        }
        self.map_put(b, hdr, &l, idx, mk);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[2])));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Add);
        b.ins(&Instruction::I64Store(at(l.fields[2])));
        self.map_val_addr(b, hdr, &l, idx, 8);
        b.ins(&Instruction::LocalGet(n));
        b.ins(&Instruction::I64Store(at(0)));
        b.ins(&Instruction::Else);
        // hit: add in place, free the surplus key.
        self.map_val_addr(b, hdr, &l, idx, 8);
        let vp = b.local(ValType::I32);
        b.ins(&Instruction::LocalTee(vp));
        b.ins(&Instruction::LocalGet(vp));
        b.ins(&Instruction::I64Load(at(0)));
        b.ins(&Instruction::LocalGet(n));
        b.ins(&Instruction::I64Add);
        b.ins(&Instruction::I64Store(at(0)));
        self.depth -= 1;
        b.ins(&Instruction::End);
        b.ins(&Instruction::LocalGet(hdr));
        Ok(mty)
    }

    fn map_set(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        l: &Layout,
        key: &Expr,
        value: &Expr,
        key_t: &Type,
        val: &Type,
        drop_old: bool,
        line: usize,
    ) -> Result<(), String> {
        // An Int64-keyed map (RFC-0117): the key by value, stored by value,
        // never dup'd or freed. A packed user key (M2): canonical bytes in a
        // fixed-stride column, the same nothing-to-free property.
        let mk = self.map_key(key_t, line)?;
        let esz = self.stride(val, line)? as i32;
        let r = self.cx.repr(val, line)?;
        // Key then value, before the scan: the textual backend evaluates both
        // first, and a side-effecting value expression must not run at a
        // different point on the two backends.
        let k = match mk {
            MapKey::I64 => {
                let k = b.local(ValType::I64);
                self.expr_as(m, b, key, &Type::Int)?;
                b.ins(&Instruction::LocalSet(k));
                k
            }
            MapKey::Pack(_) => {
                let raw = b.local(ValType::I32);
                self.expr_as(m, b, key, key_t)?;
                b.ins(&Instruction::LocalSet(raw));
                self.pack_key(b, raw, key_t, line)?
            }
            MapKey::Str => {
                let k = b.local(ValType::I32);
                self.expr_as(m, b, key, &Type::Str)?;
                b.ins(&Instruction::LocalSet(k));
                k
            }
        };
        let v = b.local(match &r {
            Repr::Scalar(t) => *t,
            Repr::Agg(_) => ValType::I32,
            Repr::Unit => return unsupported("a Map of Unit", line),
        });
        self.expr_as(m, b, value, val)?;
        b.ins(&Instruction::LocalSet(v));

        let idx = b.local(ValType::I32);
        self.map_scan(b, hdr, l, k, idx, mk);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32LtS);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        self.map_reserve(b, hdr, l, esz, mk);
        // keys[len] = k, and the new entry's index IS the old length.
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[0])));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[2])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::LocalTee(idx));
        b.ins(&Instruction::I32Const(mk.stride()));
        b.ins(&Instruction::I32Mul);
        b.ins(&Instruction::I32Add);
        match mk {
            MapKey::I64 => {
                b.ins(&Instruction::LocalGet(k));
                b.ins(&Instruction::I64Store(word8()));
            }
            MapKey::Pack(stride) => {
                b.ins(&Instruction::LocalGet(k));
                b.ins(&Instruction::I32Const(stride as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }
            MapKey::Str => {
                b.ins(&Instruction::LocalGet(k));
                b.ins(&Instruction::I32Store(word()));
            }
        }
        // The key is in its slot, so the index can record where. `map_reserve`
        // above grew the bucket array and rebuilt it, so this is the only entry it
        // is missing — and the reason the append stays O(1).
        self.map_put(b, hdr, l, idx, mk);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[2])));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Add);
        b.ins(&Instruction::I64Store(at(l.fields[2])));
        // A hit keeps the key it already has, so this one is surplus — the map
        // takes the key, so the map releases the key it does not keep. The
        // textual backend's `@__vyrn_str_free` call, instruction for
        // instruction. The value it already has is surplus too, once the store
        // below lands on it: no reserve ran on this path, so `vals` is still the
        // buffer that value lives in.
        b.ins(&Instruction::Else);
        if drop_old {
            self.map_val_addr(b, hdr, l, idx, esz);
            let old = b.local(ValType::I32);
            b.ins(&Instruction::LocalSet(old));
            self.rel_entry(m, b, old, val, line)?;
        }
        // Inside a `region` the surplus key came from the arena, which hands it
        // back at the exit — freeing it here would give one block two owners.
        // The same partition `rel_at` draws for a `String`, drawn here too. An
        // Int64 key owns nothing, so it has no surplus to return (RFC-0117).
        if mk == MapKey::Str {
            b.ins(&Instruction::LocalGet(k));
            str_hdr(b);
            b.ins(&Instruction::Call(self.cx.rt.free));
        }
        self.depth -= 1;
        b.ins(&Instruction::End);

        // `vals` is read AFTER the branch, because a reserve replaced it.
        self.map_val_addr(b, hdr, l, idx, esz);
        b.ins(&Instruction::LocalGet(v));
        match &r {
            Repr::Scalar(_) => {
                b.ins(&store_of(&self.cx.ll(val)));
            }
            Repr::Agg(vl) => {
                b.ins(&Instruction::I32Const(vl.size as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }
            Repr::Unit => return unsupported("a Map of Unit", line),
        }
        Ok(())
    }

    /// `mapFind(kind, klen, keys, len, key, idx, cap)` into `idx`. `k` is the
    /// local the key travels in: an `i64` for an Int64-keyed map, else an
    /// `i32` address (a String, or a packed key's slot; RFC-0117), which the
    /// runtime takes as an `i64` too.
    fn map_scan(&mut self, b: &mut Frame, hdr: u32, l: &Layout, k: u32, idx: u32, mk: MapKey) {
        let (kind, klen) = mk.kind();
        b.ins(&Instruction::I32Const(kind));
        b.ins(&Instruction::I32Const(klen));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[0])));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[2])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::LocalGet(k));
        if mk != MapKey::I64 {
            b.ins(&Instruction::I64ExtendI32U);
        }
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[4])));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[3])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::Call(self.cx.rt.map_find));
        b.ins(&Instruction::LocalSet(idx));
    }

    /// `mapPut(kind, klen, keys, idx, cap * 2, i)`: record the entry at
    /// position `idx`, whose key is already in the column.
    fn map_put(&mut self, b: &mut Frame, hdr: u32, l: &Layout, idx: u32, mk: MapKey) {
        let (kind, klen) = mk.kind();
        b.ins(&Instruction::I32Const(kind));
        b.ins(&Instruction::I32Const(klen));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[0])));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[4])));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[3])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::I32Const(2));
        b.ins(&Instruction::I32Mul);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::Call(self.cx.rt.map_put));
    }

    /// Which key family a map runs on (RFC-0117): `String` pointers, `Int64`
    /// values, or packed user keys of a fixed stride (M2).
    fn map_key(&mut self, key_t: &Type, line: usize) -> Result<MapKey, String> {
        Ok(match self.cx.resolve(key_t) {
            Type::Int => MapKey::I64,
            Type::Record(_) | Type::Enum(_) => MapKey::Pack(self.layout_of(key_t, line)?.size),
            _ => MapKey::Str,
        })
    }

    /// RFC-0117 M2: pack a user key into a ZEROED frame slot — the canonical
    /// bytes, so `map_slot_pack`'s byte compare IS field-wise equality. `src`
    /// is a local holding the key value's address; gives a local holding the
    /// slot's.
    fn pack_key(
        &mut self,
        b: &mut Frame,
        src: u32,
        key_t: &Type,
        line: usize,
    ) -> Result<u32, String> {
        let l = self.layout_of(key_t, line)?;
        let off = b.alloc(l.size, l.align);
        let dst = b.local(ValType::I32);
        b.slot(off);
        b.ins(&Instruction::LocalSet(dst));
        b.ins(&Instruction::LocalGet(dst));
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32Const(l.size as i32));
        b.ins(&Instruction::MemoryFill(0));
        self.pack_fields(b, src, dst, 0, key_t, line)?;
        Ok(dst)
    }

    /// One level of the pack: a record copies each field at its own offset
    /// (recursively — a nested record's padding must stay zero); a scalar or
    /// a fieldless enum copies its bytes whole, which carry no padding.
    fn pack_fields(
        &mut self,
        b: &mut Frame,
        src: u32,
        dst: u32,
        off: u32,
        ty: &Type,
        line: usize,
    ) -> Result<(), String> {
        if let Type::Record(fs) = self.cx.resolve(ty) {
            let l = self.layout_of(ty, line)?;
            for (i, f) in fs.iter().enumerate() {
                self.pack_fields(b, src, dst, off + l.fields[i], &f.ty, line)?;
            }
            return Ok(());
        }
        let sz = self.layout_of(ty, line)?.size;
        b.ins(&Instruction::LocalGet(dst));
        if off != 0 {
            b.ins(&Instruction::I32Const(off as i32));
            b.ins(&Instruction::I32Add);
        }
        b.ins(&Instruction::LocalGet(src));
        if off != 0 {
            b.ins(&Instruction::I32Const(off as i32));
            b.ins(&Instruction::I32Add);
        }
        b.ins(&Instruction::I32Const(sz as i32));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        Ok(())
    }

    /// The address of entry `idx`'s value.
    fn map_val_addr(&mut self, b: &mut Frame, hdr: u32, l: &Layout, idx: u32, esz: i32) {
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[1])));
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I32Const(esz));
        b.ins(&Instruction::I32Mul);
        b.ins(&Instruction::I32Add);
    }

    /// `mapReserve(kind, klen, hdr, esz)`: room for one more entry — 0 to 4,
    /// else double, both columns and the index (PLAN-0125-runtime §6 step 5;
    /// the runtime's comment has the shape). The `len + 1 > cap` test stays
    /// here, so an insert that fits pays no call: k-nucleotide is that insert
    /// five million times.
    fn map_reserve(&mut self, b: &mut Frame, hdr: u32, l: &Layout, esz: i32, mk: MapKey) {
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[2])));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Add);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[3])));
        b.ins(&Instruction::I64GtS);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        let (kind, klen) = mk.kind();
        b.ins(&Instruction::I32Const(kind));
        b.ins(&Instruction::I32Const(klen));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Const(esz));
        b.ins(&Instruction::Call(self.cx.rt.map_reserve));
        self.depth -= 1;
        b.ins(&Instruction::End);
    }

    /// `m[k]` — an honest `Option<V>`, never a trap.
    ///
    /// The map's address is already on the stack (`at` evaluated it).
    fn map_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        mty: &Type,
        val: &Type,
        key: &Expr,
        line: usize,
    ) -> Result<Type, String> {
        let key_t = match self.cx.resolve(mty) {
            Type::Map(k, _) => *k,
            _ => Type::Str,
        };
        let mk = self.map_key(&key_t, line)?;
        let l = self.layout_of(mty, line)?;
        let esz = self.stride(val, line)? as i32;
        let hdr = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(hdr));
        let k = match mk {
            MapKey::I64 => {
                let k = b.local(ValType::I64);
                self.expr_as(m, b, key, &Type::Int)?;
                b.ins(&Instruction::LocalSet(k));
                k
            }
            MapKey::Pack(_) => {
                let raw = b.local(ValType::I32);
                self.expr_as(m, b, key, &key_t)?;
                b.ins(&Instruction::LocalSet(raw));
                self.pack_key(b, raw, &key_t, line)?
            }
            MapKey::Str => {
                let k = b.local(ValType::I32);
                self.expr_as(m, b, key, &Type::Str)?;
                b.ins(&Instruction::LocalSet(k));
                k
            }
        };
        let idx = b.local(ValType::I32);
        self.map_scan(b, hdr, &l, k, idx, mk);

        let oty = Type::option(val.clone());
        let Repr::Agg(ol) = self.cx.repr(&oty, line)? else {
            return unsupported("an `Option` that is not an aggregate", line);
        };
        let off = b.alloc(ol.size, ol.align);
        // `None` first, then overwritten on a hit — one destination, no join, and
        // the miss case is exactly the zero header.
        b.slot(off);
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32Const(ol.size as i32));
        b.ins(&Instruction::MemoryFill(0));
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32GeS);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        b.slot(off + ol.fields[0]);
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Store(word8()));
        match self.word2(val)? {
            // Two words already side by side in the value buffer: one copy, and
            // nothing to encode. A `Ref`, or a stored `fn` (RFC-0037).
            Word::Inline2 => {
                b.slot(off + ol.fields[1]);
                self.map_val_addr(b, hdr, &l, idx, esz);
                b.ins(&Instruction::I32Const(16));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }
            // A wider aggregate: the payload word is a pointer to a COPY, because
            // the map's buffer moves on the next insert.
            Word::Boxed if matches!(self.cx.repr(val, line)?, Repr::Agg(_)) => {
                b.slot(off + ol.fields[1]);
                self.map_val_addr(b, hdr, &l, idx, esz);
                self.box_value(b, val, line)?;
                b.ins(&Instruction::I64Store(word8()));
            }
            _ => {
                b.slot(off + ol.fields[1]);
                self.map_val_addr(b, hdr, &l, idx, esz);
                b.ins(&load_of(&self.cx.ll(val), 0, self.cx.signed(val)));
                self.encode_word2(b, val, line)?;
                b.ins(&Instruction::I64Store(word8()));
            }
        }
        self.depth -= 1;
        b.ins(&Instruction::End);
        b.slot(off);
        Ok(oty)
    }

    /// `m.has(k)`, `m.remove(k)` and `m.keys()`.
    fn map_method(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        // `remove` mutates, so it needs the binding rather than a value; the other
        // two read, and read through the same address for one code path.
        let (hdr, mty, owns) = if name == "@remove" {
            let (place, ty) = self.receiver(args, "remove", line)?;
            let hdr = b.local(ValType::I32);
            place
                .addr(b, 0)
                .ok_or_else(|| gap("`remove` on a non-map binding", line))?;
            b.ins(&Instruction::LocalSet(hdr));
            // An entry a `remove` drops is unreachable afterwards whoever owns
            // the map, and nothing aliases it (RFC-0092 M2 made `keys()` copy).
            // A `region` is not asked here either: a `String` key routed into
            // the arena comes back refused, and a `Map<String, Array<Int>>`
            // built inside one holds buffers the arena never had.
            (hdr, ty, true)
        } else {
            let ty = self.expr(m, b, &args[0])?;
            let hdr = b.local(ValType::I32);
            b.ins(&Instruction::LocalSet(hdr));
            (hdr, ty, false)
        };
        let (key_t, val) = match self.cx.resolve(&mty) {
            Type::Map(k, v) => (*k, v),
            _ => return unsupported(&format!("`{name}` on `{mty}`"), line),
        };
        let mk = self.map_key(&key_t, line)?;
        let l = self.layout_of(&mty, line)?;

        if name == "@keys" {
            // A snapshot `Array<K>`: the keys copied into a buffer of their
            // own, so the map may be mutated afterwards without disturbing it.
            // String keys are then dup'd per element (RFC-0092 M2 — an array
            // owns its elements, so a snapshot of the map's own pointers would
            // be freed twice); Int64 keys copy with the buffer (RFC-0117).
            let aty = Type::Array(Box::new(key_t.clone()));
            let al = self.layout_of(&aty, line)?;
            let (len, buf) = (b.local(ValType::I32), b.local(ValType::I32));
            b.ins(&Instruction::LocalGet(hdr));
            b.ins(&Instruction::I64Load(at(l.fields[2])));
            b.ins(&Instruction::I32WrapI64);
            b.ins(&Instruction::LocalSet(len));
            let (kind, klen) = mk.kind();
            b.ins(&Instruction::I32Const(kind));
            b.ins(&Instruction::I32Const(klen));
            b.ins(&Instruction::LocalGet(hdr));
            b.ins(&Instruction::Call(self.cx.rt.map_keys_copy));
            b.ins(&Instruction::LocalSet(buf));
            if mk == MapKey::Str {
                self.copy_each(m, b, buf, len, 4, &Type::Str, line)?;
            }
            let off = b.alloc(al.size, al.align);
            b.slot(off + al.fields[0]);
            b.ins(&Instruction::LocalGet(buf));
            b.ins(&Instruction::I32Store(word()));
            for f in [al.fields[1], al.fields[2]] {
                b.slot(off + f);
                b.ins(&Instruction::LocalGet(len));
                b.ins(&Instruction::I64ExtendI32U);
                b.ins(&Instruction::I64Store(word8()));
            }
            b.slot(off);
            return Ok(aty);
        }

        let k = match mk {
            MapKey::I64 => {
                let k = b.local(ValType::I64);
                self.expr_as(m, b, &args[1], &Type::Int)?;
                b.ins(&Instruction::LocalSet(k));
                k
            }
            MapKey::Pack(_) => {
                let raw = b.local(ValType::I32);
                self.expr_as(m, b, &args[1], &key_t)?;
                b.ins(&Instruction::LocalSet(raw));
                self.pack_key(b, raw, &key_t, line)?
            }
            MapKey::Str => {
                let k = b.local(ValType::I32);
                self.expr_as(m, b, &args[1], &Type::Str)?;
                b.ins(&Instruction::LocalSet(k));
                k
            }
        };
        let idx = b.local(ValType::I32);
        self.map_scan(b, hdr, &l, k, idx, mk);
        let found = b.local(ValType::I32);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32GeS);
        b.ins(&Instruction::LocalSet(found));
        if name == "@remove" {
            // Shift the survivors down, so first-insertion order survives a
            // removal — which is why a remove-then-insert moves a key to the end.
            let esz = self.stride(&val, line)? as i32;
            b.ins(&Instruction::LocalGet(found));
            b.ins(&Instruction::If(BlockType::Empty));
            self.depth += 1;
            // The map took the key and the value, so the map hands both back
            // when the entry goes — BEFORE the shift moves the survivors over
            // the slots they live in. The runtime's `map_remove_at` twin shifts
            // bytes and is handed no types, so this is the only place that can.
            // An Int64 key owns nothing to hand back (RFC-0117).
            if owns {
                let mut cols = vec![(l.fields[1], esz, val.as_ref().clone())];
                if mk == MapKey::Str {
                    cols.insert(0, (l.fields[0], 4i32, Type::Str));
                }
                for (field, stride, ety) in cols {
                    let a = b.local(ValType::I32);
                    b.ins(&Instruction::LocalGet(hdr));
                    b.ins(&Instruction::I32Load(word_at(field)));
                    b.ins(&Instruction::LocalGet(idx));
                    b.ins(&Instruction::I32Const(stride));
                    b.ins(&Instruction::I32Mul);
                    b.ins(&Instruction::I32Add);
                    b.ins(&Instruction::LocalSet(a));
                    self.rel_entry(m, b, a, &ety, line)?;
                }
            }
            // `mapRemoveAt(kind, klen, hdr, esz, i)`: the shift of both
            // columns, the length, and the index rebuilt — every survivor
            // after the hole moved down a slot, so every bucket naming one
            // was off by one.
            let (kind, klen) = mk.kind();
            b.ins(&Instruction::I32Const(kind));
            b.ins(&Instruction::I32Const(klen));
            b.ins(&Instruction::LocalGet(hdr));
            b.ins(&Instruction::I32Const(esz));
            b.ins(&Instruction::LocalGet(idx));
            b.ins(&Instruction::Call(self.cx.rt.map_remove_at));
            self.depth -= 1;
            b.ins(&Instruction::End);
        }
        b.ins(&Instruction::LocalGet(found));
        Ok(Type::Bool)
    }
}

// ---------------------------------------------------------------------------
// `SmallArray<T, N>` (RFC-0056, RFC-0077 M2l)
// ---------------------------------------------------------------------------

/// A `SmallArray<T, N>` is `{ i64 len, i64 cap, ptr data, [N x T] inline }` with
/// TWO live states: `cap == N` is inline (`data` is null and never read) and
/// `cap > N` is spilled. M2c refused it, and the reason was exact — its first
/// field is a length where a growable array's is a pointer, so reading one as a
/// triple compiles, validates, and indexes garbage.
///
/// What made it affordable here is that the hazard is confined to ONE function.
/// Every element access — `a[i]`, `a[i] = v`, `for x in a`, `.length` — goes
/// through [`Walk`], and a `Walk` is a base pointer and a count. So the state
/// branch lives in [`Fn_::walk`] and nothing downstream knows there are two
/// states. Only the four operations that MUTATE the header (`push`, `pop`,
/// `swapRemove`, `toArray`) need their own arms, and only `push` needs the spill.
impl<'p> Fn_<'_, 'p> {
    /// `(len, cap, base)` of the SmallArray whose header is at `hdr`.
    ///
    /// `base` is the inline field's address while `cap == N`, else `data`. This is
    /// the branch RFC-0056 documents as the small-buffer trade-off, and the reason
    /// its benches show a read-heavy loop losing to `Array`.
    fn sa_parts(&mut self, b: &mut Frame, hdr: u32, l: &Layout, n: usize) -> (u32, u32, u32) {
        let (len, cap, base) = (
            b.local(ValType::I64),
            b.local(ValType::I64),
            b.local(ValType::I32),
        );
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[0])));
        b.ins(&Instruction::LocalSet(len));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[1])));
        b.ins(&Instruction::LocalSet(cap));
        b.ins(&Instruction::LocalGet(cap));
        b.ins(&Instruction::I64Const(n as i64));
        b.ins(&Instruction::I64Eq);
        b.ins(&Instruction::If(BlockType::Result(ValType::I32)));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Const(l.fields[3] as i32));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::Else);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[2])));
        b.ins(&Instruction::End);
        b.ins(&Instruction::LocalSet(base));
        (len, cap, base)
    }

    /// A contextual `[a, b, c]` (or `[]`) in a `SmallArray<T, N>` position: the
    /// elements copied into the inline buffer, `cap == N`, `data` null.
    ///
    /// The checker proved `len <= N`, so the copy is unconditional and slots
    /// `len..N` stay whatever the frame held — dead, because `len` bounds every
    /// read.
    fn sa_from_fixed(
        &mut self,
        b: &mut Frame,
        inner: &Type,
        len: usize,
        want: &Type,
        n: usize,
        line: usize,
    ) -> Result<(), String> {
        // Nothing on the stack when `len` is 0: an empty `[]` has no fixed literal
        // to have produced an address, and `array_lit` reaches here directly.
        let src = b.local(ValType::I32);
        if len > 0 {
            b.ins(&Instruction::LocalSet(src));
        }
        let l = self.layout_of(want, line)?;
        let off = b.alloc(l.size, l.align);
        b.slot(off + l.fields[0]);
        b.ins(&Instruction::I64Const(len as i64));
        b.ins(&Instruction::I64Store(word8()));
        b.slot(off + l.fields[1]);
        b.ins(&Instruction::I64Const(n as i64));
        b.ins(&Instruction::I64Store(word8()));
        b.slot(off + l.fields[2]);
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32Store(word()));
        if len > 0 {
            b.slot(off + l.fields[3]);
            b.ins(&Instruction::LocalGet(src));
            b.ins(&Instruction::I32Const(self.extent(inner, len, line)? as i32));
            b.ins(&Instruction::MemoryCopy {
                src_mem: 0,
                dst_mem: 0,
            });
        }
        b.slot(off);
        Ok(())
    }

    /// `xs.push(v)` on a `SmallArray<T, N>` — store into the live buffer, growing
    /// at `len == cap`.
    ///
    /// From inline it allocates `2N` and copies the inline slots out; from a
    /// spilled buffer it doubles. It never un-spills, so a `pop` below `N` stays on
    /// the heap — smallvec semantics, and what the example prints.
    ///
    /// Returns the whole reshaped value, like the `Array` path: the parser turned
    /// the statement into `xs = @push(xs, v)`, so the write-back is an assignment.
    fn sa_push(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        aty: &Type,
        inner: &Type,
        n: usize,
        value: &Expr,
        line: usize,
    ) -> Result<Type, String> {
        let l = self.layout_of(aty, line)?;
        let stride = self.stride(inner, line)? as i32;
        // A fresh copy of the header, because `push` yields a new value and must
        // not write through the one it was handed.
        let src = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(src));
        let off = b.alloc(l.size, l.align);
        b.slot(off);
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::I32Const(l.size as i32));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        let hdr = b.local(ValType::I32);
        b.slot(off);
        b.ins(&Instruction::LocalSet(hdr));

        let (len, cap, base) = self.sa_parts(b, hdr, &l, n);
        let stale = b.local(ValType::I32);
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::LocalSet(stale));
        b.ins(&Instruction::LocalGet(len));
        b.ins(&Instruction::LocalGet(cap));
        b.ins(&Instruction::I64Eq);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        let (nc, nb) = (b.local(ValType::I64), b.local(ValType::I32));
        b.ins(&Instruction::LocalGet(cap));
        b.ins(&Instruction::I64Const(2));
        b.ins(&Instruction::I64Mul);
        b.ins(&Instruction::LocalTee(nc));
        b.ins(&Instruction::I64Const(stride as i64));
        b.ins(&Instruction::I64Mul);
        b.ins(&Instruction::Call(self.cx.rt.malloc));
        b.ins(&Instruction::LocalTee(nb));
        // From the live buffer, whichever it was: this is the one place the two
        // states converge, and `base` already picked.
        b.ins(&Instruction::LocalGet(base));
        b.ins(&Instruction::LocalGet(len));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::I32Const(stride));
        b.ins(&Instruction::I32Mul);
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        // From the inline slots `base` is a frame address, which is below
        // `HEAP_BASE` and which `free` therefore ignores; from a spilled buffer it
        // is the block `realloc` would have released for the textual backend. Held
        // until the element is stored, for `push`'s reason: the value expression
        // may read the array through the caller's header, which still names it.
        b.ins(&Instruction::LocalGet(base));
        b.ins(&Instruction::LocalSet(stale));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::LocalGet(nb));
        b.ins(&Instruction::I32Store(word_at(l.fields[2])));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::LocalGet(nc));
        b.ins(&Instruction::I64Store(at(l.fields[1])));
        b.ins(&Instruction::LocalGet(nb));
        b.ins(&Instruction::LocalSet(base));
        self.depth -= 1;
        b.ins(&Instruction::End);

        let w = Walk {
            data: base,
            len,
            stride: stride as u32,
            elem: inner.clone(),
            byte: false,
        };
        self.elem_addr(b, &w, len);
        let r = self.cx.repr(inner, line)?;
        self.expr_as(m, b, value, inner)?;
        match &r {
            Repr::Scalar(_) => {
                b.ins(&store_of(&self.cx.ll(inner)));
            }
            Repr::Agg(_) => {
                b.ins(&Instruction::I32Const(stride));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            }
            Repr::Unit => return unsupported("a SmallArray of Unit", line),
        }
        b.ins(&Instruction::LocalGet(stale));
        b.ins(&Instruction::Call(self.cx.rt.free));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::LocalGet(len));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Add);
        b.ins(&Instruction::I64Store(at(l.fields[0])));
        b.slot(off);
        Ok(aty.clone())
    }

    /// `xs.pop()`, `xs.swapRemove(i)` and `xs.toArray()` on a `SmallArray`.
    ///
    /// The first two shrink the binding in place through its own header, so they
    /// take the `Place` rather than a value — the same restriction the `Array`
    /// forms have, and the checker's.
    fn sa_method(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[Expr],
        aty: &Type,
        inner: &Type,
        n: usize,
        line: usize,
    ) -> Result<Type, String> {
        let l = self.layout_of(aty, line)?;
        let stride = self.stride(inner, line)? as u32;
        let hdr = b.local(ValType::I32);
        if name == "@toArray" {
            self.expr(m, b, &args[0])?;
        } else {
            let (place, _) = self.receiver(args, name.trim_start_matches('@'), line)?;
            place
                .addr(b, 0)
                .ok_or_else(|| gap(&format!("`{name}` on a non-SmallArray binding"), line))?;
        }
        b.ins(&Instruction::LocalSet(hdr));
        let (len, _cap, base) = self.sa_parts(b, hdr, &l, n);
        let w = Walk {
            data: base,
            len,
            stride,
            elem: inner.clone(),
            byte: false,
        };

        match name {
            // A fresh growable `Array<T>` holding a copy of the live elements —
            // the one explicit conversion RFC-0056 has, and the interpreter's is
            // the identity because both are `Val::Array`.
            //
            // The result is a fresh `Array<T>` and an array owns its elements
            // (RFC-0092 M2), so the words it copies are given their own heap.
            // Before M2 it handed back the receiver's element POINTERS and the
            // census counted it as one of the three view constructors.
            "@toArray" => {
                let want = Type::Array(Box::new(inner.clone()));
                let al = self.layout_of(&want, line)?;
                let buf = b.local(ValType::I32);
                b.ins(&Instruction::LocalGet(len));
                b.ins(&Instruction::I64Const(stride as i64));
                b.ins(&Instruction::I64Mul);
                b.ins(&Instruction::I64Const(1));
                b.ins(&Instruction::I64Add);
                b.ins(&Instruction::Call(self.cx.rt.malloc));
                b.ins(&Instruction::LocalTee(buf));
                b.ins(&Instruction::LocalGet(base));
                b.ins(&Instruction::LocalGet(len));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::I32Const(stride as i32));
                b.ins(&Instruction::I32Mul);
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                let count = b.local(ValType::I32);
                b.ins(&Instruction::LocalGet(len));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::LocalSet(count));
                self.copy_each(m, b, buf, count, stride, inner, line)?;
                let off = b.alloc(al.size, al.align);
                b.slot(off + al.fields[0]);
                b.ins(&Instruction::LocalGet(buf));
                b.ins(&Instruction::I32Store(word()));
                for f in [al.fields[1], al.fields[2]] {
                    b.slot(off + f);
                    b.ins(&Instruction::LocalGet(len));
                    b.ins(&Instruction::I64Store(word8()));
                }
                b.slot(off);
                Ok(want)
            }
            // `Option<T>`: `None` on empty, else the last element with the header
            // shrunk. Never un-spills, exactly like the LLVM path.
            "@pop" => {
                let oty = Type::option(inner.clone());
                let Repr::Agg(ol) = self.cx.repr(&oty, line)? else {
                    return unsupported("an `Option` that is not an aggregate", line);
                };
                let off = b.alloc(ol.size, ol.align);
                b.slot(off);
                b.ins(&Instruction::I32Const(0));
                b.ins(&Instruction::I32Const(ol.size as i32));
                b.ins(&Instruction::MemoryFill(0));
                b.ins(&Instruction::LocalGet(len));
                b.ins(&Instruction::I64Eqz);
                b.ins(&Instruction::I32Eqz);
                b.ins(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                let last = b.local(ValType::I64);
                b.ins(&Instruction::LocalGet(len));
                b.ins(&Instruction::I64Const(1));
                b.ins(&Instruction::I64Sub);
                b.ins(&Instruction::LocalSet(last));
                b.slot(off + ol.fields[0]);
                b.ins(&Instruction::I64Const(1));
                b.ins(&Instruction::I64Store(word8()));
                b.slot(off + ol.fields[1]);
                self.elem_addr(b, &w, last);
                match self.word2(inner)? {
                    Word::Inline2 => {
                        // The payload word IS the address here, so the two-word
                        // copy has to be the destination's, not an encode.
                        return unsupported("a `SmallArray` of two-word values", line);
                    }
                    Word::Boxed if matches!(self.cx.repr(inner, line)?, Repr::Agg(_)) => {
                        self.box_value(b, inner, line)?;
                    }
                    _ => {
                        b.ins(&load_of(&self.cx.ll(inner), 0, self.cx.signed(inner)));
                        self.encode_word2(b, inner, line)?;
                    }
                }
                b.ins(&Instruction::I64Store(word8()));
                b.ins(&Instruction::LocalGet(hdr));
                b.ins(&Instruction::LocalGet(last));
                b.ins(&Instruction::I64Store(at(l.fields[0])));
                self.depth -= 1;
                b.ins(&Instruction::End);
                b.slot(off);
                Ok(oty)
            }
            // The removed element, with the last one moved into its place — for
            // `i == len - 1` those are the same address, and the copy is a no-op.
            _ => {
                self.expr_as(m, b, &args[1], &Type::Int)?;
                let idx = b.local(ValType::I64);
                b.ins(&Instruction::LocalSet(idx));
                self.bounds_check(b, &w, idx, false);
                let r = self.cx.repr(inner, line)?;
                let taken = self.place_for(b, &r, line)?;
                match (taken, &r) {
                    (Place::Local(loc), _) => {
                        self.elem_addr(b, &w, idx);
                        self.load_elem(b, &w, line)?;
                        b.ins(&Instruction::LocalSet(loc));
                    }
                    (Place::Slot(o), Repr::Agg(el)) => {
                        b.slot(o);
                        self.elem_addr(b, &w, idx);
                        b.ins(&Instruction::I32Const(el.size as i32));
                        b.ins(&Instruction::MemoryCopy {
                            src_mem: 0,
                            dst_mem: 0,
                        });
                    }
                    _ => return unsupported("a SmallArray of Unit", line),
                }
                let last = b.local(ValType::I64);
                b.ins(&Instruction::LocalGet(len));
                b.ins(&Instruction::I64Const(1));
                b.ins(&Instruction::I64Sub);
                b.ins(&Instruction::LocalSet(last));
                self.elem_addr(b, &w, idx);
                self.elem_addr(b, &w, last);
                b.ins(&Instruction::I32Const(stride as i32));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
                b.ins(&Instruction::LocalGet(hdr));
                b.ins(&Instruction::LocalGet(last));
                b.ins(&Instruction::I64Store(at(l.fields[0])));
                match taken {
                    Place::Local(loc) => b.ins(&Instruction::LocalGet(loc)),
                    Place::Slot(o) => b.slot(o),
                    Place::Static(_) => return unsupported("a static temporary", line),
                };
                Ok(inner.clone())
            }
        }
    }
}

/// An 8-byte access at a static offset.
fn at(off: u32) -> MemArg {
    MemArg {
        offset: off as u64,
        align: 3,
        memory_index: 0,
    }
}

/// A 4-byte access at a static offset.
fn word_at(off: u32) -> MemArg {
    MemArg {
        offset: off as u64,
        align: 2,
        memory_index: 0,
    }
}

fn word8() -> MemArg {
    MemArg {
        offset: 0,
        align: 3,
        memory_index: 0,
    }
}

/// A Vyrn integer type as this backend has to think about it: a width, a
/// signedness, and the wasm carrier both imply.
///
/// wasm has `i32` and `i64` arithmetic and nothing narrower, so an `Int8` rides
/// an `i32` that has to be put back in range after every operator which could
/// leave it. The invariant kept everywhere is the interpreter's own: a value is
/// **correctly represented** in its carrier — sign-extended when signed,
/// zero-extended when not — which is exactly where `wrap_intn` leaves it in an
/// `i64`. That is what makes [`renorm`] the only place a width is enforced, and
/// lets signedness pick an opcode rather than a fixup.
#[derive(Clone, Copy, PartialEq, Eq)]
struct Num {
    bits: u8,
    signed: bool,
}

impl Num {
    /// `Int` and `Int64` are one type, and it is the default.
    const PLAIN: Num = Num {
        bits: 64,
        signed: true,
    };

    /// The integer type `ty` *is*, or `None` for anything that is not one. Takes
    /// a RESOLVED type, so a validated name has already become its base.
    ///
    /// `vyrn_frontend::validate::width` is the answer, not a second copy of it:
    /// "which types are integers, and how wide" is the fact the `int-narrowing`
    /// row rests on, and the interpreter reads the same one (RFC-0125 §3 M6).
    fn of(ty: &Type) -> Option<Num> {
        vyrn_frontend::validate::width(ty).map(|(bits, signed)| Num { bits, signed })
    }

    /// Whether the carrier is an `i64`. Everything 32 bits and under rides an
    /// `i32`, which is `wasm::abi`'s answer rather than a choice made here.
    fn wide(self) -> bool {
        self.bits == 64
    }
}

/// Put a value back in range after an operator that could have left it.
///
/// A no-op where the carrier IS the width (32 and 64 bits), one instruction
/// otherwise. Called after every wrapping operator rather than only where
/// overflow looks possible, because the invariant is what every other site reads.
fn renorm(b: &mut Frame, n: Num) {
    match (n.bits, n.signed) {
        (8, true) => b.ins(&Instruction::I32Extend8S),
        (16, true) => b.ins(&Instruction::I32Extend16S),
        (8, false) => b
            .ins(&Instruction::I32Const(0xFF))
            .ins(&Instruction::I32And),
        (16, false) => b
            .ins(&Instruction::I32Const(0xFFFF))
            .ins(&Instruction::I32And),
        _ => b,
    };
}

/// Widen an integer to the `i64` the `print`/`toString` runtime takes.
fn widen(b: &mut Frame, n: Num) {
    if !n.wide() {
        b.ins(if n.signed {
            &Instruction::I64ExtendI32S
        } else {
            &Instruction::I64ExtendI32U
        });
    }
}

/// The wasm opcode for `op` at width `n` — the whole sized-int table, in one
/// place, which is what M2e's "the arithmetic is i64-only" note was about.
///
/// The two carriers have structurally identical opcode sets, so the shape of the
/// match is the shape of the fact: the carrier picks `i32` or `i64`, and
/// signedness picks only where wasm has two opcodes (divide, remainder, the
/// orderings, and the right shift). `Eq`/`NotEq` and `+`/`-`/`*` have one each
/// because two's complement makes them signedness-blind.
fn int_op(op: BinOp, n: Num) -> Option<Instruction<'static>> {
    let (w, s) = (n.wide(), n.signed);
    Some(match op {
        BinOp::Add if w => Instruction::I64Add,
        BinOp::Add => Instruction::I32Add,
        BinOp::Sub if w => Instruction::I64Sub,
        BinOp::Sub => Instruction::I32Sub,
        BinOp::Mul if w => Instruction::I64Mul,
        BinOp::Mul => Instruction::I32Mul,
        BinOp::Div => match (w, s) {
            (true, true) => Instruction::I64DivS,
            (true, false) => Instruction::I64DivU,
            (false, true) => Instruction::I32DivS,
            (false, false) => Instruction::I32DivU,
        },
        BinOp::Rem => match (w, s) {
            (true, true) => Instruction::I64RemS,
            (true, false) => Instruction::I64RemU,
            (false, true) => Instruction::I32RemS,
            (false, false) => Instruction::I32RemU,
        },
        BinOp::Eq if w => Instruction::I64Eq,
        BinOp::Eq => Instruction::I32Eq,
        BinOp::NotEq if w => Instruction::I64Ne,
        BinOp::NotEq => Instruction::I32Ne,
        BinOp::Lt => match (w, s) {
            (true, true) => Instruction::I64LtS,
            (true, false) => Instruction::I64LtU,
            (false, true) => Instruction::I32LtS,
            (false, false) => Instruction::I32LtU,
        },
        BinOp::LtEq => match (w, s) {
            (true, true) => Instruction::I64LeS,
            (true, false) => Instruction::I64LeU,
            (false, true) => Instruction::I32LeS,
            (false, false) => Instruction::I32LeU,
        },
        BinOp::Gt => match (w, s) {
            (true, true) => Instruction::I64GtS,
            (true, false) => Instruction::I64GtU,
            (false, true) => Instruction::I32GtS,
            (false, false) => Instruction::I32GtU,
        },
        BinOp::GtEq => match (w, s) {
            (true, true) => Instruction::I64GeS,
            (true, false) => Instruction::I64GeU,
            (false, true) => Instruction::I32GeS,
            (false, false) => Instruction::I32GeU,
        },
        BinOp::BitAnd if w => Instruction::I64And,
        BinOp::BitAnd => Instruction::I32And,
        BinOp::BitOr if w => Instruction::I64Or,
        BinOp::BitOr => Instruction::I32Or,
        BinOp::BitXor if w => Instruction::I64Xor,
        BinOp::BitXor => Instruction::I32Xor,
        BinOp::Shl if w => Instruction::I64Shl,
        BinOp::Shl => Instruction::I32Shl,
        // A signed `>>` is arithmetic and an unsigned one is logical — and both
        // preserve the representation invariant, because shifting a
        // sign-extended value right keeps its sign bits and shifting a masked one
        // keeps its zeroes.
        BinOp::Shr => match (w, s) {
            (true, true) => Instruction::I64ShrS,
            (true, false) => Instruction::I64ShrU,
            (false, true) => Instruction::I32ShrS,
            (false, false) => Instruction::I32ShrU,
        },
        // `&&`, `||` and `=~` are not arithmetic; they were handled before this.
        BinOp::And | BinOp::Or | BinOp::Match => return None,
    })
}

/// The comparison instruction for an `i32`-shaped operand pair.
fn cmp_i32(op: BinOp) -> Option<Instruction<'static>> {
    Some(match op {
        BinOp::Eq => Instruction::I32Eq,
        BinOp::NotEq => Instruction::I32Ne,
        BinOp::Lt => Instruction::I32LtS,
        BinOp::LtEq => Instruction::I32LeS,
        BinOp::Gt => Instruction::I32GtS,
        BinOp::GtEq => Instruction::I32GeS,
        _ => return None,
    })
}

/// The load for a scalar of LLVM shape `ll`, at a static offset.
///
/// The widths come from `llt`'s vocabulary rather than from a guess, and the
/// alignment is the natural one because `layout` placed the field there.
///
/// `signed` is [`Num`]'s invariant crossing a load: `llt` prints `i8` for both
/// `Int8` and `UInt8`, so the bytes in memory do not say how to extend them —
/// which is the same ambiguity the textual backend resolves with a `sext`/`zext`
/// at each use. Here it rides the load, so a caller cannot forget it. It is
/// ignored for every shape whose carrier IS its width, and for a `Bool`, which
/// occupies a byte holding 0 or 1.
fn load_of(ll: &str, off: u32, signed: bool) -> Instruction<'static> {
    let m = |align| MemArg {
        offset: off as u64,
        align,
        memory_index: 0,
    };
    match ll {
        "i64" => Instruction::I64Load(m(3)),
        "double" => Instruction::F64Load(m(3)),
        "float" => Instruction::F32Load(m(2)),
        "i32" | "ptr" => Instruction::I32Load(m(2)),
        "i16" if signed => Instruction::I32Load16S(m(1)),
        "i16" => Instruction::I32Load16U(m(1)),
        "i8" if signed => Instruction::I32Load8S(m(0)),
        // RFC-0083's four spellings, one `v128` — the same collapse `repr`
        // makes, for the same reason: wasm has one vector type and the lane
        // interpretation belongs to the instruction, not to the access. Before
        // this arm they fell through to `i32.load8_u` and a vector in a record
        // was silently truncated to its first BYTE.
        //
        // `align: 0` (a log2 exponent, so one byte) understates on purpose,
        // exactly as the `@f32x4Load` builtin does: the frame is 8-aligned, so
        // nothing guarantees the 16 a `v128.load` would like, and an overstated
        // hint is a validation-legal lie the engine may act on.
        "<4 x float>" | "<4 x i32>" | "<2 x double>" | "<2 x i64>" => Instruction::V128Load(m(0)),
        _ => Instruction::I32Load8U(m(0)),
    }
}

vyrn_frontend::body_scope_descent!(HoistVisit, hoist_block, hoist_stmt, hoist_expr);

/// The one line the hoist writes at a node: hand it to `fe` or `fs`, and stop
/// at a lambda. Nothing here asks what is in scope.
struct Hoist<'e, 's> {
    fe: &'e mut dyn FnMut(&Expr),
    fs: &'s mut dyn FnMut(&Stmt),
}

impl HoistVisit<'_> for Hoist<'_, '_> {
    const SCOPED: bool = false;

    fn stmt(&mut self, s: &Stmt, _: &std::collections::HashSet<String>) {
        (self.fs)(s)
    }

    fn expr(&mut self, e: &Expr, _: &std::collections::HashSet<String>) -> bool {
        (self.fe)(e);
        // A lambda is a leaf: its body is lowered as its own function, so
        // nothing inside it is this loop's to hoist, and `header_invariant`
        // refuses a lambda that so much as mentions the binding.
        !matches!(e, Expr::Lambda { .. })
    }
}

/// Every expression under `e`, pre-order, `e` itself first, and every
/// statement under it through `fs` — `ast::body_scope_descent!`'s descent
/// since RFC-0125 §3 M6, where this file wrote the arms out itself.
fn each_expr(e: &Expr, fe: &mut dyn FnMut(&Expr), fs: &mut dyn FnMut(&Stmt)) {
    hoist_expr(e, &std::collections::HashSet::new(), &mut Hoist { fe, fs });
}

fn each_block(blk: &Block, fe: &mut dyn FnMut(&Expr), fs: &mut dyn FnMut(&Stmt)) {
    hoist_block(
        blk,
        &mut std::collections::HashSet::new(),
        &mut Hoist { fe, fs },
    );
}

/// Could evaluating `e` read the binding `name`, or run code that might
/// (RFC-0125 M1's rule on [`Dest`])? A mention of the binding, or any call —
/// a callee can reach module state, and a `modify` argument is the binding
/// itself. A lambda literal counts as a call: it captures now.
fn observes(e: &Expr, name: &str) -> bool {
    let mut hit = vyrn_frontend::movecheck::mentions_place(e, name);
    each_expr(
        e,
        &mut |x| {
            if matches!(
                x,
                Expr::Call { .. } | Expr::Spawn { .. } | Expr::Lambda { .. } | Expr::Try { .. }
            ) {
                hit = true;
            }
        },
        &mut |_| {},
    );
    hit
}

fn is_var(e: &Expr, name: &str) -> bool {
    matches!(e, Expr::Var { name: n, .. } if n == name)
}

/// Does `p` bind `name`? A binder inside the loop would shadow the hoisted
/// binding, so the hoist is refused.
fn binds(p: &Pattern, name: &str) -> bool {
    match p {
        Pattern::Success(n) | Pattern::Failure(n) => n == name,
        Pattern::Variant(_, ns) => ns.iter().any(|n| n == name),
        Pattern::Other => false,
    }
}

/// The names a `while` indexes: every `@at(name, _)` in its condition or body.
fn indexed_names(cond: &Expr, body: &Block) -> Vec<String> {
    let mut names = Vec::new();
    let mut fe = |e: &Expr| {
        if let Expr::Call { name, args, .. } = e {
            if name == "@at" && args.len() == 2 {
                if let Expr::Var { name: n, .. } = &args[0] {
                    names.push(n.clone());
                }
            }
        }
    };
    each_expr(cond, &mut fe, &mut |_| {});
    each_block(body, &mut fe, &mut |_| {});
    names.sort();
    names.dedup();
    names
}

/// Can nothing in `cond` or `body` move `name`'s header? Conservative, on the
/// syntax alone: an assignment to it, a `let` or a pattern that shadows it, a
/// `drop`, a `consume`, a `for .. in consume` over it, the binding handed
/// whole to any call other than `@at` (a `push` is an assignment; a `pop` and
/// a user method are such calls), or a lambda that mentions it — each refuses.
/// An `@at` read, a `.length` read and an element store are the only things
/// the hoist admits.
fn header_invariant(cond: &Expr, body: &Block, name: &str) -> bool {
    let ok = std::cell::Cell::new(true);
    let mut fe = |e: &Expr| {
        if !ok.get() {
            return;
        }
        let fine = match e {
            Expr::Call { name: f, args, .. }
                if f == "@at" && args.len() == 2 && is_var(&args[0], name) =>
            {
                true
            }
            Expr::Call { args, .. }
            | Expr::Spawn { args, .. }
            | Expr::TryConstruct { args, .. } => !args.iter().any(|a| {
                is_var(a, name) || matches!(a, Expr::Consume { place, .. } if is_var(place, name))
            }),
            Expr::Consume { place, .. } => !is_var(place, name),
            Expr::Lambda { .. } => !vyrn_frontend::movecheck::mentions_place(e, name),
            Expr::Match { arms, .. } => !arms.iter().any(|a| binds(&a.pattern, name)),
            _ => true,
        };
        ok.set(fine);
    };
    let mut fs = |s: &Stmt| {
        if !ok.get() {
            return;
        }
        let fine = match s {
            Stmt::Let { name: n, .. }
            | Stmt::Assign { name: n, .. }
            | Stmt::SetField { name: n, .. }
            | Stmt::Drop { name: n, .. } => n != name,
            Stmt::IfLet { pattern, .. } => !binds(pattern, name),
            Stmt::ForIn {
                var,
                iter,
                consuming,
                ..
            } => var != name && !(*consuming && is_var(iter, name)),
            _ => true,
        };
        ok.set(fine);
    };
    each_expr(cond, &mut fe, &mut fs);
    each_block(body, &mut fe, &mut fs);
    ok.get()
}

fn store_of(ll: &str) -> Instruction<'static> {
    let m = |align| MemArg {
        offset: 0,
        align,
        memory_index: 0,
    };
    match ll {
        "i64" => Instruction::I64Store(m(3)),
        "double" => Instruction::F64Store(m(3)),
        "float" => Instruction::F32Store(m(2)),
        "i32" | "ptr" => Instruction::I32Store(m(2)),
        "i16" => Instruction::I32Store16(m(1)),
        // See [`load_of`] for the collapse and for the understated hint.
        "<4 x float>" | "<4 x i32>" | "<2 x double>" | "<2 x i64>" => Instruction::V128Store(m(0)),
        _ => Instruction::I32Store8(m(0)),
    }
}

// ---------------------------------------------------------------------------
// The emitted runtime
// ---------------------------------------------------------------------------

/// The handful of functions a standalone module needs and has nowhere to get.
///
/// RFC-0076's shim owns `malloc` and the string runtime for its generator
/// artifacts, but `vyrn build --target wasm` produces ONE module with no shim
/// beside it, so these are emitted. All forty of them, whether the program reaches one or not —
/// and then [`wasm::Module::sweep`] (M2p) drops the ones no export reaches, which
/// is why the whole table costs `fib.wasm` 290 bytes of code rather than 4,420.
/// The data each interned on its way past is swept with them (RFC-0125 §3 M4).
/// The runtime functions written in Vyrn (`std/runtime`, PLAN-0125-runtime §6
/// steps 1 and 2): each by the name the module declares it under, with the
/// wasm signature this emitter calls it with.
///
/// The index is reserved before the hand-emitted runtime is written, because
/// that runtime calls these (`trap` calls `strLen`, `env_get` calls `starts`,
/// the fixed-clock preamble calls `strI64`). The body arrives when the
/// module's function is lowered like any other user function, and
/// [`VyrnRt::take`] is where the declared signature and the one spelled here
/// have to agree. [`VyrnRt::check`] refuses a program whose link has no
/// `std/runtime` — a resolver serving a partial std tree — rather than letting
/// `Module::sweep` panic on an unfilled body.
const VYRN_RUNTIME: &[(&str, &[ValType], &[ValType])] = &[
    // Step 2: the allocator. The request is an `i64` for the reason the
    // module's own comment gives (`push` once wrapped `cap * stride` at an
    // `i32` call site), and it is the signature `__vyrn_malloc` exports.
    ("malloc", &[ValType::I64], &[ValType::I32]),
    ("free", &[ValType::I32], &[]),
    // Step 4: the allocating strings. `strFromBytes` returns a
    // `Result<Int64, Int64>`, an aggregate, so the hidden destination leads;
    // the check's answer and the two interned messages are its last three
    // arguments (RFC-0125 §3 M6, the third judgment's fifth slice — the DFA
    // table used to be the first of those).
    ("strNew", &[ValType::I32, ValType::I32], &[ValType::I32]),
    ("strConcat", &[ValType::I32, ValType::I32], &[ValType::I32]),
    (
        "strAppend",
        &[ValType::I32, ValType::I32, ValType::I32],
        &[ValType::I32],
    ),
    ("strFromBytes", &[ValType::I32; 6], &[]),
    ("strLen", &[ValType::I32], &[ValType::I32]),
    ("strCmp", &[ValType::I32, ValType::I32], &[ValType::I32]),
    ("starts", &[ValType::I32, ValType::I32], &[ValType::I32]),
    ("intStr", &[ValType::I64, ValType::I32], &[ValType::I32]),
    // `Option<Int64>` is an aggregate result: the hidden destination leads.
    ("parseI64", &[ValType::I32, ValType::I32], &[]),
    ("strI64", &[ValType::I32], &[ValType::I64]),
    (
        "utf8Valid",
        &[ValType::I32, ValType::I32, ValType::I32],
        &[ValType::I32],
    ),
    (
        "lineAt",
        &[ValType::I32, ValType::I64, ValType::I64],
        &[ValType::I64],
    ),
    (
        "colAt",
        &[ValType::I32, ValType::I64, ValType::I64],
        &[ValType::I64],
    ),
    (
        "regexRun",
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        &[ValType::I32],
    ),
    // Step 5: the maps. `kind` and `klen` lead (see [`MapKey::kind`]); the key
    // of `mapFind` is an `i64` whatever the layout, the value itself for an
    // `Int64` key and an address zero-extended for the rest.
    (
        "mapFind",
        &[
            ValType::I32,
            ValType::I32,
            ValType::I32,
            ValType::I32,
            ValType::I64,
            ValType::I32,
            ValType::I32,
        ],
        &[ValType::I32],
    ),
    ("mapPut", &[ValType::I32; 6], &[]),
    ("mapReserve", &[ValType::I32; 4], &[]),
    ("mapRemoveAt", &[ValType::I32; 5], &[]),
    ("mapKeysCopy", &[ValType::I32; 3], &[ValType::I32]),
    // Step 6: the arrays. `dst` and `src` are header addresses and `stride` the
    // element size; `arrPush` answers the buffer a growth left behind, which
    // the emitter frees after the element is stored.
    ("arrPush", &[ValType::I32; 3], &[ValType::I32]),
    (
        "arrReserve",
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I64],
        &[],
    ),
    ("arrAppend", &[ValType::I32; 4], &[]),
    ("arrCopyFrom", &[ValType::I32; 4], &[]),
    ("arrClear", &[ValType::I32; 2], &[]),
    // Step 7: the I/O family. A `dest` leads where the result is an
    // aggregate (an `Option`, a `Result`, the `args` triple); the interned
    // message halves, the DFA table and the fixed-clock keys trail, as
    // `strFromBytes`'s tail does. The generator-host twins take the host's
    // read mode after the path.
    ("writeAll", &[ValType::I32; 3], &[ValType::I32]),
    ("printStr", &[ValType::I32], &[]),
    ("nowMillis", &[ValType::I32], &[ValType::I64]),
    ("monoNanos", &[ValType::I32], &[ValType::I64]),
    ("randomSeedV", &[ValType::I32], &[ValType::I64]),
    ("argsV", &[ValType::I32], &[]),
    ("readLineV", &[ValType::I32; 2], &[]),
    (
        "openAt",
        &[ValType::I32, ValType::I32, ValType::I64],
        &[ValType::I32],
    ),
    ("readFileV", &[ValType::I32; 9], &[]),
    ("readFileGen", &[ValType::I32; 10], &[]),
    ("readFileBytesV", &[ValType::I32; 4], &[]),
    ("readFileBytesGen", &[ValType::I32; 7], &[]),
    ("writeFileBytesV", &[ValType::I32; 6], &[]),
    ("writeFileV", &[ValType::I32; 5], &[]),
    ("renameFileV", &[ValType::I32; 7], &[]),
    ("fsyncFileV", &[ValType::I32; 4], &[]),
    ("listDirV", &[ValType::I32; 5], &[]),
    ("listDirGen", &[ValType::I32; 5], &[]),
    // Step 9: the two traps and the two renderers. `boolStr` is handed the
    // two interned literals, as `strFromBytes` is handed its two wordings;
    // `trapAt`'s table is `trap::Rule`'s, laid out by `runtime` below.
    ("trapV", &[ValType::I32], &[]),
    ("trapAt", &[ValType::I32, ValType::I64, ValType::I32], &[]),
    ("printI64", &[ValType::I64, ValType::I32], &[]),
    ("boolStr", &[ValType::I32; 3], &[ValType::I32]),
    // Step 8: the region arena. `regionEnter` answers the bump top and
    // `regionExit` puts it back; the nesting counter and its trap stay inline
    // (see [`Fn_::region_enter`]).
    ("regionEnter", &[], &[ValType::I32]),
    ("regionExit", &[ValType::I32], &[]),
];

struct VyrnRt {
    index: HashMap<&'static str, u32>,
    filled: std::collections::HashSet<&'static str>,
}

impl VyrnRt {
    fn reserve(m: &mut Module) -> Self {
        let index = VYRN_RUNTIME
            .iter()
            .map(|(name, params, results)| (*name, m.reserve_func(params, results)))
            .collect();
        VyrnRt {
            index,
            filled: Default::default(),
        }
    }

    fn get(&self, name: &str) -> u32 {
        self.index[name]
    }

    /// The reserved index for `name` when it is one of the table's, after checking
    /// the signature the module declares against the one the emitter calls.
    fn take(
        &mut self,
        name: &str,
        params: &[ValType],
        results: &[ValType],
        line: usize,
    ) -> Result<Option<u32>, String> {
        let Some(short) = name.strip_prefix(vyrn_frontend::loader::RUNTIME_PREFIX) else {
            return Ok(None);
        };
        let Some((key, want_p, want_r)) = VYRN_RUNTIME.iter().find(|(k, ..)| *k == short) else {
            return Ok(None);
        };
        if params != *want_p || results != *want_r {
            return Err(format!(
                "std/runtime.{short} (line {line}) is declared as {params:?} -> {results:?}; \
                 the emitter calls it as {want_p:?} -> {want_r:?}"
            ));
        }
        self.filled.insert(key);
        Ok(Some(self.index[key]))
    }

    fn check(&self) -> Result<(), String> {
        let missing: Vec<&str> = VYRN_RUNTIME
            .iter()
            .map(|(k, ..)| *k)
            .filter(|k| !self.filled.contains(k))
            .collect();
        if missing.is_empty() {
            return Ok(());
        }
        Err(format!(
            "direct backend: `std/runtime` is not linked (no body for {}); the loader \
             injects it from the std root, so the std tree this program was loaded \
             against is missing it",
            missing.join(", ")
        ))
    }
}

#[derive(Clone, Copy, Default)]
struct Rt {
    /// `wasi_snapshot_preview1.proc_exit`, kept here so a lowering outside
    /// `runtime` can end the process: `std/mem`'s `trap` primitive
    /// (PLAN-0125-runtime §2.3) is the write to descriptor 2 and this call.
    proc_exit: u32,
    /// The host-import table, carried so `Fn_::mem_prim` can lower a
    /// `std/mem` import declaration to its `call` (PLAN-0125-runtime §2.2).
    wasi: Wasi,
    /// `std/runtime`'s `writeAll` (PLAN-0125-runtime §6 step 7): the ONE
    /// place bytes leave a program, with the stdout buffer behind it.
    write_all: u32,
    /// The allocator, written in Vyrn since PLAN-0125-runtime §6 step 2: a
    /// segregated free list whose heads and bump offset live in the heap's
    /// first 480 bytes. Reserved by [`VyrnRt`] like the strings below.
    malloc: u32,
    free: u32,
    /// Allocate a `String` buffer: its `{ len, cap }` header, `cap` bytes of
    /// room, and the NUL (RFC-0089 M1a). Returns the address of the BYTES, so
    /// everything downstream still holds an ordinary NUL-terminated pointer.
    /// Vyrn since PLAN-0125-runtime §6 step 4, with `concat`, `str_append`
    /// and `str_from_bytes` below.
    str_new: u32,
    /// Of the ten functions `std/runtime` supplies (PLAN-0125-runtime §6 step
    /// 1), the seven this emitter calls: `strlen`, `strcmp`, `int_str`,
    /// `regex_run`, `parse_i64`, `line_at`, `col_at`. `utf8Valid`, `starts` and
    /// `strI64` are the other three, and no field carries them, because nothing
    /// here calls them: `starts` and `strI64` were reached by the hand-emitted
    /// runtime that §6 deleted, and `utf8Valid` is reached from inside
    /// `std/runtime`. They keep their [`VYRN_RUNTIME`] rows, which is what
    /// reserves their indices; a Vyrn caller finds them through `sigs` like any
    /// other function. Not slots of this table — [`VyrnRt`] reserves them before
    /// `runtime` runs and `runtime` copies the indices in, so every call site
    /// reads one field whichever side wrote the body.
    strlen: u32,
    strcmp: u32,
    /// The address of the UTF-8 DFA table `utf8Valid` walks, interned by
    /// `runtime`; every caller passes it as the third argument.
    utf8d: u32,
    /// `std/runtime`'s `trapV` (PLAN-0125-runtime §6 step 9): the canonical
    /// line on descriptor 2 and exit 1, called from every refusal this
    /// backend emits.
    trap: u32,
    print_str: u32,
    print_i64: u32,
    int_str: u32,
    /// `std/runtime`'s `boolStr`; the two interned literals below go in as its
    /// last two arguments, so the module holds no wording of its own.
    bool_str: u32,
    str_true: u32,
    str_false: u32,
    concat: u32,
    /// Grow a `String` accumulator in place (RFC-0081): `std/runtime`'s
    /// `strAppend`, called at every `s = s + …`; `std/json` alone has six.
    str_append: u32,
    /// `std/runtime`'s `trapAt` and the trap table it indexes (RFC-0125 §2.3):
    /// eight rows of two interned addresses, laid out by `runtime` from
    /// `trap::Rule`. Every check the EMITTER inserts pushes its row's number
    /// and calls this; the module spells none of the eight wordings.
    trap_at: u32,
    trap_table: u32,
    /// `std/runtime`'s `strFromBytes`; its two failure messages, interned by
    /// `runtime` from `io_message`, go in as its last two arguments so the
    /// wording stays `trap.rs`'s. What DECIDES between them is `std/text`'s
    /// `stringFault`, called at the site and passed in (RFC-0125 §3 M6).
    str_from_bytes: u32,
    bnul: u32,
    butf8: u32,
    // RFC-0014's input I/O and RFC-0043's host boundary, `std/runtime`'s since
    // PLAN-0125-runtime §6 step 7, over the host imports `std/mem` declares
    // (M2j served them straight from WASI rather than through the shim — a
    // standalone module has no shim — and the module keeps that route). Each
    // takes what it needs as arguments: the fixed-clock keys, the DFA table,
    // the interned message halves.
    now_millis: u32,
    mono_nanos: u32,
    random_seed: u32,
    args: u32,
    read_line: u32,
    open_at: u32,
    read_file: u32,
    read_file_bytes: u32,
    write_file: u32,
    write_file_bytes: u32,
    rename_file: u32,
    fsync_file: u32,
    /// `listDir` and `listDirKinds` (RFC-0021, RFC-0119): one function, told
    /// which by a constant, over WASI's `fd_readdir` (RFC-0125 §3 M5).
    list_dir: u32,
    /// The three readers under a generation (RFC-0076 M7): the listing and the
    /// bytes come from the loader's resolver through `vyrn_gen.read`, so the
    /// emitter calls the twin when `Cx::gen` is set.
    read_file_gen: u32,
    read_file_bytes_gen: u32,
    list_dir_gen: u32,
    /// RFC-0043's injected clock and seed, `VYRN_FIXED_TIME=` and
    /// `VYRN_FIXED_SEED=`: the env NAME carries its own `=`, so a lookup is one
    /// prefix test. Interned here and handed to the three readers.
    fixed_time: u32,
    fixed_seed: u32,
    /// The canonical I/O wordings (RFC-0014, RFC-0044), each in the two halves
    /// `io_message_parts` splits around the path, interned from `trap.rs`'s
    /// one table and handed to the runtime functions as arguments so the
    /// module never spells one.
    readerr: (u32, u32),
    utf8err: (u32, u32),
    nulerr: (u32, u32),
    writeerr: (u32, u32),
    xdeverr: (u32, u32),
    listerr: (u32, u32),
    /// The `Map` runtime (RFC-0028, RFC-0116, RFC-0117), Vyrn since
    /// PLAN-0125-runtime §6 step 5: one body over the three key layouts, told
    /// which by a `kind` and a `klen` the emitter passes as constants
    /// ([`MapKey::kind`]). `reserve`, `remove_at` and `keys_copy` were inline
    /// at their single sites and are runtime functions now, because the header
    /// is one fixed 32-byte shape the module can read.
    map_find: u32,
    map_put: u32,
    map_reserve: u32,
    map_remove_at: u32,
    map_keys_copy: u32,
    /// The array family (RFC-0011, RFC-0115), Vyrn since PLAN-0125-runtime §6
    /// step 6 and functions for the first time in any engine: each reads the
    /// receiver's triple and writes the rebuilt one into a fresh slot, told
    /// the element stride as a constant. `a[i]` is not among them; the plan's
    /// results block for step 6 has the measurement that keeps it inline.
    arr_push: u32,
    arr_reserve: u32,
    arr_append: u32,
    arr_copy_from: u32,
    arr_clear: u32,
    /// RFC-0046's `=~` (M2m): walk a complete DFA over a NUL-terminated string.
    /// One helper for every pattern in the module, because the pattern is entirely
    /// in the table it is handed — which is the same split the textual backend's
    /// `@__vyrn_regex_run` makes.
    regex_run: u32,
    /// The two builtins RFC-0078 refused to route into Vyrn, for two DIFFERENT
    /// reasons — which is why they are three emitted functions here rather than a
    /// library this backend already compiles.
    ///
    /// `parse` wraps on overflow where `std/num`'s `parseInt64` declines, so the
    /// two are not one function and folding them would be a language change
    /// (RFC-0078 M4a). `lineAt`/`colAt` exist because the obvious loop is
    /// O(offset) and the interpreter memoizes a line-start table a Vyrn library
    /// cannot hold — generators may not touch module state.
    ///
    /// All three are a loop over a buffer, and each has exactly one counterpart to
    /// agree with: `parse` with the interpreter's `parse_int`, the other two with
    /// `__vyrn_line_at`/`__vyrn_col_at` in `toolchain.rs`.
    parse_i64: u32,
    line_at: u32,
    col_at: u32,
    /// `regionEnter() -> mark` and `regionExit(mark)` — the bump arena of
    /// PLAN-0125-runtime §4.3, in `std/runtime`. A `return` out of a region
    /// calls NEITHER: the value it carries out is a block the arena bumped and
    /// belongs to the caller now, so the bump stays where it is.
    region_enter: u32,
    region_exit: u32,
    /// RFC-0004 §4's region nesting counter: four reserved bytes, because the
    /// depth is dynamic (a `region` in a callee nests inside its caller's) and
    /// entering a 65th is a trap the interpreter also takes. Storage rather than a
    /// wasm global for M2f's reason — module state showed that one mechanism in
    /// memory beats two, and `reserve` is that mechanism.
    region_sp: u32,
    /// The call-depth counter (audit A5.3): four reserved bytes holding how many
    /// Vyrn calls are in flight, in the same storage and for the same reason
    /// `region_sp` is. Every named function's prologue bumps it and its one exit
    /// gives it back; past [`vyrn_frontend::trap::CALL_DEPTH_LIMIT`] it traps
    /// with the words the interpreter and the native binary use.
    call_depth: u32,
}

impl Rt {
    /// A string literal's address in the data segment: its `{ len, cap }` header
    /// (RFC-0089 M1a), then the bytes, then the NUL. The address handed back is
    /// the BYTES, so a literal is an ordinary `String` pointer and every C-shaped
    /// consumer still scans for the zero.
    ///
    /// `cap` is all ones — the runtime's word for static, and the same word the
    /// textual backend writes at twice the width. `free` here already refuses
    /// anything below `HEAP_BASE`; this makes the refusal a fact in the value
    /// rather than a fact about the address.
    ///
    /// It was 0 until the audit measured what 0 costs on the backend that DOES
    /// read the capacity to answer this: an empty String built at run time has
    /// capacity 0, so the native free read it as a literal and leaked it. This
    /// backend never had the leak, and it carries the new sentinel anyway —
    /// two answers to "is this a literal" is how the two backends drift.
    ///
    /// Four-byte aligned so the two header words load aligned.
    fn intern(&self, m: &mut Module, s: &str) -> u32 {
        let mut bytes = (s.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend_from_slice(s.as_bytes());
        bytes.push(0);
        m.data(&bytes, 4) + SHDR
    }
}

/// The `{ i32 len, i32 cap }` header in front of every Vyrn `String`
/// (RFC-0089 M1a). Two pointer-sized words, which is eight bytes here and
/// sixteen on the textual backend — one rule, two widths.
///
/// `s.byteLength` is a load off it. `a + b` reads two. RFC-0081's `str_append`
/// used to keep the same pair beside the variable; the header IS that pair now.
/// And an all-ones `cap` marks a data-segment literal, so a drop site knows what
/// it may hand back without knowing where the pointer came from. A capacity of
/// zero is an ordinary empty buffer, and freeing it is the whole of C2.3.
const SHDR: u32 = 8;

/// How many `region` scopes may be open at once — the language's number, not
/// this backend's ([`vyrn_frontend::trap::REGION_MAX`]).
///
/// It was declared here as well, with the same value, and then not used by the
/// comparison a few thousand lines up, which spelled `64` again. Re-exported
/// rather than deleted because the reservations below read better with a short
/// name.
use vyrn_frontend::trap::REGION_MAX;

/// `std/runtime`'s region routing flag, at this offset from `heapBase()`. The
/// only address the module and the emitter both name: the module owns the
/// heap's first 480 bytes (the class heads, the bump offset, and the arena's
/// three words at 468, 472 and 476), and the emitter writes just this one, to
/// say that the `String` it is about to allocate belongs to the open region's
/// arena. See [`Fn_::arena_route`] and `std/runtime.vyrn`, step 8.
const ARENA_ON: u32 = 476;

/// Replace a `String` pointer on the stack with the address of its header.
/// [`word`] then reads `len` and [`cap_at`] reads `cap`.
fn str_hdr(b: &mut Frame) {
    b.ins(&Instruction::I32Const(SHDR as i32));
    b.ins(&Instruction::I32Sub);
}

/// Replace a `String` pointer on the stack with its byte length.
fn str_len(b: &mut Frame) {
    str_hdr(b);
    b.ins(&Instruction::I32Load(word()));
}

fn byte() -> MemArg {
    MemArg {
        offset: 0,
        align: 0,
        memory_index: 0,
    }
}

/// Push `1` when the sum at address local `a` carries variant `tag`, `0`
/// otherwise — the condition an `If` over a sum's tag takes. Since M2 every
/// sum's tag is the enum's `i64` (RFC-0126 §8.4), so this is one test for the
/// built-in sums and the declared ones alike.
fn tag_eq(b: &mut Frame, a: u32, tag: i64) {
    b.ins(&Instruction::LocalGet(a));
    b.ins(&Instruction::I64Load(word8()));
    b.ins(&Instruction::I64Const(tag));
    b.ins(&Instruction::I64Eq);
}

fn word() -> MemArg {
    MemArg {
        offset: 0,
        align: 2,
        memory_index: 0,
    }
}

/// The second word of a String's `{ len, cap }` header. Named because the two
/// halves are addressed from the same base in a dozen places and an offset of 0
/// where 4 was meant is a silent wrong length.
fn cap_at() -> MemArg {
    MemArg {
        offset: 4,
        align: 2,
        memory_index: 0,
    }
}

fn runtime(m: &mut Module, wasi: &Wasi, v: &VyrnRt) -> Rt {
    let proc_exit = wasi.proc_exit;
    // Every field is an index [`VyrnRt`] already reserved, or an address the
    // interning below hands back. This function emits no wasm function of its
    // own, so there is no order to keep and nothing to number.
    let mut rt = Rt::default();
    rt.proc_exit = proc_exit;
    rt.malloc = v.get("malloc");
    rt.free = v.get("free");
    rt.str_new = v.get("strNew");
    rt.concat = v.get("strConcat");
    rt.str_append = v.get("strAppend");
    rt.str_from_bytes = v.get("strFromBytes");
    rt.strlen = v.get("strLen");
    rt.strcmp = v.get("strCmp");
    rt.int_str = v.get("intStr");
    rt.parse_i64 = v.get("parseI64");
    rt.line_at = v.get("lineAt");
    rt.col_at = v.get("colAt");
    rt.regex_run = v.get("regexRun");
    rt.map_find = v.get("mapFind");
    rt.map_put = v.get("mapPut");
    rt.map_reserve = v.get("mapReserve");
    rt.map_remove_at = v.get("mapRemoveAt");
    rt.map_keys_copy = v.get("mapKeysCopy");
    rt.arr_push = v.get("arrPush");
    rt.arr_reserve = v.get("arrReserve");
    rt.arr_append = v.get("arrAppend");
    rt.arr_copy_from = v.get("arrCopyFrom");
    rt.arr_clear = v.get("arrClear");
    rt.trap = v.get("trapV");
    rt.trap_at = v.get("trapAt");
    rt.print_i64 = v.get("printI64");
    rt.bool_str = v.get("boolStr");
    rt.wasi = *wasi;
    rt.write_all = v.get("writeAll");
    rt.print_str = v.get("printStr");
    rt.now_millis = v.get("nowMillis");
    rt.mono_nanos = v.get("monoNanos");
    rt.random_seed = v.get("randomSeedV");
    rt.args = v.get("argsV");
    rt.read_line = v.get("readLineV");
    rt.open_at = v.get("openAt");
    rt.read_file = v.get("readFileV");
    rt.read_file_gen = v.get("readFileGen");
    rt.read_file_bytes = v.get("readFileBytesV");
    rt.read_file_bytes_gen = v.get("readFileBytesGen");
    rt.write_file_bytes = v.get("writeFileBytesV");
    rt.write_file = v.get("writeFileV");
    rt.rename_file = v.get("renameFileV");
    rt.fsync_file = v.get("fsyncFileV");
    rt.list_dir = v.get("listDirV");
    rt.list_dir_gen = v.get("listDirGen");
    rt.fixed_time = rt.intern(m, "VYRN_FIXED_TIME=");
    rt.fixed_seed = rt.intern(m, "VYRN_FIXED_SEED=");
    // The two halves of each canonical I/O message come from
    // `io_message_parts`, i.e. from the same format string the textual backend
    // hands `__vyrn_snprintf`, so there is no second wording to keep in step.
    let msg = |m: &mut Module, rt: &Rt, which: &str| {
        let (pre, post) = crate::io_message_parts(which);
        (rt.intern(m, pre), rt.intern(m, post))
    };
    rt.readerr = msg(m, &rt, "readerr");
    rt.utf8err = msg(m, &rt, "utf8err");
    rt.nulerr = msg(m, &rt, "nulerr");
    rt.writeerr = msg(m, &rt, "writeerr");
    rt.xdeverr = msg(m, &rt, "xdeverr");
    rt.listerr = msg(m, &rt, "listerr");
    rt.region_enter = v.get("regionEnter");
    rt.region_exit = v.get("regionExit");
    rt.str_true = rt.intern(m, "true");
    rt.str_false = rt.intern(m, "false");
    // (The three spellings `{:.6}` gives a non-finite double were interned here
    // for `float_str`. `std/num`'s `f64Str` builds them out of bytes, in Vyrn —
    // RFC-0081 M2.)
    // The trap table — RFC-0125 §2.3, and the eight rows the census of §3 M6
    // sorts into "a check the emitter inserts". Each row is the two halves
    // `trap::Rule` states, interned, and the table is those addresses in row
    // order: the wording before the value, and the wording after it, or zero
    // where the row has no value. Every trap site pushes a row NUMBER now, so
    // the eight `msg_*` fields this replaced are gone and the emitter knows no
    // sentence. `trapAt` reads the row.
    let mut table = Vec::with_capacity(8 * 8);
    for r in vyrn_frontend::trap::Rule::ALL {
        let (pre, post) = r.parts();
        let pre = rt.intern(m, &pre);
        let post = post.map_or(0, |p| rt.intern(m, &p));
        table.extend_from_slice(&pre.to_le_bytes());
        table.extend_from_slice(&post.to_le_bytes());
    }
    rt.trap_table = m.data(&table, 4);
    // RFC-0004 §4. The 64 is the LLVM prelude's fixed region stack, and the
    // interpreter traps at the same depth with the same words precisely so the
    // three engines agree about it; the depth counter itself stays inline (see
    // [`Fn_::region_enter`]).
    rt.region_sp = m.reserve(4, 4);
    // Audit A5.3. The row is built from the constant the prologue compares
    // against, so the number in the message and the number enforced cannot
    // drift apart.
    rt.call_depth = m.reserve(4, 4);

    // Every runtime FUNCTION this backend once wrote is now `std/runtime`'s
    // (PLAN-0125-runtime §6): the allocator at step 2, the strings at steps 1
    // and 4, the maps at 5, the arrays at 6, the I/O family at 7, and at step
    // 9 the last four — `trapV`, `trapAt`, `printI64` and `boolStr`. The call
    // sites reach them through the indices `VyrnRt` reserved. What is left in
    // this file is instruction sequences at their one site each, listed in
    // step 9's results: the prologue's depth counter, the trap site of
    // RFC-0125 M1, the `a[i]` check, the `SmallArray` push and `_start`.

    // (`charcount(s)` was here — ~30 lines of scan for the bytes that are not UTF-8
    // continuation bytes. RFC-0078's census found `charCount` the one builtin with
    // no justification for being one, and `std/text`'s `charCountV` is the same scan
    // written in Vyrn, so this backend has a row it no longer has to lower. It is
    // the first runtime function this table has LOST.)

    // Björn Höhrmann's UTF-8 DFA, the SAME table the textual backend emits
    // (`crate::utf8d_table`). Sharing the bytes is the point: two tables would
    // be two answers to "is this valid UTF-8", free to drift by one entry, and
    // the thing they decide is whether a program traps. The walk over it is
    // `std/runtime`'s `utf8Valid` (PLAN-0125-runtime §6 step 1), which takes
    // the table's address as its third argument; every call below passes it.
    let utf8d = m.data(&crate::utf8d_table(), 1);
    rt.utf8d = utf8d;
    // `str_from_bytes` is `std/runtime`'s `strFromBytes` (PLAN-0125-runtime §6
    // step 4). Its two failure wordings are parity's: an embedded NUL is
    // refused before the UTF-8 check, and both are `io_message`'s. Interned here
    // and handed to every call as arguments, so the module never spells them.
    rt.bnul = rt.intern(m, crate::io_message("bnul"));
    rt.butf8 = rt.intern(m, crate::io_message("butf8"));

    // (`slice` was emitted here — 60 instructions: a signed three-clause bounds
    // test, a continuation-byte probe at each cut point, two interned trap strings
    // and a `memory.copy`. RFC-0079 M3 deleted it along with the interpreter's arm
    // and the textual emitter's branch; `std/strpred`'s `sliceV` is the one range
    // check now — the second removal this table has seen, after `charcount`.)

    // (`float_str` was emitted here — see the note where its 511 lines stood.
    // RFC-0081 M2 routed `%f` to `std/num`'s `f64Str` — the third removal this
    // table has seen, after `charcount` and `slice`.)

    rt
}

/// The two `path_open` arguments `_start` opens a `file(..)` log sink with
/// (RFC-0008), from the `wasi_snapshot_preview1` witx: `oflags::creat |
/// trunc` is `fopen(path, "w")`, and `right::fd_write` is what the sink needs.
/// Every other right and flag is spelled at its one use in `std/runtime`
/// (PLAN-0125-runtime §6 step 7).
const RIGHT_FD_WRITE: i64 = 1 << 6;
const OFLAGS_CREAT_TRUNC: i32 = 1 | 8;

// (`float_str` — 511 lines — stood here: `%f`'s six decimal places computed
// exactly, in base-10^6 limbs, because wasm has no `printf` to defer to. It was
// the one runtime function in this backend that was an algorithm rather than a
// loop, and RFC-0081 M2 replaced it with a call to `std/num`'s `f64Str` — the
// same expansion, written once in Vyrn, where the interpreter's `{:.6}` stays as
// the oracle a differential test compares it against. The measurement that
// bought it: 330 ns hand-written here against 721 ns compiled, and no difference
// a program could observe.)

/// The result type of an RFC-0014/RFC-0044 I/O builtin, or `None` if the name is
/// not one.
///
/// ONE spelling, read by the emitting path (which sizes a destination slot with
/// it) and by [`Fn_::peek`] (which needs the same answer when the call is a
/// branch's value). M2l's rule is that a builtin `call` lowers owes `peek` a row;
/// this is that row and that lowering reading one function, because two
/// spellings of `Result<Bool, String>` are two chances to size a slot one field
/// differently from the value written into it.
/// `listDir`'s type (RFC-0021), in one place so the lowering and [`Fn_::peek`]
/// cannot size a destination slot differently from the value written into it —
/// M2l's rule, and the shape `io_builtin_ty` exists for on the other builtins.
fn gen_list_dir_ty() -> Type {
    Type::result(Type::Array(Box::new(Type::Str)), Type::Str)
}

/// The receivers that have a `.length` or a `.byteLength`, in ONE list.
///
/// Neither name is a field, so both paths that meet one have to know the list:
/// [`Fn_::length_of`] emits the load, and [`Fn_::peek`] answers what the load
/// will produce. Each held its own copy, and the copies drifted — `peek`'s
/// omitted a `Map` and a `SmallArray`, so `match o { Some(m) => m.length, .. }`
/// read as "a field of the non-record type `Map<String, Int64>`" while the
/// same read outside a branch compiled. `base` is already resolved.
///
/// This is `io_builtin_ty`'s rule on a second table: one spelling, two readers.
fn length_ty(field: &str, base: &Type) -> Option<Type> {
    matches!(
        (field, base),
        ("byteLength", Type::Str)
            | (
                "length",
                Type::Array(_) | Type::ArrayN(..) | Type::SmallArray(..) | Type::Map(..)
            )
    )
    .then_some(Type::Int)
}

fn io_builtin_ty(name: &str, argc: usize) -> Option<Type> {
    let str_err = |ok| Type::result(ok, Type::Str);
    Some(match (name, argc) {
        ("args", 0) => Type::Array(Box::new(Type::Str)),
        ("readLine", 0) => Type::option(Type::Str),
        ("readFile", 1) => str_err(Type::Str),
        ("readFileBytes", 1) => str_err(Type::Array(Box::new(Type::IntN {
            bits: 8,
            signed: false,
        }))),
        ("fsyncFile", 1) => str_err(Type::Bool),
        ("writeFile", 2) | ("renameFile", 2) | ("writeFileBytes", 2) => str_err(Type::Bool),
        _ => return None,
    })
}

fn expr_name(e: &Expr) -> String {
    match e {
        Expr::Match { .. } => "`match`",
        Expr::Try { .. } => "`?`",
        Expr::TryConstruct { .. } => "a fallible construction",
        Expr::ArrayLit { .. } => "an array literal",
        Expr::MapLit { .. } => "a map literal",
        Expr::Spawn { .. } => "`spawn`",
        Expr::Lambda { .. } => "a lambda",
        _ => "this expression",
    }
    .to_string()
}

/// The forms of the AST dispatch whose arm the per-statement unit could retire
/// — RFC-0125 §3 M3, the interleave slice.
///
/// An arm goes when nothing reaches it. Against each name the count below says
/// how many occurrences the ARM emitted and how many the core's rows did, so a
/// form whose first number is zero over the whole corpus is one whose arm has
/// no reader left.
pub const FORMS: [&str; 20] = [
    "Stmt::Let",
    "Stmt::Assign",
    "Stmt::Return",
    "Stmt::If",
    "Stmt::Expr",
    "Stmt::While",
    "Stmt::ForIn",
    "Stmt::Break",
    "Stmt::Continue",
    "Stmt::IfLet",
    "Stmt::Drop",
    "a statement of another form",
    "Expr::Int",
    "Expr::Byte",
    "Expr::Bool",
    "Expr::Float",
    "Expr::Str",
    "Expr::Var",
    "Expr::Unary",
    "Expr::Binary",
];

thread_local! {
    /// Per [`FORMS`] entry: how many occurrences the AST arm emitted, and how
    /// many the core's rows did.
    static COUNTS: std::cell::RefCell<[(usize, usize); FORMS.len()]> =
        const { std::cell::RefCell::new([(0, 0); FORMS.len()]) };
}

/// How many occurrences of each of [`FORMS`] the AST arm emitted, and how many
/// the core's rows did, since [`forget_walks`].
pub fn forms() -> [(usize, usize); FORMS.len()] {
    COUNTS.with(|c| *c.borrow())
}

fn count(form: usize, carried: bool) {
    COUNTS.with(|c| {
        let mut c = c.borrow_mut();
        if carried {
            c[form].1 += 1;
        } else {
            c[form].0 += 1;
        }
    });
}

/// Which of [`FORMS`] a statement is.
fn stmt_form(s: &Stmt) -> usize {
    match s {
        Stmt::Let { .. } => 0,
        Stmt::Assign { .. } => 1,
        Stmt::Return { .. } => 2,
        Stmt::If { .. } => 3,
        Stmt::Expr(_) => 4,
        Stmt::While { .. } => 5,
        Stmt::ForIn { .. } => 6,
        Stmt::Break { .. } => 7,
        Stmt::Continue { .. } => 8,
        Stmt::IfLet { .. } => 9,
        Stmt::Drop { .. } => 10,
        _ => 11,
    }
}

/// Which of [`FORMS`] an expression is, for the eight the driver reads.
fn expr_form(e: &Expr) -> Option<usize> {
    Some(match e {
        Expr::Int(_) => 12,
        Expr::Byte(_) => 13,
        Expr::Bool(_) => 14,
        Expr::Float(_) => 15,
        Expr::Str(_) => 16,
        Expr::Var { .. } => 17,
        Expr::Unary { .. } => 18,
        Expr::Binary { .. } => 19,
        _ => return None,
    })
}

/// Whether the AST walk is asked for even where the core's rows carry the body
/// — RFC-0125 §3 M3, the driver slice.
///
/// The two walks are compared by emitting the same corpus twice, so the switch
/// exists for the comparison and for nothing else: a build that sets it gets
/// the emission it had before the driver landed.
fn core_walk_off() -> bool {
    std::env::var_os("VYRN_NO_CORE_WALK").is_some()
}

thread_local! {
    /// How many bodies this thread emitted from the core's statements, and how
    /// many it emitted at all — RFC-0125 §3 M3, the driver slice's own count.
    ///
    /// The count is the measurement §2.3 is judged by: it rises as the core
    /// carries more rows, and every body it does not hold is a line of the
    /// residue table with the row it waits on.
    static WALKS: std::cell::Cell<(usize, usize)> = const { std::cell::Cell::new((0, 0)) };
}

/// How many bodies came from the core, and how many were emitted, since
/// [`forget_walks`].
pub fn walks() -> (usize, usize) {
    WALKS.with(std::cell::Cell::get)
}

/// Start the count again.
pub fn forget_walks() {
    WALKS.with(|w| w.set((0, 0)));
    COUNTS.with(|c| *c.borrow_mut() = [(0, 0); FORMS.len()]);
}

/// Where the core's names live while one body is walked, and what the wasm
/// operand stack is holding — RFC-0125 §3 M3, the driver slice.
///
/// The core names EVERY value (§2.1) and wasm has an operand stack, so a walk
/// that gave each name a local would emit a `local.set`/`local.get` pair the
/// AST walk does not. `held` is the one name the stack is carrying: a value
/// bound by the statement just walked and read by this one. Where a name is
/// not stack-shaped it gets a local, in the order the AST walk allocates one.
#[derive(Default)]
struct Walked {
    /// The wasm place of each of the core's names, by [`vyrn_lower::core::Name`].
    at: Vec<Option<(Place, Type)>>,
    /// How many times each name is READ over the whole body.
    reads: Vec<u32>,
    /// The name the operand stack is holding, if any.
    held: Option<vyrn_lower::core::Name>,
}

impl<'p> Fn_<'_, 'p> {
    /// One function body, emitted from the core's own statements — RFC-0125
    /// §2.3: "the emitter reads the core and writes wasm ... it decides
    /// nothing".
    ///
    /// This is the walk the AST dispatch (`Fn_::stmt`, `Fn_::expr`) is beside.
    /// It reads [`vyrn_lower::core::Body`] and nothing else: a statement is a
    /// [`St`], what it computes is the [`Op`], the [`Ctor`] and the [`Lit`] the
    /// operation slice put on the rows, and the type of every operand is the
    /// checker's, carried on [`vyrn_lower::core::NameInfo`].
    ///
    /// It runs only where [`Fn_::core_walkable`] says the rows carry the whole
    /// body. What that leaves out is the ranked list in §3 M3 and not a
    /// judgement of this walk's: each form it stands down at names the row the
    /// core still lacks.
    fn core_body(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
    ) -> Result<(), String> {
        if body.params.len() > self.scope.len() {
            return unsupported("a core body whose parameters are not the frame's", 0);
        }
        let mut w = std::mem::take(&mut self.core_w);
        let r = self.core_stmts(m, b, body, &mut w, &body.stmts);
        self.core_w = w;
        r
    }

    /// One SOURCE statement, emitted from the core's rows where they carry it —
    /// RFC-0125 §3 M3, the interleave slice.
    ///
    /// [`Fn_::core_body`] picks its walk per FUNCTION, so an arm of the AST
    /// dispatch could only go when every body of the corpus went through the
    /// core. This is the same walk asked one statement at a time: where the
    /// rows carry the statement it is emitted from them, and where they do not
    /// the arm below emits it, into the same frame with the same locals and
    /// the same scope. An arm goes when no occurrence of its form reaches it.
    ///
    /// Returns whether the statement was emitted. The screen
    /// ([`Fn_::core_run`]) stands before the first instruction, so a `false`
    /// costs nothing and the arm emits exactly what it always did.
    fn core_took(&mut self, m: &mut Module, b: &mut Frame, s: &Stmt) -> Result<bool, String> {
        let Some(body) = self.core.clone() else {
            return Ok(false);
        };
        let Some(run) = self.core_run(&body, s) else {
            return Ok(false);
        };
        let mut w = std::mem::take(&mut self.core_w);
        let r = self.core_stmts(m, b, &body, &mut w, &run);
        self.core_w = w;
        count(stmt_form(s), true);
        r.map(|()| true)
    }

    /// The core's rows for one source statement, where this walk reads all of
    /// them — RFC-0125 §3 M3, the interleave slice's screen.
    ///
    /// Three clauses, and each one is a line of the record. The FRAME clause: a
    /// placed release and an aggregate destination are emissions the rows do
    /// not carry. The SCALAR clause, per statement rather than per body: every
    /// name the run names is one this walk reads. The STATEMENT screen:
    /// [`Fn_::core_readable`], unchanged.
    fn core_run(&self, body: &vyrn_lower::core::Body, s: &Stmt) -> Option<Vec<St>> {
        // A node is an ADDRESS, and `project::iterate_loop`'s copy of a loop
        // body gives a statement a second one. `key_of` is the mapping back,
        // and ten other readers of the plan in this file ask through it.
        let at = self.cx.plan.key_of(s as *const Stmt as usize);
        let run = self.core_at.get(&at)?;
        // THE FRAME CLAUSE, per statement. It was per BODY until the release
        // half of it moved here, and then it refused any statement the
        // placement keyed a release AT — 8,362 of them per body, and the `if`s
        // of every frame that placed one. Since the release slice it names the
        // three exits this walk gives back at itself: a `break`, a `continue`
        // and a `return` each run the same [`Fn_::emit_releases`] the arm runs,
        // keyed by the row's own site. A block's fall-through release and a
        // scrutinee's stay the arm's, and their rows are what
        // [`Fn_::core_readable`] refuses, so no run reaches this walk holding
        // one.
        if self
            .placed
            .keys()
            .any(|(kind, node)| *node == at && !CORE_EXITS.contains(kind))
        {
            return None;
        }
        // An aggregate result travels through `dest` and a stream cursor is a
        // release at a function exit that no plan row names, so both belong to
        // a `return` and to nothing else: `streamlazy.vyrn` lost 51 bytes of a
        // cursor when the rows took its `return`. The clause is about the RUN
        // rather than about the form, because a subtree carries the `return` of
        // every branch under it.
        if (self.dest.is_some() || !self.cursors.is_empty()) && run.iter().any(core_returns) {
            return None;
        }
        // A node is an ADDRESS. The row's FORM and the name it binds are
        // checked against the statement's, so a row is never read as a
        // statement it did not come from.
        let named =
            |n: &vyrn_lower::core::Name, name: &String| &body.names[*n as usize].source == name;
        let mut annotated = None;
        match (s, run.last()?) {
            (Stmt::Let { name, ty, .. }, St::Let(n, _)) if named(n, name) => {
                // The arm binds the ANNOTATION where the reader wrote one and
                // the recorded type of the initializer otherwise, and the two
                // walks have to bind the same type or they pick different
                // instructions for it: `simd.vyrn`'s `a / b` on two `Float32`
                // widens to `Float64` in the arm and stays single in the row.
                if let Some(t) = ty {
                    // As WRITTEN, not resolved: `let a: Age = 25` is a `where`
                    // type, the arm parks the value in a temporary and calls
                    // its check, and `Age` resolved to `Int64` is the flow that
                    // does not (M2d). The row states no check.
                    if !core_scalar(t)
                        || self.cx.resolve(t) != self.cx.resolve(&body.names[*n as usize].ty)
                    {
                        return None;
                    }
                    annotated = Some(*n);
                }
            }
            (
                Stmt::Assign { name, .. },
                St::Store {
                    place: vyrn_lower::core::Place::Name(n),
                    ..
                },
            ) if named(n, name) => {}
            // The DECLARED return type, for the same reason: a function
            // returning `Age` validates at its `return` and the row does not.
            (Stmt::Return { line, .. }, St::Return { line: at, .. })
                if line == at && core_scalar(&self.ret_ty) => {}
            // RFC-0114 Rule N's edge releases are the plan's rows at the JOIN,
            // and the core states them as drops inside the branch — which the
            // statement screen refuses. An `if` that owes one is the arm's.
            (Stmt::If { .. }, St::If { .. }) if self.cx.edge_rows(at).is_empty() => {}
            // The four forms the site slice took off the floor. Each names its
            // own node on the row now, so the FORM is the whole of the
            // agreement: no binding to check and no type the arm would bind.
            // What each still waits on is the statement screen below — a `for`
            // reads its element through a place row, an `if let` is a switch,
            // and the tag on `Arm` is the list's row 6.
            (Stmt::Expr(_), St::Do { .. }) => {}
            (Stmt::Break { .. }, St::Break { .. }) => {}
            (Stmt::Continue { .. }, St::Continue { .. }) => {}
            (Stmt::While { .. } | Stmt::ForIn { .. }, St::Loop { .. }) => {}
            (Stmt::IfLet { .. }, St::Switch { .. }) => {}
            _ => return None,
        }
        // Every OTHER binding of the run: the row types it by its destination
        // and the arm by what it evaluated, and the two are not always the
        // same. `simd.vyrn`'s `let neg = 0.0 - o` is `Float32` to the checker
        // and to the row, and `Float64` to the arm, which reads the literal's
        // own width and promotes `o` to meet it — so the row divides single
        // where the arm promotes and divides double. Where the two disagree
        // the rows do not carry the statement, and the disagreement is a
        // finding of its own.
        let mut lets = Vec::new();
        for st in run {
            core_lets(st, &mut lets);
        }
        for (n, rhs) in &lets {
            if Some(*n) == annotated {
                continue;
            }
            let info = &body.names[*n as usize];
            let want = self.core_arm_ty(body, rhs)?;
            if !core_scalar(&want) {
                return None;
            }
            let got = self.cx.resolve(&info.ty);
            // A truth value is the one result an operator states and its
            // operands do not.
            if got != self.cx.resolve(&want) && got != Type::Bool {
                return None;
            }
        }
        let mut names = Vec::new();
        for st in run {
            // A release row names a binding the PLACEMENT holds, and the
            // emitter reads its place off `rel_slots` rather than off this
            // walk's own table. So the name is not one this walk has to read,
            // and the scalar clause below is not asked about it — a released
            // name is a String or an array by definition, and asking would
            // refuse every run that carries one.
            if matches!(st, St::Row { .. }) {
                continue;
            }
            vyrn_lower::core::names_in(st, &mut names);
        }
        for n in &names {
            let info = &body.names[*n as usize];
            if !core_scalar(&info.ty) {
                return None;
            }
        }
        // Every name the run READS has to have a place before the first
        // instruction is written: one this walk bound, one the arm below bound
        // (a local of the scope), or a parameter. What the run binds is the
        // same walk the type clause above already did, branches and loop bodies
        // included — a top-level reading of it left every name a `while` binds
        // inside its own body with no place, which is why that form stood at
        // zero until the site slice asked the question once.
        for n in &names {
            if lets.iter().any(|(b, _)| b == n) {
                continue;
            }
            let (_, ty) = self.core_place(&self.core_w, body, *n)?;
            // And the two walks have to agree about the type of a name they
            // share: `stringops.vyrn` compared two bytes at byte width from the
            // row and at `Int64` from the frame, for the same source. The
            // frame's answer is as DECLARED, so a `where` type is refused here
            // as it is at a `let`.
            if !core_scalar(&ty)
                || self.cx.resolve(&ty) != self.cx.resolve(&body.names[*n as usize].ty)
            {
                return None;
            }
        }
        self.core_readable(body, run, &self.core_w.reads)
            .then(|| run.clone())
    }

    /// Where one of the core's names lives: the place this walk bound it at, or
    /// the one the AST arm bound it at, which is the scope's.
    fn core_place(
        &self,
        w: &Walked,
        body: &vyrn_lower::core::Body,
        n: vyrn_lower::core::Name,
    ) -> Option<(Place, Type)> {
        if let Some(p) = w.at[n as usize].clone() {
            return Some(p);
        }
        let info = &body.names[n as usize];
        // `@t3` is the naming pass's own spelling for a temporary, and no
        // reader wrote one — so a name that starts with it is this walk's or
        // nobody's.
        if info.source.starts_with('@') {
            return None;
        }
        let p = self.lookup(&info.source, info.line).ok()?;
        matches!(p.0, Place::Local(_)).then_some(p)
    }

    fn core_stmts(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        ss: &[St],
    ) -> Result<(), String> {
        for (i, s) in ss.iter().enumerate() {
            match s {
                St::Let(n, rhs) => {
                    let info = &body.names[*n as usize];
                    let line = info.line;
                    self.core_rhs(m, b, body, w, rhs, &info.ty, line)?;
                    // A value the next statement reads once, first, and
                    // nothing else reads: wasm's operand stack is where it
                    // lives, and no local is taken for it. Every other name
                    // gets one, in the order the AST walk takes them.
                    if info.binding.is_none()
                        && w.reads[*n as usize] == 1
                        && ss.get(i + 1).and_then(first_read) == Some(*n)
                    {
                        w.held = Some(*n);
                        continue;
                    }
                    let r = self.cx.repr(&info.ty, line)?;
                    let place = self.place_for(b, &r, line)?;
                    let Place::Local(l) = place else {
                        return unsupported("a core `let` of an aggregate", line);
                    };
                    b.ins(&Instruction::LocalSet(l));
                    w.at[*n as usize] = Some((place, info.ty.clone()));
                    // A binding the READER wrote goes on the scope too, so a
                    // statement the AST arm emits after this one finds it
                    // exactly where that arm would have put it (RFC-0125 §3
                    // M3, the interleave slice).
                    if !info.source.starts_with('@') {
                        self.scope
                            .push((info.source.clone(), place, info.ty.clone()));
                    }
                }
                St::Store {
                    place: vyrn_lower::core::Place::Name(n),
                    value,
                    line,
                    ..
                } => {
                    let Some((Place::Local(l), ty)) = self.core_place(w, body, *n) else {
                        return unsupported("a core store into a place with no local", *line);
                    };
                    self.core_val(m, b, body, w, value, &ty, *line)?;
                    b.ins(&Instruction::LocalSet(l));
                }
                // A LOOP'S EXIT, which the core states and wasm has one
                // instruction for. The pass makes up exactly one `break`
                // (`site: 0`, "a break this pass made up") and puts it in the
                // else arm of a two-way branch it also made up, at the head
                // of the loop it desugared: that row is a conditional branch
                // out of the loop's block and nothing else. The reader's own
                // `if c { break }` carries the reader's site and is emitted
                // as the two-way branch it is, so this reads the row rather
                // than recognizing a shape.
                St::If {
                    cond,
                    then,
                    els,
                    site: 0,
                } if then.is_empty()
                    && matches!(els.as_slice(), [St::Break { site: 0 }])
                    && !self.loops.is_empty() =>
                {
                    self.core_val(m, b, body, w, cond, &Type::Bool, 0)?;
                    b.ins(&Instruction::I32Eqz);
                    let brk = self.loops.last().expect("a loop is open").0;
                    let out = self.br_to(brk);
                    b.ins(&Instruction::BrIf(out));
                }
                // `block { loop { .. br 0 } }` — the block is where a `break`
                // goes and the loop is where a `continue` goes, which is the
                // pair `St::Break` and `St::Continue` name. `St::Loop` is the
                // infinite loop the row states, so the back edge is this
                // walk's and unconditional.
                St::Loop { body: inner, .. } => {
                    let brk = self.depth;
                    b.ins(&Instruction::Block(BlockType::Empty));
                    self.depth += 1;
                    let cont = self.depth;
                    b.ins(&Instruction::Loop(BlockType::Empty));
                    self.depth += 1;
                    self.loops.push((brk, cont, self.region_depth));
                    let scope = self.scope.len();
                    let r = self.core_stmts(m, b, body, w, inner);
                    self.scope.truncate(scope);
                    self.loops.pop();
                    r?;
                    let back = self.br_to(cont);
                    b.ins(&Instruction::Br(back));
                    self.depth -= 1;
                    b.ins(&Instruction::End);
                    self.depth -= 1;
                    b.ins(&Instruction::End);
                }
                St::Break { site } | St::Continue { site } => {
                    let Some(&(brk, cont, regions)) = self.loops.last() else {
                        return unsupported("a core exit outside a loop", 0);
                    };
                    let kind = match s {
                        St::Break { .. } => ExitKind::Break,
                        _ => ExitKind::Continue,
                    };
                    // Every frame the loop body opened, before the branch, as
                    // the AST arm does.
                    self.emit_releases(m, b, kind, *site)?;
                    self.exit_regions_above(b, regions, true);
                    let to = match s {
                        St::Break { .. } => brk,
                        _ => cont,
                    };
                    let d = self.br_to(to);
                    b.ins(&Instruction::Br(d));
                }
                St::If {
                    cond, then, els, ..
                } => {
                    self.core_val(m, b, body, w, cond, &Type::Bool, 0)?;
                    b.ins(&Instruction::If(BlockType::Empty));
                    self.depth += 1;
                    let mark = self.scope.len();
                    self.core_stmts(m, b, body, w, then)?;
                    self.scope.truncate(mark);
                    if !els.is_empty() {
                        b.ins(&Instruction::Else);
                        self.core_stmts(m, b, body, w, els)?;
                        self.scope.truncate(mark);
                    }
                    self.depth -= 1;
                    b.ins(&Instruction::End);
                }
                // `region { .. }` (RFC-0004 §4) is the same block with an arena
                // scope around it, which is the row the driver slice added:
                // this walk emitted neither the mark nor the hand-back while
                // the two blocks were one row, and 65 nested regions ran
                // without reaching their limit. The DEPTH is still counted
                // here, because it is a fact about the code being written.
                St::Block {
                    body: inner,
                    region,
                    ..
                } => {
                    let scope = self.scope.len();
                    if *region {
                        self.region_enter(b);
                        self.region_depth += 1;
                        let r = self.core_stmts(m, b, body, w, inner);
                        self.region_depth -= 1;
                        let mark = self.region_marks.pop().expect("one mark per open region");
                        r?;
                        self.region_exit(b, mark);
                    } else {
                        self.core_stmts(m, b, body, w, inner)?;
                    }
                    self.scope.truncate(scope);
                }
                St::Return {
                    value, line, site, ..
                } => {
                    match value {
                        Some(v) => {
                            let want = self.ret_ty.clone();
                            self.core_val(m, b, body, w, v, &want, *line)?;
                        }
                        None if matches!(self.ret, Repr::Unit) => {}
                        None => {
                            return unsupported(
                                "a return whose value does not match the signature",
                                *line,
                            )
                        }
                    }
                    // Every open frame, before the branch, as the AST arm
                    // does: the value is on the operand stack already and a
                    // release does not disturb it.
                    self.emit_releases(m, b, ExitKind::Return, *site)?;
                    // Every region scope this return leaves, as the AST arm
                    // does: a returned value built inside a region points into
                    // the arena and its caller owns it, so the scope POPS
                    // rather than frees.
                    self.exit_regions_above(b, 0, false);
                    b.ins(&Instruction::Br(self.depth));
                }
                // A placed release, one row per step. The exit row below asks
                // [`Fn_::emit_releases`] for the whole group in the placement's
                // own order — the same walk over `rel_slots` the AST arm makes
                // at the same exit — so a step of its own emits nothing here.
                // §2.3 leaves the PLACEMENT to the emitter; what the row adds
                // is which exit, and which node the plan keys it by.
                St::Row { exit, .. } if CORE_EXITS.contains(exit) => {}
                St::Trap => {
                    b.ins(&Instruction::Unreachable);
                }
                // An expression for its effect. What it leaves on the stack
                // is dropped, or the enclosing block's type will not check —
                // the same sentence the AST walk's statement arm writes, on
                // the row rather than on the node.
                St::Do { rhs, line, .. } => {
                    let got = self.core_rhs_ty(rhs, *line)?;
                    self.core_rhs(m, b, body, w, rhs, &got, *line)?;
                    if self.cx.repr(&got, *line)? != Repr::Unit {
                        b.ins(&Instruction::Drop);
                    }
                }
                _ => return unsupported("a core statement this walk does not read", 0),
            }
        }
        Ok(())
    }

    /// What produced the value a `let` binds, in `want`.
    fn core_rhs(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        rhs: &Rhs,
        want: &Type,
        line: usize,
    ) -> Result<(), String> {
        match rhs {
            Rhs::Val(v) => self.core_val(m, b, body, w, v, want, line),
            Rhs::Prim(op, vs, ret) => {
                let got = self.core_prim(m, b, body, w, *op, vs, line)?;
                let got = ret.clone().unwrap_or(got);
                self.coerce(m, b, None, &got, want, line)
            }
            Rhs::Call {
                callee, args, kind, ..
            } => {
                let got = self.core_call(m, b, body, w, callee, *kind, args, line)?;
                self.coerce(m, b, None, &got, want, line)
            }
            _ => unsupported("a core right-hand side this walk does not read", line),
        }
    }

    /// What a right-hand side the driver reads PRODUCES, without emitting it —
    /// which a `St::Do` needs and a `St::Let` does not, because a `let` has a
    /// name and a name has the checker's type on it.
    fn core_rhs_ty(&self, rhs: &Rhs, line: usize) -> Result<Type, String> {
        match rhs {
            Rhs::Call { callee, kind, .. } => match self.core_sig(callee, *kind) {
                Some(s) => Ok(s.ret_ty),
                None => unsupported("a core call this walk does not read", line),
            },
            _ => unsupported("a discarded value the row does not type", line),
        }
    }

    /// One call, its callee read off the row — RFC-0125 §3 M3, the callee
    /// slice.
    ///
    /// The ABI is the DECLARATION's and the emitter reads it there: which
    /// wasm index the callee is, what each parameter's type is, whether one
    /// crosses by address. What the row states is WHO the callee is
    /// ([`Callee`]), which is the fourteen-rung ladder [`Fn_::call_inner`]
    /// walks over the source at every site and this walk does not walk at
    /// all.
    fn core_call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        callee: &str,
        kind: Callee,
        args: &[(Val, vyrn_frontend::ast::Capability)],
        line: usize,
    ) -> Result<Type, String> {
        let Some(sig) = self.core_sig(callee, kind) else {
            return unsupported("a core call this walk does not read", line);
        };
        for ((v, _), p) in args.iter().zip(&sig.params) {
            self.core_val(m, b, body, w, v, p, line)?;
        }
        b.ins(&Instruction::Call(sig.index));
        Ok(sig.ret_ty)
    }

    /// The signature this walk calls a [`Callee::Fn`] through, and `None` for
    /// every callee whose emission is more than a `call`.
    ///
    /// `Cx::sigs` holds exactly the functions this module DEFINES: a generic,
    /// a higher-order shell and a `std/mem` declaration are all skipped
    /// before it is filled, so a hit is the last rung of the ladder and a
    /// miss is one of the thirteen above it. The audited instrument is the
    /// one name that hits and must not be called — an unaudited build drops
    /// its four hooks rather than emitting them.
    fn core_sig(&self, callee: &str, kind: Callee) -> Option<Sig> {
        if kind != Callee::Fn || (!self.cx.audit && callee.starts_with(AUDIT_PREFIX)) {
            return None;
        }
        let sig = self.cx.sigs.get(callee)?;
        // A `modify` parameter crosses as the address of the caller's binding
        // and an aggregate result through a hidden leading pointer. Both are
        // placements, which is where §2.3 draws the emitter's own line, and
        // neither is reachable from a body of scalars.
        (!sig.modify.iter().any(|m| *m) && sig.ret.agg().is_none()).then(|| sig.clone())
    }

    /// An operator, its operands read off the row — RFC-0125 §3 M3, the
    /// operation slice's own reader.
    ///
    /// The instruction is [`Fn_::bin_ins`]'s and [`Fn_::un_ins`]'s: the same
    /// table the AST walk maps to, asked once. What this adds is where the
    /// operands come from — a name the core carries a type for, or a literal
    /// the row names.
    fn core_prim(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        op: Op,
        vs: &[Val],
        line: usize,
    ) -> Result<Type, String> {
        match (op, vs) {
            (Op::Un(u), [v]) => {
                let t = self.core_ty(body, v, &Type::Int);
                self.core_val(m, b, body, w, v, &t, line)?;
                self.un_ins(b, u, &t, line)
            }
            // `&&` and `||` short-circuit, and the row states them as prims
            // that read both operands — the linear judgment is the same either
            // way and the operator is what tells a reader the second read may
            // not happen (the operation slice's first finding). An honest row
            // is control flow, which is `St::If` over a temporary and moves
            // bytes; until it exists this walk stands down rather than
            // evaluating an operand the AST walk does not.
            (Op::Bin(BinOp::And | BinOp::Or), _) => unsupported(
                "`&&` or `||` from the core's row, which states no branch",
                line,
            ),
            (Op::Bin(o), [l, r]) => {
                let lt = self.core_ty(body, l, &Type::Int);
                let lt = self.cx.resolve(&lt);
                self.core_val(m, b, body, w, l, &lt, line)?;
                let opty = match Num::of(&lt) {
                    Some(n) if n == Num::PLAIN && matches!(r, Val::Name(_)) => {
                        let rt = self.core_ty(body, r, &lt);
                        self.op_width(&lt, Some(&rt))
                    }
                    _ => lt.clone(),
                };
                if opty != lt {
                    self.coerce(m, b, None, &lt, &opty, line)?;
                }
                self.core_val(m, b, body, w, r, &opty, line)?;
                self.bin_ins(b, o, &opty, &lt, line)
            }
            _ => unsupported("an operator of this arity", line),
        }
    }

    /// One value: the name's own place, or the literal the row names.
    fn core_val(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        v: &Val,
        want: &Type,
        line: usize,
    ) -> Result<(), String> {
        let got = match v {
            Val::Name(n) => {
                // Not a place: the stack is already holding it.
                if w.held == Some(*n) {
                    w.held = None;
                    let ty = body.names[*n as usize].ty.clone();
                    return self.coerce(m, b, None, &ty, want, line);
                }
                // The place is this walk's, or — since the interleave slice —
                // the one the AST arm bound the name at, which is the scope's.
                let Some((place, ty)) = self.core_place(w, body, *n) else {
                    return unsupported("a core name with no place", line);
                };
                let Place::Local(l) = place else {
                    return unsupported("a core name that is not a local", line);
                };
                b.ins(&Instruction::LocalGet(l));
                ty
            }
            // A literal is emitted at the type the AST walk gives one and
            // reconciled by the same seam: an integer literal is an `Int64`
            // that its destination narrows (RFC-0058), not a constant this
            // walk sizes itself.
            Val::Lit(l) => match l {
                Lit::Int(n) => {
                    b.ins(&Instruction::I64Const(*n));
                    Type::Int
                }
                Lit::Byte(n) => {
                    b.ins(&Instruction::I64Const(i64::from(*n)));
                    Type::Int
                }
                Lit::Bool(v) => {
                    b.ins(&Instruction::I32Const(i32::from(*v)));
                    Type::Bool
                }
                Lit::Float(f) => {
                    b.ins(&Instruction::F64Const((*f).into()));
                    Type::Float
                }
                Lit::Str(s) => {
                    let at = self.cx.rt.intern(m, s);
                    b.ins(&Instruction::I32Const(at as i32));
                    Type::Str
                }
                Lit::Opaque => return unsupported("a value the row does not name", line),
            },
        };
        self.coerce(m, b, None, &got, want, line)
    }

    /// What the AST arm would bind for one row — the screen's type clause.
    ///
    /// It is not the operator table stated twice: what it asks is which VALUE
    /// the arm evaluates, whose type is the arm's answer for the binding. An
    /// operator's is its first operand's, because that is the one the width
    /// rule ([`Fn_::op_width`]) adopts from.
    fn core_arm_ty(&self, body: &vyrn_lower::core::Body, rhs: &Rhs) -> Option<Type> {
        Some(match rhs {
            Rhs::Val(Val::Lit(l)) => match l {
                Lit::Int(_) | Lit::Byte(_) => Type::Int,
                Lit::Float(_) => Type::Float,
                Lit::Bool(_) => Type::Bool,
                Lit::Str(_) => Type::Str,
                Lit::Opaque => return None,
            },
            Rhs::Val(Val::Name(m)) => body.names[*m as usize].ty.clone(),
            Rhs::Call { callee, kind, .. } => self.core_sig(callee, *kind)?.ret_ty,
            Rhs::Prim(_, vs, _) => self.core_ty(body, vs.first()?, &Type::Int),
            _ => return None,
        })
    }

    /// The type of one value: the checker's, off the name the row carries.
    fn core_ty(&self, body: &vyrn_lower::core::Body, v: &Val, lit: &Type) -> Type {
        match v {
            Val::Name(n) => body.names[*n as usize].ty.clone(),
            Val::Lit(Lit::Bool(_)) => Type::Bool,
            Val::Lit(Lit::Float(_)) => Type::Float,
            Val::Lit(Lit::Str(_)) => Type::Str,
            Val::Lit(_) => lit.clone(),
        }
    }

    /// Whether the core's rows carry this whole body, so the walk above may
    /// have it — RFC-0125 §3 M3, the driver slice.
    ///
    /// Everything this refuses is a row the core lacks or a shape the two
    /// walks do not agree on yet, and §3 M3's residue table names each one
    /// with what it waits on. It is a screen and not a judgement: a body it
    /// stands down at is emitted from the AST exactly as before.
    fn core_walkable(&self, body: &vyrn_lower::core::Body, stmts: Option<&Block>) -> bool {
        // A frame with a release row, a region, an aggregate return or a
        // hoisted walk is one whose emission is more than its statements.
        if !self.placed.is_empty() || self.dest.is_some() || !body.lambdas.is_empty() {
            return false;
        }
        if !matches!(self.ret, Repr::Scalar(_) | Repr::Unit) {
            return false;
        }
        for info in &body.names {
            // A scalar of a type that needs no validation: a `where` type
            // (RFC-0079) is a `check` row the core does not carry, which is
            // the census's row 7.
            if !core_scalar(&info.ty) {
                return false;
            }
        }
        // The type the READER wrote, which the clause above cannot see: the
        // core names a `let` by the type of its VALUE (`core::Builder`'s `let`
        // arm), so `let a: Age = 20` reads `Int64` there and the check the
        // annotation asks for goes unrefused. The AST arm validates at the
        // `let` and at every later store into the binding, and the row states
        // neither. [`Fn_::core_run`] asks the same question per statement.
        if stmts.is_some_and(|blk| self.annotates_a_check(blk)) {
            return false;
        }
        let reads = body.reads();
        self.core_readable(body, &body.stmts, &reads)
    }

    /// Whether any `let` of `blk` is annotated with a type that carries a
    /// `where` clause (RFC-0079).
    fn annotates_a_check(&self, blk: &Block) -> bool {
        let mut found = false;
        each_block(blk, &mut |_| {}, &mut |s| {
            if let Stmt::Let { ty: Some(t), .. } = s {
                found |= self.checks(t);
            }
        });
        found
    }

    /// Whether a value of `t` is checked where it is made or stored: `t` names
    /// a declaration with a `where` clause, under this instance's type
    /// arguments.
    fn checks(&self, t: &Type) -> bool {
        matches!(self.cx.sub(t), Type::Named(n)
            if self.cx.types.get(&n).is_some_and(|d| d.predicate.is_some()))
    }

    /// Whether every statement of `ss` is one [`Fn_::core_stmts`] reads.
    fn core_readable(&self, body: &vyrn_lower::core::Body, ss: &[St], reads: &[u32]) -> bool {
        ss.iter().enumerate().all(|(i, s)| match s {
            St::Let(n, rhs) => {
                let held = body.names[*n as usize].binding.is_none()
                    && reads[*n as usize] == 1
                    && ss.get(i + 1).and_then(first_read) == Some(*n);
                // A temporary the stack cannot carry needs a local the AST walk
                // never takes, so the two would emit different locals.
                (held || body.names[*n as usize].binding.is_some()) && self.core_rhs_readable(rhs)
            }
            // A store's destination has to have a place before the store runs,
            // and this walk gives one to a temporary only where it BINDS it.
            // The temporary an `if` expression joins through is stored into by
            // each branch and bound by the `let` after them (RFC-0030), so it
            // has none: `let x = if c { 10 } else { 20 }` reached the store
            // with nowhere to put the value. A name the reader wrote is the
            // AST arm's local and is always placed.
            St::Store { place, value, .. } => {
                matches!(place, vyrn_lower::core::Place::Name(n)
                    if !body.names[*n as usize].source.starts_with('@'))
                    && core_val_readable(value)
            }
            St::If {
                cond, then, els, ..
            } => {
                core_val_readable(cond)
                    && self.core_readable(body, then, reads)
                    && self.core_readable(body, els, reads)
            }
            St::Block { body: inner, .. } => self.core_readable(body, inner, reads),
            St::Loop { body: inner, .. } => self.core_readable(body, inner, reads),
            St::Break { .. } | St::Continue { .. } => true,
            // A placed release runs at an exit. At the three exits this walk
            // takes itself, the exit's own row asks [`Fn_::emit_releases`] for
            // the whole group, so the steps before it are already accounted
            // for. A block's fall-through release and a scrutinee's are the
            // arm's, and they are refused.
            St::Row { exit, .. } => CORE_EXITS.contains(exit),
            St::Return { value, .. } => value.as_ref().is_none_or(core_val_readable),
            // A discarded value is dropped at the type the ROW produces, and
            // only a call row states one — a `St::Do` of anything else would
            // reach [`Fn_::core_rhs_ty`] and fail there rather than stand down.
            St::Do { rhs, line, .. } => {
                self.core_rhs_readable(rhs) && self.core_rhs_ty(rhs, *line).is_ok()
            }
            St::Trap => true,
            _ => false,
        })
    }

    fn core_rhs_readable(&self, rhs: &Rhs) -> bool {
        match rhs {
            Rhs::Val(v) => core_val_readable(v),
            // `&&` and `||` are stated as prims and emit a branch; the row states
            // no branch, so the walk stands down at them.
            Rhs::Prim(Op::Bin(BinOp::And | BinOp::Or), ..) | Rhs::Prim(Op::Closure, ..) => false,
            Rhs::Prim(_, vs, _) => vs.iter().all(core_val_readable),
            // A `spawn` runs the call as a task and a write-back stores the
            // receiver the call handed back; both are more than a `call`.
            Rhs::Call {
                callee,
                args,
                spawn,
                write_back,
                kind,
                ..
            } => {
                !spawn
                    && !write_back
                    && args.iter().all(|(v, _)| core_val_readable(v))
                    && self
                        .core_sig(callee, *kind)
                        .is_some_and(|s| s.params.len() == args.len())
            }
            _ => false,
        }
    }
}

/// The first name a statement READS, which is the only one the operand stack
/// can be carrying for it.
fn first_read(s: &St) -> Option<vyrn_lower::core::Name> {
    let name = |v: &Val| match v {
        Val::Name(n) => Some(*n),
        Val::Lit(_) => None,
    };
    match s {
        St::Let(_, Rhs::Val(v)) | St::Store { value: v, .. } => name(v),
        St::Let(_, Rhs::Prim(_, vs, _)) => vs.first().and_then(name),
        // A call pushes its arguments in order, so only the FIRST of them can
        // be the value the stack is already carrying.
        St::Let(_, Rhs::Call { args, .. })
        | St::Do {
            rhs: Rhs::Call { args, .. },
            ..
        } => args.first().and_then(|(v, _)| name(v)),
        St::Return { value: Some(v), .. } | St::If { cond: v, .. } => name(v),
        _ => None,
    }
}

fn core_val_readable(v: &Val) -> bool {
    // A NAME is screened by its recorded type ([`core_scalar`], in
    // `Fn_::core_run` and `Fn_::core_walkable`). A literal has no name and
    // carries its type in its own variant, so the same screen has to be spelled
    // here or it is not asked: a `Lit::Str` is a String, which this walk emits
    // no operation on. `"a" < "b"` reached `Op::Lt` with two of them and not one
    // name for the other screen to refuse.
    !matches!(v, Val::Lit(Lit::Opaque | Lit::Str(_)))
}

/// Every `let` a statement's rows bind, itself and everything under it.
fn core_lets<'r>(s: &'r St, out: &mut Vec<(vyrn_lower::core::Name, &'r Rhs)>) {
    match s {
        St::Let(n, rhs) => out.push((*n, rhs)),
        St::If { then, els, .. } => {
            then.iter().for_each(|s| core_lets(s, out));
            els.iter().for_each(|s| core_lets(s, out));
        }
        St::Loop { body: inner, .. } | St::Block { body: inner, .. } => {
            inner.iter().for_each(|s| core_lets(s, out));
        }
        St::Switch { arms, .. } => {
            for a in arms {
                a.body.iter().for_each(|s| core_lets(s, out));
            }
        }
        _ => {}
    }
}

/// The exits this walk gives back at itself — RFC-0125 §3 M3, the release
/// slice. A `break`, a `continue` and a `return` each carry the node the plan
/// keys their releases by, so the walk asks [`Fn_::emit_releases`] for the
/// group there. A block's fall-through release and a scrutinee's are keyed by
/// the block and by the construct, which no statement of a run names.
const CORE_EXITS: [ExitKind; 3] = [ExitKind::Break, ExitKind::Continue, ExitKind::Return];

/// Whether a run leaves the FUNCTION anywhere under it — the exit clause of
/// [`Fn_::core_run`]'s screen, which a subtree carries for every branch.
fn core_returns(s: &St) -> bool {
    match s {
        St::Return { .. } => true,
        St::If { then, els, .. } => then.iter().any(core_returns) || els.iter().any(core_returns),
        St::Loop { body: inner, .. } | St::Block { body: inner, .. } => {
            inner.iter().any(core_returns)
        }
        St::Switch { arms, .. } => arms.iter().any(|a| a.body.iter().any(core_returns)),
        _ => false,
    }
}

/// A type this walk reads a name of: a scalar that needs no validation. A
/// `where` type (RFC-0079) is a `check` row the core does not carry.
fn core_scalar(t: &Type) -> bool {
    matches!(
        t,
        Type::Int | Type::IntN { .. } | Type::Float | Type::Float32 | Type::Bool
    )
}

#[cfg(test)]
mod tests {
    use super::*;

    /// A single-source program with the runtime linked. Since PLAN-0125-runtime
    /// §6 step 1 the string family is `std/runtime`, which the loader injects
    /// into every program, so a test that compiles anything loads the way the
    /// CLI does rather than through the bare `vyrn_frontend::check`.
    fn linked(src: &str) -> Result<Program, String> {
        let files = vyrn_frontend::loader::MapResolver(
            [
                ("main.vyrn", src),
                (
                    "std/runtime.vyrn",
                    include_str!("../../../std/runtime.vyrn"),
                ),
                ("std/mem.vyrn", include_str!("../../../std/mem.vyrn")),
                // RFC-0125 §3 M6 (the third judgment's fifth slice): the runtime's
                // own `intStr` makes a `String` from bytes, and that check is
                // `std/text`'s now, so every linked program needs the module.
                ("std/text.vyrn", include_str!("../../../std/text.vyrn")),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        );
        let opts = vyrn_frontend::loader::LoadOptions {
            std_root: Some("std".into()),
            ..Default::default()
        };
        vyrn_frontend::load(src, "main.vyrn", &opts, &files)
            .map_err(|ds| ds.iter().map(|d| d.render()).collect::<Vec<_>>().join("\n"))
    }

    /// The gap message is the ladder's grouping key, so its shape is pinned:
    /// one construct, one line, no site-specific text in between.
    #[test]
    fn a_gap_names_the_construct_and_the_line() {
        let e: Result<(), String> = unsupported("`while`", 12);
        assert_eq!(
            e.unwrap_err(),
            "direct backend: no lowering for `while` at line 12"
        );
    }

    /// The runtime table's invariant, now that every runtime FUNCTION is
    /// `std/runtime`'s (`PLAN-0125-runtime.md` §6): a name is declared once, so
    /// [`VyrnRt::reserve`] hands out one index per row and [`VyrnRt::take`] can
    /// find the row a body belongs to. The other half — that every row gets a
    /// body — is [`VyrnRt::check`], which refuses the link rather than asserting.
    #[test]
    fn every_runtime_helper_is_declared_once() {
        let names: std::collections::HashSet<&str> =
            VYRN_RUNTIME.iter().map(|(n, ..)| *n).collect();
        assert_eq!(
            names.len(),
            VYRN_RUNTIME.len(),
            "a runtime function is declared twice"
        );
    }

    fn cx() -> Cx<'static> {
        Cx {
            plan: Default::default(),
            facts: None,
            types: HashMap::new(),
            lambdas: HashMap::new(),
            impls: Vec::new(),
            sigs: HashMap::new(),
            gen: None,
            variants: HashMap::new(),
            generics: HashMap::new(),
            higher_order: HashMap::new(),
            protocol_methods: HashMap::new(),
            owned: Default::default(),
            subst: HashMap::new(),
            mono: RefCell::new(Mono::default()),
            fnvals: RefCell::new(Vec::new()),
            fnval_copy: 0,
            dispatch: RefCell::new(Dispatch::default()),
            shapes: RefCell::new(Shapes::default()),
            globals: HashMap::new(),
            gappend: HashMap::new(),
            externs: HashMap::new(),
            releases: HashMap::new(),
            // RFC-0008's defaults, which are `Program`'s: nothing here logs.
            log_level: DEFAULT_LOG_LEVEL,
            log_sink: LogSink::Stderr,
            log_fd: None,
            audit: false,
            // Every index 0: a `Cx` for a type-level test never emits a call, and
            // a field per runtime function would have to be edited for each new
            // one.
            rt: Rt::default(),
        }
    }

    /// The whole aggregate ABI in one assertion: a scalar is a wasm value, an
    /// aggregate is an `i32` address, and the layout comes from `llt` rather
    /// than from anything written here.
    #[test]
    fn an_aggregate_travels_as_the_address_of_its_slot() {
        let c = cx();
        assert_eq!(c.repr(&Type::Int, 0).unwrap(), Repr::Scalar(ValType::I64));
        assert_eq!(c.repr(&Type::Bool, 0).unwrap(), Repr::Scalar(ValType::I32));
        // A String is a NUL-terminated pointer, so it is a scalar — the 23
        // examples it blocked were blocked by what you can DO with one.
        assert_eq!(c.repr(&Type::Str, 0).unwrap(), Repr::Scalar(ValType::I32));
        assert_eq!(c.repr(&Type::Unit, 0).unwrap(), Repr::Unit);
        let r = c.repr(
            &Type::Record(vec![
                Field {
                    name: "a".into(),
                    ty: Type::Bool,
                },
                Field {
                    name: "b".into(),
                    ty: Type::Int,
                },
            ]),
            0,
        );
        // `{ i1, i64 }` — the byte, then seven of hole. M0's clang test is why
        // this number is not a guess.
        assert_eq!(
            r.unwrap(),
            Repr::Agg(Layout {
                size: 16,
                align: 8,
                fields: vec![0, 8]
            })
        );
        assert_eq!(
            c.repr(&Type::option(Type::Int), 0).unwrap().val(),
            Some(ValType::I32)
        );
    }

    /// M0 left two ways for an escaped type parameter to be silent: `llt_of`
    /// prints `void` for one, and `layout` gives `void` a size of zero. Between
    /// them a parameter that survived monomorphization became a *smaller
    /// function* rather than an error. `ty_gap`'s refusal stood in front of that,
    /// but it was the ordinary case rather than the unreachable one.
    ///
    /// Since M2e every type this `Cx` is asked about goes through [`Cx::sub`]
    /// first, so the refusal is what is left over when an instantiation failed to
    /// fix something — asserted here from both sides, because "it never fires" is
    /// not the same claim as "it cannot".
    #[test]
    fn a_type_parameter_is_substituted_before_it_can_reach_a_layout() {
        let t = Type::Param("T".into());
        let mut c = cx();
        // Outside a monomorphization: refused, and `void` is what the refusal is
        // standing in front of.
        assert!(c.repr(&t, 0).is_err());
        assert_eq!(c.ll(&t), "void");
        // Inside one: the type the instantiation fixed, at every entry point —
        // one `sub`, not one substitution per caller.
        c.subst.insert("T".into(), Type::Int);
        assert_eq!(c.repr(&t, 0).unwrap(), Repr::Scalar(ValType::I64));
        assert_eq!(c.ll(&t), "i64");
        assert_eq!(c.resolve(&t), Type::Int);
        assert!(c.ty_gap(&t, 0).is_none());
        // And through a constructor, because `Array<T>` is the same triple for
        // every `T` but its element STRIDE is not: the substitution has to reach
        // inside the shape, not just past the outermost one.
        assert_eq!(c.ll(&Type::ArrayN(Box::new(t.clone()), 3)), "[3 x i64]");
        assert_eq!(c.ll(&Type::option(t)), "{ i64, i64 }");
    }

    /// A validated type has the SAME representation as its base, so a lowering
    /// that emits the type and forgets the check turns every refinement example
    /// green while validating nothing, permanently. `Even` and `Int64` are the
    /// same bytes; "the examples pass" is therefore not evidence, and this is.
    ///
    /// It used to be a refusal (`a_validated_type_is_a_gap_not_a_bare_int`),
    /// asserting the same two positions — the bare type, and inside a record,
    /// "because that is where it would hide". Now that RFC-0077 M2d emits the
    /// check, both positions assert that it IS emitted, which is the same
    /// property from the other side.
    ///
    /// The evidence is the trap message in the data segment. `emit_validation` is
    /// the only thing that interns it, so its presence means a check was emitted
    /// and its absence means one was not — a stronger signal than any byte count,
    /// and one no amount of correct-looking wasm can fake.
    #[test]
    fn a_validated_type_is_checked_wherever_it_is_reached() {
        let msg = "validation failed for `Age`";
        let bare = "type Age = Int64 where value >= 18 \
                    fn f(n: Int64) -> Int64 { let a: Age = n return a }
                    fn main() -> Int64 { return f(20) }";
        // Inside a record field, the position the refusal called out: nothing
        // about `{ i64 }` says one of those words is refined.
        let hidden = "type Age = Int64 where value >= 18 \
                      type U = { age: Age } \
                      fn f(n: Int64) -> Int64 { let u = U { age: n } return u.age }
                      fn main() -> Int64 { return f(20) }";
        for (what, src) in [("bare", bare), ("in a record", hidden)] {
            let p = linked(src).expect(what);
            let bytes = compile(&p).expect(what);
            assert!(
                bytes.windows(msg.len()).any(|w| w == msg.as_bytes()),
                "{what}: no `where` check was emitted"
            );
        }
        // And the negative. Since RFC-0125 §3 M6's fourth slice the message is
        // the CONSTRUCTOR's own `panic` string, so a module that declares the
        // type carries it whether or not anything crosses into it — the word is
        // no longer evidence of a check, and the check is the CALL. The same
        // program with a constant the checker proved emits no call, so the
        // reached module is the larger of the two.
        let proved = "type Age = Int64 where value >= 18                       fn f(n: Int64) -> Int64 { let a = Age(20) return a }
                      fn main() -> Int64 { return f(20) }";
        let small = compile(&linked(proved).unwrap()).unwrap();
        let big = compile(&linked(bare).unwrap()).unwrap();
        assert!(
            big.len() > small.len(),
            "a proven constant emitted a check: {} against {}",
            big.len(),
            small.len()
        );
    }

    /// A branch that yields `Ok(..)`/`Err(..)` — the shape `std/json` re-wraps a
    /// `stringFromBytes` result with, and the reason importing `std/json` was a
    /// gap at all. The emitting path (`sum_ctor`) has always typed these from the
    /// position; `peek` did not, so the arm fell through to the signature table,
    /// which holds no entry for a constructor, and read as "a branch yielding
    /// `Ok`". A `peek` with nothing expecting a `Result` is still a refusal —
    /// the half the constructor does not carry is unknowable from the arm alone —
    /// but a program cannot reach that state, so only the positive is asserted.
    #[test]
    fn a_branch_yields_a_result_when_the_position_names_one() {
        let src = "fn f(b: Array<UInt8>) -> Result<String, String> { \
                       return match stringFromBytes(b) { \
                           Ok(v) => Ok(v), \
                           Err(e) => Err(e), \
                       } } \
                   fn main() -> Int64 { \
                       return match f(bytes(\"hi\")) { Ok(s) => s.byteLength, Err(e) => 0 - 1 } }";
        let p = linked(src).unwrap();
        assert!(compile(&p).is_ok());
    }

    /// `.length` in a BRANCH, on every receiver that has one.
    ///
    /// The emitting path ([`Fn_::length_of`]) and the predicting path
    /// ([`Fn_::peek`]) each held their own copy of this list, and the copies
    /// disagreed on a `Map` and a `SmallArray`: a legal `m.length` in an arm
    /// read as "a field of the non-record type `Map<String, Int64>`". Both now
    /// read [`length_ty`], so a receiver added to one is added to both.
    #[test]
    fn a_branch_reads_a_length_on_every_receiver_that_has_one() {
        let each = [
            ("a String", "String", "\"hi\"", "byteLength"),
            ("an Array", "Array<Int64>", "[1, 2]", "length"),
            ("a Map", "Map<String, Int64>", "[\"a\": 1]", "length"),
            ("a SmallArray", "SmallArray<Int64, 4>", "[]", "length"),
        ];
        for (what, ty, lit, field) in each {
            let src = format!(
                "fn main() -> Int64 {{ \
                     let v: {ty} = {lit} \
                     let o: Option<Int64> = Some(1) \
                     return match o {{ Some(n) => v.{field}, None => 0 }} }}"
            );
            let p = linked(&src).expect(what);
            assert!(
                compile(&p).is_ok(),
                "{what}: {:?}",
                compile(&p).unwrap_err()
            );
        }
    }

    /// The rest of the `peek` audit RFC-0086's lesson asked for, once the
    /// `.length` rows were found missing.
    ///
    /// `peek` is deliberately shallow, but shallow means "refuses what it
    /// cannot see", not "has not been told about the emitting path". Each shape
    /// below compiles OUTSIDE a branch and was refused INSIDE one, and each row
    /// now reads what the emitting path reads rather than a second copy of it.
    #[test]
    fn a_branch_types_every_shape_the_emitting_path_lowers() {
        let cases = [
            // `Int32(n)` — the frontend's own conversion table, which `call`
            // already reads.
            (
                "a numeric conversion",
                "fn main() -> Int64 { let o: Option<Int64> = Some(1)                      let x: Int32 = match o { Some(n) => Int32(n), None => Int32(0) } return 0 }",
            ),
            // `t.join()` — the task's payload. Joined in EVERY arm: RFC-0095 M3
            // made the walk arm-granular, so a join in one arm and nothing in
            // the other is a task the other path abandons, and this case was
            // written that way while nothing could see it.
            (
                "a join",
                "fn work(n: Int64) -> Int64 { return n + 1 }                  fn main() -> Int64 { let t = spawn work(1) let o: Option<Int64> = Some(1)                      return match o { Some(n) => t.join() + n, None => t.join() } }",
            ),
            // An empty `[]` is typed by the position, in an arm like anywhere.
            (
                "an empty array",
                "fn main() -> Int64 { let o: Option<Int64> = Some(1)                      let a: Array<Int64> = match o { Some(n) => [], None => [] }                      return a.length }",
            ),
            // `?` in an arm — the sum's success half.
            (
                "a propagation",
                "fn f(n: Int64) -> Option<Int64> { return Some(n) }                  fn g() -> Option<Int64> { let o: Option<Int64> = Some(1)                      return Some(match o { Some(n) => f(n)?, None => 0 }) }                  fn main() -> Int64 { return 0 }",
            ),
            // `Age?(n)` in an arm — an `Option` of the named type.
            (
                "a fallible construction",
                "type Age = Int64 where value >= 0                  fn main() -> Int64 { let o: Option<Int64> = Some(1)                      let r: Option<Age> = match o { Some(n) => Age?(n), None => Age?(0) }                      return 0 }",
            ),
            // `spawn f(a)` in an arm — the call's type, in a `Task`.
            (
                "a spawn",
                "fn work(n: Int64) -> Int64 { return n + 1 }                  fn main() -> Int64 { let o: Option<Int64> = Some(1)                      let t: Task<Int64> = match o { Some(n) => spawn work(n), None => spawn work(0) }                      return t.join() }",
            ),
        ];
        for (what, src) in cases {
            let p = linked(src).expect(what);
            assert!(compile(&p).is_ok(), "{what}: {}", compile(&p).unwrap_err());
        }
    }
}
