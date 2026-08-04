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
use vyrn_frontend::types as ftypes;
use vyrn_frontend::types::INT32;

use crate::layout::{self, Layout};
use crate::llt_of;
use crate::wasm::{self, BlockType, Frame, Instruction, MemArg, Module, ValType, HEAP};

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
/// reaches AFTER the bodies exist, so the thirteen here cost a program only what
/// it calls (`fib.wasm` imports two). Which is why `path_rename` could be added at
/// all — M2o refused it as a thirteenth UNCONDITIONAL import, renumbering every
/// module in the corpus.
///
/// The set is implemented twice over: wasmtime provides all of preview1, and
/// `web/wasi-min.js` implements exactly these for the browser — with RFC-0014's
/// graceful degradation (no argv, EOF on stdin, no preopens, every `path_open`
/// NOENT), which is what a page's `readFile` is supposed to be.
#[derive(Clone, Copy)]
struct Wasi {
    fd_write: u32,
    fd_read: u32,
    fd_close: u32,
    proc_exit: u32,
    path_open: u32,
    path_rename: u32,
    fd_prestat_get: u32,
    args_sizes_get: u32,
    args_get: u32,
    environ_sizes_get: u32,
    environ_get: u32,
    clock_time_get: u32,
    random_get: u32,
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
        fd_prestat_get: im("fd_prestat_get", &[I32, I32], &[I32]),
        args_sizes_get: im("args_sizes_get", &[I32, I32], &[I32]),
        args_get: im("args_get", &[I32, I32], &[I32]),
        environ_sizes_get: im("environ_sizes_get", &[I32, I32], &[I32]),
        environ_get: im("environ_get", &[I32, I32], &[I32]),
        clock_time_get: im("clock_time_get", &[I32, I64, I32], &[I32]),
        random_get: im("random_get", &[I32, I32], &[I32]),
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
    (params, wasm::abi(crate::extern_abi_ll(&f.ret)).into_iter().collect())
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

/// Compile `program` to run as a GENERATOR under RFC-0076's engine: the same
/// traversal, plus the `vyrn_gen` host imports and the two lowerings that only
/// make sense with them (`listDir`, and `Code` as an opaque `i64` handle).
///
/// This is RFC-0076 M7, and it closes RFC-0077's own opening complaint. The
/// generation engine reached the wasm target through `emit_gen_host` and clang, so
/// `find_clang() == None` made it DECLINE — a `.vyx` keystroke was 54 ms or 250 ms
/// depending on whether someone had installed a C toolchain, which is the shape
/// RFC-0077 exists to remove and which M5 removed for `vyrn build` only.
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

    let rt = runtime(&mut m, &wasi, gen.as_ref());

