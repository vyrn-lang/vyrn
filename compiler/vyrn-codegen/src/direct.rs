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
//! allocated BEFORE the branch and each arm copies into it, however many arms
//! there are.

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
use vyrn_lower::core::{Arg, Callee, Ctor, Lit, Op, Rhs, Spec, St, Target, Val};

use crate::layout::{self, Layout};
use crate::llt_of;
use crate::wasm::{self, BlockType, Frame, Instruction, MemArg, Module, ValType, HEAP_BASE};

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
    // Three kinds of function define nothing, and are skipped exactly as the
    // textual driver skips them (`lib.rs`, step 1). Lowering an unspecializable
    // shell would fail the whole build over a function nothing calls. The
    // fourth kind is [`gen_reach`]'s.
    let gen_reach = gen_reach(program);
    let mut generics: HashMap<String, &Function> = HashMap::new();
    let mut higher_order: HashMap<String, &Function> = HashMap::new();
    let mut user: Vec<&Function> = Vec::new();
    let mut skipped = std::collections::HashSet::new();
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
            skipped.insert(f.name.clone());
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

    let ownership = vyrn_frontend::own::analyze(program);
    // RFC-0114 §25's instrument, and never on the GENERATOR path (RFC-0076
    // M7): a generator module runs inside the compiler and its exit code is a
    // protocol between the host and the module, so a residue report there
    // would fail the build instead of measuring the program.
    let audited = gen.is_none() && vyrn_frontend::loader::audit_build();
    let mut cx = Cx {
        types,
        lambdas: vyrn_frontend::ast::lambdas(program),
        kept: RefCell::new(Vec::new()),
        impls: program.impls.clone(),
        sigs: HashMap::new(),
        rt,
        gen,
        generics,
        higher_order,
        skipped,
        subst: HashMap::new(),
        mono: RefCell::new(Mono::default()),
        fnvals: RefCell::new(Vec::new()),
        fnval_copy: 0,
        fnval_free: 0,
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
    let gaccs: Vec<String> = vyrn_lower::append::global_append_candidates(program)
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
    cx.fnval_free = m.reserve_func(&[ValType::I64, ValType::I32], &[]);

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
    m.fill(init_index, init)?;
    // Before the drain, like every other body: a global of a DECLARED generic
    // release reaches the teardown and nowhere else, and its instance has to
    // be on a worklist the drain below still reads. That instance is what
    // `vyrn-lower`'s `<teardown>` root queues.
    if let Some(ti) = teardown_index {
        let t = lower_globals_teardown(&mut m, program, &cx)?;
        m.fill(ti, t)?;
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
    let mut derived = false;
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
            let body = lower_body(
                &mut m,
                &p.f,
                &p.core_key,
                p.body,
                &p.sig,
                &cx,
                p.binds.clone(),
            );
            crate::observe::set_ctx(was);
            cx.subst = HashMap::new();
            cx.mono.borrow_mut().done += 1;
            match body {
                Ok(body) => {
                    m.fill(p.sig.index, body)?;
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
            m.fill(index, body)?;
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
        if let Some((sig_ty, dsig)) = d {
            let body = lower_dispatcher(&mut m, &cx, &sig_ty, &dsig)?;
            m.fill(dsig.index, body)?;
            cx.dispatch.borrow_mut().done += 1;
            continue;
        }
        // The registry is closed once every body above is walked, so the two
        // derived walks over it can be written — and they are written INSIDE
        // this loop because each is an ordinary release or copy of a capture
        // type, which puts a shape body on the worklist the next turn drains.
        if derived {
            break;
        }
        derived = true;
        let fncopy = lower_fnval_copy(&mut m, &cx)?;
        m.fill(cx.fnval_copy, fncopy)?;
        let fnfree = lower_fnval_free(&mut m, &cx)?;
        m.fill(cx.fnval_free, fnfree)?;
    }
    if let Some(e) = deferred {
        return Err(e);
    }

    // RFC-0114 §26's finish check stood here. It counted the rows the PLAN
    // placed and this emission never queried, and the emission queries no
    // plan row any more (RFC-0125 §3 M3, the emitter-reads-the-core-alone
    // slice). What answers the same question about the core's rows is
    // measurement: the residue ratchet and the memory suite.

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
    /// is exactly the clone this milestone deleted — so the core's rows state
    /// the body, and [`lower_body`] refuses one they do not carry.
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
    /// The name the core built this body under ([`vyrn_lower::core::body_of`]).
    core_key: String,
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
/// a target is [`Fn_::emit_call_with`] with the captures prepended to the argument
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
    lambdas: HashMap<usize, (&'a str, &'a Expr)>,
    /// The nodes this backend makes or copies and then hands to a walk that
    /// keys on their addresses, kept alive for the compile: a key built from a
    /// node's address is sound only while the node lives (#444).
    kept: RefCell<Vec<Rc<dyn std::any::Any>>>,
    /// Every `impl` block, for `place` projection lookup (RFC-0091 M2). A
    /// projection is not a function, so `sigs` cannot answer for it.
    impls: Vec<vyrn_frontend::ast::ImplBlock>,
    sigs: HashMap<String, Sig>,
    rt: Rt,
    /// The `vyrn_gen` host imports, on the generator path only (RFC-0076 M7).
    /// `None` is an ordinary `vyrn build --target wasm`, where every one of those
    /// builtins is refused by name exactly as it was.
    gen: Option<Gen>,
    /// Generic functions by name. They have no index and no body of their own —
    /// only specializations do — so a call to one is a discovery.
    generics: HashMap<String, &'a Function>,
    /// Functions with a `fn`-typed parameter (RFC-0023). Like a generic they have
    /// no index and no body of their own — only specializations do — so a call to
    /// one is a discovery, and the shell is skipped exactly as the textual driver
    /// skips it.
    higher_order: HashMap<String, &'a Function>,
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
    /// The module's one derived RELEASE over that registry: `(tag, block) -> ()`.
    /// The twin of [`Cx::fnval_copy`], reserved and filled beside it.
    fnval_free: u32,
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
    /// [`vyrn_lower::append::global_append_candidates`] cleared, so `g = g + …` grows the
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
    /// The functions [`gen_reach`] leaves out of the module. A call to one
    /// refuses at its own site, naming it.
    skipped: std::collections::HashSet<String>,
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

impl<'a> Cx<'a> {
    /// Does the container's release at this `for` walk the BUFFER alone? The
    /// core states it at the loop
    /// ([`vyrn_lower::core::Facts::loop_buffer_only`]), out of the same
    /// sentence that says whose an element is.
    fn loop_buffer_only(&self, node: usize) -> bool {
        self.facts
            .as_ref()
            .is_some_and(|f| f.loop_buffer_only.contains(&self.plan.key_of(node)))
    }

    /// RFC-0114 Rule N read off the core (RFC-0125 §3 M3, the derivation
    /// slice): the releases one edge of the join at `node` owes because
    /// another edge took the name. The core states each as a `St::Drop` at a
    /// `Site::Edge`, which is the join and the edge — a position in a branch
    /// is not a key, and this is why the drop carries one. The kernel's
    /// `equalize` is the one statement of the rule, and `own.rs` states none.
    fn edge_rows(&self, node: usize) -> Vec<vyrn_lower::core::EdgeRow> {
        let Some(f) = self.facts.as_ref() else {
            return Vec::new();
        };
        f.edges
            .get(&self.plan.key_of(node))
            .cloned()
            .unwrap_or_default()
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

    /// `node`, moved where it lives as long as this `Cx`, so its address keys
    /// nothing else for the whole compile.
    fn keep<T: 'static>(&self, node: T) -> Rc<T> {
        let kept = Rc::new(node);
        self.kept.borrow_mut().push(kept.clone());
        kept
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
        core_key: String,
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
            core_key,
        });
        Ok(sig)
    }

    /// The function this module defines that `t` calls with no captures, by
    /// the name [`Cx::sigs`] holds it under.
    fn named(&self, t: &FnTarget) -> Option<String> {
        (self.sigs.iter())
            .find(|(_, s)| t.ncaps == 0 && s.index == t.sig.index)
            .map(|(n, _)| n.clone())
    }

    /// The core key a lifted lambda was queued under, by its function index.
    fn lambda_key(&self, index: u32) -> Option<String> {
        let mono = self.mono.borrow();
        let p = (mono.insts.iter())
            .find(|p| p.sig.index == index && matches!(p.key, Key::Lambda(..)))?;
        Some(p.core_key.clone())
    }

    /// The signature of the one lifted lambda queued under `key`.
    fn lambda_sig(&self, key: &str) -> Option<Sig> {
        let mono = self.mono.borrow();
        let mut hits =
            (mono.insts.iter()).filter(|p| p.core_key == key && matches!(p.key, Key::Lambda(..)));
        let p = hits.next()?;
        hits.next().is_none().then(|| p.sig.clone())
    }

    /// An RFC-0023 specialization: [`Cx::enqueue`] of `f` with each `fn`
    /// parameter bound to its target, under the shell [`ho_shell`] states.
    fn specialize(
        &self,
        m: &mut Module,
        f: &'a Function,
        type_args: Vec<Type>,
        subst: HashMap<String, Type>,
        targets: Vec<FnTarget>,
    ) -> Result<Sig, String> {
        let (sf, binds) = ho_shell(f, &subst, &targets);
        let core_key = vyrn_lower::spell(&f.name, &type_args);
        self.enqueue(
            m,
            Key::Ho(f.name.clone(), type_args, targets),
            Rc::new(sf),
            Body::Block(&f.body),
            subst,
            binds,
            core_key,
        )
    }

    /// A generic instantiation (M2e): [`Cx::enqueue`] with no `fn` parameters.
    fn instantiate(
        &self,
        m: &mut Module,
        f: &'a Function,
        type_args: Vec<Type>,
        subst: HashMap<String, Type>,
    ) -> Result<Sig, String> {
        let core_key = vyrn_lower::spell(&f.name, &type_args);
        self.enqueue(
            m,
            Key::Generic(f.name.clone(), type_args),
            Rc::new(instance_shell(f, &subst)),
            Body::Block(&f.body),
            subst,
            HashMap::new(),
            core_key,
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

/// The chain a switch is being written as — see [`Fn_::chain_open`].
struct Chain {
    /// The arm the two-way collapse tests, and its tag; `None` for the chain.
    two: Option<(usize, usize)>,
    /// The depth a chain arm branches out to.
    out: u32,
    /// What the join carries.
    bt: BlockType,
    /// Where the `Else` went, so an arm that writes nothing can take it back.
    els: Option<usize>,
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

/// The storage a call in part position writes into ([`Fn_::core_part_at`]).
#[derive(Clone, Copy, PartialEq)]
enum PartIn {
    /// The parent's own: its slot, or the caller's where it lands.
    Parent,
    /// A heap array's element buffer, of this many bytes.
    Buffer(u32),
    /// A variant's boxed payload, of this many bytes.
    Box(u32),
}

/// A part whose own row writes it into its parent's storage
/// ([`Fn_::core_part_at`]).
struct PartAt {
    /// The parent's row.
    row: usize,
    parent: vyrn_lower::core::Name,
    /// The part's offset in the storage `into` names.
    off: u32,
    into: PartIn,
    /// The type the parent's layout gives the part.
    ty: Type,
}

/// How one `std/mem` primitive lowers (PLAN-0125-runtime §2.2): a `call` of
/// the host import `wasi_imports` declared, or this emitter's own
/// instruction. A host import the build declares none of is `unreachable`,
/// which is what the `vyrn_gen` pair is outside a generation.
#[derive(Clone, Copy)]
enum Mem {
    Host(Option<u32>),
    Ins,
}

/// What a `std/mem` primitive pushes UNDER its arguments. Only `trap` has
/// one: the descriptor its message goes to.
fn mem_pre(b: &mut Frame, prim: &str) {
    if prim == "trap" {
        b.ins(&Instruction::I32Const(2));
    }
}

/// The end of an aggregate store, with the destination's address and the
/// value's on the stack: two drops when the value was built `in_place`, and
/// the copy of `size` bytes otherwise. Both walks end a store with it.
fn agg_landed(b: &mut Frame, size: u32, in_place: bool) {
    if in_place {
        b.ins(&Instruction::Drop);
        b.ins(&Instruction::Drop);
    } else {
        b.ins(&Instruction::I32Const(size as i32));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
    }
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

/// Where the parts of a made layout come from — RFC-0125 M7, the layout-made
/// family.
///
/// A record literal, an array literal and a map literal each write their parts
/// at offsets the layout decides, and the offsets are the same whichever walk
/// is emitting: the AST arm has an expression per part and the core's row has a
/// [`Val`]. The builders below take this rather than a slice of expressions, so
/// the placement is stated once and the two walks cannot drift on it.
enum Parts<'a, 'c> {
    Core(&'a vyrn_lower::core::Body, &'a [Val], &'c mut Walked),
}

impl<'a> Parts<'a, '_> {
    fn len(&self) -> usize {
        match self {
            Parts::Core(_, vs, ..) => vs.len(),
        }
    }

    /// Where the `i`th part's own row wrote it ([`Fn_::core_part_at`]).
    fn built(&mut self, i: usize) -> Option<Dest> {
        match self {
            Parts::Core(_, vs, w) => match vs[i] {
                Val::Name(n) => w.built[n as usize].take(),
                Val::Lit(_) => None,
            },
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
        after_check: false,
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
    /// call ends.
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
    /// wasm local holding the accumulator's pointer → the frame slot holding its
    /// ownership flag. Keyed by local index rather than by name because the
    /// local IS the binding: two `let out`s in one body are two accumulators, and
    /// a global (a `Place::Static`) never gets an entry at all.
    str_append: HashMap<u32, u32>,
    dest_used: bool,
    /// The declared function this frame's plan rows are under: the function
    /// itself, or for a lifted lambda the function that holds the literal
    /// (RFC-0125 M3, third slice).
    owner: String,
    /// The name the core built this body under, which a lambda it lifts is
    /// keyed under too ([`vyrn_lower::core::lambda_spelling`]).
    core_key: String,
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
    /// The release rows held back for the read an exit hands back — see
    /// [`Fn_::core_releases`].
    core_rows: Vec<(vyrn_lower::core::Name, Vec<String>, ExitKind)>,
    /// Where the core's names live, and what the operand stack is holding.
    /// Shared by the two walks: a name the AST arm bound is found through
    /// [`Fn_::scope`], and one this walk bound is pushed onto it.
    core_w: Walked,
    /// The type the reader ANNOTATED the statement this walk is emitting with,
    /// for the one row that needs it — a made layout (RFC-0125 M7).
    ///
    /// The row's type is the VALUE's (`core::Builder::stmt` asks `ty_of`) and
    /// the arm builds into the annotation's layout, so `let xs: Array<Int64> =
    /// [1, 2, 3]` writes a heap triple where the row alone says a fixed three.
    /// Keyed by the statement's own binding, because the run makes other
    /// layouts too, each at its own type. `None` for an unannotated statement
    /// and for the per-body walk, which refuses a body that annotates anything
    /// at all.
    core_bound: Option<(vyrn_lower::core::Name, Type)>,
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
        str_append: HashMap::new(),
        dest_used: false,
        owner: String::new(),
        core_key: String::new(),
        core: None,
        core_at: HashMap::new(),
        core_rows: Vec::new(),
        core_w: Walked::default(),
        core_bound: None,
    }
}

/// The module-state initializer (RFC-0013): every top-level `let`'s value stored
/// into its fixed address, in declaration order, in one function `_start` calls
/// before `main`.
///
/// The core's module-state body is what is walked. A program whose
/// initializers it does not carry is refused at its first global.
fn lower_globals_init(m: &mut Module, program: &Program, cx: &Cx<'_>) -> Result<Frame, String> {
    let mut b = Frame::new(&[], &[], &[], 0);
    let mut f = top_level(cx);
    // The core's module-state body ([`vyrn_lower::core::build_module_state`])
    // stores each initializer's value and states no check, so a global whose
    // type validates is refused here.
    let from_core = vyrn_lower::core::body_of("")
        .filter(|_| (program.globals.iter()).all(|g| !f.checks(&cx.globals[&g.name].1)))
        .filter(|core| {
            f.core_enter(core);
            f.core = Some(std::rc::Rc::new(core.clone()));
            f.core_walkable(core, None)
        });
    match &from_core {
        Some(core) => f.core_body(m, &mut b, core)?,
        None => {
            if let Some(g) = program.globals.first() {
                return unsupported("a module-state initializer the core did not state", g.line);
            }
        }
    }
    for g in &program.globals {
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
    let mut b = Frame::new(&[], &[], &[], 0);
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
    let frame = lower_body(m, f, &f.name, Body::Block(&f.body), sig, cx, binds)?;
    m.fill(sig.index, frame)?;
    if std::env::var_os("VYRN_WASM_NAMES").is_some() {
        m.name(sig.index, &f.name);
    }
    Ok(())
}

/// The body itself, before it is installed at the index reserved for it.
///
/// `f` is the DECLARATION — the name, the line and the signature — `key` the
/// name the core built the body under, and `body` the statements. `f` and
/// `body` are two arguments because a specialization's signature is
/// synthesized and its statements are the callee's own, borrowed rather than
/// cloned (see [`Pending::body`]).
fn lower_body(
    m: &mut Module,
    f: &Function,
    key: &str,
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
    let (params, results) = cx.wasm_sig(&sig, f.line)?;
    let dest = sig.ret.agg().map(|_| 0u32);
    let shift = dest.map_or(0, |_| 1);
    // A lifted lambda's rows are the enclosing function's (see `f_shell`).
    let owner = f
        .name
        .strip_prefix(LAMBDA)
        .map(|rest| rest.trim_start().to_string())
        .filter(|o| !o.is_empty())
        .unwrap_or_else(|| f.name.clone());

    let mut b = Frame::new(&params, &results, &[], 0);
    let core = core_body(key, f, &binds, cx).map(std::rc::Rc::new);
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
        str_append: HashMap::new(),
        dest_used: false,
        owner,
        core_key: key.to_string(),
        core,
        core_at: HashMap::new(),
        core_rows: Vec::new(),
        core_w: Walked::default(),
        core_bound: None,
    };
    if let Some(core) = cx_fn.core.clone() {
        cx_fn.core_enter(&core);
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
    if let Some(core) = cx_fn.core.clone() {
        cx_fn.core_lift_targets(m, &core.stmts);
    }
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
            Body::Value(e) => {
                let what = format!(
                    "the lambda body in `{}` the core did not state",
                    cx_fn.owner
                );
                return unsupported(&what, e.line());
            }
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

/// What a `fn` parameter bound to `b` calls, as the core names it: a
/// function this module calls with no captures, a stored value called through
/// its signature's dispatcher, or a lifted lambda with the captures `b`
/// forwards. `None` for a lambda key two bodies share, which
/// [`vyrn_lower::core::body_of`] names no body for.
fn core_target_of(cx: &Cx<'_>, b: &FnBinding) -> Option<Target> {
    if let Some(f) = cx.named(&b.target) {
        return Some(Target::Fn(f));
    }
    let index = b.target.sig.index;
    if cx
        .dispatch
        .borrow()
        .sigs
        .iter()
        .any(|(_, s)| s.index == index)
    {
        return match b.cap_srcs.as_slice() {
            [value] => Some(Target::Value(value.clone())),
            _ => None,
        };
    }
    let key = cx.lambda_key(b.target.sig.index)?;
    vyrn_lower::core::body_of(&key)?;
    let tys = b.target.sig.params.get(..b.target.ncaps)?;
    (b.cap_srcs.len() == tys.len())
        .then(|| Target::Lambda(key, b.cap_srcs.iter().cloned().zip(tys.to_vec()).collect()))
}

/// A higher-order call row's arguments in its instance's order. The row
/// leaves out each `fn` argument, and [`vyrn_lower::core::specialize`]
/// appends the captures a lambda target forwards after the row's own
/// arguments, target by target; the instance takes them where the `fn`
/// parameter stood. `None` where the counts disagree.
fn ho_args(
    f: &Function,
    targets: &[Target],
    args: &[(Arg, vyrn_frontend::ast::Capability)],
) -> Option<Vec<(Arg, vyrn_frontend::ast::Capability)>> {
    let caps = |t: &Target| match t {
        Target::Lambda(_, c) => c.len(),
        Target::Value(_) => 1,
        _ => 0,
    };
    let at = args.len().checked_sub(targets.iter().map(caps).sum())?;
    let (mut own, mut forwarded, mut ts) = (args[..at].iter(), args[at..].iter(), targets.iter());
    let mut out = Vec::new();
    for p in &f.params {
        if matches!(p.ty, Type::Fn(..)) {
            out.extend(forwarded.by_ref().take(caps(ts.next()?)).cloned());
        } else {
            out.push(own.next()?.clone());
        }
    }
    (own.next().is_none() && forwarded.next().is_none()).then_some(out)
}

/// The core body a queued function reads: its own, and for an RFC-0023
/// specialization the instance [`vyrn_lower::core::specialize`] states for
/// its targets. A specialization with a target the core does not name reads
/// the declaration's body, whose calls through the parameter the screen
/// stands down at.
///
/// The body is the frame's only where its parameters are the frame's, by name
/// and in order. A body with another parameter list was built for another
/// signature: a specialization [`vyrn_lower::core::specialize`] could not
/// state, or a lambda whose captures the two sides listed apart.
fn core_body(
    key: &str,
    f: &Function,
    binds: &HashMap<String, FnBinding>,
    cx: &Cx<'_>,
) -> Option<vyrn_lower::core::Body> {
    let body = vyrn_lower::core::body_of(key)?;
    let bound: Option<Vec<(vyrn_lower::core::Name, Target)>> = (body.params.iter())
        .filter_map(|&n| Some((n, binds.get(&body.names[n as usize].source)?)))
        .map(|(n, b)| Some((n, core_target_of(cx, b)?)))
        .collect();
    let body = match bound {
        Some(bound) if !binds.is_empty() && bound.len() == binds.len() => {
            vyrn_lower::core::specialize(&body, &bound).unwrap_or(body)
        }
        _ => body,
    };
    let sources = body.params.iter().map(|&n| &body.names[n as usize].source);
    (body.params.len() == f.params.len() && sources.eq(f.params.iter().map(|p| &p.name)))
        .then_some(body)
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
/// The copy is **deep**, because the CONSTRUCTION is
/// ([`Fn_::fnval_into`]): a heap capture is duplicated into the block, so the
/// block owns what its captures point at and a copy of the block owes a second
/// copy of that. The release twin below walks the same captures, so the two
/// stay mirrors.
fn lower_fnval_copy(m: &mut Module, cx: &Cx<'_>) -> Result<Frame, String> {
    let mut b = Frame::new(&[ValType::I64, ValType::I32], &[ValType::I32], &[], 0);
    let mut f = top_level(cx);
    let (tag, pay) = (0u32, 1u32);
    let vals = cx.fnvals.borrow().clone();
    for (i, v) in vals.iter().enumerate() {
        let cap_tys = v.target.sig.params[..v.target.ncaps].to_vec();
        // No captures means payload 0, and 0 copies to itself.
        if cap_tys.is_empty() {
            continue;
        }
        let bl = f.cap_block(&cap_tys)?;
        b.ins(&Instruction::LocalGet(tag));
        b.ins(&Instruction::I64Const(i as i64));
        b.ins(&Instruction::I64Eq);
        b.ins(&Instruction::If(BlockType::Empty));
        let dst = b.local(ValType::I32);
        b.ins(&Instruction::I64Const(bl.size as i64));
        b.ins(&Instruction::Call(cx.rt.malloc));
        b.ins(&Instruction::LocalTee(dst));
        b.ins(&Instruction::LocalGet(pay));
        b.ins(&Instruction::I32Const(bl.size as i32));
        b.ins(&Instruction::MemoryCopy {
            src_mem: 0,
            dst_mem: 0,
        });
        fnval_captures(m, &mut f, &mut b, dst, &bl, &cap_tys, false)?;
        b.ins(&Instruction::LocalGet(dst));
        b.ins(&Instruction::Return);
        b.ins(&Instruction::End);
    }
    // Every other tag has no block to copy: the payload is 0 and the copy is the
    // value, exactly as a scalar's is.
    b.ins(&Instruction::LocalGet(pay));
    Ok(b)
}

/// The module's one derived RELEASE over the defunctionalized enum:
/// `(tag, block) -> ()`, the twin of [`lower_fnval_copy`].
///
/// A stored `fn` value's block holds its captures BY VALUE and owns their heap
/// (`Fn_::build_fnval`), so giving the block back means walking those captures
/// first. Only the tag knows which captures a block holds, so the walk is here
/// — the one place the registry is readable — rather than at the release site,
/// which sees a `Fn(..)` type and no tag.
fn lower_fnval_free(m: &mut Module, cx: &Cx<'_>) -> Result<Frame, String> {
    let mut b = Frame::new(&[ValType::I64, ValType::I32], &[], &[], 0);
    let mut f = top_level(cx);
    let (tag, pay) = (0u32, 1u32);
    let vals = cx.fnvals.borrow().clone();
    for (i, v) in vals.iter().enumerate() {
        let cap_tys = v.target.sig.params[..v.target.ncaps].to_vec();
        // A tag with no heap capture has nothing above the block itself, and
        // the tail below frees that.
        if !cap_tys.iter().any(|t| f.owns_heap(t)) {
            continue;
        }
        let bl = f.cap_block(&cap_tys)?;
        b.ins(&Instruction::LocalGet(tag));
        b.ins(&Instruction::I64Const(i as i64));
        b.ins(&Instruction::I64Eq);
        b.ins(&Instruction::If(BlockType::Empty));
        fnval_captures(m, &mut f, &mut b, pay, &bl, &cap_tys, true)?;
        b.ins(&Instruction::End);
    }
    // The block itself, whatever the tag. A payload of 0 is the no-capture
    // case and `free` refuses it.
    b.ins(&Instruction::LocalGet(pay));
    b.ins(&Instruction::Call(cx.rt.free));
    Ok(b)
}

/// Walk the captures of one tag's block, whose address is in local `at`:
/// release each one that owns heap, or give each one its own heap. The two
/// directions differ by which call the walk makes, so they are one function.
fn fnval_captures(
    m: &mut Module,
    f: &mut Fn_<'_, '_>,
    b: &mut Frame,
    at: u32,
    bl: &Layout,
    cap_tys: &[Type],
    rel: bool,
) -> Result<(), String> {
    for (ci, ct) in cap_tys.iter().enumerate() {
        if !f.owns_heap(ct) {
            continue;
        }
        let a = b.local(ValType::I32);
        b.ins(&Instruction::LocalGet(at));
        if bl.fields[ci] != 0 {
            b.ins(&Instruction::I32Const(bl.fields[ci] as i32));
            b.ins(&Instruction::I32Add);
        }
        b.ins(&Instruction::LocalSet(a));
        if rel {
            f.rel_at(m, b, a, ct, 0)?;
        } else {
            f.copy_at(m, b, a, ct, 0)?;
        }
    }
    Ok(())
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
    let mut b = Frame::new(&[ValType::I32], &[], &[], 0);
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
    let (params, results) = cx.wasm_sig(dsig, 0)?;
    let mut b = Frame::new(&params, &results, &[], 0);
    let mut f = top_level(cx);
    let mut args: Vec<(Place, Type)> = Vec::new();

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
        args.push((place, pty.clone()));
    }

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
        // The capture block, copied off the heap into a frame slot so each capture
        // has a `Place`. The copy is what the textual backend's `load {block_ll}`
        // is, and it is also the by-value read a capture is.
        let cap_tys = v.target.sig.params[..v.target.ncaps].to_vec();
        let mut all: Vec<(Place, Type)> = Vec::new();
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
                all.push((place, ct.clone()));
            }
        }
        all.extend(args.iter().cloned());
        // An aggregate result is written through OUR destination, so it goes on the
        // stack under the call — M2d's "a value may sit beneath an `if`" applied to
        // the call rather than to a check.
        if let Some(d) = dest {
            b.ins(&Instruction::LocalGet(d));
        }
        // Each value crosses at the target's parameter type, as an argument
        // does ([`Fn_::expr_as`]).
        let mut operand = |s: &mut Fn_, m: &mut Module, b: &mut Frame, i: usize, p: &Type| {
            let (place, ty) = &all[i];
            s.push_place(b, *place, ty, 0)?;
            s.coerce(m, b, None, ty, p, 0).map(|_| None)
        };
        let got = f.emit_call_with(m, &mut b, &v.target.sig, all.len(), &mut operand, None)?;
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
            Rel::Call(f, ty) => {
                self.release_call(m, b, f, ty, p, line)?;
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

    /// Call the `release` a type declares (`impl Owned`, RFC-0096) on the value
    /// at `p`: the instance whose type arguments `ty` fixes where the impl is
    /// generic. The value crosses as a read of a binding does and coerces to
    /// the release's one parameter.
    fn release_call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        f: &str,
        ty: &Type,
        p: Place,
        line: usize,
    ) -> Result<(), String> {
        // Inside a monomorphized instance the value's type names the
        // instance's parameters, which the solve reads substituted.
        let sig = match self.cx.generics.get(f).copied() {
            Some(g) => self.generic_sig(m, g, &[self.cx.sub(ty)], None, line)?,
            None => match self.cx.sigs.get(f) {
                Some(sig) => sig.clone(),
                None => return unsupported(&format!("the release `{f}`"), line),
            },
        };
        let ([param], [false]) = (sig.params.as_slice(), sig.modify.as_slice()) else {
            return unsupported(&format!("the release `{f}` at this signature"), line);
        };
        let param = param.clone();
        self.push_place(b, p, ty, line)?;
        self.coerce(m, b, None, ty, &param, line)?;
        b.ins(&Instruction::Call(sig.index));
        Ok(())
    }

    /// Push the value of type `t` at `place`: a local's value, a slot's
    /// address, and module state's value or, for an aggregate, its address.
    fn push_place(&self, b: &mut Frame, place: Place, t: &Type, line: usize) -> Result<(), String> {
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
            Place::Static(at) => {
                b.ins(&Instruction::I32Const(at as i32));
                if let Repr::Scalar(_) = self.cx.repr(t, line)? {
                    b.ins(&load_of(&self.cx.ll(t), 0, self.cx.signed(t)));
                }
            }
        }
        Ok(())
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

    /// Whether `own` gives `ty` a walking release rather than a buffer one:
    /// RFC-0092 M2's element row for an `Array`, M3's for a `Map` and a
    /// `SmallArray`. `own` decides and this asks it, including the stop for a
    /// self-referring element, whose walk has no bottom.
    fn deep_row(&self, ty: &Type) -> bool {
        matches!(self.cx.owned.release_kind(ty), Some(DropKind::Deep(_)))
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

    /// The release a value of `ty` bound at the node `key` owes:
    /// [`Fn_::rel_for`]'s, or the buffer alone, the triple's field 0, where
    /// `key` is a `for` whose every element left through the loop variable
    /// ([`Cx::loop_buffer_only`]). The deep walk would free values somebody
    /// else owns there; a blanket buffer-only free took somebody else's
    /// storage in round fourteen.
    fn rel_owed(&mut self, key: usize, ty: &Type, line: usize) -> Result<Option<Rel>, String> {
        let buffer = self.cx.loop_buffer_only(key);
        Ok(self
            .rel_for(ty, line)?
            .map(|r| if buffer { Rel::Buffers(vec![0]) } else { r }))
    }

    /// How a value of `ty` is reclaimed, or `None` for one that owns no heap.
    ///
    /// [`vyrn_frontend::declared::Owned::release_kind`]'s row, and nothing
    /// else. The row says what a release MEANS — a call the type declared, a
    /// buffer, a walk — and this adds the byte offsets, which are `layout`'s
    /// and which no pass above this crate can state.
    ///
    /// The spelling matters, and this is the one place it does. A declared row
    /// is keyed by the type's NAME, so it is asked of `ty`. Every other row is
    /// asked of the SUBSTITUTED type: a generic body's `Array<T>` has a
    /// `Param` element and `own` answers `Deep` for one, because inside that
    /// body the element is unknowable — an emitter has the instance in hand
    /// and is owed the instance's row.
    fn rel_for(&mut self, ty: &Type, line: usize) -> Result<Option<Rel>, String> {
        if let Some(DropKind::Release(f, _)) = self.cx.owned.release_kind(ty) {
            return Ok(Some(Rel::Call(f, ty.clone())));
        }
        let t = self.cx.resolve(ty);
        let bufs = |which: &[usize]| -> Result<Rel, String> {
            let l = self.layout_of(&t, line)?;
            Ok(Rel::Buffers(which.iter().map(|i| l.fields[*i]).collect()))
        };
        Ok(match self.cx.owned.release_kind(&t) {
            Some(DropKind::Release(f, _)) => Some(Rel::Call(f, t.clone())),
            Some(DropKind::FreeStr) => Some(Rel::Str),
            Some(DropKind::FreeArr) => Some(bufs(&[0])?),
            Some(DropKind::FreeMap) => Some(bufs(&[0, 1, 4])?),
            // `{ i64 len, i64 cap, ptr data, [N x T] inline }` — field 2, and it
            // is null until the array spills, which is exactly the case `free`
            // refuses. The inline slots need no reclamation.
            Some(DropKind::FreeSmallArr) => Some(bufs(&[2])?),
            // The element walk needs an address, which is why this answer is a
            // `Deep` and the buffer-only ones are a `Buffers`.
            Some(DropKind::Deep(d)) => Some(Rel::Deep(d, Vec::new())),
            // A `Stream<T>` is the one shape with no row: it reaches its
            // release through the stream lowering (RFC-0075 M2b), and a row
            // here would release it twice.
            Some(DropKind::CloseStream) | None => match t {
                Type::Stream(i) => Some(Rel::Stream(*i)),
                _ => None,
            },
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
            Type::Array(inner) if self.deep_row(&self.cx.resolve(ty)) => {
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
                self.each(m, b, true, data, n, stride, &inner, line)?;
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
                self.each(m, b, true, base, n, stride, &inner, line)?;
                b.ins(&Instruction::LocalGet(a))
                    .ins(&Instruction::I32Load(word_at(l.fields[2])))
                    .ins(&Instruction::Call(self.cx.rt.free));
                Ok(())
            }
            // Two parallel buffers. String keys are released per entry
            // (RFC-0092 M3); Int64 and packed keys go with their buffer
            // (RFC-0117). The elements first, then the buffers they live in.
            Type::Map(kt, vt) if self.deep_row(ty) => {
                let mk = self.map_key(&kt, line)?;
                let l = self.layout_of(ty, line)?;
                let vstride = self.stride(&vt, line)?;
                let n = b.local(ValType::I32);
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[2])));
                b.ins(&Instruction::I32WrapI64);
                b.ins(&Instruction::LocalSet(n));
                let kstride = mk.stride() as u32;
                for (i, (stride, elem)) in [(kstride, Type::Str), (vstride, (*vt).clone())]
                    .into_iter()
                    .enumerate()
                {
                    let buf = b.local(ValType::I32);
                    b.ins(&Instruction::LocalGet(a));
                    b.ins(&Instruction::I32Load(word_at(l.fields[i])));
                    b.ins(&Instruction::LocalSet(buf));
                    if i == 1 || mk == MapKey::Str {
                        self.each(m, b, true, buf, n, stride, &elem, line)?;
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
                self.each(m, b, true, a, count, stride, &inner, line)
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
                        // A `consume` took the payload (`Elem.1`), and the box
                        // it rode in is still the sum's to free.
                        let key = format!("{}.{j}", var.name);
                        if holes.contains(&key) {
                            if w == Word::Boxed {
                                b.ins(&Instruction::LocalGet(a))
                                    .ins(&Instruction::I64Load(word_at8(l.fields[at])))
                                    .ins(&Instruction::I32WrapI64)
                                    .ins(&Instruction::Call(self.cx.rt.free));
                            }
                            continue;
                        }
                        self.rel_holes = vyrn_frontend::declared::holes_under(holes, &key);
                        self.rel_word(m, b, a, l.fields[at], pty, w, line)?;
                    }
                    self.depth -= 1;
                    b.ins(&Instruction::End);
                }
                Ok(())
            }
            // A stored function value is `{ i64 tag, i64 captures }` (RFC-0037).
            // The captures are one heap block, DUPLICATED into it at the
            // construction site, so the block owns their heap and the walk over
            // them is the tag's. Only the registry knows which captures a tag
            // holds, so this hands both words to the module's derived release
            // ([`lower_fnval_free`]) exactly as the copy hands them to
            // [`lower_fnval_copy`]. Census §16.
            Type::Fn(..) => {
                let l = self.layout_of(ty, line)?;
                b.ins(&Instruction::LocalGet(a))
                    .ins(&Instruction::I64Load(word_at8(l.fields[0])))
                    .ins(&Instruction::LocalGet(a))
                    .ins(&Instruction::I32Load(word_at(l.fields[1])))
                    .ins(&Instruction::Call(self.cx.fnval_free));
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
    /// The whole value's release, as a store's displaced value takes it
    /// ([`Fn_::free_snap`]), under the same exceptions
    /// ([`Fn_::replaced_releases`]).
    fn rel_entry(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        a: u32,
        ty: &Type,
        line: usize,
    ) -> Result<(), String> {
        if self.replaced_releases(ty) {
            self.rel_at(m, b, a, ty, line)?;
        }
        Ok(())
    }

    /// Whether a value of `ty` that a store or a map entry displaces is
    /// released: every value that owns heap but a stream, which is linear and
    /// whose end is its consumer's. A declared `release` runs, because a value
    /// that reaches its end without it leaks (record `m7-box`).
    fn replaced_releases(&self, ty: &Type) -> bool {
        !matches!(
            self.cx.owned.release_kind(ty),
            None | Some(DropKind::CloseStream)
        )
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

    /// Keep the value of `ty` at address `a`, for a store that is about to
    /// overwrite it (RFC-0089 rule 4), with the release that value takes, or
    /// `None` when displacing it releases nothing
    /// ([`Fn_::replaced_releases`]).
    ///
    /// A store into an owned place releases what it held; a boxed payload
    /// displaced by a store leaked (record `m7-box`). So the whole value is
    /// kept, and [`Fn_::free_snap`] runs the release a `let` exit runs
    /// ([`Fn_::emit_rel`]) after the store. A scalar is kept in a local and an
    /// aggregate in a frame slot, because `emit_rel` reads a local as the
    /// value of the one and the address of the other. The snapshot is taken
    /// BEFORE the store because an aggregate is built destination-first, and
    /// it is only reached where the new value does not name the place
    /// ([`vyrn_frontend::ast::mentions`]), so nothing the store computes can
    /// read it.
    fn snap_at(
        &mut self,
        b: &mut Frame,
        a: u32,
        ty: &Type,
        line: usize,
    ) -> Result<Option<(Place, Rel)>, String> {
        let Some(rel) = self.replaced_rel(ty, line)? else {
            return Ok(None);
        };
        let place = match self.cx.repr(ty, line)? {
            Repr::Agg(l) => {
                let at = b.alloc(l.size, l.align);
                b.slot(at)
                    .ins(&Instruction::LocalGet(a))
                    .ins(&Instruction::I32Const(l.size as i32))
                    .ins(&Instruction::MemoryCopy {
                        src_mem: 0,
                        dst_mem: 0,
                    });
                Place::Slot(at)
            }
            Repr::Scalar(v) => {
                let t = b.local(v);
                b.ins(&Instruction::LocalGet(a))
                    .ins(&load_of(&self.cx.ll(ty), 0, false))
                    .ins(&Instruction::LocalSet(t));
                Place::Local(t)
            }
            Repr::Unit => return Ok(None),
        };
        Ok(Some((place, rel)))
    }

    /// [`Fn_::snap_at`] for a scalar local, which IS the value and has no
    /// address.
    fn snap_word(
        &mut self,
        b: &mut Frame,
        l: u32,
        v: ValType,
        ty: &Type,
        line: usize,
    ) -> Result<Option<(Place, Rel)>, String> {
        let Some(rel) = self.replaced_rel(ty, line)? else {
            return Ok(None);
        };
        let t = b.local(v);
        b.ins(&Instruction::LocalGet(l))
            .ins(&Instruction::LocalSet(t));
        Ok(Some((Place::Local(t), rel)))
    }

    fn replaced_rel(&mut self, ty: &Type, line: usize) -> Result<Option<Rel>, String> {
        if !self.replaced_releases(ty) {
            return Ok(None);
        }
        self.rel_for(ty, line)
    }

    /// Release what a snapshot kept, after the store that replaced it.
    fn free_snap(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        snap: Option<(Place, Rel)>,
        line: usize,
    ) -> Result<(), String> {
        match snap {
            Some((p, rel)) => self.emit_rel(m, b, p, &rel, line),
            None => Ok(()),
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

    /// `stringFromBytes(b)` (RFC-0014): the bytes checked by `std/text`'s
    /// `stringFault` and then copied into a fresh NUL-terminated buffer, as
    /// a `Result<String, String>`. The result is an aggregate, so the slot
    /// is allocated here and the runtime writes through it — the same
    /// hidden destination an aggregate-returning Vyrn call gets.
    ///
    /// RFC-0125 §3 M6 (the third judgment's fifth slice): the check is the
    /// call this arm makes first, and its answer travels into
    /// `strFromBytes` where the DFA table used to go. This backend was
    /// never a carrier of the two `String` rows — it called the runtime —
    /// and now the runtime is not one either.
    ///
    /// `operand` writes argument `i` at the type asked for.
    fn string_from_bytes(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        operand: &mut dyn FnMut(
            &mut Self,
            &mut Module,
            &mut Frame,
            usize,
            &Type,
        ) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
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
        operand(self, m, b, 0, &bytes)?;
        let src = self.scratch(b, ValType::I32, 0);
        let al = self.layout_of(&bytes, line)?;
        b.ins(&Instruction::LocalSet(src));
        let off = b.alloc(l.size, l.align);
        self.str_from_bytes(b, off, src, &al, line)?;
        b.slot(off);
        Ok(ty)
    }

    /// An RFC-0014 or RFC-0044 I/O builtin, a directory listing, or `parse`: one runtime function
    /// that writes its whole result through a slot allocated here, the hidden
    /// destination an aggregate-returning call gets (`wasm_sig`). The
    /// destination leads, the operands follow, then what the function needs
    /// after them. Under a generation
    /// (`Cx::gen`) the two readers are their host twins, which take the host's
    /// read mode after the path and read through the loader's resolver rather
    /// than `path_open` (RFC-0076 M7).
    ///
    /// `operand` writes argument `i` at the type asked for. The arm over the
    /// source and [`Fn_::core_call`] over the rows both call this.
    fn slot_call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        operand: &mut dyn FnMut(
            &mut Self,
            &mut Module,
            &mut Frame,
            usize,
            &Type,
        ) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
        let Some(ty) = slot_arity(name).and_then(|n| slot_ty(name, n)) else {
            return unsupported(&format!("`{name}` as an I/O builtin"), line);
        };
        let l = self.layout_of(&ty, line)?;
        let off = b.alloc(l.size, l.align);
        b.slot(off);
        let rt = self.cx.rt;
        let gen = self.cx.gen.is_some();
        let halves = |b: &mut Frame, (pre, post): (u32, u32)| {
            b.ins(&Instruction::I32Const(pre as i32))
                .ins(&Instruction::I32Const(post as i32));
        };
        let f = match name {
            "args" => rt.args,
            "readLine" => {
                b.ins(&Instruction::I32Const(rt.utf8d as i32));
                rt.read_line
            }
            "readFile" => {
                operand(self, m, b, 0, &Type::Str)?;
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
                operand(self, m, b, 0, &Type::Str)?;
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
            "parse" => {
                operand(self, m, b, 0, &Type::Str)?;
                rt.parse_i64
            }
            // `listDir` (RFC-0021) and `listDirKinds` (RFC-0119). Where the
            // listing comes from is the twin called: the generator host's
            // resolver under a generation, told the host's list mode, and
            // WASI's `fd_readdir` on an ordinary build (RFC-0125 §3 M5), told
            // whether names carry kinds, so `vyrn run` lists the real
            // filesystem.
            "listDir" | "listDirKinds" => {
                operand(self, m, b, 0, &Type::Str)?;
                let kinds = name == "listDirKinds";
                let f = if gen {
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
                halves(b, rt.listerr);
                f
            }
            "fsyncFile" => {
                operand(self, m, b, 0, &Type::Str)?;
                halves(b, rt.writeerr);
                rt.fsync_file
            }
            // RFC-0111: the array arrives as a POINTER to its `{ ptr, len, cap }`
            // record, so the two words are loaded out of it and pushed
            // separately. The buffer may hold NULs, which the String writer's
            // `strlen` could not have measured.
            "writeFileBytes" => {
                operand(self, m, b, 0, &Type::Str)?;
                let bytes = Type::Array(Box::new(Type::IntN {
                    bits: 8,
                    signed: false,
                }));
                operand(self, m, b, 1, &bytes)?;
                let src = self.scratch(b, ValType::I32, 0);
                let al = self.layout_of(&bytes, line)?;
                b.ins(&Instruction::LocalSet(src));
                b.ins(&Instruction::LocalGet(src));
                b.ins(&Instruction::I32Load(word_at(al.fields[0])));
                b.ins(&Instruction::LocalGet(src));
                b.ins(&Instruction::I64Load(at(al.fields[1])));
                b.ins(&Instruction::I32WrapI64);
                halves(b, rt.writeerr);
                rt.write_file_bytes
            }
            // `renameFile` differs from `writeFile` only in the cross-device
            // message and the runtime function.
            _ => {
                operand(self, m, b, 0, &Type::Str)?;
                operand(self, m, b, 1, &Type::Str)?;
                halves(b, rt.writeerr);
                if name == "renameFile" {
                    halves(b, rt.xdeverr);
                    rt.rename_file
                } else {
                    rt.write_file
                }
            }
        };
        b.ins(&Instruction::Call(f));
        b.slot(off);
        Ok(ty)
    }

    /// `bytes(s)` — the string's UTF-8 bytes as an `Array<UInt8>`, i8 stride.
    /// A copy, because the array is growable and the string is not: a `push`
    /// on the result must not write into the string's storage.
    /// `bytes(s)` and `bytes(s, start, end)` (RFC-0113). One arm: the
    /// three-argument form differs only in where the copy starts and how
    /// long it is, and `MemoryCopy` does not care which.
    ///
    /// `operand` writes argument `i` at the type asked for; `ranged` is the
    /// three-argument form.
    fn bytes_of(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        ranged: bool,
        operand: &mut dyn FnMut(
            &mut Self,
            &mut Module,
            &mut Frame,
            usize,
            &Type,
        ) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
        let ty = Type::Array(Box::new(Type::IntN {
            bits: 8,
            signed: false,
        }));
        let l = self.layout_of(&ty, line)?;
        operand(self, m, b, 0, &Type::Str)?;
        let s = self.scratch(b, ValType::I32, 0);
        let n = self.scratch(b, ValType::I32, 1);
        let buf = self.scratch(b, ValType::I32, 2);
        let from = self.scratch(b, ValType::I32, 3);
        let malloc = self.cx.rt.malloc;
        b.ins(&Instruction::LocalTee(s));
        if ranged {
            // `start` and `end` as i32 offsets, bounds checked against
            // the string's length before either is used. The wording is
            // `s[i]`'s, so the trap catalogue does not grow.
            str_len(b);
            let len = self.scratch(b, ValType::I32, 4);
            b.ins(&Instruction::LocalSet(len));
            operand(self, m, b, 1, &Type::Int)?;
            b.ins(&Instruction::I32WrapI64);
            b.ins(&Instruction::LocalSet(from));
            operand(self, m, b, 2, &Type::Int)?;
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
        Ok(ty)
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

    /// Lower one statement from the core's rows (RFC-0125 §2.3).
    fn stmt(&mut self, m: &mut Module, b: &mut Frame, s: &Stmt) -> Result<(), String> {
        if self.core_took(m, b, s)? {
            return Ok(());
        }
        let what = format!("a statement of `{}` the core did not state", self.owner);
        unsupported(&what, s.line())
    }

    /// Where a new binding of representation `r` lives.
    fn place_for(&mut self, b: &mut Frame, r: &Repr, line: usize) -> Result<Place, String> {
        Ok(match r {
            Repr::Scalar(v) => Place::Local(b.local(*v)),
            Repr::Agg(l) => Place::Slot(b.alloc(l.size, l.align)),
            Repr::Unit => return unsupported("a binding of a Unit value", line),
        })
    }

    /// Give the accumulator in wasm local `l` its ownership word, in the frame
    /// slot at `at`.
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
    /// unowned again. Both walks call it at the accumulator's `let`, keyed by
    /// its node `site`.
    ///
    /// It starts OWNED when this `let` owns its initializer, which is the fact
    /// `own` already decided. Starting it unowned abandoned the initializer's
    /// buffer at the first append (Phase 4c). Starting it owned for a binding
    /// that names somebody else's storage (`let mut s = r.name` is a borrow,
    /// not a move) would free that storage instead, which is why the answer is
    /// read rather than assumed. A `literal` initializer is somebody else's
    /// storage too: `let mut acc = ""` is released (a placed row answers for
    /// the buffer the loop ENDS on) and its first append would otherwise grow
    /// a data segment address in place.
    fn str_append_shadow(&mut self, b: &mut Frame, l: u32, at: u32, site: usize, literal: bool) {
        let owns = !literal && self.releases_whole(site);
        self.str_append.insert(l, at);
        b.slot(at)
            .ins(&Instruction::I32Const(owns as i32))
            .ins(&Instruction::I32Store(word()));
    }

    /// `s = s + a + b` grown in place: one runtime `strAppend` per part into
    /// `place`, whose ownership word is at `own` (RFC-0125 M7, the `@strAppend`
    /// row; the AST arm's spine calls it too). `operand` pushes part `i` and
    /// hands back a String temporary to free once the part is copied.
    ///
    /// When the word says the buffer is not this path's, the first append
    /// COPIES out of it and abandons it. That is right for a borrow and a leak
    /// when the store that put the buffer there was owned, because a general
    /// store resets the word (exit-residue round sixteen). So where
    /// `owned_here`, the word and the incoming pointer are saved before the
    /// appends and the old buffer is freed after them, if the take ran (the
    /// saved word was 0) and the buffer is heap: an interned literal's `cap`
    /// is `u32::MAX` and is nobody's to free.
    ///
    /// The helper hands back the pointer to store, because a wasm local has
    /// no address to write through (RFC-0081). A global is stored to a fixed
    /// address, which goes down BEFORE the call so the result lands on top.
    #[allow(clippy::too_many_arguments)]
    fn append_in_place(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        place: Place,
        own: Place,
        owned_here: bool,
        parts: usize,
        operand: &mut dyn FnMut(
            &mut Self,
            &mut Module,
            &mut Frame,
            usize,
        ) -> Result<Option<u32>, String>,
        line: usize,
    ) -> Result<(), String> {
        let taken = if owned_here {
            let f0 = b.local(ValType::I32);
            let op = b.local(ValType::I32);
            own.addr(b, 0)
                .ok_or_else(|| gap("an append flag with no address", line))?;
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
                Place::Slot(_) => return unsupported("an in-place append into a slot", line),
            }
            b.ins(&Instruction::LocalSet(op));
            Some((f0, op))
        } else {
            None
        };
        // A CALL-producer part's `Released` row is teed by `expr` into
        // `arg_frees`, and this path is a consumer that drains it
        // (exit-residue round five, herofield's per-glyph temporary).
        let mark = self.arg_frees.len();
        for i in 0..parts {
            // The append COPIES the operand into the accumulator, so an
            // operand this statement allocated is released after it
            // (RFC-0096 M3).
            match place {
                Place::Local(l) => {
                    own.addr(b, 0)
                        .ok_or_else(|| gap("an append flag with no address", line))?;
                    b.ins(&Instruction::LocalGet(l));
                    let k = operand(self, m, b, i)?;
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
                    let k = operand(self, m, b, i)?;
                    b.ins(&Instruction::Call(self.cx.rt.str_append));
                    b.ins(&Instruction::I32Store(word()));
                    self.free_str_temp(b, k);
                }
                Place::Slot(_) => return unsupported("an in-place append into a slot", line),
            }
        }
        for (l, t2) in self.arg_frees.split_off(mark) {
            self.free_arg_temp(m, b, l, &t2, line)?;
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

    /// Reconcile the value on the stack, of type `from`, into `to`, by the rung
    /// [`crate::coerce_plan`] places for the pair.
    ///
    /// **The seam** (RFC-0077 M2d). Before this the backend had no coercion
    /// concept at all: it lowered when `repr()` already agreed on both sides and
    /// [`Cx::ty_gap`] refused everything needing reconciliation — which is why a
    /// validated type, a `modify` parameter, a `SmallArray`, a `Map` index and a
    /// two-word `Option` payload were five gaps rather than one absence wearing
    /// five hats. Every flow site reaches here: a typed `let`, an assignment, a
    /// field or element store, a call argument, a return, a join arm, an enum
    /// payload. A reconciliation added here is added at all
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

    /// Whether the checker proved `e` a value of `to` ([`vyrn_frontend::validate::proven`]),
    /// with this walk's scope resolving a name.
    fn proven(&self, e: &Expr, to: &Type) -> bool {
        let resolve = |x: &Expr| match x {
            Expr::Var { name, .. } => self.lookup(name, 0).ok().map(|(_, t)| t),
            _ => None,
        };
        vyrn_frontend::validate::proven(e, to, &self.cx.types, &resolve)
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
        let name = vyrn_frontend::ctor::ctor_name(&decl.name);
        if let Some(held) = self.call_generated(b, decl, &name, "constructor", line)? {
            b.ins(&Instruction::LocalGet(held));
        }
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
        let name = vyrn_frontend::ctor::pred_name(&decl.name);
        self.call_generated(b, decl, &name, "predicate", line)
    }

    /// Park the value on the stack and hand it to the generated function
    /// `name`, giving back the local it was parked in — or `None`, stack
    /// untouched, for a type with no refinement.
    ///
    /// The two callers above are the same three instructions over two
    /// `what` is the one word the two callers' refusals differ by.
    fn call_generated(
        &mut self,
        b: &mut Frame,
        decl: &TypeDecl,
        name: &str,
        what: &str,
        line: usize,
    ) -> Result<Option<u32>, String> {
        if decl.predicate.is_none() {
            return Ok(None);
        }
        let held = self.park_for_predicate(b, decl, line)?;
        let Some(sig) = self.cx.sigs.get(name) else {
            return unsupported(
                &format!(
                    "a `where` clause on `{}` with no {what} in the link",
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
    /// parameter of that type is passed as ([`Fn_::emit_call_with`]), so one local
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
            // A record literal names its type; `applied_record` solves the
            // parameters an unannotated generic literal leaves open.
            Expr::StructLit { name, fields, line } => self.applied_record(name, fields, *line)?,
            Expr::Call { name, args, .. } => match name.as_str() {
                // `blackBox(x)` is `x` (RFC-0055): a bench body that ends in it
                // yields the argument's type.
                "blackBox" if args.len() == 1 => self.peek(&args[0], line)?,
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

    /// A String operator, both operands on the stack: `+` concatenates into
    /// the arena a `region` routes to, and a comparison is the sign of a byte
    /// compare. [`Fn_::core_prim`] reads it.
    fn str_bin(&mut self, b: &mut Frame, op: BinOp, line: usize) -> Result<Type, String> {
        if op == BinOp::Add {
            self.arena_route(b, true);
            b.ins(&Instruction::Call(self.cx.rt.concat));
            self.arena_route(b, false);
            return Ok(Type::Str);
        }
        b.ins(&Instruction::Call(self.cx.rt.strcmp));
        b.ins(&Instruction::I32Const(0));
        b.ins(&cmp_i32(op).ok_or_else(|| gap(&format!("`{op:?}` on strings"), line))?);
        Ok(Type::Bool)
    }

    /// `s =~ pat`, the String on the stack: the pattern's DFA, compiled once
    /// (RFC-0046), and the runtime's walk over it, which leaves a `Bool`.
    fn str_match(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        pat: &str,
        line: usize,
    ) -> Result<(), String> {
        let (table, accept, start) = self.regex_dfa(m, pat, line)?;
        b.ins(&Instruction::I32Const(table as i32));
        b.ins(&Instruction::I32Const(start as i32));
        b.ins(&Instruction::I32Const(accept as i32));
        b.ins(&Instruction::Call(self.cx.rt.regex_run));
        Ok(())
    }

    /// The instruction a unary operator IS, once its operand stands on the
    /// stack — RFC-0125 §2.3's "maps `prim` rows to wasm instructions".
    ///
    /// [`Fn_::core_prim`] reads the operator off [`vyrn_lower::core::Op`].
    /// Nothing here interleaves the
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
    /// [`Fn_::core_prim`] reads the operator off [`vyrn_lower::core::Op`].
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

    /// A generator host import ([`Spec::Host`]): the operands at the types
    /// the import takes, and the call. The arm over the source and
    /// [`Fn_::core_call`] over the rows both call this.
    ///
    /// `ty` answers operand `i`'s own type without writing it, and `operand`
    /// writes operand `i` at the type given.
    fn host(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        argc: usize,
        ty: &mut dyn FnMut(&mut Self, usize) -> Result<Type, String>,
        operand: &mut dyn FnMut(
            &mut Self,
            &mut Module,
            &mut Frame,
            usize,
            &Type,
        ) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
        let Some(g) = self.cx.gen else {
            return unsupported(&format!("`{name}` outside a generator"), line);
        };
        let code = Type::Named("Code".to_string());
        match (name, argc) {
            // `raw(s)` IS `@codeText(s)` in the interpreter — one verbatim piece,
            // no origin — so it is the same import.
            ("@codeText", 1) | ("raw", 1) => {
                operand(self, m, b, 0, &Type::Str)?;
                b.ins(&Instruction::Call(g.text));
                Ok(code)
            }
            ("rawAt", 4) => {
                operand(self, m, b, 0, &Type::Str)?;
                operand(self, m, b, 1, &Type::Str)?;
                operand(self, m, b, 2, &Type::Int)?;
                operand(self, m, b, 3, &Type::Int)?;
                b.ins(&Instruction::Call(g.raw_at));
                Ok(code)
            }
            // The host renders and stashes; the guest asks for the length,
            // allocates, and fetches — the same protocol every host result uses,
            // because the host must not allocate inside guest memory.
            ("render", 1) => {
                operand(self, m, b, 0, &code)?;
                b.ins(&Instruction::Call(g.render));
                self.fetch_str(b, g);
                Ok(Type::Str)
            }
            // M3b's atom stream. `reflect` computes the value host-side and
            // leaves it as atoms; the two `next` calls pull them back. Nothing
            // about the value's SHAPE is encoded here: the synthesized decoder
            // walks the type, and so does the host.
            (crate::GEN_REFLECT, 2) => {
                operand(self, m, b, 0, &Type::Int)?;
                operand(self, m, b, 1, &Type::Str)?;
                b.ins(&Instruction::Call(g.reflect));
                Ok(Type::Unit)
            }
            (crate::GEN_NEXT_INT, 0) => {
                b.ins(&Instruction::Call(g.next_int));
                Ok(Type::Int)
            }
            (crate::GEN_NEXT_STR, 0) => {
                b.ins(&Instruction::Call(g.next_str));
                self.fetch_str(b, g);
                Ok(Type::Str)
            }
            // `Code + Code` concatenates fragments with their origins carried
            // (RFC-0054), in the HOST's arena (RFC-0076 M3a).
            ("+", 2) => {
                operand(self, m, b, 0, &code)?;
                operand(self, m, b, 1, &code)?;
                b.ins(&Instruction::Call(g.concat));
                Ok(code)
            }
            // The spliced value crosses as a TAG plus one 64-bit word (plus a
            // pointer when it is a String), because the host needs the value itself
            // and cannot chase a guest pointer to anything else. The tag names the
            // interpreter `Val` the host rebuilds and is a COMPILE-TIME constant —
            // the static type is known here — so there is no runtime dispatch.
            //
            // The tag is the FIRST argument on the stack and the value is the
            // second, so the type is asked before the value is written.
            ("@codeSplice", 2) => {
                let vty = ty(self, 0)?;
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
                    operand(self, m, b, 0, &Type::Str)?;
                } else {
                    operand(self, m, b, 0, &vty)?;
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
                operand(self, m, b, 1, &Type::Int)?;
                b.ins(&Instruction::Call(g.splice));
                Ok(code)
            }
            _ => unsupported(&format!("`{name}` at {argc} operands"), line),
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
    /// A call by INDEX rather than by name: the value is
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

    /// Whether this build drops the call to `name` rather than emitting it.
    ///
    /// RFC-0114 §25: the residue instrument's hooks are calls only in an
    /// audited build. The arm drops the whole expression, operand included,
    /// and an unaudited core states no row for either (RFC-0125 M7). A
    /// generator's build is never audited, so its core can still state a
    /// hook, and [`Fn_::core_sig`] refuses that row.
    fn audit_dropped(&self, name: &str) -> bool {
        !self.cx.audit && vyrn_frontend::loader::audit_hook(name)
    }

    /// Whether `name` is an `extern fn` (RFC-0012) or one of RFC-0043's
    /// host-boundary names, which [`Fn_::extern_call`] writes.
    fn is_extern(&self, name: &str) -> bool {
        crate::host_boundary_extern(name).is_some() || self.cx.externs.contains_key(name)
    }

    /// One call to an `extern fn` or a host-boundary name, `argc` operands
    /// written by `operand` at each parameter's type. [`Fn_::core_call`]
    /// calls it over the rows.
    fn extern_call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        argc: usize,
        operand: &mut dyn FnMut(
            &mut Self,
            &mut Module,
            &mut Frame,
            usize,
            &Type,
        ) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
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
            if argc != 0 {
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
            if ext.params.len() != argc {
                return unsupported(&format!("the call `{name}` at this arity"), line);
            }
            for (i, p) in ext.params.iter().enumerate() {
                operand(self, m, b, i, p)?;
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
        unsupported(&format!("the call `{name}`"), line)
    }

    /// `print` of the value on the stack, rendered by its own type `t`. Both
    /// walks call it: the arm over the source after it evaluates the operand,
    /// and [`Fn_::core_call`] after it reads the name (RFC-0125 M7).
    fn print_value(&mut self, b: &mut Frame, t: &Type, line: usize) -> Result<(), String> {
        match self.cx.resolve(t) {
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
        Ok(())
    }

    /// `toString` of the value on the stack, rendered by its own type `t` into
    /// a String the caller owns. `arg` is the operand's expression where the
    /// arm over the source has one: a String temporary is freed once it is
    /// copied ([`vyrn_frontend::declared::str_temporary`]), and the rows state
    /// that release as a row of their own.
    fn str_value(
        &mut self,
        b: &mut Frame,
        t: &Type,
        arg: Option<&Expr>,
        line: usize,
    ) -> Result<(), String> {
        match self.cx.resolve(t) {
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
                let k = arg.and_then(|a| self.tee_str_temp(b, a));
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
        Ok(())
    }

    /// One `std/mem` primitive off the core's row: [`Fn_::mem_spec`]'s table,
    /// its operands read from the row (RFC-0125 M7).
    fn core_mem(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        prim: &str,
        args: &[(Val, vyrn_frontend::ast::Capability)],
        line: usize,
    ) -> Result<Type, String> {
        let (how, params, ret) = self.mem_spec(prim, line)?;
        if args.len() != params.len() {
            return unsupported(&format!("`std/mem.{prim}` at this arity"), line);
        }
        mem_pre(b, prim);
        for ((v, _), p) in args.iter().zip(&params) {
            self.core_val(m, b, body, w, v, p, line)?;
        }
        self.mem_ins(b, prim, how);
        Ok(ret)
    }

    /// What one `std/mem` primitive takes and hands back, and how it lowers.
    ///
    /// The argument types are spelled here as well as in the module because
    /// each caller needs the target type to coerce a literal, and this is the
    /// one place the two lists meet. A mismatch is a checker error at the call
    /// in `std/runtime` before it is anything here.
    ///
    /// # Errors
    ///
    /// Refuses a name `std/mem` does not declare.
    fn mem_spec(&self, prim: &str, line: usize) -> Result<(Mem, Vec<Type>, Type), String> {
        let u = |bits: u8| Type::IntN {
            bits,
            signed: false,
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
            return Ok((Mem::Host(index), params, ret));
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
        Ok((Mem::Ins, params, ret))
    }

    /// The instructions one `std/mem` primitive IS, its operands already on
    /// the stack.
    fn mem_ins(&mut self, b: &mut Frame, prim: &str, how: Mem) {
        let at = |align: u32| MemArg {
            offset: 0,
            align,
            memory_index: 0,
        };
        if let Mem::Host(index) = how {
            match index {
                Some(i) => b.ins(&Instruction::Call(i)),
                None => b.ins(&Instruction::Unreachable),
            };
            return;
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
            _ => unreachable!("`mem_spec` answered, so the name is one of these"),
        };
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
    /// syscall either way.
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

    /// The first half of the aggregate-result convention, which both walks
    /// read (RFC-0125 M7): the out-pointer a call's result is written
    /// through, pushed before the arguments.
    ///
    /// The storage is the consumer's own when it holds this very type, so the
    /// callee's `return` lands there and no slot is taken (RFC-0125 M1), and a
    /// slot of this frame otherwise. `None` for a result that is not an
    /// aggregate. [`Fn_::out_ptr_back`] is the second half.
    fn out_ptr(
        &mut self,
        b: &mut Frame,
        sig: &Sig,
        hint: Option<(Dest, Type)>,
    ) -> Option<(Dest, bool)> {
        let l = sig.ret.agg()?;
        let (d, used) = match hint {
            Some((d, t)) if self.cx.ll(&t) == self.cx.ll(&sig.ret_ty) => (d, true),
            _ => (Dest::Slot(b.alloc(l.size, l.align)), false),
        };
        d.addr(b, 0);
        Some((d, used))
    }

    /// The second half: after the call, the out-pointer again as the value,
    /// and `dest_used` says whether it was the consumer's storage.
    fn out_ptr_back(&mut self, b: &mut Frame, dest: Option<(Dest, bool)>) {
        if let Some((d, used)) = dest {
            d.addr(b, 0);
            self.dest_used = used;
        }
    }

    /// The instance of the generic `f` that arguments of `arg_tys`, and the
    /// expected result `want`, solve, which hands out its function index.
    fn generic_sig(
        &mut self,
        m: &mut Module,
        f: &'p Function,
        arg_tys: &[Type],
        want: Option<&Type>,
        line: usize,
    ) -> Result<Sig, String> {
        let (subst, solved) = crate::solve_with_expected(
            &f.type_params,
            &f.params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>(),
            arg_tys,
            &f.ret,
            want,
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
                            "a generic type parameter `{tp}` the call `{}` does not fix",
                            f.name
                        ),
                        line,
                    )
                }
            }
        }
        self.cx.instantiate(m, f, type_args, subst)
    }

    /// A call once the callee's signature is known, with argument `i` written
    /// by `operand` at its parameter's type, which answers the slot a `modify`
    /// scalar was spilled to, if any: the out-pointer, the operands, the call,
    /// the reloads.
    fn emit_call_with(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        sig: &Sig,
        argc: usize,
        operand: &mut dyn FnMut(
            &mut Self,
            &mut Module,
            &mut Frame,
            usize,
            &Type,
        ) -> Result<Option<Spill>, String>,
        hint: Option<(Dest, Type)>,
    ) -> Result<Type, String> {
        let dest = self.out_ptr(b, sig, hint);
        let mut spilled = Vec::new();
        for (i, p) in sig.params.iter().take(argc).enumerate() {
            spilled.extend(operand(self, m, b, i, p)?);
        }
        b.ins(&Instruction::Call(sig.index));
        reload(b, &spilled);
        self.out_ptr_back(b, dest);
        // The DECLARED return type, not its structural form. Resolving here threw
        // away exactly the information a caller needs to solve a further generic:
        // a `Pair<Int64, Int64>` reduced to its record shape no longer matches
        // `Pair<A, B>`, so `firstOf(twice(41))` could not fix `A`. The textual
        // emitter returns the declared type for the same reason.
        Ok(sig.ret_ty.clone())
    }

    /// Spill the scalar in the local `l` to a slot of its own and push the
    /// slot's address, which a `modify` parameter takes and a wasm local does
    /// not have. [`reload`] writes the slot back into `l` after the call.
    fn spill(&self, b: &mut Frame, l: u32, ty: &Type, line: usize) -> Result<Spill, String> {
        let Repr::Scalar(_) = self.cx.repr(ty, line)? else {
            return unsupported("a `modify` argument in a local", line);
        };
        let ll = self.cx.ll(ty);
        let l2 = layout::of_ll(&ll).map_err(|e| format!("direct backend: {e}"))?;
        let off = b.alloc(l2.size, l2.align);
        b.slot(off);
        b.ins(&Instruction::LocalGet(l));
        b.ins(&store_of(&ll));
        b.slot(off);
        Ok((off, l, ll, self.cx.signed(ty)))
    }

    // ---- RFC-0023 higher-order specialization -----------------------------

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
        params: &[Binder],
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
                        .push((pn.name.clone(), Place::Local(u32::MAX), pt.clone()));
                }
                let got = self.peek(e, line);
                self.scope.truncate(mark);
                self.cx.sub(&got?)
            }
            (Type::Param(_), LambdaBody::Block(_)) => Type::Unit,
            (t, _) => t.clone(),
        })
    }

    /// The PROGRAM's own body for a lambda literal at this address, or `None`
    /// for a literal the program does not hold — see [`Cx::lambdas`].
    fn lambda(&self, at: &Expr) -> Option<&'p LambdaBody> {
        match self.cx.lambdas.get(&(at as *const Expr as usize))?.1 {
            Expr::Lambda { body, .. } => Some(body),
            _ => None,
        }
    }

    /// The literal this body holds under the key a closure row names
    /// ([`vyrn_lower::core::lambda_spelling`]).
    fn literal(&self, key: &str) -> Option<&'p Expr> {
        self.cx.lambdas.values().find_map(|(o, e)| match e {
            Expr::Lambda { line, col, .. }
                if *o == self.owner
                    && vyrn_lower::core::lambda_spelling(&self.core_key, *line, *col) == key =>
            {
                Some(*e)
            }
            _ => None,
        })
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
        ptys: &[Type],
        expected_ret: &Type,
        caps: Option<&[(String, Type)]>,
        line: usize,
    ) -> Result<(FnTarget, Vec<Expr>, Vec<Type>), String> {
        // The key below is the literal's address. A literal the program does
        // not hold dies with its tree, and a later one could land where it was,
        // so the key names a copy this `Cx` keeps.
        let kept = self.lambda(at).is_none().then(|| self.cx.keep(at.clone()));
        let at = kept.as_deref().unwrap_or(at);
        let Expr::Lambda {
            params,
            body,
            line: at_line,
            col: at_col,
        } = at
        else {
            return unsupported("a lambda lifted from another expression", line);
        };
        if params.len() != ptys.len() {
            return unsupported("a lambda with the wrong number of parameters", line);
        }
        // The free locals, in first-seen order — the SHARED walk (`lib.rs`),
        // because a capture list is part of the lifted function's signature and two
        // backends disagreeing about its length would emit calls with the wrong
        // number of arguments.
        //
        // A literal lifted from a core row takes the captures the row names,
        // which the same walk computed where the literal stands: the frame
        // lifts it before its first statement, where no capture is in scope.
        let (cap_names, mut cap_tys) = match caps {
            Some(c) => (
                c.iter().map(|(n, _)| n.clone()).collect(),
                c.iter().map(|(_, t)| self.cx.sub(t)).collect(),
            ),
            None => (
                vyrn_lower::core::lambda_captures(
                    body,
                    params.iter().map(|p| p.name.clone()).collect(),
                    &|n| self.scope.iter().any(|(s, _, _)| s == n) || self.fn_binds.contains_key(n),
                ),
                Vec::new(),
            ),
        };
        for cn in cap_names.iter().skip(cap_tys.len()) {
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
        let queued = match self.lambda(at) {
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
                line: 0,
                col: 0,
            })
            .chain(params.iter().zip(ptys).map(|(n, t)| Param {
                name: n.name.clone(),
                capability: Capability::Read,
                ty: t.clone(),
                line: n.line,
                col: n.col,
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
            vyrn_lower::core::lambda_spelling(&self.core_key, *at_line, *at_col),
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

    /// Write a stored function value into `dest`: its tag, and its payload.
    /// `caps` are the captures in the target's order.
    #[allow(clippy::too_many_arguments)]
    fn fnval_into(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        sig_ty: &Type,
        target: FnTarget,
        caps: &mut Parts,
        line: usize,
    ) -> Result<(), String> {
        let cap_tys = target.sig.params[..target.ncaps].to_vec();
        if cap_tys.len() != caps.len() {
            return unsupported(
                "a function value whose captures do not match its target",
                line,
            );
        }
        let tag = self.register_fnval(sig_ty, &target);
        let Repr::Agg(l) = self.cx.repr(sig_ty, line)? else {
            return unsupported("a function value that is not an aggregate", line);
        };
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
            for (i, ty) in cap_tys.iter().enumerate() {
                // The snapshot OWNS its heap (RFC-0114 §25 round three, the
                // textual backend's rule, mirrored here in round fifty-seven):
                // a heap capture READ OUT OF A PLACE is duplicated into the
                // block, which is what lets the release twin walk it and the
                // captured binding keep releasing its own value at block exit.
                //
                // A capture the source MINTS is already the block's, and
                // duplicating it orphans what was minted. That is the one such
                // source: a `fn`-typed parameter inside a specialization has no
                // slot, so reading its name BUILDS the aggregate (RFC-0023 ×
                // RFC-0037, [`Fn_::fnval_binding`]) rather than reading one.
                let dup = self.owns_heap(ty);
                b.ins(&Instruction::LocalGet(p));
                if bl.fields[i] != 0 {
                    b.ins(&Instruction::I32Const(bl.fields[i] as i32));
                    b.ins(&Instruction::I32Add);
                }
                self.part(m, b, caps, i, ty, line)?;
                match self.cx.repr(ty, line)? {
                    Repr::Scalar(_) => {
                        if dup {
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
                        if dup {
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
        dest.addr(b, l.fields[0]);
        b.ins(&Instruction::I64Const(tag));
        b.ins(&Instruction::I64Store(word8()));
        dest.addr(b, l.fields[1]);
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
        Ok(())
    }

    /// The target a stored lambda calls, and its captures in the target's
    /// order, for [`Fn_::core_make`].
    fn lift_stored(
        &mut self,
        m: &mut Module,
        e: &Expr,
        sig_ty: &Type,
    ) -> Result<(FnTarget, Vec<Expr>), String> {
        let Expr::Lambda { line, .. } = e else {
            return unsupported("a function value from a non-lambda", Expr::line(e));
        };
        let Type::Fn(ptys, ret) = sig_ty else {
            return unsupported("a lambda in a non-function position", *line);
        };
        // The expected-type stack must not leak into the lifted body: its own
        // storage boundaries push their own types.
        let saved = std::mem::take(&mut self.expect);
        let r = self.lift_lambda(m, e, ptys, ret, None, *line);
        self.expect = saved;
        r.map(|(target, srcs, _)| (target, srcs))
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
    ///
    /// `operand` emits the `i`th argument: the two cursor words at `Int64`, and
    /// the step at its own type, which it answers.
    fn stream_from_step(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        operand: &mut dyn FnMut(&mut Self, &mut Module, &mut Frame, usize) -> Result<Type, String>,
        line: usize,
    ) -> Result<Type, String> {
        // Written argument order — the interpreter evaluates `slot`, `gen` and
        // the step left to right, so their effects (and any trap) happen in
        // that order here too. The step's SIGNATURE names the element type, but
        // its value is not needed until the header exists, so the two cursor
        // words wait in locals while the step is evaluated.
        operand(self, m, b, 0)?;
        let c0 = b.local(ValType::I64);
        b.ins(&Instruction::LocalSet(c0));
        operand(self, m, b, 1)?;
        let c1 = b.local(ValType::I64);
        b.ins(&Instruction::LocalSet(c1));
        let fty = operand(self, m, b, 2)?;
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
    /// `addr` emits the box's address as an `Int64`.
    fn stream_box_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        addr: &mut dyn FnMut(&mut Self, &mut Module, &mut Frame) -> Result<(), String>,
    ) -> Result<u32, String> {
        let a = b.local(ValType::I32);
        addr(self, m, b)?;
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
        operand: &mut dyn FnMut(
            &mut Self,
            &mut Module,
            &mut Frame,
            Option<&Type>,
        ) -> Result<Type, String>,
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
        let got = operand(self, m, b, None)?;
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
        elem: &Type,
        addr: &mut dyn FnMut(&mut Self, &mut Module, &mut Frame) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
        let sl = self.stream_layout(line)?;
        let a = self.stream_box_at(m, b, addr)?;
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
        Ok(Type::Stream(Box::new(elem.clone())))
    }

    /// `pullAt(a)`: one element from the stream in that box (RFC-0075 M2c),
    /// which is the whole of what a wrapper's step can do that an ordinary
    /// producer's cannot. The element type is the annotation's: an address is an
    /// `Int64` whatever it addresses.
    fn stream_pull_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        elem: &Type,
        addr: &mut dyn FnMut(&mut Self, &mut Module, &mut Frame) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
        let opt = Type::option(elem.clone());
        let Repr::Agg(ol) = self.cx.repr(&opt, line)? else {
            return unsupported("an Option that is not an aggregate", line);
        };
        let a = self.stream_box_at(m, b, addr)?;
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
    /// `w0` (RFC-0075 M2c), from a place, which is what `pull` has.
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
        let some = Pattern::Variant("Some".into(), vec![Binder::synthetic("")]);
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

    /// The line a `panic` writes, and the call that traps after it: `error: `,
    /// the message `msg` pushes, and the site where the call names one
    /// (RFC-0125 M7). The arm over the source and [`Fn_::core_call`] both
    /// write it, and the `unreachable` after it is the arm's own and the
    /// core's [`St::Trap`].
    ///
    /// Both wordings are interned, and the local taken, before the message is
    /// pushed, so the two walks lay out the same data and the same locals. The
    /// message waits in the local because `write_all` consumes three operands,
    /// and it is pushed first because the other engines evaluate the argument
    /// before they write any byte of the line.
    fn panic_line(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        at: Option<&str>,
        msg: impl FnOnce(&mut Self, &mut Module, &mut Frame) -> Result<(), String>,
    ) -> Result<(), String> {
        let write_all = self.cx.rt.write_all;
        let tail = at.map_or_else(|| "\n".to_string(), |at| format!(" ({at})\n"));
        let (pre, nl) = (self.cx.rt.intern(m, "error: "), self.cx.rt.intern(m, &tail));
        let slot = self.scratch(b, ValType::I32, 7);
        msg(self, m, b)?;
        b.ins(&Instruction::LocalSet(slot));
        b.ins(&Instruction::I32Const(2))
            .ins(&Instruction::I32Const(pre as i32))
            .ins(&Instruction::I32Const(7))
            .ins(&Instruction::Call(write_all))
            .ins(&Instruction::Drop);
        b.ins(&Instruction::I32Const(2))
            .ins(&Instruction::LocalGet(slot));
        b.ins(&Instruction::LocalGet(slot));
        str_len(b);
        b.ins(&Instruction::Call(write_all)).ins(&Instruction::Drop);
        b.ins(&Instruction::I32Const(nl as i32))
            .ins(&Instruction::Call(self.cx.rt.trap));
        Ok(())
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

    /// The elements of a literal, one after another from `dest` (RFC-0125 M1).
    /// An aggregate element is built in its own place, so a nested literal
    /// costs no frame.
    fn fixed_elems(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        elem: &Type,
        elems: &mut Parts,
        line: usize,
    ) -> Result<(), String> {
        let stride = self.stride(elem, line)?;
        let r = self.cx.repr(elem, line)?;
        for i in 0..elems.len() {
            let at = stride * i as u32;
            match &r {
                Repr::Scalar(_) => {
                    dest.addr(b, at);
                    self.part(m, b, elems, i, elem, line)?;
                    b.ins(&store_of(&self.cx.ll(elem)));
                }
                Repr::Agg(_) => self.agg_part(m, b, elems, i, dest.at(at), stride, elem, line)?,
                Repr::Unit => return unsupported("an array of Unit", line),
            }
        }
        Ok(())
    }

    /// A record literal's fields, each at its layout offset — RFC-0125 M7.
    ///
    /// `order[i]` is the part that fills the `i`th DECLARED field. The layout's
    /// order is the declaration's and a reader writes the fields in whatever
    /// order suits, so the join is by name and the caller makes it: the AST arm
    /// joins its `(name, expr)` pairs and the core's row joins the field names
    /// on [`vyrn_lower::core::Ctor::Record`].
    #[allow(clippy::too_many_arguments)]
    fn record_into(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        decl: &[Field],
        l: &Layout,
        order: &[usize],
        parts: &mut Parts,
        line: usize,
    ) -> Result<(), String> {
        for (i, f) in decl.iter().enumerate() {
            match self.cx.repr(&f.ty, line)? {
                Repr::Scalar(_) => {
                    dest.addr(b, l.fields[i]);
                    self.part(m, b, parts, order[i], &f.ty, line)?;
                    b.ins(&store_of(&self.cx.ll(&f.ty)));
                }
                Repr::Agg(fl) => {
                    let at = dest.at(l.fields[i]);
                    self.agg_part(m, b, parts, order[i], at, fl.size, &f.ty, line)?;
                }
                Repr::Unit => return unsupported("a Unit field", line),
            }
        }
        dest.addr(b, 0);
        Ok(())
    }

    /// One part of a literal, at the type the layout puts it at: the AST arm's
    /// expression, or the value the core's row names (RFC-0125 M7).
    fn part(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        parts: &mut Parts,
        i: usize,
        want: &Type,
        line: usize,
    ) -> Result<(), String> {
        match parts {
            Parts::Core(body, vs, w) => {
                let (body, v) = (*body, &vs[i]);
                self.core_val(m, b, body, w, v, want, line)
            }
        }
    }

    /// One layout part of a literal, left at `dest`, which is the parent's
    /// storage at the part's offset (RFC-0125 M7). The AST arm builds the
    /// expression there. The core's row names the part: a part its own row
    /// wrote there ([`Fn_::core_part_at`]) is left, and any other layout name
    /// is copied from its place, as the arm copies a variable.
    #[allow(clippy::too_many_arguments)]
    fn agg_part(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        parts: &mut Parts,
        i: usize,
        dest: Dest,
        size: u32,
        ty: &Type,
        line: usize,
    ) -> Result<(), String> {
        if parts.built(i).is_some() {
            return Ok(());
        }
        match parts {
            Parts::Core(body, vs, w) => {
                let (body, v) = (*body, &vs[i]);
                dest.addr(b, 0);
                self.core_val(m, b, body, w, v, ty, line)?;
                agg_landed(b, size, false);
                Ok(())
            }
        }
    }

    /// `[a, b, c]` in an `Array<T>` position, built on the heap at once
    /// (RFC-0125 M1): the buffer is taken first, the elements are built in it,
    /// and the `{ptr, len, cap}` triple is written at `dest`. `len` and `cap`
    /// are both N, the schedule [`Fn_::heapify`] gives a literal. The empty
    /// literal is the empty triple, `data` null, as it always was. `taken`
    /// is the buffer a part took before the literal's row
    /// ([`Fn_::core_part_dest`]).
    #[allow(clippy::too_many_arguments)]
    fn array_lit_heap(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        inner: &Type,
        elems: &mut Parts,
        taken: Option<u32>,
        line: usize,
        used: bool,
    ) -> Result<Type, String> {
        let ty = Type::Array(Box::new(inner.clone()));
        let l = self.layout_of(&ty, line)?;
        let n = elems.len();
        let buf = match taken {
            Some(buf) => buf,
            None if n == 0 => {
                let buf = b.local(ValType::I32);
                b.ins(&Instruction::I32Const(0));
                b.ins(&Instruction::LocalSet(buf));
                buf
            }
            None => self.heap_buf(b, self.extent(inner, n, line)?),
        };
        if n > 0 {
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

    /// A heap buffer of `bytes`, in a local of its own.
    fn heap_buf(&mut self, b: &mut Frame, bytes: u32) -> u32 {
        let buf = b.local(ValType::I32);
        b.ins(&Instruction::I64Const(bytes.max(1) as i64));
        b.ins(&Instruction::Call(self.cx.rt.malloc));
        b.ins(&Instruction::LocalSet(buf));
        buf
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
    /// element stride, and the receiver's address parked in a local. That
    /// address is the runtime's `dst` as well as its `src`. Every array
    /// function reads all of `src` before it stores, which step 6 states, so
    /// the new triple lands in the receiver and `xs = @push(xs, v)` copies a
    /// header onto itself. A destination slot of its own cost the store-heavy
    /// rows half their time: the temp forced a store and two reloads the
    /// runtime call already fenced, and wasm2c spells the write-back
    /// `memmove`. `examples/benching.vyrn`'s "push 1000" went 6.52 to 3.47 us.
    fn arr_recv(
        &mut self,
        b: &mut Frame,
        aty: &Type,
        verb: &str,
        line: usize,
    ) -> Result<(Type, Layout, i32, u32), String> {
        let Type::Array(elem) = self.cx.resolve(aty) else {
            return unsupported(&format!("`{verb}` on `{aty}`"), line);
        };
        let l = self.layout_of(aty, line)?;
        let stride = self.stride(&elem, line)? as i32;
        let src = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(src));
        Ok((*elem, l, stride, src))
    }

    /// One array operation `std/runtime` rebuilds the receiver with, the
    /// receiver's address on the stack (RFC-0115, PLAN-0125-runtime section 6 step
    /// 6). Leaves the same address, which is the result: the runtime wrote the
    /// new triple into the receiver ([`Fn_::arr_recv`]). `operand` writes the
    /// second argument at the type asked for; `clear` has none.
    ///
    /// `reserve`, `append` and `copyFrom` take their operand into a local
    /// before the call, so no operand of the call is on the stack while a user
    /// expression runs, and the runtime moves bytes and is handed no type,
    /// because the checker held the element type to heapless ones.
    ///
    /// `push`: `arrPush` grows and writes the new triple with `len + 1`; the
    /// element is stored here, because the runtime knows the stride and not
    /// the type. The old buffer comes back from the call and is released only
    /// after the element is stored, and that is not tidiness. Over the source
    /// the value expression is evaluated BELOW, and it may read the array
    /// being pushed onto — `w.push(rot1(w[t - 3] ^ w[t - 8] …))` in
    /// `std/hash` does, through the caller's header, which still names the
    /// OLD buffer. Freeing at the growth made that a read of a block already
    /// on a free list, and SHA-1 came out wrong from the seventeenth word.
    fn arr_rebuild(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        aty: &Type,
        operand: &mut dyn FnMut(&mut Self, &mut Module, &mut Frame, &Type) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
        if let (Type::SmallArray(inner, n), "@push") = (self.cx.resolve(aty), name) {
            let aty = Type::SmallArray(inner.clone(), n);
            return self.sa_push(m, b, &aty, &inner, n, operand, line);
        }
        let verb = name.trim_start_matches('@');
        let (elem, l, stride, src) = self.arr_recv(b, aty, verb, line)?;
        let rt = &self.cx.rt;
        let (call, arg) = match verb {
            "clear" => (rt.arr_clear, None),
            "reserve" => (rt.arr_reserve, Some((ValType::I64, Type::Int))),
            "append" => (rt.arr_append, Some((ValType::I32, aty.clone()))),
            "copyFrom" => (rt.arr_copy_from, Some((ValType::I32, aty.clone()))),
            "push" => (rt.arr_push, None),
            _ => return unsupported(&format!("`{name}` rebuilds no array"), line),
        };
        let arg = match arg {
            Some((vt, t)) => {
                let x = b.local(vt);
                operand(self, m, b, &t)?;
                b.ins(&Instruction::LocalSet(x));
                Some(x)
            }
            None => None,
        };
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::LocalGet(src));
        if verb != "clear" {
            b.ins(&Instruction::I32Const(stride));
        }
        if let Some(x) = arg {
            b.ins(&Instruction::LocalGet(x));
        }
        b.ins(&Instruction::Call(call));
        if verb == "push" {
            let stale = b.local(ValType::I32);
            b.ins(&Instruction::LocalSet(stale));
            // The element goes at the old length, which the new triple holds
            // plus one.
            let (data, last) = (b.local(ValType::I32), b.local(ValType::I64));
            b.ins(&Instruction::LocalGet(src));
            b.ins(&Instruction::I32Load(word_at(l.fields[0])));
            b.ins(&Instruction::LocalSet(data));
            b.ins(&Instruction::LocalGet(src));
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
            operand(self, m, b, &elem)?;
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
        }
        b.ins(&Instruction::LocalGet(src));
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

    /// `pop` on the array whose address is in the local `slot`. The arm over
    /// the source and [`Fn_::core_call`] over the rows both call this.
    fn pop_at(
        &mut self,
        b: &mut Frame,
        slot: u32,
        aty: &Type,
        line: usize,
    ) -> Result<Type, String> {
        let elem = match self.cx.resolve(aty) {
            Type::Array(elem) => elem,
            Type::SmallArray(..) => return self.sa_pop(b, slot, aty, line),
            _ => return unsupported(&format!("`pop` on `{aty}`"), line),
        };
        let elem = *elem;
        let al = self.layout_of(aty, line)?;
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
        let w = self.walk(b, aty, line)?;
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

    /// `swapRemove` on the array whose address is in the local `slot`, with
    /// `index` writing the index. The arm over the source and
    /// [`Fn_::core_call`] over the rows both call this.
    fn swap_remove_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        slot: u32,
        aty: &Type,
        index: &mut dyn FnMut(&mut Self, &mut Module, &mut Frame) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
        let elem = match self.cx.resolve(aty) {
            Type::Array(elem) => elem,
            Type::SmallArray(..) => return self.sa_swap_remove(m, b, slot, aty, index, line),
            _ => return unsupported(&format!("`swapRemove` on `{aty}`"), line),
        };
        let elem = *elem;
        let al = self.layout_of(aty, line)?;
        b.ins(&Instruction::LocalGet(slot));
        let w = self.walk(b, aty, line)?;
        index(self, m, b)?;
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

    /// Walk the first `count` elements of `buf`, releasing each or giving each
    /// a copy of its own — one loop, both directions, the way
    /// [`Fn_::rel_body`] and [`Fn_::copy_body`] are one walk per type.
    ///
    /// The two gates are different questions and neither contains the other. A
    /// release is gated on the element's own release ROW, which is `own`'s
    /// proof that the container owns the element; walking into an element with
    /// no row would free places no rule says the array owns. A copy is gated on
    /// reachability, because a copy of a value that reaches heap has to
    /// duplicate what it reaches whether or not anything releases it.
    fn each(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        rel: bool,
        buf: u32,
        count: u32,
        stride: u32,
        elem: &Type,
        line: usize,
    ) -> Result<(), String> {
        let walks = if rel {
            self.cx.owned.release_kind(elem).is_some()
        } else {
            self.owns_heap(elem)
        };
        if !walks {
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
        if rel {
            self.rel_at(m, b, p, elem, line)?;
        } else {
            self.copy_at(m, b, p, elem, line)?;
        }
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
                self.each(m, b, false, nb, n, stride, &inner, line)
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
                self.each(m, b, false, base, n, stride, &inner, line)
            }
            Type::Map(kt, vt) => {
                // String keys are dup'd per entry; Int64 and packed keys copy
                // with the buffer (RFC-0117), at their stride, with no
                // per-element walk.
                let mk = self.map_key(&kt, line)?;
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
                let kstride = mk.stride() as u32;
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
                    if i == 1 || mk == MapKey::Str {
                        self.each(m, b, false, nb, n, stride, &elem, line)?;
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
                self.each(m, b, false, a, count, stride, &inner, line)
            }
            // ANY sum: the payload slots of the live variant, and only the ones
            // that own something, a box the emitter made included. The tag is
            // the variant's position, exactly as `match` reads it. The mirror
            // of `rel_body`'s own arm, asking its question of each payload: a
            // copy that skipped a boxed `Handle` shared the box, and both
            // copies released it.
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
            Type::Lazy(_) => Ok(()),
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
        args: &mut Parts,
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
        for (i, t) in payload.iter().enumerate() {
            let at = self.cx.payload_slot(payload, i);
            if self.word2(t)? == Word::Inline2 {
                // Two words already side by side: one copy, no encoding.
                dest.addr(b, l.fields[at]);
                self.part(m, b, args, i, t, line)?;
                b.ins(&Instruction::I32Const(16));
                b.ins(&Instruction::MemoryCopy {
                    src_mem: 0,
                    dst_mem: 0,
                });
            } else {
                dest.addr(b, l.fields[at]);
                match args.built(i) {
                    Some(boxed) => {
                        boxed.addr(b, 0);
                        b.ins(&Instruction::I64ExtendI32U);
                    }
                    None => {
                        self.part(m, b, args, i, t, line)?;
                        self.encode_word2(b, t, line)?;
                    }
                }
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

    /// `Age?(n)` — a validated construction whose refinement answers with a tag
    /// instead of a trap, yielding `Option<Age>` (RFC-0003). Both walks call it:
    /// `operand` pushes the value at the base type it is handed, and `dest`
    /// names the storage the `Option` is written into, whose address is left
    /// on the stack.
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
        line: usize,
        operand: impl FnOnce(&mut Self, &mut Module, &mut Frame, &Type) -> Result<(), String>,
        dest: impl FnOnce(&mut Frame, &Layout) -> Dest,
    ) -> Result<Type, String> {
        let decl = self
            .cx
            .types
            .get(name)
            .cloned()
            .ok_or_else(|| gap(&format!("a fallible construction of `{name}`"), line))?;
        let ty = Type::option(Type::Named(name.to_string()));
        let Repr::Agg(l) = self.cx.repr(&ty, line)? else {
            return unsupported("a fallible construction of a non-aggregate Option", line);
        };
        let base = decl.base.clone();
        operand(self, m, b, &base)?;
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
        let at = dest(b, &l);
        at.addr(b, l.fields[0]);
        b.ins(&Instruction::LocalGet(tag));
        b.ins(&Instruction::I64ExtendI32U);
        b.ins(&Instruction::I64Store(word8()));
        at.addr(b, l.fields[1]);
        b.ins(&Instruction::LocalGet(held));
        self.encode_word2(b, &base, line)?;
        b.ins(&Instruction::I64Store(word8()));
        for f in &l.fields[2..] {
            at.addr(b, *f);
            b.ins(&Instruction::I64Const(0));
            b.ins(&Instruction::I64Store(word8()));
        }
        at.addr(b, 0);
        Ok(ty)
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
        Self::tag_is(b, addr, tag_of(sum, pat, line)?.map(|t| t as u64));
        Ok(())
    }

    /// Open the chain a switch is, and say which arm goes first.
    ///
    /// The arms are tried in order inside one `block`, each leaving by a
    /// branch to it; two arms that are one tag and a default collapse to an
    /// `if`/`else`, which joins where it ends and needs neither
    /// ([`crate::two_way`]). `bt` is what the join carries.
    ///
    /// [`Fn_::core_switch`] writes it over the row's arms (RFC-0125 M7).
    fn chain_open(&mut self, b: &mut Frame, tags: &[Option<usize>], bt: BlockType) -> Chain {
        let two = crate::two_way(tags);
        let out = self.depth;
        if two.is_none() {
            b.ins(&Instruction::Block(bt));
            self.depth += 1;
        }
        Chain {
            two,
            out,
            bt,
            els: None,
        }
    }

    /// The arms in the order they are emitted. The tagged arm goes first in a
    /// two-way branch, whichever side the source wrote it on: the `if` tests a
    /// tag and the `else` is what is left.
    fn chain_order(c: &Chain, arms: usize) -> Vec<usize> {
        match c.two {
            Some((at, _)) => vec![at, 1 - at],
            None => (0..arms).collect(),
        }
    }

    /// Enter arm `slot` of the chain: its test, and the block it writes into.
    /// `probe` pushes the test's `i32`, which a two-way branch's second side
    /// does not ask.
    fn chain_enter(
        &mut self,
        b: &mut Frame,
        c: &mut Chain,
        slot: usize,
        probe: impl FnOnce(&mut Frame),
    ) {
        if c.two.is_some() && slot == 1 {
            c.els = Some(b.here());
            b.ins(&Instruction::Else);
            return;
        }
        probe(b);
        // A chain arm carries its value out on the branch, so only the
        // two-way branch's own `if` is the join.
        let bt = if c.two.is_some() {
            c.bt
        } else {
            BlockType::Empty
        };
        b.ins(&Instruction::If(bt));
        self.depth += 1;
    }

    /// Leave arm `slot`: the branch out of the chain, or the end of the
    /// two-way branch once its second side has been written.
    fn chain_leave(&mut self, b: &mut Frame, c: &Chain, slot: usize) {
        match c.two {
            Some(_) if slot == 0 => return,
            Some(_) => {
                // An `else` that wrote nothing is no `else` at all. A branch
                // that carries a value always writes one, so only an
                // empty-result `if` can lose it.
                if let Some(at) = c
                    .els
                    .filter(|at| b.here() == at + 1 && matches!(c.bt, BlockType::Empty))
                {
                    b.rewind(at);
                }
            }
            None => {
                let d = self.br_to(c.out);
                b.ins(&Instruction::Br(d));
            }
        }
        self.depth -= 1;
        b.ins(&Instruction::End);
    }

    /// Close the chain. The checker proves the arms exhaustive; the validator
    /// cannot see the proof, so it is told instead.
    fn chain_close(&mut self, b: &mut Frame, c: &Chain) {
        if c.two.is_some() {
            return;
        }
        b.ins(&Instruction::Unreachable);
        self.depth -= 1;
        b.ins(&Instruction::End);
    }

    /// The probe itself: whether the sum at `addr` carries `tag`, and constant
    /// truth for an arm no tag chooses.
    ///
    /// The arm above asks it of a PATTERN and the core walk asks it of the row
    /// ([`vyrn_lower::core::Test`], RFC-0125 M7), so the two spellings of
    /// "a tag is read and an arm is chosen" are one.
    fn tag_is(b: &mut Frame, addr: u32, tag: Option<u64>) {
        b.ins(&Instruction::LocalGet(addr));
        // The refutable-`let` desugar's default arm (RFC-0121): the probe is
        // constant truth — the address read above is discarded, and the one
        // `i32` every caller expects is pushed in its place.
        let Some(tag) = tag else {
            b.ins(&Instruction::Drop);
            b.ins(&Instruction::I32Const(1));
            return;
        };
        // One tag read for every sum since RFC-0126 §8.11's M4b, where the two
        // built-in ones stopped having a variant list of their own. A second
        // spelling of this probe would be a second chance to read the tag at the
        // wrong width, which is silent rather than loud.
        b.ins(&Instruction::I64Load(word8()));
        b.ins(&Instruction::I64Const(tag as i64));
        b.ins(&Instruction::I64Eq);
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
                payload_at(b, addr, off, true);
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
                payload_at(b, addr, off, false);
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

    /// Whether a placed row releases the value at `key` WHOLE (RFC-0125 §3 M3,
    /// the walk's deletion).
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
    /// A map literal of type `mty` built at `dest`: the header zeroed, then
    /// each key and value of `parts`, in pairs, inserted in written order, so
    /// a duplicate key updates in place and keeps its slot —
    /// `["usd": 1, "eur": 2, "usd": 3]` is length 2 with `usd` first. The
    /// value a repeated key shadows has no owner left, so the insert releases
    /// it; inside a `region` the arena owns it. The AST arm and
    /// [`Fn_::core_make`] both call this.
    fn map_into(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        mty: &Type,
        parts: &mut Parts,
        line: usize,
    ) -> Result<(), String> {
        let Type::Map(key_t, val) = self.cx.resolve(mty) else {
            return unsupported(&format!("a map literal of `{mty}`"), line);
        };
        let l = self.layout_of(mty, line)?;
        dest.addr(b, 0);
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32Const(l.size as i32));
        b.ins(&Instruction::MemoryFill(0));
        let hdr = b.local(ValType::I32);
        dest.addr(b, 0);
        b.ins(&Instruction::LocalSet(hdr));
        for i in (0..parts.len()).step_by(2) {
            self.map_set(m, b, hdr, &l, parts, i, &key_t, &val, true, line)?;
        }
        Ok(())
    }

    /// `m.tallyBytes(w, n)` (RFC-0116): the byte-keyed probe, on this backend
    /// too. `mapFind`'s kind 3 compares the window where it lies, so a hit — the
    /// hot path in a counting loop — builds no String, validates nothing, and
    /// allocates nothing. Only a miss goes through `str_from_bytes` (whose Err
    /// is the trap) and the insert path, where the fresh key is stored, not
    /// copied.
    ///
    /// The map's header address is in `hdr`. `operand` pushes operand 0, the
    /// bytes, or operand 1, `n`, at the type it is handed, as for
    /// [`Fn_::map_tally`].
    fn map_tally_bytes(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        mty: &Type,
        operand: &mut dyn FnMut(
            &mut Self,
            &mut Module,
            &mut Frame,
            usize,
            &Type,
        ) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
        let Type::Map(..) = self.cx.resolve(mty) else {
            return unsupported(&format!("`tallyBytes` on `{mty}`"), line);
        };
        let l = self.layout_of(mty, line)?;
        let bytes = Type::Array(Box::new(Type::IntN {
            bits: 8,
            signed: false,
        }));
        let wsrc = b.local(ValType::I32);
        operand(self, m, b, 0, &bytes)?;
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
        operand(self, m, b, 1, &Type::Int)?;
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
        Ok(mty.clone())
    }

    /// `m.tally(k, n)` (RFC-0116): insert-or-add, ONE probe. The callee never
    /// takes the key — a hit adds in place and touches nothing else, a miss
    /// stores a COPY — so the caller's ownership is the same on both paths.
    ///
    /// The map's header address is in `hdr`. `operand` pushes operand 0, the
    /// key, or operand 1, `n`, at the type it is handed: the arm over the
    /// source evaluates expressions and the core's walk reads names off the
    /// row (RFC-0125 M7).
    fn map_tally(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        mty: &Type,
        operand: &mut dyn FnMut(
            &mut Self,
            &mut Module,
            &mut Frame,
            usize,
            &Type,
        ) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
        if !matches!(self.cx.resolve(mty), Type::Map(..)) {
            return unsupported(&format!("`tally` on `{mty}`"), line);
        }
        let mut key =
            |s: &mut Self, m: &mut Module, b: &mut Frame, t: &Type| operand(s, m, b, 0, t);
        let (k, l, mk) = self.map_key_local(m, b, mty, &mut key, line)?;
        let n = b.local(ValType::I64);
        operand(self, m, b, 1, &Type::Int)?;
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
        Ok(mty.clone())
    }

    /// `m[k] = v` — update in place on a hit, append on a miss.
    ///
    /// `hdr` is a local holding the header's address. `drop_old` is rule 4's own
    /// question — may this store release what the slot holds now — answered by
    /// the caller, because a new value that names the map could name the very
    /// bytes this frees. The map takes the value as well as the key, so a hit
    /// that only stored over the old value leaked it.
    fn map_set(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        l: &Layout,
        parts: &mut Parts,
        key: usize,
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
                self.part(m, b, parts, key, &Type::Int, line)?;
                b.ins(&Instruction::LocalSet(k));
                k
            }
            MapKey::Pack(_) => {
                let raw = b.local(ValType::I32);
                self.part(m, b, parts, key, key_t, line)?;
                b.ins(&Instruction::LocalSet(raw));
                self.pack_key(b, raw, key_t, line)?
            }
            MapKey::Str => {
                let k = b.local(ValType::I32);
                self.part(m, b, parts, key, &Type::Str, line)?;
                b.ins(&Instruction::LocalSet(k));
                k
            }
        };
        let v = b.local(match &r {
            Repr::Scalar(t) => *t,
            Repr::Agg(_) => ValType::I32,
            Repr::Unit => return unsupported("a Map of Unit", line),
        });
        self.part(m, b, parts, key + 1, val, line)?;
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

    /// The entry of the key `key` pushes in the map whose header address is in
    /// `hdr`: its index, negative when the map has none, with the map's layout
    /// and key family. `key` pushes the key at the type it is handed.
    fn map_find(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        mty: &Type,
        key: &mut dyn FnMut(&mut Self, &mut Module, &mut Frame, &Type) -> Result<(), String>,
        line: usize,
    ) -> Result<(u32, Layout, MapKey), String> {
        let (k, l, mk) = self.map_key_local(m, b, mty, key, line)?;
        let idx = b.local(ValType::I32);
        self.map_scan(b, hdr, &l, k, idx, mk);
        Ok((idx, l, mk))
    }

    /// The key `key` pushes, in a local at the form [`Fn_::map_scan`] probes
    /// with, with the map's layout and key family.
    fn map_key_local(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        mty: &Type,
        key: &mut dyn FnMut(&mut Self, &mut Module, &mut Frame, &Type) -> Result<(), String>,
        line: usize,
    ) -> Result<(u32, Layout, MapKey), String> {
        let key_t = match self.cx.resolve(mty) {
            Type::Map(k, _) => *k,
            _ => Type::Str,
        };
        let mk = self.map_key(&key_t, line)?;
        let l = self.layout_of(mty, line)?;
        let k = match mk {
            MapKey::I64 => {
                let k = b.local(ValType::I64);
                key(self, m, b, &Type::Int)?;
                b.ins(&Instruction::LocalSet(k));
                k
            }
            MapKey::Pack(_) => {
                let raw = b.local(ValType::I32);
                key(self, m, b, &key_t)?;
                b.ins(&Instruction::LocalSet(raw));
                self.pack_key(b, raw, &key_t, line)?
            }
            MapKey::Str => {
                let k = b.local(ValType::I32);
                key(self, m, b, &Type::Str)?;
                b.ins(&Instruction::LocalSet(k));
                k
            }
        };
        Ok((k, l, mk))
    }

    /// `m[k]` — an honest `Option<V>`, never a trap.
    ///
    /// The map's address is already on the stack. `key` pushes the key at the
    /// type it is handed: the arm over the source evaluates an expression and
    /// the core's walk reads a name off the row (RFC-0125 M7), and the lookup
    /// between them is this one sequence.
    fn map_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        mty: &Type,
        val: &Type,
        key: &mut dyn FnMut(&mut Self, &mut Module, &mut Frame, &Type) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
        let esz = self.stride(val, line)? as i32;
        let hdr = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(hdr));
        let (idx, l, _) = self.map_find(m, b, hdr, mty, key, line)?;

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

    /// `assert(c)` and `assertEq(a, b)`, the builtins [`Spec::Asserts`] names.
    /// `operand` writes argument `i` at the type asked for, or at its own
    /// where none is, and answers the type it wrote.
    fn asserts(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        operand: &mut dyn FnMut(
            &mut Self,
            &mut Module,
            &mut Frame,
            usize,
            Option<&Type>,
        ) -> Result<Type, String>,
        line: usize,
    ) -> Result<Type, String> {
        match name {
            // `assert(c)` (RFC-0015): the interpreter's trap, in its words. Lowered
            // here rather than rewritten into `panic` by the CLI before the compile
            // (RFC-0125 §3 M5), so the rule is stated once.
            "assert" => {
                operand(self, m, b, 0, Some(&Type::Bool))?;
                let msg = self.cx.rt.intern(
                    m,
                    &vyrn_frontend::trap::line(&format!("assertion failed at line {line}")),
                );
                b.ins(&Instruction::I32Eqz)
                    .ins(&Instruction::If(BlockType::Empty))
                    .ins(&Instruction::I32Const(msg as i32))
                    .ins(&Instruction::Call(self.cx.rt.trap))
                    .ins(&Instruction::End);
            }
            // `assertEq(a, b)` (RFC-0015): each operand evaluated once into a
            // local, the two compared by their type — the checker allows one
            // equatable scalar type for both — and on a mismatch rendered the way
            // `toString` renders them, around ` != `, after the interpreter's
            // `scalar_to_string`. The line is written in pieces the way `panic`
            // writes its message, and `trap` writes the last piece and exits. An
            // operand that allocated is released by [`Fn_::call`] after this
            // returns, as every call argument is (`rfcs/census-call-arguments.md`).
            _ => {
                let t = operand(self, m, b, 0, None)?;
                let t = self.cx.resolve(&t);
                let Some(vt) = self.cx.repr(&t, line)?.val() else {
                    return unsupported("`assertEq` on a non-scalar", line);
                };
                let la = b.local(vt);
                b.ins(&Instruction::LocalSet(la));
                operand(self, m, b, 1, Some(&t))?;
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
            }
        }
        Ok(Type::Unit)
    }

    /// `writeStdout(bytes)`, `close(s)` and `boxStream(s)`, the builtins
    /// [`Spec::Effect`] names. `operand` writes the argument at the type asked
    /// for, or at its own where none is, and answers the type it wrote.
    fn effect(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        operand: &mut dyn FnMut(
            &mut Self,
            &mut Module,
            &mut Frame,
            Option<&Type>,
        ) -> Result<Type, String>,
        line: usize,
    ) -> Result<Type, String> {
        match name {
            // RFC-0111: `print` for bytes. `write_all` is already the gathered
            // stdout writer every printed line goes through, so this is that call
            // with the caller's buffer — same buffering, same ordering against
            // `print` and against standard error. Its status is dropped, for the
            // reason `print` drops it.
            "writeStdout" => {
                let bytes = Type::Array(Box::new(Type::IntN {
                    bits: 8,
                    signed: false,
                }));
                operand(self, m, b, Some(&bytes))?;
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
            }
            "boxStream" => return self.stream_box(m, b, operand, line),
            // `close` reclaims what this backend CAN reclaim. Its `malloc` is a
            // bump pointer that never frees, so a buffer stream's teardown is
            // still nothing — but a stepped one owns a cell, and cells come from
            // a fixed slab of 65536 that a leak would exhaust. Which of the two
            // it is, is the tag.
            _ => {
                let got = operand(self, m, b, None)?;
                let elem = match self.cx.resolve(&got) {
                    Type::Stream(i) => *i,
                    other => return unsupported(&format!("`{name}` of `{other}`"), line),
                };
                let s = b.local(ValType::I32);
                b.ins(&Instruction::LocalSet(s));
                self.stream_release(m, b, Place::Local(s), &elem, line)?;
            }
        }
        Ok(Type::Unit)
    }

    /// The builtins [`Spec::Logs`] names. `operand` writes the `i`th
    /// argument at the type asked for.
    ///
    /// `logger(name)` is the identity on a `ptr`: a `Logger` is its name
    /// string and has no other content. A level evaluates both operands
    /// whatever the threshold says, because the interpreter evaluates them
    /// before it checks (RFC-0008 Q4, pinned), and emits the write only if
    /// the level clears it. That test is the whole feature: with `logging {
    /// level: warn }` a `.debug(..)` call emits no `write_all` at all, so a
    /// disabled log site costs nothing on any engine. A runtime comparison
    /// would turn a deleted call into a branch, the mistake RFC-0078's census
    /// names.
    fn logs(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        operand: &mut dyn FnMut(
            &mut Self,
            &mut Module,
            &mut Frame,
            usize,
            &Type,
        ) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
        if name == "logger" {
            operand(self, m, b, 0, &Type::Str)?;
            return Ok(Type::Logger);
        }
        operand(self, m, b, 0, &Type::Logger)?;
        operand(self, m, b, 1, &Type::Str)?;
        if log_internal(name).unwrap_or(0) < self.cx.log_level {
            // Below the threshold: the two values are the only thing this
            // site leaves behind, and `Unit` means nobody consumes them.
            b.ins(&Instruction::Drop);
            b.ins(&Instruction::Drop);
        } else {
            self.log_write(m, b, name, line)?;
        }
        Ok(Type::Unit)
    }

    /// A SIMD builtin (RFC-0083): a lane constructor, a lane read or write at
    /// a constant index, a mask reduction, or a load or store of consecutive
    /// array elements. The vector operand's own type chooses the opcode.
    ///
    /// `operand` writes argument `i`, at the type asked for or else at its own,
    /// and answers the type it wrote. `lane_at` answers argument `i` as a lane
    /// index below the count given, or `None` where it is no such constant.
    /// The arm over the source and [`Fn_::core_call`] over the rows both call
    /// this.
    fn lanes(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        argc: usize,
        operand: &mut dyn FnMut(
            &mut Self,
            &mut Module,
            &mut Frame,
            usize,
            Option<&Type>,
        ) -> Result<Type, String>,
        lane_at: &dyn Fn(usize, i64) -> Option<u8>,
        line: usize,
    ) -> Result<Type, String> {
        match name {
            // RFC-0083 M1. Construction starts from `v128.const 0` and replaces
            // each lane in written order; a splat is the one opcode. No lane is
            // read back before it is written, so the zero start costs nothing that
            // an undefined one would have saved.
            //
            // M3's integer width is the same two shapes with the lane-typed
            // opcodes swapped, which is what M1 meant by "one internal name per
            // width": nothing here decodes a receiver.
            "F32x4" | "I32x4" | "F64x2" if argc > 0 => {
                let wide = name == "F64x2";
                let int = name == "I32x4";
                let (vec, lane) = if int {
                    (Type::I32x4, INT32)
                } else if wide {
                    (Type::F64x2, Type::Float)
                } else {
                    (Type::F32x4, Type::Float32)
                };
                b.ins(&Instruction::V128Const(0));
                for i in 0..argc {
                    operand(self, m, b, i, Some(&lane))?;
                    b.ins(&if int {
                        Instruction::I32x4ReplaceLane(i as u8)
                    } else if wide {
                        Instruction::F64x2ReplaceLane(i as u8)
                    } else {
                        Instruction::F32x4ReplaceLane(i as u8)
                    });
                }
                return Ok(vec);
            }
            // The lane index was proven constant and in range by the checker, so
            // this is a plain immediate and there is no bounds check to emit.
            "@lane" if argc == 2 => {
                let vt = operand(self, m, b, 0, None)?;
                let vt = self.cx.resolve(&vt);
                let lanes = if matches!(vt, Type::F64x2 | Type::Mask64x2) {
                    2
                } else {
                    4
                };
                let Some(k) = lane_at(1, lanes) else {
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
            "@replaceLane" if argc == 3 => {
                let vt = operand(self, m, b, 0, None)?;
                let vt = self.cx.resolve(&vt);
                let int = vt == Type::I32x4;
                let wide = vt == Type::F64x2;
                let Some(k) = lane_at(1, if wide { 2 } else { 4 }) else {
                    return unsupported("a lane index that is not a constant", line);
                };
                let lane = if int {
                    &INT32
                } else if wide {
                    &Type::Float
                } else {
                    &Type::Float32
                };
                operand(self, m, b, 2, Some(lane))?;
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
            "@anyTrue" | "@allTrue" if argc == 1 => {
                let mt = operand(self, m, b, 0, None)?;
                let mt = self.cx.resolve(&mt);
                let wide = mt == Type::Mask64x2;
                b.ins(&if name == "@anyTrue" {
                    Instruction::V128AnyTrue
                } else if wide {
                    Instruction::I64x2AllTrue
                } else {
                    Instruction::I32x4AllTrue
                });
                return Ok(Type::Bool);
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
            | "@f64x2Store"
                if argc == 2 + usize::from(name.ends_with("Store")) =>
            {
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
                let aty = operand(self, m, b, 0, None)?;
                let w = self.walk(b, &aty, line)?;
                operand(self, m, b, 1, Some(&Type::Int))?;
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
                operand(self, m, b, 2, Some(&vec))?;
                b.ins(&Instruction::V128Store(MemArg {
                    offset: 0,
                    align: 0,
                    memory_index: 0,
                }));
                return Ok(Type::Unit);
            }
            _ => unsupported(&format!("`{name}` at this arity"), line),
        }
    }

    /// `m.keys()` with the map's address in the local `hdr`: a snapshot
    /// `Array<K>`, the keys copied into a buffer of their own, so the map may
    /// be mutated afterwards without disturbing it. String keys are then dup'd
    /// per element (RFC-0092 M2 — an array owns its elements, so a snapshot of
    /// the map's own pointers would be freed twice); Int64 keys copy with the
    /// buffer (RFC-0117). The arm over the source and [`Fn_::core_call`] over
    /// the rows both call this.
    fn map_keys(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        mty: &Type,
        line: usize,
    ) -> Result<Type, String> {
        let Type::Map(key_t, _) = self.cx.resolve(mty) else {
            return unsupported(&format!("`keys` on `{mty}`"), line);
        };
        let mk = self.map_key(&key_t, line)?;
        let l = self.layout_of(mty, line)?;
        let aty = Type::Array(key_t);
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
            self.each(m, b, false, buf, len, 4, &Type::Str, line)?;
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
        Ok(aty)
    }

    /// `m.remove(k)` on the map whose header address is in `hdr`: the entry
    /// of the key `key` pushes released and dropped, and whether there was
    /// one left on the stack. The arm over the source and the core's walk
    /// (RFC-0125 M7) differ only in how they push the key.
    fn map_remove(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        mty: &Type,
        key: &mut dyn FnMut(&mut Self, &mut Module, &mut Frame, &Type) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
        let Type::Map(_, val) = self.cx.resolve(mty) else {
            return unsupported(&format!("`remove` on `{mty}`"), line);
        };
        let (idx, l, mk) = self.map_find(m, b, hdr, mty, key, line)?;
        let found = b.local(ValType::I32);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32GeS);
        b.ins(&Instruction::LocalSet(found));
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
        // An Int64 key owns nothing to hand back (RFC-0117). An entry a
        // `remove` drops is unreachable afterwards whoever owns the map, and
        // nothing aliases it (RFC-0092 M2 made `keys()` copy). A `region` is
        // not asked here either: a `String` key routed into the arena comes
        // back refused, and a `Map<String, Array<Int>>` built inside one
        // holds buffers the arena never had.
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
        b.ins(&Instruction::LocalGet(found));
        Ok(Type::Bool)
    }
}

// ---------------------------------------------------------------------------
// `SmallArray<T, N>` (RFC-0056, RFC-0077 M2l)
// ---------------------------------------------------------------------------

/// Write the inline state of a `SmallArray<T, N>` header at `dest`: `len`,
/// `cap == n` and a null `data` (RFC-0056). Both walks build one through it.
fn sa_head(b: &mut Frame, dest: Dest, l: &Layout, len: usize, n: usize) {
    dest.addr(b, l.fields[0]);
    b.ins(&Instruction::I64Const(len as i64));
    b.ins(&Instruction::I64Store(word8()));
    dest.addr(b, l.fields[1]);
    b.ins(&Instruction::I64Const(n as i64));
    b.ins(&Instruction::I64Store(word8()));
    dest.addr(b, l.fields[2]);
    b.ins(&Instruction::I32Const(0));
    b.ins(&Instruction::I32Store(word()));
}

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
        sa_head(b, Dest::Slot(off), &l, len, n);
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

    /// A `SmallArray<T, N>` literal the core's row makes (RFC-0125 M7): the
    /// header of [`sa_head`] at `dest`, and the parts written straight into
    /// the inline buffer. The checker proved `parts.len() <= N`.
    #[allow(clippy::too_many_arguments)]
    fn sa_into(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        ty: &Type,
        inner: &Type,
        n: usize,
        parts: &mut Parts,
        line: usize,
    ) -> Result<(), String> {
        let l = self.layout_of(ty, line)?;
        sa_head(b, dest, &l, parts.len(), n);
        self.fixed_elems(m, b, dest.at(l.fields[3]), inner, parts, line)
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
        operand: &mut dyn FnMut(&mut Self, &mut Module, &mut Frame, &Type) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
        let l = self.layout_of(aty, line)?;
        let stride = self.stride(inner, line)? as i32;
        // The receiver's address is the destination, the rule RFC-0125's push
        // route states for `Array`: this emitter reads the whole header before
        // it stores, so a temp to write through buys nothing.
        let hdr = b.local(ValType::I32);
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
        operand(self, m, b, inner)?;
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
        b.ins(&Instruction::LocalGet(hdr));
        Ok(aty.clone())
    }

    /// The layout of the `SmallArray` whose header address is in `hdr`, and
    /// the walk over its live slots.
    fn sa_open(
        &mut self,
        b: &mut Frame,
        hdr: u32,
        aty: &Type,
        line: usize,
    ) -> Result<(Layout, Walk), String> {
        let Type::SmallArray(inner, n) = self.cx.resolve(aty) else {
            return unsupported(&format!("a SmallArray operation on `{aty}`"), line);
        };
        let l = self.layout_of(aty, line)?;
        let stride = self.stride(&inner, line)?;
        let (len, _cap, base) = self.sa_parts(b, hdr, &l, n);
        let w = Walk {
            data: base,
            len,
            stride,
            elem: *inner,
            byte: false,
        };
        Ok((l, w))
    }

    /// `xs.toArray()` on the `SmallArray` whose header address is in `hdr`.
    ///
    /// The result is a fresh `Array<T>` holding a copy of the live elements,
    /// the one explicit conversion RFC-0056 has; the interpreter's is the
    /// identity because both are `Val::Array`. An array owns its elements
    /// (RFC-0092 M2), so the words it copies are given their own heap.
    fn sa_to_array(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        aty: &Type,
        line: usize,
    ) -> Result<Type, String> {
        let (_, w) = self.sa_open(b, hdr, aty, line)?;
        let (len, base, stride, inner) = (w.len, w.data, w.stride, &w.elem);
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
        self.each(m, b, false, buf, count, stride, inner, line)?;
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

    /// `xs.pop()` on the `SmallArray` whose header address is in `hdr`: an
    /// `Option<T>`, `None` on empty, else the last element with the header
    /// shrunk. It never un-spills, like the LLVM path.
    fn sa_pop(&mut self, b: &mut Frame, hdr: u32, aty: &Type, line: usize) -> Result<Type, String> {
        let (l, w) = self.sa_open(b, hdr, aty, line)?;
        let (len, inner) = (w.len, &w.elem);
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

    /// `xs.swapRemove(i)` on the `SmallArray` whose header address is in
    /// `hdr`, with `index` writing the index: the removed element, with the
    /// last one moved into its place. For `i == len - 1` those are the same
    /// address, and the copy is a no-op.
    fn sa_swap_remove(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        aty: &Type,
        index: &mut dyn FnMut(&mut Self, &mut Module, &mut Frame) -> Result<(), String>,
        line: usize,
    ) -> Result<Type, String> {
        let (l, w) = self.sa_open(b, hdr, aty, line)?;
        let (len, stride, inner) = (w.len, w.stride, &w.elem);
        index(self, m, b)?;
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

/// An 8-byte access at a static offset.
fn word_at8(off: u32) -> MemArg {
    MemArg {
        offset: off as u64,
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

fn each_block(blk: &Block, fe: &mut dyn FnMut(&Expr), fs: &mut dyn FnMut(&Stmt)) {
    hoist_block(
        blk,
        &mut std::collections::HashSet::new(),
        &mut Hoist { fe, fs },
    );
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

/// A scalar spilled for a `modify` call: its slot, its local, its LLVM type
/// and whether it loads signed ([`Fn_::spill`]).
type Spill = (u32, u32, String, bool);

/// Write each spilled scalar back into its local after the call.
fn reload(b: &mut Frame, spilled: &[Spill]) {
    for (off, l, ll, signed) in spilled {
        b.slot(*off);
        b.ins(&load_of(ll, 0, *signed));
        b.ins(&Instruction::LocalSet(*l));
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
    /// Of the eight functions `std/runtime` supplies (PLAN-0125-runtime §6 step
    /// 1), the five this emitter calls: `strlen`, `strcmp`, `int_str`,
    /// `regex_run`, `parse_i64`. `utf8Valid`, `starts` and
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
    /// `parse`, which RFC-0078 refused to route into Vyrn: it wraps on overflow
    /// where `std/num`'s `parseInt64` declines, so the two are not one function
    /// and folding them would be a language change (RFC-0078 M4a).
    parse_i64: u32,
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

/// The receivers that have a `.length` or a `.byteLength`, in ONE list.
///
/// Neither name is a field, so both paths that meet one have to know the list:
/// [`Fn_::length_of`] emits the load, and [`Fn_::peek`] answers what the load
/// will produce. Each held its own copy, and the copies drifted — `peek`'s
/// omitted a `Map` and a `SmallArray`, so `match o { Some(m) => m.length, .. }`
/// read as "a field of the non-record type `Map<String, Int64>`" while the
/// same read outside a branch compiled. `base` is already resolved.
///
/// This is `slot_ty`'s rule on a second table: one spelling, two readers.
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

/// The result type of a builtin [`Fn_::slot_call`] writes, at `argc` operands,
/// or `None` where the name is not one or the arity is not its own.
///
/// The type is the core's `Spec::Builds` row, so the slot the call sizes and
/// the value the row lands are one spelling.
fn slot_ty(name: &str, argc: usize) -> Option<Type> {
    match vyrn_lower::core::builtin_row(name) {
        Some(Spec::Builds(t)) if slot_arity(name) == Some(argc) => Some(t.clone()),
        _ => None,
    }
}

/// How many operands a builtin [`Fn_::slot_call`] writes takes, or `None`
/// where the name is not one.
fn slot_arity(name: &str) -> Option<usize> {
    match name {
        "args" | "readLine" => Some(0),
        "readFile" | "readFileBytes" | "fsyncFile" | "parse" | "listDir" | "listDirKinds" => {
            Some(1)
        }
        "writeFile" | "renameFile" | "writeFileBytes" => Some(2),
        _ => None,
    }
}

/// What a builtin the core's row specifies emits: its operand types, the one
/// instruction it is, and its result type — RFC-0125 M7, the builtin family.
///
/// `vyrn_lower::core::builtin_row` states the types, because a row is what
/// makes such a call a `call` with a specification and not a gap; this states
/// the instruction, because an instruction is the emitter's. `None` where the
/// name has no row, or where the site's arity is not the row's.
/// [`Fn_::core_call`] asks it over the rows. `builtin_rows_all_emit` refuses a
/// row with no instruction.
fn builtin_spec(
    name: &str,
    argc: usize,
) -> Option<(&'static [Type], Instruction<'static>, &'static Type)> {
    let Some(Spec::Typed(params, ret)) = vyrn_lower::core::builtin_row(name) else {
        return None;
    };
    if params.len() != argc {
        return None;
    }
    let ins = match name {
        // `f64` and `i64` are the same 64 bits on this backend's value stack,
        // so a reinterpretation is free where a conversion rounds.
        "floatBits" => Instruction::I64ReinterpretF64,
        "floatFromBits" => Instruction::F64ReinterpretI64,
        "@f32x4Splat" => Instruction::F32x4Splat,
        "@i32x4Splat" => Instruction::I32x4Splat,
        "@f64x2Splat" => Instruction::F64x2Splat,
        // RFC-0083 M2 and M4: wasm's own `min` and `max`, which propagate a
        // NaN in either operand and order `-0.0` below `+0.0`, and
        // `f32x4.nearest`, which is roundTiesToEven. The other two engines are
        // pointed at these rules rather than at their own defaults.
        "@f32x4Min" => Instruction::F32x4Min,
        "@f32x4Max" => Instruction::F32x4Max,
        "@f32x4Sqrt" => Instruction::F32x4Sqrt,
        "@f32x4Ceil" => Instruction::F32x4Ceil,
        "@f32x4Floor" => Instruction::F32x4Floor,
        "@f32x4Trunc" => Instruction::F32x4Trunc,
        "@f32x4Nearest" => Instruction::F32x4Nearest,
        "@f64x2Min" => Instruction::F64x2Min,
        "@f64x2Max" => Instruction::F64x2Max,
        "@f64x2Sqrt" => Instruction::F64x2Sqrt,
        _ => return None,
    };
    Some((params.as_slice(), ins, ret))
}

/// Argument `i` of a call row as a lane index below `lanes`: the literal the
/// row carries, which the checker proved constant and in range.
/// The arguments, where every one is a value.
fn arg_vals(
    args: &[(Arg, vyrn_frontend::ast::Capability)],
) -> Option<Vec<(Val, vyrn_frontend::ast::Capability)>> {
    args.iter()
        .map(|(a, c)| Some((a.val()?.clone(), *c)))
        .collect()
}

fn core_lane(args: &[(Val, vyrn_frontend::ast::Capability)], i: usize, lanes: i64) -> Option<u8> {
    match args.get(i) {
        Some((Val::Lit(Lit::Int(k)), _)) if (0..lanes).contains(k) => Some(*k as u8),
        _ => None,
    }
}

/// The specification row of the builtin a CALL ROW names, or `None` where the
/// row names a function this program declares or a callee with no row.
/// The module-state binding a receiver reads, where `n` is a read of one
/// that a `@strAppend` row grows ([`vyrn_lower::core::NameInfo::grows`]).
fn core_global(body: &vyrn_lower::core::Body, n: vyrn_lower::core::Name) -> Option<&str> {
    if !body.names[n as usize].grows {
        return None;
    }
    let mut lets = Vec::new();
    for s in &body.stmts {
        core_lets(s, &mut lets);
    }
    lets.iter().find_map(|(m, rhs)| match rhs {
        Rhs::Read(vyrn_lower::core::Place::Global(g)) if *m == n => Some(g.as_str()),
        _ => None,
    })
}

/// Clears an accumulator's ownership word at `own`: the place holds a
/// pointer a store put there, which this path did not allocate, so the next
/// append copies rather than grows ([`Fn_::append_in_place`]).
fn disown(b: &mut Frame, own: Place) {
    own.addr(b, 0);
    b.ins(&Instruction::I32Const(0))
        .ins(&Instruction::I32Store(word()));
}

/// How many operands `assert` (one) and `assertEq` (two) take.
fn assert_arity(name: &str) -> usize {
    if name == "assert" {
        1
    } else {
        2
    }
}

/// How many operands a [`Spec::Logs`] builtin takes: `logger` its name, a
/// level the logger and the message.
fn logs_arity(name: &str) -> usize {
    1 + usize::from(name != "logger")
}

fn core_builtin(callee: &str, kind: Callee) -> Option<&'static Spec> {
    matches!(kind, Callee::Builtin | Callee::Reserved)
        .then(|| vyrn_lower::core::builtin_row(callee))
        .flatten()
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
    /// [`vyrn_lower::core::Body::occurrences`], for the slot's extent.
    occurs: Vec<u32>,
    /// The frame slot each name took, as the mark before it and its end,
    /// until [`Fn_::core_give_back`] hands it back.
    slot: Vec<Option<(u32, u32)>>,
    /// The name the operand stack is holding, if any.
    held: Option<vyrn_lower::core::Name>,
    /// The temporary built in the caller's storage, which the `return` after
    /// it hands back without a copy ([`Fn_::core_lands`]).
    landed: Option<vyrn_lower::core::Name>,
    /// The header of each borrow a loop walks, taken apart where the borrow
    /// is bound ([`NameInfo::walked`](vyrn_lower::core::NameInfo::walked)).
    walks: Vec<Option<Walk>>,
    /// The storage a name's value is built in when it was taken before the
    /// name's own row: a parent's, at the first of its parts whose own row
    /// writes it, and a part's, at its offset in the parent's
    /// ([`Fn_::core_part_at`]).
    built: Vec<Option<Dest>>,
    /// A heap array's element buffer, taken at the first of its parts whose
    /// own row writes it.
    bufs: Vec<Option<u32>>,
    over: Vec<(vyrn_lower::core::Name, vyrn_lower::core::Name)>,
    /// The place a stream's pull wrote its element to, by the pull's name,
    /// until the read at that name binds it ([`Spec::Pulls`]).
    pulled: Vec<Option<(Place, Type)>>,
}

impl<'p> Fn_<'_, 'p> {
    /// Emit the rows held back for the read an exit hands back, in the order
    /// the core stated them.
    fn core_releases(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
    ) -> Result<(), String> {
        for (name, holes, exit) in std::mem::take(&mut self.core_rows) {
            self.core_release(m, b, body, name, &holes, exit)?;
        }
        Ok(())
    }

    /// Emit the one release a core row STATES, where the row stands —
    /// RFC-0125 M7.
    ///
    /// The row is the whole of the answer: the name, whose binding is the node
    /// the plan keys the slot by, and the holes the walk goes around.
    /// [`Fn_::emit_releases`] asks `own::placed` the same question by exit and
    /// node, and its readers are the AST arms alone.
    ///
    /// A name with no slot releases nothing: the walk registers one for every
    /// layout it makes, and a body it takes holds no other value that owns
    /// heap ([`Fn_::core_walkable`]'s scalar clause).
    fn core_release(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        name: vyrn_lower::core::Name,
        holes: &[String],
        exit: ExitKind,
    ) -> Result<(), String> {
        let Some(step) = body.names[name as usize].binding else {
            return unsupported("a release row whose binding the plan does not key", 0);
        };
        let Some(r) = self.rel_slots.get(&step) else {
            return Ok(());
        };
        let (place, rel) = (r.place, around(r.rel.clone(), holes));
        // A FALL-THROUGH exit ends what the row holds on the path that carries
        // on, so the slot is the next statement's — see [`Fn_::rel_pending`].
        if matches!(exit, ExitKind::Block | ExitKind::Scrutinee) {
            self.rel_pending.retain(|(k, _)| *k != step);
        }
        self.emit_rel(m, b, place, &rel, 0)
    }

    /// Emit a release the core STATES as a statement — a `drop` the reader
    /// wrote, a temporary its reading site frees, a payload binder at its
    /// arm's end, an edge's release — at the place this walk gave the name.
    ///
    /// What a release of the type runs is [`Fn_::rel_for`]'s answer, as at the
    /// `Stmt::Drop` arm, walking around the holes the row carries.
    #[allow(clippy::too_many_arguments)]
    fn core_drop(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &Walked,
        n: vyrn_lower::core::Name,
        holes: &Option<Vec<String>>,
        line: usize,
    ) -> Result<(), String> {
        let Some((place, ty)) = self.core_place(w, body, n) else {
            return unsupported("a release of a name with no place", line);
        };
        let info = &body.names[n as usize];
        let rel = match info.binding {
            Some(key) => self.rel_owed(key, &ty, line)?,
            None => self.rel_for(&ty, line)?,
        };
        let Some(rel) = rel else {
            return Ok(());
        };
        let rel = around(rel, body.drop_holes(n, holes));
        if let Some(step) = info.binding {
            self.rel_pending.retain(|(k, _)| *k != step);
        }
        self.emit_rel(m, b, place, &rel, line)
    }

    /// A tag is read and an arm is chosen, off the row — RFC-0125 M7.
    ///
    /// The arms are a chain of `if`s inside one `block`, each leaving by a
    /// branch to it, tested by [`Fn_::tag_is`]. What is not here
    /// is the join, because the core's switch carries no value — every arm
    /// stores its own into the name the reader bound, and a `match`
    /// expression's destination, result type and two-way collapse are all about a value
    /// this row does not have.
    ///
    /// The payload binder is the PLACE the row names (§2.1): the walk binds it
    /// where the arm is entered and the arm's own rows read it there. A layout
    /// the row reads out of the scrutinee is its address inside the
    /// scrutinee's storage; any other layout moves out into a slot, as the arm
    /// moves it.
    #[allow(clippy::too_many_arguments)]
    fn core_switch(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        on: &Val,
        arms: &[vyrn_lower::core::Arm],
        owns: bool,
        line: usize,
    ) -> Result<(), String> {
        let Val::Name(n) = on else {
            return unsupported("a switch on a value the row does not name", line);
        };
        let Some((place, sty)) = self.core_place(w, body, *n) else {
            return unsupported("a switch on a name with no place", line);
        };
        let Some(sum) = self.sum_of(&sty) else {
            return unsupported("a switch on a value that is no sum", line);
        };
        let Repr::Agg(sl) = self.cx.repr(&sty, line)? else {
            return unsupported("a switch on a non-aggregate", line);
        };
        // The scrutinee's address, in the scratch local the arm takes for it.
        if place.addr(b, 0).is_none() {
            let Place::Local(l) = place else {
                unreachable!("a place is a slot, a static or a local")
            };
            b.ins(&Instruction::LocalGet(l));
        }
        let addr = self.scratch(b, ValType::I32, 3);
        b.ins(&Instruction::LocalSet(addr));
        // The boxes the binders come out of, on the clauses the arm asks
        // ([`Fn_::frees_boxes`]): the construct owns the value, and no row
        // releases it whole afterwards. The screen refuses the rest — a
        // declared `release` taking its own receiver apart, and a construct
        // whose own copy the plan releases.
        let free_box = owns;
        // A named `Bool` is the arm's own probe: the local that holds it.
        let mut probes = Vec::new();
        for a in arms {
            probes.push(match a.test {
                vyrn_lower::core::Test::Tag(t) => (Some(t as usize), None),
                vyrn_lower::core::Test::Else => (None, None),
                vyrn_lower::core::Test::Holds(h) => match self.core_place(w, body, h) {
                    Some((Place::Local(l), _)) => (Some(0), Some(l)),
                    _ => return unsupported("a switch on a predicate with no local", line),
                },
            });
        }
        let tags: Vec<Option<usize>> = probes.iter().map(|p| p.0).collect();
        // The switch carries no value: the core stores each arm's own into the
        // name the reader bound, so the join is empty and there is no
        // destination for it to write through.
        let mut chain = self.chain_open(b, &tags, BlockType::Empty);
        for (slot, ix) in Self::chain_order(&chain, arms.len())
            .into_iter()
            .enumerate()
        {
            let arm = &arms[ix];
            self.chain_enter(b, &mut chain, slot, |b| match probes[ix] {
                (_, Some(l)) => {
                    b.ins(&Instruction::LocalGet(l));
                }
                (tag, None) => Self::tag_is(b, addr, tag.map(|t| t as u64)),
            });
            // A payload's slot is the width of the ones before it (RFC-0126
            // §8.4), so the binder needs the whole variant's list and not its
            // own type. The row binds them in payload order.
            let ptys: Vec<Type> = match tags[ix] {
                Some(t) => sum[t].payload.clone(),
                None => Vec::new(),
            };
            let from = b.mark();
            for (i, bn) in arm.binds.iter().enumerate() {
                let ty = body.names[*bn as usize].ty.clone();
                let layout = matches!(self.cx.repr(&ty, line)?, Repr::Agg(_));
                let read = arm
                    .reads(on)
                    .iter()
                    .any(|r| matches!(r, St::Let(x, _) if x == bn));
                let at = match self.word2(&ty)? {
                    // A layout the row reads out of the scrutinee is read
                    // where it lies ([`vyrn_lower::core::Arm::reads`]).
                    k @ (Word::Inline2 | Word::Boxed) if layout && read => {
                        let off = sl.fields[self.cx.payload_slot(&ptys, i)];
                        payload_at(b, addr, off, matches!(k, Word::Inline2));
                        let Place::Local(l) =
                            self.place_for(b, &Repr::Scalar(ValType::I32), line)?
                        else {
                            return unsupported("an address with no local", line);
                        };
                        b.ins(&Instruction::LocalSet(l));
                        Place::Local(l)
                    }
                    _ => self.bind_payload(b, addr, &sl, &ptys, i, &ty, line, free_box)?,
                };
                self.core_bind(b, body, w, *bn, at, ty)?;
            }
            let to = b.mark();
            self.core_stmts(m, b, body, w, &arm.body[arm.reads(on).len()..])?;
            // A binder's scope is its arm, so the slots a binder moved out
            // into go back at the arm's end, as the arm gives them back.
            if from < to {
                b.give_back(from, to);
            }
            self.chain_leave(b, &chain, slot);
        }
        self.chain_close(b, &chain);
        Ok(())
    }

    /// One function body, emitted from the core's own statements — RFC-0125
    /// §3 M3, the driver slice.
    ///
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
    /// Hold `core`'s rows by statement and a place table for its names, so
    /// either walk may read it.
    fn core_enter(&mut self, core: &vyrn_lower::core::Body) {
        self.core_at = core.rows_by_statement();
        self.core_w = Walked {
            at: vec![None; core.names.len()],
            reads: core.reads(),
            occurs: core.occurrences(),
            slot: vec![None; core.names.len()],
            held: None,
            landed: None,
            walks: vec![None; core.names.len()],
            built: vec![None; core.names.len()],
            bufs: vec![None; core.names.len()],
            over: Vec::new(),
            pulled: vec![None; core.names.len()],
        };
    }

    fn core_body(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
    ) -> Result<(), String> {
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
        self.core_bound = match (s, core_head(&run)) {
            (Stmt::Let { ty: Some(t), .. }, Some(St::Let(n, _))) => Some((*n, t.clone())),
            _ => None,
        };
        let mut w = std::mem::take(&mut self.core_w);
        let r = self.core_stmts(m, b, &body, &mut w, &run);
        self.core_w = w;
        self.core_bound = None;
        r.map(|()| true)
    }

    /// Lift each lambda literal a call row of `rows` names as a target, at
    /// the callee's `fn` parameter type, so the screen finds its signature
    /// ([`Cx::lambda_sig`]). The arm lifts the same literal at the same type
    /// to the same instance, so rows the screen refuses lose nothing.
    /// [`lower_body`] asks it once, of the body's rows, before either screen:
    /// every statement's run is a part of them.
    fn core_lift_targets(&mut self, m: &mut Module, rows: &[St]) {
        let mut calls = Vec::new();
        for r in rows {
            core_leaf_rows(r, &mut |x| {
                if let St::Let(_, rhs) | St::Do { rhs, .. } = x {
                    if let Rhs::Call {
                        callee,
                        solved,
                        targets,
                        kind: Callee::Fn,
                        ..
                    } = rhs
                    {
                        calls.push((callee.clone(), solved.clone(), targets.clone()));
                    }
                }
            });
        }
        for (callee, solved, targets) in calls {
            let Some(f) = self.cx.higher_order.get(callee.as_str()).copied() else {
                continue;
            };
            let Some((_, subst)) = solved_instance(f, &solved) else {
                continue;
            };
            let fns = f.params.iter().filter_map(|p| match &p.ty {
                Type::Fn(ptys, ret) => Some((ptys, ret)),
                _ => None,
            });
            for ((ptys, ret), t) in fns.zip(&targets) {
                let Target::Lambda(key, caps) = t else {
                    continue;
                };
                if self.cx.lambda_sig(key).is_some() {
                    continue;
                }
                let Some(lit) = self.literal(key) else {
                    continue;
                };
                let ptys: Vec<Type> = (ptys.iter())
                    .map(|t| ftypes::substitute(t, &subst))
                    .collect();
                let ret = ftypes::substitute(ret, &subst);
                let saved = std::mem::take(&mut self.expect);
                let _ = self.lift_lambda(m, lit, &ptys, &ret, Some(caps), lit.line());
                self.expect = saved;
            }
        }
    }

    /// The core's rows for one source statement, where this walk reads all of
    /// them — RFC-0125 §3 M3, the interleave slice's screen.
    ///
    /// Three clauses, and each one is a line of the record. The FRAME clause: a
    /// placed release and an aggregate destination are emissions the rows do
    /// not carry. The PLACE clause, per statement rather than per body: a name
    /// the run reads, or a `let` binds, has a place whose type is its type on
    /// both walks, whatever that type is. The STATEMENT screen:
    /// [`Fn_::core_readable`], unchanged.
    fn core_run(&self, body: &vyrn_lower::core::Body, s: &Stmt) -> Option<Vec<St>> {
        // A node is an ADDRESS, and `project::iterate_loop`'s copy of a loop
        // body gives a statement a second one. `key_of` is the mapping back,
        // and ten other readers of the plan in this file ask through it.
        let at = self.cx.plan.key_of(s as *const Stmt as usize);
        // A `region`'s row names its block, which is not the statement.
        let run = match s {
            Stmt::Region { body, .. } => self
                .core_at
                .get(&self.cx.plan.key_of(body as *const Block as usize))?,
            _ => self.core_at.get(&at)?,
        };
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
        // A stream cursor is a release at a function exit that no plan row
        // names, so it belongs to a `return` and to nothing else:
        // `streamlazy.vyrn` lost 51 bytes of a cursor when the rows took its
        // `return`. The clause is about the RUN rather than about the form,
        // because a subtree carries the `return` of every branch under it.
        if !self.cursors.is_empty() && run.iter().any(core_returns) {
            return None;
        }
        // A stream handed to a call is the callee's to close, where the arm
        // hands it on, and the core states its release after the call. Streams
        // wait for the runtime in Vyrn.
        if run.iter().any(|r| {
            matches!(r, St::Drop(n, ..) if matches!(body.names[*n as usize].ty, Type::Stream(_)))
        }) {
            return None;
        }
        // A statement inside a `while` the arm emits that names a binding the
        // arm's hoist holds in locals, which the rows would walk again.
        if !self.walks.is_empty() {
            let mut names = Vec::new();
            for st in run {
                vyrn_lower::core::names_in(st, &mut names);
            }
            if names.iter().any(|n| {
                self.walks
                    .contains_key(body.names[*n as usize].source.as_str())
            }) {
                return None;
            }
        }
        // A result checked where a `return` of the run hands it back, under a
        // branch or at the run's end, is a check the row does not state
        // (RFC-0079).
        //
        // A place argument is a move-out window's call ([`Arg`]), whose `let`
        // and put-back are source statements with no rows: the arm would emit
        // them around the call, and the put-back would undo it.
        let mut released = Vec::new();
        let (mut returns, mut windowed) = (false, false);
        for r in run {
            core_leaf_rows(r, &mut |x| match x {
                St::Return { .. } => returns = true,
                St::Row { name, .. } => released.push(*name),
                St::Let(_, Rhs::Call { args, .. })
                | St::Do {
                    rhs: Rhs::Call { args, .. },
                    ..
                } => windowed |= args.iter().any(|(a, _)| matches!(a, Arg::Place(_))),
                _ => {}
            });
        }
        if windowed || self.dest.is_some() && self.checks(&self.ret_ty) && returns {
            return None;
        }
        // A node is an ADDRESS. The row's FORM and the name it binds are
        // checked against the statement's, so a row is never read as a
        // statement it did not come from.
        let named =
            |n: &vyrn_lower::core::Name, name: &String| &body.names[*n as usize].source == name;
        let mut annotated = None;
        // The statement's own binding, and the type the reader annotated it
        // with: what a made layout is built into (RFC-0125 M7).
        let mut bound: (Option<vyrn_lower::core::Name>, Option<&Type>) = (None, None);
        match (s, core_head(run)?) {
            (Stmt::Let { name, ty, .. }, St::Let(n, rhs)) if named(n, name) => {
                bound = (Some(*n), ty.as_ref());
                // The arm binds the ANNOTATION where the reader wrote one and
                // the recorded type of the initializer otherwise, and the two
                // walks have to bind the same type or they pick different
                // instructions for it: `simd.vyrn`'s `a / b` on two `Float32`
                // widens to `Float64` in the arm and stays single in the row.
                if let Some(t) = ty {
                    // A made layout is the one row whose destination is the
                    // ANNOTATION's layout rather than the value's, and
                    // [`Fn_::core_stmts`] builds into it (RFC-0125 M7). The
                    // `where` screen is the same one either way: an
                    // annotation that checks is a row the core does not state.
                    // An aggregate call writes through the same destination,
                    // and the arm converts its result to the annotation after
                    // the call, which the row does not state.
                    let call = self.core_agg_call(body, rhs);
                    if matches!(rhs, Rhs::Make(..)) || self.core_ctor(body, rhs) || call {
                        if self.checks(t)
                            || (call
                                && self.cx.resolve(t)
                                    != self.cx.resolve(&body.names[*n as usize].ty))
                        {
                            return None;
                        }
                    } else {
                        // As WRITTEN, not resolved: `let a: Age = 25` is a
                        // `where` type, and `Age` resolved to `Int64` is the
                        // flow that does not check (M2d). The row states the
                        // check where it types the name at the annotation.
                        let named = &body.names[*n as usize].ty;
                        if self.cx.resolve(t) != self.cx.resolve(named)
                            || (self.checks(t) && self.cx.sub(t) != *named)
                        {
                            return None;
                        }
                        annotated = Some(*n);
                    }
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
                if line == at
                    && (core_scalar(&self.ret_ty)
                        || (matches!(self.ret, Repr::Agg(_)) && !self.checks(&self.ret_ty))) => {}
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
            (Stmt::Region { .. }, St::Block { region: true, .. }) => {}
            (Stmt::Drop { .. }, St::Drop(..)) => {}
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
        // A name the run binds by a made layout or an aggregate call is built
        // at the layout's type, so the type clause below does not ask it
        // (RFC-0125 M7). A temporary the run's `return` hands back is built in
        // the caller's storage.
        let lands: Vec<_> = (0..run.len())
            .filter(|&i| self.core_lands(body, run, i, &self.core_w.reads))
            .filter_map(|i| match &run[i] {
                St::Let(n, _) => Some(*n),
                _ => None,
            })
            .collect();
        let mut made = Vec::new();
        for (n, rhs) in &lets {
            if matches!(rhs, Rhs::Make(..)) || self.core_ctor(body, rhs) {
                let made_ty = self.core_made_ty(body, *n);
                let at = match bound {
                    _ if lands.contains(n) => &self.ret_ty,
                    (Some(top), Some(t)) if top == *n => t,
                    _ => &made_ty,
                };
                if !self.core_makes(body, at, rhs) {
                    return None;
                }
                made.push(*n);
            } else if self.core_agg_call(body, rhs) {
                made.push(*n);
            }
        }
        let rebuilt: Vec<_> = (0..run.len())
            .filter_map(|i| self.core_rebuilt(body, run, i))
            .flat_map(|(x, t)| [x, t])
            .collect();
        // A made layout other than the statement's own binding lands in a slot
        // of its own, which the row gives back at its extent's end, and is
        // built at its name's type. A `let` under the statement that annotates
        // another type is the arm's, as it is for the per-body walk.
        let under = self.annotations(|fs| {
            hoist_stmt(
                s,
                &mut std::collections::HashSet::new(),
                &mut Hoist {
                    fe: &mut |_| {},
                    fs,
                },
            )
        });
        if made.iter().any(|n| {
            Some(*n) != bound.0
                && !lands.contains(n)
                && self.annotated_apart(&under, &body.names[*n as usize])
        }) {
            return None;
        }
        for (n, rhs) in &lets {
            if Some(*n) == annotated || made.contains(n) || rebuilt.contains(n) {
                continue;
            }
            let info = &body.names[*n as usize];
            let want = self.core_arm_ty(body, rhs)?;
            let got = self.cx.resolve(&info.ty);
            // A truth value is the one result an operator states and its
            // operands do not.
            if got != self.cx.resolve(&want) && got != Type::Bool {
                return None;
            }
        }
        let mut names = Vec::new();
        let mut switched = Vec::new();
        for st in run {
            core_switched(st, true, &mut switched);
            vyrn_lower::core::names_in(st, &mut names);
        }
        // A release row names a binding the PLACEMENT holds, and the emitter
        // reads its place off `rel_slots` rather than off this walk's own
        // table, at any depth of the run. So the name is not one this walk has
        // to read, and the place clause below is not asked about it.
        for r in &released {
            if let Some(i) = names.iter().position(|n| n == r) {
                names.swap_remove(i);
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
            if lets.iter().any(|(b, _)| b == n) || switched.contains(n) {
                continue;
            }
            let (_, ty) = self.core_place(&self.core_w, body, *n)?;
            // And the two walks have to agree about the type of a name they
            // share: `stringops.vyrn` compared two bytes at byte width from the
            // row and at `Int64` from the frame, for the same source. The
            // frame's answer is as DECLARED, so a `where` type is refused here
            // as it is at a `let`.
            let named = &body.names[*n as usize].ty;
            if self.cx.resolve(&ty) != self.cx.resolve(named)
                || ((self.checks(&ty) || self.checks(named)) && self.cx.sub(&ty) != *named)
            {
                return None;
            }
        }
        self.core_readable(body, run, &self.core_w.reads, &[])
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
        self.lookup(&info.source, info.line).ok()
    }

    fn core_stmts(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        ss: &[St],
    ) -> Result<(), String> {
        let ends = vyrn_lower::core::extent_ends(ss, &w.occurs);
        let mut due = Vec::new();
        let (mut last, mut mark): (Option<usize>, u32) = (None, b.mark());
        for (i, s) in ss.iter().enumerate() {
            if let Some(j) = last {
                core_row_done(b, w, &ss[j], mark);
                self.core_give_back(b, w, &mut due, &ends[j]);
            }
            (last, mark) = (Some(i), b.mark());
            if let St::Store {
                place: vyrn_lower::core::Place::Name(n),
                line,
                ..
            } = s
            {
                if w.at[*n as usize].is_none() && self.core_joins(body, *n) {
                    let ty = body.names[*n as usize].ty.clone();
                    let r = self.cx.repr(&ty, *line)?;
                    let off = self.core_slot(b, w, *n, &r, *line)?;
                    self.core_bind(b, body, w, *n, Place::Slot(off), ty)?;
                }
            }
            match s {
                // A receiver rebuilt in place: the result is the receiver's
                // own storage, so the name takes the receiver's place.
                St::Let(
                    n,
                    rhs @ Rhs::Call {
                        callee, kind, args, ..
                    },
                ) if self.core_rebuild(body, rhs) => {
                    let line = body.names[*n as usize].line;
                    let Some(((Arg::Val(Val::Name(x)), _), rest)) = args.split_first() else {
                        return unsupported("a rebuild of no named receiver", line);
                    };
                    let Some(rest) = arg_vals(rest) else {
                        return unsupported("a rebuild with a place argument", line);
                    };
                    if body.names[*x as usize].grows {
                        // `@strAppend`: the store after it states whether the
                        // buffer the accumulator held is this path's to free.
                        let Some(St::Store { releases, .. }) =
                            ss.get(i + 1 + drops_ahead(ss[i + 1..].iter()))
                        else {
                            return unsupported("an append with no store", line);
                        };
                        // Module state grows at its fixed address, with the
                        // word the module reserved for it.
                        let (place, own) = match core_global(body, *x) {
                            Some(g) => match (self.lookup(g, line)?.0, self.cx.gappend.get(g)) {
                                (at @ Place::Static(_), Some(&word)) => (at, Place::Static(word)),
                                _ => return unsupported("an append with no ownership word", line),
                            },
                            None => {
                                let Some((Place::Local(l), _)) = self.core_place(w, body, *x)
                                else {
                                    return unsupported(
                                        "an append into a place with no local",
                                        line,
                                    );
                                };
                                let Some(&at) = self.str_append.get(&l) else {
                                    return unsupported("an append with no ownership word", line);
                                };
                                (Place::Local(l), Place::Slot(at))
                            }
                        };
                        let mut operand =
                            |f: &mut Self, m: &mut Module, b: &mut Frame, k: usize| {
                                f.core_val(m, b, body, w, &rest[k].0, &Type::Str, line)?;
                                Ok(None)
                            };
                        let parts = rest.len();
                        self.append_in_place(
                            m,
                            b,
                            place,
                            own,
                            *releases,
                            parts,
                            &mut operand,
                            line,
                        )?;
                    } else {
                        self.core_call(
                            m,
                            b,
                            body,
                            w,
                            callee,
                            *kind,
                            &[],
                            &[],
                            args,
                            None,
                            None,
                            line,
                        )?;
                        b.ins(&Instruction::Drop);
                    }
                    w.at[*n as usize] = self.core_place(w, body, *x);
                    // With no store to put it back, the result holds the
                    // receiver's slot for its own extent, unless the receiver
                    // lives on to be stored again: a join after the rebuild
                    // puts the result back into it.
                    if self
                        .core_rebuilt(body, ss, i + 1 + drops_ahead(ss[i + 1..].iter()))
                        .is_none()
                        && !self.core_restored(body, *x)
                    {
                        w.slot[*n as usize] = w.slot[*x as usize].take();
                    }
                }
                // The head of a `for` over a stream: the element goes into a
                // place of its own, which the read at this row's name binds,
                // and the name is whether one came, as the arm's `for` pulls.
                St::Let(
                    n,
                    Rhs::Call {
                        callee, kind, args, ..
                    },
                ) if matches!(core_builtin(callee, *kind), Some(Spec::Pulls)) => {
                    let line = body.names[*n as usize].line;
                    let [(Arg::Val(Val::Name(s)), _)] = args.as_slice() else {
                        return unsupported("a pull of no named stream", line);
                    };
                    let Some(elem) = self.core_stream_elem(body, *s) else {
                        return unsupported("a pull of no stream", line);
                    };
                    let src = self.core_addr_local(b, w, body, *s, line)?;
                    let r = self.cx.repr(&elem, line)?;
                    let place = self.place_for(b, &r, line)?;
                    let has = self.stream_next(m, b, src, place, &elem, line)?;
                    w.pulled[*n as usize] = Some((place, elem));
                    self.core_bind(b, body, w, *n, Place::Local(has), Type::Bool)?;
                }
                St::Let(n, Rhs::Read(vyrn_lower::core::Place::Elem(s, c)))
                    if self.core_pulls(body, s) =>
                {
                    let line = body.names[*n as usize].line;
                    let Some((place, ty)) = (match c {
                        Val::Name(c) => w.pulled[*c as usize].take(),
                        Val::Lit(_) => None,
                    }) else {
                        return unsupported("an element read of a stream no pull wrote", line);
                    };
                    self.core_bind(b, body, w, *n, place, ty)?;
                }
                // The store that puts the rebuilt receiver back, which the
                // rebuild already wrote.
                St::Store { .. } if self.core_rebuilt(body, ss, i).is_some() => {}
                // A module-state receiver is read at its address by the
                // append, and by nothing else.
                St::Let(n, _) if core_global(body, *n).is_some() => {}
                // A LAYOUT MADE, built into the binding's own slot — RFC-0125
                // M7. It is a `let` arm of its own because the slot has to
                // exist before the parts are written: the arm below evaluates
                // and then binds, and a record or an array is never on the
                // operand stack to be bound.
                St::Let(n, rhs)
                    if self.core_makes(body, &self.core_made_ty(body, *n), rhs)
                        || self
                            .core_bound
                            .as_ref()
                            .is_some_and(|(top, t)| top == n && self.core_makes(body, t, rhs)) =>
                {
                    let info = &body.names[*n as usize];
                    let line = info.line;
                    let taken = w.bufs[*n as usize].take();
                    if let Some(dest) = self.core_part_dest(b, body, w, ss, i, line)? {
                        let Some(at) = self.core_part_at(body, ss, i, w) else {
                            return unsupported("a part with no parent", line);
                        };
                        let ty = at.ty;
                        mark = b.mark();
                        self.core_make(m, b, body, w, dest, &ty, rhs, taken, line)?;
                        continue;
                    }
                    // The storage a part took before this row.
                    let pre = w.built[*n as usize].take();
                    if self.core_lands(body, ss, i, &w.reads) {
                        let dest = Dest::Addr(self.core_out(line)?, 0);
                        let ty = self.ret_ty.clone();
                        self.core_make(m, b, body, w, dest, &ty, rhs, taken, line)?;
                        w.landed = Some(*n);
                        continue;
                    }
                    // The DESTINATION's type, which is the annotation where the
                    // reader wrote one: the arm takes the slot and writes the
                    // hint from it, and the row states the value's type instead.
                    let ty = match self.core_bound.take_if(|(top, _)| top == n) {
                        Some((_, t)) => t,
                        None => self.core_made_ty(body, *n),
                    };
                    let r = self.cx.repr(&ty, line)?;
                    if !matches!(r, Repr::Agg(_)) {
                        return unsupported("a made layout with no layout", line);
                    }
                    let place = match pre {
                        Some(Dest::Slot(off)) => Place::Slot(off),
                        _ => Place::Slot(self.core_slot(b, w, *n, &r, line)?),
                    };
                    let dest = Dest::of(place).expect("a slot is a destination");
                    self.core_make(m, b, body, w, dest, &ty, rhs, taken, line)?;
                    self.core_bind(b, body, w, *n, place, ty)?;
                }
                // A LAYOUT TAKEN OUT OF A FIELD in part position: the header
                // moves to the part's offset, as the arm's `consume t.d` in a
                // literal moves it, and the field is the hole the root's
                // release carries.
                St::Let(n, Rhs::Take(p)) if self.core_part_at(body, ss, i, w).is_some() => {
                    let line = body.names[*n as usize].line;
                    let Some(dest) = self.core_part_dest(b, body, w, ss, i, line)? else {
                        return unsupported("a taken layout with no parent", line);
                    };
                    let Repr::Agg(l) = self.cx.repr(&body.names[*n as usize].ty, line)? else {
                        return unsupported("a taken layout with no layout", line);
                    };
                    dest.addr(b, 0);
                    let (_, off) = self.core_addr(m, b, body, w, p, line)?;
                    self.core_step(b, off);
                    agg_landed(b, l.size, false);
                }
                // A HEADER a loop walks: the container's value in a local,
                // taken apart once here, so every element and length read of
                // the loop reads the parts (RFC-0125 M7). The kernel ends the
                // borrow at any write under the container, and a read after
                // one is refused, so the parts cannot go stale.
                St::Let(n, Rhs::Read(p)) if self.core_walked(body, *n) => {
                    let info = &body.names[*n as usize];
                    let (line, ty) = (info.line, info.ty.clone());
                    if let Repr::Agg(_) = self.cx.repr(&ty, line)? {
                        let (_, off) = self.core_addr(m, b, body, w, p, line)?;
                        self.core_step(b, off);
                    } else {
                        self.core_read(m, b, body, w, p, line)?;
                    }
                    w.walks[*n as usize] = Some(self.walk(b, &ty, line)?);
                }
                // A LAYOUT READ OUT OF A PLACE, or taken out of one and handed
                // back, held as the place's address in a local, the way a
                // layout parameter is (RFC-0125 M7).
                St::Let(n, Rhs::Read(p) | Rhs::Take(p)) if self.core_alias(body, *n).is_some() => {
                    let line = body.names[*n as usize].line;
                    let (ty, off) = self.core_addr(m, b, body, w, p, line)?;
                    self.core_step(b, off);
                    let Place::Local(l) = self.place_for(b, &Repr::Scalar(ValType::I32), line)?
                    else {
                        return unsupported("an address with no local", line);
                    };
                    b.ins(&Instruction::LocalSet(l));
                    self.core_bind(b, body, w, *n, Place::Local(l), ty)?;
                }
                // A MOVE of a layout: the name takes the place the moved name
                // held, and its slot's extent with it.
                // A literal's growable array: `@list` takes the literal, which
                // was made in its heap buffer, so the name takes its place.
                St::Let(n, _) if self.core_lists(body).iter().any(|(_, a)| a == n) => {
                    let info = &body.names[*n as usize];
                    let Some(&(x, _)) = self.core_lists(body).iter().find(|(_, a)| a == n) else {
                        return unsupported("a list of no literal", info.line);
                    };
                    // A literal made as its parent's part has no place of its
                    // own: the part the parent reads is this name.
                    if let Some(d) = w.built[x as usize].take() {
                        w.built[*n as usize] = Some(d);
                        continue;
                    }
                    let Some((place, _)) = self.core_place(w, body, x) else {
                        return unsupported("a list of a literal with no place", info.line);
                    };
                    w.slot[*n as usize] = w.slot[x as usize].take();
                    self.core_bind(b, body, w, *n, place, info.ty.clone())?;
                }
                St::Let(n, Rhs::Val(Val::Name(x))) if self.core_renames(body, *n).is_some() => {
                    let info = &body.names[*n as usize];
                    let Some((place, _)) = self.core_place(w, body, *x) else {
                        return unsupported("a move of a name with no place", info.line);
                    };
                    w.slot[*n as usize] = w.slot[*x as usize].take();
                    self.core_bind(b, body, w, *n, place, info.ty.clone())?;
                }
                St::Let(n, _) if self.core_copies(body, *n).is_some() => {
                    let line = body.names[*n as usize].line;
                    let Some(p) = self.core_copies(body, *n) else {
                        return unsupported("a copy of no place", line);
                    };
                    let ty = body.names[*n as usize].ty.clone();
                    let r = self.cx.repr(&ty, line)?;
                    let Repr::Agg(l) = &r else {
                        return unsupported("a copy of no layout", line);
                    };
                    let slot = self.core_slot(b, w, *n, &r, line)?;
                    b.slot(slot);
                    let (_, off) = self.core_addr(m, b, body, w, &p, line)?;
                    self.core_step(b, off);
                    agg_landed(b, l.size, false);
                    self.core_bind(b, body, w, *n, Place::Slot(slot), ty)?;
                }
                // An AGGREGATE CALL RESULT, written through the out-pointer
                // into the binding's own slot, or into the caller's storage
                // when the `return` after it hands the temporary back. The
                // bytes are the AST arm's at `let p = f(a)` and at
                // `return f(a)`: [`Fn_::agg_into`]'s destination, then the
                // call's own convention ([`Fn_::out_ptr`]). Any other
                // temporary is the storage the call wrote, and its name holds
                // that address, as the arm hands the call's own slot on: to
                // the reader, or to the `match` or `for` the plan keys it by.
                St::Let(
                    n,
                    rhs @ (Rhs::Call { .. } | Rhs::Read(vyrn_lower::core::Place::Key(..))),
                ) if self.core_agg_call(body, rhs) => {
                    let line = body.names[*n as usize].line;
                    let lands = self.core_lands(body, ss, i, &w.reads);
                    let bound = self.core_bound.take_if(|(top, _)| top == n);
                    let ty = match bound {
                        _ if lands => self.ret_ty.clone(),
                        Some((_, t)) => t,
                        None => body.names[*n as usize].ty.clone(),
                    };
                    let r = self.cx.repr(&ty, line)?;
                    let Repr::Agg(l) = &r else {
                        return unsupported("an aggregate call with no layout", line);
                    };
                    let part = self.core_part_dest(b, body, w, ss, i, line)?;
                    mark = b.mark();
                    let (dest, place) = if lands {
                        (Some(Dest::Addr(self.core_out(line)?, 0)), None)
                    } else if part.is_some() {
                        (part, None)
                    } else if body.names[*n as usize].source.starts_with('@') {
                        (None, None)
                    } else {
                        let off = self.core_slot(b, w, *n, &r, line)?;
                        (Some(Dest::Slot(off)), Some(Place::Slot(off)))
                    };
                    let from = b.mark();
                    if let Some(d) = dest {
                        d.addr(b, 0);
                    }
                    self.dest_used = false;
                    match rhs {
                        Rhs::Call {
                            callee,
                            kind,
                            args,
                            solved,
                            targets,
                            ret,
                            ..
                        } => {
                            let hint = dest.map(|d| (d, ty.clone()));
                            self.core_call(
                                m,
                                b,
                                body,
                                w,
                                callee,
                                *kind,
                                solved,
                                targets,
                                args,
                                ret.as_ref(),
                                hint,
                                line,
                            )?;
                        }
                        Rhs::Read(vyrn_lower::core::Place::Key(base, k)) => {
                            let (mty, off) = self.core_addr(m, b, body, w, base, line)?;
                            self.core_step(b, off);
                            let mty = self.cx.resolve(&mty);
                            let Type::Map(_, val) = &mty else {
                                return unsupported("a key read of no map", line);
                            };
                            let val = (**val).clone();
                            let mut key =
                                |s: &mut Self, m: &mut Module, b: &mut Frame, t: &Type| {
                                    s.core_val(m, b, body, w, k, t, line)
                                };
                            self.map_at(m, b, &mty, &val, &mut key, line)?;
                        }
                        _ => return unsupported("an aggregate row that is no call", line),
                    }
                    let used = std::mem::take(&mut self.dest_used);
                    match (dest, place) {
                        // The name holds the call's own slot, to the end of
                        // its extent.
                        (None, _) => {
                            w.slot[*n as usize] = Some((from, b.mark()));
                            let a = b.local(ValType::I32);
                            b.ins(&Instruction::LocalSet(a));
                            self.core_bind(b, body, w, *n, Place::Local(a), ty)?;
                        }
                        (Some(_), place) => {
                            agg_landed(b, l.size, used);
                            match place {
                                Some(place) => self.core_bind(b, body, w, *n, place, ty)?,
                                None if part.is_some() => {}
                                None => w.landed = Some(*n),
                            }
                        }
                    }
                }
                St::Let(n, rhs) => {
                    let info = &body.names[*n as usize];
                    let line = info.line;
                    self.core_rhs(m, b, body, w, rhs, &info.ty, line)?;
                    if self.core_unit(&info.ty) {
                        continue;
                    }
                    // The slot rule (RFC-0125 M7). A temporary the next
                    // statement reads once, first, and nothing else reads
                    // stays on wasm's operand stack. Every other name takes a
                    // local from [`Fn_::place_for`], in row order.
                    if info.binding.is_none()
                        && w.reads[*n as usize] == 1
                        && self.core_first_read(body, ss.get(i + 1)) == Some(*n)
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
                    self.core_bind(b, body, w, *n, place, info.ty.clone())?;
                    if let (true, Some(site)) = (info.grows, info.binding) {
                        let literal = matches!(rhs, Rhs::Val(Val::Lit(Lit::Str(_))));
                        self.core_word(b, w, *n, l, site, literal);
                    }
                }
                St::Store {
                    place: vyrn_lower::core::Place::Name(n),
                    value,
                    line,
                    ..
                } if self.core_unit(&body.names[*n as usize].ty) => {
                    self.core_val(m, b, body, w, value, &Type::Unit, *line)?;
                }
                St::Store {
                    place: vyrn_lower::core::Place::Name(n),
                    value,
                    line,
                    releases,
                    ..
                } if self.core_framed(&body.names[*n as usize].ty) => {
                    // The temporary an `if` expression joins through is stored
                    // by each branch and bound by the `let` after them
                    // (RFC-0030), so the first store it meets takes its slot.
                    let (l, ty) = match self.core_place(w, body, *n) {
                        Some((Place::Local(l), ty)) => (l, ty),
                        Some(_) => {
                            return unsupported("a core store into a place with no local", *line)
                        }
                        None => {
                            let ty = body.names[*n as usize].ty.clone();
                            let r = self.cx.repr(&ty, *line)?;
                            let Place::Local(l) = self.place_for(b, &r, *line)? else {
                                return unsupported("a core store of an aggregate", *line);
                            };
                            self.core_bind(b, body, w, *n, Place::Local(l), ty.clone())?;
                            (l, ty)
                        }
                    };
                    // The store releases what the name held where the row says
                    // so, in the arm's order: the old value aside, the new one
                    // in, the old one freed.
                    let snap = match (*releases, self.cx.repr(&ty, *line)?) {
                        (false, _) => None,
                        (true, Repr::Scalar(v)) => self.snap_word(b, l, v, &ty, *line)?,
                        (true, _) => {
                            return unsupported("a core store that releases an aggregate", *line)
                        }
                    };
                    self.core_val(m, b, body, w, value, &ty, *line)?;
                    b.ins(&Instruction::LocalSet(l));
                    self.free_snap(m, b, snap, *line)?;
                    if let Some(&at) = self.str_append.get(&l) {
                        disown(b, Place::Slot(at));
                    }
                }
                // A map entry: the insert or update the arm and a map literal
                // make ([`Fn_::map_set`]), which releases the value it
                // displaces where the row says so.
                St::Store {
                    place: vyrn_lower::core::Place::Key(base, k),
                    value,
                    line,
                    releases,
                    ..
                } => {
                    let (mty, off) = self.core_addr(m, b, body, w, base, *line)?;
                    self.core_step(b, off);
                    let Type::Map(key_t, val) = self.cx.resolve(&mty) else {
                        return unsupported("a key store into no map", *line);
                    };
                    let hdr = b.local(ValType::I32);
                    b.ins(&Instruction::LocalSet(hdr));
                    let l = self.layout_of(&mty, *line)?;
                    let kv = [k.clone(), value.clone()];
                    let mut parts = Parts::Core(body, &kv, w);
                    self.map_set(m, b, hdr, &l, &mut parts, 0, &key_t, &val, *releases, *line)?;
                }
                // A place with an address: the address, what the place held
                // kept aside where the row releases it, the value landed, and
                // the kept value freed, the arm's order at `x = v`, `r.f = v`
                // and `a[i] = v`. A layout lands as a copy of its bytes. A
                // String in module state has its word cleared, as a String
                // name's.
                St::Store {
                    place,
                    value,
                    line,
                    releases,
                    ..
                } => {
                    let (ty, off) = self.core_addr(m, b, body, w, place, *line)?;
                    self.core_step(b, off);
                    let snap = if *releases {
                        let a = b.local(ValType::I32);
                        b.ins(&Instruction::LocalTee(a));
                        self.snap_at(b, a, &ty, *line)?
                    } else {
                        None
                    };
                    self.core_val(m, b, body, w, value, &ty, *line)?;
                    match self.cx.repr(&ty, *line)? {
                        Repr::Agg(l) => agg_landed(b, l.size, false),
                        _ => {
                            b.ins(&store_of(&self.cx.ll(&ty)));
                        }
                    }
                    self.free_snap(m, b, snap, *line)?;
                    if let vyrn_lower::core::Place::Global(g) = place {
                        if let Some(&word) = self.cx.gappend.get(g) {
                            disown(b, Place::Static(word));
                        }
                    }
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
                    && matches!(els.as_slice(), [St::Break { site: 0, .. }])
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
                    let over = w.over.len();
                    for p in ss[..i].iter().rev() {
                        match p {
                            St::Let(h, Rhs::Read(vyrn_lower::core::Place::Name(r)))
                                if body.names[*h as usize].walked
                                    == Some(vyrn_lower::core::Walk::While) =>
                            {
                                w.over.push((*r, *h))
                            }
                            _ => break,
                        }
                    }
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
                    w.over.truncate(over);
                    r?;
                    let back = self.br_to(cont);
                    b.ins(&Instruction::Br(back));
                    self.depth -= 1;
                    b.ins(&Instruction::End);
                    self.depth -= 1;
                    b.ins(&Instruction::End);
                }
                St::Break { .. } | St::Continue { .. } => {
                    let Some(&(brk, cont, regions)) = self.loops.last() else {
                        return unsupported("a core exit outside a loop", 0);
                    };
                    // The frames the loop body opened are the rows before this
                    // one, each emitted where it stands.
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
                St::Return { value, line, .. } => {
                    match (value, self.ret.agg().map(|l| l.size)) {
                        // The caller's storage, which the `let` before this
                        // row built into or which the value is copied into:
                        // [`Fn_::ret_value`]'s two cases.
                        (Some(Val::Name(n)), Some(size)) => {
                            if w.landed.take() != Some(*n) {
                                Dest::Addr(self.core_out(*line)?, 0).addr(b, 0);
                                self.core_addr_of(b, w, body, *n, *line)?;
                                agg_landed(b, size, false);
                            }
                        }
                        (Some(v), _) => {
                            let want = self.ret_ty.clone();
                            self.core_val(m, b, body, w, v, &want, *line)?;
                        }
                        (None, _) if matches!(self.ret, Repr::Unit) => {}
                        (None, _) => {
                            return unsupported(
                                "a return whose value does not match the signature",
                                *line,
                            )
                        }
                    }
                    self.core_releases(m, b, body)?;
                    // Every region scope this return leaves, as the AST arm
                    // does: a returned value built inside a region points into
                    // the arena and its caller owns it, so the scope POPS
                    // rather than frees.
                    self.exit_regions_above(b, 0, false);
                    b.ins(&Instruction::Br(self.depth));
                }
                // An exit's releases run AFTER the read it hands back, which
                // is the order the AST arm writes and the order a reader of
                // the wasm expects: the value is on the operand stack and a
                // release does not disturb it. The row states the release and
                // not its place among the reads, so the two commute.
                St::Row {
                    name, holes, exit, ..
                } => {
                    self.core_rows.push((*name, holes.clone(), *exit));
                    if !matches!(
                        ss.get(i + 1),
                        Some(St::Row { .. } | St::Return { value: Some(_), .. })
                    ) {
                        self.core_releases(m, b, body)?;
                    }
                }
                St::Switch {
                    on,
                    arms,
                    owns,
                    line,
                    ..
                } => self.core_switch(m, b, body, w, on, arms, *owns, *line)?,
                St::Drop(n, _, line, holes) => self.core_drop(m, b, body, w, *n, holes, *line)?,
                St::Trap => {
                    b.ins(&Instruction::Unreachable);
                }
                // An expression for its effect. What it leaves on the stack
                // is dropped, or the enclosing block's type will not check —
                // the same sentence the AST walk's statement arm writes, on
                // the row rather than on the node.
                St::Do { rhs, line, .. } if self.core_checks_made(body, rhs).is_some() => {
                    let (decl, n) = self.core_checks_made(body, rhs).expect("the guard's");
                    self.core_val(
                        m,
                        b,
                        body,
                        w,
                        &Val::Name(n),
                        &Type::Named(decl.name.clone()),
                        *line,
                    )?;
                    self.emit_validation(b, &decl, *line)?;
                    b.ins(&Instruction::Drop);
                }
                St::Do { rhs, line, .. } => {
                    let got = self.core_rhs_ty(body, rhs, *line)?;
                    self.core_rhs(m, b, body, w, rhs, &got, *line)?;
                    if self.cx.repr(&got, *line)? != Repr::Unit {
                        b.ins(&Instruction::Drop);
                    }
                }
            }
        }
        if let Some(j) = last {
            core_row_done(b, w, &ss[j], mark);
            self.core_give_back(b, w, &mut due, &ends[j]);
        }
        Ok(())
    }

    /// The slot [`Fn_::place_for`] gives the core's name `n`, kept with the
    /// mark before it until [`Fn_::core_give_back`] hands it back.
    fn core_slot(
        &mut self,
        b: &mut Frame,
        w: &mut Walked,
        n: vyrn_lower::core::Name,
        r: &Repr,
        line: usize,
    ) -> Result<u32, String> {
        let from = b.mark();
        let Place::Slot(off) = self.place_for(b, r, line)? else {
            return unsupported("a layout with no slot", line);
        };
        w.slot[n as usize] = Some((from, b.mark()));
        Ok(off)
    }

    /// The ownership word of the accumulator `n`, in local `l`: a slot held
    /// for `n`'s extent, as [`Fn_::core_slot`] holds a layout's.
    fn core_word(
        &mut self,
        b: &mut Frame,
        w: &mut Walked,
        n: vyrn_lower::core::Name,
        l: u32,
        site: usize,
        literal: bool,
    ) {
        let from = b.mark();
        let at = b.alloc(4, 4);
        w.slot[n as usize] = Some((from, b.mark()));
        self.str_append_shadow(b, l, at, site, literal);
    }

    /// Give back the slots of the names whose extent ended at the row just
    /// walked ([`vyrn_lower::core::extent_ends`]). A release deferred to the
    /// exit after the row still reads its slot, so `due` waits until
    /// [`Fn_::core_rows`] is empty.
    fn core_give_back(
        &mut self,
        b: &mut Frame,
        w: &mut Walked,
        due: &mut Vec<vyrn_lower::core::Name>,
        ended: &[vyrn_lower::core::Name],
    ) {
        due.extend_from_slice(ended);
        if self.core_rows.is_empty() {
            for n in due.drain(..) {
                if let Some((from, to)) = w.slot[n as usize].take() {
                    b.give_back(from, to);
                }
            }
        }
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
                let got = self.core_prim(m, b, body, w, op, vs, line)?;
                let got = ret.clone().unwrap_or(got);
                self.coerce(m, b, None, &got, want, line)
            }
            Rhs::Call {
                callee,
                args,
                kind,
                solved,
                targets,
                ret,
                ..
            } => {
                let got = self.core_call(
                    m,
                    b,
                    body,
                    w,
                    callee,
                    *kind,
                    solved,
                    targets,
                    args,
                    ret.as_ref(),
                    None,
                    line,
                )?;
                self.coerce(m, b, None, &got, want, line)
            }
            // A read and a take load the same address; what parts them is the
            // hole a take leaves, and the release that walks around it is the
            // driver's row (RFC-0125 M7, the layout-read family).
            Rhs::Read(p) | Rhs::Take(p) => {
                let got = self.core_read(m, b, body, w, p, line)?;
                self.coerce(m, b, None, &got, want, line)
            }
            _ => unsupported("a core right-hand side this walk does not read", line),
        }
    }

    /// What a right-hand side the driver reads PRODUCES, without emitting it —
    /// which a `St::Do` needs and a `St::Let` does not, because a `let` has a
    /// name and a name has the checker's type on it.
    fn core_rhs_ty(
        &self,
        body: &vyrn_lower::core::Body,
        rhs: &Rhs,
        line: usize,
    ) -> Result<Type, String> {
        match rhs {
            Rhs::Call {
                callee,
                kind,
                args,
                ret: at,
                solved,
                targets,
                ..
            } => match (
                builtin_spec(callee, args.len()),
                core_builtin(callee, *kind),
            ) {
                (Some((_, _, ret)), _) | (None, Some(Spec::Renders(ret) | Spec::Effect(ret))) => {
                    Ok(ret.clone())
                }
                (None, Some(Spec::Traps)) => Ok(Type::Never),
                (None, Some(Spec::Asserts)) => Ok(Type::Unit),
                (None, Some(Spec::Logs)) if callee == "logger" => Ok(Type::Logger),
                (None, Some(Spec::Logs)) => Ok(Type::Unit),
                // [`Fn_::lanes`] decides a lane builtin's type as it emits, and
                // the row carries the checker's answer for the site.
                (None, Some(Spec::Lanes)) => at
                    .clone()
                    .ok_or_else(|| gap("a lane builtin the checker did not type", line)),
                // What a removal hands back is the element, or an `Option` of
                // it, at the type the row states.
                (None, Some(Spec::Removes)) => at
                    .clone()
                    .ok_or_else(|| gap("a removal the checker did not type", line)),
                (None, Some(Spec::Finds)) => Ok(Type::Bool),
                // A generator host import answers at the type the checker
                // gave the site.
                (None, Some(Spec::Host)) => at
                    .clone()
                    .ok_or_else(|| gap("a host import the checker did not type", line)),
                (None, _) => match self.core_mem_ty(callee, args.len()) {
                    Some(t) => Ok(t),
                    None => match self.core_sig(body, callee, *kind, solved, targets) {
                        Some(s) => Ok(s.ret_ty),
                        None if *kind == Callee::Fn && self.is_extern(callee) => Ok(self
                            .cx
                            .externs
                            .get(callee)
                            .map_or(Type::Unit, |e| e.ret.clone())),
                        None => unsupported("a core call this walk does not read", line),
                    },
                },
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
    /// ([`Callee`]), so this walk has no ladder of its own to decide it.
    fn core_call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        callee: &str,
        kind: Callee,
        solved: &[(String, Type)],
        targets: &[Target],
        args: &[(Arg, vyrn_frontend::ast::Capability)],
        ret: Option<&Type>,
        hint: Option<(Dest, Type)>,
        line: usize,
    ) -> Result<Type, String> {
        if let Some(vs) = arg_vals(args) {
            return self.core_call_vals(
                m, b, body, w, callee, kind, solved, targets, &vs, ret, hint, line,
            );
        }
        if self.core_user_callee(callee, kind) {
            return self.core_user_call(
                m, b, body, w, callee, kind, solved, targets, args, hint, line,
            );
        }
        // A place receiver is shrunk where it lies, which is the move-out
        // window's whole extent ([`Fn_::core_removes`]).
        let ([(Arg::Place(p), _), rest @ ..], Some(Spec::Removes)) =
            (args, core_builtin(callee, kind))
        else {
            return unsupported("a place argument to a builtin other than a removal", line);
        };
        let Some(rest) = arg_vals(rest) else {
            return unsupported("a removal with two places", line);
        };
        let (aty, off) = self.core_addr(m, b, body, w, p, line)?;
        self.core_step(b, off);
        let slot = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(slot));
        self.core_remove(m, b, body, w, callee, slot, &aty, &rest, line)
    }

    /// [`Fn_::core_call`] with every argument a value.
    fn core_call_vals(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        callee: &str,
        kind: Callee,
        solved: &[(String, Type)],
        targets: &[Target],
        args: &[(Val, vyrn_frontend::ast::Capability)],
        ret: Option<&Type>,
        hint: Option<(Dest, Type)>,
        line: usize,
    ) -> Result<Type, String> {
        // PLAN-0125-runtime §2.1: a `std/mem` primitive is one instruction and
        // never a call, so no signature answers for it. The table is
        // [`Fn_::mem_spec`]'s, which the arm over the source reads too.
        if let Some(prim) = callee.strip_prefix(vyrn_frontend::loader::MEM_PREFIX) {
            return self.core_mem(m, b, body, w, prim, args, line);
        }
        // RFC-0125 M7, the builtin family: a builtin with a specification row
        // is emitted from that row. Every kind of row is answered here, so a
        // kind added to [`Spec`] is a compile error until it is.
        match core_builtin(callee, kind) {
            Some(Spec::Typed(..)) => {
                let Some((params, ins, ret)) = builtin_spec(callee, args.len()) else {
                    return unsupported("a specified builtin at another arity", line);
                };
                for ((v, _), p) in args.iter().zip(params) {
                    self.core_val(m, b, body, w, v, p, line)?;
                }
                b.ins(&ins);
                return Ok(ret.clone());
            }
            // `x.copy()` (RFC-0089 M1b): the operand at its own type, which
            // the row put on the name, and then the duplication the arm over
            // the source makes from the type `peek` answers with. A type that
            // declares `impl Copy for T` never reaches here: the core states
            // that call as the declaration's.
            Some(Spec::OwnType) => {
                let [(v, _)] = args else {
                    return unsupported("`copy` of other than one value", line);
                };
                let ty = self.core_ty(body, v, &Type::Int);
                self.core_val(m, b, body, w, v, &ty, line)?;
                self.copy_stack(m, b, &ty, line)?;
                return Ok(ty);
            }
            // `print(x)` and `x.toString()`: the operand at its own type, and
            // the rendering that type chooses, which the arm over the source
            // calls too.
            Some(Spec::Renders(ret)) => {
                let [(v, _)] = args else {
                    return unsupported("a rendering of other than one value", line);
                };
                let ty = self.core_ty(body, v, &Type::Int);
                self.core_val(m, b, body, w, v, &ty, line)?;
                match ret {
                    Type::Unit => self.print_value(b, &ty, line)?,
                    _ => self.str_value(b, &ty, None, line)?,
                }
                return Ok(ret.clone());
            }
            // `panic(msg)` and `@panicAt(msg, site)`: the line, and the call
            // that traps. The `unreachable` is the next row's.
            Some(Spec::Traps) => {
                let [(v, _), site @ ..] = args else {
                    return unsupported("`panic` with other than one argument", line);
                };
                let at = match site {
                    [(Val::Lit(Lit::Str(at)), _)] => Some(at.as_str()),
                    _ => None,
                };
                self.panic_line(m, b, at, |s, m, b| {
                    s.core_val(m, b, body, w, v, &Type::Str, line)
                })?;
                return Ok(Type::Never);
            }
            Some(Spec::Asserts) => {
                let mut operand =
                    |s: &mut Self, m: &mut Module, b: &mut Frame, i: usize, t: Option<&Type>| {
                        let Some((v, _)) = args.get(i) else {
                            return unsupported(&format!("`{callee}` with too few operands"), line);
                        };
                        let t = t.cloned().unwrap_or_else(|| s.core_ty(body, v, &Type::Int));
                        s.core_val(m, b, body, w, v, &t, line)?;
                        Ok(t)
                    };
                return self.asserts(m, b, callee, &mut operand, line);
            }
            // `xs.push(v)` and its siblings, and `m.tally(k, n)`: the
            // receiver's address, which the call rebuilds in place, and the
            // operands the row names.
            Some(Spec::Rebuilds) => {
                let [(Val::Name(x), _), rest @ ..] = args else {
                    return unsupported("a rebuild of no named receiver", line);
                };
                let aty = body.names[*x as usize].ty.clone();
                self.core_addr_of(b, w, body, *x, line)?;
                if let ("@tally" | "@tallyBytes", [_, _]) = (callee, rest) {
                    let hdr = b.local(ValType::I32);
                    b.ins(&Instruction::LocalSet(hdr));
                    let mut operand =
                        |s: &mut Self, m: &mut Module, b: &mut Frame, i: usize, t: &Type| {
                            s.core_val(m, b, body, w, &rest[i].0, t, line)
                        };
                    return match callee {
                        "@tally" => self.map_tally(m, b, hdr, &aty, &mut operand, line),
                        _ => self.map_tally_bytes(m, b, hdr, &aty, &mut operand, line),
                    };
                }
                let mut operand = |s: &mut Self, m: &mut Module, b: &mut Frame, t: &Type| match rest
                {
                    [(v, _)] => s.core_val(m, b, body, w, v, t, line),
                    _ => unsupported(&format!("`{callee}` with no operand"), line),
                };
                return self.arr_rebuild(m, b, callee, &aty, &mut operand, line);
            }
            // A SIMD builtin: each operand at the type asked for or its own, and
            // a lane index as the literal the row carries.
            Some(Spec::Lanes) => {
                let mut operand =
                    |s: &mut Self, m: &mut Module, b: &mut Frame, i: usize, want: Option<&Type>| {
                        let Some((v, _)) = args.get(i) else {
                            return unsupported(&format!("`{callee}` with too few operands"), line);
                        };
                        let t = match want {
                            Some(t) => t.clone(),
                            None => s.core_ty(body, v, &Type::Int),
                        };
                        s.core_val(m, b, body, w, v, &t, line)?;
                        Ok(t)
                    };
                let lane_at = |i: usize, lanes: i64| core_lane(args, i, lanes);
                return self.lanes(m, b, callee, args.len(), &mut operand, &lane_at, line);
            }
            // A generator host import: each operand at the type the import
            // takes, and `@codeSplice`'s tag from the type the row put on its
            // operand.
            Some(Spec::Host) => {
                let mut ty = |s: &mut Self, i: usize| match args.get(i) {
                    Some((v, _)) => Ok(s.cx.resolve(&s.core_ty(body, v, &Type::Int))),
                    None => unsupported(&format!("`{callee}` with too few operands"), line),
                };
                let mut operand =
                    |s: &mut Self, m: &mut Module, b: &mut Frame, i: usize, t: &Type| match args
                        .get(i)
                    {
                        Some((v, _)) => s.core_val(m, b, body, w, v, t, line),
                        None => unsupported(&format!("`{callee}` with too few operands"), line),
                    };
                return self.host(m, b, callee, args.len(), &mut ty, &mut operand, line);
            }
            // `xs.pop()`, `xs.swapRemove(i)` and `m.remove(k)`: the receiver's
            // address, which the call shrinks in place, and the operand the
            // row names.
            Some(Spec::Removes) => {
                let [(Val::Name(x), _), rest @ ..] = args else {
                    return unsupported("a removal from no named receiver", line);
                };
                let aty = body.names[*x as usize].ty.clone();
                let slot = b.local(ValType::I32);
                self.core_addr_of(b, w, body, *x, line)?;
                b.ins(&Instruction::LocalSet(slot));
                return self.core_remove(m, b, body, w, callee, slot, &aty, rest, line);
            }
            // `m.has(k)`: the map's address, and the scan [`Fn_::map_at`]
            // makes, answered as whether it found an entry.
            Some(Spec::Finds) => {
                let [(mv, _), (kv, _)] = args else {
                    return unsupported(&format!("`{callee}` at this arity"), line);
                };
                let mty = self.core_ty(body, mv, &Type::Int);
                self.core_val(m, b, body, w, mv, &mty, line)?;
                let hdr = b.local(ValType::I32);
                b.ins(&Instruction::LocalSet(hdr));
                let mut key = |s: &mut Self, m: &mut Module, b: &mut Frame, t: &Type| {
                    s.core_val(m, b, body, w, kv, t, line)
                };
                let (idx, ..) = self.map_find(m, b, hdr, &mty, &mut key, line)?;
                b.ins(&Instruction::LocalGet(idx));
                b.ins(&Instruction::I32Const(0));
                b.ins(&Instruction::I32GeS);
                return Ok(Type::Bool);
            }
            // `bytes` and `stringFromBytes`: built in a slot of the call's
            // own, which [`agg_landed`] copies into the destination.
            Some(Spec::Builds(_)) => {
                let mut operand =
                    |s: &mut Self, m: &mut Module, b: &mut Frame, i: usize, t: &Type| match args
                        .get(i)
                    {
                        Some((v, _)) => s.core_val(m, b, body, w, v, t, line),
                        None => unsupported(&format!("`{callee}` with too few operands"), line),
                    };
                return match (callee, args) {
                    ("bytes", _) => self.bytes_of(m, b, args.len() == 3, &mut operand, line),
                    ("stringFromBytes", _) => self.string_from_bytes(m, b, &mut operand, line),
                    ("@toArray", [(v, _)]) => {
                        let aty = self.core_ty(body, v, &Type::Int);
                        self.core_val(m, b, body, w, v, &aty, line)?;
                        let hdr = b.local(ValType::I32);
                        b.ins(&Instruction::LocalSet(hdr));
                        self.sa_to_array(m, b, hdr, &aty, line)
                    }
                    ("fromArray", [(v, _)]) => {
                        let aty = self.core_ty(body, v, &Type::Int);
                        let Type::Array(inner) = self.cx.resolve(&aty) else {
                            return unsupported(&format!("`fromArray` of `{aty}`"), line);
                        };
                        self.core_val(m, b, body, w, v, &aty, line)?;
                        self.stream_from_array(b, &inner, line)
                    }
                    ("fromStep", [_, _, _]) => {
                        let mut step = |s: &mut Self, m: &mut Module, b: &mut Frame, i: usize| {
                            let v = &args[i].0;
                            let t = match i {
                                0 | 1 => Type::Int,
                                _ => s.core_ty(body, v, &Type::Int),
                            };
                            s.core_val(m, b, body, w, v, &t, line)?;
                            Ok(t)
                        };
                        self.stream_from_step(m, b, &mut step, line)
                    }
                    // The element type is the row's result: nothing in the
                    // call carries it, because an address is an `Int64`.
                    ("unboxStream" | "pullAt", [(v, _)]) => {
                        let mut addr = |s: &mut Self, m: &mut Module, b: &mut Frame| {
                            s.core_val(m, b, body, w, v, &Type::Int, line)
                        };
                        let ret = ret.map(|t| self.cx.resolve(t));
                        match (callee, ret) {
                            ("unboxStream", Some(Type::Stream(elem))) => {
                                self.stream_unbox(m, b, &elem, &mut addr, line)
                            }
                            ("pullAt", Some(opt)) => match ftypes::option_payload(&opt) {
                                Some(elem) => self.stream_pull_at(m, b, elem, &mut addr, line),
                                None => unsupported("a `pullAt` of no Option", line),
                            },
                            _ => unsupported(&format!("`{callee}` with no result type"), line),
                        }
                    }
                    ("@keys", [(v, _)]) => {
                        let mty = self.core_ty(body, v, &Type::Int);
                        self.core_val(m, b, body, w, v, &mty, line)?;
                        let hdr = b.local(ValType::I32);
                        b.ins(&Instruction::LocalSet(hdr));
                        self.map_keys(m, b, hdr, &mty, line)
                    }
                    _ => self.slot_call(m, b, callee, &mut operand, line),
                };
            }
            Some(Spec::Effect(_)) => {
                let [(v, _)] = args else {
                    return unsupported(&format!("`{callee}` of other than one value"), line);
                };
                let mut operand =
                    |s: &mut Self, m: &mut Module, b: &mut Frame, want: Option<&Type>| {
                        let t = want
                            .cloned()
                            .unwrap_or_else(|| s.core_ty(body, v, &Type::Int));
                        s.core_val(m, b, body, w, v, &t, line)?;
                        Ok(t)
                    };
                return self.effect(m, b, callee, &mut operand, line);
            }
            Some(Spec::Logs) => {
                let mut operand =
                    |s: &mut Self, m: &mut Module, b: &mut Frame, i: usize, t: &Type| match args
                        .get(i)
                    {
                        Some((v, _)) => s.core_val(m, b, body, w, v, t, line),
                        None => unsupported(&format!("`{callee}` with too few operands"), line),
                    };
                return self.logs(m, b, callee, &mut operand, line);
            }
            // A pull binds two names, and [`Fn_::core_stmts`] emits it.
            Some(Spec::Pulls) => return unsupported("a pull apart from its loop head", line),
            // A routed builtin is the declared call below, through the
            // signature [`Fn_::core_sig`] answers for the function it names.
            Some(Spec::Routes(_)) | None => {}
        }
        // `T(v)` of a validated type: the operand at the base, then the check
        // (RFC-0125 §2.2), which a crossing the checker proved does not run.
        if let Some(decl) = self.core_named(callee, kind) {
            let [(v, _)] = args else {
                return unsupported(&format!("`{callee}` at this arity"), line);
            };
            self.core_val(m, b, body, w, v, &decl.base, line)?;
            if kind == Callee::Named {
                self.emit_validation(b, &decl, line)?;
            }
            return Ok(Type::Named(decl.name));
        }
        if kind == Callee::Fn && self.is_extern(callee) {
            let mut operand = |s: &mut Self, m: &mut Module, b: &mut Frame, i: usize, p: &Type| {
                s.core_val(m, b, body, w, &args[i].0, p, line)
            };
            return self.extern_call(m, b, callee, args.len(), &mut operand, line);
        }
        let args: Vec<_> = args
            .iter()
            .map(|(v, c)| (Arg::Val(v.clone()), *c))
            .collect();
        self.core_user_call(
            m, b, body, w, callee, kind, solved, targets, &args, hint, line,
        )
    }

    /// A call to a declared function, whose arguments are values or the
    /// places `modify` parameters write ([`Arg`]): a place crosses as its
    /// address, as a layout name does.
    #[allow(clippy::too_many_arguments)]
    fn core_user_call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        callee: &str,
        kind: Callee,
        solved: &[(String, Type)],
        targets: &[Target],
        args: &[(Arg, vyrn_frontend::ast::Capability)],
        hint: Option<(Dest, Type)>,
        line: usize,
    ) -> Result<Type, String> {
        // A call through a stored value (RFC-0037) is one call to the
        // signature's dispatcher, with the value as its leading argument, as
        // [`Fn_::fnval_call`] makes it.
        let through: Vec<(Arg, vyrn_frontend::ast::Capability)>;
        let mut spliced = Vec::new();
        let (sig, args) = match self.core_through(body, callee, kind) {
            Some((n, sig_ty)) => {
                through =
                    std::iter::once((Arg::Val(Val::Name(n)), vyrn_frontend::ast::Capability::Read))
                        .chain(args.iter().cloned())
                        .collect();
                (self.dispatcher(m, &sig_ty, line)?, &through[..])
            }
            None => {
                // A stored value's dispatcher is registered before the
                // instance is named, so the target carries its index.
                if let Some((.., bound)) = self.core_ho(callee, kind, solved, targets) {
                    for (t, ft) in targets.iter().zip(&bound) {
                        if matches!(t, Target::Value(_)) {
                            self.dispatcher(m, &ft.sig.params[0], line)?;
                        }
                    }
                }
                let ho = match targets {
                    [] => None,
                    _ => self.core_ho(callee, kind, solved, targets),
                };
                let sig = match (ho, self.core_instance(callee, kind, solved)) {
                    (Some((f, targs, subst, bound)), _) => {
                        let Some(ordered) = ho_args(f, targets, args) else {
                            return unsupported("a core call this walk does not read", line);
                        };
                        spliced = ordered;
                        self.cx.specialize(m, f, targs, subst, bound)?
                    }
                    (None, Some((f, targs, subst))) => self.cx.instantiate(m, f, targs, subst)?,
                    (None, None) => match self.core_sig(body, callee, kind, solved, targets) {
                        Some(sig) => sig,
                        None if self.cx.skipped.contains(callee) => {
                            return unsupported(&format!("the call `{callee}`"), line);
                        }
                        None => return unsupported("a core call this walk does not read", line),
                    },
                };
                (
                    sig,
                    if spliced.is_empty() {
                        args
                    } else {
                        &spliced[..]
                    },
                )
            }
        };
        let dest = self.out_ptr(b, &sig, hint);
        let mut spilled = Vec::new();
        for ((a, c), p) in args.iter().zip(&sig.params) {
            match (a, c) {
                (Arg::Val(Val::Name(n)), vyrn_frontend::ast::Capability::Modify)
                    if !matches!(self.cx.repr(p, line)?, Repr::Agg(_)) =>
                {
                    let Some((Place::Local(l), ty)) = self.core_place(w, body, *n) else {
                        return unsupported("a `modify` argument with no local", line);
                    };
                    spilled.push(self.spill(b, l, &ty, line)?);
                }
                (Arg::Val(v), _) => self.core_val(m, b, body, w, v, p, line)?,
                (Arg::Place(pl), _) => {
                    let (_, off) = self.core_addr(m, b, body, w, pl, line)?;
                    self.core_step(b, off);
                }
            }
        }
        b.ins(&Instruction::Call(sig.index));
        reload(b, &spilled);
        self.out_ptr_back(b, dest);
        Ok(sig.ret_ty)
    }

    /// One read of a place, the address computed from the row — RFC-0125 M7,
    /// the layout-read family.
    ///
    /// §2.1 states the rule: a place is an address, a read of it is one scalar
    /// load, and a place is never copied to read a field of it.
    /// [`Fn_::core_addr`] is the address, stated once over
    /// [`vyrn_lower::core::Place`]; what this adds is the load the value's own
    /// type asks for, with the offset folded into the access.
    ///
    /// A take reads the same address. The hole it leaves in the base, and the
    /// release that walks around it, are rows the driver places, so a body
    /// holding one stands down at [`Fn_::core_walkable`]'s placement clause.
    fn core_read(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        p: &vyrn_lower::core::Place,
        line: usize,
    ) -> Result<Type, String> {
        // A scalar name lives in a wasm local, which has no address at all, so
        // the read IS the local — the one place kind [`Fn_::core_addr`] cannot
        // answer for.
        if let vyrn_lower::core::Place::Name(n) = p {
            let ty = body.names[*n as usize].ty.clone();
            self.core_val(m, b, body, w, &Val::Name(*n), &ty, line)?;
            return Ok(ty);
        }
        // A length is a header read and no field ([`Fn_::length_of`]). What
        // it reads is the base's value: an address for a layout, the pointer
        // for a String.
        if let vyrn_lower::core::Place::Field(base, f) = p {
            if let Some(walk) = core_header(w, base).filter(|_| f == "length" || f == "byteLength")
            {
                b.ins(&Instruction::LocalGet(walk.len));
                return Ok(Type::Int);
            }
            if let Some(bty) = self
                .core_place_ty(body, base)
                .filter(|t| length_ty(f, &self.cx.resolve(t)).is_some())
            {
                if let Repr::Agg(_) = self.cx.repr(&bty, line)? {
                    let (_, off) = self.core_addr(m, b, body, w, base, line)?;
                    self.core_step(b, off);
                } else {
                    self.core_read(m, b, body, w, base, line)?;
                }
                return self
                    .length_of(b, &bty, f, line)?
                    .ok_or_else(|| gap("a length of no container", line));
            }
        }
        let (ty, off) = self.core_addr(m, b, body, w, p, line)?;
        let Repr::Scalar(_) = self.cx.repr(&ty, line)? else {
            return unsupported("a read of a place this walk does not load", line);
        };
        b.ins(&load_of(
            &self.cx.ll(&ty),
            off.unwrap_or(0),
            self.cx.signed(&ty),
        ));
        Ok(ty)
    }

    /// The address of a place, and the offset the load still owes it —
    /// RFC-0125 M7, §2.1's address arithmetic written once.
    ///
    /// `Some(off)` is a FIELD step: the address on the stack is the record's
    /// and the field is `off` bytes into it, which a scalar load folds into its
    /// own access and an aggregate step adds. `None` is an address that is
    /// already the place's. The distinction is what keeps the bytes the
    /// `Expr::Field` arm's: it loads at the offset rather than adding it.
    /// A removal from the receiver of type `aty` whose address is in `slot`.
    fn core_remove(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        callee: &str,
        slot: u32,
        aty: &Type,
        rest: &[(Val, vyrn_frontend::ast::Capability)],
        line: usize,
    ) -> Result<Type, String> {
        match (callee, rest) {
            ("@pop", []) => self.pop_at(b, slot, aty, line),
            ("@swapRemove", [(i, _)]) => {
                let mut index = |s: &mut Self, m: &mut Module, b: &mut Frame| {
                    s.core_val(m, b, body, w, i, &Type::Int, line)
                };
                self.swap_remove_at(m, b, slot, aty, &mut index, line)
            }
            ("@remove", [(k, _)]) => {
                let mut key = |s: &mut Self, m: &mut Module, b: &mut Frame, t: &Type| {
                    s.core_val(m, b, body, w, k, t, line)
                };
                self.map_remove(m, b, slot, aty, &mut key, line)
            }
            _ => unsupported(&format!("`{callee}` at this arity"), line),
        }
    }

    fn core_addr(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        p: &vyrn_lower::core::Place,
        line: usize,
    ) -> Result<(Type, Option<u32>), String> {
        use vyrn_lower::core::Place as At;
        match p {
            At::Name(n) => {
                let Some((place, ty)) = self.core_place(w, body, *n) else {
                    return unsupported("a core place with no storage", line);
                };
                match place {
                    // An aggregate in a local is the ADDRESS of one, which is
                    // what the `Expr::Var` arm pushes for it.
                    Place::Local(l) => {
                        b.ins(&Instruction::LocalGet(l));
                    }
                    other => {
                        if other.addr(b, 0).is_none() {
                            return unsupported("the address of a scalar local", line);
                        }
                    }
                }
                Ok((ty, None))
            }
            // Module state is storage at a fixed address, which is the one way
            // a global differs from a local here (RFC-0013).
            At::Global(name) => {
                let (place, ty) = self.lookup(name, line)?;
                let Place::Static(at) = place else {
                    return unsupported("module state that is not static", line);
                };
                b.ins(&Instruction::I32Const(at as i32));
                Ok((ty, None))
            }
            At::Field(base, f) => {
                let (bty, off) = self.core_addr(m, b, body, w, base, line)?;
                self.core_step(b, off);
                let (at, fty) = self.field_of(&bty, f, line)?;
                Ok((thunk_of(fty), Some(at)))
            }
            // The element's address is the walk's: the header read, the bounds
            // check the language states, and one stride multiply — the same
            // three [`Fn_::at`] emits for `a[i]`. A String is walked from its
            // pointer, which is its value and not its address, and its element
            // is a `UInt8` where the walk hands a `for` the byte widened.
            At::Elem(base, i) => {
                let walk = match core_header(w, base) {
                    Some(walk) => walk,
                    None => {
                        let bty = match self.core_place_ty(body, base) {
                            Some(t) if self.cx.resolve(&t) == Type::Str => {
                                self.core_read(m, b, body, w, base, line)?
                            }
                            _ => {
                                let (bty, off) = self.core_addr(m, b, body, w, base, line)?;
                                self.core_step(b, off);
                                bty
                            }
                        };
                        self.walk(b, &bty, line)?
                    }
                };
                self.core_val(m, b, body, w, i, &Type::Int, line)?;
                let ix = b.local(ValType::I64);
                b.ins(&Instruction::LocalSet(ix));
                self.bounds_check(b, &walk, ix, walk.byte);
                self.elem_addr(b, &walk, ix);
                let byte = Type::IntN {
                    bits: 8,
                    signed: false,
                };
                Ok((if walk.byte { byte } else { walk.elem }, None))
            }
            // A key has no address: its read is the runtime's lookup, which
            // the aggregate `let` arm of [`Fn_::core_stmts`] writes.
            At::Key(..) => unsupported("a read of a key", line),
        }
    }

    /// Fold a field step's offset into the address on the stack, which is what
    /// an aggregate field costs and a scalar one does not.
    fn core_step(&self, b: &mut Frame, off: Option<u32>) {
        if let Some(off) = off {
            b.ins(&Instruction::I32Const(off as i32));
            b.ins(&Instruction::I32Add);
        }
    }

    /// The type of a place, without emitting it — the layout-read family's
    /// screen, asking exactly what [`Fn_::core_addr`] walks.
    fn core_place_ty(
        &self,
        body: &vyrn_lower::core::Body,
        p: &vyrn_lower::core::Place,
    ) -> Option<Type> {
        use vyrn_lower::core::Place as At;
        match p {
            At::Name(n) => Some(body.names[*n as usize].ty.clone()),
            At::Global(name) => {
                let (place, ty) = self.lookup(name, 0).ok()?;
                matches!(place, Place::Static(_)).then_some(ty)
            }
            At::Field(base, f) => {
                let bty = self.core_place_ty(body, base)?;
                if let Some(t) = length_ty(f, &self.cx.resolve(&bty)) {
                    return Some(t);
                }
                Some(thunk_of(self.field_of(&bty, f, 0).ok()?.1))
            }
            At::Elem(base, _) => match self.cx.resolve(&self.core_place_ty(body, base)?) {
                Type::Array(e) | Type::ArrayN(e, _) | Type::SmallArray(e, _) => Some(*e),
                Type::Str => Some(Type::IntN {
                    bits: 8,
                    signed: false,
                }),
                _ => None,
            },
            At::Key(..) => None,
        }
    }

    /// Whether `n` is a header a loop walks: a borrow of an array, a small
    /// array or a String, which a `for` binds its container to and a `while`
    /// binds a container it indexes and never rebuilds.
    fn core_walked(&self, body: &vyrn_lower::core::Body, n: vyrn_lower::core::Name) -> bool {
        let info = &body.names[n as usize];
        info.walked.is_some()
            && info.borrow
            && matches!(
                self.cx.resolve(&info.ty),
                Type::Array(_) | Type::SmallArray(..) | Type::Str
            )
    }

    /// The place a layout name holds the ADDRESS of — RFC-0125 M7, a name read
    /// from a layout.
    ///
    /// §2.1: a place is never copied to read it, so a borrow bound by a read of
    /// a layout is that place for the read's extent. The kernel ends the alias
    /// at every store, take and drop of the place, and refuses a read after
    /// one.
    ///
    /// A layout that owns no heap is a borrow only where the core minted the
    /// name, a `for` head's or a scrutinee's, which the arm reads by address.
    /// A `let` the reader wrote binds a value ([`Fn_::core_copies`]).
    ///
    /// An owned name read out of an element is the element itself: a `for`
    /// over a container it alone owns hands each element on through the
    /// variable, and the container's release frees the buffer alone
    /// ([`Fn_::rel_owed`]). The name's release is its own row, and a
    /// `consume` of it hands the element on.
    ///
    /// `None` where the program can observe the copy. A binding the body
    /// stores into, or hands to `modify`, writes a value of its own; a store
    /// into an element of an Array writes the buffer ([`core_written`]). A root on
    /// the chain handed to `consume` anywhere in the body may be freed under
    /// the name. A row of the name's extent that hands a root on the chain to
    /// `modify` ([`vyrn_lower::kernel::modifies`]), or rebuilds it as a
    /// write-back receiver (`out.push(v)`), may replace what the name points
    /// into. Outside the extent the kernel ends the alias at that call
    /// and refuses a read after it, as it does at a store into the root; for
    /// module state, a callee's store too.
    fn core_alias<'b>(
        &self,
        body: &'b vyrn_lower::core::Body,
        n: vyrn_lower::core::Name,
    ) -> Option<&'b vyrn_lower::core::Place> {
        let info = &body.names[n as usize];
        if self.checks(&info.ty) || !matches!(self.cx.repr(&info.ty, 0), Ok(Repr::Agg(_))) {
            return None;
        }
        // A take a rebuild hands back is the place it was taken from: the
        // arm rebuilds a field in place (`s.keys.push(k)`), and the hole the
        // take leaves is the rebuild's own until the store fills it. So is a
        // take the next row moves on ([`core_moves_on`]).
        if let Some(p) = core_taken(body, n) {
            return (self.core_hands_back(body, &body.stmts, n) || core_moves_on(body, n))
                .then_some(p);
        }
        let minted = info.source.starts_with('@') && !info.heap && !self.owns_heap(&info.ty);
        let owned = info.releases && !info.borrow;
        if !(info.borrow || minted || owned) {
            return None;
        }
        let mut lets = Vec::new();
        let mut written = Vec::new();
        for s in &body.stmts {
            core_lets(s, &mut lets);
            core_written(&body.names, s, &mut written);
        }
        let read = |m: vyrn_lower::core::Name| {
            let mut at = lets.iter().filter(|(b, _)| *b == m);
            match (at.next(), at.next()) {
                (Some((_, Rhs::Read(p))), None) => Some(p),
                _ => None,
            }
        };
        let place = read(n)?;
        let extent = core_extent(&body.stmts, n, &body.occurrences())?;
        let mut rebuilt = Vec::new();
        extent
            .iter()
            .for_each(|s| core_written(&body.names, s, &mut rebuilt));
        if written
            .iter()
            .any(|(m, c)| *m == n && !(owned && *c == Some(Capability::Consume)))
            || matches!(place, vyrn_lower::core::Place::Key(..))
            || (owned && !matches!(place, vyrn_lower::core::Place::Elem(..)))
            || self
                .core_place_ty(body, place)
                .is_none_or(|t| self.cx.resolve(&t) != self.cx.resolve(&info.ty))
        {
            return None;
        }
        // Each name on the chain is bound once, before the name it reads, so
        // the walk ends within `body.names.len()` steps.
        let mut on = place;
        for _ in 0..body.names.len() {
            let Some((root, _)) = vyrn_lower::kernel::root_of(on) else {
                return Some(place);
            };
            if written
                .iter()
                .any(|(m, c)| *m == root && *c == Some(Capability::Consume))
                || rebuilt
                    .iter()
                    .any(|(m, c)| *m == root && *c == Some(Capability::Modify))
                || vyrn_lower::kernel::modifies(
                    extent,
                    vyrn_lower::kernel::Root::N(root),
                    &body.names,
                    &body.name,
                )
            {
                return None;
            }
            match read(root) {
                Some(p) => on = p,
                None => return Some(place),
            }
        }
        None
    }

    /// The place the layout name `n` holds a COPY of — RFC-0125 M7, a layout
    /// that owns no heap, a take, and a move that is no rename.
    ///
    /// A layout that owns no heap is a value, and the kernel lets its place be
    /// written while the name lives ([`Fn_::core_alias`]). A take leaves a hole
    /// in its place, and a store may fill the hole while the name lives; a
    /// move leaves the whole place empty, which a store may fill. So the name
    /// takes a slot and the bytes.
    fn core_copies(
        &self,
        body: &vyrn_lower::core::Body,
        n: vyrn_lower::core::Name,
    ) -> Option<vyrn_lower::core::Place> {
        let info = &body.names[n as usize];
        if self.checks(&info.ty) || !matches!(self.cx.repr(&info.ty, 0), Ok(Repr::Agg(_))) {
            return None;
        }
        // A name this pass minted, a `for` head's borrow or a scrutinee, is
        // read by address in the arm. A `let` of the source copies, and that
        // includes one a projection's prologue writes (`let @p0.h = h`).
        let value = (info.bound_by_let || !info.source.starts_with('@'))
            && !info.heap
            && !self.owns_heap(&info.ty);
        let mut lets = Vec::new();
        for s in &body.stmts {
            core_lets(s, &mut lets);
        }
        let mut at = lets.iter().filter(|(b, _)| *b == n);
        let p = match (at.next(), at.next()) {
            (Some((_, Rhs::Take(p))), None) => p.clone(),
            (Some((_, Rhs::Read(p))), None) if value => p.clone(),
            // A move [`Fn_::core_renames`] refuses takes the bytes, as a take
            // does: the kernel refuses a read of `x` before its next store.
            (Some((_, Rhs::Val(Val::Name(x)))), None)
                if value || (info.heap && !info.borrow && !body.names[*x as usize].borrow) =>
            {
                vyrn_lower::core::Place::Name(*x)
            }
            _ => return None,
        };
        (!matches!(p, vyrn_lower::core::Place::Key(..))
            && self
                .core_place_ty(body, &p)
                .is_some_and(|t| self.cx.resolve(&t) == self.cx.resolve(&info.ty)))
        .then_some(p)
    }

    /// The name whose place the layout name `n` takes over — RFC-0125 M7, a
    /// move is a rename.
    ///
    /// `let y = x` of an owned layout that owns heap moves `x`, and the kernel
    /// refuses a read of `x` after it. So `y` is `x`'s place, with no slot and
    /// no copy of its own, and the release the driver placed for the value is
    /// `y`'s. `None` where the body stores into `x` after the move
    /// ([`core_after`]), because such a store writes the storage `y` holds. A
    /// store before it is part of the value that moves.
    fn core_renames(
        &self,
        body: &vyrn_lower::core::Body,
        n: vyrn_lower::core::Name,
    ) -> Option<vyrn_lower::core::Name> {
        let info = &body.names[n as usize];
        if self.checks(&info.ty) || !matches!(self.cx.repr(&info.ty, 0), Ok(Repr::Agg(_))) {
            return None;
        }
        let mut lets = Vec::new();
        let mut written = Vec::new();
        for s in &body.stmts {
            core_lets(s, &mut lets);
        }
        match core_after(&body.stmts, n) {
            Some(after) => after
                .into_iter()
                .for_each(|s| core_written(&body.names, s, &mut written)),
            None => (body.stmts.iter()).for_each(|s| core_written(&body.names, s, &mut written)),
        }
        let mut at = lets.iter().filter(|(b, _)| *b == n);
        let (Some((_, Rhs::Val(Val::Name(x)))), None) = (at.next(), at.next()) else {
            return None;
        };
        let from = &body.names[*x as usize];
        let unwritten = |m: vyrn_lower::core::Name| !written.iter().any(|(w, _)| *w == m);
        // A join's stores are its branches', which run before the rename. A
        // borrow takes over a join's place, which holds the bytes the borrow
        // reads, and neither name releases them; or a borrowed parameter's.
        // A borrowed layout parameter holds the caller's address for the
        // whole body, so a second name for it is that address while neither
        // name is written: the temporary a scrutinee binds (a declared
        // release's `match consume self`, [`vyrn_lower::core`]'s
        // `owns_boxes`), or a reader's `let data = d`.
        let joins = self.core_joins(body, *x);
        let param = from.borrow
            && body.params.contains(x)
            && (info.source.starts_with('@') || info.borrow)
            && unwritten(n);
        ((joins || param || (!info.borrow && self.owns_heap(&info.ty) && !from.borrow))
            && self.cx.resolve(&from.ty) == self.cx.resolve(&info.ty)
            && (joins || unwritten(*x)))
        .then_some(*x)
    }

    /// Whether `n` is the temporary a layout `if` or `match` expression joins
    /// through (RFC-0030): a name the naming pass minted, that no `let` binds
    /// and each branch stores whole. Its place is a slot the first store takes
    /// ([`Fn_::core_slot`]), and the `let` that renames it holds the slot to
    /// the end of its own extent ([`Fn_::core_renames`]).
    fn core_joins(&self, body: &vyrn_lower::core::Body, n: vyrn_lower::core::Name) -> bool {
        let info = &body.names[n as usize];
        if !info.source.starts_with('@')
            || body.params.contains(&n)
            || self.checks(&info.ty)
            || !matches!(self.cx.repr(&info.ty, 0), Ok(Repr::Agg(_)))
        {
            return false;
        }
        let (mut lets, mut binders, mut written) = (Vec::new(), Vec::new(), Vec::new());
        for s in &body.stmts {
            core_lets(s, &mut lets);
            core_switched(s, true, &mut binders);
            core_written(&body.names, s, &mut written);
        }
        let mut whole = 0;
        each_list(&body.stmts, &mut |ss| {
            whole += ss
                .iter()
                .filter(|r| {
                    matches!(r, St::Store { place: vyrn_lower::core::Place::Name(m), .. } if *m == n)
                })
                .count();
        });
        whole > 0
            && !lets.iter().any(|(b, _)| *b == n)
            && !binders.contains(&n)
            && written.iter().filter(|(m, _)| *m == n).count() == whole
    }

    /// Whether [`Fn_::core_switch`] gives a payload binder of `ty` a place —
    /// RFC-0125 M7, a payload binder that is a layout.
    ///
    /// A value in one wasm local is loaded into one. A layout the row reads
    /// out of the scrutinee holds its address there, and the kernel refuses a
    /// write to the scrutinee while the binder lives; any other layout moves
    /// out, as the arm moves it.
    fn core_payload(&self, ty: &Type) -> bool {
        self.core_framed(ty)
            || (matches!(self.cx.repr(ty, 0), Ok(Repr::Agg(_))) && !self.checks(ty))
    }

    /// One made layout, built into the binding's own slot — RFC-0125 M7, the
    /// layout-made family.
    ///
    /// The bytes are the destination's address, the parts at the offsets the layout gives them,
    /// the address again, and the two drops that stand for the copy an in-place
    /// build does not make. The placement itself is [`Fn_::record_into`]'s,
    /// [`Fn_::array_lit_heap`]'s and [`Fn_::build_variant`]'s, which the AST arm
    /// calls with the same destination.
    #[allow(clippy::too_many_arguments)]
    fn core_make(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        dest: Dest,
        ty: &Type,
        rhs: &Rhs,
        taken: Option<u32>,
        line: usize,
    ) -> Result<(), String> {
        if let Rhs::Make(Ctor::Closure(t), vs) = rhs {
            let Some((sig_ty, target)) = self.core_closure(body, ty, t, vs) else {
                return unsupported("a function value this walk does not make", line);
            };
            return self.fnval_into(
                m,
                b,
                dest,
                &sig_ty,
                target,
                &mut Parts::Core(body, vs, w),
                line,
            );
        }
        // A lambda: the literal lifted as the arm lifts it, and its captures
        // read off the row, which lists them in the lifted signature's order.
        if let Rhs::Prim(Op::Closure(key), vs, _) = rhs {
            let sig_ty = crate::normalize_fn_sig(&self.cx.sub(ty), &self.cx.types);
            let Some(at) = self.literal(key) else {
                return unsupported("a lambda this walk does not find", line);
            };
            let (target, _) = self.lift_stored(m, at, &sig_ty)?;
            return self.fnval_into(
                m,
                b,
                dest,
                &sig_ty,
                target,
                &mut Parts::Core(body, vs, w),
                line,
            );
        }
        dest.addr(b, 0);
        match rhs {
            // A variant: its tag, then its payload in the slots the sum gives
            // it. The hint is this walk's destination, which the builder takes
            // because the type it is building is the one the slot holds.
            Rhs::Call { args, .. } => {
                let Some((tag, payload)) = self
                    .core_ctor_name(body, rhs)
                    .and_then(|v| self.core_variant(ty, v))
                else {
                    return unsupported("a variant the row states of no sum", line);
                };
                let Some(vs) = arg_vals(args) else {
                    return unsupported("a variant built from a place", line);
                };
                let vs: Vec<Val> = vs.into_iter().map(|(v, _)| v).collect();
                let mut parts = Parts::Core(body, &vs, w);
                let hint = Some((dest, ty.clone()));
                self.build_variant(m, b, ty, tag, &mut parts, &payload, line, hint)?;
            }
            Rhs::Make(Ctor::Record(_, names), vs) => {
                let decl = self
                    .cx
                    .fields(ty)
                    .ok_or_else(|| gap("a record literal the row states", line))?;
                let Repr::Agg(l) = self.cx.repr(ty, line)? else {
                    return unsupported("a record literal the row states", line);
                };
                let mut order = Vec::new();
                for f in &decl {
                    let at = names
                        .iter()
                        .position(|n| *n == f.name)
                        .ok_or_else(|| gap(&format!("the missing field `{}`", f.name), line))?;
                    order.push(at);
                }
                let mut parts = Parts::Core(body, vs, w);
                self.record_into(m, b, dest, &decl, &l, &order, &mut parts, line)?;
            }
            Rhs::Make(Ctor::Array, vs) => match self.cx.resolve(ty) {
                Type::Array(inner) => {
                    let mut parts = Parts::Core(body, vs, w);
                    self.array_lit_heap(m, b, dest, &inner, &mut parts, taken, line, true)?;
                }
                Type::ArrayN(inner, n) if n == vs.len() => {
                    let mut parts = Parts::Core(body, vs, w);
                    self.fixed_elems(m, b, dest, &inner, &mut parts, line)?;
                    dest.addr(b, 0);
                }
                Type::SmallArray(inner, n) if vs.len() <= n => {
                    let mut parts = Parts::Core(body, vs, w);
                    self.sa_into(m, b, dest, ty, &inner, n, &mut parts, line)?;
                    dest.addr(b, 0);
                }
                _ => return unsupported("an array literal the row does not place", line),
            },
            Rhs::Make(Ctor::Map, vs) => {
                let mut parts = Parts::Core(body, vs, w);
                self.map_into(m, b, dest, ty, &mut parts, line)?;
                dest.addr(b, 0);
            }
            Rhs::Make(Ctor::Try(name), vs) => {
                let [v] = vs.as_slice() else {
                    return unsupported(&format!("`{name}?` at this arity"), line);
                };
                let operand = |s: &mut Self, m: &mut Module, b: &mut Frame, base: &Type| {
                    s.core_val(m, b, body, w, v, base, line)
                };
                self.try_construct(m, b, name, line, operand, |_, _| dest)?;
            }
            _ => return unsupported("a made layout this walk does not build", line),
        }
        // `dest_used` is the AST arm's answer to [`Fn_::agg_into`], and this
        // walk writes the in-place build's bytes itself.
        self.dest_used = false;
        b.ins(&Instruction::Drop);
        b.ins(&Instruction::Drop);
        Ok(())
    }

    /// Whether this walk stores one PART of a made layout at the type the
    /// layout puts it at through [`Fn_::part`] (RFC-0125 M7).
    ///
    /// A value in one wasm local, which a String is as much as an `Int64`
    /// ([`Fn_::core_framed`]). A layout part is [`Fn_::agg_part`]'s.
    fn core_part_ty(&self, t: &Type) -> bool {
        self.core_framed(t)
    }

    /// Whether this walk gives a name of `t` the place the AST walk gives it —
    /// RFC-0125 M7, the frame.
    ///
    /// The allocation is [`Fn_::place_for`]'s and both walks call it, so what
    /// is asked here is whether the type HAS a place of that kind: a value
    /// that lives in one wasm local, which a `String`, a stream cursor and a
    /// vector are as much as an `Int64` is. A layout is the make arm's, which
    /// takes a slot before the parts are written.
    ///
    /// A `where` type has its base's place. Its value comes from its
    /// constructor, which is a row, or from a name already of the type.
    fn core_framed(&self, t: &Type) -> bool {
        matches!(self.cx.repr(t, 0), Ok(Repr::Scalar(_)))
    }

    /// Whether `t` is Unit, which is no value (RFC-0125 M7): a name of it
    /// needs no place, a read of it writes nothing, and its `let` or store
    /// emits only the right-hand side's effects, as the arm's statement does.
    fn core_unit(&self, t: &Type) -> bool {
        self.cx.repr(t, 0) == Ok(Repr::Unit)
    }

    /// Whether `v` is a name of a layout, which a read position takes as its
    /// address ([`Fn_::core_val`]): a call's argument, and a map's key, which
    /// [`Fn_::pack_key`] packs from there. A layout of a validated type was
    /// checked where it was made.
    fn core_layout_name(&self, body: &vyrn_lower::core::Body, v: &Val) -> bool {
        matches!(v, Val::Name(n) if {
            let t = &body.names[*n as usize].ty;
            matches!(self.cx.repr(t, 0), Ok(Repr::Agg(_)))
        })
    }

    /// Whether this walk can put the value `v` on the operand stack: a name it
    /// frames, or a literal it writes ([`Fn_::core_val`]).
    ///
    /// A different question from [`Fn_::core_operand`], which is what an ARITHMETIC
    /// row computes with. An order on two string literals is two values this
    /// walk writes and no operation it applies.
    fn core_val_readable(&self, body: &vyrn_lower::core::Body, v: &Val) -> bool {
        match v {
            Val::Name(n) => {
                let ty = &body.names[*n as usize].ty;
                self.core_framed(ty) || self.core_unit(ty)
            }
            Val::Lit(l) => !matches!(l, Lit::Opaque(_)),
        }
    }

    /// Whether a row is a call to a variant constructor, which is a made layout
    /// (RFC-0125 M7) rather than the `call` [`Fn_::core_call`] writes.
    fn core_ctor(&self, body: &vyrn_lower::core::Body, rhs: &Rhs) -> bool {
        self.core_ctor_name(body, rhs).is_some()
    }

    /// The variant a constructor row builds: the callee of a [`Callee::Ctor`]
    /// row, and for the `value(x)` box the variant of the built-in `Value` enum
    /// its operand's type picks ([`value_scalar`]). A type that boxes through
    /// its `show` is no row here, because the row does not state the call.
    fn core_ctor_name<'r>(&self, body: &vyrn_lower::core::Body, rhs: &'r Rhs) -> Option<&'r str> {
        match rhs {
            Rhs::Call {
                kind: Callee::Ctor,
                callee,
                ..
            } => Some(callee),
            Rhs::Call {
                kind: Callee::Reserved,
                callee,
                args,
                ..
            } if callee == "value" => match args.as_slice() {
                [(Arg::Val(v), _)] => {
                    value_scalar(&self.cx.resolve(&self.core_ty(body, v, &Type::Int)))
                }
                _ => None,
            },
            _ => None,
        }
    }

    /// Each fixed literal a `@list` row takes, with the name the row binds:
    /// `let l = [..]` at `[T; n]`, then `let a = @list(l)` at `Array<T>`,
    /// which takes `l` (RFC-0125 M7). The literal is made at the growable
    /// type, its parts in the heap buffer, and `a` takes its place, so no
    /// fixed copy exists.
    fn core_lists(
        &self,
        body: &vyrn_lower::core::Body,
    ) -> Vec<(vyrn_lower::core::Name, vyrn_lower::core::Name)> {
        let mut lets = Vec::new();
        for s in &body.stmts {
            core_lets(s, &mut lets);
        }
        lets.iter()
            .filter_map(|(a, rhs)| match rhs {
                Rhs::Call {
                    kind: Callee::Reserved,
                    callee,
                    args,
                    ..
                } if callee == "@list" => match args.as_slice() {
                    [(Arg::Val(Val::Name(l)), vyrn_frontend::ast::Capability::Consume)] => {
                        Some((*l, *a))
                    }
                    _ => None,
                },
                _ => None,
            })
            .filter(|(l, a)| {
                lets.iter()
                    .any(|(m, r)| m == l && matches!(r, Rhs::Make(Ctor::Array, _)))
                    && matches!(
                        (self.cx.resolve(&body.names[*l as usize].ty), self.cx.resolve(&body.names[*a as usize].ty)),
                        (Type::ArrayN(e, _), Type::Array(g)) if e == g
                    )
            })
            .collect()
    }

    /// The type a made name is built at: the growable array a `@list` row
    /// takes it into ([`Fn_::core_lists`]), and its own type otherwise.
    fn core_made_ty(&self, body: &vyrn_lower::core::Body, n: vyrn_lower::core::Name) -> Type {
        match self.core_lists(body).iter().find(|(l, _)| *l == n) {
            Some((_, a)) => body.names[*a as usize].ty.clone(),
            None => body.names[n as usize].ty.clone(),
        }
    }

    /// The tag and the payload types of the variant `name` of the sum `ty` —
    /// RFC-0125 M7. `None` when `ty` is no sum, or names no such variant.
    ///
    /// The row states the binding's type, which is the expectation the
    /// variant is picked by.
    fn core_variant(&self, ty: &Type, name: &str) -> Option<(u64, Vec<Type>)> {
        let Type::Enum(vs) = self.cx.resolve(ty) else {
            return None;
        };
        let i = vs.iter().position(|v| v.name == name)?;
        Some((i as u64, vs[i].payload.clone()))
    }

    /// Whether this walk builds the layout a row MAKES — RFC-0125 M7, the
    /// layout-made family's screen.
    ///
    /// Two rows make one: [`Rhs::Make`] states a record, an array or a map
    /// literal, and a call to [`Callee::Ctor`] states a variant of a sum, which
    /// is the same build with a tag in front of it.
    fn core_makes(&self, body: &vyrn_lower::core::Body, ty: &Type, rhs: &Rhs) -> bool {
        match rhs {
            Rhs::Make(c, vs) => self.core_made(body, ty, c, vs),
            Rhs::Prim(Op::Closure(key), vs, _) => self.core_lambda(body, key, ty, vs),
            Rhs::Call { args, .. } => {
                let Some(callee) = self.core_ctor_name(body, rhs) else {
                    return false;
                };
                matches!(self.cx.repr(ty, 0), Ok(Repr::Agg(_)))
                    && self.core_variant(ty, callee).is_some_and(|(_, p)| {
                        p.len() == args.len()
                            && args.iter().zip(&p).all(|((a, _), t)| {
                                let Arg::Val(v) = a else {
                                    return false;
                                };
                                (self.core_val_readable(body, v) && self.core_part_ty(t))
                                    || self.core_payload_layout(body, v, t)
                            })
                    })
            }
            _ => false,
        }
    }

    /// Whether `v` is a layout name that fills a payload or a part of `t` from
    /// its address: [`Fn_::build_variant`] boxes it or copies its two words,
    /// [`Fn_::agg_part`] copies its bytes, and [`Fn_::core_val`] pushes the
    /// address, as the arm's `Expr::Var` does.
    fn core_payload_layout(&self, body: &vyrn_lower::core::Body, v: &Val, t: &Type) -> bool {
        matches!(v, Val::Name(n) if {
            let nt = &body.names[*n as usize].ty;
            matches!(self.cx.repr(nt, 0), Ok(Repr::Agg(_)))
                && !self.checks(nt)
                && !self.checks(t)
                && self.cx.ll(nt) == self.cx.ll(t)
        })
    }

    /// Whether this walk builds the layout a [`Rhs::Make`] row states —
    /// RFC-0125 M7, the layout-made family's screen.
    ///
    /// It asks what [`Fn_::core_make`] needs: a layout with an offset for every
    /// part, parts this walk emits, and no check at the construction that the
    /// row does not carry.
    ///
    /// A layout part of a record or an array is a layout name here, and
    /// [`Fn_::core_built`] asks where its bytes come from, which needs the
    /// row's list. [`Fn_::map_into`] stores a map's parts through
    /// [`Fn_::part`]: a layout value is a name with a slot of its own, and
    /// [`Fn_::map_set`] moves its bytes into the entry.
    fn core_made(&self, body: &vyrn_lower::core::Body, ty: &Type, ctor: &Ctor, vs: &[Val]) -> bool {
        let layout = |t: &Type| matches!(self.cx.repr(t, 0), Ok(Repr::Agg(_))) && !self.checks(t);
        // A record of a validated type is its own: its cross-field `where`
        // is the constructor row after it (RFC-0079), or the checker's proof.
        let own =
            matches!(ctor, Ctor::Record(name, _) if self.cx.sub(ty) == Type::Named(name.clone()));
        if !(layout(ty) || own && matches!(self.cx.repr(ty, 0), Ok(Repr::Agg(_)))) {
            return false;
        }
        if let Ctor::Closure(t) = ctor {
            return self.core_closure(body, ty, t, vs).is_some();
        }
        self.core_part_tys(ty, ctor, vs.len()).is_some_and(|tys| {
            vs.iter().zip(&tys).all(|(v, t)| {
                (self.core_val_readable(body, v) && self.core_part_ty(t))
                    || (layout(t)
                        && matches!(v, Val::Name(n) if layout(&body.names[*n as usize].ty)))
            })
        })
    }

    /// The type of each part of a record, an array or a map literal of `ty`,
    /// in the order the row lists the `n` parts, and the one operand of `T?(v)`
    /// at `T`'s base where the base is one wasm local ([`Fn_::try_construct`]).
    /// `None` when the row does not fill the layout exactly.
    fn core_part_tys(&self, ty: &Type, ctor: &Ctor, n: usize) -> Option<Vec<Type>> {
        match (ctor, self.cx.resolve(ty)) {
            (Ctor::Record(_, names), _) => {
                let decl = self.cx.fields(ty)?;
                if decl.len() != n || names.len() != n {
                    return None;
                }
                names
                    .iter()
                    .map(|nm| decl.iter().find(|f| f.name == *nm).map(|f| f.ty.clone()))
                    .collect()
            }
            (Ctor::Array, Type::Array(inner)) => Some(vec![*inner; n]),
            (Ctor::Array, Type::ArrayN(inner, k)) if k == n && n > 0 => Some(vec![*inner; n]),
            (Ctor::Array, Type::SmallArray(inner, k)) if n <= k => Some(vec![*inner; n]),
            // A key and a value per entry.
            (Ctor::Map, Type::Map(k, v)) if n % 2 == 0 => Some(
                (0..n)
                    .map(|i| {
                        if i % 2 == 0 {
                            (*k).clone()
                        } else {
                            (*v).clone()
                        }
                    })
                    .collect(),
            ),
            (Ctor::Try(name), _) if n == 1 => {
                let base = &self.cx.types.get(name)?.base;
                self.core_framed(base).then(|| vec![base.clone()])
            }
            // A function value that captures nothing has no part.
            (Ctor::Closure(_), _) if n == 0 => Some(Vec::new()),
            _ => None,
        }
    }

    /// Whether this walk builds the layout the row at `ss[i]` makes, at `ty`:
    /// RFC-0125 M7, the layout-made family's screen with the row's list.
    ///
    /// A layout part of a record or an array is written at its offset by its
    /// own row ([`Fn_::core_part_at`]), or copied from a name of the part's
    /// own layout that a reader bound or that holds a place's address. A
    /// temporary's slot would stay live beside the parent's storage, where
    /// the arm writes the part in place, so such a part stays in the arm.
    fn core_built(
        &self,
        body: &vyrn_lower::core::Body,
        ss: &[St],
        i: usize,
        ty: &Type,
        rhs: &Rhs,
    ) -> bool {
        if !matches!(ss[i], St::Let(..)) || !self.core_makes(body, ty, rhs) {
            return false;
        }
        // A function value's captures are read into its capture box
        // ([`Fn_::core_closure`]), as a lambda's are.
        let Rhs::Make(ctor, vs) = rhs else {
            return true;
        };
        if let Ctor::Closure(_) = ctor {
            return true;
        }
        let Some(tys) = self.core_part_tys(ty, ctor, vs.len()) else {
            return false;
        };
        vs.iter().zip(&tys).all(|(v, t)| {
            self.core_framed(t)
                || (self.core_payload_layout(body, v, t)
                    && (matches!(ctor, Ctor::Map)
                        || matches!(v, Val::Name(n)
                            if body.names[*n as usize].binding.is_some()
                                || self.core_alias(body, *n).is_some())))
                || self.core_part_of(body, ss, i, v)
        })
    }

    /// Whether the part `v` of the layout made at `ss[i]` is written at its
    /// offset by its own row ([`Fn_::core_part_at`]).
    fn core_part_of(&self, body: &vyrn_lower::core::Body, ss: &[St], i: usize, v: &Val) -> bool {
        let Val::Name(n) = v else {
            return false;
        };
        let lists = self.core_lists(body);
        let n = &lists.iter().find(|(_, a)| a == n).map_or(*n, |(l, _)| *l);
        ss[..i]
            .iter()
            .rposition(|s| matches!(s, St::Let(l, _) if l == n))
            .and_then(|k| self.core_part_at(body, ss, k, &self.core_w))
            .is_some_and(|at| at.row == i)
    }

    /// Where the row at `ss[i]` makes a part of a record or array literal or
    /// of a variant's boxed payload ([`PartAt`]): RFC-0125 M7, a part written
    /// where its row stands. The row is a call, a variant or a literal, and a
    /// variant or a literal is built at the part's type.
    ///
    /// The row writes the part at its offset, so the parent's storage is
    /// taken before it
    /// ([`Fn_::core_part_dest`]). Every row stays where it stands, so no
    /// effect moves. Nothing names that storage until the parent's
    /// row, and no row between leaves the list, so a part written early is
    /// never left in storage no row owns. The temporary is named twice, by
    /// its `let` and by the parent, so no release row reads it.
    fn core_part_at(
        &self,
        body: &vyrn_lower::core::Body,
        ss: &[St],
        i: usize,
        w: &Walked,
    ) -> Option<PartAt> {
        let St::Let(t, rhs) = &ss[i] else {
            return None;
        };
        let made = matches!(rhs, Rhs::Make(..) | Rhs::Prim(Op::Closure(_), ..))
            || self.core_ctor(body, rhs);
        let taken = self.core_take_part(body, rhs);
        if body.names[*t as usize].binding.is_some()
            || w.occurs.get(*t as usize) != Some(&2)
            || !(made || taken || self.core_agg_call(body, rhs))
        {
            return None;
        }
        // A literal is made at the part's type, and a call's result or a
        // taken field must already have its layout.
        let fits = |part: &Type| {
            !self.checks(part)
                && if made {
                    self.core_makes(body, part, rhs)
                } else {
                    self.core_payload_layout(body, &Val::Name(*t), part)
                }
        };
        // A literal a `@list` row takes is a part under the name that row
        // binds ([`Fn_::core_lists`]).
        let named = self
            .core_lists(body)
            .iter()
            .find(|(l, _)| l == t)
            .map_or(*t, |(_, a)| *a);
        let j = (i + 1..ss.len()).find(|&j| match &ss[j] {
            St::Let(_, Rhs::Make(_, ps)) => ps.contains(&Val::Name(named)),
            St::Let(_, r @ Rhs::Call { args, .. }) if self.core_ctor(body, r) => {
                args.iter().any(|(v, _)| *v == Arg::Val(Val::Name(named)))
            }
            _ => false,
        })?;
        if ss[i + 1..j].iter().any(core_leaves) {
            return None;
        }
        let St::Let(parent, prhs) = &ss[j] else {
            return None;
        };
        let made = self.core_made_ty(body, *parent);
        let ty = if self.core_lands(body, ss, j, &w.reads) {
            &self.ret_ty
        } else {
            &made
        };
        let (ctor, ps) = match prhs {
            Rhs::Make(c, ps) => (c, ps),
            Rhs::Call { args, .. } => {
                let (_, payload) = self.core_variant(ty, self.core_ctor_name(body, prhs)?)?;
                let at = args
                    .iter()
                    .position(|(v, _)| *v == Arg::Val(Val::Name(named)))?;
                let part = payload.get(at)?.clone();
                let Ok(Repr::Agg(l)) = self.cx.repr(&part, 0) else {
                    return None;
                };
                return (self.word2(&part).ok()? == Word::Boxed && fits(&part)).then_some(PartAt {
                    row: j,
                    parent: *parent,
                    off: 0,
                    into: PartIn::Box(l.size),
                    ty: part,
                });
            }
            _ => return None,
        };
        let at = ps.iter().position(|v| *v == Val::Name(named))?;
        let (part, off, into) = match (ctor, self.cx.resolve(ty)) {
            (Ctor::Record(_, names), _) => {
                let Ok(Repr::Agg(l)) = self.cx.repr(ty, 0) else {
                    return None;
                };
                let decl = self.cx.fields(ty)?;
                let k = decl.iter().position(|f| f.name == names[at])?;
                (decl[k].ty.clone(), l.fields[k], PartIn::Parent)
            }
            (Ctor::Array, Type::ArrayN(inner, n)) if n == ps.len() => {
                let off = self.stride(&inner, 0).ok()? * at as u32;
                (*inner, off, PartIn::Parent)
            }
            (Ctor::Array, Type::Array(inner)) => {
                let off = self.stride(&inner, 0).ok()? * at as u32;
                let bytes = self.extent(&inner, ps.len(), 0).ok()?;
                (*inner, off, PartIn::Buffer(bytes))
            }
            _ => return None,
        };
        fits(&part).then_some(PartAt {
            row: j,
            parent: *parent,
            off,
            into,
            ty: part,
        })
    }

    /// Whether a row takes a layout out of a field, which in part position
    /// moves the header to the part's offset ([`Fn_::core_part_at`]). The
    /// field is a hole from the take on, and the release of its root carries
    /// the hole ([`vyrn_lower::core::Body::drop_holes`]).
    fn core_take_part(&self, body: &vyrn_lower::core::Body, rhs: &Rhs) -> bool {
        matches!(rhs, Rhs::Take(p @ vyrn_lower::core::Place::Field(..))
        if self.core_place_ty(body, p).is_some_and(|t| {
            !self.checks(&t) && matches!(self.cx.repr(&t, 0), Ok(Repr::Agg(_)))
        }))
    }

    /// Where the row at `ss[i]` writes a part of its parent
    /// ([`Fn_::core_part_at`]): a box of its own for a variant's payload, a
    /// heap array's buffer, and otherwise the parent's own storage, which is
    /// the caller's where the parent lands there, its offset in its own
    /// parent where it is a part in turn, and a slot of its own otherwise.
    /// The parent's storage and a buffer are taken at the first such part.
    fn core_part_dest(
        &mut self,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        ss: &[St],
        i: usize,
        line: usize,
    ) -> Result<Option<Dest>, String> {
        let Some(PartAt {
            row: j,
            parent: p,
            off,
            into,
            ..
        }) = self.core_part_at(body, ss, i, w)
        else {
            return Ok(None);
        };
        let St::Let(t, _) = &ss[i] else {
            return Ok(None);
        };
        if let Some(d) = w.built[*t as usize] {
            return Ok(Some(d));
        }
        let d = match into {
            PartIn::Box(bytes) => Dest::Addr(self.heap_buf(b, bytes), 0),
            PartIn::Buffer(bytes) => {
                let buf = match w.bufs[p as usize] {
                    Some(buf) => buf,
                    None => self.heap_buf(b, bytes),
                };
                w.bufs[p as usize] = Some(buf);
                Dest::Addr(buf, off)
            }
            PartIn::Parent => {
                let base = match w.built[p as usize] {
                    Some(d) => d,
                    None if self.core_lands(body, ss, j, &w.reads) => {
                        Dest::Addr(self.core_out(line)?, 0)
                    }
                    None => match self.core_part_dest(b, body, w, ss, j, line)? {
                        Some(d) => d,
                        None => {
                            let r = self.cx.repr(&body.names[p as usize].ty, line)?;
                            Dest::Slot(self.core_slot(b, w, p, &r, line)?)
                        }
                    },
                };
                w.built[p as usize] = Some(base);
                base.at(off)
            }
        };
        w.built[*t as usize] = Some(d);
        Ok(Some(d))
    }

    /// The local holding the caller's out-pointer.
    fn core_out(&self, line: usize) -> Result<u32, String> {
        self.dest
            .ok_or_else(|| gap("an aggregate result with no out-pointer", line))
    }

    /// The element type of `n` where it is a stream, and `None` elsewhere.
    fn core_stream_elem(
        &self,
        body: &vyrn_lower::core::Body,
        n: vyrn_lower::core::Name,
    ) -> Option<Type> {
        match self.cx.resolve(&body.names[n as usize].ty) {
            Type::Stream(t) => Some(*t),
            _ => None,
        }
    }

    /// Whether an element read of `base` is the read a stream's pull answers:
    /// a stream is pulled and never indexed.
    fn core_pulls(&self, body: &vyrn_lower::core::Body, base: &vyrn_lower::core::Place) -> bool {
        matches!(base, vyrn_lower::core::Place::Name(s) if self.core_stream_elem(body, *s).is_some())
    }

    /// A local holding the address of the layout `n`: its own, where its
    /// place is one, or a fresh one set from [`Fn_::core_addr_of`].
    fn core_addr_local(
        &self,
        b: &mut Frame,
        w: &Walked,
        body: &vyrn_lower::core::Body,
        n: vyrn_lower::core::Name,
        line: usize,
    ) -> Result<u32, String> {
        if let Some((Place::Local(l), _)) = self.core_place(w, body, n) {
            return Ok(l);
        }
        self.core_addr_of(b, w, body, n, line)?;
        let l = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(l));
        Ok(l)
    }

    /// Push the address of the aggregate the name `n` holds: a slot's, or the
    /// one a parameter's local holds, which is what `Expr::Var` pushes for a
    /// layout.
    fn core_addr_of(
        &self,
        b: &mut Frame,
        w: &Walked,
        body: &vyrn_lower::core::Body,
        n: vyrn_lower::core::Name,
        line: usize,
    ) -> Result<(), String> {
        let Some((place, _)) = self.core_place(w, body, n) else {
            return unsupported("a core name with no place", line);
        };
        match place {
            Place::Local(l) => {
                b.ins(&Instruction::LocalGet(l));
            }
            Place::Slot(_) | Place::Static(_) => {
                place.addr(b, 0);
            }
        }
        Ok(())
    }

    /// Bind the core name `n` at `place`. A name a `Stmt::Let` wrote goes on
    /// the scope too, so a statement the AST arm emits after this one finds it
    /// exactly where that arm would have put it (RFC-0125 §3 M3, the
    /// interleave slice), with the release it owes, keyed as the arm keys it:
    /// the plan names the `Stmt::Let` and the row carries the same node.
    ///
    /// The question is the ROW's: [`vyrn_lower::core::NameInfo::binding`] is
    /// the node the plan keys the binding by, and a temporary this pass minted
    /// has none. The spelling is not the question: a projection inlined at its
    /// access site renames its own bindings to `@b<tag>.<name>`, and the
    /// statements after them still name them.
    fn core_bind(
        &mut self,
        b: &mut Frame,
        body: &vyrn_lower::core::Body,
        w: &mut Walked,
        n: vyrn_lower::core::Name,
        place: Place,
        ty: Type,
    ) -> Result<(), String> {
        let info = &body.names[n as usize];
        w.at[n as usize] = Some((place, ty.clone()));
        let Some(key) = info.binding else {
            return Ok(());
        };
        self.scope.push((info.source.clone(), place, ty.clone()));
        if self.releases_whole(key) {
            if let Some(r) = self.rel_owed(key, &ty, info.line)? {
                self.register_rel(b, key, place, r);
            }
        }
        Ok(())
    }

    /// The declaration a check row names: `T(v)` of a type with a `where`
    /// clause whose base is one wasm value.
    fn core_named(&self, callee: &str, kind: Callee) -> Option<TypeDecl> {
        let decl = self
            .cx
            .types
            .get(callee)
            .filter(|d| d.predicate.is_some())?;
        (matches!(kind, Callee::Named | Callee::Proven)
            && matches!(self.cx.repr(&decl.base, 0), Ok(Repr::Scalar(_))))
        .then(|| decl.clone())
    }

    /// The check a validated record owes once it is made
    /// ([`vyrn_lower::core::Builder`]'s `bind`): its constructor reading the
    /// made layout `n` in place, and the declaration whose `where` it runs.
    fn core_checks_made(
        &self,
        body: &vyrn_lower::core::Body,
        rhs: &Rhs,
    ) -> Option<(TypeDecl, vyrn_lower::core::Name)> {
        let Rhs::Call {
            callee,
            args,
            kind: Callee::Named,
            ..
        } = rhs
        else {
            return None;
        };
        let [(Arg::Val(Val::Name(n)), vyrn_frontend::ast::Capability::Read)] = args.as_slice()
        else {
            return None;
        };
        let decl = self
            .cx
            .types
            .get(callee)
            .filter(|d| d.predicate.is_some())?;
        (matches!(self.cx.repr(&decl.base, 0), Ok(Repr::Agg(_)))
            && body.names[*n as usize].ty == Type::Named(callee.clone()))
        .then(|| (decl.clone(), *n))
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
    fn core_sig(
        &self,
        body: &vyrn_lower::core::Body,
        callee: &str,
        kind: Callee,
        solved: &[(String, Type)],
        targets: &[Target],
    ) -> Option<Sig> {
        if let Some((_, t)) = self.core_through(body, callee, kind) {
            return self.value_sig(&t);
        }
        if !targets.is_empty() {
            let (f, _, subst, bound) = self.core_ho(callee, kind, solved, targets)?;
            return self.cx.signature(&ho_shell(f, &subst, &bound).0).ok();
        }
        // A routed builtin is a call to the function its row names.
        let (callee, kind) = match core_builtin(callee, kind) {
            Some(Spec::Routes(f)) => (*f, Callee::Fn),
            _ => (callee, kind),
        };
        if kind != Callee::Fn || self.audit_dropped(callee) {
            return None;
        }
        // A `modify` parameter crosses as the address of the caller's binding,
        // which [`Fn_::core_args_readable`] admits for a layout alone. An
        // aggregate result crosses through the out-pointer, which
        // [`Fn_::out_ptr`] states for both walks.
        match self.core_instance(callee, kind, solved) {
            Some((f, _, subst)) => self.cx.signature(&instance_shell(f, &subst)).ok(),
            None => (self.cx.sigs.get(callee).cloned()).or_else(|| self.cx.lambda_sig(callee)),
        }
    }

    /// The generic function a call row names and the instance the checker
    /// solved for it ([`solved_instance`]). `None` for a callee with no type
    /// parameters.
    fn core_instance(
        &self,
        callee: &str,
        kind: Callee,
        solved: &[(String, Type)],
    ) -> Option<(&'p Function, Vec<Type>, HashMap<String, Type>)> {
        let f = (self.cx.generics.get(callee).copied()).filter(|_| kind == Callee::Fn)?;
        let (targs, subst) = solved_instance(f, solved)?;
        Some((f, targs, subst))
    }

    /// The specialization a call row names (RFC-0023): the higher-order
    /// function, its type arguments and substitution, and the target of each
    /// `fn`-typed parameter as the row names it. `None` where a target is not
    /// a function this module defines and calls directly, and where a type
    /// parameter is unsolved.
    #[allow(clippy::type_complexity)]
    fn core_ho(
        &self,
        callee: &str,
        kind: Callee,
        solved: &[(String, Type)],
        targets: &[Target],
    ) -> Option<(
        &'p Function,
        Vec<Type>,
        HashMap<String, Type>,
        Vec<FnTarget>,
    )> {
        let f = (self.cx.higher_order.get(callee).copied()).filter(|_| kind == Callee::Fn)?;
        let (targs, subst) = solved_instance(f, solved)?;
        let fns: Vec<&Type> = (f.params.iter())
            .map(|p| &p.ty)
            .filter(|t| matches!(t, Type::Fn(..)))
            .collect();
        if fns.len() != targets.len() {
            return None;
        }
        let bound = (targets.iter().zip(fns))
            .map(|(t, p)| match t {
                Target::Value(_) => self.core_dispatch(&ftypes::substitute(p, &subst)),
                t => self.core_target(t),
            })
            .collect::<Option<_>>()?;
        Some((f, targs, subst, bound))
    }

    /// The function a [`Target`] calls: one this module defines and calls
    /// directly, with no captures. `None` for a pass-through, which only a
    /// specialization binds, and for a function taking a `modify` parameter,
    /// which no function value may name.
    fn core_target(&self, t: &Target) -> Option<FnTarget> {
        match t {
            Target::Fn(name) => {
                let sig = (self.cx.sigs.get(name)).filter(|s| !s.modify.iter().any(|m| *m))?;
                Some(FnTarget {
                    sig: sig.clone(),
                    ncaps: 0,
                })
            }
            Target::Lambda(key, caps) => Some(FnTarget {
                sig: self.cx.lambda_sig(key)?,
                ncaps: caps.len(),
            }),
            Target::Param(_) | Target::Value(_) => None,
        }
    }

    /// The target a stored value of the `fn` type `p` is (RFC-0037): its
    /// signature's dispatcher, with the value as its one capture. The index is
    /// the registered
    /// dispatcher's, which [`Fn_::core_call`] registers before it emits;
    /// a screen asks before that and reads 0.
    fn core_dispatch(&self, p: &Type) -> Option<FnTarget> {
        let sig_ty = crate::normalize_fn_sig(&self.cx.sub(p), &self.cx.types);
        let Type::Fn(ptys, ret) = &sig_ty else {
            return None;
        };
        let index = (self.cx.dispatch.borrow().sigs.iter())
            .find(|(t, _)| *t == sig_ty)
            .map_or(0, |(_, s)| s.index);
        let params: Vec<Type> = std::iter::once(sig_ty.clone())
            .chain(ptys.iter().cloned())
            .collect();
        Some(FnTarget {
            sig: Sig {
                index,
                modify: vec![false; params.len()],
                params,
                ret: self.cx.repr(ret, 0).ok()?,
                ret_ty: (**ret).clone(),
            },
            ncaps: 1,
        })
    }

    /// The stored value a call row calls through, and its signature. `None`
    /// also for a parameter a specialization bound (RFC-0023).
    fn core_through(
        &self,
        body: &vyrn_lower::core::Body,
        callee: &str,
        kind: Callee,
    ) -> Option<(vyrn_lower::core::Name, Type)> {
        let n = kind
            .value()
            .filter(|_| !self.fn_binds.contains_key(callee))?;
        let info = &body.names[n as usize];
        let sig_ty = crate::normalize_fn_sig(&self.cx.sub(&info.ty), &self.cx.types);
        matches!(sig_ty, Type::Fn(..)).then_some((n, sig_ty))
    }

    /// The signature a call through a value of `sig_ty` sees: its own
    /// parameters and result. Its index names no function; the call is the
    /// dispatcher's ([`Fn_::core_call`]).
    fn value_sig(&self, sig_ty: &Type) -> Option<Sig> {
        let Type::Fn(ps, r) = sig_ty else {
            return None;
        };
        Some(Sig {
            index: 0,
            modify: vec![false; ps.len()],
            params: ps.clone(),
            ret: self.cx.repr(r, 0).ok()?,
            ret_ty: (**r).clone(),
        })
    }

    /// The signature and the target of a function value a row makes
    /// (RFC-0037), [`Fn_::core_target`]'s with no parts. `None` where `ty` is
    /// no function type.
    fn core_closure(
        &self,
        body: &vyrn_lower::core::Body,
        ty: &Type,
        t: &Target,
        parts: &[Val],
    ) -> Option<(Type, FnTarget)> {
        let sig_ty = crate::normalize_fn_sig(&self.cx.sub(ty), &self.cx.types);
        if !matches!(sig_ty, Type::Fn(..)) {
            return None;
        }
        let target = self.core_target(t)?;
        let readable = parts.iter().all(|v| {
            matches!(v, Val::Name(c)
                if self.core_val_readable(body, v)
                    || self.core_payload_layout(body, v, &body.names[*c as usize].ty))
        });
        (target.ncaps == parts.len() && readable).then_some((sig_ty, target))
    }

    /// Whether this walk makes the lambda a closure row names by `key` at `ty`:
    /// RFC-0125 M7, [`Fn_::core_make`]'s screen. The row lists its captures
    /// in the order the lifted signature takes them
    /// ([`vyrn_lower::core::lambda_captures`]), and each is a name this walk
    /// reads.
    fn core_lambda(&self, body: &vyrn_lower::core::Body, key: &str, ty: &Type, vs: &[Val]) -> bool {
        let sig_ty = crate::normalize_fn_sig(&self.cx.sub(ty), &self.cx.types);
        let (Type::Fn(ptys, _), Some(Expr::Lambda { params, .. })) = (&sig_ty, self.literal(key))
        else {
            return false;
        };
        matches!(self.cx.repr(&sig_ty, 0), Ok(Repr::Agg(_)))
            && params.len() == ptys.len()
            && vs.iter().all(|v| {
                matches!(v, Val::Name(c)
                    if self.core_val_readable(body, v)
                        || self.core_payload_layout(body, v, &body.names[*c as usize].ty))
            })
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
        op: &Op,
        vs: &[Val],
        line: usize,
    ) -> Result<Type, String> {
        match (op, vs) {
            (Op::Un(u), [v]) => {
                let t = self.cx.resolve(&self.core_ty(body, v, &Type::Int));
                self.core_val(m, b, body, w, v, &t, line)?;
                self.un_ins(b, *u, &t, line)
            }
            // A conversion is the operand at its own type and then the
            // coercion plan's rungs to the target, which is what the AST arm
            // writes for `Int32(n)` (`Fn_::call_inner`'s conversion rung).
            // The plan decides the instructions; this row decides nothing.
            (Op::Conv(to), [v]) => {
                let from = self.core_ty(body, v, &Type::Int);
                self.core_val(m, b, body, w, v, &from, line)?;
                self.coerce(m, b, None, &from, to, line)?;
                Ok(to.clone())
            }
            // A String operator is a call, and the builder states its
            // operand temporaries' releases as rows of their own.
            (Op::Bin(o), [l, r]) if self.core_str_op(body, *o, l, r) => {
                self.core_val(m, b, body, w, l, &Type::Str, line)?;
                if let (BinOp::Match, Val::Lit(Lit::Str(pat))) = (o, r) {
                    self.str_match(m, b, pat, line)?;
                    return Ok(Type::Bool);
                }
                self.core_val(m, b, body, w, r, &Type::Str, line)?;
                self.str_bin(b, *o, line)
            }
            (Op::Bin(o), [l, r]) if self.core_code_concat(body, *o, l, r) => {
                let mut ty = |_: &mut Self, _: usize| unsupported("a concatenation's type", line);
                let mut operand =
                    |s: &mut Self, m: &mut Module, b: &mut Frame, i: usize, t: &Type| {
                        s.core_val(m, b, body, w, [l, r][i], t, line)
                    };
                self.host(m, b, "+", 2, &mut ty, &mut operand, line)
            }
            (Op::Bin(o), [l, r]) => {
                // A float literal has the type the checker gave it, which is
                // its sibling's: `0.0 - o` with `o: Float32` runs at
                // `Float32`, the type of the local the row binds. An integer
                // literal widens by [`Fn_::op_width`] below.
                let lt = match (l, r) {
                    (Val::Lit(Lit::Float(_)), Val::Name(_)) => self.core_ty(body, r, &Type::Int),
                    _ => self.core_ty(body, l, &Type::Int),
                };
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
                self.bin_ins(b, *o, &opty, &lt, line)
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
                if self.core_unit(&body.names[*n as usize].ty) {
                    return Ok(());
                }
                // The place is this walk's, or — since the interleave slice —
                // the one the AST arm bound the name at, which is the scope's.
                let Some((place, ty)) = self.core_place(w, body, *n) else {
                    return unsupported("a core name with no place", line);
                };
                match place {
                    Place::Local(l) => {
                        b.ins(&Instruction::LocalGet(l));
                    }
                    // A layout is its address, which is what the `Expr::Var`
                    // arm pushes for one.
                    _ if matches!(self.cx.repr(&ty, line)?, Repr::Agg(_))
                        && place.addr(b, 0).is_some() => {}
                    _ => return unsupported("a core name that is not a local", line),
                }
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
                Lit::Opaque(_) => return unsupported("a value the row does not name", line),
            },
        };
        self.coerce(m, b, None, &got, want, line)
    }

    /// The result of a `std/mem` primitive call, and `None` for a callee that
    /// is not one or an arity the table does not state.
    fn core_mem_ty(&self, callee: &str, args: usize) -> Option<Type> {
        let prim = callee.strip_prefix(vyrn_frontend::loader::MEM_PREFIX)?;
        let (_, params, ret) = self.mem_spec(prim, 0).ok()?;
        (params.len() == args).then_some(ret)
    }

    /// What the AST arm would bind for one row — the screen's type clause.
    ///
    /// It is not the operator table stated twice: what it asks is which VALUE
    /// the arm evaluates, whose type is the arm's answer for the binding. An
    /// operator's is its first operand's, because that is the one the width
    /// rule ([`Fn_::op_width`]) adopts from. A call and a closure bind what
    /// the checker typed at the site, which the row carries as its producer
    /// type, so the clause reads it there and asks no callee.
    fn core_arm_ty(&self, body: &vyrn_lower::core::Body, rhs: &Rhs) -> Option<Type> {
        Some(match rhs {
            Rhs::Val(Val::Lit(l)) => match l {
                Lit::Int(_) | Lit::Byte(_) => Type::Int,
                Lit::Float(_) => Type::Float,
                Lit::Bool(_) => Type::Bool,
                Lit::Str(_) => Type::Str,
                Lit::Opaque(_) => return None,
            },
            Rhs::Val(Val::Name(m)) => body.names[*m as usize].ty.clone(),
            Rhs::Call { ret, .. } | Rhs::Prim(Op::Closure(_), _, ret) => ret.clone()?,
            Rhs::Prim(Op::Conv(to), ..) => to.clone(),
            Rhs::Prim(_, vs, _) => self.core_ty(body, vs.first()?, &Type::Int),
            Rhs::Read(p) | Rhs::Take(p) => self.core_place_ty(body, p)?,
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
        // A frame with a hoisted walk is one whose emission is more than its
        // statements. A placed release is not: the rows state it and
        // [`Fn_::core_release`] emits it (RFC-0125 M7). Nor is an aggregate
        // result, which crosses through the caller's out-pointer and is
        // written at the `return` ([`Fn_::core_lands`]); what is refused is a
        // result checked where it is returned, because the row states no
        // check (RFC-0079). A value of the result's own validated type was
        // checked where it was made.
        if matches!(self.ret, Repr::Agg(_)) && self.checks(&self.ret_ty) {
            let mut crosses = false;
            for st in &body.stmts {
                core_leaf_rows(st, &mut |x| {
                    crosses |= matches!(x, St::Return { value: Some(v), .. }
                        if !matches!(v, Val::Name(r) if body.names[*r as usize].ty == self.ret_ty));
                });
            }
            if crosses {
                return false;
            }
        }
        let mut lets = Vec::new();
        let mut binders = Vec::new();
        for st in &body.stmts {
            core_lets(st, &mut lets);
            core_switched(st, false, &mut binders);
        }
        // A made layout is built into the ANNOTATION's layout, and the
        // per-body walk builds into the name's, which is the type of the
        // VALUE. The two are one layout where they resolve alike, and a made
        // layout whose `let` annotates another type stays in the arm. The key
        // is the node the plan keys the binding by, which is that `Stmt::Let`.
        let annotated = match stmts {
            Some(blk) => self.annotations(|fs| each_block(blk, &mut |_| {}, fs)),
            None => Vec::new(),
        };
        let occurs = body.occurrences();
        for (n, info) in body.names.iter().enumerate() {
            // A value with a place of its own: one wasm local, whatever the
            // type in it (RFC-0125 M7, the frame). A `where` type is not one —
            // it is a `check` row the core does not carry, which is the
            // census's row 7.
            //
            // Or a layout this walk MAKES. Such a name is bound and never
            // read: every read of a value goes through
            // [`Fn_::core_val_readable`], which asks the same question.
            //
            // Or a PARAMETER of a layout type, whose local holds the address
            // the caller passed. The name has a place either way; what this
            // walk cannot do with a layout is read it as a value, and
            // [`Fn_::core_val_readable`] is where that is refused.
            //
            // Or a layout read out of a place, which holds the place's address
            // ([`Fn_::core_alias`]) or, where it owns no heap, a copy of its
            // bytes, as a take out of a place does ([`Fn_::core_copies`]), or
            // a header a loop walks, which holds its parts
            // ([`Fn_::core_walked`]).
            //
            // Or a layout bound by a move, which holds the place the moved
            // name held ([`Fn_::core_renames`]).
            //
            // Or a payload binder, whose place the switch gives it
            // ([`Fn_::core_payload`]).
            //
            // Or an aggregate a call returns into the slot this walk takes for
            // it ([`Fn_::out_ptr`]). A temporary made or returned into is
            // one [`Fn_::core_readable`] asks about where it stands, because
            // what places it is the `return` after it.
            //
            // Or the temporary a layout `if` or `match` expression joins
            // through, which its first store slots ([`Fn_::core_joins`]).
            //
            // Or a Unit name, which is no value ([`Fn_::core_unit`]), or a
            // name no row names. Neither needs a place.
            if occurs[n] != 0
                && !(self.core_framed(&info.ty)
                    || self.core_unit(&info.ty)
                    || (body.params.contains(&(n as vyrn_lower::core::Name))
                        && matches!(self.cx.repr(&info.ty, 0), Ok(Repr::Agg(_)))))
                && !(!self.annotated_apart(&annotated, info)
                    && lets.iter().any(|(b, rhs)| {
                        *b as usize == n
                            && (self.core_makes(body, &self.core_made_ty(body, *b), rhs)
                                || self.core_agg_call(body, rhs)
                                || self.core_take_part(body, rhs)
                                || self.core_rebuild(body, rhs))
                    }))
                && self.core_alias(body, n as vyrn_lower::core::Name).is_none()
                && self
                    .core_copies(body, n as vyrn_lower::core::Name)
                    .is_none()
                && self
                    .core_renames(body, n as vyrn_lower::core::Name)
                    .is_none()
                && !self.core_lists(body).iter().any(|(_, a)| *a as usize == n)
                && !binders.contains(&(n as vyrn_lower::core::Name))
                && !self.core_walked(body, n as vyrn_lower::core::Name)
                && !self.core_joins(body, n as vyrn_lower::core::Name)
            {
                return false;
            }
        }
        // The type the READER wrote, which the clause above cannot see: the
        // core names a `let` by the type of its VALUE (`core::Builder`'s `let`
        // arm) except where it states the annotation's check, so a check the
        // rows do not state is a binding whose type is not the annotation's.
        // [`Fn_::core_run`] asks the same question per statement.
        if stmts.is_some_and(|blk| self.annotates_a_check(body, blk)) {
            return false;
        }
        let reads = body.reads();
        self.core_readable(body, &body.stmts, &reads, &[])
    }

    /// The type each annotated `let` a walk reaches names, resolved, keyed by
    /// the node the core keys a binding by.
    fn annotations(&self, walk: impl FnOnce(&mut dyn FnMut(&Stmt))) -> Vec<(usize, Type)> {
        let mut out = Vec::new();
        walk(&mut |s| {
            if let Stmt::Let { ty: Some(t), .. } = s {
                let at = self.cx.plan.key_of(s as *const Stmt as usize);
                out.push((at, self.cx.resolve(t)));
            }
        });
        out
    }

    /// Whether a name is bound by a `let` of `annotated` whose annotation is
    /// another type than the name's. The core names a `let` by the type of its
    /// value, so the row does not state the annotation's layout.
    fn annotated_apart(
        &self,
        annotated: &[(usize, Type)],
        info: &vyrn_lower::core::NameInfo,
    ) -> bool {
        info.binding.is_some_and(|at| {
            annotated
                .iter()
                .any(|(a, t)| *a == at && *t != self.cx.resolve(&info.ty))
        })
    }

    /// Whether any `let` of `blk` is annotated with a type that carries a
    /// `where` clause (RFC-0079) and binds a name of another type, which is a
    /// check the rows do not state.
    fn annotates_a_check(&self, body: &vyrn_lower::core::Body, blk: &Block) -> bool {
        let mut found = false;
        each_block(blk, &mut |_| {}, &mut |s| {
            if let Stmt::Let { ty: Some(t), .. } = s {
                let at = s as *const Stmt as usize;
                found |= self.checks(t)
                    && !body
                        .names
                        .iter()
                        .any(|i| i.binding == Some(at) && i.ty == self.cx.sub(t));
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
    /// `bound` holds the names with a place on the path to `ss`: the payload
    /// binders of the arms it is inside, and the names an enclosing list made
    /// before it, whose slots the walk holds to the end of their extent.
    fn core_readable(
        &self,
        body: &vyrn_lower::core::Body,
        ss: &[St],
        reads: &[u32],
        bound: &[vyrn_lower::core::Name],
    ) -> bool {
        let path = |i: usize| -> Vec<vyrn_lower::core::Name> {
            let made = ss[..i].iter().filter_map(|p| match p {
                St::Let(l, rhs)
                    if matches!(rhs, Rhs::Make(..))
                        || self.core_ctor(body, rhs)
                        || self.core_agg_call(body, rhs) =>
                {
                    Some(*l)
                }
                _ => None,
            });
            bound.iter().copied().chain(made).collect()
        };
        ss.iter().enumerate().all(|(i, s)| match s {
            // A made layout is built into the name's own slot, which the
            // name holds to the end of its extent, or into the caller's
            // storage where the `return` after it hands it back
            // ([`Fn_::core_lands`]) (RFC-0125 M7).
            St::Let(n, rhs)
                if matches!(rhs, Rhs::Make(..) | Rhs::Prim(Op::Closure(_), ..))
                    || self.core_ctor(body, rhs) =>
            {
                let part = self.core_part_at(body, ss, i, &self.core_w);
                let made = self.core_made_ty(body, *n);
                let ty = match &part {
                    Some(at) => &at.ty,
                    None if self.core_lands(body, ss, i, reads) => &self.ret_ty,
                    None => &made,
                };
                self.core_built(body, ss, i, ty, rhs)
            }
            // An aggregate call result has a slot of its own, which the
            // reader's `let` takes before the call, the storage the call
            // wrote, or the caller's storage.
            St::Let(_, rhs) if self.core_agg_call(body, rhs) => true,
            St::Let(_, Rhs::Take(_)) if self.core_part_at(body, ss, i, &self.core_w).is_some() => {
                true
            }
            // An accumulator's append is read with the store after it.
            St::Let(_, rhs @ Rhs::Call { args, .. }) if self.core_rebuild(body, rhs) => {
                !matches!(args.first(), Some((Arg::Val(Val::Name(x)), _)) if body.names[*x as usize].grows)
                    || self
                        .core_rebuilt(body, ss, i + 1 + drops_ahead(ss[i + 1..].iter()))
                        .is_some()
            }
            St::Let(n, Rhs::Read(p)) if self.core_walked(body, *n) => {
                self.core_place_ty(body, p).is_some()
            }
            // The head of a `for` over a stream, whose element is one wasm
            // value ([`Spec::Pulls`]).
            St::Let(
                _,
                Rhs::Call {
                    callee, kind, args, ..
                },
            ) if matches!(core_builtin(callee, *kind), Some(Spec::Pulls)) => {
                matches!(args.as_slice(), [(Arg::Val(Val::Name(s)), _)]
                    if self.core_stream_elem(body, *s).is_some_and(|t| self.core_framed(&t)))
            }
            St::Let(_, Rhs::Read(vyrn_lower::core::Place::Elem(s, _)))
                if self.core_pulls(body, s) =>
            {
                true
            }
            St::Let(n, _)
                if self.core_alias(body, *n).is_some()
                    || self.core_copies(body, *n).is_some()
                    || self.core_renames(body, *n).is_some()
                    || self.core_lists(body).iter().any(|(_, a)| a == n) =>
            {
                true
            }
            St::Let(_, rhs) => self.core_rhs_readable(body, rhs),
            St::Store { .. } if self.core_rebuilt(body, ss, i).is_some() => true,
            // A store into a place with an address ([`Fn_::core_stmts`]),
            // module state's static address as much as a name's slot. A
            // layout's value is a name of its type, whose bytes are copied.
            St::Store { place, value, .. } => {
                use vyrn_lower::core::Place as At;
                let ty = match place {
                    At::Name(n) => Some(body.names[*n as usize].ty.clone()),
                    At::Key(_, k)
                        if !self.core_val_readable(body, k) && !self.core_layout_name(body, k) =>
                    {
                        None
                    }
                    At::Key(m, _) => {
                        match self.core_place_ty(body, m).map(|t| self.cx.resolve(&t)) {
                            Some(Type::Map(_, v)) => Some(*v),
                            _ => None,
                        }
                    }
                    p => self.core_place_ty(body, p),
                };
                // A value of the place's own validated type crosses nothing;
                // any other one is a check the row does not state.
                ty.is_some_and(|t| {
                    let r = self.cx.resolve(&t);
                    let fits = match self.cx.repr(&t, 0) {
                        Ok(Repr::Unit | Repr::Scalar(_)) => self.core_val_readable(body, value),
                        Ok(Repr::Agg(_)) => {
                            matches!(value, Val::Name(v)
                                    if self.cx.resolve(&body.names[*v as usize].ty) == r)
                        }
                        _ => false,
                    };
                    fits && (!self.checks(&t)
                        || matches!(value, Val::Name(n) if body.names[*n as usize].ty == t))
                })
            }
            St::If {
                cond, then, els, ..
            } => {
                self.core_val_readable(body, cond)
                    && self.core_readable(body, then, reads, &path(i))
                    && self.core_readable(body, els, reads, &path(i))
            }
            St::Block { body: inner, .. } => self.core_readable(body, inner, reads, &path(i)),
            St::Loop { body: inner, .. } => self.core_readable(body, inner, reads, &path(i)),
            St::Break { .. } | St::Continue { .. } => true,
            // A tag is read and an arm is chosen off the row
            // ([`Fn_::core_switch`]). What that needs is a scrutinee this walk
            // can take the address of, a tag on every arm, and a place for
            // every payload binder ([`Fn_::core_payload`]).
            St::Switch { on, arms, site, .. } => {
                let Val::Name(n) = on else {
                    return false;
                };
                // The place is the one this walk bound (a layout the run MAKES,
                // which the `let` arm slots before the switch is reached, or a
                // payload binder, which the enclosing switch binds when it
                // enters the arm) or the one the AST arm bound.
                let path = path(i);
                let placed = path.contains(n)
                    || self.core_place(&self.core_w, body, *n).is_some()
                    || self.core_alias(body, *n).is_some()
                    || self.core_copies(body, *n).is_some()
                    || self.core_renames(body, *n).is_some();
                placed
                    && self.sum_of(&body.names[*n as usize].ty).is_some()
                    // A construct the plan releases WHOLE is one the arm gives
                    // a slot and a copy of its own, because an arm may build
                    // over the scratch the scrutinee was left in. The row
                    // states the release and not the copy.
                    && !self.releases_whole(*site)
                    && arms.iter().all(|a| {
                        a.test
                            .reads()
                            .is_none_or(|h| self.core_framed(&body.names[h as usize].ty))
                            && a.binds
                                .iter()
                                .all(|bn| self.core_payload(&body.names[*bn as usize].ty))
                            && self.core_readable(
                                body,
                                &a.body[a.reads(on).len()..],
                                reads,
                                &[&path[..], &a.binds[..]].concat(),
                            )
                    })
            }
            // A release is the row's, at every exit, and the walk emits it
            // where it stands ([`Fn_::core_release`]). What it needs is the
            // node the plan keys the slot by, which is the name's binding.
            St::Row { name, .. } => body.names[*name as usize].binding.is_some(),
            // A `?` on an `Option` returns `None` with no value on the row,
            // which is the return type's tag and no value this walk writes.
            St::Return { value, .. } => match value {
                None => matches!(self.ret, Repr::Unit),
                Some(Val::Name(n)) if matches!(self.ret, Repr::Agg(_)) => {
                    self.core_returns_as_is(&body.names[*n as usize].ty)
                }
                Some(v) => self.core_val_readable(body, v),
            },
            // A discarded value is dropped at the type the ROW produces, and
            // only a call row states one — a `St::Do` of anything else would
            // reach [`Fn_::core_rhs_ty`] and fail there rather than stand down.
            // A discarded layout is the storage its call wrote, a slot of the
            // row's own that the row's end gives back, as an unbound
            // temporary's is.
            St::Do { rhs, line, .. } => {
                self.core_checks_made(body, rhs).is_some()
                    || (self.core_rhs_readable(body, rhs) || self.core_agg_call(body, rhs))
                        && self.core_rhs_ty(body, rhs, *line).is_ok()
            }
            // A release stated as a statement ([`Fn_::core_drop`]) needs the
            // name's place: one wasm local, or a layout the walk bound, which
            // [`Fn_::core_walkable`]'s name clause has already asked.
            St::Drop(n, ..) => {
                let ty = &body.names[*n as usize].ty;
                self.core_framed(ty) || matches!(self.cx.repr(ty, 0), Ok(Repr::Agg(_)))
            }
            St::Trap => true,
        })
    }

    /// Whether this walk writes every argument of a call row.
    ///
    /// A value that owns heap crosses as its pointer, and who owns it after
    /// the call is the call arm's decision: this walk writes the pointer and
    /// nothing else, which is what a `read` or a `consume` argument is. A
    /// String literal is static data, which `free` refuses. A layout crosses as
    /// its address the same way ([`Fn_::core_val`]), whatever the capability:
    /// every layout this walk names lives in a slot, and a `modify` callee
    /// copies its result back into it. A scalar `modify` argument lives in a
    /// local, which [`Fn_::spill`] gives an address for the call's extent. A
    /// name that holds another place's address ([`Fn_::core_alias`]) may not
    /// be written through. A place argument is a removal's receiver, which
    /// [`Fn_::core_removes`] reads, or a layout a declared function modifies
    /// ([`Fn_::core_user_callee`]).
    fn core_args_readable(
        &self,
        body: &vyrn_lower::core::Body,
        args: &[(Arg, vyrn_frontend::ast::Capability)],
    ) -> bool {
        use vyrn_frontend::ast::Capability as Cap;
        args.iter().all(|(a, c)| {
            let v = match a {
                Arg::Val(v) => v,
                // A place a `modify` parameter writes crosses as its address,
                // as a layout name does; a scalar one has no local to reload.
                Arg::Place(p) => {
                    return *c == Cap::Modify
                        && self.core_place_ty(body, p).is_some_and(|t| {
                            matches!(self.cx.repr(&t, 0), Ok(Repr::Agg(_))) && !self.checks(&t)
                        });
                }
            };
            let layout = self.core_layout_name(body, v);
            match c {
                Cap::Read | Cap::Consume => self.core_val_readable(body, v) || layout,
                Cap::Modify => match v {
                    Val::Name(n) if layout => self.core_alias(body, *n).is_none(),
                    // A temporary may ride the operand stack, which has no
                    // local to reload into.
                    Val::Name(n) => {
                        self.core_val_readable(body, v)
                            && (body.names[*n as usize].binding.is_some()
                                || body.params.contains(n))
                    }
                    Val::Lit(_) => false,
                },
            }
        })
    }

    /// Whether a row is a call this walk makes whose result is an aggregate,
    /// which crosses through an out-pointer ([`Fn_::out_ptr`], RFC-0125 M7).
    ///
    /// A map's key read is one too (§2.1): the runtime's lookup builds the
    /// `Option` in a slot of its own ([`Fn_::map_at`]), and the binding copies
    /// it. The row keeps the PLACE, because the kernel's alias of `m[..]` is
    /// what refuses a write to the map while the read is live. A value that
    /// travels boxed stays in the arm: the lookup copies it into a box, and
    /// the row states no release for that box.
    fn core_agg_call(&self, body: &vyrn_lower::core::Body, rhs: &Rhs) -> bool {
        match rhs {
            Rhs::Call {
                callee,
                args,
                kind,
                solved,
                targets,
                ret,
                ..
            } => {
                self.core_removes(body, callee, *kind, args) == Some(true)
                    || self.core_args_readable(body, args)
                        && (arg_vals(args).is_some() || self.core_user_callee(callee, *kind))
                        && (match core_builtin(callee, *kind) {
                            // The result type is the row's, which a stream
                            // reader's operands do not carry.
                            Some(Spec::Builds(_)) => ret.is_some(),
                            // `x.copy()` of a layout: [`Fn_::copy_stack`] builds
                            // the copy in a slot of its own, as `Builds` does.
                            Some(Spec::OwnType) => {
                                matches!(args.as_slice(), [(Arg::Val(Val::Name(n)), _)]
                                if matches!(self.cx.repr(&body.names[*n as usize].ty, 0), Ok(Repr::Agg(_))))
                            }
                            _ => false,
                        } || self
                            .core_sig(body, callee, *kind, solved, targets)
                            .is_some_and(|s| s.params.len() == args.len() && s.ret.agg().is_some()))
            }
            Rhs::Read(vyrn_lower::core::Place::Key(base, k)) => {
                (self.core_val_readable(body, k) || self.core_layout_name(body, k))
                    && matches!(
                        self.core_place_ty(body, base).map(|t| self.cx.resolve(&t)),
                        Some(Type::Map(..))
                    )
            }
            _ => false,
        }
    }

    /// Whether a row rebuilds a named `Array` or `Map` receiver, a
    /// `SmallArray` one it pushes to, or a String accumulator in place
    /// ([`Spec::Rebuilds`]), with operands this walk writes.
    fn core_rebuild(&self, body: &vyrn_lower::core::Body, rhs: &Rhs) -> bool {
        let Rhs::Call {
            callee, kind, args, ..
        } = rhs
        else {
            return false;
        };
        matches!(core_builtin(callee, *kind), Some(Spec::Rebuilds))
            && matches!(args.split_first(), Some(((Arg::Val(Val::Name(x)), _), rest))
                if (body.names[*x as usize].grows
                    || matches!(
                        (callee.as_str(), self.cx.resolve(&body.names[*x as usize].ty)),
                        (_, Type::Array(_))
                            | ("@push", Type::SmallArray(..))
                            | ("@tally" | "@tallyBytes", Type::Map(..))
                    ))
                    && core_global(body, *x).is_none_or(|g| self.cx.gappend.contains_key(g))
                    && self.core_args_readable(body, rest))
    }

    /// Whether `callee` is a declared function, whose arguments
    /// [`Fn_::core_user_call`] writes, place arguments included, and not a
    /// builtin, a validated type or a host import, whose readers take values.
    fn core_user_callee(&self, callee: &str, kind: Callee) -> bool {
        matches!(core_builtin(callee, kind), None | Some(Spec::Routes(_)))
            && self.core_named(callee, kind).is_none()
            && !(kind == Callee::Fn && self.is_extern(callee))
            && !callee.starts_with(vyrn_frontend::loader::MEM_PREFIX)
    }

    /// Whether a row removes from a receiver, a name or a place
    /// ([`Spec::Removes`]), with operands this walk writes, and if so whether
    /// what it hands back is an aggregate, which lands through a slot.
    fn core_removes(
        &self,
        body: &vyrn_lower::core::Body,
        callee: &str,
        kind: Callee,
        args: &[(Arg, vyrn_frontend::ast::Capability)],
    ) -> Option<bool> {
        if !matches!(core_builtin(callee, kind), Some(Spec::Removes)) {
            return None;
        }
        let [recv, rest @ ..] = args else {
            return None;
        };
        let (ty, readable) = match &recv.0 {
            Arg::Val(Val::Name(x)) => (
                body.names[*x as usize].ty.clone(),
                self.core_args_readable(body, std::slice::from_ref(recv)),
            ),
            Arg::Place(p) => (self.core_place_ty(body, p)?, true),
            Arg::Val(Val::Lit(_)) => return None,
        };
        let agg = match (callee, self.cx.resolve(&ty)) {
            ("@remove", Type::Map(..)) => false,
            ("@pop", Type::Array(_) | Type::SmallArray(..)) => true,
            ("@swapRemove", Type::Array(e) | Type::SmallArray(e, _)) => {
                matches!(self.cx.repr(&e, 0), Ok(Repr::Agg(_)))
            }
            _ => return None,
        };
        (readable
            && self.core_args_readable(body, rest)
            && rest.len() == usize::from(callee != "@pop"))
        .then_some(agg)
    }

    /// The receiver and the result of the rebuild before `ss[i]` when `ss[i]`
    /// is the store that puts the result back into that receiver, or into the
    /// place the receiver was taken from: one address, which
    /// [`Fn_::arr_rebuild`] has already written ([`Fn_::core_alias`]).
    /// Between the two stand the releases of the call's argument temporaries.
    fn core_rebuilt(
        &self,
        body: &vyrn_lower::core::Body,
        ss: &[St],
        i: usize,
    ) -> Option<(vyrn_lower::core::Name, vyrn_lower::core::Name)> {
        let k = drops_ahead(ss[..i].iter().rev());
        let (
            St::Let(t, rhs @ Rhs::Call { args, .. }),
            St::Store {
                place,
                value: Val::Name(v),
                ..
            },
        ) = (ss.get(i.checked_sub(k + 1)?)?, ss.get(i)?)
        else {
            return None;
        };
        let Some((Arg::Val(Val::Name(r)), _)) = args.first() else {
            return None;
        };
        let back = match place {
            vyrn_lower::core::Place::Name(x) => x == r,
            vyrn_lower::core::Place::Global(g) if core_global(body, *r) == Some(g.as_str()) => true,
            p => core_taken(body, *r) == Some(p),
        };
        (t == v && back && self.core_rebuild(body, rhs)).then_some((*r, *t))
    }

    /// Whether some store writes `x` whole other than the one that puts a
    /// rebuilt `x` back ([`Fn_::core_rebuilt`]).
    fn core_restored(&self, body: &vyrn_lower::core::Body, x: vyrn_lower::core::Name) -> bool {
        let mut found = false;
        each_list(&body.stmts, &mut |ss| {
            found |= (0..ss.len()).any(|i| {
                matches!(ss[i], St::Store { place: vyrn_lower::core::Place::Name(m), .. } if m == x)
                    && self.core_rebuilt(body, ss, i).is_none()
            });
        });
        found
    }

    /// Whether a rebuild in `ss` hands `n` back to the place it was taken
    /// from ([`Fn_::core_rebuilt`]).
    fn core_hands_back(
        &self,
        body: &vyrn_lower::core::Body,
        ss: &[St],
        n: vyrn_lower::core::Name,
    ) -> bool {
        (1..ss.len()).any(|i| {
            matches!(ss[i], St::Store { .. })
                && self.core_rebuilt(body, ss, i).is_some_and(|(r, _)| r == n)
        }) || ss.iter().any(|s| match s {
            St::If { then, els, .. } => {
                self.core_hands_back(body, then, n) || self.core_hands_back(body, els, n)
            }
            St::Loop { body: inner, .. } | St::Block { body: inner, .. } => {
                self.core_hands_back(body, inner, n)
            }
            St::Switch { arms, .. } => arms.iter().any(|a| self.core_hands_back(body, &a.body, n)),
            _ => false,
        })
    }

    /// Whether a layout of type `ty` is handed back with no instruction: its
    /// bits are the declared result's, as `Array<Int64>` is
    /// `Array<UiRouteInt>`'s of an alias, and a function value's are under
    /// another spelling of its type. The arm asks the same plan
    /// ([`crate::coerce_plan`]).
    fn core_returns_as_is(&self, ty: &Type) -> bool {
        matches!(
            crate::coerce_plan(&self.cx.sub(ty), &self.cx.sub(&self.ret_ty), &self.cx.types),
            crate::Rung::Identity | crate::Rung::FnRetag
        )
    }

    /// Whether the `let` at `ss[i]` binds the value the next `return` hands
    /// back, so the value is built in the caller's storage (RFC-0125 M7).
    ///
    /// A temporary, read once, by the `return` after it with nothing but
    /// releases of other names between, at the declared result's own type,
    /// is built into `dest`.
    fn core_lands(
        &self,
        body: &vyrn_lower::core::Body,
        ss: &[St],
        i: usize,
        reads: &[u32],
    ) -> bool {
        let St::Let(n, _) = &ss[i] else {
            return false;
        };
        let info = &body.names[*n as usize];
        self.dest.is_some()
            && info.binding.is_none()
            && reads[*n as usize] == 1
            && self.core_returns_as_is(&info.ty)
            && matches!(ss[i + 1..].iter().find(|s| {
                    !matches!(s, St::Row { .. }) && !matches!(s, St::Drop(d, ..) if d != n)
                }),
                Some(St::Return { value: Some(Val::Name(r)), .. }) if r == n)
    }

    /// The first name the statement `s` reads, which is the only one the
    /// operand stack can be carrying for it. None for an aggregate call, a
    /// variant and a store into a place, whose destination goes on the stack
    /// before their parts, for `@codeSplice`, whose tag goes before its
    /// value, and for a call through a stored value, whose value goes there
    /// first.
    fn core_first_read(
        &self,
        body: &vyrn_lower::core::Body,
        s: Option<&St>,
    ) -> Option<vyrn_lower::core::Name> {
        match s? {
            St::Let(_, Rhs::Call { callee, kind, .. })
            | St::Do {
                rhs: Rhs::Call { callee, kind, .. },
                ..
            } if self.core_through(body, callee, *kind).is_some() => None,
            St::Let(_, rhs)
                if self.core_ctor(body, rhs)
                    || self.core_agg_call(body, rhs)
                    || self.core_rebuild(body, rhs)
                    || matches!(rhs, Rhs::Call { callee, kind, args, .. }
                        if self.core_removes(body, callee, *kind, args).is_some()
                            || callee == "@codeSplice") =>
            {
                None
            }
            St::Store { place, .. }
                if !matches!(place, vyrn_lower::core::Place::Name(n)
                    if self.core_framed(&body.names[*n as usize].ty)) =>
            {
                None
            }
            // A higher-order call pushes its arguments in its instance's
            // order ([`ho_args`]), which puts a target's captures where the
            // `fn` parameter stands.
            St::Let(
                _,
                Rhs::Call {
                    callee,
                    kind,
                    solved,
                    targets,
                    args,
                    ..
                },
            )
            | St::Do {
                rhs:
                    Rhs::Call {
                        callee,
                        kind,
                        solved,
                        targets,
                        args,
                        ..
                    },
                ..
            } if !targets.is_empty() => {
                let (f, ..) = self.core_ho(callee, *kind, solved, targets)?;
                let ordered = ho_args(f, targets, args)?;
                let (a, _) = ordered.first()?;
                match a.val()? {
                    Val::Name(n) => Some(*n),
                    Val::Lit(_) => None,
                }
            }
            s => first_read(s),
        }
    }

    /// Whether a call row's builtin is one [`Fn_::core_call`] emits: the
    /// arity the row states.
    fn core_builtin_readable(
        &self,
        body: &vyrn_lower::core::Body,
        callee: &str,
        kind: Callee,
        args: &[(Arg, vyrn_frontend::ast::Capability)],
    ) -> bool {
        match core_builtin(callee, kind) {
            Some(Spec::Typed(params, _)) => params.len() == args.len(),
            Some(Spec::OwnType) => matches!(args, [(Arg::Val(_), _)]),
            Some(Spec::Renders(_) | Spec::Effect(_)) => matches!(args, [_]),
            Some(Spec::Logs) => args.len() == logs_arity(callee),
            Some(Spec::Traps) => matches!(args, [_] | [_, (Arg::Val(Val::Lit(Lit::Str(_))), _)]),
            Some(Spec::Asserts) => args.len() == assert_arity(callee),
            // A lane index is an immediate, so the row carries it as a literal.
            Some(Spec::Lanes) => {
                !matches!(callee, "@lane" | "@replaceLane")
                    || arg_vals(args).is_some_and(|vs| core_lane(&vs, 1, 4).is_some())
            }
            Some(Spec::Host) => self.cx.gen.is_some(),
            // A removal that hands back a scalar leaves it on the stack; one
            // that hands back an aggregate is an aggregate call.
            Some(Spec::Removes) => self.core_removes(body, callee, kind, args) == Some(false),
            Some(Spec::Finds) => matches!(args, [(Arg::Val(Val::Name(x)), _), _]
                if matches!(self.cx.resolve(&body.names[*x as usize].ty), Type::Map(..))),
            // A rebuild is read together with the store after it
            // ([`Fn_::core_rebuilt`]), a built aggregate as an aggregate call
            // ([`Fn_::core_agg_call`]), and a routed builtin as the declared
            // call [`Fn_::core_sig`] answers for; none alone.
            Some(Spec::Rebuilds | Spec::Builds(_) | Spec::Routes(_) | Spec::Pulls) | None => false,
        }
    }

    /// Whether `l o r` is `Code + Code`, the host's concatenation
    /// ([`Fn_::host`]), which exists only while a generator runs.
    fn core_code_concat(&self, body: &vyrn_lower::core::Body, o: BinOp, l: &Val, r: &Val) -> bool {
        let is_code = |v: &Val| matches!(self.cx.resolve(&self.core_ty(body, v, &Type::Int)), Type::Named(n) if n == "Code");
        self.cx.gen.is_some() && o == BinOp::Add && is_code(l) && is_code(r)
    }

    /// Whether `l o r` is a String operator [`Fn_::str_bin`] or
    /// [`Fn_::str_match`] writes: a String on the left, and a String or, for
    /// `=~`, the pattern literal on the right.
    fn core_str_op(&self, body: &vyrn_lower::core::Body, o: BinOp, l: &Val, r: &Val) -> bool {
        let is_str = |v: &Val| self.cx.resolve(&self.core_ty(body, v, &Type::Int)) == Type::Str;
        is_str(l)
            && match o {
                BinOp::Match => matches!(r, Val::Lit(Lit::Str(_))),
                BinOp::Add
                | BinOp::Eq
                | BinOp::NotEq
                | BinOp::Lt
                | BinOp::LtEq
                | BinOp::Gt
                | BinOp::GtEq => is_str(r),
                _ => false,
            }
    }

    fn core_rhs_readable(&self, body: &vyrn_lower::core::Body, rhs: &Rhs) -> bool {
        match rhs {
            Rhs::Val(v) => self.core_val_readable(body, v),
            Rhs::Prim(Op::Closure(_), ..) => false,
            Rhs::Prim(Op::Bin(o), vs, _) if matches!(vs.as_slice(), [l, r] if self.core_str_op(body, *o, l, r)) => {
                vs.iter().all(|v| self.core_val_readable(body, v))
            }
            Rhs::Prim(Op::Bin(o), vs, _) if matches!(vs.as_slice(), [l, r] if self.core_code_concat(body, *o, l, r)) => {
                vs.iter().all(|v| self.core_val_readable(body, v))
            }
            Rhs::Prim(_, vs, _) => vs.iter().all(|v| self.core_operand(body, v)),
            // A handed-back receiver asks nothing extra of this walk: the
            // builder states the call and the store that puts the result back
            // as two rows, and each is read where it stands (RFC-0125 M7).
            Rhs::Call {
                callee,
                args,
                kind,
                solved,
                targets,
                ..
            } => {
                self.core_removes(body, callee, *kind, args) == Some(false)
                    || self.core_args_readable(body, args)
                        && (arg_vals(args).is_some() || self.core_user_callee(callee, *kind))
                        && (self.core_builtin_readable(body, callee, *kind, args)
                            || (*kind == Callee::Fn && self.is_extern(callee))
                            || (*kind == Callee::Fn && self.cx.skipped.contains(callee))
                            || self.core_named(callee, *kind).is_some()
                            || self.core_mem_ty(callee, args.len()).is_some()
                            || self
                                .core_sig(body, callee, *kind, solved, targets)
                                .is_some_and(|s| {
                                    // An aggregate result crosses through an out-pointer
                                    // the caller allocates, and this walk writes a plain
                                    // `call`: the frame it would need is the callee's
                                    // destination and not a name of this body.
                                    s.params.len() == args.len()
                                        && matches!(
                                            self.cx.repr(&s.ret_ty, 0),
                                            Ok(Repr::Scalar(_) | Repr::Unit)
                                        )
                                }))
            }
            // A place this walk addresses, whose value is one it loads: any
            // value in one wasm local, which a String's pointer is as much as
            // an `Int64` ([`Fn_::core_read`]). An aggregate read is refused by
            // the same clause that refuses an aggregate name. A take loads
            // the same value and leaves a hole, which the release rows of the
            // place it left carry.
            Rhs::Read(p) | Rhs::Take(p) => self
                .core_place_ty(body, p)
                .is_some_and(|t| self.core_framed(&t)),
            _ => false,
        }
    }

    /// Whether `v` is a value one of this walk's ARITHMETIC rows computes
    /// with — a different question from [`Fn_::core_val_readable`], which is
    /// whether the walk can write the value at all. A `where` type computes
    /// as its base. A vector is one wasm `v128`, and [`Fn_::bin_ins`] and
    /// [`Fn_::un_ins`] write its lane-wise operators (RFC-0083).
    ///
    /// A literal has no name and carries its type in its own variant: a
    /// `Lit::Str` is a String, which this walk applies no operation to. `"a" <
    /// "b"` reached `Op::Lt` with two of them and not one name for the other
    /// screen to refuse.
    fn core_operand(&self, body: &vyrn_lower::core::Body, v: &Val) -> bool {
        match v {
            Val::Name(n) => {
                let t = self.cx.resolve(&body.names[*n as usize].ty);
                core_scalar(&t)
                    || matches!(
                        t,
                        Type::F32x4 | Type::I32x4 | Type::F64x2 | Type::Mask32x4 | Type::Mask64x2
                    )
            }
            Val::Lit(l) => !matches!(l, Lit::Opaque(_) | Lit::Str(_)),
        }
    }
}

/// Give back the slots the row `s` took for its own work once it is written:
/// every slot above `mark`, the frame before the row, and above each slot the
/// row took for a name. A name's slot need not be the row's own name's: a part
/// written at its parent's offset takes the parent's slot in the part's row
/// ([`Fn_::core_part_dest`]). A row that holds rows of its own gives back
/// through each of them.
fn core_row_done(b: &mut Frame, w: &Walked, s: &St, mark: u32) {
    if matches!(
        s,
        St::If { .. } | St::Loop { .. } | St::Block { .. } | St::Switch { .. }
    ) {
        return;
    }
    let from = w
        .slot
        .iter()
        .flatten()
        .filter(|&&(at, _)| at >= mark)
        .fold(mark, |top, &(_, end)| top.max(end));
    if from < b.mark() {
        b.give_back(from, b.mark());
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
        } => args.first().and_then(|(a, _)| a.val()).and_then(name),
        St::Return { value: Some(v), .. } | St::If { cond: v, .. } => name(v),
        _ => None,
    }
}

/// What a field of type `ty` holds: a `lazy T` field holds its stored nullary
/// closure, `fn() -> T` (RFC-0085 M4a), which the core reads and calls.
fn thunk_of(ty: Type) -> Type {
    match ty {
        Type::Lazy(inner) => Type::Fn(Vec::new(), inner),
        ty => ty,
    }
}

/// The place the name `n` was taken from, when its one `let` is a take.
fn core_taken(
    body: &vyrn_lower::core::Body,
    n: vyrn_lower::core::Name,
) -> Option<&vyrn_lower::core::Place> {
    let mut lets = Vec::new();
    for s in &body.stmts {
        core_lets(s, &mut lets);
    }
    let mut at = lets.iter().filter(|(b, _)| *b == n);
    match (at.next(), at.next()) {
        (Some((_, Rhs::Take(p))), None) => Some(p),
        _ => None,
    }
}

/// Whether the take `n` moves on at the next row and nowhere else: the value
/// a store writes, or a `consume` argument no other argument's root shares.
/// The part moves from its field to its destination, as the arm's
/// `x = consume r.f` does, so the name needs no slot of its own. The take's
/// root is a local that the store does not write, so no row between the take
/// and the move writes the field.
fn core_moves_on(body: &vyrn_lower::core::Body, n: vyrn_lower::core::Name) -> bool {
    let Some((root, _)) = core_taken(body, n).and_then(vyrn_lower::kernel::root_of) else {
        return false;
    };
    let rooted = |p: &vyrn_lower::core::Place| {
        vyrn_lower::kernel::root_of(p).is_some_and(|(r, _)| r == root)
    };
    let mut found = false;
    each_list(&body.stmts, &mut |ss| {
        let Some(i) = ss
            .iter()
            .position(|s| matches!(s, St::Let(t, _) if *t == n))
        else {
            return;
        };
        found = match ss.get(i + 1) {
            Some(St::Store {
                place,
                value: Val::Name(v),
                ..
            }) => *v == n && !rooted(place),
            Some(St::Let(_, Rhs::Call { args, .. })) => {
                args.iter()
                    .any(|a| *a == (Arg::Val(Val::Name(n)), Capability::Consume))
                    && args.iter().all(|(a, _)| match a {
                        Arg::Val(v) => {
                            *v == Val::Name(n) || !matches!(v, Val::Name(m) if *m == root)
                        }
                        Arg::Place(p) => !rooted(p),
                    })
            }
            _ => false,
        };
    });
    found && body.occurrences()[n as usize] == 2
}

/// The `Value` variant `value(x)` boxes a resolved scalar type into.
fn value_scalar(t: &Type) -> Option<&'static str> {
    match t {
        Type::Int
        | Type::IntN {
            bits: 64,
            signed: true,
        } => Some("IntVal"),
        Type::Bool => Some("BoolVal"),
        Type::Str => Some("StrVal"),
        _ => None,
    }
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

/// `ss` and every list of rows inside it, each before the lists inside it.
fn each_list(ss: &[St], f: &mut dyn FnMut(&[St])) {
    f(ss);
    for s in ss {
        match s {
            St::If { then, els, .. } => {
                each_list(then, f);
                each_list(els, f);
            }
            St::Loop { body: inner, .. } | St::Block { body: inner, .. } => each_list(inner, f),
            St::Switch { arms, .. } => arms.iter().for_each(|a| each_list(&a.body, f)),
            _ => {}
        }
    }
}

/// The parts of the header `base` names, when it is a borrow a loop walks.
fn core_header(w: &Walked, base: &vyrn_lower::core::Place) -> Option<Walk> {
    match base {
        vyrn_lower::core::Place::Name(n) => w.walks.get(*n as usize)?.clone().or_else(|| {
            let (_, h) = w.over.iter().rev().find(|(r, _)| r == n)?;
            w.walks[*h as usize].clone()
        }),
        _ => None,
    }
}

/// The names a statement writes: the root of a store, `None`, and a `modify`
/// or `consume` argument, with its capability. [`Fn_::core_alias`] reads it.
///
/// A store into an element of an Array writes none: it lands in the buffer,
/// which a name and a copy of its header share, and it moves no header
/// ([`vyrn_lower::kernel::in_element`]).
fn core_written(
    names: &[vyrn_lower::core::NameInfo],
    s: &St,
    out: &mut Vec<(vyrn_lower::core::Name, Option<Capability>)>,
) {
    let args = |r: &Rhs, out: &mut Vec<(vyrn_lower::core::Name, Option<Capability>)>| {
        if let Rhs::Call {
            args, write_back, ..
        } = r
        {
            for (k, (a, c)) in args.iter().enumerate() {
                match (a, c) {
                    // A write-back receiver is taken and stored back by the
                    // row after, so it changes no owner: a modify.
                    (Arg::Val(Val::Name(n)), Capability::Consume) if k == 0 && *write_back => {
                        out.push((*n, Some(Capability::Modify)))
                    }
                    (Arg::Val(Val::Name(n)), Capability::Modify | Capability::Consume) => {
                        out.push((*n, Some(*c)))
                    }
                    (Arg::Place(p), _) => out.extend(
                        vyrn_lower::kernel::root_of(p).map(|(n, _)| (n, Some(Capability::Modify))),
                    ),
                    _ => {}
                }
            }
        }
    };
    match s {
        St::Let(_, r) | St::Do { rhs: r, .. } => args(r, out),
        St::Store { place, .. } => out.extend(
            vyrn_lower::kernel::root_of(place)
                .filter(|(n, path)| {
                    !(vyrn_lower::kernel::in_element(path)
                        && matches!(names[*n as usize].ty, Type::Array(_)))
                })
                .map(|(n, _)| (n, None)),
        ),
        St::If { then, els, .. } => {
            then.iter().for_each(|s| core_written(names, s, out));
            els.iter().for_each(|s| core_written(names, s, out));
        }
        St::Loop { body: inner, .. } | St::Block { body: inner, .. } => {
            inner.iter().for_each(|s| core_written(names, s, out));
        }
        St::Switch { arms, .. } => {
            for a in arms {
                a.body.iter().for_each(|s| core_written(names, s, out));
            }
        }
        _ => {}
    }
}

/// The rows that run after the `let` of `n`: its later siblings and those of
/// every statement around it. `None` where no list binds `n`. A loop's
/// earlier rows run again only after `n`'s extent ends, since the `let` in
/// the loop binds `n` anew each turn.
fn core_after(ss: &[St], n: vyrn_lower::core::Name) -> Option<Vec<&St>> {
    ss.iter().enumerate().find_map(|(i, s)| {
        let mut after = match s {
            St::Let(b, _) if *b == n => Vec::new(),
            St::Loop { body, .. } | St::Block { body, .. } => core_after(body, n)?,
            St::If { then, els, .. } => core_after(then, n).or_else(|| core_after(els, n))?,
            St::Switch { arms, .. } => arms.iter().find_map(|a| core_after(&a.body, n))?,
            _ => return None,
        };
        after.extend(&ss[i + 1..]);
        Some(after)
    })
}

/// The rows of `ss`, or of a list inside it, from the `let` of `n` to the row
/// its extent ends at ([`vyrn_lower::core::extent_ends`]). `None` where no
/// list both binds `n` and holds every row that names it.
fn core_extent<'r>(ss: &'r [St], n: vyrn_lower::core::Name, occurs: &[u32]) -> Option<&'r [St]> {
    if let Some(at) = ss
        .iter()
        .position(|s| matches!(s, St::Let(m, _) if *m == n))
    {
        let ends = vyrn_lower::core::extent_ends(ss, occurs);
        let end = ends.iter().position(|e| e.contains(&n))?;
        return Some(&ss[at..=end]);
    }
    ss.iter().find_map(|s| match s {
        St::If { then, els, .. } => {
            core_extent(then, n, occurs).or_else(|| core_extent(els, n, occurs))
        }
        St::Loop { body, .. } | St::Block { body, .. } => core_extent(body, n, occurs),
        St::Switch { arms, .. } => arms.iter().find_map(|a| core_extent(&a.body, n, occurs)),
        _ => None,
    })
}

/// The signature of one instance of the generic `f`: its parameters and its
/// result under `subst`, with no type parameters and no body.
fn instance_shell(f: &Function, subst: &HashMap<String, Type>) -> Function {
    let mut sf = shell_of(f);
    for p in &f.params {
        sf.params.push(Param {
            name: p.name.clone(),
            capability: p.capability,
            ty: ftypes::substitute(&p.ty, subst),
            line: p.line,
            col: p.col,
        });
    }
    sf.ret = ftypes::substitute(&f.ret, subst);
    sf
}

/// The instance of `f` a call row names by the checker's solution (the row's
/// `solved`): the type arguments in `f`'s own order, and the substitution.
/// `None` where a type parameter is unsolved or still names a parameter; the
/// walk solves nothing itself.
fn solved_instance(
    f: &Function,
    solved: &[(String, Type)],
) -> Option<(Vec<Type>, HashMap<String, Type>)> {
    let subst: HashMap<String, Type> = solved.iter().cloned().collect();
    let targs: Vec<Type> = (f.type_params.iter())
        .map(|p| subst.get(p).cloned())
        .collect::<Option<_>>()?;
    (!targs.iter().any(vyrn_frontend::types::mentions_param)).then_some((targs, subst))
}

/// The signature of `f`'s specialization per `targets` (RFC-0023), and what
/// each `fn` parameter is bound to inside it. An ordinary parameter keeps its
/// place, and a `fn` parameter becomes its target's captures at that same
/// place, so a wasm argument is evaluated where the interpreter evaluates it.
/// A capture parameter's name holds an `@`, which no Vyrn identifier can, so
/// nothing the body names shadows it.
fn ho_shell(
    f: &Function,
    subst: &HashMap<String, Type>,
    targets: &[FnTarget],
) -> (Function, HashMap<String, FnBinding>) {
    let mut sf = shell_of(f);
    let mut binds = HashMap::new();
    let mut bound = targets.iter();
    for p in &f.params {
        if !matches!(p.ty, Type::Fn(..)) {
            sf.params.push(Param {
                name: p.name.clone(),
                capability: p.capability,
                ty: ftypes::substitute(&p.ty, subst),
                line: p.line,
                col: p.col,
            });
            continue;
        }
        let Some(target) = bound.next() else { break };
        let mut cap_srcs = Vec::new();
        for t in &target.sig.params[..target.ncaps] {
            let n = format!("@cap{}", sf.params.len());
            sf.params.push(Param {
                name: n.clone(),
                capability: Capability::Read,
                ty: t.clone(),
                line: 0,
                col: 0,
            });
            cap_srcs.push(n);
        }
        binds.insert(
            p.name.clone(),
            FnBinding {
                target: target.clone(),
                cap_srcs,
            },
        );
    }
    sf.ret = ftypes::substitute(&f.ret, subst);
    (sf, binds)
}

/// The names a run's switches account for themselves — RFC-0125 M7: every
/// payload binder, and each scrutinee where `ons`.
///
/// A payload binder is a place [`Fn_::core_switch`] binds when it enters the
/// arm, and a scrutinee is read as an ADDRESS rather than as a value. So the
/// screen's place clause is asked about neither.
fn core_switched(s: &St, ons: bool, out: &mut Vec<vyrn_lower::core::Name>) {
    match s {
        St::Switch { on, arms, .. } => {
            if let (Val::Name(n), true) = (on, ons) {
                out.push(*n);
            }
            for a in arms {
                out.extend(a.binds.iter().copied());
                a.body.iter().for_each(|s| core_switched(s, ons, out));
            }
        }
        St::If { then, els, .. } => {
            then.iter().for_each(|s| core_switched(s, ons, out));
            els.iter().for_each(|s| core_switched(s, ons, out));
        }
        St::Loop { body: inner, .. } | St::Block { body: inner, .. } => {
            inner.iter().for_each(|s| core_switched(s, ons, out));
        }
        _ => {}
    }
}

/// `rel`, walking around the holes a core row names. The row spells a hole
/// with the leading dot a path has, and a walk names the field.
fn around(rel: Rel, holes: &[String]) -> Rel {
    match rel {
        Rel::Deep(ty, _) => Rel::Deep(
            ty,
            holes
                .iter()
                .filter_map(|h| h.strip_prefix('.').map(str::to_string))
                .collect(),
        ),
        rel => rel,
    }
}

/// The exits this walk gives back at itself — RFC-0125 §3 M3, the release
/// slice. A `break`, a `continue` and a `return` each carry the node the plan
/// keys their releases by, so the walk asks [`Fn_::emit_releases`] for the
/// group there. A scrutinee's joins them with the tag family (RFC-0125 M7):
/// the core states it as a [`St::Row`] after the switch, keyed by the
/// construct, and [`Fn_::core_release`] emits it where the row stands. A
/// block's fall-through release is keyed by the block, which no statement of
/// a run names.
const CORE_EXITS: [ExitKind; 4] = [
    ExitKind::Break,
    ExitKind::Continue,
    ExitKind::Return,
    ExitKind::Scrutinee,
];

/// The row a run states its statement with: the last one, but for the
/// releases of its temporaries after it ([`vyrn_lower::core::Body::rows_by_statement`]).
/// A release this pass placed has line 0; a `drop` the reader wrote is its
/// statement's own row.
fn core_head(run: &[St]) -> Option<&St> {
    run.iter()
        .rev()
        .find(|r| !matches!(r, St::Drop(_, _, 0, _)))
}

/// Whether a run leaves the FUNCTION anywhere under it — the exit clause of
/// [`Fn_::core_run`]'s screen, which a subtree carries for every branch.
fn core_returns(s: &St) -> bool {
    let mut out = false;
    core_leaf_rows(s, &mut |r| out |= matches!(r, St::Return { .. }));
    out
}

/// Every row under `s` that holds no rows of its own, `s` itself included,
/// in row order.
fn core_leaf_rows<'r>(s: &'r St, f: &mut dyn FnMut(&'r St)) {
    match s {
        St::If { then, els, .. } => then.iter().chain(els).for_each(|s| core_leaf_rows(s, f)),
        St::Loop { body: inner, .. } | St::Block { body: inner, .. } => {
            inner.iter().for_each(|s| core_leaf_rows(s, f))
        }
        St::Switch { arms, .. } => arms
            .iter()
            .flat_map(|a| &a.body)
            .for_each(|s| core_leaf_rows(s, f)),
        _ => f(s),
    }
}

/// Whether a run leaves the list it stands in anywhere under it: a `return`,
/// or a `break` or a `continue` outside a loop of its own.
fn core_leaves(s: &St) -> bool {
    match s {
        St::Break { .. } | St::Continue { .. } => true,
        St::If { then, els, .. } => then.iter().chain(els).any(core_leaves),
        St::Block { body: inner, .. } => inner.iter().any(core_leaves),
        St::Switch { arms, .. } => arms.iter().any(|a| a.body.iter().any(core_leaves)),
        s => core_returns(s),
    }
}

/// A type this walk reads a name of: a scalar that needs no validation. A
/// `where` type (RFC-0079) is a `check` row the core does not carry.
/// Push the address of the payload at `off` in the sum at `addr`: inside the
/// sum where the payload is two words `inline`, and the box's otherwise.
fn payload_at(b: &mut Frame, addr: u32, off: u32, inline: bool) {
    b.ins(&Instruction::LocalGet(addr));
    if inline {
        b.ins(&Instruction::I32Const(off as i32));
        b.ins(&Instruction::I32Add);
    } else {
        b.ins(&Instruction::I64Load(at(off)));
        b.ins(&Instruction::I32WrapI64);
    }
}

/// How many releases lead `ss`: the argument temporaries the builder releases
/// between a rebuild and its store ([`Fn_::core_rebuilt`]).
fn drops_ahead<'s>(ss: impl Iterator<Item = &'s St>) -> usize {
    ss.take_while(|s| matches!(s, St::Drop(..))).count()
}

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
    /// CLI does, with the core installed, and not through `vyrn_frontend::check`.
    fn linked(src: &str) -> Result<Program, String> {
        vyrn_lower::install();
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

    /// The two halves of a specified builtin are keyed by the same name: the
    /// core's row states the types and [`builtin_spec`] states the
    /// instruction, so a row with no instruction would be a call the gap
    /// screen promises this walk reads and it cannot.
    #[test]
    fn builtin_rows_all_emit() {
        for (name, spec) in vyrn_lower::core::builtin_rows() {
            let Spec::Typed(params, _) = spec else {
                continue;
            };
            assert!(
                builtin_spec(name, params.len()).is_some(),
                "`{name}` has a row and no instruction"
            );
        }
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
            kept: RefCell::new(Vec::new()),
            impls: Vec::new(),
            sigs: HashMap::new(),
            gen: None,
            generics: HashMap::new(),
            higher_order: HashMap::new(),
            skipped: std::collections::HashSet::new(),
            owned: Default::default(),
            subst: HashMap::new(),
            mono: RefCell::new(Mono::default()),
            fnvals: RefCell::new(Vec::new()),
            fnval_copy: 0,
            fnval_free: 0,
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
        ];
        for (what, src) in cases {
            let p = linked(src).expect(what);
            assert!(compile(&p).is_ok(), "{what}: {}", compile(&p).unwrap_err());
        }
    }
}