    let types: HashMap<String, TypeDecl> =
        program.type_decls.iter().map(|t| (t.name.clone(), t.clone())).collect();
    let mut variants: HashMap<String, Vec<(String, u64, Vec<Type>)>> = HashMap::new();
    for d in &program.type_decls {
        if let Type::Enum(vs) = &d.base {
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
    // shell would fail the whole build over a function nothing calls.
    let mut generics: HashMap<String, Function> = HashMap::new();
    let mut higher_order: HashMap<String, Function> = HashMap::new();
    let mut user: Vec<&Function> = Vec::new();
    for f in &program.functions {
        // An `extern` is an import (declared above); a `gen fn` (RFC-0021) runs
        // only in the compiler's own interpreter and may use builtins with no
        // lowering at all.
        if f.is_extern || f.is_gen {
            continue;
        }
        // RFC-0023: a function taking a `fn`-typed parameter exists only as
        // higher-order specializations, one per set of resolved targets. The shell
        // has no first-order definition to emit — a `fn` parameter is not a value
        // in the lowered code at all.
        if f.params.iter().any(|p| matches!(p.ty, Type::Fn(..))) {
            higher_order.insert(f.name.clone(), f.clone());
            continue;
        }
        if !f.type_params.is_empty() {
            generics.insert(f.name.clone(), f.clone());
            continue;
        }
        user.push(f);
    }
    let protocol_methods: HashMap<String, String> = program
        .protocols
        .iter()
        .flat_map(|p| p.methods.iter().map(|m| (m.name.clone(), p.name.clone())))
        .collect();

    let mut cx = Cx {
        types,
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
        dispatch: RefCell::new(Dispatch::default()),
        globals: HashMap::new(),
        externs,
        droppable: vyrn_frontend::own::analyze(program).droppable,
        log_level: program.log_level,
        log_sink: program.log_sink.clone(),
        // Reserved only for a file sink, so every console-sink module — which is
        // every example — is byte-for-byte what it was.
        log_fd: matches!(program.log_sink, LogSink::File(_)).then(|| m.reserve(4, 4)),
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
        cx.sigs.insert(f.name.clone(), Sig { index: m.reserve_func(&wp, &wr), ..s });
    }

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
        cx.globals.insert(g.name.clone(), (Place::Static(m.reserve(l.size, l.align)), ty));
    }

    // The initializer's index, reserved like every other so nothing depends on
    // where in the sequence it lands.
    let has_globals = !program.globals.is_empty();
    let init_index = m.reserve_func(&[], &[]);

    for f in &user {
        let sig = cx.sigs[&f.name].clone();
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

    // Drain what the bodies discovered, and then the dispatchers the drain
    // discovered, until neither has anything left. One body may discover more of
    // either — a generic instance calling a generic, an RFC-0023 instance calling
    // either, a lifted lambda doing any of it, any of them calling a stored `fn`
    // value — so this reads both lists afresh every turn rather than iterating a
    // snapshot. That is what "the worklists feed each other" is here: appending to
    // the list being read.
    loop {
        let p = {
            let mono = cx.mono.borrow();
            mono.insts.get(mono.done).cloned()
        };
        if let Some(p) = p {
            cx.subst = p.subst.clone();
            let body = lower_body(&mut m, &p.f, &p.sig, &cx, p.binds.clone())?;
            cx.subst = HashMap::new();
            m.fill(p.sig.index, body);
            cx.mono.borrow_mut().done += 1;
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
        let LogSink::File(path) = &cx.log_sink else { unreachable!("log_fd implies a file sink") };
        (at, cx.rt.intern(&mut m, path), cx.rt.open_at)
    });
    let start = m.func(&[], &[], &[], 0, |b| {
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
        if let Some((at, ..)) = log_open {
            b.ins(&Instruction::I32Const(at as i32))
                .ins(&Instruction::I32Load(word()))
                .ins(&Instruction::Call(wasi.fd_close))
                .ins(&Instruction::Drop);
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
    if user.iter().any(|f| {
        f.is_export_extern && f.params.iter().any(|p| matches!(p.ty, Type::Str))
    }) {
        m.export("__vyrn_malloc", cx.rt.malloc);
    }
    // Keep only what those exports reach (M2p). Everything above emits eagerly —
    // 39 runtime helpers, 12 WASI imports, every function of every linked module —
    // because nothing knows what a program reaches until its bodies are walked.
    // This is where that is known.
    m.sweep();
    Ok(m.finish())
}

// ---------------------------------------------------------------------------
// Types
// ---------------------------------------------------------------------------

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

/// One body discovered while another was being emitted, with the function index
/// it was promised.
#[derive(Clone)]
struct Pending {
    key: Key,
    /// The body to lower. An `Rc` rather than a clone per drain turn, because
    /// [`Key::Lambda`] keys on a node address inside it and a fresh deep clone
    /// every turn would move the addresses of anything nested.
    f: Rc<Function>,
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
struct Mono {
    insts: Vec<Pending>,
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

struct Cx {
    types: HashMap<String, TypeDecl>,
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
    generics: HashMap<String, Function>,
    /// Functions with a `fn`-typed parameter (RFC-0023). Like a generic they have
    /// no index and no body of their own — only specializations do — so a call to
    /// one is a discovery, and the shell is skipped exactly as the textual driver
    /// skips it.
    higher_order: HashMap<String, Function>,
    /// Protocol method name → its protocol (RFC-0002 §5). A bounded generic is
    /// what protocols are for, so `x.show()` inside one has to resolve.
    protocol_methods: HashMap<String, String>,
    /// The monomorphization whose body is being lowered; empty for an ordinary
    /// function.
    subst: HashMap<String, Type>,
    mono: RefCell<Mono>,
    /// RFC-0037's variant registry, module-global so a tag means the same thing in
    /// every body that builds one.
    fnvals: RefCell<Vec<FnVal>>,
    dispatch: RefCell<Dispatch>,
    /// Module state (RFC-0013): name → its fixed address and declared type. Every
    /// body sees all of them, which is the textual backend's `globals` fallback in
    /// [`Gen::lookup`] — the checker already forbids an initializer reading a
    /// global declared after it, so there is nothing for a partial view to catch.
    globals: HashMap<String, (Place, Type)>,
    /// `extern fn` declarations by name (RFC-0012): the `vyrn` import's index and
    /// the signature the ABI is read off. There is no body to lower — a call is the
    /// only thing that crosses — and what one returns is the declaration's business
    /// rather than this file's, which is also how RFC-0043's host boundary is
    /// reached by name.
    externs: HashMap<String, Ext>,
    /// Per function, which `let` statements own a heap value and how to reclaim
    /// it — `vyrn_frontend::own`'s answer, keyed by statement node address: the
    /// same map, read with the same key, as the textual backend's `droppable`.
    ///
    /// Only `ReleaseRef` is acted on. Everything else it reports is a `free`, and
    /// a bump allocator has no free — but a cell's SLOT comes out of a fixed slab
    /// of 65536, so a release that never fires IS observable (M2l).
    droppable: HashMap<String, HashMap<usize, DropKind>>,
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
}

impl Cx {
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
        subst: HashMap<String, Type>,
        binds: HashMap<String, FnBinding>,
    ) -> Result<Sig, String> {
        if let Some(p) = self.mono.borrow().insts.iter().find(|p| p.key == key) {
            return Ok(p.sig.clone());
        }
        let s = self.signature(&f)?;
        let (wp, wr) = self.wasm_sig(&s, f.line)?;
        let sig = Sig { index: m.reserve_func(&wp, &wr), ..s };
        let mut mono = self.mono.borrow_mut();
        mono.insts.push(Pending { key, f, sig: sig.clone(), subst, binds });
        Ok(sig)
    }

    /// A generic instantiation (M2e): [`Cx::enqueue`] with no `fn` parameters.
    fn instantiate(
        &self,
        m: &mut Module,
        f: &Function,
        type_args: Vec<Type>,
        subst: HashMap<String, Type>,
    ) -> Result<Sig, String> {
        let mut sf = f.clone();
        sf.type_params.clear();
        for p in &mut sf.params {
            p.ty = ftypes::substitute(&p.ty, &subst);
        }
        sf.ret = ftypes::substitute(&f.ret, &subst);
        self.enqueue(
            m,
            Key::Generic(f.name.clone(), type_args),
            Rc::new(sf),
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
            "<4 x float>" | "<4 x i32>" => Repr::Scalar(ValType::V128),
            _ if ll.starts_with('{') || ll.starts_with('[') => Repr::Agg(
                layout::of_ll(&ll).map_err(|e| format!("direct backend: layout of {ll}: {e}"))?,
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
            Type::Option(i) | Type::Array(i) | Type::Ref(i) | Type::ArrayN(i, _) => {
                self.ty_gap(&i, depth + 1)
            }
            Type::Result(a, b) | Type::Map(a, b) => {
                self.ty_gap(&a, depth + 1).or_else(|| self.ty_gap(&b, depth + 1))
            }
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
            modify: f.params.iter().map(|p| p.capability == Capability::Modify).collect(),
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
#[derive(Clone, Copy)]
enum Place {
    Local(u32),
    Slot(u32),
    Static(u32),
}

/// A stream's step signature (RFC-0075 M2b), which is a function of the ELEMENT
/// type and nothing else — the cursor is a `Ref<Int64>` precisely so that it is.
/// Both the construction site and the loop that dispatches through it derive the
/// signature from here, because a stored `fn` value is keyed by its signature and
/// two spellings of one type would be two dispatchers.
fn stream_step_sig(elem: &Type) -> Type {
    Type::Fn(
        vec![Type::Ref(Box::new(Type::Int))],
        Box::new(Type::Option(Box::new(elem.clone()))),
    )
}

/// What a release frame entry reclaims (RFC-0075 M2b added the second one).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
enum Rel {
    /// A `Ref<T>` binding — check the generation, hand the slot back.
    Cell,
    /// A `Stream<T>` — nothing if it is a buffer (this backend's allocator never
    /// frees), its cursor cell if it is a producer.
    Stream,
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

/// An empty `Function` to fill in for a lifted lambda (RFC-0023).
///
/// A synthesized declaration rather than a bespoke lowering path: the captures
/// become ordinary read parameters, so [`lower_fn`] emits it with no case of its
/// own. The name is a reserved spelling no Vyrn identifier can be, which matters
/// once — `Cx::droppable` is keyed by function name, and a lifted body must not
/// inherit the enclosing function's release list (the textual backend takes the
/// same map away for the same reason).
fn f_shell(line: usize) -> Function {
    Function {
        name: "@lambda".to_string(),
        exported: false,
        module: None,
        doc: None,
        type_params: Vec::new(),
        type_bounds: HashMap::new(),
        params: Vec::new(),
        ret: Type::Unit,
        body: Block { stmts: Vec::new() },
        line,
        is_extern: false,
        is_export_extern: false,
        is_gen: false,
        is_mut: false,
    }
}

/// One function being lowered.
struct Fn_<'a> {
    cx: &'a Cx,
    /// Name → where it lives and what it is. A scope stack rather than a map per
    /// block: shadowing pushes, and leaving a block truncates.
    scope: Vec<(String, Place, Type)>,
    /// wasm blocks open between here and the function's outermost one. A
    /// `return` is `br depth`.
    depth: u32,
    /// (break target, continue target, release boundary, region depth) per
    /// enclosing loop. The first two are the depth each was opened at, so `br`
    /// distance is `depth - opened - 1`; the third is how many release frames were
    /// open when the loop started, which is what a `break` has to unwind to; the
    /// fourth is the same question for `region` blocks, which are the other kind of
    /// scope an exit edge has to close (RFC-0004 §4).
    loops: Vec<(u32, u32, usize, u32)>,
    ret: Repr,
    ret_ty: Type,
    /// The wasm local holding the hidden aggregate-return pointer, if any.
    dest: Option<u32>,
    /// Reusable scratch, taken on first use. Every use is a set immediately
    /// followed by the reads that consume it, so one pair suffices however
    /// deeply expressions nest.
    scratch: HashMap<(ValType, u8), u32>,
    /// This function's releasable bindings, one frame per open block, released on
    /// the block's exit — innermost first, newest first, the order the textual
    /// backend's `drop_stack` uses. A `Ref` releases its cell; a `Stream`
    /// (RFC-0075 M2b) releases the cursor cell it owns IF it holds a producer
    /// rather than a buffer, which is a runtime tag rather than a static fact.
    releases: Vec<Vec<(Place, Rel)>>,
    /// Lexical `region` nesting depth within this body, so an exit edge knows how
    /// many arena scopes it is leaving. The runtime counter is dynamic (a callee's
    /// region nests inside its caller's); this is only the part one body can see,
    /// which is exactly the part its own `br`s unwind past.
    region_depth: u32,
    /// [`Cx::droppable`] for the function being lowered.
    drops: HashMap<usize, DropKind>,
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
    /// wasm local holding the accumulator's pointer → the frame slot holding its
    /// `(len, cap)` shadow. Keyed by local index rather than by name because the
    /// local IS the binding: two `let out`s in one body are two accumulators, and
    /// a global (a `Place::Static`) never gets an entry at all.
    str_append: HashMap<u32, u32>,
}

/// A lowering context with nothing in scope and nothing to return to: what the
/// globals initializer is, and what typing an initializer outside any function
/// needs. Module state itself is still visible, because it lives in [`Cx`].
fn top_level<'a>(cx: &'a Cx) -> Fn_<'a> {
    Fn_ {
        cx,
        scope: Vec::new(),
        depth: 0,
        loops: Vec::new(),
        ret: Repr::Unit,
        ret_ty: Type::Unit,
        dest: None,
        scratch: HashMap::new(),
        releases: Vec::new(),
        region_depth: 0,
        drops: HashMap::new(),
        expect: Vec::new(),
        fn_binds: HashMap::new(),
        append_ok: std::collections::HashSet::new(),
        str_append: HashMap::new(),
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
fn lower_globals_init(m: &mut Module, program: &Program, cx: &Cx) -> Result<Frame, String> {
    let mut b = Frame::new(0, &[], 0);
    let mut f = top_level(cx);
    for g in &program.globals {
        let (place, ty) = cx.globals[&g.name].clone();
        let r = cx.repr(&ty, g.line)?;
        f.store_into(m, &mut b, place, &r, &g.init, &ty)?;
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
    cx: &Cx,
    binds: HashMap<String, FnBinding>,
) -> Result<(), String> {
    let body = lower_body(m, f, sig, cx, binds)?;
    m.fill(sig.index, body);
    Ok(())
}

/// The body itself, before it is installed at the index reserved for it.
fn lower_body(
    m: &mut Module,
    f: &Function,
    sig: &Sig,
    cx: &Cx,
    binds: HashMap<String, FnBinding>,
) -> Result<Frame, String> {
    let sig = sig.clone();
    let (params, _results) = cx.wasm_sig(&sig, f.line)?;
    let dest = sig.ret.agg().map(|_| 0u32);
    let shift = dest.map_or(0, |_| 1);

    let mut b = Frame::new(params.len(), &[], 0);
    let mut cx_fn = Fn_ {
        cx,
        scope: Vec::new(),
        depth: 0,
        loops: Vec::new(),
        ret: sig.ret.clone(),
        // As DECLARED, not resolved. A function returning `Age` has to validate
        // at its `return`, and `Age` resolved to `Int64` is the flow that does
        // not — which is the whole class of silent hole M2d exists to close.
        ret_ty: sig.ret_ty.clone(),
        dest,
        scratch: HashMap::new(),
        releases: Vec::new(),
        region_depth: 0,
        drops: cx.droppable.get(&f.name).cloned().unwrap_or_default(),
        expect: Vec::new(),
        fn_binds: binds,
        append_ok: crate::append_candidates(&f.body),
        str_append: HashMap::new(),
    };

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
                    b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
                    b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                    Place::Slot(off)
                }
                _ => Place::Local(local),
            }
        };
        cx_fn.scope.push((p.name.clone(), place, ty));
    }

    // The one block every `return` targets. Its result IS the function's when
    // that is a scalar; an aggregate return travels through `dest` instead, so
    // the block carries nothing.
    b.ins(&Instruction::Block(match &sig.ret {
        Repr::Scalar(v) => BlockType::Result(*v),
        _ => BlockType::Empty,
    }));
    cx_fn.block(m, &mut b, &f.body)?;
    // Falling off the end of a value-returning function is unreachable — the
    // checker proves every path returns — but the validator needs to be told,
    // since it cannot see the proof.
    if matches!(sig.ret, Repr::Scalar(_)) {
        b.ins(&Instruction::Unreachable);
    }
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
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
            }
            _ => return unsupported("a `modify` parameter of this shape", f.line),
        }
    }

    Ok(b)
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
fn lower_dispatcher(
    m: &mut Module,
    cx: &Cx,
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
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                Place::Slot(off)
            }
            Repr::Scalar(_) => Place::Local(local),
            Repr::Unit => return unsupported("a Unit parameter of a stored `fn`", 0),
        };
        f.scope.push((format!("@a{i}"), place, pty.clone()));
    }
    let args: Vec<Expr> =
        (0..ptys.len()).map(|i| Expr::Var { name: format!("@a{i}"), line: 0 }).collect();

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
            b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
                all.push(Expr::Var { name: format!("@c{ci}"), line: 0 });
            }
        }
        all.extend(args.iter().cloned());
        // An aggregate result is written through OUR destination, so it goes on the
        // stack under the call — M2d's "a value may sit beneath an `if`" applied to
        // the call rather than to a check.
        if let Some(d) = dest {
            b.ins(&Instruction::LocalGet(d));
        }
        let got = f.emit_call(m, &mut b, &v.target.sig, &all)?;
        match (&dsig.ret, cx.repr(&got, 0)?) {
            // The target's declared result may differ from the signature's — a
            // named source's validated scalar, a wider record — so it crosses the
            // M2d seam like any other flow.
            (Repr::Scalar(_), _) => f.coerce(m, &mut b, None, &got, ret, 0)?,
            (Repr::Agg(l), Repr::Agg(_)) => {
                f.coerce(m, &mut b, None, &got, ret, 0)?;
                b.ins(&Instruction::I32Const(l.size as i32));
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
    let msg = cx.rt.intern(m, "error: internal: invalid function value\n");
    b.ins(&Instruction::I32Const(msg as i32));
    b.ins(&Instruction::Call(cx.rt.trap));
    b.ins(&Instruction::Unreachable);
    for _ in &variants {
        f.depth -= 1;
        b.ins(&Instruction::End);
    }
    Ok(b)
}

impl Fn_<'_> {
    /// Scratch local `n` of type `t`, taken on first use.
    ///
    /// Reusable because every use is a set immediately followed by the reads
    /// that consume it — a nested expression evaluates to completion before the
    /// outer one touches scratch, and anything already on the operand stack is
    /// untouched by a local.
    fn scratch(&mut self, b: &mut Frame, t: ValType, n: u8) -> u32 {
        *self.scratch.entry((t, n)).or_insert_with(|| b.local(t))
    }

    fn block(&mut self, m: &mut Module, b: &mut Frame, blk: &Block) -> Result<(), String> {
        let mark = self.scope.len();
        self.releases.push(Vec::new());
        for s in &blk.stmts {
            self.stmt(m, b, s)?;
        }
        // The fall-through exit. An early `return`/`break`/`continue` releases the
        // same frames before its branch, so this runs after a branch only in code
        // wasm has already marked unreachable.
        let boundary = self.releases.len() - 1;
        self.emit_releases_above(b, boundary)?;
        self.releases.pop();
        self.scope.truncate(mark);
        Ok(())
    }

    /// Release every cell owned by `self.releases[boundary..]`, innermost frame
    /// first and newest binding first, WITHOUT popping the frames.
    ///
    /// Not popping is what makes an early exit safe: the frames stay so the
    /// enclosing [`Fn_::block`] still emits its own copy, which lands after the
    /// branch and is therefore unreachable rather than a second release.
    fn emit_releases_above(&mut self, b: &mut Frame, boundary: usize) -> Result<(), String> {
        let frames: Vec<Vec<(Place, Rel)>> =
            self.releases[boundary..].iter().rev().cloned().collect();
        for frame in frames {
            for (p, k) in frame.into_iter().rev() {
                match k {
                    Rel::Cell => self.emit_release(b, p, 0)?,
                    Rel::Stream => self.stream_release(b, p, 0)?,
                }
            }
        }
        Ok(())
    }

    /// Push a region scope: trap if this would be the 65th, else bump the counter.
    ///
    /// The bound is the LLVM prelude's fixed 64-slot region stack and the
    /// interpreter's own `region_depth >= 64`, so all three engines refuse the same
    /// nesting with the same words. Inline rather than a runtime helper: it is
    /// fourteen instructions at a handful of sites, and a helper would be a
    /// thirty-sixth index in a table whose numbering is load-bearing.
    fn region_enter(&mut self, b: &mut Frame) {
        let (sp, msg, trap) = (self.cx.rt.region_sp, self.cx.rt.msg_region, self.cx.rt.trap);
        b.ins(&Instruction::I32Const(sp as i32))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::I32Const(64))
            .ins(&Instruction::I32GeU)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(msg as i32))
            .ins(&Instruction::Call(trap))
            .ins(&Instruction::End);
        self.region_bump(b, 1);
    }

    /// Pop a region scope. Stack-neutral, so it may be emitted with a return value
    /// already on the operand stack — the same property M2f's `modify` copy-out
    /// needs and M2d's note about a value sitting under a block established.
    ///
    /// It reclaims nothing, and that is this backend's allocator showing through
    /// rather than a region-specific hole: `malloc` is a bump pointer that never
    /// frees for `push`, for a cell payload and for `Stmt::Drop` alike (see
    /// `runtime`). What a region owns that IS finite is this counter, so the
    /// counter is what has to be exact.
    ///
    /// ponytail: no arena reclamation. The sound version is a SEPARATE bump arena
    /// with a per-region mark, routed lexically the way `Gen::heap_alloc` routes in
    /// the textual backend — marking the shared heap would reclaim the array buffer
    /// a `push` inside a region grew for a binding outside it, and routing on the
    /// *runtime* depth instead would arena-allocate a callee's String that the
    /// region escape guard never examined. Both are silent wrong answers, and the
    /// difference between them and this is not observable, so it waits for a real
    /// allocator here.
    fn region_exit(&mut self, b: &mut Frame) {
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
    fn exit_regions_above(&mut self, b: &mut Frame, depth: u32) {
        for _ in depth..self.region_depth {
            self.region_exit(b);
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
    /// this match, which is the same trade `Rt::slots`'s all-fields-named struct
    /// literal makes. Expressions keep theirs — `expr_name` still has work.
    fn stmt(&mut self, m: &mut Module, b: &mut Frame, s: &Stmt) -> Result<(), String> {
        match s {
            Stmt::Let { name, ty, value, line, .. } => {
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
                        self.store_into(m, b, place, &r, value, t)?;
                        (place, t.clone())
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
                                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                            }
                            _ => return unsupported("a `let` of a Unit value", *line),
                        }
                        (place, got)
                    }
                };
                // A String accumulator gets its append shadow here, at the one
                // declaration site — the same place, under the same whitelist, as
                // the textual backend's.
                if let Place::Local(l) = place {
                    if self.cx.resolve(&bound) == Type::Str && self.append_ok.contains(name.as_str())
                    {
                        self.str_append_shadow(b, l);
                    }
                }
                self.scope.push((name.clone(), place, bound));
                // A non-escaping `cell(..)` is released when this block exits.
                // The key is the statement's node address, which is `own`'s own
                // identity for it — the textual backend reads the same map with
                // the same key, so the two cannot disagree about which `let` owns
                // a cell.
                if self.drops.get(&(s as *const Stmt as usize)) == Some(&DropKind::ReleaseRef) {
                    self.releases
                        .last_mut()
                        .expect("a `let` outside any block")
                        .push((place, Rel::Cell));
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
                if let Place::Local(l) = place {
                    if self.region_depth == 0 && self.str_append.contains_key(&l) {
                        if let Some(parts) = crate::self_append_spine(name, value) {
                            let slot = self.str_append[&l];
                            for p in parts {
                                b.slot(slot).ins(&Instruction::LocalGet(l));
                                self.expr_as(m, b, p, &Type::Str)?;
                                self.emit_str_append(b, l);
                            }
                            return Ok(());
                        }
                    }
                }
                self.store_into(m, b, place, &r, value, &ty.clone())?;
                if let Place::Local(l) = place {
                    self.str_append_reset(b, l);
                }
            }
            Stmt::SetField { name, field, value, line } => {
                let (place, ty) = self.lookup(name, *line)?;
                let (foff, fty) = self.field_of(&ty, field, *line)?;
                let fr = self.cx.repr(&fty, *line)?;
                place
                    .addr(b, foff)
                    .ok_or_else(|| gap("a field assignment to a non-record", *line))?;
                match &fr {
                    Repr::Scalar(_) => {
                        self.expr_as(m, b, value, &fty)?;
                        b.ins(&store_of(&self.cx.ll(&fty)));
                    }
                    Repr::Agg(l) => {
                        self.expr_as(m, b, value, &fty)?;
                        b.ins(&Instruction::I32Const(l.size as i32));
                        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                    }
                    Repr::Unit => return unsupported("a Unit field", *line),
                }
            }
            Stmt::Return { value, line } => {
                match (value, self.ret.clone()) {
                    (Some(e), Repr::Scalar(_)) => {
                        let want = self.ret_ty.clone();
                        self.expr_as(m, b, e, &want)?;
                    }
                    (Some(e), Repr::Agg(l)) => {
                        // Destination-first, at the function's own boundary: the
                        // caller's slot address is already in `dest`.
                        b.ins(&Instruction::LocalGet(self.dest.unwrap()));
                        let want = self.ret_ty.clone();
                        self.expr_as(m, b, e, &want)?;
                        b.ins(&Instruction::I32Const(l.size as i32));
                        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                    }
                    (None, Repr::Unit) => {}
                    _ => {
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
                self.emit_releases_above(b, 0)?;
                // And every region scope, for the same reason the interpreter
                // decrements its counter on this path: a `return` out of a region
                // leaves it. The textual backend does NOT — see the M2m note; there
                // that omission is load-bearing, because its `region_exit` also
                // frees the arena and a returned `a + b` built inside the region
                // points into it. Nothing here frees, so nothing here can dangle,
                // and the counter can simply be right.
                self.exit_regions_above(b, 0);
                b.ins(&Instruction::Br(self.depth));
            }
            Stmt::If { cond, then_block, else_block, line } => {
                self.cond(m, b, cond, *line)?;
                b.ins(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                self.block(m, b, then_block)?;
                if let Some(e) = else_block {
                    b.ins(&Instruction::Else);
                    self.block(m, b, e)?;
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
            Stmt::IfLet { pattern, scrutinee, then_block, else_block, line } => {
                let st = self.expr(m, b, scrutinee)?;
                let sum = self
                    .sum_of(&st)
                    .ok_or_else(|| gap(&format!("an `if let` on `{st}`"), *line))?;
                let Repr::Agg(sl) = self.cx.repr(&st, *line)? else {
                    return unsupported("an `if let` on a non-aggregate", *line);
                };
                // A local of its own rather than shared scratch: the address has to
                // survive the test AND the binds, and an `if let` nests — an inner
                // one's scrutinee would take the same scratch slot back.
                let addr = b.local(ValType::I32);
                b.ins(&Instruction::LocalSet(addr));
                self.tag_test(b, addr, &sum, pattern, *line)?;
                b.ins(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                let mark = self.scope.len();
                for (i, (n, t)) in
                    self.pattern_binds(&sum, pattern, *line)?.into_iter().enumerate()
                {
                    let place = self.bind_payload(b, addr, &sum, &sl, i, &t, *line)?;
                    self.scope.push((n, place, t));
                }
                self.block(m, b, then_block)?;
                // The binders are the then-arm's only: an `else` that could see
                // them would be reading a payload the tag says is not there.
                self.scope.truncate(mark);
                if let Some(e) = else_block {
                    b.ins(&Instruction::Else);
                    self.block(m, b, e)?;
                }
                self.depth -= 1;
                b.ins(&Instruction::End);
            }
            Stmt::While { cond, body, line } => {
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
                self.loops.push((brk, cont, self.releases.len(), self.region_depth));
                self.block(m, b, body)?;
                self.loops.pop();
                let back = self.br_to(cont);
                b.ins(&Instruction::Br(back));
                self.depth -= 1;
                b.ins(&Instruction::End);
                self.depth -= 1;
                b.ins(&Instruction::End);
            }
            Stmt::ForIn { var, iter, body, line } => {
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
                        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                    }
                    _ => return unsupported("an array of Unit", *line),
                }
                let mark = self.scope.len();
                self.scope.push((var.clone(), place, w.elem.clone()));

                let cont = self.depth;
                b.ins(&Instruction::Block(BlockType::Empty));
                self.depth += 1;
                self.loops.push((brk, cont, self.releases.len(), self.region_depth));
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
            }
            Stmt::IndexSet { name, index, value, line } => {
                let (place, ty) = self.lookup(name, *line)?;
                place
                    .addr(b, 0)
                    .ok_or_else(|| gap("an element assignment to a non-array", *line))?;
                // `m[k] = v` (RFC-0028) inserts or updates; it is not a bounded
                // element store and has no index to check.
                if let Type::Map(_, val) = self.cx.resolve(&ty) {
                    let l = self.layout_of(&ty, *line)?;
                    let hdr = b.local(ValType::I32);
                    b.ins(&Instruction::LocalSet(hdr));
                    return self.map_set(m, b, hdr, &l, index, value, &val, *line);
                }
                let w = self.walk(b, &ty, *line)?;
                if w.byte {
                    return unsupported("an element assignment into a String", *line);
                }
                self.expr_as(m, b, index, &Type::Int)?;
                let i = b.local(ValType::I64);
                b.ins(&Instruction::LocalSet(i));
                self.bounds_check(b, &w, i, false);
                self.elem_addr(b, &w, i);
                let elem = w.elem.clone();
                match self.cx.repr(&elem, *line)? {
                    Repr::Scalar(_) => {
                        self.expr_as(m, b, value, &elem)?;
                        b.ins(&store_of(&self.cx.ll(&elem)));
                    }
                    Repr::Agg(el) => {
                        self.expr_as(m, b, value, &elem)?;
                        b.ins(&Instruction::I32Const(el.size as i32));
                        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                    }
                    Repr::Unit => return unsupported("an array of Unit", *line),
                }
            }
            Stmt::Break { line } => {
                let &(brk, _, boundary, regions) =
                    self.loops.last().ok_or_else(|| gap("`break` outside a loop", *line))?;
                self.emit_releases_above(b, boundary)?;
                self.exit_regions_above(b, regions);
                let d = self.br_to(brk);
                b.ins(&Instruction::Br(d));
            }
            Stmt::Continue { line } => {
                let &(_, cont, boundary, regions) =
                    self.loops.last().ok_or_else(|| gap("`continue` outside a loop", *line))?;
                self.emit_releases_above(b, boundary)?;
                self.exit_regions_above(b, regions);
                let d = self.br_to(cont);
                b.ins(&Instruction::Br(d));
            }
            // `drop` is reclamation, and reclamation is not observable for
            // anything this backend's allocator owns — it never reuses (see
            // `runtime`). A `Ref` is the exception, and not a small one: releasing
            // a cell bumps its generation and returns its SLOT to a fixed slab of
            // 65536. `freelist.vyrn` puts 100,000 allocations through it and only
            // fits because the release fires.
            // `region { .. }` (RFC-0004 §4). An arena scope, and in this backend
            // that is a counter and its trap — see `region_exit` for why the arena
            // itself is the allocator's ceiling rather than a region-shaped hole.
            //
            // The body is an ordinary block, so its scope and its `ReleaseRef`
            // frame come free; a region is one more frame the exit edges close, the
            // same shape M2l gave the inferred release. No `if !terminated` guard
            // like the textual backend's: a fall-through exit after a `br` is code
            // wasm has already marked unreachable, which is the same argument
            // `Fn_::block` makes about its own releases.
            Stmt::Region { body, .. } => {
                self.region_enter(b);
                self.region_depth += 1;
                self.block(m, b, body)?;
                self.region_depth -= 1;
                self.region_exit(b);
            }
            Stmt::Drop { name, line } => {
                let (place, ty) = self.lookup(name, *line)?;
                if matches!(self.cx.resolve(&ty), Type::Ref(_)) {
                    self.emit_release(b, place, *line)?;
                }
            }
            Stmt::Expr(e) => {
                // A call for its effect leaves its result on the stack; drop it,
                // or the block's type will not check.
                if !matches!(self.cx.repr(&self.expr(m, b, e)?, Expr::line(e))?, Repr::Unit) {
                    b.ins(&Instruction::Drop);
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

    /// Give the accumulator in wasm local `l` its `(len, cap)` shadow, and start
    /// it unowned.
    ///
    /// A Vyrn `String` is a bare NUL-terminated pointer with no header, so growing
    /// one in place needs its length and capacity kept beside it. They go in the
    /// frame rather than in two more wasm locals because the runtime helper writes
    /// them back and wasm has no way to pass a local by reference — eight bytes of
    /// shadow stack against a three-result function type, and the frame is already
    /// per-invocation, so a recursive writer (`emitArr` calling `emit`) gets its
    /// own without anything being said about recursion.
    ///
    /// `cap == 0` means "this pointer was not allocated by the append path" — a
    /// literal in a data segment, a `concat` result, a call result — so it may not
    /// be grown and `len` says nothing. Emitted at the `let`, so the second trip
    /// through an enclosing loop starts unowned again.
    fn str_append_shadow(&mut self, b: &mut Frame, l: u32) {
        let at = *self.str_append.entry(l).or_insert_with(|| b.alloc(8, 4));
        b.slot(at).ins(&Instruction::I32Const(0)).ins(&Instruction::I32Store(cap_at()));
    }

    /// Append the `String` on top of the stack to the accumulator in local `l`,
    /// in place. The helper takes the shadow's address, the current pointer and
    /// the operand, and gives back the pointer to store — which is the whole
    /// convention, and why the call site is five instructions.
    fn emit_str_append(&mut self, b: &mut Frame, l: u32) {
        b.ins(&Instruction::Call(self.cx.rt.str_append));
        b.ins(&Instruction::LocalSet(l));
    }

    /// Invalidate the shadow after any other write to the accumulator: the local
    /// now holds a pointer this path did not allocate.
    fn str_append_reset(&mut self, b: &mut Frame, l: u32) {
        if let Some(&at) = self.str_append.get(&l) {
            b.slot(at).ins(&Instruction::I32Const(0)).ins(&Instruction::I32Store(cap_at()));
        }
    }

    /// Evaluate `value` into an existing place of known type.
    fn store_into(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        place: Place,
        r: &Repr,
        value: &Expr,
        ty: &Type,
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
            (Place::Slot(_) | Place::Static(_), Repr::Agg(l)) => {
                place.addr(b, 0);
                self.expr_as(m, b, value, ty)?;
                b.ins(&Instruction::I32Const(l.size as i32));
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
            }
            (Place::Static(_), Repr::Scalar(_)) => {
                place.addr(b, 0);
                self.expr_as(m, b, value, ty)?;
                b.ins(&store_of(&self.cx.ll(ty)));
            }
            _ => return unsupported("a store of a Unit value", Expr::line(value)),
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
        let got = self.expr(m, b, e);
        self.expect.pop();
        let got = got?;
        self.coerce(m, b, Some(e), &got, want, Expr::line(e))
    }

    /// Reconcile the value on the stack, of type `from`, into `to`.
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
    /// `expr` is the expression that produced the value, when there is one — only
    /// RFC-0020's containment proof needs it, and only for strings.
    ///
    /// **Validation runs FIRST**, and that order is the entire point. A refined
    /// type has the SAME representation as its base, so the `ll`-equality
    /// shortcut below would let `Int64 → Even` past unchecked: same bytes, no
    /// check, wrong program, forever.
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
        // it is the one thing that MUST be reduced before `validation_required`
        // looks at it: a `T` where `T = Age` is an `Age` flow, and a `Param`
        // would silently be neither `Named` nor a boundary.
        let (from, to) = (&self.cx.sub(from), &self.cx.sub(to));
        // A `Never` (RFC-0079) reached this seam from a `panic`, which left
        // nothing on the stack and ended the block in `unreachable`. There is no
        // value to reconcile and no validation to owe — the polymorphic stack
        // after `unreachable` satisfies `to` on its own.
        if matches!(from, Type::Never) {
            return Ok(());
        }
        if let Some(decl) = crate::validation_required(from, to, &self.cx.types).cloned() {
            // The value has to be in the base's representation before the
            // predicate reads it. The recursion terminates because a base is one
            // step nearer a builtin than the name it backs.
            self.coerce(m, b, expr, from, &decl.base, line)?;
            if !expr.is_some_and(|e| self.proven(e, to)) {
                self.emit_validation(m, b, &decl, line)?;
            }
            return Ok(());
        }
        // An integer resize, and it has to come BEFORE the `ll`-equality
        // shortcut: `llt` prints `i8` for both `Int8` and `UInt8`, so two types
        // with the same shape can still want different bits in the carrier —
        // an `Int8` is sign-extended where a `UInt8` is masked. That pair is
        // exactly what the shortcut would have swallowed silently.
        //
        // Widening reads the SOURCE's signedness (a `UInt8` zero-extends, an
        // `Int8` sign-extends); narrowing discards bits and renormalizes into the
        // TARGET's. That is the interpreter's `wrap_intn` and the textual
        // backend's `sext`/`zext`/`trunc`, and both stop being separate rules the
        // moment [`Num`]'s invariant is written down.
        if let (Some(f), Some(t)) = (Num::of(&self.cx.resolve(from)), Num::of(&self.cx.resolve(to)))
        {
            match (f == t, f.wide(), t.wide()) {
                (true, ..) => {}
                (_, false, true) => widen(b, f),
                (_, true, false) => {
                    b.ins(&Instruction::I32WrapI64);
                    renorm(b, t);
                }
                // Both carriers are `i64`, so only the signedness changed and the
                // bits do not move.
                (_, true, true) => {}
                // Both in an `i32`: the source's representation already holds the
                // bits, and only the target's normalization is owed.
                (_, false, false) => renorm(b, t),
            }
            return Ok(());
        }
        // Across the int/float line, and between the two float widths.
        //
        // `trunc_sat` rather than `trunc`: wasm's plain `i64.trunc_f64_s` TRAPS
        // out of range, where LLVM's `fptosi` is undefined and Rust's `as`
        // saturates — and the interpreter IS Rust's `as`, which is the answer the
        // ladder compares against.
        //
        // Float → sized int goes through 64 bits FIRST and narrows after, because
        // that is what the interpreter does (`f as i64`, then `wrap_intn`) and the
        // two genuinely disagree: `Int8(1e10)` is 0 through an `i64` and -1
        // through an `i32` whose saturation clamped at `i32::MAX`.
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
                return Ok(());
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
                return Ok(());
            }
            (_, Some(f), _, Some(t)) if f != t => {
                b.ins(if f {
                    &Instruction::F32DemoteF64
                } else {
                    &Instruction::F64PromoteF32
                });
                return Ok(());
            }
            _ => {}
        }
        if self.cx.ll(from) == self.cx.ll(to) {
            return Ok(());
        }
        // A literal is a fixed `[N x T]`; an `Array<T>` slot wants the growable
        // triple. One conversion, so every literal position — a `let`, an
        // argument, a `return`, a field, an element — reaches the heap the same
        // way.
        if let (Type::ArrayN(inner, n), Type::Array(el)) =
            (self.cx.resolve(from), self.cx.resolve(to))
        {
            if self.cx.ll(&inner) == self.cx.ll(&el) {
                return self.heapify(b, &inner, n, to, line);
            }
        }
        // The same literal in a `SmallArray<T, N>` position stays OFF the heap: the
        // elements are copied into the inline buffer and `cap` is set to `N`, which
        // is the state discriminant (RFC-0056). The checker proved `len <= N`.
        if let (Type::ArrayN(inner, len), Type::SmallArray(el, n)) =
            (self.cx.resolve(from), self.cx.resolve(to))
        {
            if self.cx.ll(&inner) == self.cx.ll(&el) && len <= n {
                return self.sa_from_fixed(b, &inner, len, to, n, line);
            }
        }
        // RFC-0002's record width subtyping: a wider record used as a narrower
        // one. A rebuild rather than a prefix, because the two field orders need
        // not agree — the shapes are the same length only by coincidence.
        let (got, want) = (from, to);
        let (Some(from), Some(to)) = (self.cx.fields(got), self.cx.fields(want)) else {
            return unsupported(
                &format!("a conversion from `{got}` to `{want}`"),
                line,
            );
        };
        let src = self.scratch(b, ValType::I32, 0);
        b.ins(&Instruction::LocalSet(src));
        let l = self.cx.repr(want, line)?;
        let Repr::Agg(dl) = &l else {
            return unsupported("a record that is not an aggregate", line);
        };
        let off = b.alloc(dl.size, dl.align);
        let sl = layout::of_ll(&self.cx.ll(got)).map_err(|e| format!("direct backend: {e}"))?;
        for (i, f) in to.iter().enumerate() {
            let j = from
                .iter()
                .position(|g| g.name == f.name)
                .ok_or_else(|| gap(&format!("the field `{}`", f.name), line))?;
            if self.cx.ll(&from[j].ty) != self.cx.ll(&f.ty) {
                return unsupported("a record conversion that changes a field's shape", line);
            }
            match self.cx.repr(&f.ty, line)? {
                Repr::Scalar(_) => {
                    b.slot(off + dl.fields[i]);
                    b.ins(&Instruction::LocalGet(src));
                    b.ins(&load_of(&self.cx.ll(&f.ty), sl.fields[j], self.cx.signed(&f.ty)));
                    b.ins(&store_of(&self.cx.ll(&f.ty)));
                }
                Repr::Agg(fl) => {
                    b.slot(off + dl.fields[i]);
                    b.ins(&Instruction::LocalGet(src));
                    b.ins(&Instruction::I32Const(sl.fields[j] as i32));
                    b.ins(&Instruction::I32Add);
                    b.ins(&Instruction::I32Const(fl.size as i32));
                    b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                }
                Repr::Unit => return unsupported("a Unit field", line),
            }
        }
        b.slot(off);
        Ok(())
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

    /// Emit the runtime check that the value on the stack satisfies `decl`'s
    /// `where` predicate, trapping with the canonical message if it does not.
    ///
    /// The value is LEFT on the stack: a validation is a check on a flow, not a
    /// step in it. But the predicate's own code would bury it — the operand stack
    /// is not addressable — so it is parked in the place the predicate binds it
    /// to, which for a scalar base is the same place and therefore costs no copy.
    ///
    /// What binds is [`crate::predicate_binds`]'s call, shared with the LLVM
    /// emitter. The lowering of the predicate itself cannot be shared, since one
    /// prints text and this writes bytes; what is shared is the structure walked.
    fn emit_validation(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        decl: &TypeDecl,
        line: usize,
    ) -> Result<(), String> {
        let Some(held) = self.predicate_holds(m, b, decl, line)? else { return Ok(()) };
        // The message on stderr and exit 1 — `Rt::trap`, the same path the
        // division and bounds checks take, because parity compares stderr and a
        // wasm `unreachable` would print wasmtime's wording instead of ours.
        let msg = self.cx.rt.intern(m, &crate::validation_message(decl));
        let trap = self.cx.rt.trap;
        b.ins(&Instruction::I32Eqz);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        b.ins(&Instruction::I32Const(msg as i32));
        b.ins(&Instruction::Call(trap));
        self.depth -= 1;
        b.ins(&Instruction::End);
        b.ins(&Instruction::LocalGet(held));
        Ok(())
    }

    /// Consume the value on the stack and leave `decl`'s `where` predicate's
    /// answer (a Bool) there instead, giving the local the value was parked in —
    /// or `None`, stack untouched, for a type with no refinement.
    ///
    /// Split out of [`Fn_::emit_validation`] because a fallible construction wants
    /// the same answer without the trap (RFC-0077 M2k): two spellings of "run the
    /// predicate" could disagree about what the predicate binds, and a `Age?(n)`
    /// that read a different `value` than `Age(n)` does would be a `None` on one
    /// backend only.
    fn predicate_holds(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        decl: &TypeDecl,
        line: usize,
    ) -> Result<Option<u32>, String> {
        let Some(pred) = decl.predicate.clone() else { return Ok(None) };
        let binds = crate::predicate_binds(decl);
        let mark = self.scope.len();
        // Whatever the value was parked in, so the flow can carry on with it.
        let held = match (self.cx.repr(&decl.base, line)?, &decl.base) {
            // A record base binds every field by name, so the value is parked by
            // ADDRESS and each field copied out of it. A copy rather than a view
            // because a predicate cannot write to what it was given, so the two
            // can never be observed to differ.
            (Repr::Agg(l), Type::Record(_)) => {
                let addr = b.local(ValType::I32);
                b.ins(&Instruction::LocalSet(addr));
                for (name, ty, field) in &binds {
                    let i = field.expect("a record base binds by field index");
                    let fr = self.cx.repr(ty, line)?;
                    let place = self.place_for(b, &fr, line)?;
                    match (place, &fr) {
                        (Place::Local(loc), _) => {
                            b.ins(&Instruction::LocalGet(addr));
                            b.ins(&load_of(&self.cx.ll(ty), l.fields[i], self.cx.signed(ty)));
                            b.ins(&Instruction::LocalSet(loc));
                        }
                        (Place::Slot(off), Repr::Agg(fl)) => {
                            b.slot(off);
                            b.ins(&Instruction::LocalGet(addr));
                            b.ins(&Instruction::I32Const(l.fields[i] as i32));
                            b.ins(&Instruction::I32Add);
                            b.ins(&Instruction::I32Const(fl.size as i32));
                            b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                        }
                        _ => return unsupported("a Unit field in a `where` clause", line),
                    }
                    self.scope.push((name.clone(), place, ty.clone()));
                }
                addr
            }
            // Every other base binds `value`, and the parked local IS it.
            (Repr::Scalar(v), _) => {
                let loc = b.local(v);
                b.ins(&Instruction::LocalSet(loc));
                let (name, ty, _) = binds.into_iter().next().expect("a scalar base binds `value`");
                self.scope.push((name, Place::Local(loc), ty));
                loc
            }
            // An aggregate base that is not a record has one `value` binding, and
            // M2d refused it because `Place` could not name where to put it. M2f's
            // `Static` does not change that — a global's address is fixed and this
            // one is on the operand stack — but the record arm above shows the
            // shape that would: copy the whole value into a frame slot and bind
            // `value` to it, needing no new variant at all. Still refused, because
            // no example has one and an untested lowering is worse than a named
            // gap: this is the milestone where it stopped being a Place problem.
            _ => {
                return unsupported(
                    &format!("a `where` clause over the non-record aggregate `{}`", decl.base),
                    line,
                );
            }
        };
        let cond = self.expr(m, b, &pred)?;
        self.scope.truncate(mark);
        if self.cx.resolve(&cond) != Type::Bool {
            return unsupported("a `where` clause that is not a Bool", line);
        }
        Ok(Some(held))
    }

    /// Evaluate `e`, leaving its value (a scalar) or its address (an aggregate)
    /// on the stack, and giving the Vyrn type of what it left.
    fn expr(&mut self, m: &mut Module, b: &mut Frame, e: &Expr) -> Result<Type, String> {
        Ok(match e {
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
                match self.sum_ctor(m, b, name, &[], *line)? {
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
                if let Some(t) = self.length_of(b, &base, field, *line)? {
                    return Ok(t);
                }
                let (off, fty) = self.field_of(&base, field, *line)?;
                match self.cx.repr(&fty, *line)? {
                    Repr::Scalar(_) => b.ins(&load_of(&self.cx.ll(&fty), off, self.cx.signed(&fty))),
                    Repr::Agg(_) => b
                        .ins(&Instruction::I32Const(off as i32))
                        .ins(&Instruction::I32Add),
                    Repr::Unit => return unsupported("a Unit field", *line),
                };
                fty
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
                let off = b.alloc(l.size, l.align);
                for (i, f) in decl.iter().enumerate() {
                    let init = fields
                        .iter()
                        .find(|(n, _)| *n == f.name)
                        .map(|(_, e)| e)
                        .ok_or_else(|| gap(&format!("the missing field `{}`", f.name), *line))?;
                    match self.cx.repr(&f.ty, *line)? {
                        Repr::Scalar(_) => {
                            b.slot(off + l.fields[i]);
                            self.expr_as(m, b, init, &f.ty)?;
                            b.ins(&store_of(&self.cx.ll(&f.ty)));
                        }
                        Repr::Agg(fl) => {
                            b.slot(off + l.fields[i]);
                            self.expr_as(m, b, init, &f.ty)?;
                            b.ins(&Instruction::I32Const(fl.size as i32));
                            b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                        }
                        Repr::Unit => return unsupported("a Unit field", *line),
                    }
                }
                b.slot(off);
                // A predicated record's cross-field `where` runs on the finished
                // literal. There is no coercion to hang it on — the literal
                // already IS the named type, so `from == to` and
                // `validation_required` correctly says no — which is exactly why
                // the LLVM emitter validates at its construction site too. A
                // wholly constant literal was proven by the checker, so only a
                // dynamic one pays.
                if let Some(d) =
                    self.cx.types.get(name).filter(|d| d.predicate.is_some()).cloned()
                {
                    let dynamic = fields.iter().any(|(_, e)| {
                        vyrn_frontend::consteval::eval(e, &HashMap::new()).is_none()
                    });
                    if dynamic {
                        self.emit_validation(m, b, &d, *line)?;
                    }
                }
                ty
            }
            Expr::IfExpr { cond, then_branch, else_branch, line } => {
                let els = else_branch
                    .as_deref()
                    .ok_or_else(|| gap("an `if` expression with no `else`", *line))?;
                self.join(m, b, cond, then_branch, els, *line)?
            }
            Expr::Unary { op, expr, line } => {
                let t = self.expr(m, b, expr)?;
                let rt = self.cx.resolve(&t);
                match (op, Num::of(&rt)) {
                    // `x * -1`, which is also what makes the width's minimum
                    // negate to itself — the wrapping the interpreter does, for
                    // free. `~x` is `x ^ -1`, and both then renormalize because a
                    // narrow carrier holds more bits than the width.
                    (UnOp::Neg | UnOp::BitNot, Some(n)) => {
                        if n.wide() {
                            b.ins(&Instruction::I64Const(-1));
                            b.ins(if *op == UnOp::Neg {
                                &Instruction::I64Mul
                            } else {
                                &Instruction::I64Xor
                            });
                        } else {
                            b.ins(&Instruction::I32Const(-1));
                            b.ins(if *op == UnOp::Neg {
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
                    (UnOp::BitNot, None) if matches!(rt, Type::Mask32x4 | Type::I32x4) => {
                        b.ins(&Instruction::V128Not);
                    }
                    (UnOp::Not, _) if rt == Type::Bool => {
                        b.ins(&Instruction::I32Eqz);
                    }
                    _ => return unsupported("a unary operator on this type", *line),
                }
                t
            }
            Expr::ArrayLit { elems, line } => self.array_lit(m, b, elems, *line)?,
            Expr::MapLit { entries, line } => self.map_lit(m, b, entries, *line)?,
            Expr::Match { scrutinee, arms, line } => self.match_expr(m, b, scrutinee, arms, *line)?,
            Expr::Try { expr, line } => self.try_(m, b, expr, *line)?,
            Expr::TryConstruct { name, args, line } => {
                self.try_construct(m, b, name, args, *line)?
            }
            Expr::Binary { op, lhs, rhs, line } => self.binary(m, b, *op, lhs, rhs, *line)?,
            Expr::Call { name, args, line } => self.call(m, b, name, args, *line)?,
            Expr::Spawn { name, args, line } => self.spawn(m, b, name, args, *line)?,
            other => return unsupported(&expr_name(other), Expr::line(other)),
        })
    }

    /// The concrete type a record literal produces.
    ///
    /// For a generic record the type arguments come from the FIELD values, by the
    /// same shared rule a call site uses — and they have to be solved before the
    /// literal's slot is allocated, because `Box<Int64>` and `Box<Bool>` are not
    /// the same size. Non-generic is the overwhelming majority and costs nothing:
    /// the name IS the type.
    fn applied_record(
        &mut self,
        name: &str,
        fields: &[(String, Expr)],
        line: usize,
    ) -> Result<Type, String> {
        let named = Type::Named(name.to_string());
        let Some(decl) = self.cx.types.get(name).filter(|d| !d.type_params.is_empty()).cloned()
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
        let mut actual = Vec::new();
        for f in &declared {
            let e = fields
                .iter()
                .find(|(n, _)| *n == f.name)
                .map(|(_, e)| e)
                .ok_or_else(|| gap(&format!("the missing field `{}`", f.name), line))?;
            let t = self.peek(e, line)?;
            actual.push(self.cx.sub(&t));
        }
        Ok(crate::applied_type(
            Some(&decl),
            name,
            &declared.iter().map(|f| f.ty.clone()).collect::<Vec<_>>(),
            &actual,
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
        Ok(Some(crate::applied_type(decl.as_ref(), &e, &declared, &actual)))
    }

    /// Whether `t` is an instantiation with every type argument fixed, under this
    /// body's substitution. [`crate::ty_is_concrete_app`] is the rule.
    fn concrete_app(&self, t: &Type) -> bool {
        crate::ty_is_concrete_app(t, &|a| self.cx.resolve(a))
    }

    /// The type a `match`'s arms agree on, without emitting anything.
    ///
    /// The FIRST arm answers, as it does for an `if`: [`Fn_::expr_as`] re-checks
    /// every other arm against the answer, so a wrong guess is a compile error
    /// rather than a miscompile. The one thing a later arm can add is a type
    /// ARGUMENT — see [`crate::ty_is_concrete_app`] — so a non-applied answer is
    /// upgraded by the first arm that has one, and the answer stops depending on
    /// arm ORDER. `genericpayload.vyrn` puts the concrete arm first and the
    /// param-free one last precisely so a first-arm-wins rule looks correct.
    ///
    /// A later arm `peek` cannot see forfeits the upgrade and nothing else: its own
    /// `expr_as` refuses it later if it truly cannot be lowered. So the scan never
    /// narrows what this backend reaches.
    fn match_ty(&mut self, sum: &Sum, arms: &[MatchArm], line: usize) -> Result<Type, String> {
        let first = arms.first().ok_or_else(|| gap("an empty `match`", line))?;
        let ty = self.peek_arm(first, sum, line)?;
        if self.concrete_app(&ty) {
            return Ok(ty);
        }
        // A `panic` arm (RFC-0079) is `Never` and answers nothing, so a later arm
        // answers instead — the same fall-through the type-argument upgrade uses,
        // which is why "the first arm answers" survives a `?? panic(..)` in one.
        let mut ty = ty;
        for arm in &arms[1..] {
            if let Ok(t) = self.peek_arm(arm, sum, line) {
                if self.concrete_app(&t) {
                    return Ok(t);
                }
                if matches!(ty, Type::Never) && !matches!(t, Type::Never) {
                    ty = t;
                }
            }
        }
        Ok(ty)
    }

    /// One arm's type, with its bindings in scope. The place is a dummy: `peek`
    /// reads types and never emits, so a scope frame it cannot mutate is enough.
    fn peek_arm(&mut self, arm: &MatchArm, sum: &Sum, line: usize) -> Result<Type, String> {
        let mark = self.scope.len();
        for (n, t) in self.pattern_binds(sum, &arm.pattern, line)? {
            self.scope.push((n, Place::Local(u32::MAX), t));
        }
        let got = self.peek(&arm.body, line);
        self.scope.truncate(mark);
        got
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
        cond: &Expr,
        then_e: &Expr,
        else_e: &Expr,
        line: usize,
    ) -> Result<Type, String> {
        // A `panic` then-branch (RFC-0079) names no type, so the else answers.
        let want = match self.peek(then_e, line)? {
            Type::Never => self.peek(else_e, line)?,
            t => t,
        };
        let want = self.join_ty(want);
        let r = self.cx.repr(&want, line)?;
        self.cond(m, b, cond, line)?;
        match &r {
            Repr::Agg(l) => {
                let off = b.alloc(l.size, l.align);
                b.ins(&Instruction::If(BlockType::Empty));
                self.depth += 1;
                b.slot(off);
                self.expr_as(m, b, then_e, &want)?;
                b.ins(&Instruction::I32Const(l.size as i32));
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                b.ins(&Instruction::Else);
                b.slot(off);
                self.expr_as(m, b, else_e, &want)?;
                b.ins(&Instruction::I32Const(l.size as i32));
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                self.depth -= 1;
                b.ins(&Instruction::End);
                b.slot(off);
            }
            Repr::Scalar(v) => {
                b.ins(&Instruction::If(BlockType::Result(*v)));
                self.depth += 1;
                self.expr_as(m, b, then_e, &want)?;
                b.ins(&Instruction::Else);
                self.expr_as(m, b, else_e, &want)?;
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

    /// The type a join carries, which is not always the one its first arm names.
    ///
    /// A REFINED type decays to its base here, and only here, because a join is
    /// not a value boundary. The checker unifies `Some(a) => a` (an `Age`) with
    /// `None => 0 - 1` (an `Int64`) at the base and asks nothing of the second
    /// arm; a lowering that made `Age` the arms' target instead would send that
    /// arm through M2d's seam and validate it against a refinement the language
    /// never required, which is `error: validation failed for `Age`` here and
    /// `-1` on the other two engines.
    ///
    /// Found by `validate.vyrn` becoming compilable in M2k, but the hole is
    /// M2b's: a plain `match` on an `Option<Age>` had it all along, and no
    /// example held one. The boundary the value really crosses — the `let`, the
    /// `return`, the field — still validates, because that coercion is a
    /// separate one outside the join.
    fn join_ty(&self, t: Type) -> Type {
        match &t {
            Type::Named(n) if self.cx.types.get(n).is_some_and(|d| d.predicate.is_some()) => {
                self.cx.resolve(&t)
            }
            _ => t,
        }
    }

    /// The type an expression WILL have, without emitting anything.
    ///
    /// Needed only at a join, where the destination has to exist before either
    /// arm runs. Deliberately shallow: anything it cannot see is a gap rather
    /// than a guess, and [`Fn_::expr_as`] re-checks the answer against what the
    /// arm actually produced, so a wrong prediction is loud rather than silent.
    fn peek(&mut self, e: &Expr, line: usize) -> Result<Type, String> {
        Ok(match e {
            Expr::Int(_) | Expr::Byte(_) => Type::Int,
            Expr::Float(_) => Type::Float,
            Expr::Bool(_) => Type::Bool,
            Expr::Str(_) => Type::Str,
            // A `fn`-typed parameter or a bare function name used as a VALUE
            // (RFC-0023 × RFC-0037): its type is the target's signature, which is
            // also what a generic record field solves its parameters from —
            // `Deferred { run: run }` fixes `P` and `T` from exactly this.
            Expr::Var { name, .. } if self.fn_binds.contains_key(name) => {
                let t = &self.fn_binds[name].target;
                Type::Fn(t.sig.params[t.ncaps..].to_vec(), Box::new(t.sig.ret_ty.clone()))
            }
            // A nullary constructor (`None`, or an enum's `Empty`) parses as a bare
            // name, so it is only distinguishable from a local by failing to be one
            // — the same test, in the same order, that the emitting path makes.
            // Without this a `match` whose FIRST arm is a param-free variant could
            // not be typed at all, which is the arm order the checker demands an
            // annotation for.
            Expr::Var { name, .. }
                if self.lookup(name, line).is_err()
                    && (name == "None" || self.cx.variants.contains_key(name)) =>
            {
                match (name.as_str(), self.expected_sum()) {
                    ("None", Some(t)) => t,
                    ("None", None) => {
                        return unsupported("a branch yielding `None` with no expected type", line)
                    }
                    _ => match self.applied_variant(name, &[], line)? {
                        Some(t) => t,
                        None => {
                            return unsupported(
                                &format!("a branch yielding the ambiguous variant `{name}`"),
                                line,
                            )
                        }
                    },
                }
            }
            Expr::Var { name, .. }
                if self.lookup(name, line).is_err() && self.cx.sigs.contains_key(name) =>
            {
                let sig = &self.cx.sigs[name];
                Type::Fn(sig.params.clone(), Box::new(sig.ret_ty.clone()))
            }
            Expr::Var { name, .. } => self.lookup(name, line)?.1,
            Expr::Field { expr, field, .. } => {
                let base = self.peek(expr, line)?;
                match (field.as_str(), self.cx.resolve(&base)) {
                    ("byteLength", Type::Str)
                    | ("length", Type::Array(_))
                    | ("length", Type::ArrayN(..)) => Type::Int,
                    _ => self.field_of(&base, field, line)?.1,
                }
            }
            Expr::StructLit { name, fields, .. } => self.applied_record(name, fields, line)?,
            // A map literal in a branch: the position decides its value type, the
            // same rule the emitting path uses, and an empty one has nothing else
            // to be typed by at all.
            Expr::MapLit { entries, .. } => match self.expect.last().map(|t| self.cx.resolve(t)) {
                Some(t @ Type::Map(..)) => t,
                _ => match entries.first() {
                    Some((_, ve)) => Type::Map(
                        Box::new(Type::Str),
                        Box::new(self.peek(ve, line)?),
                    ),
                    None => Type::Map(Box::new(Type::Str), Box::new(Type::Int)),
                },
            },
            // A `panic` then-branch names no type, so the else answers — the rule
            // [`Fn_::join`] emits under.
            Expr::IfExpr { then_branch, else_branch, .. } => {
                match (self.peek(then_branch, line)?, else_branch) {
                    (Type::Never, Some(e)) => self.peek(e, line)?,
                    (t, _) => t,
                }
            }
            // A `match` is typed by its arms — see [`Fn_::match_ty`], which is the
            // same rule the emitting path uses.
            Expr::Match { scrutinee, arms, .. } => {
                let st = self.peek(scrutinee, line)?;
                let sum = self
                    .sum_of(&st)
                    .ok_or_else(|| gap(&format!("a `match` on `{st}`"), line))?;
                self.match_ty(&sum, arms, line)?
            }
            Expr::Unary { expr, .. } => self.peek(expr, line)?,
            Expr::Binary { op, lhs, .. } => match op {
                BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
                | BinOp::And | BinOp::Or | BinOp::Match => {
                    // Comparing two vectors yields a mask, not a `Bool` (RFC-0083
                    // M2) — the one place in this table where the operator alone
                    // does not settle the answer.
                    match self.peek(lhs, line)? {
                        Type::F32x4 | Type::I32x4 => Type::Mask32x4,
                        _ => Type::Bool,
                    }
                }
                _ => self.peek(lhs, line)?,
            },
            // A literal in a branch is the fixed shape; the join's conversion
            // heapifies it if the other arm made it an `Array<T>`. An EMPTY one
            // has no element to be typed by, so it can only be what the position
            // expects — the same rule the emitting path uses.
            Expr::ArrayLit { elems, .. } if !elems.is_empty() => {
                Type::ArrayN(Box::new(self.peek(&elems[0], line)?), elems.len())
            }
            Expr::ArrayLit { .. } => match self.expect.last().map(|t| self.cx.resolve(t)) {
                Some(t @ Type::Array(_)) => t,
                _ => return unsupported("a branch yielding an empty array literal", line),
            },
            Expr::Call { name, args, .. } => match name.as_str() {
                // RFC-0076 M7's generator-only builtins in a BRANCH's value rather
                // than a statement's — `std/vyx` and `std/ui` both have arms
                // yielding a `Code`, and `std/rpc` has one yielding a `listDir`.
                // Every row reads the same thing the emitting path does: the entry's
                // own signature, `gen_list_dir_ty`, or the `Code` handle.
                n if self.gen_peek(n, args).is_some() => {
                    self.gen_peek(n, args).expect("guarded above")
                }
                "panic" | "serveStream" => Type::Never,
                "@str" | "@concat" | "jsonSchema" | "toJson" => Type::Str,
                "floatBits" => Type::IntN { bits: 64, signed: false },
                "floatFromBits" => Type::Float,
                "stringFromBytes" => Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
                // RFC-0014/RFC-0044's I/O as a BRANCH's value rather than a
                // statement's — `std/storage`'s `Ok(done) => renameFile(tmp, path)`
                // is the shape, and nothing in the corpus had put one in an arm
                // before, which is why M2o read `storage.vyrn` as blocked on the
                // syscall alone. Same function the emitting path reads.
                n if io_builtin_ty(n, args.len()).is_some() => {
                    io_builtin_ty(n, args.len()).expect("guarded above")
                }
                "bytes" => Type::Array(Box::new(Type::IntN { bits: 8, signed: false })),
                // `Some`/`Ok`/`Err`/`None` in a branch, typed by the position the
                // same way `sum_ctor` types them when it emits: an arm yielding
                // `Ok(v)` cannot name the error half, so the expectation has to.
                // Without this the arm falls through to `sigs`, which has no entry
                // for a constructor, and reads as "a branch yielding `Ok`".
                "None" | "Some" | "Ok" | "Err" => match self.expected_sum() {
                    Some(t) => t,
                    // A bare `Some` still types itself from its payload; the other
                    // three carry only one half of theirs.
                    None if name == "Some" && args.len() == 1 => {
                        Type::Option(Box::new(self.peek(&args[0], line)?))
                    }
                    None => {
                        return unsupported(
                            &format!("a branch yielding `{name}` with no expected type"),
                            line,
                        )
                    }
                },
                // The two builtins whose result type is a declared one: `Schema` is
                // the record `schema_struct_lit` names, and `Value`'s name comes off
                // the variant table rather than being spelled here twice.
                "schemaOf" => Type::Named("Schema".into()),
                "value" if args.len() == 1 => {
                    let v = self.value_variant(&args[0], line)?;
                    match self.cx.variants.get(v).and_then(|c| c.first()) {
                        Some((e, _, _)) => Type::Named(e.clone()),
                        None => return unsupported("the built-in `Value` enum", line),
                    }
                }
                // An arm that only prints — or only logs: the join carries nothing,
                // which the `match` lowering already handles, it just has to be
                // told. Suppressed or not, a log call is `Unit` (RFC-0008), so the
                // threshold cannot change a type.
                "print" | "trace" | "debug" | "info" | "warn" | "error" => Type::Unit,
                "logger" => Type::Logger,
                // RFC-0083: the vector builtins, whose result type is fixed.
                "F32x4" | "@f32x4Splat" | "@f32x4Load" | "@f32x4Min" | "@f32x4Max"
                | "@f32x4Sqrt" | "@f32x4Ceil" | "@f32x4Floor" | "@f32x4Trunc"
                | "@f32x4Nearest" => Type::F32x4,
                "I32x4" | "@i32x4Splat" | "@i32x4Load" => Type::I32x4,
                // `replaceLane` is the one that reads its receiver: it is a value
                // method, so the width is the receiver's rather than the name's.
                "@replaceLane" => self.peek(&args[0], line)?,
                "@f32x4Store" | "@i32x4Store" => Type::Unit,
                "@lane" => match self.peek(&args[0], line)? {
                    Type::Mask32x4 => Type::Bool,
                    Type::I32x4 => INT32,
                    _ => Type::Float32,
                },
                "@anyTrue" | "@allTrue" => Type::Bool,
                "at" | "@swapRemove" if args.len() == 2 => {
                    let a = self.peek(&args[0], line)?;
                    match self.cx.resolve(&a) {
                        Type::Array(i) | Type::ArrayN(i, _) | Type::SmallArray(i, _) => *i,
                        Type::Str => Type::IntN { bits: 8, signed: false },
                        // `m[k]` is an honest lookup, so it is an `Option` where
                        // an array index is the element (RFC-0028).
                        Type::Map(_, v) if name == "at" => Type::Option(v),
                        other => return unsupported(&format!("a branch indexing `{other}`"), line),
                    }
                }
                // RFC-0075. `Stream<T>` is `Array<T>`'s three words here as
                // everywhere, so producing one is a retype and nothing more.
                "fromArray" if args.len() == 1 => {
                    match self.cx.resolve(&self.peek(&args[0], line)?) {
                        Type::Array(i) => Type::Stream(i),
                        other => {
                            return unsupported(&format!("`fromArray` of `{other}`"), line)
                        }
                    }
                }
                // The element type is the step's, not the seed's — the seed is
                // always `Int64` (RFC-0075 M2b).
                "fromStep" if args.len() == 2 => {
                    match self.cx.resolve(&self.peek(&args[1], line)?) {
                        Type::Fn(_, r) => match self.cx.resolve(&r) {
                            Type::Option(i) => Type::Stream(i),
                            other => {
                                return unsupported(&format!("a step returning `{other}`"), line)
                            }
                        },
                        other => return unsupported(&format!("`fromStep` of `{other}`"), line),
                    }
                }
                // RFC-0075 M2c. A wrapper's element type is its STEP's, exactly
                // as `fromStep`'s is — the source it wraps says nothing about
                // what comes out.
                "fromWrap" if args.len() == 2 => {
                    match self.cx.resolve(&self.peek(&args[1], line)?) {
                        Type::Fn(_, r) => match self.cx.resolve(&r) {
                            Type::Option(i) => Type::Stream(i),
                            other => {
                                return unsupported(&format!("a step returning `{other}`"), line)
                            }
                        },
                        other => return unsupported(&format!("`fromWrap` of `{other}`"), line),
                    }
                }
                // `pull` has nothing to infer from: every cursor is a
                // `Ref<Int64>`. The annotation is the type (RFC-0075 M2c).
                "pull" if args.len() == 1 => match self.expect.last().map(|t| self.cx.resolve(t)) {
                    Some(t @ Type::Option(_)) => t,
                    _ => return unsupported("a `pull` with no expected Option type", line),
                },
                "close" => Type::Unit,
                "@has" | "@remove" => Type::Bool,
                "@keys" => Type::Array(Box::new(Type::Str)),
                "push" | "@list" if !args.is_empty() => match self.peek(&args[0], line)? {
                    t => match self.cx.resolve(&t) {
                        // A `SmallArray` push yields a `SmallArray`, inline state
                        // or spilled — it never becomes a growable Array.
                        Type::SmallArray(..) => self.cx.resolve(&t),
                        Type::ArrayN(i, _) => Type::Array(i),
                        _ => t,
                    },
                },
                "@pop" if args.len() == 1 => {
                    let a = self.peek(&args[0], line)?;
                    match self.cx.resolve(&a) {
                        Type::Array(i) | Type::SmallArray(i, _) => Type::Option(i),
                        other => return unsupported(&format!("a branch popping `{other}`"), line),
                    }
                }
                "@toArray" if args.len() == 1 => {
                    let a = self.peek(&args[0], line)?;
                    match self.cx.resolve(&a) {
                        Type::SmallArray(i, _) | Type::Array(i) => Type::Array(i),
                        other => {
                            return unsupported(&format!("a branch copying `{other}`"), line)
                        }
                    }
                }
                // The generational reference (M2l). `cell` and `get` are inverses
                // and typed as such; `set` and `release` carry nothing, which a
                // `match` arm may still be — an arm that only mutates.
                "cell" if args.len() == 1 => Type::Ref(Box::new(self.peek(&args[0], line)?)),
                "get" if args.len() == 1 => {
                    let r = self.peek(&args[0], line)?;
                    match self.cx.resolve(&r) {
                        Type::Ref(i) => *i,
                        other => return unsupported(&format!("a branch reading `{other}`"), line),
                    }
                }
                "set" | "release" => Type::Unit,
                "parse" if args.len() == 1 => Type::Option(Box::new(Type::Int)),
                // M2l's rule: a builtin `call` types as it emits owes this a row,
                // and these two are `call`'s newest. Both are 1-based positions, so
                // `Int` — the checker's own answer, and the only one it could be.
                "lineAt" | "colAt" if args.len() == 2 => Type::Int,
                // The two builtins `call` lowers by REWRITING: peek the rewrite
                // rather than naming its type here, so there is no second answer
                // to keep in step with the one the emitting path will produce.
                // `fromJson`'s rewrite bottoms out in a generated decoder whose
                // signature `cx.sigs` already holds, so this needs no new case.
                "fromJson" if args.len() == 2 => {
                    let Expr::Var { name: tn, .. } = &args[0] else {
                        return unsupported("`fromJson` without a type name", line);
                    };
                    let target = vyrn_frontend::ast::Type::Named(tn.clone());
                    let e = vyrn_frontend::jsondec::decode_expr(&target, args[1].clone(), line);
                    self.peek(&e, line)?
                }
                _ if self.cx.types.get(name).is_some_and(|d| d.predicate.is_some()) => {
                    Type::Named(name.clone())
                }
                // A protocol method (RFC-0084 M2). The receiver's own type picks
                // the impl, so this peeks the REWRITE — the mangled call `call`
                // will emit — rather than answering for it here. Reached by a
                // fluent chain, where every receiver after the first is one of
                // these, and by any `match` arm that ends in a method call.
                _ if self.cx.protocol_methods.contains_key(name) && !args.is_empty() => {
                    let proto = self.cx.protocol_methods[name].clone();
                    let rty = self.cx.sub(&self.peek(&args[0], line)?);
                    let key = ftypes::type_key(&rty)
                        .ok_or_else(|| gap(&format!("`{name}` dispatched on `{rty}`"), line))?;
                    let e = Expr::Call {
                        name: ftypes::impl_method_name(&proto, &key, name),
                        args: args.clone(),
                        line,
                    };
                    self.peek(&e, line)?
                }
                // An `extern fn` (RFC-0012) in a branch. Its declared return type,
                // which is the same thing `call` hands back — the declaration is
                // the only source there is.
                _ if self.cx.externs.contains_key(name) => self.cx.externs[name].ret.clone(),
                // A branch yielding a call through a `fn`-typed parameter
                // (RFC-0023). Exact rather than predicted: the target's signature
                // is already resolved, so this asks it rather than answering for
                // it — the property M2l's `peek` work was about.
                _ if self.fn_binds.contains_key(name) => {
                    self.fn_binds[name].target.sig.ret_ty.clone()
                }
                // A branch yielding a call through a STORED function value
                // (RFC-0037). `runChain`'s `match m(x) { .. }` peeks its scrutinee,
                // so this is reached by the shape the feature exists for.
                _ if matches!(
                    self.lookup(name, line)
                        .ok()
                        .map(|(_, t)| crate::normalize_fn_sig(&self.cx.sub(&t), &self.cx.types)),
                    Some(Type::Fn(..))
                ) =>
                {
                    let (_, t) = self.lookup(name, line)?;
                    match crate::normalize_fn_sig(&self.cx.sub(&t), &self.cx.types) {
                        Type::Fn(_, ret) => *ret,
                        _ => unreachable!("guarded above"),
                    }
                }
                // A branch yielding a call to a function with `fn`-typed parameters
                // (RFC-0023): the same three solving passes the emitting path runs,
                // minus the lifting and the index.
                _ if self.cx.higher_order.contains_key(name) => {
                    let f = self.cx.higher_order[name].clone();
                    self.peek_ho(&f, args, line)?
                }
                // A generic call in a branch: the same solve the emitting path
                // does, so the join's destination is sized for the type the arm
                // will actually produce.
                _ if self.cx.generics.contains_key(name) => {
                    let f = self.cx.generics[name].clone();
                    let declared: Vec<Type> = f.params.iter().map(|p| p.ty.clone()).collect();
                    let actual = self.arg_types(&declared, args, line)?;
                    let (subst, _) = crate::solve_type_args(&f.type_params, &declared, &actual);
                    ftypes::substitute(&f.ret, &subst)
                }
                // A user enum's variant constructor in a branch (`One(a)`). The
                // fully APPLIED type, from the same shared rule `sum_ctor` uses
                // when it emits — the bare enum name left a generic variant's
                // payload a `Type::Param`, and a `match` on the result then bound
                // it. An ambiguous name — two enums with one variant spelling — is
                // a gap rather than a guess, exactly as `sum_ctor` treats it.
                _ if self.cx.variants.contains_key(name) => {
                    match self.applied_variant(name, args, line)? {
                        Some(t) => t,
                        None => {
                            return unsupported(
                                &format!("a branch yielding the ambiguous variant `{name}`"),
                                line,
                            )
                        }
                    }
                }
                // RFC-0078 M4c: a builtin whose implementation IS a Vyrn function
                // is typed by that function's signature, exactly as `call` lowers
                // it by routing to the same name.
                _ => match vyrn_frontend::loader::routed_builtin(name)
                    .and_then(|rt| self.cx.sigs.get(rt))
                    .or_else(|| self.cx.sigs.get(name))
                {
                    Some(s) => s.ret_ty.clone(),
                    None => return unsupported(&format!("a branch yielding `{name}`"), line),
                },
            },
            // A lambda is whatever signature the position names; there is nothing
            // else it could be typed by (RFC-0037).
            Expr::Lambda { line, .. } => match self.expected_fn_sig() {
                Some(t) => t,
                None => return unsupported("a lambda with no expected function type", *line),
            },
            other => return unsupported(&format!("a branch yielding {}", expr_name(other)), line),
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

    fn binary(
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
            let (table, accept, start) = self.regex_dfa(m, pat, line)?;
            b.ins(&Instruction::I32Const(table as i32));
            b.ins(&Instruction::I32Const(start as i32));
            b.ins(&Instruction::I32Const(accept as i32));
            b.ins(&Instruction::Call(self.cx.rt.regex_run));
            return Ok(Type::Bool);
        }

        let l = self.expr(m, b, lhs)?;
        let lt = self.cx.resolve(&l);
        // A string `+` is a concatenation and a string comparison is a byte
        // compare; both are calls, so they are handled before the numeric table.
        if lt == Type::Str {
            let r = self.expr(m, b, rhs)?;
            if self.cx.resolve(&r) != Type::Str {
                return unsupported("a string operator with a non-string operand", line);
            }
            if op == BinOp::Add {
                b.ins(&Instruction::Call(self.cx.rt.concat));
                return Ok(Type::Str);
            }
            b.ins(&Instruction::Call(self.cx.rt.strcmp));
            b.ins(&Instruction::I32Const(0));
            b.ins(&cmp_i32(op).ok_or_else(|| gap(&format!("`{op:?}` on strings"), line))?);
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
        if lt == Type::Bool {
            self.expr_as(m, b, rhs, &Type::Bool)?;
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
            self.expr_as(m, b, rhs, &lt)?;
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
        // Combining masks (RFC-0083 M2). The `v128.*` opcodes are width-agnostic —
        // they are bit operations on 128 bits — which costs nothing here because a
        // `Mask32x4` lane is all-ones or all-zeros and no program can build one
        // that is neither. That is the same closed set of inhabitants `any_true`
        // already leans on. `v128.andnot` exists and has no Vyrn spelling: `a & ~b`
        // is one instruction more and nothing measured wanted it.
        if lt == Type::Mask32x4 {
            self.expr_as(m, b, rhs, &lt)?;
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
            self.expr_as(m, b, rhs, &lt)?;
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
            self.expr_as(m, b, rhs, &lt)?;
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
        let Some(mut n) = Num::of(&lt) else {
            return unsupported(&format!("`{op:?}` on `{l}`"), line);
        };
        // The op width comes from EITHER operand: a plain-`Int` literal sibling
        // adopts a sized one's width, which is the textual backend's `numty`
        // rule. Taking it from the left alone would compute `0 - eight` (an
        // `Int32`) in 64 bits — the same answer for `+`/`-`/`*` and a different
        // one for `/`, `>>` and every comparison. `peek` is allowed to fail here:
        // "not obviously sized" is the answer the left operand already gave.
        let mut opty = lt.clone();
        if n == Num::PLAIN {
            if let Ok(rt) = self.peek(rhs, line) {
                let rt = self.cx.resolve(&rt);
                if let Some(rn) = Num::of(&rt).filter(|rn| *rn != Num::PLAIN) {
                    // The left operand is already on the stack. It moves to the
                    // narrower width through the M2d seam, like any other flow.
                    self.coerce(m, b, None, &lt, &rt, line)?;
                    (opty, n) = (rt, rn);
                }
            }
        }
        // The RESOLVED operand type — arithmetic runs on the base representation,
        // so `age + 1` must not validate `1` against `Age`'s predicate. It is the
        // *assignment* that re-validates the sum, which is why the LLVM emitter
        // returns its `numty` rather than `lty`.
        self.expr_as(m, b, rhs, &opty)?;
        // Division and the shifts are the operators with control flow in them.
        // Both operands come off the stack into scratch first, because the checks
        // have to look at them and then hand them back; and every case is checked
        // rather than left to wasm, whose own `div_s` trap would put wasmtime's
        // wording on stderr where parity compares ours.
        if matches!(op, BinOp::Div | BinOp::Rem | BinOp::Shl | BinOp::Shr) {
            let c = if n.wide() { ValType::I64 } else { ValType::I32 };
            let (d, num) = (self.scratch(b, c, 0), self.scratch(b, c, 1));
            let trap = self.cx.rt.trap;
            let msg = match op {
                BinOp::Div => self.cx.rt.msg_div0,
                BinOp::Rem => self.cx.rt.msg_rem0,
                _ => self.cx.rt.msg_shift,
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
            b.ins(&Instruction::I32Const(msg as i32));
            b.ins(&Instruction::Call(trap));
            b.ins(&Instruction::End);
            if op == BinOp::Div && n.signed {
                // The width's minimum over -1 has no representable answer.
                // (`%` is exempt: wasm defines `rem_s` there as 0, which is what
                // LLVM's rewritten `srem` and the interpreter both produce. An
                // unsigned divide is exempt because it has no minimum.)
                let min = i64::MIN >> (64 - n.bits);
                let ovf = self.cx.rt.msg_divovf;
                b.ins(&Instruction::LocalGet(d));
                if n.wide() {
                    b.ins(&Instruction::I64Const(-1)).ins(&Instruction::I64Eq);
                    b.ins(&Instruction::LocalGet(num));
                    b.ins(&Instruction::I64Const(min)).ins(&Instruction::I64Eq);
                } else {
                    b.ins(&Instruction::I32Const(-1)).ins(&Instruction::I32Eq);
                    b.ins(&Instruction::LocalGet(num));
                    b.ins(&Instruction::I32Const(min as i32)).ins(&Instruction::I32Eq);
                }
                b.ins(&Instruction::I32And);
                b.ins(&Instruction::If(BlockType::Empty));
                b.ins(&Instruction::I32Const(ovf as i32));
                b.ins(&Instruction::Call(trap));
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
                opty
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

    /// What one of RFC-0076 M7's generator-only builtins yields, or `None` if
    /// `name` is not one of them (or there is no generator host at all).
    ///
    /// [`Fn_::gen_builtin`]'s type column, read by `peek` when one of these is a
    /// BRANCH's value — M2l's rule that a builtin `call` lowers owes `peek` a row,
    /// because an arm's value is typed by `peek` and the join's destination is sized
    /// from it. The three structured builtins answer with their ENTRY's signature
    /// rather than a type spelled twice.
    fn gen_peek(&self, name: &str, args: &[Expr]) -> Option<Type> {
        self.cx.gen?;
        if let Some(e) = self.gen_entry(name, args) {
            return Some(self.cx.sigs[&e].ret_ty.clone());
        }
        let code = || Type::Named("Code".to_string());
        Some(match (name, args.len()) {
            ("@codeSplice", 2) => code(),
            ("@codeText", 1) => code(),
            ("raw", 1) | ("rawAt", 4) if !self.user_claims(name) => code(),
            ("render", 1) if !self.user_claims(name) => Type::Str,
            (crate::GEN_REFLECT, 2) => Type::Unit,
            (crate::GEN_NEXT_INT, 0) => Type::Int,
            (crate::GEN_NEXT_STR, 0) => Type::Str,
            ("listDir", 1) => gen_list_dir_ty(),
            _ => return None,
        })
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
    /// [`gen_slurp`] is that difference — one mediated import in place of
    /// `path_open`.
    fn gen_builtin(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Option<Type>, String> {
        let g = self.cx.gen.expect("the caller checked there is a generator host");
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
            // `listDir` (RFC-0021) has no runtime lowering in the language and must
            // keep having none, so it lives behind this flag rather than in the
            // table below. The listing comes from the loader's resolver — sorted and
            // recorded host-side, in the interpreter's own encoding.
            ("listDir", 1) => {
                let Some(f) = self.cx.rt.list_dir else {
                    return unsupported("`listDir` without a generator host", line);
                };
                let ty = gen_list_dir_ty();
                let l = self.layout_of(&ty, line)?;
                self.expr_as(m, b, &args[0], &Type::Str)?;
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                b.ins(&Instruction::Call(f));
                b.slot(off);
                Ok(Some(ty))
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
        // The length stays 64-bit all the way into `malloc`, which is the only
        // thing here that can judge it — the host names the size, so this is the
        // one length in the module that is not bounded by the memory it has to
        // fit in.
        let len = b.local(ValType::I64);
        let buf = b.local(ValType::I32);
        b.ins(&Instruction::LocalTee(len))
            .ins(&Instruction::I64Const(1))
            .ins(&Instruction::I64Add)
            .ins(&Instruction::Call(self.cx.rt.malloc))
            .ins(&Instruction::LocalTee(buf))
            .ins(&Instruction::Call(g.fetch))
            .ins(&Instruction::LocalGet(buf))
            .ins(&Instruction::LocalGet(len))
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Const(0))
            .ins(&Instruction::I32Store8(byte()))
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

    fn call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
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
        // RFC-0076 M7: the builtins that exist only while a generator runs — the
        // `Code` handle operations, M3b's atom stream, and `listDir`, none of which
        // has a row in the table below because none of them has a runtime meaning
        // outside generation.
        if self.cx.gen.is_some() {
            if let Some(t) = self.gen_builtin(m, b, name, args, line)? {
                return Ok(t);
            }
        }
        match name {
            // RFC-0079: `panic(msg)` — `error: `, the caller's message, a
            // newline, exit 1, in three `write_all`s for the reason `log_write`
            // takes five (the pieces are already where they need to be, and
            // concatenating first would cost a `malloc` out of an allocator that
            // never frees). The LAST piece is handed to `trap`, which writes its
            // argument and `proc_exit(1)`s — so the exit path is the one every
            // trap already takes, and this lowering adds no runtime function.
            "panic" => {
                if args.len() != 1 {
                    return unsupported("`panic` with other than one argument", line);
                }
                let (write_all, strlen) = (self.cx.rt.write_all, self.cx.rt.strlen);
                let (pre, nl) = (self.cx.rt.intern(m, "error: "), self.cx.rt.intern(m, "\n"));
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
                    .ins(&Instruction::Call(write_all));
                b.ins(&Instruction::I32Const(2))
                    .ins(&Instruction::LocalGet(msg))
                    .ins(&Instruction::LocalGet(msg))
                    .ins(&Instruction::Call(strlen))
                    .ins(&Instruction::Call(write_all));
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
                let msg = self.cx.rt.intern(m, crate::SERVE_STREAM_TRAP);
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
                    // Fixed six decimals, which `std/num`'s `f64Str` owns.
                    ref f if matches!(f, Type::Float | Type::Float32) => {
                        self.f64_str(b, f, line)?;
                        b.ins(&Instruction::Call(self.cx.rt.print_str));
                    }
                    Type::Str => {
                        b.ins(&Instruction::Call(self.cx.rt.print_str));
                    }
                    Type::Bool => {
                        b.ins(&Instruction::Call(self.cx.rt.bool_str));
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
            // (The five spellings are RESERVED by the checker, so no user function
            // can reach this arm — the same reason the textual backend needs no
            // guard either.)
            "trace" | "debug" | "info" | "warn" | "error" if args.len() == 2 => {
                self.expr_as(m, b, &args[0], &Type::Logger)?;
                self.expr_as(m, b, &args[1], &Type::Str)?;
                if log_level_ordinal(name).unwrap_or(0) < self.cx.log_level {
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
                    Type::Str => {}
                    // The same two steps `print` takes, for the same reason: the
                    // digits of a sized int are the digits of the `i64` its own
                    // signedness widens it to.
                    ref it if Num::of(it).is_some() => {
                        let n = Num::of(it).unwrap();
                        widen(b, n);
                        b.ins(&Instruction::I32Const(n.signed as i32));
                        b.ins(&Instruction::Call(self.cx.rt.int_str));
                    }
                    ref f if matches!(f, Type::Float | Type::Float32) => {
                        self.f64_str(b, f, line)?;
                    }
                    Type::Bool => {
                        b.ins(&Instruction::Call(self.cx.rt.bool_str));
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
                self.expr_as(m, b, &args[1], &Type::Str)?;
                b.ins(&Instruction::Call(self.cx.rt.concat));
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
                return self.expr(m, b, &e);
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
                if !self.cx.sigs.contains_key(&vyrn_frontend::jsondec::top_name(&target)) {
                    return unsupported("`fromJson` without the JSON runtime linked", line);
                }
                let e = vyrn_frontend::jsondec::decode_expr(&target, args[1].clone(), line);
                return self.expr(m, b, &e);
            }
            // `value(x)` boxes a scalar into the built-in `Value` enum. Its variant
            // is picked by the argument's type and built by the ordinary enum path,
            // so the tag and the payload encoding are the same ones a user's
            // `IntVal(3)` would get.
            "value" if args.len() == 1 => {
                let name = self.value_variant(&args[0], line)?;
                return match self.sum_ctor(m, b, name, args, line)? {
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
                return Ok(Type::IntN { bits: 64, signed: false });
            }
            "floatFromBits" if args.len() == 1 => {
                self.expr_as(m, b, &args[0], &Type::IntN { bits: 64, signed: false })?;
                b.ins(&Instruction::F64ReinterpretI64);
                return Ok(Type::Float);
            }
            // `stringFromBytes(b)` (RFC-0014): the bytes copied into a fresh
            // NUL-terminated buffer and UTF-8-validated, as a
            // `Result<String, String>`. The result is an aggregate, so the slot is
            // allocated here and the runtime writes through it — the same hidden
            // destination an aggregate-returning Vyrn call gets.
            "stringFromBytes" if args.len() == 1 => {
                let ty = Type::Result(Box::new(Type::Str), Box::new(Type::Str));
                let Repr::Agg(l) = self.cx.repr(&ty, line)? else {
                    return unsupported("`stringFromBytes` returning a non-aggregate", line);
                };
                // Through `expr_as`, so a literal argument is typed by the position
                // rather than by its first element: `['h', 'i']` is bytes because
                // this is where bytes are wanted, and an empty one has nothing else
                // to be typed by at all.
                let bytes = Type::Array(Box::new(Type::IntN { bits: 8, signed: false }));
                self.expr_as(m, b, &args[0], &bytes)?;
                let src = self.scratch(b, ValType::I32, 0);
                let al = self.layout_of(&bytes, line)?;
                b.ins(&Instruction::LocalSet(src));
                b.ins(&Instruction::LocalGet(src));
                b.ins(&Instruction::I32Load(word_at(al.fields[0])));
                b.ins(&Instruction::LocalGet(src));
                b.ins(&Instruction::I64Load(at(al.fields[1])));
                b.ins(&Instruction::I32WrapI64);
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                b.ins(&Instruction::Call(self.cx.rt.str_from_bytes));
                b.slot(off);
                return Ok(ty);
            }
            // `bytes(s)` — the string's UTF-8 bytes as an `Array<UInt8>`, i8 stride.
            // A copy, because the array is growable and the string is not: a `push`
            // on the result must not write into the string's storage.
            "bytes" if args.len() == 1 => {
                let ty = Type::Array(Box::new(Type::IntN { bits: 8, signed: false }));
                let l = self.layout_of(&ty, line)?;
                self.expr_as(m, b, &args[0], &Type::Str)?;
                let s = self.scratch(b, ValType::I32, 0);
                let n = self.scratch(b, ValType::I32, 1);
                let buf = self.scratch(b, ValType::I32, 2);
                let (strlen, malloc) = (self.cx.rt.strlen, self.cx.rt.malloc);
                b.ins(&Instruction::LocalTee(s));
                b.ins(&Instruction::Call(strlen));
                b.ins(&Instruction::LocalTee(n));
                // A zero-length string still gets a buffer, so the triple's pointer
                // is never null — `push` reallocs from it either way.
                b.ins(&Instruction::I32Const(1));
                b.ins(&Instruction::I32Add);
                b.ins(&Instruction::I64ExtendI32U);
                b.ins(&Instruction::Call(malloc));
                b.ins(&Instruction::LocalTee(buf));
                b.ins(&Instruction::LocalGet(s));
                b.ins(&Instruction::LocalGet(n));
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
                b.ins(&Instruction::Call(self.cx.rt.read_line));
                b.slot(off);
                return Ok(ty);
            }
            "readFile" | "readFileBytes" if args.len() == 1 => {
                let ty = io_builtin_ty(name, 1).expect("both readers are I/O builtins");
                let l = self.layout_of(&ty, line)?;
                self.expr_as(m, b, &args[0], &Type::Str)?;
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                let f = if name == "readFile" {
                    self.cx.rt.read_file
                } else {
                    self.cx.rt.read_file_bytes
                };
                b.ins(&Instruction::Call(f));
                b.slot(off);
                return Ok(ty);
            }
            // Two strings in, a `Result<Bool, String>` out, through a destination
            // slot — the same shape, so one arm. RFC-0044's `renameFile` differs
            // from `writeFile` only in which runtime function it calls.
            "writeFile" | "renameFile" if args.len() == 2 => {
                let ty = io_builtin_ty(name, 2).expect("both writers are I/O builtins");
                let l = self.layout_of(&ty, line)?;
                self.expr_as(m, b, &args[0], &Type::Str)?;
                self.expr_as(m, b, &args[1], &Type::Str)?;
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                b.ins(&Instruction::Call(if name == "writeFile" {
                    self.cx.rt.write_file
                } else {
                    self.cx.rt.rename_file
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
            // aggregate rule rather than a case of its own.
            "parse" if args.len() == 1 => {
                let ty = Type::Option(Box::new(Type::Int));
                let l = self.layout_of(&ty, line)?;
                self.expr_as(m, b, &args[0], &Type::Str)?;
                let off = b.alloc(l.size, l.align);
                b.slot(off);
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
                let f =
                    if name == "lineAt" { self.cx.rt.line_at } else { self.cx.rt.col_at };
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
            "F32x4" | "@f32x4Splat" | "I32x4" | "@i32x4Splat" if !args.is_empty() => {
                let int = name.starts_with("@i32x4") || name == "I32x4";
                let (vec, lane) = if int {
                    (Type::I32x4, INT32)
                } else {
                    (Type::F32x4, Type::Float32)
                };
                if name.ends_with("Splat") {
                    self.expr_as(m, b, &args[0], &lane)?;
                    b.ins(if int {
                        &Instruction::I32x4Splat
                    } else {
                        &Instruction::F32x4Splat
                    });
                } else {
                    b.ins(&Instruction::V128Const(0));
                    for (i, a) in args.iter().enumerate() {
                        self.expr_as(m, b, a, &lane)?;
                        b.ins(&if int {
                            Instruction::I32x4ReplaceLane(i as u8)
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
                let Some(k) = ftypes::const_lane(&args[1], 4) else {
                    return unsupported("a lane index that is not a constant in 0..3", line);
                };
                // A mask lane is all-ones or all-zeros; `Bool` rides an `i32` that
                // must be 0 or 1, so the extract is followed by a test against
                // zero rather than being handed over raw — `-1` where `1` is
                // expected would print `true` and compare unequal to `true`.
                if self.cx.resolve(&vt) == Type::Mask32x4 {
                    b.ins(&Instruction::I32x4ExtractLane(k));
                    b.ins(&Instruction::I32Eqz);
                    b.ins(&Instruction::I32Eqz);
                    return Ok(Type::Bool);
                }
                // An `Int32` lane needs no normalising: `i32x4.extract_lane` is
                // already the whole 32-bit value, and `Int32` rides an `i32`.
                if self.cx.resolve(&vt) == Type::I32x4 {
                    b.ins(&Instruction::I32x4ExtractLane(k));
                    return Ok(INT32);
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
                self.expr_as(m, b, &args[0], &vt)?;
                let Some(k) = ftypes::const_lane(&args[1], 4) else {
                    return unsupported("a lane index that is not a constant in 0..3", line);
                };
                self.expr_as(m, b, &args[2], if int { &INT32 } else { &Type::Float32 })?;
                b.ins(&if int {
                    Instruction::I32x4ReplaceLane(k)
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
            "@anyTrue" | "@allTrue" => {
                self.expr_as(m, b, &args[0], &Type::Mask32x4)?;
                b.ins(&if name == "@anyTrue" {
                    Instruction::V128AnyTrue
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
            "@f32x4Load" | "@f32x4Store" | "@i32x4Load" | "@i32x4Store" => {
                let int = name.starts_with("@i32x4");
                let vec = if int { Type::I32x4 } else { Type::F32x4 };
                let aty = self.expr(m, b, &args[0])?;
                let w = self.walk(b, &aty, line)?;
                self.expr_as(m, b, &args[1], &Type::Int)?;
                let idx = b.local(ValType::I64);
                b.ins(&Instruction::LocalSet(idx));
                self.bounds_check_span(b, &w, idx);
                if name.ends_with("Load") {
                    self.elem_addr(b, &w, idx);
                    // `align: 0` — one byte. The buffer is an array of 4-byte
                    // elements, so nothing guarantees the 16 a `v128.load` would
                    // like, and an overstated hint is a validation-legal lie the
                    // engine may act on. The textual backend states `align 4` for
                    // the same reason.
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
            // RFC-0004 §4's generational references (M2l). The slab and the
            // generation check are `cell_runtime`'s; what belongs here is the
            // BOXING, because only this file knows the payload's layout — the slab
            // stores one `ptr` per slot and has no idea what it points at.
            "cell" if args.len() == 1 => {
                let vty = self.expr(m, b, &args[0])?;
                if self.cx.resolve(&vty) == Type::Unit {
                    return unsupported("a `cell` holding Unit", line);
                }
                self.box_value(b, &vty, line)?;
                b.ins(&Instruction::I32WrapI64);
                let payload = self.scratch(b, ValType::I32, 4);
                b.ins(&Instruction::LocalSet(payload));
                let rty = Type::Ref(Box::new(vty));
                let Repr::Agg(l) = self.cx.repr(&rty, line)? else {
                    return unsupported("a `Ref` that is not an aggregate", line);
                };
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                b.ins(&Instruction::LocalGet(payload));
                b.ins(&Instruction::Call(self.cx.rt.cell_new));
                b.slot(off);
                return Ok(rty);
            }
            "get" | "set" | "release" if !args.is_empty() => {
                let elem = self.ref_addr(m, b, &args[0], line)?;
                if name == "release" {
                    b.ins(&Instruction::Drop);
                    return Ok(Type::Unit);
                }
                let r = self.cx.repr(&elem, line)?;
                if name == "set" {
                    if args.len() != 2 {
                        return unsupported("`set` at this arity", line);
                    }
                    let dest = self.scratch(b, ValType::I32, 5);
                    b.ins(&Instruction::LocalTee(dest));
                    self.expr_as(m, b, &args[1], &elem)?;
                    match &r {
                        Repr::Scalar(_) => b.ins(&store_of(&self.cx.ll(&elem))),
                        Repr::Agg(l) => {
                            b.ins(&Instruction::I32Const(l.size as i32));
                            b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 })
                        }
                        Repr::Unit => return unsupported("a `Ref` to Unit", line),
                    };
                    // The `LocalTee` above left the destination under the value, so
                    // the store consumed it; nothing is left on the stack.
                    return Ok(Type::Unit);
                }
                match &r {
                    Repr::Scalar(_) => {
                        b.ins(&load_of(&self.cx.ll(&elem), 0, self.cx.signed(&elem)));
                    }
                    // A by-VALUE read, like the LLVM backend's `load {ll}`: the
                    // payload address is the slab's, and handing it out would make
                    // `get(r)` an alias into the cell rather than a copy of it.
                    Repr::Agg(l) => {
                        let src = self.scratch(b, ValType::I32, 5);
                        b.ins(&Instruction::LocalSet(src));
                        let off = b.alloc(l.size, l.align);
                        b.slot(off);
                        b.ins(&Instruction::LocalGet(src));
                        b.ins(&Instruction::I32Const(l.size as i32));
                        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                        b.slot(off);
                    }
                    Repr::Unit => return unsupported("a `Ref` to Unit", line),
                }
                return Ok(elem);
            }
            // `t.join()` (RFC-0025). The task already ran, at the spawn point, so
            // there is nothing to wait for: this is a read out of its heap box.
            // Idempotent for the same reason a second `__vyrn_join` is — the box
            // is written once and never freed.
            "@join" if args.len() == 1 => {
                let t = self.expr(m, b, &args[0])?;
                let Type::Task(inner) = self.cx.resolve(&t) else {
                    // The checker admits nothing else; keep the textual backend's
                    // defensive identity rather than inventing a diagnostic.
                    return Ok(t);
                };
                match self.cx.repr(&inner, line)? {
                    Repr::Scalar(_) => {
                        b.ins(&load_of(&self.cx.ll(&inner), 0, self.cx.signed(&inner)));
                    }
                    // A copy, where the LLVM backend emits `load {ll}`. Handing
                    // out the box's own address would make a joined aggregate an
                    // alias into the task's result — M2l's `get` hazard, one
                    // container along.
                    Repr::Agg(l) => {
                        let src = b.local(ValType::I32);
                        b.ins(&Instruction::LocalSet(src));
                        let off = b.alloc(l.size, l.align);
                        b.slot(off);
                        b.ins(&Instruction::LocalGet(src));
                        b.ins(&Instruction::I32Const(l.size as i32));
                        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                        b.slot(off);
                    }
                    // A Unit task has no result to read, but it still has a box:
                    // the `Task` was a value and has to be consumed.
                    Repr::Unit => {
                        b.ins(&Instruction::Drop);
                    }
                }
                return Ok(*inner);
            }
            "@has" | "@remove" if args.len() == 2 => {
                return self.map_method(m, b, name, args, line)
            }
            "@keys" if args.len() == 1 => return self.map_method(m, b, name, args, line),
            "at" if args.len() == 2 => return self.at(m, b, args, line),
            "push" if args.len() == 2 => return self.push(m, b, args, line),
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
            "fromStep" if args.len() == 2 => return self.stream_from_step(m, b, args, line),
            "fromWrap" if args.len() == 2 => return self.stream_from_wrap(m, b, args, line),
            "pull" if args.len() == 1 => return self.stream_pull(m, b, args, line),
            // `close` reclaims what this backend CAN reclaim. Its `malloc` is a
            // bump pointer that never frees, so a buffer stream's teardown is
            // still nothing — but a stepped one owns a cell, and cells come from
            // a fixed slab of 65536 that a leak would exhaust. Which of the two
            // it is, is the tag.
            "close" if args.len() == 1 => {
                let got = self.expr(m, b, &args[0])?;
                if !matches!(self.cx.resolve(&got), Type::Stream(_)) {
                    return unsupported(&format!("`close` of `{got}`"), line);
                }
                let s = b.local(ValType::I32);
                b.ins(&Instruction::LocalSet(s));
                self.stream_release(b, Place::Local(s), line)?;
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
                let recv = Expr::Var { name: name.to_string(), line };
                return self.fnval_call(m, b, &recv, &norm, args, line);
            }
        }
        if let Some(t) = self.sum_ctor(m, b, name, args, line)? {
            return Ok(t);
        }
        // `Age(n)` — the explicit spelling of what a boundary now does by itself
        // (RFC-0003). Same rule as the record literal above: a constant was
        // proven by the checker, so only a dynamic value pays for a check.
        if let Some(d) = self.cx.types.get(name).filter(|d| d.predicate.is_some()).cloned() {
            if args.len() != 1 {
                return unsupported(&format!("`{name}` at this arity"), line);
            }
            self.expr_as(m, b, &args[0], &d.base)?;
            if vyrn_frontend::consteval::eval(&args[0], &HashMap::new()).is_none() {
                self.emit_validation(m, b, &d, line)?;
            }
            return Ok(Type::Named(name.to_string()));
        }
        // A protocol method (RFC-0002 §5): `x.show()` parses as `show(x)` and
        // dispatches statically on the receiver's concrete type — which inside a
        // bounded generic is concrete only because `subst` says so. The same
        // mangled impl the textual emitter calls, so there is one naming scheme.
        if let Some(proto) = self.cx.protocol_methods.get(name).cloned() {
            let recv = args
                .first()
                .ok_or_else(|| gap(&format!("the protocol method `{name}` with no receiver"), line))?;
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
        if let Some(f) = self.cx.higher_order.get(name).cloned() {
            if f.params.len() != args.len() {
                return unsupported(&format!("the call `{name}` at this arity"), line);
            }
            return self.ho_call(m, b, &f, args, line);
        }
        // A generic callee: solve its type arguments, discover the specialization
        // (which is what hands out its function index), then call it like any
        // other function.
        if let Some(f) = self.cx.generics.get(name).cloned() {
            if f.params.len() != args.len() {
                return unsupported(&format!("the call `{name}` at this arity"), line);
            }
            let arg_tys = self.arg_types(&f.params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>(), args, line)?;
            let (subst, solved) = crate::solve_type_args(
                &f.type_params,
                &f.params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>(),
                &arg_tys,
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
                            &format!("a generic type parameter `{tp}` the call `{name}` does not fix"),
                            line,
                        )
                    }
                }
            }
            let sig = self.cx.instantiate(m, &f, type_args, subst)?;
            return self.emit_call(m, b, &sig, args);
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
            let f = match sym {
                "__vyrn_now_millis" => self.cx.rt.now_millis,
                "__vyrn_monotonic_nanos" => self.cx.rt.mono_nanos,
                _ => self.cx.rt.random_seed,
            };
            if !args.is_empty() {
                return unsupported(&format!("the call `{name}` at this arity"), line);
            }
            // What it returns is the declaration's business, not this file's, and
            // the boundary hands back an `i64`. Anything else spelled over one of
            // these reserved names would read the wrong bytes silently.
            let ret =
                self.cx.externs.get(name).map(|e| e.ret.clone()).unwrap_or(Type::Unit);
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
                        .ins(&Instruction::LocalGet(s))
                        .ins(&Instruction::Call(self.cx.rt.strlen))
                        .ins(&Instruction::I64ExtendI32U);
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
        let Some(sig) = self.cx.sigs.get(name).cloned() else {
            return unsupported(&format!("the call `{name}`"), line);
        };
        if sig.params.len() != args.len() {
            return unsupported(&format!("the call `{name}` at this arity"), line);
        }
        self.emit_call(m, b, &sig, args)
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
        let prefix = format!("[{}] ", level.to_uppercase());
        let (at, plen) = (self.cx.rt.intern(m, &prefix), prefix.len() as i32);
        let colon = self.cx.rt.intern(m, ": ");
        let nl = self.cx.rt.intern(m, "\n");
        let (name, msg) = (self.scratch(b, ValType::I32, 7), self.scratch(b, ValType::I32, 8));
        b.ins(&Instruction::LocalSet(msg));
        b.ins(&Instruction::LocalSet(name));
        let (write_all, strlen) = (self.cx.rt.write_all, self.cx.rt.strlen);
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
                .ins(&Instruction::Call(write_all));
        };
        konst(b, at, plen);
        let string = |b: &mut Frame, l: u32| {
            fd(b);
            b.ins(&Instruction::LocalGet(l))
                .ins(&Instruction::LocalGet(l))
                .ins(&Instruction::Call(strlen))
                .ins(&Instruction::Call(write_all));
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
            return unsupported(&format!("`{which}` of something other than a type name"), line);
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
            Type::Int | Type::IntN { bits: 64, signed: true } => "IntVal",
            Type::Bool => "BoolVal",
            Type::Str => "StrVal",
            other => return unsupported(&format!("`value` of `{other}`"), line),
        })
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
    ) -> Result<Type, String> {
        // An aggregate result is written through a hidden leading pointer into a
        // slot of ours, so the destination goes on the stack before the
        // arguments and is pushed again afterwards as the value.
        let dest = match sig.ret.agg() {
            Some(l) => {
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                Some(off)
            }
            None => None,
        };
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
        if let Some(off) = dest {
            b.slot(off);
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
        f: &Function,
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
                if let Some(tptys) = self.fn_arg_param_types(&args[i]) {
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
        let mut cap_srcs: Vec<Vec<String>> = Vec::new();
        for (i, p) in f.params.iter().enumerate() {
            let Type::Fn(dptys, dret) = &p.ty else { continue };
            let ptys: Vec<Type> =
                dptys.iter().map(|t| ftypes::substitute(t, &subst)).collect();
            let dret_sub = ftypes::substitute(dret, &subst);
            let (target, srcs, tys) =
                self.resolve_fn_arg(m, &args[i], &ptys, &dret_sub, line)?;
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
                        &format!("a generic type parameter `{tp}` the call `{}` does not fix", f.name),
                        line,
                    )
                }
            }
        }
        // The specialization's own signature: the ordinary parameters, then one
        // capture parameter per capture per `fn` parameter, in `fn`-parameter
        // order. A synthesized `Function` rather than a hand-built signature, so
        // `lower_fn` lowers it with no case of its own — the prologue's by-value
        // copy of an aggregate parameter is exactly what a captured record wants.
        let mut sf = f.clone();
        sf.type_params.clear();
        sf.type_bounds.clear();
        let mut params: Vec<Param> = Vec::new();
        for p in &f.params {
            if matches!(p.ty, Type::Fn(..)) {
                continue;
            }
            params.push(Param {
                name: p.name.clone(),
                capability: p.capability,
                ty: ftypes::substitute(&p.ty, &subst),
            });
        }
        let mut binds: HashMap<String, FnBinding> = HashMap::new();
        let fn_params = f.params.iter().filter(|p| matches!(p.ty, Type::Fn(..)));
        for ((p, target), tys) in fn_params.zip(&targets).zip(&cap_tys) {
            let mut srcs = Vec::new();
            for t in tys {
                // A reserved spelling: no Vyrn identifier can contain `@`, so an
                // instance's capture parameter cannot shadow or be shadowed by
                // anything the callee's body names.
                let n = format!("@cap{}", params.len());
                params.push(Param { name: n.clone(), capability: Capability::Read, ty: t.clone() });
                srcs.push(n);
            }
            binds.insert(p.name.clone(), FnBinding { target: target.clone(), cap_srcs: srcs });
        }
        sf.params = params;
        sf.ret = ftypes::substitute(&f.ret, &subst);
        let sig = self.cx.enqueue(
            m,
            Key::Ho(f.name.clone(), type_args, targets),
            Rc::new(sf),
            subst,
            binds,
        )?;
        // Ordinary arguments in parameter order, then the capture values — read
        // from the caller's own scope, which is what fixes them at this site.
        let mut call_args: Vec<Expr> = f
            .params
            .iter()
            .enumerate()
            .filter(|(_, p)| !matches!(p.ty, Type::Fn(..)))
            .map(|(i, _)| args[i].clone())
            .collect();
        for srcs in &cap_srcs {
            for s in srcs {
                call_args.push(Expr::Var { name: s.clone(), line });
            }
        }
        self.emit_call(m, b, &sig, &call_args)
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
            .map(|s| Expr::Var { name: s.clone(), line })
            .collect();
        all.extend(args.iter().cloned());
        if all.len() != bnd.target.sig.params.len() {
            return unsupported("a call through a `fn` parameter at another arity", line);
        }
        self.emit_call(m, b, &bnd.target.sig, &all)
    }

    /// Resolve one `fn`-typed argument to a call target, giving the names to read
    /// its capture values from at THIS site and their types.
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
    ) -> Result<(FnTarget, Vec<String>, Vec<Type>), String> {
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
                    return Ok((bnd.target.clone(), bnd.cap_srcs.clone(), tys));
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
                            FnTarget { sig: dsig, ncaps: 1 },
                            vec![name.clone()],
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
                    Some(sig) => Ok((FnTarget { sig: sig.clone(), ncaps: 0 }, Vec::new(), Vec::new())),
                    None => unsupported(&format!("`{name}` as a function value"), line),
                }
            }
            other => unsupported(
                &format!("a `fn`-typed argument that is {}", expr_name(other)),
                Expr::line(other),
            ),
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
                    self.scope.push((pn.clone(), Place::Local(u32::MAX), pt.clone()));
                }
                let got = self.peek(e, line);
                self.scope.truncate(mark);
                self.cx.sub(&got?)
            }
            (Type::Param(_), LambdaBody::Block(_)) => Type::Unit,
            (t, _) => t.clone(),
        })
    }

    /// The return type of a call to a function with `fn`-typed parameters, WITHOUT
    /// resolving its targets — which is what makes it usable from [`Fn_::peek`],
    /// where nothing may be emitted and no index may be handed out.
    ///
    /// It runs the same three solving passes [`Fn_::ho_call`] does, in the same
    /// order, and reads a target's return from the same two places
    /// ([`Fn_::fn_arg_ret`]); what it skips is lifting and enqueueing. A wrong
    /// answer here is a compile error rather than a miscompile, because `expr_as`
    /// re-checks what the arm actually produced (M2b).
    fn peek_ho(&mut self, f: &Function, args: &[Expr], line: usize) -> Result<Type, String> {
        if f.params.len() != args.len() {
            return unsupported(&format!("the call `{}` at this arity", f.name), line);
        }
        let generic = !f.type_params.is_empty();
        let mut subst: HashMap<String, Type> = HashMap::new();
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
        if generic {
            for (i, p) in f.params.iter().enumerate() {
                let Type::Fn(dptys, _) = &p.ty else { continue };
                if let Some(tptys) = self.fn_arg_param_types(&args[i]) {
                    for (d, t) in dptys.iter().zip(&tptys) {
                        crate::solve_param(d, t, &mut subst);
                    }
                }
            }
            for (i, p) in f.params.iter().enumerate() {
                let Type::Fn(dptys, dret) = &p.ty else { continue };
                let ptys: Vec<Type> =
                    dptys.iter().map(|t| ftypes::substitute(t, &subst)).collect();
                let want = ftypes::substitute(dret, &subst);
                let got = self.fn_arg_ret(&args[i], &ptys, &want, line)?;
                crate::solve_param(dret, &got, &mut subst);
            }
        }
        Ok(ftypes::substitute(&f.ret, &subst))
    }

    /// What a `fn`-typed argument's target returns, read rather than resolved.
    fn fn_arg_ret(
        &mut self,
        arg: &Expr,
        ptys: &[Type],
        expected_ret: &Type,
        line: usize,
    ) -> Result<Type, String> {
        match arg {
            Expr::Lambda { params, body, line } => {
                self.lambda_ret(params, body, ptys, expected_ret, *line)
            }
            Expr::Var { name, .. } => {
                if let Some(bnd) = self.fn_binds.get(name) {
                    return Ok(bnd.target.sig.ret_ty.clone());
                }
                if let Ok((_, ty)) = self.lookup(name, line) {
                    return match crate::normalize_fn_sig(&self.cx.sub(&ty), &self.cx.types) {
                        Type::Fn(_, ret) => Ok(*ret),
                        _ => unsupported(&format!("`{name}` as a function value"), line),
                    };
                }
                match self.cx.sigs.get(name) {
                    Some(sig) => Ok(sig.ret_ty.clone()),
                    None => unsupported(&format!("`{name}` as a function value"), line),
                }
            }
            other => unsupported(
                &format!("a `fn`-typed argument that is {}", expr_name(other)),
                Expr::line(other),
            ),
        }
    }

    /// The DECLARED parameter types of a `fn`-typed argument's target, when the
    /// argument names one. `None` for a lambda literal, whose parameters take their
    /// types from the signature they flow into and so can solve nothing.
    fn fn_arg_param_types(&self, arg: &Expr) -> Option<Vec<Type>> {
        let Expr::Var { name, .. } = arg else { return None };
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
    ) -> Result<(FnTarget, Vec<String>, Vec<Type>), String> {
        if params.len() != ptys.len() {
            return unsupported("a lambda with the wrong number of parameters", line);
        }
        // The free locals, in first-seen order — the SHARED walk (`lib.rs`),
        // because a capture list is part of the lifted function's signature and two
        // backends disagreeing about its length would emit calls with the wrong
        // number of arguments.
        let cap_names = crate::lambda_captures(
            body,
            params.iter().cloned().collect(),
            &|n| {
                self.scope.iter().any(|(s, _, _)| s == n) || self.fn_binds.contains_key(n)
            },
        );
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
                    &Type::Fn(t.sig.params[t.ncaps..].to_vec(), Box::new(t.sig.ret_ty.clone())),
                    &self.cx.types,
                ));
                continue;
            }
            let (_, t) = self.lookup(cn, line)?;
            cap_tys.push(self.cx.sub(&t));
        }
        let ret = self.lambda_ret(params, body, ptys, expected_ret, line)?;
        // `LambdaBody::Expr` is a `return` of that expression — the same thing the
        // block form writes by hand, so there is one body shape to lower. A
        // Unit-returning signature is the exception and not a cosmetic one: `each(xs,
        // |x| print(x))` has an expression body whose value the signature does not
        // carry, so it is a statement rather than a return. The textual emitter
        // reaches the same split by testing `llt(ret) == "void"`.
        let block = match body {
            LambdaBody::Block(b) => b.clone(),
            LambdaBody::Expr(e) if self.cx.repr(&ret, line)? == Repr::Unit => {
                Block { stmts: vec![Stmt::Expr((**e).clone())] }
            }
            LambdaBody::Expr(e) => Block {
                stmts: vec![Stmt::Return { value: Some((**e).clone()), line }],
            },
        };
        let mut sf = f_shell(line);
        sf.params = cap_names
            .iter()
            .zip(&cap_tys)
            .map(|(n, t)| Param { name: n.clone(), capability: Capability::Read, ty: t.clone() })
            .chain(
                params
                    .iter()
                    .zip(ptys)
                    .map(|(n, t)| Param {
                        name: n.clone(),
                        capability: Capability::Read,
                        ty: t.clone(),
                    }),
            )
            .collect();
        sf.ret = ret.clone();
        sf.body = block;
        // The key: the literal's node address, the concrete shape, AND the
        // substitution the body is under. One literal inside a generic body lifts a
        // distinct copy per instantiation, and the shape alone does not say so when
        // the type parameter appears only in a statement.
        let mut shape: Vec<Type> = cap_tys.clone();
        shape.extend(ptys.iter().cloned());
        shape.push(ret);
        let mut under: Vec<(String, Type)> =
            self.cx.subst.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
        under.sort_by(|a, b| a.0.cmp(&b.0));
        let key = Key::Lambda(at as *const Expr as usize, shape, under);
        let sig = self.cx.enqueue(m, key, Rc::new(sf), self.cx.subst.clone(), HashMap::new())?;
        Ok((FnTarget { sig, ncaps: cap_names.len() }, cap_names, cap_tys))
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
        v.push(FnVal { sig: sig.clone(), target: target.clone() });
        (v.len() - 1) as i64
    }

    /// The LLVM shape of a capture block: the captures packed by value, in order.
    fn cap_block(&self, cap_tys: &[Type]) -> Result<Layout, String> {
        let ll = format!(
            "{{ {} }}",
            cap_tys.iter().map(|t| self.cx.ll(t)).collect::<Vec<_>>().join(", ")
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
        cap_srcs: &[String],
        line: usize,
    ) -> Result<Type, String> {
        let cap_tys = target.sig.params[..target.ncaps].to_vec();
        if cap_tys.len() != cap_srcs.len() {
            return unsupported("a function value whose captures do not match its target", line);
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
            for (i, (name, ty)) in cap_srcs.iter().zip(&cap_tys).enumerate() {
                let src = Expr::Var { name: name.clone(), line };
                b.ins(&Instruction::LocalGet(p));
                if bl.fields[i] != 0 {
                    b.ins(&Instruction::I32Const(bl.fields[i] as i32));
                    b.ins(&Instruction::I32Add);
                }
                self.expr_as(m, b, &src, ty)?;
                match self.cx.repr(ty, line)? {
                    Repr::Scalar(_) => {
                        b.ins(&store_of(&self.cx.ll(ty)));
                    }
                    Repr::Agg(fl) => {
                        b.ins(&Instruction::I32Const(fl.size as i32));
                        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
            &Type::Fn(t.sig.params[t.ncaps..].to_vec(), Box::new(t.sig.ret_ty.clone())),
            &self.cx.types,
        );
        let sig_ty = self.expected_fn_sig().unwrap_or(own);
        self.build_fnval(m, b, &sig_ty, t.clone(), &bnd.cap_srcs, line)
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
    fn dispatcher(
        &mut self,
        m: &mut Module,
        sig_ty: &Type,
        line: usize,
    ) -> Result<Sig, String> {
        if let Some((_, s)) = self.cx.dispatch.borrow().sigs.iter().find(|(t, _)| t == sig_ty) {
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
        let sig = Sig { index: m.reserve_func(&wp, &wr), ..s };
        self.cx.dispatch.borrow_mut().sigs.push((sig_ty.clone(), sig.clone()));
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
        self.emit_call(m, b, &dsig, &all)
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
        match (field, self.cx.resolve(base)) {
            ("byteLength", Type::Str) => {
                b.ins(&Instruction::Call(self.cx.rt.strlen));
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
    /// the shim does with the pointer on this target (`toolchain::RUNTIME_SHIM`,
    /// `#if defined(__wasi__)`): wasm has no threads, so `__vyrn_spawn` calls
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
    /// shadow-stack frame that made it and `join` is idempotent, so the result is
    /// boxed on the heap and the `Task` is that address — the shim's
    /// `VTask { frame }` minus the thunk field it no longer needs. Never freed,
    /// which is the shim's own stated ownership rule for a task.
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
            return unsupported(&format!("`spawn {name}(..)` of something not a function"), line);
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
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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

impl Fn_<'_> {
    fn layout_of(&self, ty: &Type, line: usize) -> Result<Layout, String> {
        layout::of_ll(&self.cx.ll(ty)).map_err(|e| gap(&format!("the layout of `{ty}` ({e})"), line))
    }

    /// The distance between consecutive elements. `of_ll` already rounds a
    /// shape's size up to its own alignment, so a size IS a stride.
    fn stride(&self, elem: &Type, line: usize) -> Result<u32, String> {
        Ok(self.layout_of(elem, line)?.size)
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

    /// `fromStep(seed, step)`: the step's two words and the cursor cell's two,
    /// each written straight into the pair of header fields that IS that value.
    fn stream_from_step(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        // The step first, because its signature names the element type.
        let fty = self.expr(m, b, &args[1])?;
        let fv = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(fv));
        let sig = self.cx.resolve(&fty);
        let elem = match &sig {
            Type::Fn(_, r) => match self.cx.resolve(r) {
                Type::Option(i) => *i,
                other => return unsupported(&format!("a step returning `{other}`"), line),
            },
            other => return unsupported(&format!("`fromStep` of `{other}`"), line),
        };
        // The loop reconstructs this signature from the element type alone, so a
        // step registered under any other spelling would dispatch through a
        // table it is not in. Refuse rather than miscompile.
        if sig != stream_step_sig(&elem) {
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
        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
        // `cell_new(dest, payload)` writes `{ slot, generation }` at `dest`, and
        // fields 4 and 5 are adjacent and 8-aligned, so `dest` is that pair.
        b.slot(off + sl.fields[4]);
        self.expr_as(m, b, &args[0], &Type::Int)?;
        self.box_value(b, &Type::Int, line)?;
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::Call(self.cx.rt.cell_new));
        b.slot(off);
        Ok(Type::Stream(Box::new(elem)))
    }

    /// `fromWrap(src, step)`: `fromStep` with a source (RFC-0075 M2c).
    ///
    /// The cell payload is `{ i64 cursor, Stream src }` in ONE allocation — the
    /// cursor a plain producer's cell holds, with the wrapped stream behind it —
    /// so `get`/`set` inside the step are the ordinary cell operations (that is
    /// `take`'s counter) and `src[slot]` points at the second half. Non-null is
    /// what makes a cell a wrapper: `pull` reads it and the release walks it.
    fn stream_from_wrap(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        // The step first, because its signature names the element type.
        let fty = self.expr(m, b, &args[1])?;
        let fv = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(fv));
        let sig = self.cx.resolve(&fty);
        let elem = match &sig {
            Type::Fn(_, r) => match self.cx.resolve(r) {
                Type::Option(i) => *i,
                other => return unsupported(&format!("a step returning `{other}`"), line),
            },
            other => return unsupported(&format!("`fromWrap` of `{other}`"), line),
        };
        if sig != stream_step_sig(&elem) {
            return unsupported(&format!("a step of type `{sig}`"), line);
        }
        let Repr::Agg(fl) = self.cx.repr(&sig, line)? else {
            return unsupported("a step value that is not an aggregate", line);
        };
        let sl = self.stream_layout(line)?;
        // The payload: a cursor word, then the source header.
        let pay = b.local(ValType::I32);
        b.ins(&Instruction::I64Const((8 + sl.size) as i64));
        b.ins(&Instruction::Call(self.cx.rt.malloc));
        b.ins(&Instruction::LocalSet(pay));
        b.ins(&Instruction::LocalGet(pay));
        b.ins(&Instruction::I64Const(0));
        b.ins(&Instruction::I64Store(word8()));
        b.ins(&Instruction::LocalGet(pay));
        b.ins(&Instruction::I32Const(8));
        b.ins(&Instruction::I32Add);
        self.expr(m, b, &args[0])?;
        b.ins(&Instruction::I32Const(sl.size as i32));
        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });

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
        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
        b.slot(off + sl.fields[4]);
        b.ins(&Instruction::LocalGet(pay));
        b.ins(&Instruction::Call(self.cx.rt.cell_new));
        // After the allocation, which clears whatever the recycled slot held.
        b.slot(off + sl.fields[4]);
        b.ins(&Instruction::I64Load(word8()));
        b.ins(&Instruction::Call(self.cx.rt.cell_srcp));
        b.ins(&Instruction::LocalGet(pay));
        b.ins(&Instruction::I32Const(8));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::I32Store(word()));
        b.slot(off);
        Ok(Type::Stream(Box::new(elem)))
    }

    /// `pull(c)`: one element from the stream behind this cursor (RFC-0075 M2c).
    ///
    /// The generation check is `cell_addr`'s, so a cursor that outlived its
    /// stream traps here exactly as `get` would; a cursor with no stream behind
    /// it traps on its own wording rather than reading past an 8-byte box.
    fn stream_pull(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let elem = match self.expect.last().map(|t| self.cx.resolve(t)) {
            Some(Type::Option(i)) => *i,
            _ => return unsupported("a `pull` with no expected Option type", line),
        };
        let opt = Type::Option(Box::new(elem.clone()));
        let Repr::Agg(ol) = self.cx.repr(&opt, line)? else {
            return unsupported("an Option that is not an aggregate", line);
        };
        // The cursor: a two-word `{ slot, generation }` aggregate.
        let c = b.local(ValType::I32);
        self.expr_as(m, b, &args[0], &Type::Ref(Box::new(Type::Int)))?;
        b.ins(&Instruction::LocalSet(c));
        b.ins(&Instruction::LocalGet(c));
        b.ins(&Instruction::I64Load(word8()));
        b.ins(&Instruction::LocalGet(c));
        b.ins(&Instruction::I32Const(8));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::I64Load(word8()));
        b.ins(&Instruction::Call(self.cx.rt.cell_addr));
        b.ins(&Instruction::Drop);
        let src = b.local(ValType::I32);
        b.ins(&Instruction::LocalGet(c));
        b.ins(&Instruction::I64Load(word8()));
        b.ins(&Instruction::Call(self.cx.rt.cell_srcp));
        b.ins(&Instruction::I32Load(word()));
        b.ins(&Instruction::LocalTee(src));
        b.ins(&Instruction::I32Eqz);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        let msg = self.cx.rt.intern(m, "error: no stream behind this cursor\n");
        b.ins(&Instruction::I32Const(msg as i32));
        b.ins(&Instruction::Call(self.cx.rt.trap));
        self.depth -= 1;
        b.ins(&Instruction::End);

        // The element, then the `Option` this call's own signature owes.
        let r = self.cx.repr(&elem, line)?;
        let place = self.place_for(b, &r, line)?;
        let has = self.stream_next(m, b, src, place, &elem, line)?;
        let ooff = b.alloc(ol.size, ol.align);
        b.slot(ooff);
        b.ins(&Instruction::LocalGet(has));
        b.ins(&Instruction::I32Store8(byte()));
        for a in [ooff + ol.fields[1], ooff + ol.fields[2]] {
            b.slot(a);
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
            place.addr(b, 0).ok_or_else(|| gap("a two-word payload with no address", line))?;
            b.ins(&Instruction::I32Const(16));
            b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
                place.addr(b, 0).ok_or_else(|| gap("a boxed payload with no address", line))?;
                self.box_value(b, t, line)?;
                b.ins(&Instruction::I64ExtendI32U);
            }
        }
        b.ins(&Instruction::I64Store(word8()));
        Ok(())
    }

    /// A stream's release. A buffer's is nothing — this backend's allocator is a
    /// bump pointer that never frees, exactly as for `Stmt::Drop` of an array —
    /// but a producer owns a cursor cell, and a cell is a slot in a slab of
    /// 65536 that a leak WOULD exhaust. So the branch is real here even though
    /// half of it is empty.
    fn stream_release(
        &mut self,
        b: &mut Frame,
        place: Place,
        line: usize,
    ) -> Result<(), String> {
        // A stream is an aggregate, so a `Place::Local` holding one holds its
        // ADDRESS — the opposite of what it means for a scalar, and the reason
        // this does not just call `place.addr`.
        if let Place::Local(a) = place {
            return self.stream_release_at(b, a, line);
        }
        let a = b.local(ValType::I32);
        place
            .addr(b, 0)
            .ok_or_else(|| gap("a stream with no address", line))?;
        b.ins(&Instruction::LocalSet(a));
        self.stream_release_at(b, a, line)
    }

    /// One release is a WALK since M2c: a wrapper holds the stream it wraps in
    /// its own cursor cell, so a chain of three combinators over one producer is
    /// four streams and this loop visits each of them once. A loop rather than a
    /// recursion because a chain is a list — the textual backend runs the same
    /// one, spelled as a `phi`.
    fn stream_release_at(&mut self, b: &mut Frame, a: u32, line: usize) -> Result<(), String> {
        let sl = self.stream_layout(line)?;
        let cur = b.local(ValType::I32);
        let src = b.local(ValType::I32);
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::LocalSet(cur));

        let out = self.depth;
        b.ins(&Instruction::Block(BlockType::Empty));
        self.depth += 1;
        let again = self.depth;
        b.ins(&Instruction::Loop(BlockType::Empty));
        self.depth += 1;

        // A buffer holds nothing this allocator can hand back.
        b.ins(&Instruction::LocalGet(cur));
        b.ins(&Instruction::I64Load(at(sl.fields[2])));
        b.ins(&Instruction::I64Const(0));
        b.ins(&Instruction::I64LtS);
        let leave = self.br_to(out);
        b.ins(&Instruction::BrIf(leave));

        // The generation check, then the cell's two halves: the stream behind it
        // (null unless this is a wrapper) and the slot itself.
        b.ins(&Instruction::LocalGet(cur));
        b.ins(&Instruction::I64Load(at(sl.fields[4])));
        b.ins(&Instruction::LocalGet(cur));
        b.ins(&Instruction::I64Load(at(sl.fields[5])));
        b.ins(&Instruction::Call(self.cx.rt.cell_addr));
        b.ins(&Instruction::Drop);
        b.ins(&Instruction::LocalGet(cur));
        b.ins(&Instruction::I64Load(at(sl.fields[4])));
        b.ins(&Instruction::Call(self.cx.rt.cell_srcp));
        b.ins(&Instruction::I32Load(word()));
        b.ins(&Instruction::LocalSet(src));
        b.ins(&Instruction::LocalGet(cur));
        b.ins(&Instruction::I64Load(at(sl.fields[4])));
        b.ins(&Instruction::Call(self.cx.rt.cell_release));

        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::I32Eqz);
        b.ins(&Instruction::BrIf(leave));
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::LocalSet(cur));
        let back = self.br_to(again);
        b.ins(&Instruction::Br(back));

        self.depth -= 1;
        b.ins(&Instruction::End);
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
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
        let opt = Type::Option(Box::new(elem.clone()));
        let Repr::Agg(ol) = self.cx.repr(&opt, line)? else {
            return unsupported("an Option that is not an aggregate", line);
        };
        let dsig = self.dispatcher(m, &stream_step_sig(elem), line)?;
        let ooff = b.alloc(ol.size, ol.align);
        b.slot(ooff);
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::I32Const(sl.fields[2] as i32));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalGet(s));
        b.ins(&Instruction::I32Const(sl.fields[4] as i32));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::Call(dsig.index));
        let oaddr = b.local(ValType::I32);
        b.slot(ooff);
        b.ins(&Instruction::LocalSet(oaddr));
        let sum = Sum::Opt(elem.clone());
        self.tag_test(b, oaddr, &sum, &Pattern::Some(String::new()), line)?;
        b.ins(&Instruction::If(BlockType::Empty));
        let got = self.bind_payload(b, oaddr, &sum, &ol, 0, elem, line)?;
        match (place, got, &r) {
            (Place::Local(d), Place::Local(v), _) => {
                b.ins(&Instruction::LocalGet(v));
                b.ins(&Instruction::LocalSet(d));
            }
            (Place::Slot(d), src, Repr::Agg(el)) => {
                b.slot(d);
                src.addr(b, 0).ok_or_else(|| gap("a stream payload in a local", line))?;
                b.ins(&Instruction::I32Const(el.size as i32));
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
            }
            _ => return unsupported("a stream element of this shape", line),
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
        // The stream's own release frame, so a `break` or an early `return` out
        // of the body leaves through it — `emit_releases_above` walks it.
        self.releases.push(vec![(Place::Local(s), Rel::Stream)]);
        let cont = self.depth;
        b.ins(&Instruction::Block(BlockType::Empty));
        self.depth += 1;
        // The boundary sits ABOVE the stream's frame: a `break` leaves the loop
        // through `fend`, which releases it, so releasing it here as well would
        // be twice. An early `return` releases from 0 and so does include it.
        self.loops.push((brk, cont, self.releases.len(), self.region_depth));
        self.block(m, b, body)?;
        self.loops.pop();
        self.depth -= 1;
        b.ins(&Instruction::End);
        self.releases.pop();
        self.scope.truncate(mark);

        let back = self.br_to(top);
        b.ins(&Instruction::Br(back));
        self.depth -= 1;
        b.ins(&Instruction::End);
        self.depth -= 1;
        b.ins(&Instruction::End);

        // Normal end and `break` both land here.
        self.stream_release_at(b, s, line)
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
                Walk { data, len, stride, elem: *inner, byte: false }
            }
            // A fixed array is its own buffer: the slot address IS element 0,
            // and the length is in the type.
            Type::ArrayN(inner, n) => {
                b.ins(&Instruction::I64Const(n as i64));
                b.ins(&Instruction::LocalSet(len));
                let stride = self.stride(&inner, line)?;
                Walk { data: addr, len, stride, elem: *inner, byte: false }
            }
            Type::Str => {
                b.ins(&Instruction::LocalGet(addr));
                b.ins(&Instruction::Call(self.cx.rt.strlen));
                b.ins(&Instruction::I64ExtendI32U);
                b.ins(&Instruction::LocalSet(len));
                Walk { data: addr, len, stride: 1, elem: Type::Int, byte: true }
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
                Walk { data: base, len: sl, stride, elem: *inner, byte: false }
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

    /// Trap unless `idx` is in `0..len`.
    ///
    /// The index is in the message, so it cannot be one interned string — hence
    /// `trap_idx(prefix, i, suffix)` rather than the plain `trap` the arithmetic
    /// checks use. Unsigned, so a negative index is caught by the same compare.
    fn bounds_check(&mut self, b: &mut Frame, w: &Walk, idx: u32, string: bool) {
        let (pre, post, trap) = (
            if string { self.cx.rt.msg_soob } else { self.cx.rt.msg_aoob },
            self.cx.rt.msg_oob_end,
            self.cx.rt.trap_idx,
        );
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::LocalGet(w.len));
        b.ins(&Instruction::I64GeU);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        b.ins(&Instruction::I32Const(pre as i32));
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I32Const(post as i32));
        b.ins(&Instruction::Call(trap));
        self.depth -= 1;
        b.ins(&Instruction::End);
    }

    /// Trap unless all four of `idx..idx+3` are in `0..len` (RFC-0083 M2).
    ///
    /// ONE branch for the whole vector — the amortisation that is the point of a
    /// vector load, and what a scalar loop cannot express. Two compares rather
    /// than [`bounds_check`]'s one because the unsigned trick does not survive a
    /// span: `idx + 4` wraps for a huge `idx` and would let the access through,
    /// while `len - 4` cannot wrap because `len >= 0`.
    fn bounds_check_span(&mut self, b: &mut Frame, w: &Walk, idx: u32) {
        let (pre, post, trap) = (self.cx.rt.msg_aoob, self.cx.rt.msg_oob_end, self.cx.rt.trap_idx);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I64Const(0));
        b.ins(&Instruction::I64LtS);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::LocalGet(w.len));
        b.ins(&Instruction::I64Const(4));
        b.ins(&Instruction::I64Sub);
        b.ins(&Instruction::I64GtS);
        b.ins(&Instruction::I32Or);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        b.ins(&Instruction::I32Const(pre as i32));
        // The first lane of `idx..idx+3` actually out of range: `idx` when it is
        // negative, `idx + 3` when the tail overruns. Reporting `idx` alone would
        // name an in-range element in the common case, and this is the cold path.
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I64Const(3));
        b.ins(&Instruction::I64Add);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I64Const(0));
        b.ins(&Instruction::I64LtS);
        b.ins(&Instruction::Select);
        b.ins(&Instruction::I32Const(post as i32));
        b.ins(&Instruction::Call(trap));
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
                return unsupported("an empty array literal with no expected `Array<T>` type", line);
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
        let elem = match elem_want {
            Some(t) => t,
            None => self.peek(&elems[0], line)?,
        };
        let stride = self.stride(&elem, line)?;
        let el = self.layout_of(&elem, line)?;
        let off = b.alloc(stride * elems.len() as u32, el.align);
        let r = self.cx.repr(&elem, line)?;
        for (i, e) in elems.iter().enumerate() {
            b.slot(off + stride * i as u32);
            self.expr_as(m, b, e, &elem)?;
            match &r {
                Repr::Scalar(_) => {
                    b.ins(&store_of(&self.cx.ll(&elem)));
                }
                Repr::Agg(_) => {
                    b.ins(&Instruction::I32Const(stride as i32));
                    b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                }
                Repr::Unit => return unsupported("an array of Unit", line),
            }
        }
        b.slot(off);
        Ok(Type::ArrayN(Box::new(elem), elems.len()))
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
        let bytes = (self.stride(from, line)? * n as u32) as i32;
        let buf = b.local(ValType::I32);
        b.ins(&Instruction::I64Const(bytes.max(1) as i64));
        b.ins(&Instruction::Call(self.cx.rt.malloc));
        b.ins(&Instruction::LocalTee(buf));
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::I32Const(bytes));
        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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

    /// `xs.push(v)` — the value, and a NEW triple describing the array with it
    /// in. The parser turns the statement into `xs = push(xs, v)`, so the
    /// write-back is an ordinary assignment and this never touches the binding.
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
        let Type::Array(elem) = self.cx.resolve(&aty) else {
            return unsupported(&format!("`push` onto `{aty}`"), line);
        };
        let elem = *elem;
        let l = self.layout_of(&aty, line)?;
        let stride = self.stride(&elem, line)? as i32;
        let (src, data, len, cap) = (
            b.local(ValType::I32),
            b.local(ValType::I32),
            b.local(ValType::I64),
            b.local(ValType::I64),
        );
        b.ins(&Instruction::LocalSet(src));
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::I32Load(word_at(l.fields[0])));
        b.ins(&Instruction::LocalSet(data));
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::I64Load(at(l.fields[1])));
        b.ins(&Instruction::LocalSet(len));
        b.ins(&Instruction::LocalGet(src));
        b.ins(&Instruction::I64Load(at(l.fields[2])));
        b.ins(&Instruction::LocalSet(cap));

        // Full: 0 → 4, else double. Growing means allocating and copying rather
        // than `realloc`ing, because this backend's allocator is a bump pointer
        // that never frees (see `runtime`) — the old buffer is simply abandoned.
        b.ins(&Instruction::LocalGet(len));
        b.ins(&Instruction::LocalGet(cap));
        b.ins(&Instruction::I64Eq);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        b.ins(&Instruction::LocalGet(cap));
        b.ins(&Instruction::I64Eqz);
        b.ins(&Instruction::If(BlockType::Result(ValType::I64)));
        self.depth += 1;
        b.ins(&Instruction::I64Const(4));
        b.ins(&Instruction::Else);
        b.ins(&Instruction::LocalGet(cap));
        b.ins(&Instruction::I64Const(2));
        b.ins(&Instruction::I64Mul);
        self.depth -= 1;
        b.ins(&Instruction::End);
        b.ins(&Instruction::LocalSet(cap));
        let grown = b.local(ValType::I32);
        // `cap * stride` in 64 bits, which is the width `cap` already is. Wrapping
        // first is what made this the worst of the truncations: doubling a 2 GiB
        // buffer asks for 4 GiB, wrapped to 0, and the copy below is `len *
        // stride` — the OLD size, which does NOT wrap and does fit — so 2 GiB
        // went into a zero-byte allocation without tripping a bounds check.
        b.ins(&Instruction::LocalGet(cap));
        b.ins(&Instruction::I64Const(stride as i64));
        b.ins(&Instruction::I64Mul);
        b.ins(&Instruction::Call(self.cx.rt.malloc));
        b.ins(&Instruction::LocalTee(grown));
        b.ins(&Instruction::LocalGet(data));
        b.ins(&Instruction::LocalGet(len));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::I32Const(stride));
        b.ins(&Instruction::I32Mul);
        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
        b.ins(&Instruction::LocalGet(grown));
        b.ins(&Instruction::LocalSet(data));
        self.depth -= 1;
        b.ins(&Instruction::End);

        let w = Walk { data, len, stride: stride as u32, elem: elem.clone(), byte: false };
        self.elem_addr(b, &w, len);
        let r = self.cx.repr(&elem, line)?;
        self.expr_as(m, b, &args[1], &elem)?;
        match &r {
            Repr::Scalar(_) => {
                b.ins(&store_of(&self.cx.ll(&elem)));
            }
            Repr::Agg(_) => {
                b.ins(&Instruction::I32Const(stride));
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
            }
            Repr::Unit => return unsupported("an array of Unit", line),
        }
        let off = b.alloc(l.size, l.align);
        b.slot(off + l.fields[0]);
        b.ins(&Instruction::LocalGet(data));
        b.ins(&Instruction::I32Store(word()));
        b.slot(off + l.fields[1]);
        b.ins(&Instruction::LocalGet(len));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Add);
        b.ins(&Instruction::I64Store(word8()));
        b.slot(off + l.fields[2]);
        b.ins(&Instruction::LocalGet(cap));
        b.ins(&Instruction::I64Store(word8()));
        b.slot(off);
        Ok(Type::Array(Box::new(elem)))
    }

    /// `xs[i]` — bounds-checked, and a String's `s[i]` with it.
    fn at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let aty = self.expr(m, b, &args[0])?;
        // A Map is not walkable and must not be reached as one: its length is
        // field 2 where an Array's is field 1, so a `Walk` over it would index off
        // the value pointer (M2c's refusal, now a branch instead).
        if let Type::Map(_, val) = self.cx.resolve(&aty) {
            let mty = self.cx.resolve(&aty);
            return self.map_at(m, b, &mty, &val, &args[1], line);
        }
        let string = self.cx.resolve(&aty) == Type::Str;
        let w = self.walk(b, &aty, line)?;
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
            return Ok(Type::IntN { bits: 8, signed: false });
        }
        self.load_elem(b, &w, line)?;
        Ok(w.elem)
    }

    /// `xs.pop()` → `Option<T>`, shrinking the binding in place. Variable-only,
    /// which is the checker's rule too: it returns a value AND mutates, so there
    /// is no assignment the parser could have desugared it into.
    fn pop(
        &mut self,
        b: &mut Frame,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        let (place, aty) = self.receiver(args, "pop", line)?;
        // The binding's ADDRESS, taken once: `pop` shrinks the triple in place, so
        // it needs the storage rather than the value — and module state is storage
        // at a fixed address exactly as a frame slot is at a moving one.
        let slot = b.local(ValType::I32);
        place.addr(b, 0).ok_or_else(|| gap("`pop` on a non-array binding", line))?;
        b.ins(&Instruction::LocalSet(slot));
        let Type::Array(elem) = self.cx.resolve(&aty) else {
            return unsupported(&format!("`pop` on `{aty}`"), line);
        };
        let elem = *elem;
        let al = self.layout_of(&aty, line)?;
        let opt = Type::Option(Box::new(elem.clone()));
        let ol = self.layout_of(&opt, line)?;
        let out = b.alloc(ol.size, ol.align);
        // `None` first, then the `Some` arm overwrites the tag and the payload:
        // one destination, filled in place, which is destination-first with the
        // trivial arm pre-applied.
        b.slot(out + ol.fields[0]);
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32Store8(byte()));
        for f in [ol.fields[1], ol.fields[2]] {
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
        b.ins(&Instruction::I32Const(1));
        b.ins(&Instruction::I32Store8(byte()));
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
        place.addr(b, 0).ok_or_else(|| gap("`swapRemove` on a non-array binding", line))?;
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
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
    /// Release the cell a `Ref` binding names: check the generation, then hand the
    /// slot back. The payload is not freed, because a bump allocator has no free.
    ///
    /// `{ i64 slot, i64 generation }` whatever the `T`, so the layout is asked for
    /// once with a stand-in rather than threaded through every caller.
    fn emit_release(&mut self, b: &mut Frame, place: Place, line: usize) -> Result<(), String> {
        let l = self.layout_of(&Type::Ref(Box::new(Type::Int)), line)?;
        let r = self.scratch(b, ValType::I32, 6);
        if place.addr(b, 0).is_none() {
            return unsupported("a `Ref` held in a wasm local", line);
        }
        b.ins(&Instruction::LocalTee(r));
        b.ins(&Instruction::I64Load(at(l.fields[0])));
        b.ins(&Instruction::LocalGet(r));
        b.ins(&Instruction::I64Load(at(l.fields[1])));
        b.ins(&Instruction::Call(self.cx.rt.cell_addr));
        b.ins(&Instruction::Drop);
        b.ins(&Instruction::LocalGet(r));
        b.ins(&Instruction::I64Load(at(l.fields[0])));
        b.ins(&Instruction::Call(self.cx.rt.cell_release));
        Ok(())
    }

    /// Evaluate a `Ref<T>` expression and leave its generation-checked payload
    /// address on the stack, giving `T`.
    ///
    /// One helper for all four operations because the check is not optional on any
    /// of them: `get`, `set`, `release` and a `drop` of a reference all trap on a
    /// stale handle, which is the whole of what Path B buys over dangling.
    fn ref_addr(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        e: &Expr,
        line: usize,
    ) -> Result<Type, String> {
        let rty = self.expr(m, b, e)?;
        let Type::Ref(elem) = self.cx.resolve(&rty) else {
            return unsupported(&format!("a reference operation on `{rty}`"), line);
        };
        let l = self.layout_of(&rty, line)?;
        let r = self.scratch(b, ValType::I32, 6);
        b.ins(&Instruction::LocalTee(r));
        b.ins(&Instruction::I64Load(at(l.fields[0])));
        b.ins(&Instruction::LocalGet(r));
        b.ins(&Instruction::I64Load(at(l.fields[1])));
        b.ins(&Instruction::Call(self.cx.rt.cell_addr));
        Ok(*elem)
    }

    fn receiver(
        &mut self,
        args: &[Expr],
        what: &str,
        line: usize,
    ) -> Result<(Place, Type), String> {
        match args.first() {
            Some(Expr::Var { name, .. }) => self.lookup(name, line),
            _ => unsupported(&format!("`{what}` on something that is not a variable"), line),
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
enum Sum {
    Opt(Type),
    Res(Type, Type),
    Enum(Vec<EnumVariant>),
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

impl Fn_<'_> {
    fn sum_of(&self, ty: &Type) -> Option<Sum> {
        match self.cx.resolve(ty) {
            Type::Option(t) => Some(Sum::Opt(*t)),
            Type::Result(a, b) => Some(Sum::Res(*a, *b)),
            Type::Enum(vs) => Some(Sum::Enum(vs)),
            _ => None,
        }
    }

    /// How an `Option`/`Result` payload of type `t` fills its two words.
    fn word2(&self, t: &Type) -> Result<Word, String> {
        Ok(match self.cx.repr(t, 0)? {
            Repr::Scalar(ValType::I64) => Word::Direct,
            Repr::Scalar(v @ (ValType::F64 | ValType::F32)) => Word::Float(v),
            Repr::Scalar(v) => Word::Ext(v),
            Repr::Agg(_) if self.cx.ll(t) == "{ i64, i64 }" => Word::Inline2,
            _ => Word::Boxed,
        })
    }

    /// How a user-enum payload of type `t` fills its ONE word: an `i64` is the
    /// word, and everything else is a pointer to itself.
    fn word1(&self, t: &Type) -> Word {
        if self.cx.ll(t) == "i64" {
            Word::Direct
        } else {
            Word::Boxed
        }
    }

    /// Copy the value on the stack (a scalar, or an aggregate's address) onto
    /// the heap, leaving its address as an `i64` word.
    fn box_value(&mut self, b: &mut Frame, t: &Type, line: usize) -> Result<(), String> {
        let malloc = self.cx.rt.malloc;
        let ll = self.cx.ll(t);
        match self.cx.repr(t, line)? {
            Repr::Scalar(v) => {
                let size = layout::of_ll(&ll).map_err(|e| format!("direct backend: {e}"))?.size;
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
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                b.ins(&Instruction::LocalGet(p));
            }
            Repr::Unit => return unsupported("a Unit payload", line),
        }
        b.ins(&Instruction::I64ExtendI32U);
        Ok(())
    }

    /// Build an `Option`/`Result` value: the tag, then the two payload words.
    fn build_sum2(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        ty: &Type,
        tag: i32,
        payload: Option<(&Expr, Type)>,
        line: usize,
    ) -> Result<Type, String> {
        let Repr::Agg(l) = self.cx.repr(ty, line)? else {
            return unsupported("a sum that is not an aggregate", line);
        };
        let off = b.alloc(l.size, l.align);
        b.slot(off);
        b.ins(&Instruction::I32Const(tag));
        b.ins(&Instruction::I32Store8(byte()));
        let (w0, w1) = (off + l.fields[1], off + l.fields[2]);
        match payload {
            None => {
                for a in [w0, w1] {
                    b.slot(a);
                    b.ins(&Instruction::I64Const(0));
                    b.ins(&Instruction::I64Store(word8()));
                }
            }
            Some((e, t)) if self.word2(&t)? == Word::Inline2 => {
                // Two words already side by side: one copy, no encoding.
                b.slot(w0);
                self.expr_as(m, b, e, &t)?;
                b.ins(&Instruction::I32Const(16));
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
            }
            Some((e, t)) => {
                b.slot(w0);
                self.expr_as(m, b, e, &t)?;
                match self.word2(&t)? {
                    Word::Direct => {}
                    Word::Ext(_) => {
                        b.ins(&Instruction::I64ExtendI32U);
                    }
                    Word::Float(v) => float_into_word(b, v),
                    _ => self.box_value(b, &t, line)?,
                }
                b.ins(&Instruction::I64Store(word8()));
                b.slot(w1);
                b.ins(&Instruction::I64Const(0));
                b.ins(&Instruction::I64Store(word8()));
            }
        }
        b.slot(off);
        Ok(ty.clone())
    }

    /// Build a user-enum value: the tag, then one word per payload.
    fn build_enum(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        ty: &Type,
        tag: u64,
        args: &[Expr],
        payload: &[Type],
        line: usize,
    ) -> Result<Type, String> {
        if args.len() != payload.len() {
            return unsupported("an enum variant at this arity", line);
        }
        let Repr::Agg(l) = self.cx.repr(ty, line)? else {
            return unsupported("an enum that is not an aggregate", line);
        };
        let off = b.alloc(l.size, l.align);
        b.slot(off);
        b.ins(&Instruction::I64Const(tag as i64));
        b.ins(&Instruction::I64Store(word8()));
        for (i, (a, t)) in args.iter().zip(payload).enumerate() {
            b.slot(off + l.fields[1 + i]);
            self.expr_as(m, b, a, t)?;
            if self.word1(t) == Word::Boxed {
                self.box_value(b, t, line)?;
            }
            b.ins(&Instruction::I64Store(word8()));
        }
        b.slot(off);
        Ok(ty.clone())
    }

    /// The sum type an expectation names, if it names one.
    fn expected_sum(&self) -> Option<Type> {
        self.expect.last().filter(|t| self.sum_of(t).is_some()).cloned()
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
    ) -> Result<Option<Type>, String> {
        let want = self.expected_sum();
        match name {
            "None" => {
                let ty = want.ok_or_else(|| gap("a `None` with no expected Option type", line))?;
                return self.build_sum2(m, b, &ty, 0, None, line).map(Some);
            }
            "Some" | "Ok" | "Err" => {
                if args.len() != 1 {
                    return unsupported(&format!("`{name}` at this arity"), line);
                }
                // The payload's type is the position's, not the argument's — a
                // `Some(0)` in an `Option<UInt8>` slot is a UInt8.
                let picked = want.as_ref().and_then(|t| self.sum_of(t).map(|s| (t.clone(), s)));
                let (ty, payload) = match picked {
                    Some((t, Sum::Opt(p))) if name == "Some" => (t, p),
                    Some((t, Sum::Res(ok, er))) if name != "Some" => {
                        (t, if name == "Ok" { ok } else { er })
                    }
                    // An unexpected `Some` still types itself from its payload;
                    // `Ok`/`Err` cannot, because the other half is unknowable.
                    _ if name == "Some" => {
                        let p = self.peek(&args[0], line)?;
                        (Type::Option(Box::new(p.clone())), p)
                    }
                    _ => {
                        return unsupported(&format!("`{name}` with no expected Result type"), line);
                    }
                };
                let tag = i32::from(name != "Err");
                return self
                    .build_sum2(m, b, &ty, tag, Some((&args[0], payload)), line)
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
        self.build_enum(m, b, &ty, tag, args, &payload, line).map(Some)
    }

    /// What a pattern binds, and to what — without emitting anything, because a
    /// join needs the arm's type before the arm exists.
    fn pattern_binds(
        &self,
        sum: &Sum,
        pat: &Pattern,
        line: usize,
    ) -> Result<Vec<(String, Type)>, String> {
        Ok(match (sum, pat) {
            (Sum::Opt(t), Pattern::Some(n)) => vec![(n.clone(), t.clone())],
            (Sum::Opt(_), Pattern::None) => vec![],
            (Sum::Res(t, _), Pattern::Ok(n)) => vec![(n.clone(), t.clone())],
            (Sum::Res(_, e), Pattern::Err(n)) => vec![(n.clone(), e.clone())],
            // `??`'s type-agnostic pair (RFC-0079) — the sum decides which side
            // each names, which is the same thing `try_` does one screen down.
            (Sum::Opt(t), Pattern::Success(n)) | (Sum::Res(t, _), Pattern::Success(n)) => {
                vec![(n.clone(), t.clone())]
            }
            (Sum::Opt(_), Pattern::Failure(_)) => vec![],
            (Sum::Res(_, e), Pattern::Failure(n)) => vec![(n.clone(), e.clone())],
            (Sum::Enum(vs), Pattern::Variant(name, binds)) => {
                let v = vs
                    .iter()
                    .find(|v| v.name == *name)
                    .ok_or_else(|| gap(&format!("the variant `{name}`"), line))?;
                if v.payload.len() != binds.len() {
                    return unsupported(&format!("the variant `{name}` at this arity"), line);
                }
                binds.iter().cloned().zip(v.payload.iter().cloned()).collect()
            }
            _ => return unsupported("a pattern of the wrong shape for its scrutinee", line),
        })
    }

    /// `match` — the n-way join M0 warned about, lowered destination-first.
    ///
    /// The arms are a chain of `if`s inside one `block`, and each arm leaves by
    /// branching to it: a scalar result rides the branch, an aggregate one is
    /// copied into a slot allocated BEFORE the first test. Nothing here counts
    /// arms, which is the property that makes 46 four-to-seven-way joins cost
    /// exactly what 103 diamonds cost.
    fn match_expr(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        scrutinee: &Expr,
        arms: &[MatchArm],
        line: usize,
    ) -> Result<Type, String> {
        let st = self.expr(m, b, scrutinee)?;
        let sum = self.sum_of(&st).ok_or_else(|| gap(&format!("a `match` on `{st}`"), line))?;
        let addr = self.scratch(b, ValType::I32, 3);
        b.ins(&Instruction::LocalSet(addr));
        let Repr::Agg(sl) = self.cx.repr(&st, line)? else {
            return unsupported("a `match` on a non-aggregate", line);
        };
        // The arms' common type — [`Fn_::match_ty`], the same answer `peek` gives a
        // `match` in a branch. `expr_as` re-checks every arm against it, so a wrong
        // guess here is a compile error rather than a miscompile.
        let want = self.match_ty(&sum, arms, line)?;
        let want = self.join_ty(want);
        let r = self.cx.repr(&want, line)?;

        let dest = match &r {
            Repr::Agg(l) => Some((b.alloc(l.size, l.align), l.size)),
            _ => None,
        };
        let out = self.depth;
        b.ins(&Instruction::Block(match &r {
            Repr::Scalar(v) => BlockType::Result(*v),
            _ => BlockType::Empty,
        }));
        self.depth += 1;

        for arm in arms {
            self.tag_test(b, addr, &sum, &arm.pattern, line)?;
            b.ins(&Instruction::If(BlockType::Empty));
            self.depth += 1;

            let mark = self.scope.len();
            let binds = self.pattern_binds(&sum, &arm.pattern, line)?;
            for (i, (n, t)) in binds.into_iter().enumerate() {
                let place = self.bind_payload(b, addr, &sum, &sl, i, &t, line)?;
                self.scope.push((n, place, t));
            }
            match dest {
                Some((off, size)) => {
                    b.slot(off);
                    self.expr_as(m, b, &arm.body, &want)?;
                    b.ins(&Instruction::I32Const(size as i32));
                    b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                }
                None => self.expr_as(m, b, &arm.body, &want)?,
            }
            self.scope.truncate(mark);
            let d = self.br_to(out);
            b.ins(&Instruction::Br(d));

            self.depth -= 1;
            b.ins(&Instruction::End);
        }
        // The checker proves the arms exhaustive; the validator cannot see the
        // proof, so it is told instead.
        b.ins(&Instruction::Unreachable);
        self.depth -= 1;
        b.ins(&Instruction::End);
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
    /// frame or skip a copy-back. (`drop` is a no-op in this backend, so the
    /// textual emitter's `emit_all_drops` before its `ret` has nothing to answer.)
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
    ) -> Result<Type, String> {
        let st = self.expr(m, b, e)?;
        // The success pattern's binder name is unread — `tag_test` and
        // `bind_payload` both take the type from `sum`, not from the pattern — so
        // it is spelled empty rather than invented.
        let (sum, ok_ty, ok_pat) = match self.sum_of(&st) {
            Some(Sum::Opt(t)) => (Sum::Opt(t.clone()), t, Pattern::Some(String::new())),
            Some(Sum::Res(t, err)) => (Sum::Res(t.clone(), err), t, Pattern::Ok(String::new())),
            // Anything else asks `Fallible` (RFC-0080 M3) instead of the tag.
            _ => return self.try_fallible(m, b, &st, line),
        };
        let Repr::Agg(sl) = self.cx.repr(&st, line)? else {
            return unsupported("`?` on a non-aggregate sum", line);
        };
        // The propagated value is the WHOLE sum, byte for byte, which is only
        // sound if the two are the same shape — `{ i1, i64, i64 }` on both sides,
        // differing at most in a payload half the failing tag says is not there.
        // The textual backend gets this for free (`ret { i1, i64, i64 } %agg`); a
        // memcpy has a width, so the width is checked rather than assumed.
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
        b.ins(&Instruction::LocalGet(self.dest.expect("an aggregate return has a destination")));
        b.ins(&Instruction::LocalGet(addr));
        b.ins(&Instruction::I32Const(rl.size as i32));
        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
        // `?` is `Stmt::Return` minus the keyword, so it owes the same two
        // unwinds. It did not pay them: a `?` out of a `region` left the counter
        // raised, and the 65th such call aborted where the interpreter kept
        // going. The value is already copied through `dest`, so neither of these
        // can disturb it — the same reason the `return` arm does them here.
        self.emit_releases_above(b, 0)?;
        self.exit_regions_above(b, 0);
        b.ins(&Instruction::Br(self.depth));
        self.depth -= 1;
        b.ins(&Instruction::End);
        // Reusing `bind_payload` costs one local or slot that nothing else reads,
        // and buys the four payload shapes (direct, extended, inline pair, boxed)
        // already being right here because they are right in `match`.
        let place = self.bind_payload(b, addr, &sum, &sl, 0, &ok_ty, line)?;
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
        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
        let mark = self.scope.len();
        self.scope.push(("@try".to_string(), Place::Slot(off), st.clone()));
        let recv = [Expr::Var { name: "@try".to_string(), line }];

        self.call(m, b, &ftypes::impl_method_name(ftypes::FALLIBLE, &key, "isSuccess"), &recv, line)?;
        b.ins(&Instruction::I32Eqz);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        b.ins(&Instruction::LocalGet(self.dest.expect("an aggregate return has a destination")));
        b.slot(off);
        b.ins(&Instruction::I32Const(sl.size as i32));
        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
        // The same two unwinds `?` owes as `return`-minus-the-keyword.
        self.emit_releases_above(b, 0)?;
        self.exit_regions_above(b, 0);
        b.ins(&Instruction::Br(self.depth));
        self.depth -= 1;
        b.ins(&Instruction::End);

        let out = self.call(m, b, &ftypes::impl_method_name(ftypes::FALLIBLE, &key, "success"), &recv, line);
        self.scope.truncate(mark);
        out
    }

    /// `Age?(n)` — a validated construction whose refinement answers with a tag
    /// instead of a trap, yielding `Option<Age>` (RFC-0003).
    ///
    /// This is the one flow that deliberately steps AROUND the M2d coercion seam,
    /// and the reason is the whole point of the form: `expr_as(n, Age)` would emit
    /// the validation that aborts. So the argument is evaluated at the refinement's
    /// BASE type and the predicate's own answer becomes the tag — the same thing
    /// the textual backend's `gen_try_construct` does, and it has to be the same
    /// thing, because a value the two disagree about is a diverging `None`.
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
        let ty = Type::Option(Box::new(Type::Named(name.to_string())));
        let Repr::Agg(l) = self.cx.repr(&ty, line)? else {
            return unsupported("a fallible construction of a non-aggregate Option", line);
        };
        let base = decl.base.clone();
        self.expr_as(m, b, &args[0], &base)?;
        // `predicate_holds` parks the value where the `where` clause binds it, so
        // both halves of the answer are in locals before either store.
        let (held, base_v) = match self.cx.repr(&base, line)? {
            Repr::Scalar(v) => (self.predicate_holds(m, b, &decl, line)?, v),
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
        b.ins(&Instruction::I32Store8(byte()));
        b.slot(off + l.fields[1]);
        b.ins(&Instruction::LocalGet(held));
        self.encode_word2(b, &base, line)?;
        b.ins(&Instruction::I64Store(word8()));
        b.slot(off + l.fields[2]);
        b.ins(&Instruction::I64Const(0));
        b.ins(&Instruction::I64Store(word8()));
        b.slot(off);
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
        b.ins(&Instruction::LocalGet(addr));
        match (sum, pat) {
            (Sum::Enum(vs), Pattern::Variant(name, _)) => {
                let tag = vs
                    .iter()
                    .position(|v| v.name == *name)
                    .ok_or_else(|| gap(&format!("the variant `{name}`"), line))?;
                b.ins(&Instruction::I64Load(word8()));
                b.ins(&Instruction::I64Const(tag as i64));
                b.ins(&Instruction::I64Eq);
            }
            (_, p) => {
                let one = matches!(p, Pattern::Some(_) | Pattern::Ok(_) | Pattern::Success(_));
                b.ins(&Instruction::I32Load8U(byte()));
                b.ins(&Instruction::I32Const(i32::from(one)));
                b.ins(&Instruction::I32Eq);
            }
        }
        Ok(())
    }

    /// Bind payload `i` of the matched variant out of the sum at `addr`.
    fn bind_payload(
        &mut self,
        b: &mut Frame,
        addr: u32,
        sum: &Sum,
        sl: &Layout,
        i: usize,
        t: &Type,
        line: usize,
    ) -> Result<Place, String> {
        let is_enum = matches!(sum, Sum::Enum(_));
        let off = sl.fields[1 + if is_enum { i } else { 0 }];
        let kind = if is_enum { self.word1(t) } else { self.word2(t)? };
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
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
                match self.cx.repr(t, line)? {
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
                        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
                        Place::Slot(slot)
                    }
                    Repr::Unit => return unsupported("a Unit payload", line),
                }
            }
        })
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
impl Fn_<'_> {
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
        let want = match self.expect.last().map(|t| self.cx.resolve(t)) {
            Some(Type::Map(_, v)) => Some(*v),
            _ => None,
        };
        let val = match (want, entries.first()) {
            (Some(v), _) => v,
            (None, Some((_, ve))) => self.peek(ve, line)?,
            // An empty literal in no map position at all. `Map<String, Int64>` is
            // what the textual backend defaults to, and the two have to agree
            // because the header is the same 24 bytes either way.
            (None, None) => Type::Int,
        };
        let mty = Type::Map(Box::new(Type::Str), Box::new(val.clone()));
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
        for (ke, ve) in entries {
            self.map_set(m, b, hdr, &l, ke, ve, &val, line)?;
        }
        b.slot(off);
        Ok(mty)
    }

    /// `m[k] = v` — update in place on a hit, append on a miss.
    ///
    /// `hdr` is a local holding the header's address.
    fn map_set(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        l: &Layout,
        key: &Expr,
        value: &Expr,
        val: &Type,
        line: usize,
    ) -> Result<(), String> {
        let esz = self.stride(val, line)? as i32;
        let r = self.cx.repr(val, line)?;
        // Key then value, before the scan: the textual backend evaluates both
        // first, and a side-effecting value expression must not run at a
        // different point on the two backends.
        let k = b.local(ValType::I32);
        self.expr_as(m, b, key, &Type::Str)?;
        b.ins(&Instruction::LocalSet(k));
        let v = b.local(match &r {
            Repr::Scalar(t) => *t,
            Repr::Agg(_) => ValType::I32,
            Repr::Unit => return unsupported("a Map of Unit", line),
        });
        self.expr_as(m, b, value, val)?;
        b.ins(&Instruction::LocalSet(v));

        let idx = b.local(ValType::I32);
        self.map_scan(b, hdr, l, k, idx);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32LtS);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        self.map_reserve(b, hdr, l, esz);
        // keys[len] = k, and the new entry's index IS the old length.
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
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[2])));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Add);
        b.ins(&Instruction::I64Store(at(l.fields[2])));
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
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
            }
            Repr::Unit => return unsupported("a Map of Unit", line),
        }
        Ok(())
    }

    /// `map_find(keys, len, k)` into `idx`.
    fn map_scan(&mut self, b: &mut Frame, hdr: u32, l: &Layout, k: u32, idx: u32) {
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[0])));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[2])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::LocalGet(k));
        b.ins(&Instruction::Call(self.cx.rt.map_find));
        b.ins(&Instruction::LocalSet(idx));
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

    /// Room for one more entry: 0 to 4, else double, growing BOTH buffers.
    ///
    /// Allocate-and-copy rather than realloc, for the reason `push` gives — this
    /// backend's allocator is a bump pointer and abandons the old buffer.
    fn map_reserve(&mut self, b: &mut Frame, hdr: u32, l: &Layout, esz: i32) {
        let (nc, nk, nv) = (b.local(ValType::I32), b.local(ValType::I32), b.local(ValType::I32));
        let len = b.local(ValType::I32);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[2])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::LocalTee(len));
        b.ins(&Instruction::I32Const(1));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[3])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::I32GtS);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[3])));
        b.ins(&Instruction::I64Eqz);
        b.ins(&Instruction::If(BlockType::Result(ValType::I32)));
        b.ins(&Instruction::I32Const(4));
        b.ins(&Instruction::Else);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[3])));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::I32Const(2));
        b.ins(&Instruction::I32Mul);
        b.ins(&Instruction::End);
        b.ins(&Instruction::LocalSet(nc));
        for (field, stride, into) in [(l.fields[0], 4i32, nk), (l.fields[1], esz, nv)] {
            // The count is safe in 32 bits — an entry costs at least twelve bytes,
            // so a wasm32 memory holds well under 2^31 of them — but `nc * stride`
            // is not, so the product is 64-bit and `malloc` decides.
            b.ins(&Instruction::LocalGet(nc));
            b.ins(&Instruction::I64ExtendI32U);
            b.ins(&Instruction::I64Const(stride as i64));
            b.ins(&Instruction::I64Mul);
            b.ins(&Instruction::Call(self.cx.rt.malloc));
            b.ins(&Instruction::LocalTee(into));
            b.ins(&Instruction::LocalGet(hdr));
            b.ins(&Instruction::I32Load(word_at(field)));
            b.ins(&Instruction::LocalGet(len));
            b.ins(&Instruction::I32Const(stride));
            b.ins(&Instruction::I32Mul);
            b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
            b.ins(&Instruction::LocalGet(hdr));
            b.ins(&Instruction::LocalGet(into));
            b.ins(&Instruction::I32Store(word_at(field)));
        }
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::LocalGet(nc));
        b.ins(&Instruction::I64ExtendI32U);
        b.ins(&Instruction::I64Store(at(l.fields[3])));
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
        let l = self.layout_of(mty, line)?;
        let esz = self.stride(val, line)? as i32;
        let hdr = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(hdr));
        let k = b.local(ValType::I32);
        self.expr_as(m, b, key, &Type::Str)?;
        b.ins(&Instruction::LocalSet(k));
        let idx = b.local(ValType::I32);
        self.map_scan(b, hdr, &l, k, idx);

        let oty = Type::Option(Box::new(val.clone()));
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
        b.ins(&Instruction::I32Const(1));
        b.ins(&Instruction::I32Store8(byte()));
        match self.word2(val)? {
            // Two words already side by side in the value buffer: one copy, and
            // nothing to encode. A `Ref`, or a stored `fn` (RFC-0037).
            Word::Inline2 => {
                b.slot(off + ol.fields[1]);
                self.map_val_addr(b, hdr, &l, idx, esz);
                b.ins(&Instruction::I32Const(16));
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
        let (hdr, mty) = if name == "@remove" {
            let (place, ty) = self.receiver(args, "remove", line)?;
            let hdr = b.local(ValType::I32);
            place.addr(b, 0).ok_or_else(|| gap("`remove` on a non-map binding", line))?;
            b.ins(&Instruction::LocalSet(hdr));
            (hdr, ty)
        } else {
            let ty = self.expr(m, b, &args[0])?;
            let hdr = b.local(ValType::I32);
            b.ins(&Instruction::LocalSet(hdr));
            (hdr, ty)
        };
        let Type::Map(_, val) = self.cx.resolve(&mty) else {
            return unsupported(&format!("`{name}` on `{mty}`"), line);
        };
        let l = self.layout_of(&mty, line)?;

        if name == "@keys" {
            // A snapshot `Array<String>`: the key POINTERS copied into a buffer of
            // its own, so the map may be mutated afterwards without disturbing it.
            let aty = Type::Array(Box::new(Type::Str));
            let al = self.layout_of(&aty, line)?;
            let (len, buf) = (b.local(ValType::I32), b.local(ValType::I32));
            b.ins(&Instruction::LocalGet(hdr));
            b.ins(&Instruction::I64Load(at(l.fields[2])));
            b.ins(&Instruction::I32WrapI64);
            b.ins(&Instruction::LocalTee(len));
            // An empty map still gets a buffer, so the triple's pointer is never
            // null — the same rule `bytes` follows, for the same `push`.
            b.ins(&Instruction::I32Const(1));
            b.ins(&Instruction::I32Add);
            b.ins(&Instruction::I32Const(4));
            b.ins(&Instruction::I32Mul);
            b.ins(&Instruction::I64ExtendI32U);
            b.ins(&Instruction::Call(self.cx.rt.malloc));
            b.ins(&Instruction::LocalTee(buf));
            b.ins(&Instruction::LocalGet(hdr));
            b.ins(&Instruction::I32Load(word_at(l.fields[0])));
            b.ins(&Instruction::LocalGet(len));
            b.ins(&Instruction::I32Const(4));
            b.ins(&Instruction::I32Mul);
            b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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

        let k = b.local(ValType::I32);
        self.expr_as(m, b, &args[1], &Type::Str)?;
        b.ins(&Instruction::LocalSet(k));
        let idx = b.local(ValType::I32);
        self.map_scan(b, hdr, &l, k, idx);
        let found = b.local(ValType::I32);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32GeS);
        b.ins(&Instruction::LocalSet(found));
        if name == "@remove" {
            // Shift the survivors down, so first-insertion order survives a
            // removal — which is why a remove-then-insert moves a key to the end.
            let esz = self.stride(&val, line)? as i32;
            let rest = b.local(ValType::I32);
            b.ins(&Instruction::LocalGet(found));
            b.ins(&Instruction::If(BlockType::Empty));
            self.depth += 1;
            b.ins(&Instruction::LocalGet(hdr));
            b.ins(&Instruction::I64Load(at(l.fields[2])));
            b.ins(&Instruction::I32WrapI64);
            b.ins(&Instruction::LocalGet(idx));
            b.ins(&Instruction::I32Sub);
            b.ins(&Instruction::I32Const(1));
            b.ins(&Instruction::I32Sub);
            b.ins(&Instruction::LocalSet(rest));
            for (field, stride) in [(l.fields[0], 4i32), (l.fields[1], esz)] {
                let base = b.local(ValType::I32);
                b.ins(&Instruction::LocalGet(hdr));
                b.ins(&Instruction::I32Load(word_at(field)));
                b.ins(&Instruction::LocalGet(idx));
                b.ins(&Instruction::I32Const(stride));
                b.ins(&Instruction::I32Mul);
                b.ins(&Instruction::I32Add);
                b.ins(&Instruction::LocalTee(base));
                b.ins(&Instruction::LocalGet(base));
                b.ins(&Instruction::I32Const(stride));
                b.ins(&Instruction::I32Add);
                b.ins(&Instruction::LocalGet(rest));
                b.ins(&Instruction::I32Const(stride));
                b.ins(&Instruction::I32Mul);
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
            }
            b.ins(&Instruction::LocalGet(hdr));
            b.ins(&Instruction::LocalGet(hdr));
            b.ins(&Instruction::I64Load(at(l.fields[2])));
            b.ins(&Instruction::I64Const(1));
            b.ins(&Instruction::I64Sub);
            b.ins(&Instruction::I64Store(at(l.fields[2])));
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
impl Fn_<'_> {
    /// `(len, cap, base)` of the SmallArray whose header is at `hdr`.
    ///
    /// `base` is the inline field's address while `cap == N`, else `data`. This is
    /// the branch RFC-0056 documents as the small-buffer trade-off, and the reason
    /// its benches show a read-heavy loop losing to `Array`.
    fn sa_parts(
        &mut self,
        b: &mut Frame,
        hdr: u32,
        l: &Layout,
        n: usize,
    ) -> (u32, u32, u32) {
        let (len, cap, base) =
            (b.local(ValType::I64), b.local(ValType::I64), b.local(ValType::I32));
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
            b.ins(&Instruction::I32Const((self.stride(inner, line)? * len as u32) as i32));
            b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
    /// the statement into `xs = push(xs, v)`, so the write-back is an assignment.
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
        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
        let hdr = b.local(ValType::I32);
        b.slot(off);
        b.ins(&Instruction::LocalSet(hdr));

        let (len, cap, base) = self.sa_parts(b, hdr, &l, n);
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
        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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

        let w = Walk { data: base, len, stride: stride as u32, elem: inner.clone(), byte: false };
        self.elem_addr(b, &w, len);
        let r = self.cx.repr(inner, line)?;
        self.expr_as(m, b, value, inner)?;
        match &r {
            Repr::Scalar(_) => {
                b.ins(&store_of(&self.cx.ll(inner)));
            }
            Repr::Agg(_) => {
                b.ins(&Instruction::I32Const(stride));
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
            }
            Repr::Unit => return unsupported("a SmallArray of Unit", line),
        }
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
        let w = Walk { data: base, len, stride, elem: inner.clone(), byte: false };

        match name {
            // A fresh growable `Array<T>` holding a copy of the live elements —
            // the one explicit conversion RFC-0056 has, and the interpreter's is
            // the identity because both are `Val::Array`.
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
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
                let oty = Type::Option(Box::new(inner.clone()));
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
                b.ins(&Instruction::I32Const(1));
                b.ins(&Instruction::I32Store8(byte()));
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
                        b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
                b.ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
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
    MemArg { offset: off as u64, align: 3, memory_index: 0 }
}

/// A 4-byte access at a static offset.
fn word_at(off: u32) -> MemArg {
    MemArg { offset: off as u64, align: 2, memory_index: 0 }
}

fn word8() -> MemArg {
    MemArg { offset: 0, align: 3, memory_index: 0 }
}

/// The comparison instruction for an `i32`-shaped operand pair.
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
    const PLAIN: Num = Num { bits: 64, signed: true };

    /// The integer type `ty` *is*, or `None` for anything that is not one. Takes
    /// a RESOLVED type, so a validated name has already become its base.
    fn of(ty: &Type) -> Option<Num> {
        match ty {
            Type::Int => Some(Num::PLAIN),
            Type::IntN { bits, signed } => Some(Num { bits: *bits, signed: *signed }),
            _ => None,
        }
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
        (8, false) => b.ins(&Instruction::I32Const(0xFF)).ins(&Instruction::I32And),
        (16, false) => b.ins(&Instruction::I32Const(0xFFFF)).ins(&Instruction::I32And),
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
    let m = |align| MemArg { offset: off as u64, align, memory_index: 0 };
    match ll {
        "i64" => Instruction::I64Load(m(3)),
        "double" => Instruction::F64Load(m(3)),
        "float" => Instruction::F32Load(m(2)),
        "i32" | "ptr" => Instruction::I32Load(m(2)),
        "i16" if signed => Instruction::I32Load16S(m(1)),
        "i16" => Instruction::I32Load16U(m(1)),
        "i8" if signed => Instruction::I32Load8S(m(0)),
        _ => Instruction::I32Load8U(m(0)),
    }
}

fn store_of(ll: &str) -> Instruction<'static> {
    let m = |align| MemArg { offset: 0, align, memory_index: 0 };
    match ll {
        "i64" => Instruction::I64Store(m(3)),
        "double" => Instruction::F64Store(m(3)),
        "float" => Instruction::F32Store(m(2)),
        "i32" | "ptr" => Instruction::I32Store(m(2)),
        "i16" => Instruction::I32Store16(m(1)),
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
/// The data each interned on its way past is NOT swept.
#[derive(Clone, Copy, Default)]
struct Rt {
    write_all: u32,
    malloc: u32,
    strlen: u32,
    strcmp: u32,
    trap: u32,
    print_str: u32,
    print_i64: u32,
    int_str: u32,
    bool_str: u32,
    concat: u32,
    /// Grow a `String` accumulator in place (RFC-0081). A function rather than an
    /// inline sequence at every `s = s + …`: the body is forty instructions with
    /// two `if`s in it, and `std/json` alone has six append sites.
    str_append: u32,
    trap_idx: u32,
    utf8valid: u32,
    str_from_bytes: u32,
    // RFC-0014 input I/O and RFC-0043's host boundary, served straight from WASI
    // (M2j) rather than through the shim — a standalone module has no shim, and
    // `clock_time_get`/`random_get`/`args_get`/`fd_read`/`path_open` are the same
    // syscalls wasi-libc would have reached for us.
    starts: u32,
    env_get: u32,
    str_i64: u32,
    now_millis: u32,
    mono_nanos: u32,
    random_seed: u32,
    args: u32,
    getbyte: u32,
    read_line: u32,
    open_at: u32,
    read_all: u32,
    err3: u32,
    read_file: u32,
    read_file_bytes: u32,
    write_file: u32,
    rename_file: u32,
    /// `listDir` (RFC-0021), on the generator path ONLY — the language gives it no
    /// runtime lowering at all, so the slot is handed out only when there is a
    /// `vyrn_gen.read` to serve it (RFC-0076 M7). An `Option` rather than an index
    /// that is sometimes a lie: the one call site has to be unreachable without it.
    ///
    /// It sits mid-table beside the other readers because the numbering is
    /// COMPUTED — `slot` appends — so an absent entry shifts the ones after it and
    /// nothing outside one compile depends on where they land.
    list_dir: Option<u32>,
    // RFC-0004 §4's generational slot table (M2l). Three entries, not the LLVM
    // prelude's five, because two of them only ever appear together: every
    // `get`/`set`/`release`/`drop` checks the generation and then wants the
    // payload address, so `cell_addr` IS the check.
    cell_new: u32,
    cell_addr: u32,
    cell_release: u32,
    /// RFC-0075 M2c's fourth array: the ADDRESS of `src[slot]`, the stream a
    /// `fromWrap` put behind a cursor. An address rather than a getter and a
    /// setter, because the slab's base is a lazily-allocated pointer this
    /// function is the only one outside `cell_runtime` that needs.
    cell_srcp: u32,
    /// RFC-0028's `Map<String, V>` key scan (M2l). The ONE piece of the map that
    /// is a function: `reserve`, `remove_at` and `keys_copy` are each reached from
    /// a single site and are a `malloc` plus a copy, so they are emitted there.
    map_find: u32,
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
    count: u32,
    msg_div0: u32,
    msg_rem0: u32,
    msg_divovf: u32,
    msg_shift: u32,
    msg_aoob: u32,
    msg_soob: u32,
    msg_oob_end: u32,
    msg_region: u32,
    /// RFC-0004 §4's region nesting counter: four reserved bytes, because the
    /// depth is dynamic (a `region` in a callee nests inside its caller's) and
    /// entering a 65th is a trap the interpreter also takes. Storage rather than a
    /// wasm global for M2f's reason — module state showed that one mechanism in
    /// memory beats two, and `reserve` is that mechanism.
    region_sp: u32,
}

impl Rt {
    /// Hand out the index of every runtime function, in the order the bodies are
    /// emitted below.
    ///
    /// The numbering has to precede the emission, because a body calls helpers
    /// that do not exist yet — `print_str` calls `strlen`, `concat` calls
    /// `malloc`. What it does NOT have to do is name numbers: `slot` appends and
    /// gives back what it appended, so a new helper is one line here beside the
    /// place its body is emitted, `count` is however many were handed out, and the
    /// two cannot disagree. The hand-numbered version could: an entry inserted
    /// mid-table renumbered every entry after it, and a `call` that came out
    /// pointing at the wrong function only failed loudly where the two signatures
    /// differed. Two helpers with the same wasm signature swapped silently, and
    /// there are several such sets here: `read_file` and `read_file_bytes` are both
    /// `(i32, i32) -> ()`, and `strlen` and `utf8valid` are both `(i32) -> i32`.
    ///
    /// The hazard was paid off rather than argued about: retiring `charcount`
    /// (RFC-0078's census) is the first REMOVAL this table has seen, and it was one
    /// deleted line here and one deleted body below, with nothing to renumber.
    ///
    /// The returned table is that record: name beside index, which is what the
    /// consistency test checks and what a reader wanting the emission order reads.
    fn slots(base: u32, gen_host: bool) -> (Rt, Vec<(&'static str, u32)>) {
        let mut table: Vec<(&'static str, u32)> = Vec::new();
        let mut slot = |name: &'static str| {
            let i = base + table.len() as u32;
            table.push((name, i));
            i
        };
        // Every field is named, so a field added to `Rt` and forgotten here is a
        // compile error rather than an index of zero pointing at `write_all`.
        let mut rt = Rt {
            write_all: slot("write_all"),
            malloc: slot("malloc"),
            strlen: slot("strlen"),
            strcmp: slot("strcmp"),
            trap: slot("trap"),
            print_str: slot("print_str"),
            print_i64: slot("print_i64"),
            int_str: slot("int_str"),
            bool_str: slot("bool_str"),
            concat: slot("concat"),
            str_append: slot("str_append"),
            trap_idx: slot("trap_idx"),
            utf8valid: slot("utf8valid"),
            str_from_bytes: slot("str_from_bytes"),
            starts: slot("starts"),
            env_get: slot("env_get"),
            str_i64: slot("str_i64"),
            now_millis: slot("now_millis"),
            mono_nanos: slot("mono_nanos"),
            random_seed: slot("random_seed"),
            args: slot("args"),
            getbyte: slot("getbyte"),
            read_line: slot("read_line"),
            open_at: slot("open_at"),
            read_all: slot("read_all"),
            err3: slot("err3"),
            read_file: slot("read_file"),
            read_file_bytes: slot("read_file_bytes"),
            write_file: slot("write_file"),
            rename_file: slot("rename_file"),
            list_dir: gen_host.then(|| slot("list_dir")),
            cell_new: slot("cell_new"),
            cell_addr: slot("cell_addr"),
            cell_release: slot("cell_release"),
            cell_srcp: slot("cell_srcp"),
            map_find: slot("map_find"),
            regex_run: slot("regex_run"),
            parse_i64: slot("parse_i64"),
            line_at: slot("line_at"),
            col_at: slot("col_at"),
            // Derived, not declared. The data segment addresses are filled in by
            // `runtime` as it interns them.
            count: 0,
            msg_div0: 0,
            msg_rem0: 0,
            msg_divovf: 0,
            msg_shift: 0,
            msg_aoob: 0,
            msg_soob: 0,
            msg_oob_end: 0,
            msg_region: 0,
            region_sp: 0,
        };
        rt.count = table.len() as u32;
        (rt, table)
    }

    /// Assert that the function about to be emitted is the one `want` reserved.
    ///
    /// The declared order and the emission order are two lists, and a `call`
    /// carries an index — so this is the seam where they have to agree, and it is
    /// checked at every helper rather than once at the end because a swap WITHIN
    /// the runtime leaves the count right. That is the silent case: `read_file` and
    /// `read_file_bytes` have the same wasm signature, so a module with the two
    /// exchanged still validates and then reads the wrong thing.
    fn next_is(&self, m: &Module, want: u32) {
        assert_eq!(m.next_func(), want, "a runtime helper was emitted out of declared order");
    }

    /// A string literal's address in the data segment, NUL-terminated because a
    /// Vyrn `String` is a `ptr` and everything downstream scans for the zero.
    fn intern(&self, m: &mut Module, s: &str) -> u32 {
        let mut bytes = s.as_bytes().to_vec();
        bytes.push(0);
        m.data(&bytes, 1)
    }
}

fn byte() -> MemArg {
    MemArg { offset: 0, align: 0, memory_index: 0 }
}

fn word() -> MemArg {
    MemArg { offset: 0, align: 2, memory_index: 0 }
}

/// The second word of a String accumulator's `(len, cap)` shadow. Named because
/// the two halves are addressed from the same base in four places and an offset
/// of 0 where 4 was meant is a silent wrong length.
fn cap_at() -> MemArg {
    MemArg { offset: 4, align: 2, memory_index: 0 }
}

fn runtime(m: &mut Module, wasi: &Wasi, gen: Option<&Gen>) -> Rt {
    let (fd_write, proc_exit) = (wasi.fd_write, wasi.proc_exit);
    let base = m.n_imports();
    let (mut rt, _table) = Rt::slots(base, gen.is_some());
    let nl = rt.intern(m, "\n");
    let t = rt.intern(m, "true");
    let f = rt.intern(m, "false");
    rt.msg_div0 = rt.intern(m, "error: division by zero\n");
    rt.msg_rem0 = rt.intern(m, "error: remainder by zero\n");
    rt.msg_divovf = rt.intern(m, "error: integer overflow in division\n");
    rt.msg_shift = rt.intern(m, "error: shift amount out of range\n");
    // (The three spellings `{:.6}` gives a non-finite double were interned here
    // for `float_str`. `std/num`'s `f64Str` builds them out of bytes, in Vyrn —
    // RFC-0081 M2.)
    // The bounds message has the offending index in the MIDDLE, so it is three
    // pieces rather than one interned string — see `trap_idx` below.
    rt.msg_aoob = rt.intern(m, "error: array index ");
    rt.msg_soob = rt.intern(m, "error: string index ");
    rt.msg_oob_end = rt.intern(m, " out of bounds\n");
    // RFC-0004 §4. The 64 is the LLVM prelude's fixed region stack, and the
    // interpreter traps at the same depth with the same words precisely so the
    // three engines agree about it.
    rt.msg_region = rt.intern(m, "error: region nesting exceeds 64\n");
    rt.region_sp = m.reserve(4, 4);

    // write_all(fd, ptr, len) — the ONE place bytes leave this module.
    //
    // A `fd_write` is allowed to write fewer bytes than it was given and say so
    // in `nwritten`; a caller that drops that number prints a prefix and calls it
    // a day. This backend found that out the direct way — two iovecs, only the
    // first of which arrived — so the retry is here rather than at three call
    // sites that would each have to remember it.
    let nw = 4;
    rt.next_is(m, rt.write_all);
    m.func(
        &[ValType::I32, ValType::I32, ValType::I32],
        &[],
        &[ValType::I32],
        12,
        |b| {
            b.ins(&Instruction::Block(BlockType::Empty)).ins(&Instruction::Loop(BlockType::Empty));
            b.ins(&Instruction::LocalGet(2)).ins(&Instruction::I32Eqz).ins(&Instruction::BrIf(1));
            b.slot(0).ins(&Instruction::LocalGet(1)).ins(&Instruction::I32Store(word()));
            b.slot(4).ins(&Instruction::LocalGet(2)).ins(&Instruction::I32Store(word()));
            b.ins(&Instruction::LocalGet(0));
            b.slot(0);
            b.ins(&Instruction::I32Const(1));
            b.slot(8);
            // A non-zero errno, or a zero-length write, would spin forever.
            b.ins(&Instruction::Call(fd_write)).ins(&Instruction::BrIf(1));
            b.slot(8)
                .ins(&Instruction::I32Load(word()))
                .ins(&Instruction::LocalTee(nw))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::BrIf(1));
            b.ins(&Instruction::LocalGet(1))
                .ins(&Instruction::LocalGet(nw))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(1));
            b.ins(&Instruction::LocalGet(2))
                .ins(&Instruction::LocalGet(nw))
                .ins(&Instruction::I32Sub)
                .ins(&Instruction::LocalSet(2));
            b.ins(&Instruction::Br(0)).ins(&Instruction::End).ins(&Instruction::End);
        },
    );

    // malloc(n) — a bump pointer over `HEAP`.
    //
    // `n` is an `i64`, not the `i32` a wasm32 pointer is, and that is the native
    // shim's signature (`__vyrn_malloc(unsigned long long)`, `toolchain.rs`) for
    // the native shim's stated reason. Every interesting caller computes
    // `count * stride` out of a Vyrn length, which IS an `i64`; taking an `i32`
    // put the truncation at the call site where nothing could see it. `push`
    // doubling a 2 GiB buffer wrapped `cap * stride` to a handful of bytes,
    // allocated those, and then copied 2 GiB into them — heap corruption out of
    // an allocation that reported success.
    //
    // ponytail: it never frees. Vyrn's ownership analysis knows exactly where every
    // value dies (`Stmt::Drop` is already in the AST), so a real allocator belongs
    // here eventually; nothing observable depends on it, because a free is not a
    // thing a program can print.
    let (p, end) = (2, 3);
    let trap = rt.trap;
    let oom = rt.intern(m, "error: out of memory\n");
    rt.next_is(m, rt.malloc);
    m.func(&[ValType::I64], &[ValType::I32], &[ValType::I32, ValType::I64], 0, |b| {
        // The width check, BEFORE the rounding — the native shim puts it before
        // the `(size_t)` cast for the same reason, and here `n + 7` is the cast:
        // a request of 2^64-1 rounds to 0 and would bump the heap by nothing,
        // handing back a pointer for sixteen exabytes.
        b.ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I64Const(0xFFFF_FFFF))
            .ins(&Instruction::I64GtU)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(oom as i32))
            .ins(&Instruction::Call(trap))
            .ins(&Instruction::End);
        // The bump itself, in 64 bits so the SUM cannot wrap either: a 3 GiB heap
        // plus a 2 GiB request is 5 GiB, which as an `i32` was a small pointer
        // that then passed the `memory.size` test below. A wasm32 memory stops at
        // 4 GiB, so a top past it is a request that can never be served —
        // reported with the words `memory.grow` failing reports, since it is the
        // same failure reached one step earlier.
        b.ins(&Instruction::GlobalGet(HEAP))
            .ins(&Instruction::LocalTee(p))
            .ins(&Instruction::I64ExtendI32U)
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I64Const(7))
            .ins(&Instruction::I64Add)
            .ins(&Instruction::I64Const(-8))
            .ins(&Instruction::I64And)
            .ins(&Instruction::I64Add)
            .ins(&Instruction::LocalTee(end))
            .ins(&Instruction::I64Const(0xFFFF_FFFF))
            .ins(&Instruction::I64GtU)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(oom as i32))
            .ins(&Instruction::Call(trap))
            .ins(&Instruction::End);
        b.ins(&Instruction::LocalGet(end))
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::GlobalSet(HEAP))
            .ins(&Instruction::Block(BlockType::Empty))
            .ins(&Instruction::Loop(BlockType::Empty))
            .ins(&Instruction::GlobalGet(HEAP))
            .ins(&Instruction::MemorySize(0))
            .ins(&Instruction::I32Const(16))
            .ins(&Instruction::I32Shl)
            .ins(&Instruction::I32LeU)
            .ins(&Instruction::BrIf(1))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::MemoryGrow(0))
            // A grow that fails returns -1 and leaves `memory.size` where it was,
            // so dropping the result re-tests the same condition and grows again
            // — forever, with no output. Not academic: a browser
            // `WebAssembly.Memory` is routinely constructed with a `maximum`, and
            // the browser is a first-class target, so the capped memory is the
            // normal case and the hang is what a user would see. Uncapped it was
            // masked, badly: growth ran to the 4 GiB ceiling and the wrapped bump
            // pointer trapped out of bounds instead.
            //
            // The wording is the native shim's `__vyrn_alloc_check`
            // (`toolchain.rs`), not new words, because parity compares stderr byte
            // for byte across the three engines.
            .ins(&Instruction::I32Const(-1))
            .ins(&Instruction::I32Eq)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(oom as i32))
            .ins(&Instruction::Call(trap))
            .ins(&Instruction::End)
            .ins(&Instruction::Br(0))
            .ins(&Instruction::End)
            .ins(&Instruction::End)
            .ins(&Instruction::LocalGet(p));
    });

    // strlen(s)
    rt.next_is(m, rt.strlen);
    m.func(&[ValType::I32], &[ValType::I32], &[ValType::I32], 0, |b| {
        b.ins(&Instruction::LocalGet(0))
            .ins(&Instruction::LocalSet(p))
            .ins(&Instruction::Block(BlockType::Empty))
            .ins(&Instruction::Loop(BlockType::Empty))
            .ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Load8U(byte()))
            .ins(&Instruction::I32Eqz)
            .ins(&Instruction::BrIf(1))
            .ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalSet(p))
            .ins(&Instruction::Br(0))
            .ins(&Instruction::End)
            .ins(&Instruction::End)
            .ins(&Instruction::LocalGet(p))
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32Sub);
    });

    // strcmp(a, b) — byte order, unsigned, which is what a Vyrn `String`
    // comparison is (RFC-0022) since a String is UTF-8 bytes.
    let (ca, cb) = (3, 4);
    rt.next_is(m, rt.strcmp);
    m.func(
        &[ValType::I32, ValType::I32],
        &[ValType::I32],
        &[ValType::I32, ValType::I32],
        0,
        |b| {
            b.ins(&Instruction::Block(BlockType::Result(ValType::I32)))
                .ins(&Instruction::Loop(BlockType::Empty))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::LocalTee(ca))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::LocalTee(cb))
                .ins(&Instruction::I32Ne)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(ca))
                .ins(&Instruction::LocalGet(cb))
                .ins(&Instruction::I32Sub)
                .ins(&Instruction::Br(2))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(ca))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::Br(2))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(0))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(1))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::Unreachable)
                .ins(&Instruction::End);
        },
    );

    // trap(msg) — the message on stderr and exit 1, which is what the
    // interpreter and the native build both do. Not a wasm `unreachable`: that
    // would print wasmtime's wording, and parity compares stderr.
    let strlen = rt.strlen;
    let write_all = rt.write_all;
    rt.next_is(m, rt.trap);
    m.func(&[ValType::I32], &[], &[], 0, |b| {
        b.ins(&Instruction::I32Const(2))
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::Call(strlen))
            .ins(&Instruction::Call(write_all))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::Call(proc_exit));
    });

    // print_str(s) — the bytes, then the newline.
    rt.next_is(m, rt.print_str);
    m.func(&[ValType::I32], &[], &[], 0, |b| {
        b.ins(&Instruction::I32Const(1))
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::Call(strlen))
            .ins(&Instruction::Call(write_all))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Const(nl as i32))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::Call(write_all));
    });

    rt.next_is(m, rt.print_i64);
    print_i64(m, write_all);

    // int_str(v, signed) — the same digit loop as `print_i64`, into a fresh
    // 24-byte block. The digits are written backwards from the end, so the result
    // pointer is wherever they stopped.
    let (pp, neg) = (3, 4);
    let malloc = rt.malloc;
    rt.next_is(m, rt.int_str);
    m.func(
        &[ValType::I64, ValType::I32],
        &[ValType::I32],
        &[ValType::I32, ValType::I32],
        0,
        |b| {
            b.ins(&Instruction::I64Const(24))
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::I32Const(23))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalTee(pp))
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::I32Store8(byte()));
            b.ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I64Const(0))
                .ins(&Instruction::I64LtS)
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I32And)
                .ins(&Instruction::LocalTee(neg))
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I64Const(0))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I64Sub)
                .ins(&Instruction::LocalSet(0))
                .ins(&Instruction::End);
            b.ins(&Instruction::Loop(BlockType::Empty))
                .ins(&Instruction::LocalGet(pp))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Sub)
                .ins(&Instruction::LocalTee(pp))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I64Const(10))
                .ins(&Instruction::I64RemU)
                .ins(&Instruction::I32WrapI64)
                .ins(&Instruction::I32Const(b'0' as i32))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Store8(byte()))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I64Const(10))
                .ins(&Instruction::I64DivU)
                .ins(&Instruction::LocalTee(0))
                .ins(&Instruction::I64Eqz)
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::BrIf(0))
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(neg))
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(pp))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Sub)
                .ins(&Instruction::LocalTee(pp))
                .ins(&Instruction::I32Const(b'-' as i32))
                .ins(&Instruction::I32Store8(byte()))
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(pp));
        },
    );

    // bool_str(v) — the literal, not a copy of it. Nothing frees a String here.
    rt.next_is(m, rt.bool_str);
    m.func(&[ValType::I32], &[ValType::I32], &[], 0, |b| {
        b.ins(&Instruction::LocalGet(0))
            .ins(&Instruction::If(BlockType::Result(ValType::I32)))
            .ins(&Instruction::I32Const(t as i32))
            .ins(&Instruction::Else)
            .ins(&Instruction::I32Const(f as i32))
            .ins(&Instruction::End);
    });

    // concat(a, b)
    let (la, lb, r) = (3, 4, 5);
    rt.next_is(m, rt.concat);
    m.func(
        &[ValType::I32, ValType::I32],
        &[ValType::I32],
        &[ValType::I32, ValType::I32, ValType::I32],
        0,
        |bb| {
            bb.ins(&Instruction::LocalGet(0))
                .ins(&Instruction::Call(strlen))
                .ins(&Instruction::LocalSet(la))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::Call(strlen))
                .ins(&Instruction::LocalSet(lb))
                .ins(&Instruction::LocalGet(la))
                .ins(&Instruction::LocalGet(lb))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::LocalSet(r))
                .ins(&Instruction::LocalGet(r))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::LocalGet(la))
                .ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 })
                .ins(&Instruction::LocalGet(r))
                .ins(&Instruction::LocalGet(la))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::LocalGet(lb))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 })
                .ins(&Instruction::LocalGet(r));
        },
    );

    // str_append(st, p, v) -> p' — append `v` to the accumulator `p`, in place,
    // growing geometrically. `st` addresses its `(len, cap)` shadow in the
    // caller's frame; the new pointer comes back as the result because a wasm
    // local has no address to write through (RFC-0081).
    //
    // The grow is `malloc` and copy, not a `realloc`, because this backend's
    // allocator IS a bump pointer with no free (see `malloc` above) — so there is
    // nothing to hand a block back to, and nothing to extend it into once the
    // next element's string has been bumped past it. That is not the quadratic
    // part: doubling makes N appends copy O(N) bytes in total and bump O(N) of
    // heap, where `concat` per element copied and bumped O(N²) — which is why 40k
    // `Int64` did not merely take 1.4 s, it walked the bump pointer past 4 GiB
    // and trapped out of bounds on 229 KB of JSON.
    //
    // ponytail: a bump allocator can extend its own top allocation in place
    // (`HEAP == p + cap`), which would make an accumulator with nothing allocated
    // after it grow for free. Not taken — the writers that matter allocate each
    // element's string BETWEEN appends, so the accumulator is never on top and
    // the fast path would never fire in the case this exists for.
    let (st, p, v) = (0, 1, 2);
    let (vlen, cap, len, need, nc, nb) = (4, 5, 6, 7, 8, 9);
    rt.next_is(m, rt.str_append);
    m.func(
        &[ValType::I32, ValType::I32, ValType::I32],
        &[ValType::I32],
        &[ValType::I32; 6],
        0,
        |b| {
            b.ins(&Instruction::LocalGet(v))
                .ins(&Instruction::Call(strlen))
                .ins(&Instruction::LocalSet(vlen));
            // `cap == 0`: the pointer is not ours (a literal, a `concat` result,
            // a call result), so copy it into a buffer that is. 32 bytes minimum,
            // matching the textual backend's floor so the two grow in step.
            b.ins(&Instruction::LocalGet(st))
                .ins(&Instruction::I32Load(cap_at()))
                .ins(&Instruction::LocalTee(cap))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(p))
                .ins(&Instruction::Call(strlen))
                .ins(&Instruction::LocalSet(len))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::LocalGet(vlen))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalTee(cap))
                .ins(&Instruction::I32Const(32))
                .ins(&Instruction::I32LtU)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(32))
                .ins(&Instruction::LocalSet(cap))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(cap))
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::LocalTee(nb))
                .ins(&Instruction::LocalGet(p))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 })
                .ins(&Instruction::LocalGet(nb))
                .ins(&Instruction::LocalSet(p))
                .ins(&Instruction::LocalGet(st))
                .ins(&Instruction::LocalGet(cap))
                .ins(&Instruction::I32Store(cap_at()))
                .ins(&Instruction::Else)
                .ins(&Instruction::LocalGet(st))
                .ins(&Instruction::I32Load(word()))
                .ins(&Instruction::LocalSet(len))
                .ins(&Instruction::End);
            // Reserve `len + vlen + 1`, doubling so N appends are O(N).
            b.ins(&Instruction::LocalGet(len))
                .ins(&Instruction::LocalGet(vlen))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalTee(need))
                .ins(&Instruction::LocalGet(cap))
                .ins(&Instruction::I32GtU)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(cap))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Shl)
                .ins(&Instruction::LocalTee(nc))
                .ins(&Instruction::LocalGet(need))
                .ins(&Instruction::I32LtU)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(need))
                .ins(&Instruction::LocalSet(nc))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(nc))
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::LocalTee(nb))
                .ins(&Instruction::LocalGet(p))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 })
                .ins(&Instruction::LocalGet(nb))
                .ins(&Instruction::LocalSet(p))
                .ins(&Instruction::LocalGet(st))
                .ins(&Instruction::LocalGet(nc))
                .ins(&Instruction::I32Store(cap_at()))
                .ins(&Instruction::End);
            // Copy the operand's bytes AND its NUL over the old terminator.
            b.ins(&Instruction::LocalGet(p))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalGet(v))
                .ins(&Instruction::LocalGet(vlen))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 })
                .ins(&Instruction::LocalGet(st))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::LocalGet(vlen))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Store(word()))
                .ins(&Instruction::LocalGet(p));
        },
    );

    // trap_idx(pre, i, post) — `error: array index 7 out of bounds`, which the
    // interpreter and the native build both print with the index interpolated.
    // Three writes rather than a `printf`: varargs are M3, and this is the only
    // runtime message with a number in it.
    let int_str = rt.int_str;
    rt.next_is(m, rt.trap_idx);
    m.func(&[ValType::I32, ValType::I64, ValType::I32], &[], &[ValType::I32], 0, |b| {
        let s = 4; // params 0..2, the frame base 3, then ours
        let put = |b: &mut Frame, p: u32| {
            b.ins(&Instruction::I32Const(2))
                .ins(&Instruction::LocalGet(p))
                .ins(&Instruction::LocalGet(p))
                .ins(&Instruction::Call(strlen))
                .ins(&Instruction::Call(write_all));
        };
        put(b, 0);
        b.ins(&Instruction::LocalGet(1))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::Call(int_str))
            .ins(&Instruction::LocalSet(s));
        put(b, s);
        put(b, 2);
        b.ins(&Instruction::I32Const(1)).ins(&Instruction::Call(proc_exit));
    });

    // (`charcount(s)` was here — ~30 lines of scan for the bytes that are not UTF-8
    // continuation bytes. RFC-0078's census found `charCount` the one builtin with
    // no justification for being one, and `std/text`'s `charCountV` is the same scan
    // written in Vyrn, so this backend has a row it no longer has to lower. It is
    // the first runtime function this table has LOST, which is what made the
    // self-registering `next_is` worth doing in 5d6a857.)

    // utf8valid(s, len) — Björn Höhrmann's DFA, over the SAME table the textual
    // backend emits (`crate::utf8d_table`). Sharing the bytes is the point: two
    // tables would be two answers to "is this valid UTF-8", free to drift by one
    // entry, and the thing they decide is whether a program traps.
    //
    // 256 byte-class entries, then 9 states × 12 classes of transitions. State 0
    // accepts, 12 rejects, and every rejecting transition stays at 12 — so the
    // loop never needs an early exit.
    let utf8d = m.data(&crate::utf8d_table(), 1);
    let (i, st) = (3, 4);
    rt.next_is(m, rt.utf8valid);
    m.func(
        &[ValType::I32, ValType::I32],
        &[ValType::I32],
        &[ValType::I32, ValType::I32],
        0,
        |b| {
            b.ins(&Instruction::Block(BlockType::Empty))
                .ins(&Instruction::Loop(BlockType::Empty))
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I32GeU)
                .ins(&Instruction::BrIf(1))
                // st = utf8d[256 + st + utf8d[s[i]]]
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::I32Const(utf8d as i32))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::LocalGet(st))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Const((utf8d + 256) as i32))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::LocalSet(st))
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(i))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(st))
                .ins(&Instruction::I32Eqz);
        },
    );

    // str_from_bytes(data, len, dest) — RFC-0014's `stringFromBytes`, writing a
    // whole `Result<String, String>` through `dest`.
    //
    // A runtime function rather than inline lowering because the two failures are
    // where the semantics live, and both are canonical wording parity compares:
    // an embedded NUL is rejected BEFORE the UTF-8 check (a Vyrn `String` is
    // NUL-terminated, so it could not carry one), and the DFA decides the rest.
    // Nothing frees, so an `Err` payload is the interned message itself rather
    // than a heap copy of it — the textual backend copies only so that every I/O
    // error payload is owned storage, and this backend's allocator has no free to
    // make that distinction observable.
    let bnul = rt.intern(m, crate::io_message("bnul"));
    let butf8 = rt.intern(m, crate::io_message("butf8"));
    let res = layout::of_ll("{ i1, i64, i64 }").expect("the Result<String, String> shape");
    // params 0..2, the frame base 3, then ours — `i` is NOT `utf8valid`'s `i`
    // above, whose 3 is this function's base.
    let (buf, err, c, at_i) = (4, 5, 6, 7);
    let (utf8valid, malloc) = (rt.utf8valid, rt.malloc);
    rt.next_is(m, rt.str_from_bytes);
    m.func(
        &[ValType::I32, ValType::I32, ValType::I32],
        &[],
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        0,
        |b| {
            b.ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::LocalSet(buf));
            b.ins(&Instruction::Block(BlockType::Empty)) // fin
                .ins(&Instruction::Block(BlockType::Empty)) // copied
                .ins(&Instruction::Loop(BlockType::Empty))
                .ins(&Instruction::LocalGet(at_i))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I32GeU)
                .ins(&Instruction::BrIf(1))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::LocalGet(at_i))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::LocalTee(c))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(bnul as i32))
                .ins(&Instruction::LocalSet(err))
                .ins(&Instruction::Br(3))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(at_i))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalGet(c))
                .ins(&Instruction::I32Store8(byte()))
                .ins(&Instruction::LocalGet(at_i))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(at_i))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::I32Store8(byte()))
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::Call(utf8valid))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(butf8 as i32))
                .ins(&Instruction::LocalSet(err))
                .ins(&Instruction::End)
                .ins(&Instruction::End); // fin
            // The tag is `no error`, and the word is whichever pointer that named.
            b.ins(&Instruction::LocalGet(2))
                .ins(&Instruction::LocalGet(err))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::I32Store8(MemArg {
                    offset: res.fields[0] as u64,
                    align: 0,
                    memory_index: 0,
                }));
            b.ins(&Instruction::LocalGet(2))
                .ins(&Instruction::LocalGet(err))
                .ins(&Instruction::If(BlockType::Result(ValType::I32)))
                .ins(&Instruction::LocalGet(err))
                .ins(&Instruction::Else)
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::End)
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::I64Store(at(res.fields[1])));
            b.ins(&Instruction::LocalGet(2))
                .ins(&Instruction::I64Const(0))
                .ins(&Instruction::I64Store(at(res.fields[2])));
        },
    );

    // (`slice` was emitted here — 60 instructions: a signed three-clause bounds
    // test, a continuation-byte probe at each cut point, two interned trap strings
    // and a `memory.copy`. RFC-0079 M3 deleted it along with the interpreter's arm
    // and the textual emitter's branch; `std/strpred`'s `sliceV` is the one range
    // check now. Removing a slot is a one-line deletion in `slots` because the
    // table hands indices out in field order — the second removal it has seen,
    // after `charcount`.)

    // (`float_str` was emitted here — see the note where its 511 lines stood.
    // RFC-0081 M2 routed `%f` to `std/num`'s `f64Str`; removing its slot is a
    // one-line deletion in `slots` because the table hands indices out in field
    // order — the third removal it has seen, after `charcount` and `slice`.)

    io_runtime(m, &rt, wasi, gen);
    cell_runtime(m, &rt);

    // map_find(keys, len, key) -> the entry's index, or -1.
    //
    // A linear `strcmp` scan over the key-pointer buffer, which is what the C
    // shim's `__vyrn_map_find` is — RFC-0028 chose insertion order over hashing,
    // so the scan IS the lookup and matching it is not a simplification. Written
    // without a `return`: M1's rule is that a body reaches its epilogue, so the
    // hit branches out of a block carrying the index in a local.
    let (i, found) = (3, 4);
    let strcmp = rt.strcmp;
    rt.next_is(m, rt.map_find);
    m.func(
        &[ValType::I32, ValType::I32, ValType::I32],
        &[ValType::I32],
        &[ValType::I32, ValType::I32],
        0,
        |b| {
            b.ins(&Instruction::I32Const(-1)).ins(&Instruction::LocalSet(found));
            b.ins(&Instruction::Block(BlockType::Empty)).ins(&Instruction::Loop(BlockType::Empty));
            b.ins(&Instruction::LocalGet(i))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I32GeS)
                .ins(&Instruction::BrIf(1));
            b.ins(&Instruction::LocalGet(0))
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32Const(4))
                .ins(&Instruction::I32Mul)
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Load(word()))
                .ins(&Instruction::LocalGet(2))
                .ins(&Instruction::Call(strcmp))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::LocalSet(found))
                .ins(&Instruction::Br(2))
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(i))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(found));
        },
    );

    // regex_run(s, table, start, accept) -> whether `s` matches in full.
    //
    // RFC-0046 compiles a `=~` pattern to a COMPLETE DFA — every state has all 256
    // transitions and a dead state absorbs a non-match — so the walk has no
    // conditional but the end of the string, and no anchoring to check: a full
    // match is "the state the last byte left us in accepts". The pattern is
    // entirely in the table, which is why one helper serves every pattern in the
    // module, and why this reads the same as `@__vyrn_regex_run` in the textual
    // backend and `Dfa::matches` in the interpreter. Three spellings of one walk is
    // two too many, but the other two already existed; what matters is that the
    // TABLE has one source (`vyrn_frontend::regex::compile`), so a disagreement
    // would have to be in the walk rather than in the language.
    let (st, ch) = (5, 6);
    rt.next_is(m, rt.regex_run);
    m.func(
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        &[ValType::I32],
        &[ValType::I32, ValType::I32],
        0,
        |b| {
            b.ins(&Instruction::LocalGet(2)).ins(&Instruction::LocalSet(st));
            b.ins(&Instruction::Block(BlockType::Empty)).ins(&Instruction::Loop(BlockType::Empty));
            b.ins(&Instruction::LocalGet(0))
                // UNSIGNED, and the whole corpus is ASCII so nothing here says so:
                // a signed load turns a UTF-8 continuation byte into a negative
                // table index, which reads memory below the table and answers
                // wrongly rather than trapping. `the_dfa_walk_...` is the test.
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::LocalTee(ch))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::BrIf(1));
            // table[st * 256 + ch], four bytes per entry: `st << 10` is the row.
            b.ins(&Instruction::LocalGet(1))
                .ins(&Instruction::LocalGet(st))
                .ins(&Instruction::I32Const(10))
                .ins(&Instruction::I32Shl)
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalGet(ch))
                .ins(&Instruction::I32Const(2))
                .ins(&Instruction::I32Shl)
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Load(word()))
                .ins(&Instruction::LocalSet(st));
            b.ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(0))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(3))
                .ins(&Instruction::LocalGet(st))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::I32Ne);
        },
    );

    text_runtime(m, &rt);

    // And the total: `count` is derived from the declarations, so this is the one
    // place it meets the emission.
    assert_eq!(m.next_func(), base + rt.count, "runtime function count");
    rt
}

/// `parse`, `lineAt` and `colAt` — the three loops RFC-0078 deliberately left as
/// builtins, and therefore the three this backend owes a lowering.
///
/// They are together because they are refused for the same *kind* of reason and
/// not the same reason. `parse` is a semantics refusal: `std/num`'s `parseInt64`
/// is the same digit loop and DECLINES where this one wraps, so routing `parse`
/// through it would change what every existing caller does with
/// `parse("18446744073709551615")` — which is `Some(-1)` here and on the other two
/// engines. `lineAt`/`colAt` are a cache refusal: the loop is four lines of Vyrn,
/// but it is O(offset) and a scanner asks once per node, so the interpreter
/// memoizes a line-start table per buffer that a generator — barred from module
/// state by comptime purity — could not hold.
///
/// **No cache here, and that is a decision.** The native shim does not memoize
/// either (`__vyrn_line_at` counts from byte 0 on every call), so counting
/// directly makes this backend agree with the engine it is closest to rather than
/// inventing a third behaviour; and the 122 ms RFC-0078 M4b(2) measured is a
/// GENERATION-time cost, paid by whichever engine runs the generator. A module
/// emitted by this backend reaches these only where compiled code calls them at
/// run time, which in the corpus is `textbytes.vyrn` comparing the builtin against
/// `std/text` over twelve short buffers. A cache would be per-buffer state in
/// linear memory keyed on an address the bump allocator can recycle — the
/// interpreter avoids that by holding the `Rc` — so it is not a small change, and
/// nothing measured wants it yet.
fn text_runtime(m: &mut Module, rt: &Rt) {
    let sum2 = layout::of_ll("{ i1, i64, i64 }").expect("the Option/Result shape");

    // parse_i64(s, dest) — RFC-0014's `parse` as an `Option<Int64>` written
    // through `dest`: an optional `-`, then digits, ALL of them consumed.
    //
    // Every decline is the same decline (`None`), and there are three ways to
    // reach it: nothing after the sign (`""` and `"-"`), a byte that is not a
    // digit anywhere in the rest (`"+1"`, `" 1"`, `"1 "`, `"1.5"`, `"abc"`,
    // `"12a"`, `"--1"`), and that is all — there is no third category, in
    // particular NOT overflow. `acc * 10 + d` in wrapping `i64` is the
    // interpreter's `wrapping_mul`/`wrapping_add` instruction for instruction, so
    // `"9223372036854775808"` is `Int64.min` and `"18446744073709551615"` is `-1`
    // on all three engines. `examples/numbytes.vyrn` pins every row of that table.
    //
    // Deliberately NOT `str_i64` next door, which reads `+` and stops at the first
    // byte that is not a digit — that is `strtoll`'s contract for an injected
    // `VYRN_FIXED_TIME`, and it is the opposite of this one's on exactly the inputs
    // `numbytes` prints.
    let (acc, neg, c) = (3, 4, 5); // params 0..1, the frame base 2, then ours
    rt.next_is(m, rt.parse_i64);
    m.func(
        &[ValType::I32, ValType::I32],
        &[],
        &[ValType::I64, ValType::I32, ValType::I32],
        0,
        |b| {
            b.ins(&Instruction::Block(BlockType::Empty)) // 1: fin
                .ins(&Instruction::Block(BlockType::Empty)); // 0: none
            // The sign, and then the first byte AFTER it — which is the byte the
            // emptiness test is about.
            b.ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::LocalTee(c))
                .ins(&Instruction::I32Const(b'-' as i32))
                .ins(&Instruction::I32Eq)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::LocalSet(neg))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalTee(0))
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::LocalSet(c))
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(c)).ins(&Instruction::I32Eqz).ins(&Instruction::BrIf(0));
            b.ins(&Instruction::Block(BlockType::Empty)) // 1: the digits ran out
                .ins(&Instruction::Loop(BlockType::Empty)); // 0
            b.ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::LocalTee(c))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::BrIf(1));
            // Not a digit: out of the `none` block, which is two levels further
            // out than the loop's own exit.
            b.ins(&Instruction::LocalGet(c))
                .ins(&Instruction::I32Const(b'0' as i32))
                .ins(&Instruction::I32LtU)
                .ins(&Instruction::BrIf(2))
                .ins(&Instruction::LocalGet(c))
                .ins(&Instruction::I32Const(b'9' as i32))
                .ins(&Instruction::I32GtU)
                .ins(&Instruction::BrIf(2));
            b.ins(&Instruction::LocalGet(acc))
                .ins(&Instruction::I64Const(10))
                .ins(&Instruction::I64Mul)
                .ins(&Instruction::LocalGet(c))
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::I64Const(b'0' as i64))
                .ins(&Instruction::I64Sub)
                .ins(&Instruction::I64Add)
                .ins(&Instruction::LocalSet(acc))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(0))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End);
            // `Some(±acc)`. Negated in place so the three stores below have no
            // branch in the middle of them; `wrapping_neg` of `Int64.min` is
            // `Int64.min`, which is `i64.sub` from zero and needs no note.
            b.ins(&Instruction::LocalGet(neg))
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I64Const(0))
                .ins(&Instruction::LocalGet(acc))
                .ins(&Instruction::I64Sub)
                .ins(&Instruction::LocalSet(acc))
                .ins(&Instruction::End);
            // The same three stores `sum2_write_to` makes, at the same
            // `layout::of_ll ∘ llt` offsets — spelled out only because the payload
            // is already an `i64` rather than a word to zero-extend.
            b.ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Store8(MemArg {
                    offset: sum2.fields[0] as u64,
                    align: 0,
                    memory_index: 0,
                }))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::LocalGet(acc))
                .ins(&Instruction::I64Store(at(sum2.fields[1])))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I64Const(0))
                .ins(&Instruction::I64Store(at(sum2.fields[2])));
            b.ins(&Instruction::Br(1)).ins(&Instruction::End); // none
            sum2_write_to(b, 1, &sum2, 0, None);
            b.ins(&Instruction::End); // fin
        },
    );

    // line_at(d, len, off) / col_at(d, len, off) — the 1-based line and column of
    // a byte offset, and a column counts BYTES.
    //
    // That last part is measured rather than assumed (RFC-0078 M4b(2)): the
    // interpreter computes `off - lineStart + 1` over a byte table and the shim
    // walks bytes backwards, so the `x` in `éx` is column 3. `std/vyx.vyrn`'s
    // wrapper documents it as "chars since the last LF", which is wrong for any
    // line with non-ASCII in it, and RFC-0033's `#line` directives want the byte
    // column anyway.
    //
    // Both clamp `off` to `len` and neither clamps it below zero, because a
    // negative `off` falls out of the loop conditions being SIGNED: `0 >= off`
    // ends the forward count before it starts and `off <= 0` ends the backward
    // walk, which is 1 either way and is what the interpreter's `.max(0)` and the
    // shim's `i < off` / `i > 0` both give.
    let (i, out) = (4, 5); // params 0..2, the frame base 3, then ours
    rt.next_is(m, rt.line_at);
    m.func(
        &[ValType::I32, ValType::I64, ValType::I64],
        &[ValType::I64],
        &[ValType::I64, ValType::I64],
        0,
        |b| {
            b.ins(&Instruction::I64Const(1)).ins(&Instruction::LocalSet(out));
            clamp_off(b);
            b.ins(&Instruction::Block(BlockType::Empty)).ins(&Instruction::Loop(BlockType::Empty));
            b.ins(&Instruction::LocalGet(i))
                .ins(&Instruction::LocalGet(2))
                .ins(&Instruction::I64GeS)
                .ins(&Instruction::BrIf(1));
            b.ins(&Instruction::LocalGet(0))
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32WrapI64)
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::I32Const(b'\n' as i32))
                .ins(&Instruction::I32Eq)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(out))
                .ins(&Instruction::I64Const(1))
                .ins(&Instruction::I64Add)
                .ins(&Instruction::LocalSet(out))
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I64Const(1))
                .ins(&Instruction::I64Add)
                .ins(&Instruction::LocalSet(i))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(out));
        },
    );

    rt.next_is(m, rt.col_at);
    m.func(
        &[ValType::I32, ValType::I64, ValType::I64],
        &[ValType::I64],
        &[ValType::I64],
        0,
        |b| {
            let out = 4;
            b.ins(&Instruction::I64Const(1)).ins(&Instruction::LocalSet(out));
            clamp_off(b);
            // `off` IS the cursor, walked down to the byte after the previous LF.
            b.ins(&Instruction::Block(BlockType::Empty)).ins(&Instruction::Loop(BlockType::Empty));
            b.ins(&Instruction::LocalGet(2))
                .ins(&Instruction::I64Const(0))
                .ins(&Instruction::I64LeS)
                .ins(&Instruction::BrIf(1));
            b.ins(&Instruction::LocalGet(0))
                .ins(&Instruction::LocalGet(2))
                .ins(&Instruction::I32WrapI64)
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Const(-1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::I32Const(b'\n' as i32))
                .ins(&Instruction::I32Eq)
                .ins(&Instruction::BrIf(1));
            b.ins(&Instruction::LocalGet(out))
                .ins(&Instruction::I64Const(1))
                .ins(&Instruction::I64Add)
                .ins(&Instruction::LocalSet(out))
                .ins(&Instruction::LocalGet(2))
                .ins(&Instruction::I64Const(-1))
                .ins(&Instruction::I64Add)
                .ins(&Instruction::LocalSet(2))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(out));
        },
    );
}

/// `if (off > len) off = len`, over parameters 1 and 2 of a `line_at`-shaped
/// signature. One place, because the two helpers must clamp identically or they
/// disagree past the end of the buffer — the offset RFC-0078's oracle sweeps to
/// `len + 3` precisely to catch.
fn clamp_off(b: &mut Frame) {
    b.ins(&Instruction::LocalGet(2))
        .ins(&Instruction::LocalGet(1))
        .ins(&Instruction::I64GtS)
        .ins(&Instruction::If(BlockType::Empty))
        .ins(&Instruction::LocalGet(1))
        .ins(&Instruction::LocalSet(2))
        .ins(&Instruction::End);
}

/// How many generational reference cells the slab holds, and where each of its
/// four parallel arrays starts inside one allocation.
///
/// 65536 is the LLVM prelude's number and it is not decoration: `autorelease` and
/// `freelist` both run past it on purpose, so a slab of a different size would
/// either exhaust where the other engines do not or hide a release that never
/// fired. The four arrays are one `malloc` because the slab is allocated LAZILY
/// — statically reserving 1 MiB would put a megabyte of zeroes in every module
/// this backend emits, including `fib`.
const CELLS: u32 = 65_536;
const CELL_PTRS: u32 = CELLS * 8;
const CELL_FREE: u32 = CELL_PTRS + CELLS * 4;
/// RFC-0075 M2c: the stream behind each cursor, null for every ordinary cell.
const CELL_SRC: u32 = CELL_FREE + CELLS * 4;
const CELL_SLAB: u32 = CELL_SRC + CELLS * 4;

/// The generational slot table (RFC-0004 §4, Path B), as three functions.
///
/// The LLVM build gets this from a hand-written IR prelude — it is not in the C
/// shim at all, so there was never anything to import and it is the one runtime
/// piece M2i's split could not have supplied. What it has to reproduce is the
/// *behaviour*, not the shape: allocation hands out `{ slot, generation }`, a
/// release bumps the slot's generation and pushes the slot on a LIFO free list,
/// and every access compares the reference's captured generation against the
/// slot's. A stale reference therefore fails a check instead of dangling, and
/// reuse order matches the prelude's because both free lists are stacks.
///
/// The payload is NOT freed on release. This backend's allocator is a bump
/// pointer (see `runtime`), so a free is unobservable — but the *slot* very much
/// is: `autorelease.vyrn` puts a million allocations through 65536 slots.
fn cell_runtime(m: &mut Module, rt: &Rt) {
    let (malloc, trap) = (rt.malloc, rt.trap);
    let uaf = rt.intern(m, "error: reference used after release\n");
    let oom = rt.intern(m, "error: out of reference cells\n");
    // slab base, next fresh slot, free-list height. Twelve bytes, versus the
    // megabyte the arrays themselves would have cost as statics.
    let st = m.reserve(12, 4);
    let (slab, top) = (st, st + 4);
    let freetop = st + 8;

    // cell_new(dest, payload) — writes `{ i64 slot, i64 generation }` through
    // `dest`, which is the aggregate ABI (M2b rule 3) rather than a special case:
    // a Ref is an aggregate, so it travels as the address of a caller's slot.
    let (s, p) = (2, 3);
    rt.next_is(m, rt.cell_new);
    m.func(&[ValType::I32, ValType::I32], &[], &[ValType::I32, ValType::I32], 0, |b| {
        // Lazily allocate the slab on the first `cell`. Bump-allocated memory is
        // fresh wasm pages, which are zero — so the generations start at 0 and
        // the pointer array starts null, exactly as the prelude's
        // `zeroinitializer` globals do.
        b.ins(&Instruction::I32Const(slab as i32))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::I32Eqz)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(slab as i32))
            .ins(&Instruction::I64Const(CELL_SLAB as i64))
            .ins(&Instruction::Call(malloc))
            .ins(&Instruction::I32Store(word()))
            .ins(&Instruction::End);
        b.ins(&Instruction::I32Const(slab as i32)).ins(&Instruction::I32Load(word())).ins(&Instruction::LocalSet(p));
        // A freed slot if there is one, else the next fresh one.
        b.ins(&Instruction::I32Const(freetop as i32))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(freetop as i32))
            .ins(&Instruction::I32Const(freetop as i32))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::I32Store(word()))
            .ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(CELL_FREE as i32))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Const(freetop as i32))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::I32Const(4))
            .ins(&Instruction::I32Mul)
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::LocalSet(s))
            .ins(&Instruction::Else)
            .ins(&Instruction::I32Const(top as i32))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::LocalTee(s))
            .ins(&Instruction::I32Const(CELLS as i32))
            .ins(&Instruction::I32GeU)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(oom as i32))
            .ins(&Instruction::Call(trap))
            .ins(&Instruction::End)
            .ins(&Instruction::I32Const(top as i32))
            .ins(&Instruction::LocalGet(s))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Store(word()))
            .ins(&Instruction::End);
        // ptr[slot] = payload
        b.ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(CELL_PTRS as i32))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalGet(s))
            .ins(&Instruction::I32Const(4))
            .ins(&Instruction::I32Mul)
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalGet(1))
            .ins(&Instruction::I32Store(word()));
        // src[slot] = 0 — a recycled slot starts with nothing behind it
        // (RFC-0075 M2c), which is what keeps `pull` on an ordinary cell a trap.
        b.ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(CELL_SRC as i32))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalGet(s))
            .ins(&Instruction::I32Const(4))
            .ins(&Instruction::I32Mul)
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Const(0))
            .ins(&Instruction::I32Store(word()));
        // dest = { slot, gen[slot] }
        b.ins(&Instruction::LocalGet(0))
            .ins(&Instruction::LocalGet(s))
            .ins(&Instruction::I64ExtendI32U)
            .ins(&Instruction::I64Store(word8()));
        b.ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32Const(8))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalGet(p))
            .ins(&Instruction::LocalGet(s))
            .ins(&Instruction::I32Const(8))
            .ins(&Instruction::I32Mul)
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I64Load(word8()))
            .ins(&Instruction::I64Store(word8()));
    });

    // cell_addr(slot, generation) -> the payload's address, having checked the
    // generation. The check and the load are one function because no caller wants
    // one without the other — `release` included, which calls this for the check
    // and drops the address.
    let base = 2;
    rt.next_is(m, rt.cell_addr);
    m.func(&[ValType::I64, ValType::I64], &[ValType::I32], &[ValType::I32], 0, |b| {
        b.ins(&Instruction::I32Const(slab as i32))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::I32Const(8))
            .ins(&Instruction::I32Mul)
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalTee(base))
            .ins(&Instruction::I64Load(word8()))
            .ins(&Instruction::LocalGet(1))
            .ins(&Instruction::I64Ne)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(uaf as i32))
            .ins(&Instruction::Call(trap))
            .ins(&Instruction::End);
        b.ins(&Instruction::I32Const(slab as i32))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::I32Const(CELL_PTRS as i32))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::I32Const(4))
            .ins(&Instruction::I32Mul)
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Load(word()));
    });

    // cell_release(slot) — bump the generation (invalidating every copy of the
    // reference) and push the slot for reuse.
    rt.next_is(m, rt.cell_release);
    m.func(&[ValType::I64], &[], &[ValType::I32, ValType::I32], 0, |b| {
        let (sl, g) = (1, 2);
        b.ins(&Instruction::I32Const(slab as i32))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::LocalSet(sl));
        b.ins(&Instruction::LocalGet(sl))
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::I32Const(8))
            .ins(&Instruction::I32Mul)
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalTee(g))
            .ins(&Instruction::LocalGet(g))
            .ins(&Instruction::I64Load(word8()))
            .ins(&Instruction::I64Const(1))
            .ins(&Instruction::I64Add)
            .ins(&Instruction::I64Store(word8()));
        b.ins(&Instruction::LocalGet(sl))
            .ins(&Instruction::I32Const(CELL_FREE as i32))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Const(freetop as i32))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::I32Const(4))
            .ins(&Instruction::I32Mul)
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::I32Store(word()));
        b.ins(&Instruction::I32Const(freetop as i32))
            .ins(&Instruction::I32Const(freetop as i32))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Store(word()));
    });

    // cell_srcp(slot) -> the address of src[slot] (RFC-0075 M2c). The address
    // rather than the value, so one function serves the wrapper that writes it,
    // the `pull` that reads it and the release that walks it. No generation
    // check: every caller has just done one, or is `close` itself.
    rt.next_is(m, rt.cell_srcp);
    m.func(&[ValType::I64], &[ValType::I32], &[], 0, |b| {
        b.ins(&Instruction::I32Const(slab as i32))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::I32Const(CELL_SRC as i32))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::I32Const(4))
            .ins(&Instruction::I32Mul)
            .ins(&Instruction::I32Add);
    });
}

/// Rights and flags from the `wasi_snapshot_preview1` witx, named rather than
/// spelled at the call: a wrong bit in `path_open` is an `ENOTCAPABLE` that reads
/// exactly like a missing file, i.e. a canonical `Err` for the wrong reason.
const RIGHT_FD_READ: i64 = 1 << 1;
const RIGHT_FD_WRITE: i64 = 1 << 6;
const OFLAGS_CREAT_TRUNC: i32 = 1 | 8;
/// `lookupflags`: follow a symlink, which is what `fopen` does.
const LOOKUP_SYMLINK_FOLLOW: i32 = 1;
/// `errno::xdev`, the last of preview1's alphabetical errno list — and NOT POSIX's
/// `EXDEV`, which is 18 and is `errno::dom` here. RFC-0044's cross-device rename
/// is the one place this backend has to read a WASI errno by value rather than
/// just testing it against zero.
const ERRNO_XDEV: i32 = 75;

/// RFC-0014's input I/O and RFC-0043's host boundary, over raw WASI.
///
/// M2i established that reaching the shared shim can never be the default — M5
/// requires `vyrn build --target wasm` to need no clang — so "the shim defines
/// these on every target" stopped being an answer for the standalone shape. WASI
/// has `clock_time_get`, `random_get`, `args_get`, `fd_read` and `path_open`, and
/// wasi-libc's own `timespec_get`/`getentropy`/`fopen` are thin wrappers over
/// exactly those, so this is the same syscall by a shorter route.
///
/// The three semantics that are parity-critical rather than mechanical, all
/// RFC-0014:
///
/// - **The canonical wording is single-sourced.** Every message comes from
///   [`crate::io_message_parts`], split on the `%s` the textual backend hands to
///   `__vyrn_snprintf`. A backend that spelled `cannot read` itself would be a
///   second wording of one fact, and parity compares these bytes.
/// - **The NUL rule.** A file (or a line) containing a NUL byte is rejected
///   BEFORE the UTF-8 check and with its own message, because a Vyrn `String` is
///   NUL-terminated and could not carry one.
/// - **`readLine` is `None` at EOF**, and also for a NUL or invalid UTF-8 —
///   exactly where the interpreter's `String::from_utf8` fails.
///
/// The functions are added in the order [`Rt`] hands out their indices; a body
/// added out of turn would renumber every call to it and the module would still
/// validate (M2e's finding, in a different place).
fn io_runtime(m: &mut Module, rt: &Rt, wasi: &Wasi, gen: Option<&Gen>) {
    let (malloc, strlen, utf8valid, concat) = (rt.malloc, rt.strlen, rt.utf8valid, rt.concat);
    let triple = layout::of_ll("{ ptr, i64, i64 }").expect("the growable-array triple");
    let sum2 = layout::of_ll("{ i1, i64, i64 }").expect("the Option/Result shape");
    // RFC-0043's injected clock and seed. The env NAME carries its own `=`, so a
    // lookup is one prefix test and the value is whatever follows — no separate
    // check that the separator is where it should be.
    let fixed_time = rt.intern(m, "VYRN_FIXED_TIME=");
    let fixed_seed = rt.intern(m, "VYRN_FIXED_SEED=");
    // The monotonic counter under a fixed clock: a static, because successive
    // calls have to differ and the interpreter's own base/step is `1e9 + n·1e6`.
    let mono_ctr = m.reserve(8, 8);

    // starts(a, b) — whether NUL-terminated `a` begins with NUL-terminated `b`.
    rt.next_is(m, rt.starts);
    m.func(&[ValType::I32, ValType::I32], &[ValType::I32], &[ValType::I32], 0, |b| {
        let c = 3; // params 0..1, the frame base 2, then ours
        b.ins(&Instruction::Block(BlockType::Result(ValType::I32)))
            .ins(&Instruction::Loop(BlockType::Empty))
            .ins(&Instruction::LocalGet(1))
            .ins(&Instruction::I32Load8U(byte()))
            .ins(&Instruction::LocalTee(c))
            .ins(&Instruction::I32Eqz)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::Br(2))
            .ins(&Instruction::End)
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32Load8U(byte()))
            .ins(&Instruction::LocalGet(c))
            .ins(&Instruction::I32Ne)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(0))
            .ins(&Instruction::Br(2))
            .ins(&Instruction::End)
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalSet(0))
            .ins(&Instruction::LocalGet(1))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalSet(1))
            .ins(&Instruction::Br(0))
            .ins(&Instruction::End)
            .ins(&Instruction::Unreachable)
            .ins(&Instruction::End);
    });

    // env_get(key) — the value of the environment entry `key` names (`key`
    // includes its `=`), or 0. WASI hands the whole environment over in one go,
    // so this is `environ_get` plus a prefix scan; nothing caches it, because a
    // clock program makes a handful of calls and the bump allocator's cost for
    // them is a few hundred bytes that nothing can observe.
    let (env_sizes, env_get_i) = (wasi.environ_sizes_get, wasi.environ_get);
    let starts = rt.starts;
    rt.next_is(m, rt.env_get);
    m.func(
        &[ValType::I32],
        &[ValType::I32],
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        8,
        |b| {
            let (cnt, ptrs, i, e) = (2, 3, 4, 5); // param 0, base 1, then ours
            b.ins(&Instruction::Block(BlockType::Result(ValType::I32)));
            // Zeroed first: a failing `environ_sizes_get` leaves the frame slots
            // holding whatever the last call put there, and a garbage count is a
            // scan over garbage pointers rather than an empty environment.
            b.slot(0).ins(&Instruction::I32Const(0)).ins(&Instruction::I32Store(word()));
            b.slot(4).ins(&Instruction::I32Const(0)).ins(&Instruction::I32Store(word()));
            b.slot(0);
            b.slot(4);
            b.ins(&Instruction::Call(env_sizes))
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::Br(1))
                .ins(&Instruction::End);
            b.slot(0).ins(&Instruction::I32Load(word())).ins(&Instruction::LocalTee(cnt));
            b.ins(&Instruction::I32Const(2))
                .ins(&Instruction::I32Shl)
                .ins(&Instruction::I32Const(8))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::LocalSet(ptrs))
                .ins(&Instruction::LocalGet(ptrs));
            b.slot(4)
                .ins(&Instruction::I32Load(word()))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::Call(env_get_i))
                .ins(&Instruction::Drop);
            b.ins(&Instruction::I32Const(0)).ins(&Instruction::LocalSet(i));
            b.ins(&Instruction::Block(BlockType::Empty)).ins(&Instruction::Loop(BlockType::Empty));
            b.ins(&Instruction::LocalGet(i))
                .ins(&Instruction::LocalGet(cnt))
                .ins(&Instruction::I32GeU)
                .ins(&Instruction::BrIf(1));
            b.ins(&Instruction::LocalGet(ptrs))
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32Const(2))
                .ins(&Instruction::I32Shl)
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Load(word()))
                .ins(&Instruction::LocalTee(e))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::Call(starts))
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(e))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::Call(strlen))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::Br(3))
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(i))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End);
            b.ins(&Instruction::I32Const(0)).ins(&Instruction::End);
        },
    );

    // str_i64(p) — `strtoll(p, 0, 10)` for everything an injected value can be:
    // an optional sign then decimal digits, stopping at the first byte that is not
    // one.
    //
    // ponytail: no leading-whitespace skip and no overflow clamp to
    // `LLONG_MAX`/`MIN`. Both are `strtoll` behaviours nothing reaches — the only
    // callers are `VYRN_FIXED_TIME` and `VYRN_FIXED_SEED`, which the harness writes
    // as bare decimals. A program that could pass arbitrary text here would need
    // the real thing.
    rt.next_is(m, rt.str_i64);
    m.func(
        &[ValType::I32],
        &[ValType::I64],
        &[ValType::I64, ValType::I32, ValType::I32],
        0,
        |b| {
            let (v, neg, c) = (2, 3, 4);
            b.ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::LocalTee(c))
                .ins(&Instruction::I32Const(b'-' as i32))
                .ins(&Instruction::I32Eq)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::LocalSet(neg))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(0))
                .ins(&Instruction::Else)
                .ins(&Instruction::LocalGet(c))
                .ins(&Instruction::I32Const(b'+' as i32))
                .ins(&Instruction::I32Eq)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End);
            b.ins(&Instruction::Block(BlockType::Empty))
                .ins(&Instruction::Loop(BlockType::Empty))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::LocalTee(c))
                .ins(&Instruction::I32Const(b'0' as i32))
                .ins(&Instruction::I32LtU)
                .ins(&Instruction::BrIf(1))
                .ins(&Instruction::LocalGet(c))
                .ins(&Instruction::I32Const(b'9' as i32))
                .ins(&Instruction::I32GtU)
                .ins(&Instruction::BrIf(1))
                .ins(&Instruction::LocalGet(v))
                .ins(&Instruction::I64Const(10))
                .ins(&Instruction::I64Mul)
                .ins(&Instruction::LocalGet(c))
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::I64Const(b'0' as i64))
                .ins(&Instruction::I64Sub)
                .ins(&Instruction::I64Add)
                .ins(&Instruction::LocalSet(v))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(0))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(neg))
                .ins(&Instruction::If(BlockType::Result(ValType::I64)))
                .ins(&Instruction::I64Const(0))
                .ins(&Instruction::LocalGet(v))
                .ins(&Instruction::I64Sub)
                .ins(&Instruction::Else)
                .ins(&Instruction::LocalGet(v))
                .ins(&Instruction::End);
        },
    );

    // The injected-value preamble every one of the three host readings starts
    // with: `if (e && e[0]) return str_i64(e)`, which is the C shim's own guard —
    // an env var set to the empty string falls through to the real host.
    let (env_get, str_i64) = (rt.env_get, rt.str_i64);
    let fixed = move |b: &mut Frame, key: u32, out: u32| {
        b.ins(&Instruction::I32Const(key as i32))
            .ins(&Instruction::Call(env_get))
            .ins(&Instruction::LocalTee(out))
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::LocalGet(out))
            .ins(&Instruction::I32Load8U(byte()))
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::LocalGet(out))
            .ins(&Instruction::Call(str_i64))
            .ins(&Instruction::Br(2))
            .ins(&Instruction::End)
            .ins(&Instruction::End);
    };

    // now_millis() — epoch millis. `clock_time_get(REALTIME)` is nanoseconds, and
    // the native shim's `tv_sec*1000 + tv_nsec/1e6` is the same floor division.
    // A failing clock reads 0, which is what `timespec_get` returning 0 gives.
    let clock_time_get = wasi.clock_time_get;
    rt.next_is(m, rt.now_millis);
    m.func(&[], &[ValType::I64], &[ValType::I32], 8, |b| {
        let p = 1; // no params, the frame base 0, then ours
        b.ins(&Instruction::Block(BlockType::Result(ValType::I64)));
        fixed(b, fixed_time, p);
        b.ins(&Instruction::I32Const(0)).ins(&Instruction::I64Const(1_000_000));
        b.slot(0);
        b.ins(&Instruction::Call(clock_time_get))
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I64Const(0))
            .ins(&Instruction::Br(1))
            .ins(&Instruction::End);
        b.slot(0);
        b.ins(&Instruction::I64Load(word8()))
            .ins(&Instruction::I64Const(1_000_000))
            .ins(&Instruction::I64DivU)
            .ins(&Instruction::End);
    });

    // mono_nanos() — monotonic nanoseconds. Under a fixed clock the interpreter's
    // own base and step, so successive calls are byte-identical across backends;
    // otherwise `clock_time_get(MONOTONIC)`.
    rt.next_is(m, rt.mono_nanos);
    m.func(&[], &[ValType::I64], &[ValType::I32], 8, |b| {
        let p = 1;
        b.ins(&Instruction::Block(BlockType::Result(ValType::I64)))
            .ins(&Instruction::I32Const(fixed_time as i32))
            .ins(&Instruction::Call(env_get))
            .ins(&Instruction::LocalTee(p))
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Load8U(byte()))
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(mono_ctr as i32))
            .ins(&Instruction::I64Load(word8()))
            .ins(&Instruction::I64Const(1_000_000))
            .ins(&Instruction::I64Mul)
            .ins(&Instruction::I64Const(1_000_000_000))
            .ins(&Instruction::I64Add)
            .ins(&Instruction::I32Const(mono_ctr as i32))
            .ins(&Instruction::I32Const(mono_ctr as i32))
            .ins(&Instruction::I64Load(word8()))
            .ins(&Instruction::I64Const(1))
            .ins(&Instruction::I64Add)
            .ins(&Instruction::I64Store(word8()))
            .ins(&Instruction::Br(2))
            .ins(&Instruction::End)
            .ins(&Instruction::End);
        b.ins(&Instruction::I32Const(1)).ins(&Instruction::I64Const(1_000));
        b.slot(0);
        b.ins(&Instruction::Call(clock_time_get)).ins(&Instruction::Drop);
        b.slot(0);
        b.ins(&Instruction::I64Load(word8())).ins(&Instruction::End);
    });

    // random_seed() — eight CSPRNG bytes as an `Int64`, which is what the native
    // shim's `getentropy(&v, sizeof v)` reads. Zeroing first is not tidiness: the
    // C leaves `v = 0` when `getentropy` fails, so pre-zeroing IS the error path
    // and there is no errno to check.
    let random_get = wasi.random_get;
    rt.next_is(m, rt.random_seed);
    m.func(&[], &[ValType::I64], &[ValType::I32], 8, |b| {
        let p = 1;
        b.ins(&Instruction::Block(BlockType::Result(ValType::I64)));
        fixed(b, fixed_seed, p);
        b.slot(0);
        b.ins(&Instruction::I64Const(0)).ins(&Instruction::I64Store(word8()));
        b.slot(0);
        b.ins(&Instruction::I32Const(8))
            .ins(&Instruction::Call(random_get))
            .ins(&Instruction::Drop);
        b.slot(0);
        b.ins(&Instruction::I64Load(word8())).ins(&Instruction::End);
    });

    // args(dest) — `argv[1..]` as an `Array<String>` triple written through `dest`.
    //
    // WASI writes the pointers as a contiguous array of wasm32 pointers, which is
    // exactly the buffer an `Array<String>` wants — the element stride IS
    // `of_ll("ptr")` — so dropping the program name is `+ 4` rather than a copy.
    let (args_sizes_get, args_get) = (wasi.args_sizes_get, wasi.args_get);
    rt.next_is(m, rt.args);
    m.func(&[ValType::I32], &[], &[ValType::I32, ValType::I32], 8, |b| {
        let (cnt, ptrs) = (2, 3);
        b.slot(0).ins(&Instruction::I32Const(0)).ins(&Instruction::I32Store(word()));
        b.slot(4).ins(&Instruction::I32Const(0)).ins(&Instruction::I32Store(word()));
        b.slot(0);
        b.slot(4);
        b.ins(&Instruction::Call(args_sizes_get)).ins(&Instruction::Drop);
        b.slot(0);
        b.ins(&Instruction::I32Load(word()))
            .ins(&Instruction::LocalTee(cnt))
            .ins(&Instruction::I32Const(2))
            .ins(&Instruction::I32Shl)
            // Two words of slack, so `ptrs + 4` is inside the allocation even
            // when there are no arguments at all and it is never read.
            .ins(&Instruction::I32Const(8))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I64ExtendI32U)
            .ins(&Instruction::Call(malloc))
            .ins(&Instruction::LocalSet(ptrs))
            .ins(&Instruction::LocalGet(ptrs));
        b.slot(4);
        b.ins(&Instruction::I32Load(word()))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I64ExtendI32U)
            .ins(&Instruction::Call(malloc))
            .ins(&Instruction::Call(args_get))
            .ins(&Instruction::Drop);
        b.ins(&Instruction::LocalGet(cnt))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32GtU)
            .ins(&Instruction::If(BlockType::Result(ValType::I32)))
            .ins(&Instruction::LocalGet(cnt))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::Else)
            .ins(&Instruction::I32Const(0))
            .ins(&Instruction::End)
            .ins(&Instruction::LocalSet(cnt));
        b.ins(&Instruction::LocalGet(0))
            .ins(&Instruction::LocalGet(ptrs))
            .ins(&Instruction::I32Const(4))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Store(word_at(triple.fields[0])));
        for f in [triple.fields[1], triple.fields[2]] {
            b.ins(&Instruction::LocalGet(0))
                .ins(&Instruction::LocalGet(cnt))
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::I64Store(at(f)));
        }
    });

    // getbyte() — one byte from stdin, or -1 at EOF (and on any error, which is
    // what `getchar` returning `EOF` covers too).
    //
    // ponytail: one `fd_read` per byte, where C's `getchar` is buffered. `readLine`
    // is the only caller and the corpus feeds it a few hundred bytes from a
    // fixture; a 4 KB buffer here would need its own invalidation story to stay
    // correct if anything else ever reads fd 0.
    let fd_read = wasi.fd_read;
    rt.next_is(m, rt.getbyte);
    m.func(&[], &[ValType::I32], &[], 16, |b| {
        b.slot(0);
        b.slot(12);
        b.ins(&Instruction::I32Store(word()));
        b.slot(4).ins(&Instruction::I32Const(1)).ins(&Instruction::I32Store(word()));
        b.slot(8).ins(&Instruction::I32Const(0)).ins(&Instruction::I32Store(word()));
        b.ins(&Instruction::I32Const(0));
        b.slot(0);
        b.ins(&Instruction::I32Const(1));
        b.slot(8);
        b.ins(&Instruction::Call(fd_read)).ins(&Instruction::If(BlockType::Result(ValType::I32)));
        b.ins(&Instruction::I32Const(-1)).ins(&Instruction::Else);
        b.slot(8);
        b.ins(&Instruction::I32Load(word()))
            .ins(&Instruction::I32Eqz)
            .ins(&Instruction::If(BlockType::Result(ValType::I32)))
            .ins(&Instruction::I32Const(-1))
            .ins(&Instruction::Else);
        b.slot(12);
        b.ins(&Instruction::I32Load8U(byte()))
            .ins(&Instruction::End)
            .ins(&Instruction::End);
    });

    // read_line(dest) — RFC-0014's `readLine()` as an `Option<String>` written
    // through `dest`. `None` at EOF, for a line carrying a NUL byte (which a
    // NUL-terminated `String` could not hold), and for one that is not UTF-8 —
    // the last of those is where the interpreter's `String::from_utf8` fails, so
    // the DFA decides it here rather than the caller.
    let getbyte = rt.getbyte;
    rt.next_is(m, rt.read_line);
    m.func(
        &[ValType::I32],
        &[],
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        0,
        |b| {
            let (buf, cap, len, c, nul, nb) = (2, 3, 4, 5, 6, 7);
            b.ins(&Instruction::Block(BlockType::Empty)) // 1: fin
                .ins(&Instruction::Block(BlockType::Empty)) // 0: none
                .ins(&Instruction::Call(getbyte))
                .ins(&Instruction::LocalTee(c))
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::I32LtS)
                .ins(&Instruction::BrIf(0))
                .ins(&Instruction::I32Const(64))
                .ins(&Instruction::LocalTee(cap))
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::LocalSet(buf))
                .ins(&Instruction::Block(BlockType::Empty)) // 0: eol
                .ins(&Instruction::Loop(BlockType::Empty)) // 0: rd
                .ins(&Instruction::LocalGet(c))
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::I32LtS)
                .ins(&Instruction::BrIf(1))
                .ins(&Instruction::LocalGet(c))
                .ins(&Instruction::I32Const(b'\n' as i32))
                .ins(&Instruction::I32Eq)
                .ins(&Instruction::BrIf(1))
                .ins(&Instruction::LocalGet(c))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::LocalSet(nul))
                .ins(&Instruction::End)
                // Grow at len+2 rather than len+1: the terminator goes on after
                // the loop and must not need a reallocation of its own.
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32Const(2))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalGet(cap))
                .ins(&Instruction::I32GeU)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(cap))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Shl)
                .ins(&Instruction::LocalTee(cap))
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::LocalTee(nb))
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 })
                .ins(&Instruction::LocalGet(nb))
                .ins(&Instruction::LocalSet(buf))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalGet(c))
                .ins(&Instruction::I32Store8(byte()))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(len))
                .ins(&Instruction::Call(getbyte))
                .ins(&Instruction::LocalSet(c))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End);
            // A trailing `\r` goes with the `\n`, so a Windows pipe and a POSIX
            // one read identically.
            b.ins(&Instruction::LocalGet(len))
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Const(-1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::I32Const(b'\r' as i32))
                .ins(&Instruction::I32Eq)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32Const(-1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(len))
                .ins(&Instruction::End)
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::I32Store8(byte()))
                .ins(&Instruction::LocalGet(nul))
                .ins(&Instruction::BrIf(0))
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::Call(utf8valid))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::BrIf(0));
            sum2_write(b, &sum2, 1, Some(buf));
            b.ins(&Instruction::Br(1)).ins(&Instruction::End); // none
            sum2_write(b, &sum2, 0, None);
            b.ins(&Instruction::End); // fin
        },
    );

    // open_at(path, oflags, rights) — a file descriptor, or -1.
    //
    // WASI has no `open` relative to a working directory: every path is resolved
    // under a preopened directory, and the host decides which ones exist. So the
    // preopens are walked from fd 3 (the first one WASI can hand out) until
    // `fd_prestat_get` says there are no more, and the first that resolves the
    // path wins — which is `--dir .` giving exactly one, and no preopens at all
    // (a browser) giving -1 for every path, i.e. RFC-0014's canonical `Err`.
    //
    // ponytail: no prefix matching against the preopens' own names, so an
    // ABSOLUTE guest path only opens under a preopen mounted at `/`. wasi-libc
    // does the matching for the textual backend; nothing in the corpus has an
    // absolute path, and adding it means a string-prefix walk over
    // `fd_prestat_dir_name` for a case no example has.
    let (path_open, fd_prestat_get) = (wasi.path_open, wasi.fd_prestat_get);
    rt.next_is(m, rt.open_at);
    m.func(
        &[ValType::I32, ValType::I32, ValType::I64],
        &[ValType::I32],
        &[ValType::I32, ValType::I32],
        16,
        |b| {
            let (fd, plen) = (4, 5); // params 0..2, the frame base 3, then ours
            b.ins(&Instruction::LocalGet(0))
                .ins(&Instruction::Call(strlen))
                .ins(&Instruction::LocalSet(plen))
                .ins(&Instruction::I32Const(3))
                .ins(&Instruction::LocalSet(fd))
                .ins(&Instruction::Block(BlockType::Result(ValType::I32)))
                .ins(&Instruction::Loop(BlockType::Empty))
                .ins(&Instruction::LocalGet(fd));
            b.slot(0);
            b.ins(&Instruction::Call(fd_prestat_get))
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(-1))
                .ins(&Instruction::Br(2))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(fd))
                .ins(&Instruction::I32Const(LOOKUP_SYMLINK_FOLLOW))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::LocalGet(plen))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::LocalGet(2))
                .ins(&Instruction::I64Const(0))
                .ins(&Instruction::I32Const(0));
            b.slot(8);
            b.ins(&Instruction::Call(path_open))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::If(BlockType::Empty));
            b.slot(8);
            b.ins(&Instruction::I32Load(word()))
                .ins(&Instruction::Br(2))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(fd))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(fd))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::Unreachable)
                .ins(&Instruction::End);
        },
    );

    // read_all(fd, outlen) — the whole descriptor into one NUL-terminated buffer,
    // with its byte length through `outlen`; 0 on a read error.
    //
    // A read loop rather than a stat-and-slurp, for the reason the C shim gives:
    // it is the same code for a regular file and for a pipe. The terminator is
    // there so a `String` result needs no second copy, and it is past `outlen`
    // bytes so a bytes result simply ignores it.
    rt.next_is(m, rt.read_all);
    m.func(
        &[ValType::I32, ValType::I32],
        &[ValType::I32],
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        16,
        |b| {
            let (buf, cap, len, nb, got) = (3, 4, 5, 6, 7);
            b.ins(&Instruction::I32Const(1024))
                .ins(&Instruction::LocalTee(cap))
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::LocalSet(buf))
                .ins(&Instruction::Block(BlockType::Empty))
                .ins(&Instruction::Loop(BlockType::Empty))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalGet(cap))
                .ins(&Instruction::I32GeU)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(cap))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Shl)
                .ins(&Instruction::LocalTee(cap))
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::LocalTee(nb))
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 })
                .ins(&Instruction::LocalGet(nb))
                .ins(&Instruction::LocalSet(buf))
                .ins(&Instruction::End);
            b.slot(0);
            b.ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Store(word()));
            b.slot(4);
            b.ins(&Instruction::LocalGet(cap))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32Sub)
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Sub)
                .ins(&Instruction::I32Store(word()));
            b.slot(8);
            b.ins(&Instruction::I32Const(0)).ins(&Instruction::I32Store(word()));
            b.ins(&Instruction::LocalGet(0));
            b.slot(0);
            b.ins(&Instruction::I32Const(1));
            b.slot(8);
            b.ins(&Instruction::Call(fd_read))
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::LocalSet(buf))
                .ins(&Instruction::Br(2))
                .ins(&Instruction::End);
            b.slot(8);
            b.ins(&Instruction::I32Load(word()))
                .ins(&Instruction::LocalTee(got))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::BrIf(1))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::LocalGet(got))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(len))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::I32Store8(byte()))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32Store(word()))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(buf));
        },
    );

    // err3(pre, mid, post) — one canonical I/O message with the path in it.
    //
    // The two halves come from `io_message_parts`, i.e. from the same format
    // string the textual backend hands `__vyrn_snprintf`, so there is no second
    // wording to keep in step. Nothing frees, so the pieces are the interned
    // constants themselves.
    rt.next_is(m, rt.err3);
    m.func(&[ValType::I32, ValType::I32, ValType::I32], &[ValType::I32], &[], 0, |b| {
        b.ins(&Instruction::LocalGet(0))
            .ins(&Instruction::LocalGet(1))
            .ins(&Instruction::Call(concat))
            .ins(&Instruction::LocalGet(2))
            .ins(&Instruction::Call(concat));
    });

    let msg = |m: &mut Module, which: &str| {
        let (pre, post) = crate::io_message_parts(which);
        (rt.intern(m, pre), rt.intern(m, post))
    };
    let (readpre, readpost) = msg(m, "readerr");
    let (utf8pre, utf8post) = msg(m, "utf8err");
    let (nulpre, nulpost) = msg(m, "nulerr");
    let (writepre, writepost) = msg(m, "writeerr");

    let (open_at, read_all, err3) = (rt.open_at, rt.read_all, rt.err3);
    let fd_close = wasi.fd_close;
    let gen = gen.copied();
    // The open-and-slurp both readers start with, leaving the buffer in `buf`, the
    // length in `len`, and branching to `err` (a depth) with the read message set
    // when either step fails.
    //
    // `mode` is RFC-0076's read mode, and it is the only thing the two shapes do
    // not share. A GENERATOR does not open files: it reads through the loader's
    // resolver, which in the LSP serves unsaved buffers, so opening one here would
    // read different bytes than the interpreter does. On that path the whole open
    // is one mediated import and the status comes back in the alphabet the error
    // rendering below already speaks (0 ok / 1 io / 3 embedded NUL) — which is why
    // the wording needed no new agreement when M2 introduced it and needs none now.
    let slurp = move |b: &mut Frame,
                      base_off: u32,
                      fd: u32,
                      buf: u32,
                      len: u32,
                      err_msg: u32,
                      err_depth: u32,
                      mode: i32| {
        if let Some(g) = gen {
            gen_slurp(
                b,
                &g,
                (malloc, err3),
                [readpre, readpost, nulpre, nulpost],
                (buf, len, err_msg, err_depth),
                mode,
            );
            return;
        }
        b.ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32Const(0))
            .ins(&Instruction::I64Const(RIGHT_FD_READ))
            .ins(&Instruction::Call(open_at))
            .ins(&Instruction::LocalTee(fd))
            .ins(&Instruction::I32Const(0))
            .ins(&Instruction::I32LtS)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(readpre as i32))
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32Const(readpost as i32))
            .ins(&Instruction::Call(err3))
            .ins(&Instruction::LocalSet(err_msg))
            .ins(&Instruction::Br(err_depth + 1))
            .ins(&Instruction::End)
            .ins(&Instruction::LocalGet(fd));
        b.slot(base_off);
        b.ins(&Instruction::Call(read_all))
            .ins(&Instruction::LocalSet(buf))
            .ins(&Instruction::LocalGet(fd))
            .ins(&Instruction::Call(fd_close))
            .ins(&Instruction::Drop)
            .ins(&Instruction::LocalGet(buf))
            .ins(&Instruction::I32Eqz)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(readpre as i32))
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32Const(readpost as i32))
            .ins(&Instruction::Call(err3))
            .ins(&Instruction::LocalSet(err_msg))
            .ins(&Instruction::Br(err_depth + 1))
            .ins(&Instruction::End);
        b.slot(base_off);
        b.ins(&Instruction::I32Load(word())).ins(&Instruction::LocalSet(len));
    };

    // read_file(path, dest) — RFC-0014's `readFile` as a `Result<String, String>`.
    //
    // The NUL scan comes BEFORE the UTF-8 check and carries its own wording,
    // because a `String` is NUL-terminated: a file with one in it is not
    // representable rather than badly encoded, and the two messages differ.
    rt.next_is(m, rt.read_file);
    m.func(
        &[ValType::I32, ValType::I32],
        &[],
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        8,
        |b| {
            let (fd, buf, len, emsg, i) = (3, 4, 5, 6, 7);
            b.ins(&Instruction::Block(BlockType::Empty)) // 1: fin
                .ins(&Instruction::Block(BlockType::Empty)); // 0: err
            slurp(b, 0, fd, buf, len, emsg, 0, crate::GEN_MODE_READ);
            b.ins(&Instruction::Block(BlockType::Empty)) // 0: scanned
                .ins(&Instruction::Loop(BlockType::Empty)) // 0: sl
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32GeU)
                .ins(&Instruction::BrIf(1))
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(nulpre as i32))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Const(nulpost as i32))
                .ins(&Instruction::Call(err3))
                .ins(&Instruction::LocalSet(emsg))
                // out of the `if`, the scan loop and its block, landing where the
                // `err` block's own exit lands.
                .ins(&Instruction::Br(3))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(i))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::Call(utf8valid))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(utf8pre as i32))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Const(utf8post as i32))
                .ins(&Instruction::Call(err3))
                .ins(&Instruction::LocalSet(emsg))
                .ins(&Instruction::Br(1))
                .ins(&Instruction::End);
            sum2_write_to(b, 1, &sum2, 1, Some(buf));
            b.ins(&Instruction::Br(1)).ins(&Instruction::End); // err
            sum2_write_to(b, 1, &sum2, 0, Some(emsg));
            b.ins(&Instruction::End); // fin
        },
    );

    // read_file_bytes(path, dest) — the same open-and-slurp with no NUL or UTF-8
    // rule at all, which is the whole point of a byte read, as a
    // `Result<Array<UInt8>, String>`.
    //
    // The `Ok` payload is three words, so it does not fit the sum's two: it is
    // boxed, exactly as `Fn_::box_value` would box it, and the box IS the triple
    // rather than a copy of one built elsewhere.
    rt.next_is(m, rt.read_file_bytes);
    m.func(
        &[ValType::I32, ValType::I32],
        &[],
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        8,
        |b| {
            let (fd, buf, len, emsg, boxed) = (3, 4, 5, 6, 7);
            b.ins(&Instruction::Block(BlockType::Empty)) // 1: fin
                .ins(&Instruction::Block(BlockType::Empty)); // 0: err
            slurp(b, 0, fd, buf, len, emsg, 0, crate::GEN_MODE_READ_BYTES);
            b.ins(&Instruction::I64Const(triple.size as i64))
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::LocalTee(boxed))
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::I32Store(word_at(triple.fields[0])));
            for f in [triple.fields[1], triple.fields[2]] {
                b.ins(&Instruction::LocalGet(boxed))
                    .ins(&Instruction::LocalGet(len))
                    .ins(&Instruction::I64ExtendI32U)
                    .ins(&Instruction::I64Store(at(f)));
            }
            sum2_write_to(b, 1, &sum2, 1, Some(boxed));
            b.ins(&Instruction::Br(1)).ins(&Instruction::End); // err
            sum2_write_to(b, 1, &sum2, 0, Some(emsg));
            b.ins(&Instruction::End); // fin
        },
    );

    // write_file(path, contents, dest) — create-or-truncate and write every byte,
    // as a `Result<Bool, String>` whose `Ok` is `true`.
    //
    // A Vyrn `String` never contains a NUL (the readers above are why), so
    // `strlen` is its full length and there is no separate length to pass.
    let write_all = rt.write_all;
    rt.next_is(m, rt.write_file);
    m.func(
        &[ValType::I32, ValType::I32, ValType::I32],
        &[],
        &[ValType::I32, ValType::I32],
        0,
        |b| {
            let (fd, emsg) = (4, 5); // params 0..2, the frame base 3, then ours
            b.ins(&Instruction::Block(BlockType::Empty)) // 1: fin
                .ins(&Instruction::Block(BlockType::Empty)) // 0: err
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Const(OFLAGS_CREAT_TRUNC))
                .ins(&Instruction::I64Const(RIGHT_FD_WRITE))
                .ins(&Instruction::Call(open_at))
                .ins(&Instruction::LocalTee(fd))
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::I32LtS)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(writepre as i32))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I32Const(writepost as i32))
                .ins(&Instruction::Call(err3))
                .ins(&Instruction::LocalSet(emsg))
                .ins(&Instruction::Br(1))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(fd))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::Call(strlen))
                .ins(&Instruction::Call(write_all))
                .ins(&Instruction::LocalGet(fd))
                .ins(&Instruction::Call(fd_close))
                .ins(&Instruction::Drop);
            // `Ok(true)`: the payload is a `Bool`, zero-extended into the word,
            // which is the encoding `build_sum2`'s `Word::Ext` arm produces.
            b.ins(&Instruction::LocalGet(2))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Store8(MemArg {
                    offset: sum2.fields[0] as u64,
                    align: 0,
                    memory_index: 0,
                }))
                .ins(&Instruction::LocalGet(2))
                .ins(&Instruction::I64Const(1))
                .ins(&Instruction::I64Store(at(sum2.fields[1])))
                .ins(&Instruction::LocalGet(2))
                .ins(&Instruction::I64Const(0))
                .ins(&Instruction::I64Store(at(sum2.fields[2])))
                .ins(&Instruction::Br(1))
                .ins(&Instruction::End); // err
            sum2_write_to(b, 2, &sum2, 0, Some(emsg));
            b.ins(&Instruction::End); // fin
        },
    );

    // rename_file(from, to, dest) — RFC-0044's atomic overwrite, as a
    // `Result<Bool, String>` whose `Ok` is `true`.
    //
    // `path_rename` needs a directory fd for each side, so this is `open_at`'s
    // preopen walk without the open: the first preopen under which the rename
    // resolves wins. Both paths go through the SAME fd, which is also why the
    // cross-device arm is nearly unreachable here — a preopen is one mount.
    //
    // Two failure classes, because RFC-0044 has two: `EXDEV` is
    // `@.io.xdeverr` and everything else is `@.io.writeerr`, both about the
    // TARGET path. The interpreter picks between the same two on
    // `is_cross_device`, and this reads the same words out of `IO_MESSAGES`
    // rather than spelling either.
    let (xdevpre, xdevpost) = msg(m, "xdeverr");
    let path_rename = wasi.path_rename;
    rt.next_is(m, rt.rename_file);
    m.func(
        &[ValType::I32, ValType::I32, ValType::I32],
        &[],
        &[ValType::I32; 7],
        8,
        |b| {
            // params 0..2, the frame base 3, then ours.
            let (fd, flen, tlen, xdev, st, e, emsg) = (4, 5, 6, 7, 8, 9, 10);
            b.ins(&Instruction::LocalGet(0))
                .ins(&Instruction::Call(strlen))
                .ins(&Instruction::LocalSet(flen))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::Call(strlen))
                .ins(&Instruction::LocalSet(tlen))
                .ins(&Instruction::I32Const(3))
                .ins(&Instruction::LocalSet(fd))
                // Nothing resolved yet: an io failure until a preopen says
                // otherwise, which is also the answer a browser gets.
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::LocalSet(st))
                .ins(&Instruction::Block(BlockType::Empty)) // 1: decided
                .ins(&Instruction::Loop(BlockType::Empty)) // 0: next preopen
                .ins(&Instruction::LocalGet(fd));
            b.slot(0);
            b.ins(&Instruction::Call(fd_prestat_get))
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::Br(2))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(fd))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::LocalGet(flen))
                .ins(&Instruction::LocalGet(fd))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::LocalGet(tlen))
                .ins(&Instruction::Call(path_rename))
                .ins(&Instruction::LocalTee(e))
                .ins(&Instruction::I32Eqz)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::LocalSet(st))
                .ins(&Instruction::Br(2))
                .ins(&Instruction::End)
                // Remembered rather than returned: a later preopen may still
                // resolve the pair, and only if none does is WHY the last one
                // failed the answer.
                .ins(&Instruction::LocalGet(e))
                .ins(&Instruction::I32Const(ERRNO_XDEV))
                .ins(&Instruction::I32Eq)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::LocalSet(xdev))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(fd))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(fd))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End) // loop
                .ins(&Instruction::End); // decided
            b.ins(&Instruction::LocalGet(st)).ins(&Instruction::If(BlockType::Empty));
            b.ins(&Instruction::LocalGet(xdev))
                .ins(&Instruction::If(BlockType::Result(ValType::I32)))
                .ins(&Instruction::I32Const(xdevpre as i32))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I32Const(xdevpost as i32))
                .ins(&Instruction::Call(err3))
                .ins(&Instruction::Else)
                .ins(&Instruction::I32Const(writepre as i32))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I32Const(writepost as i32))
                .ins(&Instruction::Call(err3))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalSet(emsg));
            sum2_write_to(b, 2, &sum2, 0, Some(emsg));
            b.ins(&Instruction::Else);
            // `Ok(true)`: a `Bool` payload is the word zero-extended, which is
            // what `sum2_write_to` does with a local — so the `1` needs one.
            b.ins(&Instruction::I32Const(1)).ins(&Instruction::LocalSet(e));
            sum2_write_to(b, 2, &sum2, 1, Some(e));
            b.ins(&Instruction::End);
        },
    );

    // list_dir(path, dest) — `listDir` as a `Result<Array<String>, String>`, on the
    // generator path only (RFC-0021 gives it no runtime meaning at all, and an
    // ordinary build still refuses it by name).
    //
    // The host sorts the entries and joins them with `\n` — the interpreter's own
    // recording encoding, so a directory whose contents change invalidates the same
    // cache entry under either engine — and this splits them in place. That is what
    // the C shim's `__vyrn_gen_list_dir` does, and it is safe for the same reason:
    // an entry name cannot contain a newline, so the join is invertible.
    //
    // The `Ok` payload is the `Array<String>` triple, which is three words where a
    // sum's payload is two, so it is BOXED — the same `Word::Boxed` encoding at the
    // same `layout::of_ll ∘ llt` offsets `read_file_bytes` uses.
    if let (Some(list_dir), Some(g)) = (rt.list_dir, gen) {
        let (listpre, listpost) = msg(m, "listerr");
        // A `String` element is a `ptr`, so the names buffer is a `char**` — the
        // stride comes off the layout engine rather than off a 4 written here.
        let stride = layout::of_ll("ptr").expect("a pointer has a layout").size as i32;
        rt.next_is(m, list_dir);
        m.func(&[ValType::I32, ValType::I32], &[], &[ValType::I32; 9], 0, |b| {
            // params 0..1, the frame base 2, then ours.
            let (buf, len, n, i, names, start, k, emsg, boxed) = (3, 4, 5, 6, 7, 8, 9, 10, 11);
            b.ins(&Instruction::Block(BlockType::Empty)) // 1: fin
                .ins(&Instruction::Block(BlockType::Empty)); // 0: err
            gen_slurp(
                b,
                &g,
                (malloc, err3),
                [listpre, listpost, listpre, listpost],
                (buf, len, emsg, 0),
                crate::GEN_MODE_LIST,
            );
            // One pass to count separators, because the pointer array has to be
            // allocated before the second pass can fill it.
            b.ins(&Instruction::Block(BlockType::Empty))
                .ins(&Instruction::Loop(BlockType::Empty))
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32GeU)
                .ins(&Instruction::BrIf(1))
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::I32Const(b'\n' as i32))
                .ins(&Instruction::I32Eq)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(n))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(n))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(i))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End)
                // A non-empty listing has one more name than it has separators; an
                // EMPTY one is zero names rather than one empty name, which is the
                // difference between `[]` and `[""]`.
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::LocalGet(n))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(n))
                .ins(&Instruction::End)
                // A zero-length array still gets a buffer, so the triple's pointer
                // is never null — `push` reallocs from it either way.
                .ins(&Instruction::LocalGet(n))
                .ins(&Instruction::If(BlockType::Result(ValType::I32)))
                .ins(&Instruction::LocalGet(n))
                .ins(&Instruction::Else)
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::End)
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::I64Const(stride as i64))
                .ins(&Instruction::I64Mul)
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::LocalSet(names))
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalSet(start))
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::LocalSet(i));
            let elem = |b: &mut Frame| {
                b.ins(&Instruction::LocalGet(names))
                    .ins(&Instruction::LocalGet(k))
                    .ins(&Instruction::I32Const(stride))
                    .ins(&Instruction::I32Mul)
                    .ins(&Instruction::I32Add)
                    .ins(&Instruction::LocalGet(start))
                    .ins(&Instruction::I32Store(word()));
            };
            b.ins(&Instruction::Block(BlockType::Empty))
                .ins(&Instruction::Loop(BlockType::Empty))
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::I32GeU)
                .ins(&Instruction::BrIf(1))
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Load8U(byte()))
                .ins(&Instruction::I32Const(b'\n' as i32))
                .ins(&Instruction::I32Eq)
                .ins(&Instruction::If(BlockType::Empty))
                // The separator becomes this name's terminator, which is what makes
                // the split in place rather than a copy per entry.
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::I32Store8(byte()));
            elem(b);
            b.ins(&Instruction::LocalGet(k))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(k))
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(start))
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(i))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalSet(i))
                .ins(&Instruction::Br(0))
                .ins(&Instruction::End)
                .ins(&Instruction::End)
                .ins(&Instruction::LocalGet(len))
                .ins(&Instruction::If(BlockType::Empty));
            elem(b);
            b.ins(&Instruction::End)
                .ins(&Instruction::I64Const(triple.size as i64))
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::LocalTee(boxed))
                .ins(&Instruction::LocalGet(names))
                .ins(&Instruction::I32Store(word_at(triple.fields[0])));
            for f in [triple.fields[1], triple.fields[2]] {
                b.ins(&Instruction::LocalGet(boxed))
                    .ins(&Instruction::LocalGet(n))
                    .ins(&Instruction::I64ExtendI32U)
                    .ins(&Instruction::I64Store(at(f)));
            }
            sum2_write_to(b, 1, &sum2, 1, Some(boxed));
            b.ins(&Instruction::Br(1)).ins(&Instruction::End); // err
            sum2_write_to(b, 1, &sum2, 0, Some(emsg));
            b.ins(&Instruction::End); // fin
        });
    }
}

/// RFC-0076's mediated read, in place of `open_at` + `read_all`: the whole
/// resource in one import, into a buffer the GUEST allocates.
///
/// Two calls rather than one because the host must not allocate inside guest
/// memory — `read` resolves the path, mediates it against the generator's allowed
/// roots, reads it, RECORDS it (which is what the on-disk generation cache
/// validates against) and stashes the bytes, returning `(status << 32) | len`;
/// `fetch` copies the stash into what the guest just malloc'd. Byte for byte the
/// protocol `__vyrn_gen_slurp` in the C shim implements, because the host serving
/// it is the same host.
///
/// The status alphabet is the shim's: 0 ok, 1 io, 3 an embedded NUL, which the
/// HOST checks (mode 0 only) because it is holding the bytes. So this picks
/// between two canonical messages, and both halves of each come out of
/// `IO_MESSAGES` — a rejected path and a NUL differ in their prefix AND their
/// suffix, so selecting only one of the two would say `cannot read \`x\` contains
/// a NUL byte`.
///
/// A scoping violation is not in the alphabet at all: it unwinds out of the guest
/// as a host error, so a generator can never observe one as a value.
fn gen_slurp(
    b: &mut Frame,
    g: &Gen,
    (malloc, err3): (u32, u32),
    [readpre, readpost, nulpre, nulpost]: [u32; 4],
    (buf, len, err_msg, err_depth): (u32, u32, u32, u32),
    mode: i32,
) {
    let packed = b.local(ValType::I64);
    let st = b.local(ValType::I32);
    let pick = |b: &mut Frame, nul: u32, other: u32| {
        b.ins(&Instruction::I32Const(nul as i32))
            .ins(&Instruction::I32Const(other as i32))
            .ins(&Instruction::LocalGet(st))
            .ins(&Instruction::I32Const(3))
            .ins(&Instruction::I32Eq)
            .ins(&Instruction::Select);
    };
    b.ins(&Instruction::LocalGet(0))
        .ins(&Instruction::I32Const(mode))
        .ins(&Instruction::Call(g.read))
        .ins(&Instruction::LocalTee(packed))
        .ins(&Instruction::I64Const(32))
        .ins(&Instruction::I64ShrU)
        .ins(&Instruction::I32WrapI64)
        .ins(&Instruction::LocalTee(st))
        .ins(&Instruction::If(BlockType::Empty));
    pick(b, nulpre, readpre);
    b.ins(&Instruction::LocalGet(0));
    pick(b, nulpost, readpost);
    b.ins(&Instruction::Call(err3))
        .ins(&Instruction::LocalSet(err_msg))
        .ins(&Instruction::Br(err_depth + 1))
        .ins(&Instruction::End)
        // The low half is the length; `i32.wrap` IS the mask. The NUL is added
        // back in 64 bits, so a host answering 4 GiB minus one gets an out of
        // memory rather than a one-byte buffer.
        .ins(&Instruction::LocalGet(packed))
        .ins(&Instruction::I32WrapI64)
        .ins(&Instruction::LocalTee(len))
        .ins(&Instruction::I64ExtendI32U)
        .ins(&Instruction::I64Const(1))
        .ins(&Instruction::I64Add)
        .ins(&Instruction::Call(malloc))
        .ins(&Instruction::LocalTee(buf))
        .ins(&Instruction::Call(g.fetch))
        // NUL-terminated, because a Vyrn `String` is a `ptr` and everything
        // downstream scans for the zero.
        .ins(&Instruction::LocalGet(buf))
        .ins(&Instruction::LocalGet(len))
        .ins(&Instruction::I32Add)
        .ins(&Instruction::I32Const(0))
        .ins(&Instruction::I32Store8(byte()));
}

/// Write an `Option`/`Result` through the destination in parameter 0: the tag,
/// then a one-word payload out of `word` (a pointer or a zero-extended scalar),
/// then the unused second word.
///
/// The same three stores `Fn_::build_sum2` emits, at the same
/// `layout::of_ll ∘ llt` offsets — a runtime function that spelled 0/8/16 could
/// disagree with the lowering about where the tag is.
fn sum2_write(b: &mut Frame, l: &Layout, tag: i32, word: Option<u32>) {
    sum2_write_to(b, 0, l, tag, word)
}

fn sum2_write_to(b: &mut Frame, dest: u32, l: &Layout, tag: i32, word: Option<u32>) {
    b.ins(&Instruction::LocalGet(dest))
        .ins(&Instruction::I32Const(tag))
        .ins(&Instruction::I32Store8(MemArg {
            offset: l.fields[0] as u64,
            align: 0,
            memory_index: 0,
        }))
        .ins(&Instruction::LocalGet(dest));
    match word {
        Some(w) => {
            b.ins(&Instruction::LocalGet(w)).ins(&Instruction::I64ExtendI32U);
        }
        None => {
            b.ins(&Instruction::I64Const(0));
        }
    }
    b.ins(&Instruction::I64Store(at(l.fields[1])))
        .ins(&Instruction::LocalGet(dest))
        .ins(&Instruction::I64Const(0))
        .ins(&Instruction::I64Store(at(l.fields[2])));
}

// (`float_str` — 511 lines — stood here: `%f`'s six decimal places computed
// exactly, in base-10^6 limbs, because wasm has no `printf` to defer to. It was
// the one runtime function in this backend that was an algorithm rather than a
// loop, and RFC-0081 M2 replaced it with a call to `std/num`'s `f64Str` — the
// same expansion, written once in Vyrn, where the interpreter's `{:.6}` stays as
// the oracle a differential test compares it against. The measurement that
// bought it: 330 ns hand-written here against 721 ns compiled, and no difference
// a program could observe.)

/// `print(n: Int64)`: the decimal digits and a newline, straight to fd 1.
///
/// Written as wasm rather than deferred to the shim because `print` is
/// `printf("%lld\n")` today and varargs are M3 — and because it is the one place
/// this backend touches the shadow stack without an aggregate being involved.
/// Digits go in backwards from the end of the frame's buffer, which is why the
/// pointer handed to `write_all` is computed rather than fixed.
///
/// Unsigned division throughout, so `Int64.min` — whose negation is itself —
/// prints its digits rather than wrapping to nothing.
fn print_i64(m: &mut Module, write_all: u32) -> u32 {
    // A 32-byte buffer at the bottom of the frame; 20 digits and a sign is the
    // widest an i64 gets.
    const BUF_END: u32 = 32;
    let (v, sgn, p, neg) = (0, 1, 3, 4); // params 0 and 1, base is 2, then our two
    m.func(&[ValType::I64, ValType::I32], &[], &[ValType::I32, ValType::I32], BUF_END, |b| {
        // neg = signed && v < 0; v = |v| as unsigned. An unsigned type prints its
        // magnitude — the interpreter's `*v as u64` — so the caller says which,
        // rather than there being a second digit loop to keep in step with this
        // one.
        b.ins(&Instruction::LocalGet(v))
            .ins(&Instruction::I64Const(0))
            .ins(&Instruction::I64LtS)
            .ins(&Instruction::LocalGet(sgn))
            .ins(&Instruction::I32And)
            .ins(&Instruction::LocalTee(neg))
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I64Const(0))
            .ins(&Instruction::LocalGet(v))
            .ins(&Instruction::I64Sub)
            .ins(&Instruction::LocalSet(v))
            .ins(&Instruction::End);
        // p = base + BUF_END - 1; *p = the newline
        b.slot(BUF_END - 1)
            .ins(&Instruction::LocalTee(p))
            .ins(&Instruction::I32Const(10)) // newline
            .ins(&Instruction::I32Store8(byte()));
        // do { *--p = '0' + v % 10; v /= 10 } while (v)
        b.ins(&Instruction::Loop(BlockType::Empty))
            .ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::LocalTee(p))
            .ins(&Instruction::LocalGet(v))
            .ins(&Instruction::I64Const(10))
            .ins(&Instruction::I64RemU)
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::I32Const(b'0' as i32))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Store8(byte()))
            .ins(&Instruction::LocalGet(v))
            .ins(&Instruction::I64Const(10))
            .ins(&Instruction::I64DivU)
            .ins(&Instruction::LocalTee(v))
            .ins(&Instruction::I64Eqz)
            .ins(&Instruction::I32Eqz)
            .ins(&Instruction::BrIf(0))
            .ins(&Instruction::End);
        // if (neg) *--p = '-'
        b.ins(&Instruction::LocalGet(neg))
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::LocalTee(p))
            .ins(&Instruction::I32Const(b'-' as i32))
            .ins(&Instruction::I32Store8(byte()))
            .ins(&Instruction::End);
        // write_all(1, p, (base + BUF_END) - p)
        b.ins(&Instruction::I32Const(1));
        b.ins(&Instruction::LocalGet(p));
        b.slot(BUF_END).ins(&Instruction::LocalGet(p)).ins(&Instruction::I32Sub);
        b.ins(&Instruction::Call(write_all));
    })
}

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
    Type::Result(Box::new(Type::Array(Box::new(Type::Str))), Box::new(Type::Str))
}

fn io_builtin_ty(name: &str, argc: usize) -> Option<Type> {
    let str_err = |ok| Type::Result(Box::new(ok), Box::new(Type::Str));
    Some(match (name, argc) {
        ("args", 0) => Type::Array(Box::new(Type::Str)),
        ("readLine", 0) => Type::Option(Box::new(Type::Str)),
        ("readFile", 1) => str_err(Type::Str),
        ("readFileBytes", 1) => {
            str_err(Type::Array(Box::new(Type::IntN { bits: 8, signed: false })))
        }
        ("writeFile", 2) | ("renameFile", 2) => str_err(Type::Bool),
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

#[cfg(test)]
mod tests {
    use super::*;

    /// The gap message is the ladder's grouping key, so its shape is pinned:
    /// one construct, one line, no site-specific text in between.
    #[test]
    fn a_gap_names_the_construct_and_the_line() {
        let e: Result<(), String> = unsupported("`while`", 12);
        assert_eq!(e.unwrap_err(), "direct backend: no lowering for `while` at line 12");
    }

    /// The runtime table's invariant, checked rather than maintained by care:
    /// one index per helper, all distinct, dense from `base`, and `count` equal to
    /// however many were handed out. `runtime` asserts the other half — that the
    /// bodies arrive at the indices declared here.
    #[test]
    fn every_runtime_helper_gets_its_own_index() {
        let base = 7; // any offset; the imports are not always the same count
        // Both shapes: the generator path hands out one more (`list_dir`), and the
        // invariant is that adding it neither duplicates a name nor leaves a hole.
        for gen_host in [false, true] {
        let (rt, table) = Rt::slots(base, gen_host);
        assert_eq!(table.len() as u32, rt.count, "count is the number of slots handed out");
        let names: std::collections::HashSet<&str> = table.iter().map(|(n, _)| *n).collect();
        assert_eq!(names.len(), table.len(), "a name is registered twice");
        let idx: Vec<u32> = table.iter().map(|(_, i)| *i).collect();
        assert_eq!(idx, (base..base + rt.count).collect::<Vec<_>>(), "indices are dense and distinct");
        }
    }

    fn cx() -> Cx {
        Cx {
            types: HashMap::new(),
            sigs: HashMap::new(),
            gen: None,
            variants: HashMap::new(),
            generics: HashMap::new(),
            higher_order: HashMap::new(),
            protocol_methods: HashMap::new(),
            subst: HashMap::new(),
            mono: RefCell::new(Mono::default()),
            fnvals: RefCell::new(Vec::new()),
            dispatch: RefCell::new(Dispatch::default()),
            globals: HashMap::new(),
            externs: HashMap::new(),
            droppable: HashMap::new(),
            // RFC-0008's defaults, which are `Program`'s: nothing here logs.
            log_level: DEFAULT_LOG_LEVEL,
            log_sink: LogSink::Stderr,
            log_fd: None,
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
        let r = c.repr(&Type::Record(vec![
            Field { name: "a".into(), ty: Type::Bool },
            Field { name: "b".into(), ty: Type::Int },
        ]), 0);
        // `{ i1, i64 }` — the byte, then seven of hole. M0's clang test is why
        // this number is not a guess.
        assert_eq!(r.unwrap(), Repr::Agg(Layout { size: 16, align: 8, fields: vec![0, 8] }));
        assert_eq!(c.repr(&Type::Option(Box::new(Type::Int)), 0).unwrap().val(), Some(ValType::I32));
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
        assert_eq!(c.ll(&Type::Option(Box::new(t))), "{ i1, i64, i64 }");
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
            let p = vyrn_frontend::check(src).expect(what);
            let bytes = compile(&p).expect(what);
            assert!(
                bytes.windows(msg.len()).any(|w| w == msg.as_bytes()),
                "{what}: no `where` check was emitted"
            );
        }
        // And the negative, so the assertion above is about a check being emitted
        // and not about the word "Age" reaching the module some other way: the
        // same declaration, with nothing flowing into it.
        let unreached = "type Age = Int64 where value >= 18 \
                         fn main() -> Int64 { return 20 }";
        let p = vyrn_frontend::check(unreached).unwrap();
        let bytes = compile(&p).unwrap();
        assert!(
            !bytes.windows(msg.len()).any(|w| w == msg.as_bytes()),
            "an unreached refinement emitted a check"
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
        let p = vyrn_frontend::check(src).unwrap();
        assert!(compile(&p).is_ok());
    }
}
