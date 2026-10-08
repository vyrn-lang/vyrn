//! Lowers Vyrn straight to wasm, with no LLVM in between.
//!
//! A construct with no lowering is [`unsupported`]: a named construct and a source line, never a
//! fallback to another backend, so the blocker list reports this backend alone.
//!
//! Four constraints shape the emitter:
//!
//! - Bodies are walked from the named core (`vyrn_lower::core`); a body the core did not state
//!   is refused. Structured control flow maps `if`/`while` onto
//!   `if`/`block`+`loop`, and `break`/`continue` onto `br <depth>`. Every construct that opens a
//!   wasm block pushes one onto [`Fn_::depth`], because a `return` is a `br` past all of them.
//! - A body never emits `return`: it would skip the shadow-stack epilogue `wasm::Module::add`
//!   emits and leak the frame. A body is wrapped in one `block`, and `return` is a `br` to it.
//! - Scalars live in wasm locals, aggregates in frame slots. On the operand stack an aggregate is
//!   always the `i32` address of a slot: a parameter is an address the callee copies out of, a
//!   return is a hidden leading address the callee writes through, a field access is an offset.
//! - An aggregate `if`-expression has no value to leave on the stack, so its slot is allocated
//!   before the branch and each arm copies into it.

use std::borrow::Cow;
use std::cell::RefCell;
use std::collections::HashMap;
use std::rc::Rc;

use vyrn_frontend::ast::*;
use vyrn_frontend::core::check::{Check, Guard, Raises, Verdict};
/// The core's statements and values. `Body` stays qualified at each use,
/// because this file defines its own `Body`.
use vyrn_frontend::core::{
    Arg, Arm, Callee, Ctor, Lit, Name, NameInfo, Op, Rhs, St, Target, Test, Val,
};
use vyrn_frontend::gen::GenError;
use vyrn_frontend::own::DropKind;
/// Shared with `vyrn-lower` and the other engines, so exits compare without a translation.
use vyrn_frontend::own::Exit as ExitKind;
use vyrn_frontend::types as ftypes;
use vyrn_frontend::types::INT32;
use vyrn_lower::core::Spec;

use crate::layout::{self, Layout, Shape};
use crate::wasm::{
    self, mem_arg, BlockType, Frame, Instruction, MemArg, Module, ValType, HEAP_BASE, MEMORY_COPY,
    SITE,
};

/// Refuses a construct this backend cannot lower, naming it and its line. One message shape for
/// every gap, because the ladder groups blockers by the text after the colon.
fn unsupported<T>(what: &str, line: usize) -> Result<T, String> {
    Err(gap(what, line))
}

fn gap(what: &str, line: usize) -> String {
    format!("direct backend: no lowering for {what} at line {line}")
}

/// A size whose lowering does not fit the `u32` every offset, `malloc` argument and copy length
/// here uses.
fn too_big(what: &str, bytes: u64, line: usize) -> String {
    format!(
        "direct backend: {what} needs {bytes} bytes at line {line}, past the {} one value may \
         occupy; a fixed array this big belongs on the heap as `Array<T>`",
        vyrn_frontend::trap::LENGTH_LIMIT
    )
}

/// The function index of each [`crate::WASI_IMPORTS`] call, in the table's order.
///
/// All are declared before the first body, because imports share the function index space and
/// nothing knows which a program reaches until the bodies are walked. A pre-scan would be a
/// second traversal that must agree with lowering. [`wasm::Module::sweep`] drops the unused ones
/// afterwards, so a program pays only for what it calls.
#[derive(Clone, Copy, Default)]
struct Wasi([u32; crate::WASI_IMPORTS.len()]);

impl Wasi {
    fn declare(m: &mut Module) -> Wasi {
        let mut w = Wasi::default();
        for (at, (name, params, results)) in w.0.iter_mut().zip(crate::WASI_IMPORTS) {
            *at = m.import("wasi_snapshot_preview1", name, params, results);
        }
        w
    }

    /// Returns the index and signature of the call `name` spells, in the table's snake_case or
    /// in `std/mem`'s lowerCamelCase.
    fn find(&self, name: &str) -> Option<(u32, &'static [ValType], &'static [ValType])> {
        let snake = crate::wasi_snake(name);
        let at = crate::WASI_IMPORTS.iter().position(|(n, ..)| *n == snake)?;
        let (_, params, results) = crate::WASI_IMPORTS[at];
        Some((self.0[at], params, results))
    }

    /// The index of a call the emitter makes by name. A name that leaves the table panics on
    /// every compile.
    fn at(&self, name: &str) -> u32 {
        self.find(name).expect("a `WASI_IMPORTS` name").0
    }
}

/// The `vyrn_gen` imports a generator module makes.
///
/// A generator runs inside the compiler's own wasmtime, not a WASI host. What it reaches for
/// stays in the host: the loader's resolver (which serves unsaved editor buffers), the piece
/// arena, the lexer, the linker. The guest holds handles and pulls atoms.
///
/// Declared from [`crate::CODE_IMPORTS`], so no signature on this boundary is written twice.
/// `fetch` copies a stash into a buffer the guest allocated, because the host must not allocate
/// inside guest memory.
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
    for (name, params, results) in crate::CODE_IMPORTS {
        at.insert(name, m.import("vyrn_gen", name, params, results));
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

/// An `extern fn`: a host import from the `vyrn` namespace, or one of the
/// host-boundary names (`trap::HOST_EXTERNS`).
#[derive(Clone)]
struct Ext {
    /// The import's function index, or `None` for a host-boundary name, which the emitted runtime
    /// serves from WASI.
    index: Option<u32>,
    params: Vec<Type>,
    ret: Type,
}

/// The wasm signature one `extern fn` crosses as, read off [`crate::extern_abi`] so the ABI is
/// stated once.
///
/// A `String` crosses as a `(ptr, len)` pair. An export's `String` parameter is one pointer
/// instead, because the JS caller can allocate inside the module (`web/README.md`).
fn extern_abi_sig(f: &Function) -> (Vec<ValType>, Vec<ValType>) {
    let mut params = Vec::new();
    for p in &f.params {
        if matches!(p.ty, Type::Str) {
            params.push(ValType::I32);
            params.push(ValType::I64);
        } else {
            params.extend(crate::extern_abi(&p.ty));
        }
    }
    (params, crate::extern_abi(&f.ret).into_iter().collect())
}

/// Compiles a whole program to a self-contained `wasm32-wasi` module.
///
/// The module defines its own memory, heap and runtime, and imports only the WASI calls it makes
/// and the `extern`s it declares, so `vyrn build --target wasm` needs no clang and no sysroot.
///
/// # Errors
///
/// Refuses a program whose expansions are unshared
/// ([`vyrn_frontend::project::Expansions::shared`]): in it a projection or a `schemaOf` has no row
/// in the core. `world` is `program`'s: the load's ([`vyrn_lower::load_warned`]) or a new one
/// ([`vyrn_lower::analyze`]).
pub fn compile(
    program: &Program,
    world: std::sync::Arc<vyrn_lower::World>,
) -> Result<Vec<u8>, String> {
    if !program.expansions.is_shared() {
        return Err("a compile needs the load's shared projection expansions".to_string());
    }
    compile_inner(program, world)
}

/// Returns the module [`compile`] emits, as WAT (`vyrn emit-wat`), so a test can pin its shape.
/// Printing stays off `compile`, because `vyrn build` writes bytes only.
pub fn wat(program: &Program, world: std::sync::Arc<vyrn_lower::World>) -> Result<String, String> {
    let bytes = compile(program, world)?;
    wasmprinter::print_bytes(&bytes).map_err(|e| e.to_string())
}

/// Returns the functions with no run-time lowering because they reach a generator.
///
/// A `gen fn` runs at generation time, so neither it nor any function that calls one,
/// transitively, has a run-time lowering. [`compile`] skips them, because failing the build over
/// a function nothing calls is the wrong answer. A call to a skipped name refuses at its site.
///
/// Public because the driver asks whether a `test` body must compile as generation, and the
/// answer must be this one, not a second walk. `world` is [`compile`]'s: its call relation holds
/// no edge for a call through a binding, so such a call reaches no generator.
pub fn gen_reach(
    program: &Program,
    world: &vyrn_lower::World,
) -> std::collections::HashSet<String> {
    let id = |f: &vyrn_frontend::ast::Function| world.fn_id(&f.name);
    let gens = program.functions.iter().filter(|f| f.is_gen);
    let reach = world.callers_closure(gens.filter_map(|f| id(f)));
    (program.functions.iter())
        .filter(|f| id(f).is_some_and(|i| reach[i.index()]))
        .map(|f| f.name.clone())
        .collect()
}

/// Compiles `program`, a generator host (`vyrn_genwasm::prepare` sets [`Host::gen`]), with the
/// `vyrn_gen` imports and the lowerings that need them (`listDir`, and `Code` as an opaque `i64`
/// handle).
///
/// Takes a program of either table: a compile's generator program carries a shared one, the
/// LSP's is unshared on purpose, and the generation engine declines a refusal to the interpreter.
///
/// # Errors
///
/// [`GenError::Refused`] when the typed judgment refused the program, whether or not it would
/// run, so no engine caches a module for it; [`GenError::Failed`] when a body has no lowering.
pub fn compile_gen_host(program: &Program) -> Result<Vec<u8>, GenError> {
    let world = vyrn_lower::analyze(program);
    // The typed judgment's refusals, else the must-use rows: the kernel's
    // other refusals of a generator's program are not the reader's.
    let refused = match world.typed_diagnostics() {
        [] => world.owed_diagnostics(),
        typed => typed.to_vec(),
    };
    match refused.is_empty() {
        true => compile_inner(program, world).map_err(GenError::Failed),
        false => Err(GenError::Refused(refused)),
    }
}

fn compile_inner(
    program: &Program,
    world: std::sync::Arc<vyrn_lower::World>,
) -> Result<Vec<u8>, String> {
    let _cg = vyrn_frontend::prof::phase("codegen");
    let mut m = Module::new();
    // Imports first — they share the function index space with definitions, so
    // `wasm::Module` panics if one arrives late.
    let wasi = Wasi::declare(&mut m);
    let gen = program.host.gen.then(|| gen_imports(&mut m));
    let oracle = (matches!(vyrn_lower::check::mode(), vyrn_lower::check::Mode::Count(_))
        && gen.is_none())
    .then(|| Oracle {
        hit: m.import("vyrn_check", "hit", &[ValType::I32], &[]),
        fail: m.import("vyrn_check", "fail", &[ValType::I32], &[]),
        labels: RefCell::new(Vec::new()),
        ids: RefCell::new(HashMap::new()),
    });
    // Every `extern fn` is one import from the `vyrn` namespace, which `web/wasi-min.js` fills
    // from the page's hooks. This is not a pre-scan: an `extern fn` is its import, one for one,
    // and `Module::sweep` drops the ones a program never calls.
    let mut externs: HashMap<String, Ext> = HashMap::new();
    for f in program.functions.iter().filter(|f| f.is_extern) {
        // The host-boundary names are not host imports on any target; see their call site.
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

    // The runtime functions written in Vyrn are reserved before the hand-emitted runtime, which
    // calls them.
    let mut vyrn_rt = VyrnRt::reserve(&mut m);
    let rt = runtime(&mut m, &wasi, &vyrn_rt);

    let types: HashMap<String, TypeDecl> = program
        .type_decls
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect();
    // Three kinds of function define nothing and are skipped: lowering an unspecializable shell
    // would fail the build over a function nothing calls. The fourth kind is [`gen_reach`]'s.
    let gen_reach = gen_reach(program, &world);
    let mut generics: HashMap<String, &Function> = HashMap::new();
    let mut higher_order: HashMap<String, &Function> = HashMap::new();
    let mut user: Vec<&Function> = Vec::new();
    let mut skipped = std::collections::HashSet::new();
    let mut mem = HashMap::new();
    for f in &program.functions {
        // An `extern` is an import (declared above); a `gen fn` runs
        // only in the compiler's own interpreter and may use builtins with no
        // lowering at all.
        if f.is_extern || f.is_gen {
            continue;
        }
        // An entry point (`main`, an export, a lifted test or serve door) is never dropped.
        // Lowering it refuses at the call into the closure, naming that call.
        let entry = f.name == "main" || f.exported || f.is_export_extern;
        if gen_reach.contains(&f.name) && !entry {
            skipped.insert(f.name.clone());
            continue;
        }
        // A `std/mem` declaration has no body here; [`mem_ins`] lowers each call.
        if let Some(prim) = f.name.strip_prefix(vyrn_frontend::loader::MEM_PREFIX) {
            mem.insert(prim, f);
            continue;
        }
        // A function with a `fn`-typed parameter exists only as specializations; the
        // shell has no first-order definition.
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

    // The leak instrument; a generator host never carries it.
    let audited = vyrn_frontend::loader::audit_build(program.host.gen);
    let mut cx = Cx {
        types,
        lambdas: vyrn_frontend::ast::lambdas(program),
        layouts: RefCell::default(),
        reprs: RefCell::default(),
        oracle,
        sigs: HashMap::new(),
        rt,
        gen,
        generics,
        higher_order,
        skipped,
        mem,
        subst: HashMap::new(),
        mono: RefCell::new(Mono::default()),
        fnvals: RefCell::new(Vec::new()),
        fnval_copy: 0,
        fnval_free: 0,
        dispatch: RefCell::new(Dispatch::default()),
        shapes: RefCell::new(Shapes::default()),
        globals: HashMap::new(),
        args_in_place: false,
        gappend: HashMap::new(),
        externs,
        world,
        log_level: program.log_level,
        log_sink: program.log_sink.clone(),
        // Reserved only for a file sink, so a console-sink module reserves nothing.
        log_fd: matches!(program.log_sink, LogSink::File(_)).then(|| m.reserve(4, 4)),
        audit: audited,
        profile: vyrn_frontend::loader::profile_build(program.host.gen).then(Profile::default),
    };
    if cx.profile.is_some() {
        m.profile();
    }

    // The first `body_of` of a function decides its checks, about a fifth of this compile. The
    // answer is memoized and the same on any thread, so the signatures below find it ready.
    let ck = vyrn_frontend::prof::phase("codegen: checks");
    vyrn_frontend::par::in_parallel(
        &user,
        |f| f.body.stmts.len(),
        || (),
        |(), f| {
            cx.world.body_of(&f.name);
        },
    );
    drop(ck);
    let sg = vyrn_frontend::prof::phase("codegen: signatures");
    // Every function is indexed before any body exists, so a call can name a callee not yet
    // emitted (recursion, forward references). The encoder hands out the index and the body is
    // filled whenever it exists, so emission order does not decide numbering.
    for f in user.iter() {
        let s = cx.signature(f, &f.name)?;
        let (wp, wr) = cx.wasm_sig(&s, f.line)?;
        let index = match vyrn_rt.take(&f.name, &wp, &wr, f.line)? {
            Some(reserved) => reserved,
            None => m.reserve_func(&wp, &wr),
        };
        cx.sigs.insert(f.name.clone(), Sig { index, ..s });
    }
    vyrn_rt.check()?;
    drop(sg);

    // Module state: each top-level `let` gets one fixed zeroed address, and every
    // access resolves to it through `Fn_::lookup`'s fallback. Reserved before any body, which may
    // read one, and after the signatures, because an unannotated initializer may be a call.
    for g in &program.globals {
        let ty = match &g.ty {
            Some(t) => t.clone(),
            None => match cx.world.ownership.record.node_types.get(&g.init.id()) {
                Some(t) => cx.sub(t).into_owned(),
                None => {
                    return unsupported(
                        "a module-state initializer the checker did not type",
                        g.line,
                    )
                }
            },
        };
        let l = cx.layout(&ty, g.line)?;
        if cx.repr(&ty, g.line)? == Repr::Unit {
            return unsupported("module state of Unit", g.line);
        }
        cx.globals.insert(
            g.name.clone(),
            (Place::Static(m.reserve(l.size, l.align)), ty),
        );
    }
    cx.args_in_place = cx.globals.values().all(|(_, ty)| {
        cx.resolve(ty) == Type::Str
            || !vyrn_frontend::declared::owns_heap(&cx.sub(ty), &cx.types)
                && matches!(cx.repr(ty, 0), Ok(Repr::Scalar(_)))
    });

    // One ownership word per module-state accumulator, in static memory because the helper writes
    // it back and wasm has no pass-by-reference. Reserved zeroed; the initializer sets it.
    //
    // A reservation moves every reservation after it, so `global_append_candidates` returns an
    // ordered set.
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
    // The teardown drops every module-state binding after `main`, so the
    // instrument reports the program's residue. Reserved only in an audited build, so an ordinary
    // module's indices do not move.
    let teardown_index = (cx.audit && has_globals).then(|| m.reserve_func(&[], &[]));
    // The derived `fn`-value copy and release, reserved like a dispatcher: their switch covers
    // every construction in the module, so their bodies wait for the last body, while a copy site
    // mid-walk must be able to call them.
    cx.fnval_copy = m.reserve_func(&[ValType::I64, ValType::I32], &[ValType::I32]);
    cx.fnval_free = m.reserve_func(&[ValType::I64, ValType::I32], &[]);

    let bd = vyrn_frontend::prof::phase("codegen: bodies");
    for f in &user {
        let sig = cx.sigs[&f.name].clone();
        crate::observe::note_inst(&f.name, &[]);
        lower_fn(&mut m, f, &sig, &cx, HashMap::new())?;
    }
    drop(bd);
    let dr = vyrn_frontend::prof::phase("codegen: drain");

    // The initializers, in declaration order, which the loader has made dependencies-first. One
    // function, called once from `_start`. Filled even with no module state, because a
    // reservation nobody fills is not a valid module.
    let init = lower_globals_init(&mut m, program, &cx)?;
    m.fill(init_index, init)?;
    // Before the drain: a global of a declared generic release reaches only the teardown, and its
    // instance must be on a worklist the drain still reads (`vyrn-lower`'s `<teardown>` root).
    if let Some(ti) = teardown_index {
        let t = lower_globals_teardown(&mut m, program, &cx)?;
        m.fill(ti, t)?;
    }

    // Drain the instances the bodies discovered, then the shapes, then the dispatchers, until
    // none is left. One body may discover more of any kind, so each turn rereads the lists.
    //
    // A frame-limit refusal waits for the drain to end. In polymorphic recursion the frames double
    // every instance, so the frame limit trips first, but the instantiation refusal is the one
    // `vyrn check` gives. The frame refusal returns only when no instantiation refusal came.
    let mut deferred: Option<String> = None;
    let mut derived = false;
    loop {
        let p = {
            let mono = cx.mono.borrow();
            mono.insts.get(mono.done).cloned()
        };
        if let Some(p) = p {
            // The shared instantiation cap: polymorphic recursion leaves an instance every turn.
            crate::check_inst_depth(&p.f.name, p.subst.values(), p.f.line, &cx.types)?;
            // A `Key::Lambda` has no program name to record. The other kinds are one callee at one
            // list of type arguments, which `vyrn-lower`'s worklist keys on.
            match &p.key {
                Key::Generic(n, args) | Key::Ho(n, args, _) => crate::observe::note_inst(n, args),
                Key::Lambda(..) => {}
            }
            cx.subst = p.subst.clone();
            let body = lower_body(
                &mut m,
                &p.f,
                &p.core_key,
                p.body,
                &p.sig,
                &cx,
                p.binds.clone(),
            );
            cx.subst = HashMap::new();
            cx.mono.borrow_mut().done += 1;
            match body {
                Ok(body) => {
                    fill_named(&mut m, p.sig.index, body, &p.f.name)?;
                }
                Err(e) if e.contains(crate::FRAME_LIMIT_NEEDLE) => {
                    deferred.get_or_insert(e);
                }
                Err(e) => return Err(e),
            }
            continue;
        }
        // One release or copy body per type. Drained after the instances and before the
        // dispatchers: a shape body reaches a declared `release`, which may be generic and queue
        // an instance. The substitution is empty because each shape's type was substituted where
        // it was asked for.
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
        // A dispatcher switches over every construction of its signature in the module, so its
        // body is complete only after the last body is walked. See [`Fn_::dispatcher`].
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
        // The registry closes once every body is walked, so the two derived walks over it are
        // written here. They stay inside the loop because each releases or copies a capture type,
        // which queues a shape body.
        if derived {
            break;
        }
        derived = true;
        let fncopy = lower_fnval_copy(&mut m, &cx)?;
        m.fill(cx.fnval_copy, fncopy)?;
        let fnfree = lower_fnval_free(&mut m, &cx)?;
        m.fill(cx.fnval_free, fnfree)?;
    }
    drop(dr);
    let _fin = vyrn_frontend::prof::phase("codegen: finish");
    if let Some(e) = deferred {
        return Err(e);
    }

    // `_start`, WASI's entry point. The exit code is `main & 255`, matching `vyrn run` and the
    // native binary, which give the OS one byte.
    let main = cx
        .sigs
        .get("main")
        .ok_or_else(|| "direct backend: program has no `main`".to_string())?;
    if main.ret != Repr::Scalar(ValType::I64) {
        return unsupported("a `main` that does not return Int64", 0);
    }
    let main = main.index;
    // The log `file(..)` sink: one descriptor opened once and held for the run. `path_open`
    // with CREAT|TRUNC is `fopen(path, "w")`.
    //
    // On failure the slot holds -1 and `write_all`'s errno test swallows every write, as the
    // interpreter does. A browser has no preopens, so its file sink is silent rather than trapping.
    let log_open = cx.log_fd.map(|at| {
        let LogSink::File(path) = &cx.log_sink else {
            unreachable!("log_fd implies a file sink")
        };
        (at, cx.rt.intern(&mut m, path), cx.rt.open_at)
    });
    // The leak instrument, in an audited build: the four wordings are interned here
    // and passed to `auditInit`. Nothing else reaches the two functions, so an unaudited build
    // sweeps them.
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
    // The counter table: 16 bytes of totals, then a 32-byte row per site, site 0 first. Reserved
    // after every body, which is the first moment the sites are all known.
    let table = cx.profile.as_ref().map_or(0, |p| {
        m.reserve(16 + 32 * (p.sites.borrow().len() as u32 + 1), 8)
    });
    let start = m.func(&[], &[], &[], 0, |b| {
        // First, so the instrument marks every block: `auditInit` allocates its own state and
        // then arms.
        if let Some((init, _, words)) = audit {
            for w in words {
                b.ins(&Instruction::I32Const(w as i32));
            }
            b.ins(&Instruction::I32Const(table as i32));
            b.ins(&Instruction::Call(init));
        }
        // Before the initializers, because a top-level `let` may log.
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
        // Flush what `print` buffered: a zero-length write to a descriptor other than fd 1
        // flushes and writes nothing. Both trap paths write to fd 2 first, so need no flush.
        b.ins(&Instruction::I32Const(2))
            .ins(&Instruction::I32Const(0))
            .ins(&Instruction::I32Const(0))
            .ins(&Instruction::Call(cx.rt.write_all))
            .ins(&Instruction::Drop);
        if let Some((at, ..)) = log_open {
            b.ins(&Instruction::I32Const(at as i32))
                .ins(&Instruction::I32Load(word()))
                .ins(&Instruction::Call(wasi.at("fd_close")))
                .ins(&Instruction::Drop);
        }
        // After the flush, so a leaking program's output precedes the report, and before the
        // exit, because the report replaces the exit code with 135.
        if let Some(ti) = teardown_index {
            b.ins(&Instruction::Call(ti));
        }
        if let Some((_, exit, _)) = audit {
            b.ins(&Instruction::Call(exit));
        }
        b.ins(&Instruction::I64Const(255))
            .ins(&Instruction::I64And)
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::Call(wasi.at("proc_exit")));
    });
    m.export("_start", start);
    // An `export extern fn`, under its own name. An export is also a sweep root.
    for f in &user {
        if f.is_export_extern {
            m.export(&f.name, cx.sigs[&f.name].index);
        }
    }
    if user.iter().any(|f| f.is_export_extern) {
        m.export_entry_state(cx.rt.region_sp);
    }
    // A `String` argument to an export is a pointer into this module's memory, so the JS caller
    // must allocate in it first. The emitted `malloc` takes an `i64`, the BigInt `wasi-min.js`
    // passes, so it is exported as itself.
    //
    // The caller owns a `String` argument and a `String` result, and
    // across this boundary the caller is JS, so `__vyrn_free` goes out too. The two go out
    // together because the free list belongs to the allocator.
    if user.iter().any(|f| {
        f.is_export_extern
            && (f.params.iter().any(|p| matches!(p.ty, Type::Str)) || matches!(f.ret, Type::Str))
    }) {
        m.export("__vyrn_malloc", cx.rt.malloc);
        m.export("__vyrn_free", cx.rt.free);
    }
    // Keep only what the exports reach. Everything above emits eagerly, because nothing knows
    // what a program reaches until its bodies are walked. `Module::sweep_pool` does the same for
    // interned data.
    m.sweep();
    abi_section(&mut m, &user, program);
    if let Some(o) = &cx.oracle {
        m.custom("vyrn:checks", o.labels.borrow().join("\n").into_bytes());
    }
    if let Some(p) = &cx.profile {
        let mut rows = vec![table.to_string()];
        rows.extend(
            (p.sites.borrow().iter())
                .map(|(f, line, verb)| format!("{f}\t{line}\t{}", verb.word())),
        );
        m.custom("vyrn:sites", rows.join("\n").into_bytes());
    }
    m.finish()
}

/// How a value crosses the JS boundary, as declared: `String`, `Bool`, `Int32` and `UInt32` all
/// lower to a wasm `i32`.
///
/// The checker's [`extern_abi_type_ok`] closes the domain, so `"opaque"` is unreachable. It stays
/// written so a type that widens the domain reaches the shim as a loud refusal.
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

/// Writes the `vyrn:exports` custom section: the declared signature of every function
/// that crosses the JS boundary, in both directions.
///
/// The wasm ABI loses what a host needs: `String`, `Bool`, `Int32` and `UInt32` all arrive as
/// `i32`, and a `String` import's two slots look like an `(Int32, Int64)` pair.
///
/// Payload, version 2:
///
/// ```text
/// u8            version = 2
/// uleb          export count
///   per entry:  name:str  ret:kind  uleb param count  param:kind …
/// uleb          import count  (the `vyrn.*` namespace)
///   per entry:  name:str  ret:kind  uleb param count  param:kind …
/// ```
///
/// `str` is a uleb length then UTF-8 bytes; `kind` is a `str` from [`abi_kind`]'s closed set. A
/// module with nothing on the boundary carries no section.
///
/// The section lists every declaration, including ones `Module::sweep` dropped: the shim learns
/// which functions exist from the platform, and reads this only for their types.
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
    // The three host-boundary names are lowered in place on every target,
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

/// Which key family a `Map` runs on: `String` pointers, `Int64` values, or packed
/// user keys of a fixed stride.
#[derive(Clone, Copy, PartialEq, Eq)]
enum MapKey {
    Str,
    I64,
    Pack(u32),
}

impl MapKey {
    /// The `(kind, klen)` pair `std/runtime`'s map body takes: 0 for a `String` column, 1 for an
    /// `Int64` column, 2 for a packed key of `klen` bytes. Kind 3 is `tallyBytes`'s alone.
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
    Agg(Rc<Layout>),
}

impl Repr {
    /// The wasm type this crosses a call as; an aggregate crosses as its address.
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
    /// Which parameters are `modify` and so cross as the address of the caller's binding. The
    /// type cannot say: `modify Counter` and `read Counter` are one type and two ABIs.
    modify: Vec<bool>,
    ret: Repr,
    ret_ty: Type,
    /// The `consume` parameter the result is left in
    /// ([`vyrn_lower::core::returned_param`]): the callee takes its storage from the caller and
    /// writes no separate result, so the call has no out-pointer and the caller moves the
    /// argument into the call's destination first.
    in_place: Option<usize>,
}

/// Identifies a body discovered during emission, so a second site reuses its function index.
///
/// Keyed on the type arguments and targets themselves, not a mangled name: `mangle_name` is not
/// injective (every record mangles as `Rec`).
#[derive(Clone, PartialEq)]
enum Key {
    /// A generic instantiation: the callee and its type arguments.
    Generic(String, Vec<Type>),
    /// A specialization over `fn`-typed parameters: the callee, its type arguments, and each `fn` parameter's
    /// target. A different lambda makes a different function.
    Ho(String, Vec<Type>, Vec<FnTarget>),
    /// A lifted lambda: the literal's node, its concrete shape (captures, parameters,
    /// return), and its substitution. One literal in a generic body lifts once per
    /// instantiation, even when the shape does not differ.
    Lambda(NodeId, Vec<Type>, Vec<(String, Type)>),
}

/// The statements a queued body walks. Each is the program's own AST, because a walk over a copy
/// asks about nodes no recorded type can reach.
#[derive(Clone, Copy)]
enum Body<'a> {
    /// A block the program holds: a generic instance's or specialization's callee, or a
    /// `|x| { .. }` literal's body.
    Block(&'a Block),
    /// A `|x| e` literal's expression. A block would need a copy of `e` for its `return`, so the
    /// core's rows state the body, and [`lower_body`] refuses one they do not carry.
    Value,
}

/// One body discovered while another was being emitted, with its promised function index.
#[derive(Clone)]
struct Pending<'a> {
    key: Key,
    /// The shell: the name, line and signature the body is lowered under. It carries the
    /// synthesized block for a [`Key::Lambda`] and no statements otherwise. An `Rc`, so a
    /// drain turn shares the shell instead of copying it.
    f: Rc<Function>,
    /// The statements to walk, borrowed from the checked program.
    body: Body<'a>,
    sig: Sig,
    /// The monomorphization the body is lowered under; empty for a lifted lambda
    /// outside any generic.
    subst: HashMap<String, Type>,
    /// The target each `fn`-typed parameter is bound to, by name.
    binds: HashMap<String, FnBinding>,
    /// The name the core built this body under ([`vyrn_lower::World::body_of`]).
    core_key: String,
}

/// The specialization worklist, fed from inside [`Fn_`] and drained by [`compile`].
///
/// A specialization is discovered at a call site, so only a body being lowered can feed it; a
/// pre-pass would be a second traversal that must agree with lowering. One queue holds all three
/// [`Key`] kinds, so "they feed each other" means appending to the list being read.
#[derive(Default)]
struct Mono<'a> {
    insts: Vec<Pending<'a>>,
    done: usize,
}

/// One source a stored `fn` value can come from: a lifted lambda, a named function,
/// or a `fn`-typed parameter inside a specialization.
///
/// A stored value is `{ i64 tag, i64 payload }`: the tag indexes this registry globally and the
/// payload is a heap block of captures, 0 when there are none. A call goes through the
/// signature's dispatcher, which switches on the tag and calls directly, so no function pointer
/// exists.
#[derive(Clone)]
struct FnVal {
    /// The normalized signature. Two spellings of one signature must land as one type, or a
    /// dispatcher misses a variant ([`crate::normalize_fn_sig`]).
    sig: Type,
    target: FnTarget,
}

/// The dispatchers, and how many are written.
///
/// Separate from [`Mono`]: a dispatcher is discovered from a signature called through, and its
/// body waits until every body is walked, since any construction adds a variant.
#[derive(Default)]
struct Dispatch {
    sigs: Vec<(Type, Sig)>,
    done: usize,
}

/// One release and one copy function per type, so a drop site emits a call.
/// Every step of the walk is a byte offset from [`crate::layout`], so the walk lives here.
#[derive(Default)]
struct Shapes {
    /// `(release rather than copy, substituted type, take holes)` -> function index. A linear
    /// scan: [`Type`] is `Eq` but not `Hash`, and a module has tens of these.
    known: Vec<(ShapeKey, u32)>,
    /// The bodies still to write, and how many are written. A body may reach a type nothing has
    /// released yet, so the driver rereads this every turn, as it does [`Mono`]'s.
    todo: Vec<(u32, ShapeKey, usize)>,
    done: usize,
}

/// Identifies one shape body. The holes are places a `consume` took: a walk that skips
/// them is a different function.
type ShapeKey = (bool, Type, Vec<String>);

/// What a `fn`-typed argument resolved to: the function a call through that parameter
/// goes to directly, and how many leading parameters are captures the outer call site supplies.
///
/// Captures come first, then the `fn` type's own parameters, so a call is
/// [`Fn_::emit_call_with`] with the captures prepended, and no second call path exists. A target
/// is a compile-time function index, so the backend needs no function table.
#[derive(Clone, PartialEq)]
struct FnTarget {
    sig: Sig,
    ncaps: usize,
}

/// A `fn`-typed parameter inside a specialization: its target, and the names of this instance's
/// leading capture parameters to forward. The captures are values fixed at the outer call site.
#[derive(Clone)]
struct FnBinding {
    target: FnTarget,
    cap_srcs: Vec<String>,
}

/// The oracle's side of a module: `vyrn_check.hit(id)` counts a row, `vyrn_check.fail(id)`
/// ends the run where a proved row would have trapped. Row `id` is line `id` of the custom
/// section `vyrn:checks`: body, line, ordinal, rule and verdict, tab-separated.
struct Oracle {
    hit: u32,
    fail: u32,
    labels: RefCell<Vec<String>>,
    ids: RefCell<HashMap<(String, usize, u32), i32>>,
}

/// The profile instrument's side of a module ([`vyrn_frontend::loader::profile_on`]): the sites
/// the emitter met, one per source function, line and verb, in the order it met them. Site `k`
/// is `sites[k - 1]`, line `k` of the custom section `vyrn:sites` after its first, and the
/// counter row at `table + 16 + 32 * k`; site 0 is "before any site".
#[derive(Default)]
struct Profile {
    sites: RefCell<Vec<(String, u32, vyrn_lower::insight::Verb)>>,
    ids: RefCell<HashMap<(String, u32, vyrn_lower::insight::Verb), u32>>,
}

struct Cx<'a> {
    types: HashMap<String, TypeDecl>,
    /// Every lambda literal the program holds, by node address and the function that holds it,
    /// so [`Fn_::lift_lambda`] queues the literal's own body instead of a clone.
    lambdas: HashMap<NodeId, (&'a str, &'a Expr)>,
    /// Every layout, computed once per substituted type.
    layouts: RefCell<HashMap<Type, Rc<Layout>>>,
    /// [`Cx::repr`]'s answers, by substituted type.
    reprs: RefCell<HashMap<Type, Repr>>,
    /// The check oracle's host imports and its row labels, under
    /// [`vyrn_lower::check::Mode::Count`]; `None` in every other build.
    oracle: Option<Oracle>,
    sigs: HashMap<String, Sig>,
    rt: Rt,
    /// The `vyrn_gen` host imports on the generator path. `None` in an ordinary build, where those
    /// builtins are refused by name.
    gen: Option<Gen>,
    /// Generic functions by name. They have no index and no body of their own —
    /// only specializations do — so a call to one is a discovery.
    generics: HashMap<String, &'a Function>,
    /// Functions with a `fn`-typed parameter. Like a generic, a call to one is a
    /// discovery.
    higher_order: HashMap<String, &'a Function>,
    /// The monomorphization whose body is being lowered; empty for an ordinary
    /// function.
    subst: HashMap<String, Type>,
    mono: RefCell<Mono<'a>>,
    /// The stored-closure variant registry, module-global so a tag means the same thing in
    /// every body that builds one.
    fnvals: RefCell<Vec<FnVal>>,
    /// The module's one derived copy over that registry: `(tag, block) -> block`. Filled after
    /// the drain loop, like a dispatcher, because capture layouts are complete only then.
    fnval_copy: u32,
    /// The module's one derived RELEASE over that registry: `(tag, block) -> ()`.
    /// The twin of [`Cx::fnval_copy`], reserved and filled beside it.
    fnval_free: u32,
    dispatch: RefCell<Dispatch>,
    /// One release and one copy per type, and the worklist of the bodies still
    /// to write — see [`Shapes`].
    shapes: RefCell<Shapes>,
    /// Module state: name -> its fixed address and declared type. Every body sees all
    /// of them; the checker forbids an initializer reading a later global.
    globals: HashMap<String, (Place, Type)>,
    /// Whether a `read` or `modify` aggregate parameter is the caller's storage, used in place.
    /// It is when no module state is an aggregate or owns heap other than a `String`'s bytes:
    /// then only the callee's own parameters name that storage during the call, and the
    /// checker refuses a `modify` argument that overlaps another.
    args_in_place: bool,
    /// Module-state `String` accumulators: name -> the address of its ownership word. Present only
    /// for a global [`vyrn_lower::append::global_append_candidates`] cleared, so `g = g + ...`
    /// grows in place. The local twin is [`Fn_::str_append`]; a global has no local, so its word
    /// is static.
    gappend: HashMap<String, u32>,
    /// `extern fn` declarations by name: the import's index and the signature the ABI
    /// is read off. A call is the only thing that crosses.
    externs: HashMap<String, Ext>,
    /// The functions [`gen_reach`] leaves out of the module. A call to one
    /// refuses at its own site, naming it.
    skipped: std::collections::HashSet<String>,
    /// The `std/mem` declarations by primitive name: each states the types [`mem_ins`] lowers.
    mem: HashMap<&'a str, &'a Function>,
    /// The program's analysis: the release steps placed per function, the `Owned` table they
    /// were decided with, the checker's record (a module-state initializer's type),
    /// and the core's bodies and facts, the only source of the releases this emitter emits.
    world: std::sync::Arc<vyrn_lower::World>,
    /// The log threshold, as an ordinal. Compile-time, so a disabled log site emits no write.
    log_level: usize,
    /// Where a log line goes. Compile-time-known, so the write names its
    /// descriptor directly rather than looking one up.
    log_sink: LogSink,
    /// The four bytes holding the file sink's descriptor. `None` for a console sink, which
    /// reserves nothing.
    log_fd: Option<u32>,
    /// The profile instrument's sites; `None` in every other build.
    profile: Option<Profile>,
    /// An audited build emits `std/runtime`'s `audit` calls and `_start`
    /// arms the instrument. Otherwise the calls are dropped and [`wasm::Module::sweep`] takes the
    /// bodies.
    audit: bool,
}

impl<'a> Cx<'a> {
    /// Whether the container's release at this `for` walks the buffer alone, as the core states
    /// ([`Facts::loop_buffer_only`]).
    fn loop_buffer_only(&self, node: NodeId) -> bool {
        self.world
            .facts
            .as_ref()
            .is_some_and(|f| f.loop_buffer_only.contains(&node))
    }

    /// Substitutes the monomorphization this lowering is inside.
    ///
    /// Every type query on this `Cx` goes through it, so a `Type::Param` never reaches `shape_of`,
    /// which lowers it to `Void` without an error. It substitutes into the type expression before
    /// any `App` expands, so `Box<T>` and `fn f<T>` both spelling `T` cannot be confused.
    fn sub<'t>(&self, ty: &'t Type) -> Cow<'t, Type> {
        if self.subst.is_empty() {
            Cow::Borrowed(ty)
        } else {
            Cow::Owned(ftypes::substitute(ty, &self.subst))
        }
    }

    /// The machine shape of `ty`.
    fn shape(&self, ty: &Type) -> Shape {
        crate::shape_of(&self.sub(ty), &self.types)
    }

    /// The layout of `ty`, or the refusal of a shape past 4 GB.
    fn layout(&self, ty: &Type, line: usize) -> Result<Rc<Layout>, String> {
        let ty = self.sub(ty);
        if let Some(l) = self.layouts.borrow().get(&*ty) {
            return Ok(l.clone());
        }
        let l = crate::shape_of(&ty, &self.types)
            .layout()
            .map_err(|e| format!("direct backend: layout of `{ty}` at line {line}: {e}"))?;
        let l = Rc::new(l);
        self.layouts.borrow_mut().insert(ty.into_owned(), l.clone());
        Ok(l)
    }

    fn resolve(&self, ty: &Type) -> Type {
        ftypes::resolve(&self.sub(ty), &self.types)
    }

    /// The slots a payload of type `ty` rides in.
    fn words(&self, ty: &Type) -> usize {
        crate::payload_words_of(&self.sub(ty), &self.types)
    }

    /// The aggregate member index the `i`th payload of a variant starts at
    /// (member 0 is the tag).
    fn payload_slot(&self, payload: &[Type], i: usize) -> usize {
        1 + payload[..i].iter().map(|p| self.words(p)).sum::<usize>()
    }

    /// The variants of the sum `ty`, in tag order ([`crate::sum_variants_of`]), so a release and
    /// a copy are one walk per sum, not per spelling.
    fn sum_vs(&self, ty: &Type) -> Option<Vec<EnumVariant>> {
        crate::sum_variants_of(&self.sub(ty), &self.types)
    }

    /// The leaf of a scalar `ty`.
    ///
    /// # Panics
    ///
    /// If `ty` is no scalar. Every caller holds a [`Repr::Scalar`] for `ty`.
    fn leaf(&self, ty: &Type) -> layout::Leaf {
        match self.shape(ty) {
            Shape::Leaf(l) => l,
            s => panic!("a load or store of `{ty}`, whose shape {s:?} is no scalar"),
        }
    }

    /// The load of a scalar `ty` at `off` bytes. It sign-extends as `ty` does, because `Int8`
    /// and `UInt8` share the leaf `I8`.
    fn load(&self, ty: &Type, off: u32) -> Instruction<'static> {
        let signed = Num::of(&self.resolve(ty)).is_some_and(|n| n.signed);
        self.leaf(ty).load(off, signed)
    }

    fn store(&self, ty: &Type) -> Instruction<'static> {
        self.leaf(ty).store()
    }

    /// Returns the signature of a body discovered during emission, reserving its function index
    /// if this is the first site to ask. `f` arrives substituted, with its type parameters
    /// cleared: the signature belongs to the specialization.
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
        let s = self.signature(&f, &core_key)?;
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

    /// Whether the function at `index` is a signature's dispatcher.
    fn is_dispatcher(&self, index: u32) -> bool {
        (self.dispatch.borrow().sigs.iter()).any(|(_, s)| s.index == index)
    }

    /// The parameter `name` of `f`'s declaration, which `p` stands for, or `p`
    /// itself. An instance's shell holds copies ([`instance_shell`],
    /// [`ho_shell`]), and the plan keys a `consume` parameter's release by the
    /// declaration's node.
    fn declared_param<'f>(&'f self, f: &Function, name: &str, p: &'f Param) -> &'f Param {
        (self.generics.get(&f.name))
            .or_else(|| self.higher_order.get(&f.name))
            .and_then(|d| d.params.iter().find(|q| q.name == name))
            .unwrap_or(p)
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

    /// A specialization over `fn`-typed parameters: [`Cx::enqueue`] of `f` with each `fn`
    /// parameter bound to its target, under the shell [`ho_shell`] states.
    fn specialize(
        &self,
        m: &mut Module,
        f: &'a Function,
        type_args: Vec<Type>,
        subst: HashMap<String, Type>,
        targets: Vec<FnTarget>,
    ) -> Result<Sig, String> {
        let (sf, binds) = ho_shell(self, f, &subst, &targets);
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

    /// A generic instantiation: [`Cx::enqueue`] with no `fn` parameters.
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

    /// Returns how `ty` lives in wasm. The answer depends on the substituted type alone, so an
    /// `Ok` is kept: the per-name screens ask the same few types thousands of times, and each ask
    /// walks the type twice ([`Cx::ty_gap`], [`Cx::shape`]).
    fn repr(&self, ty: &Type, line: usize) -> Result<Repr, String> {
        let key = self.sub(ty);
        if let Some(r) = self.reprs.borrow().get(&*key) {
            return Ok(r.clone());
        }
        if let Some(why) = self.ty_gap(ty, 0) {
            return unsupported(&why, line);
        }
        let r = match self.shape(ty) {
            Shape::Void => Repr::Unit,
            Shape::Leaf(l) => Repr::Scalar(l.val_type()),
            Shape::Struct(_) | Shape::Array(..) => Repr::Agg(self.layout(ty, line)?),
        };
        self.reprs.borrow_mut().insert(key.into_owned(), r.clone());
        Ok(r)
    }

    /// Why `ty` cannot be lowered, if it cannot.
    ///
    /// Without this, an unresolvable name lowers to `void`, to nothing at all. A validated type
    /// passes: [`Fn_::coerce`] checks the refinement at the flow.
    ///
    /// Depth-bounded because a record may hold a `Ref` to its own type.
    fn ty_gap(&self, ty: &Type, depth: usize) -> Option<String> {
        if depth > 6 {
            return None;
        }
        let ty = self.sub(ty);
        match &*ty {
            // Unreachable for a well-typed program, because [`Cx::sub`] runs first. Kept as a
            // refusal because `shape_of` gives `Void` for a parameter, and `Void` is no diagnostic.
            Type::Param(p) => return Some(format!("the unsolved type parameter `{p}`")),
            Type::Named(n) | Type::App(n, _) => match self.types.get(n) {
                Some(_) => {}
                // `Code` and `Token` are builtins `resolve` knows without a decl.
                None if n == "Code" || n == "Token" => {}
                None => return Some(format!("the unknown type `{n}`")),
            },
            _ => {}
        }
        match self.resolve(&ty) {
            Type::Record(fs) => fs.iter().find_map(|f| self.ty_gap(&f.ty, depth + 1)),
            Type::Array(i) | Type::ArrayN(i, _) => self.ty_gap(&i, depth + 1),
            Type::Map(a, b) => self
                .ty_gap(&a, depth + 1)
                .or_else(|| self.ty_gap(&b, depth + 1)),
            // Every sum, one arm: the built-in sums resolve to their variant lists.
            Type::Enum(vs) => vs
                .iter()
                .flat_map(|v| v.payload.iter())
                .find_map(|p| self.ty_gap(p, depth + 1)),
            _ => None,
        }
    }

    fn fields(&self, ty: &Type) -> Option<Vec<Field>> {
        ftypes::record_fields(&self.sub(ty), &self.types).map(std::borrow::Cow::into_owned)
    }

    /// The signature a call site sees. `index` is filled in by the caller, which
    /// is the only thing that knows where in the module this lands. The core body under `key`
    /// decides [`Sig::in_place`].
    fn signature(&self, f: &Function, key: &str) -> Result<Sig, String> {
        if !f.type_params.is_empty() {
            return unsupported(&format!("generic function `{}`", f.name), f.line);
        }
        for p in &f.params {
            // Every parameter's representation must exist, or a gap in a callee surfaces at every
            // caller. A `modify` one crosses as an address, but the callee copies its value.
            self.repr(&p.ty, f.line)?;
        }
        let ret = self.repr(&f.ret, f.line)?;
        // The host calls an `export extern fn` with an out-pointer.
        let in_place = (self.world.body_of(key))
            .filter(|_| ret.agg().is_some() && !f.is_export_extern)
            .and_then(|body| {
                let n = vyrn_lower::core::returned_param(body)?;
                f.params.iter().position(|p| {
                    p.name == body.names[n.index()].source
                        && p.capability == Capability::Consume
                        && p.ty == f.ret
                })
            });
        Ok(Sig {
            index: 0,
            params: f.params.iter().map(|p| p.ty.clone()).collect(),
            modify: f
                .params
                .iter()
                .map(|p| p.capability == Capability::Modify)
                .collect(),
            ret,
            ret_ty: f.ret.clone(),
            in_place,
        })
    }

    /// The wasm signature of a Vyrn function: an aggregate return becomes a
    /// hidden leading pointer the callee writes through, unless it is left in a parameter, and
    /// every aggregate parameter is its address.
    fn wasm_sig(&self, sig: &Sig, line: usize) -> Result<(Vec<ValType>, Vec<ValType>), String> {
        let mut params = Vec::new();
        if sig.ret.agg().is_some() && sig.in_place.is_none() {
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

/// Where a binding lives: a wasm local for a scalar, a frame slot for an aggregate, or a fixed
/// address for module state.
///
/// `Static` is its own variant because a frame offset is relative to a per-call base and a
/// global's address is not. It covers scalar globals too: a wasm global holds one value type and
/// cannot hold a record.
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

/// A stream's step signature, a function of the element type alone. The construction
/// site and the dispatching loop both derive it here, because a stored `fn` value is keyed by
/// signature and two spellings would be two dispatchers.
///
/// The third parameter is the closing flag: a stream frees its cursor slot by calling its own
/// step, because the slab is `std/stream`'s.
fn stream_step_sig(elem: &Type) -> Type {
    Type::Fn(
        vec![Type::Int, Type::Int, Type::Bool],
        Box::new(Type::option(elem.clone())),
    )
}

/// What one owned binding is released with, in this backend's vocabulary. The placement says
/// which runs at which exit; this says what running one emits.
#[derive(Clone)]
struct RelSlot {
    place: Place,
    rel: Rel,
}

/// What a release frame entry reclaims.
#[derive(Clone, PartialEq, Eq, Debug)]
enum Rel {
    /// A `Stream<T>`: its buffer, or its cursor slot if it is a producer. Releasing a producer
    /// calls its step, which is dispatched by element type.
    Stream(Type),
    /// A `String` — the place holds the buffer pointer itself.
    Str,
    /// An aggregate owning heap buffers at these byte offsets: an `Array`'s data,
    /// a `Map`'s keys and values, a `SmallArray`'s `data` (null while inline,
    /// which `free` refuses).
    Buffers(Vec<u32>),
    /// An aggregate copied by value that holds heap in its places: a record field, an enum
    /// payload, a closure's capture block. Only the live variant's payload is released. The
    /// second field is the holes a `consume` took out of this binding, relative to it; it is
    /// empty except at a drained `let`, and the walk skips them.
    Deep(Type, Vec<String>),
    /// A type that declared `impl Owned`: a call of its declared `release`, by flattened name,
    /// with the receiver's type, through the ordinary call path.
    Call(String, Type),
}

impl Place {
    /// Pushes the address `off` bytes into this place, or `None` for a wasm local, which has no
    /// address. So a scalar `modify` argument must be spilled.
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

/// Storage an aggregate is built into: a frame slot, or an address a wasm
/// local holds plus an offset.
///
/// A literal or an aggregate-returning call writes straight into one of these when the consumer
/// owns the storage, so a nested literal needs no frame of its own.
///
/// A value is built in place only into storage nothing can name during the build: a fresh
/// `let`'s slot, a part of a literal under construction, a call's result. A store into named
/// storage keeps the copy, because a field written early would be visible to a later field's
/// initializer, and the interpreter builds the whole value first.
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
    parent: Name,
    /// The part's offset in the storage `into` names.
    off: u32,
    into: PartIn,
    /// The type the parent's layout gives the part.
    ty: Type,
}

/// The wasm one `std/mem` primitive lowers to: the instructions under its arguments, then the
/// ones after them. `None` for a name with no row. `std/mem.vyrn` declares the types
/// ([`Cx::mem`]); `every_mem_declaration_has_a_row` holds the two lists together.
///
/// A host import is one `call` of its [`crate::WASI_IMPORTS`] row. The `vyrn_gen` pair exists
/// only under a generation ([`Cx::gen`]); elsewhere a call to either is `unreachable`, and its
/// callers (`readFileGen`, `listDirGen`) are unreachable and swept.
fn mem_ins(
    cx: &Cx<'_>,
    prim: &str,
) -> Option<(Vec<Instruction<'static>>, Vec<Instruction<'static>>)> {
    use Instruction as I;
    let at = |align| mem_arg(0, align);
    let rt = &cx.rt;
    let after = match prim {
        "genRead" => cx.gen.map_or(I::Unreachable, |g| I::Call(g.read)),
        "genFetch" => cx.gen.map_or(I::Unreachable, |g| I::Call(g.fetch)),
        "load8" => I::I32Load8U(at(0)),
        "load16" => I::I32Load16U(at(1)),
        "load32" => I::I32Load(at(2)),
        "load64" => I::I64Load(at(3)),
        "loadF32" => I::F32Load(at(2)),
        "loadF64" => I::F64Load(at(3)),
        "store8" => I::I32Store8(at(0)),
        "store16" => I::I32Store16(at(1)),
        "store32" => I::I32Store(at(2)),
        "store64" => I::I64Store(at(3)),
        "storeF32" => I::F32Store(at(2)),
        "storeF64" => I::F64Store(at(3)),
        "copy" => MEMORY_COPY,
        "fill" => I::MemoryFill(0),
        "memorySize" => I::MemorySize(0),
        "grow" => I::MemoryGrow(0),
        "heapBase" => I::GlobalGet(HEAP_BASE),
        "site" if cx.profile.is_some() => I::GlobalGet(SITE),
        "site" => I::I32Const(0),
        "ioTable" => I::I32Const(rt.io as i32),
        "utf8Table" => I::I32Const(rt.utf8d as i32),
        // The descriptor under the message is stderr; `write_all` first, so stdout is flushed
        // before the message.
        "trap" => {
            return Some((
                vec![I::I32Const(2)],
                vec![
                    I::Call(rt.write_all),
                    I::Drop,
                    I::I32Const(1),
                    I::Call(rt.proc_exit),
                    I::Unreachable,
                ],
            ))
        }
        _ => I::Call(rt.wasi.find(prim)?.0),
    };
    Some((Vec::new(), vec![after]))
}

/// The end of an aggregate store, with the destination's address and the
/// value's on the stack: two drops when the value was built `in_place`, and
/// the copy of `size` bytes otherwise.
fn agg_landed(b: &mut Frame, size: u32, in_place: bool) {
    if in_place {
        b.ins(&Instruction::Drop);
        b.ins(&Instruction::Drop);
    } else {
        b.copy(size);
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

    /// Whether the binding's place `p` is this destination.
    fn holds(self, p: Place) -> bool {
        match (self, p) {
            (Dest::Slot(a), Place::Slot(b)) => a == b,
            (Dest::Addr(l, 0), Place::Local(k)) => l == k,
            _ => false,
        }
    }

    /// The destination `off` bytes into this one.
    fn at(self, off: u32) -> Dest {
        match self {
            Dest::Slot(base) => Dest::Slot(base + off),
            Dest::Addr(l, base) => Dest::Addr(l, base + off),
        }
    }

    /// A binding's place as a destination. Module state is named storage and never one
    /// ([`Dest`]); a scalar local has no address.
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

/// An empty `Function` to fill in for a lifted lambda.
///
/// The captures become ordinary read parameters, so [`lower_fn`] emits it with no case of its
/// own. [`Fn_::lift_lambda`] names it `@lambda <owner>`, because the analysis records a lambda's
/// release rows under the enclosing function and [`lower_body`] reads `Cx::releases` under it.
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
        body: Block {
            id: Id::NEW,
            stmts: Vec::new(),
        },
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
/// A specialization differs from its callee only in its signature, so [`Pending::body`] points
/// at the callee's own block, the one the checker typed and `vyrn-lower` recorded.
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
    /// (break target, continue target, region depth) per enclosing loop. The first two are the
    /// depth each was opened at, so `br` distance is `depth - opened - 1`; the third is how many
    /// `region` blocks were open at the loop's start, which an exit edge must close.
    loops: Vec<(u32, u32, u32)>,
    /// The two locals a failed bounds check parks its message and index in before branching to
    /// the function's one trap site (`bounds_check`). `None` for a frame with no site (the
    /// globals initializer), which calls at the check.
    trap_site: Option<(u32, u32)>,
    ret: Repr,
    ret_ty: Type,
    /// The wasm local holding the hidden aggregate-return pointer, if any.
    dest: Option<u32>,
    /// Reusable scratch, taken on first use. Every use is a set immediately
    /// followed by the reads that consume it, so one pair suffices however
    /// deeply expressions nest.
    scratch: HashMap<(ValType, u8), u32>,
    /// What each owned binding is released with, keyed by `own`'s key: the `Stmt::Let`'s node
    /// address, or the construct's for a temporary it owns. The order is
    /// [`vyrn_frontend::own::Ownership::releases`]'; this is a lookup table.
    rel_slots: HashMap<NodeId, RelSlot>,
    /// The release steps placed at every exit of this body, keyed by the node the exit is at.
    /// Read, never derived.
    placed: HashMap<(ExitKind, NodeId), Vec<(NodeId, Option<Vec<String>>)>>,
    /// Lexical `region` depth in this body, so an exit edge knows how many arena scopes it leaves.
    /// The runtime counter is dynamic; this is the part this body's own `br`s unwind past.
    region_depth: u32,
    /// One local per region open in this body, outermost first: the mark
    /// [`Fn_::region_enter`] took, which [`Fn_::region_exit`] hands back to
    /// `std/runtime`. Parallel to `region_depth`, which is its length while a
    /// statement is being lowered.
    region_marks: Vec<u32>,
    /// The holes the walk in progress must skip, relative to the place it looks at. Taken at the
    /// top of [`Fn_::rel_at`], so a walk into anything but a record starts empty.
    rel_holes: Vec<String>,
    /// Inside a specialization, each `fn`-typed parameter's direct-call target. Empty in
    /// an ordinary function, so no function table exists.
    fn_binds: HashMap<String, FnBinding>,
    /// wasm local holding the accumulator's pointer → the frame slot holding its
    /// ownership flag. Keyed by local index rather than by name because the
    /// local IS the binding: two `let out`s in one body are two accumulators, and
    /// a global (a `Place::Static`) never gets an entry at all.
    str_append: HashMap<u32, u32>,
    dest_used: bool,
    /// The declared function this frame's plan rows are under: the function itself, or for a
    /// lifted lambda the function holding the literal.
    owner: String,
    /// The name the core built this body under, which a lambda it lifts is
    /// keyed under too ([`vyrn_lower::core::lambda_spelling`]).
    core_key: String,
    /// This frame's own core body, which [`Fn_::core_body`] walks. `None` for
    /// a frame the core states no body for, which [`lower_body`] refuses.
    core: Option<&'a vyrn_frontend::core::Body>,
    /// The release rows held back for the read an exit hands back — see
    /// [`Fn_::core_releases`].
    core_rows: Vec<(Name, Vec<String>)>,
    /// Where the core's names live, and what the operand stack is holding.
    /// A name a parameter or a `let` bound is found through [`Fn_::scope`], and
    /// one this walk bound is pushed onto it.
    core_w: Walked,
    /// Tables over [`Fn_::core`], built on first use. Empty again after [`Fn_::core_enter`].
    facts: std::cell::OnceCell<BodyFacts<'a>>,
    lists: std::cell::OnceCell<Vec<(Name, Name)>>,
}

/// What the per-name screens ask of the whole body, collected once.
#[derive(Default)]
struct BodyFacts<'a> {
    lets: Vec<(Name, &'a Rhs)>,
    written: Vec<(Name, Option<Capability>)>,
    binders: Vec<Name>,
}

/// A lowering context with nothing in scope and nothing to return to, for code outside any
/// function. Module state stays visible because it lives in [`Cx`].
fn top_level<'a, 'p>(cx: &'a Cx<'p>) -> Fn_<'a, 'p> {
    Fn_ {
        cx,
        scope: Vec::new(),
        depth: 0,
        loops: Vec::new(),
        trap_site: None,
        ret: Repr::Unit,
        ret_ty: Type::Unit,
        dest: None,
        scratch: HashMap::new(),
        rel_slots: HashMap::new(),
        placed: HashMap::new(),
        region_depth: 0,
        region_marks: Vec::new(),
        rel_holes: Vec::new(),
        fn_binds: HashMap::new(),
        str_append: HashMap::new(),
        dest_used: false,
        owner: String::new(),
        core_key: String::new(),
        core: None,
        core_rows: Vec::new(),
        core_w: Walked::default(),
        facts: Default::default(),
        lists: Default::default(),
    }
}

/// The module-state initializer, which `_start` calls before `main`. It walks the core's
/// module-state body; a program whose initializers the core lacks is refused at its first global.
fn lower_globals_init(m: &mut Module, program: &Program, cx: &Cx<'_>) -> Result<Frame, String> {
    let mut b = Frame::new(&[], &[], &[], 0);
    let mut f = top_level(cx);
    let from_core = cx.world.body_of("").filter(|core| {
        f.core = Some(core);
        f.core_enter(core);
        f.core_walkable(None)
    });
    match &from_core {
        Some(_) => f.core_body(m, &mut b)?,
        None => {
            if let Some(g) = program.globals.first() {
                return unsupported("a module-state initializer the core did not state", g.line);
            }
        }
    }
    for g in &program.globals {
        // The accumulator owns its buffer unless the initializer is a literal, which lives in
        // the data segment.
        if let Some(&at) = cx.gappend.get(&g.name) {
            let owns = !matches!(g.init, Expr::Str(_, _));
            b.ins(&Instruction::I32Const(at as i32))
                .ins(&Instruction::I32Const(owns as i32))
                .ins(&Instruction::I32Store(word()));
        }
        // One frame holds every initializer's temporaries; checking per global names the one
        // that crossed the bound.
        frame_fits(&b, &g.name, g.line)?;
    }
    Ok(b)
}

/// The module-state teardown, emitted only in an audited build: every global released in
/// reverse declaration order. Module state outlives `main`, so without it every global that
/// owns heap would count as residue.
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

/// `sig` is passed because a specialization has no entry in `Cx::sigs`: several instances
/// share one `Function`. `binds` is non-empty only for a specialization; its keys are the
/// callee's `fn`-typed parameter names, which the synthesized signature replaces with captures.
fn lower_fn(
    m: &mut Module,
    f: &Function,
    sig: &Sig,
    cx: &Cx<'_>,
    binds: HashMap<String, FnBinding>,
) -> Result<(), String> {
    let frame = lower_body(m, f, &f.name, Body::Block(&f.body), sig, cx, binds)?;
    fill_named(m, sig.index, frame, &f.name)
}

/// Fills function `index` with `body` and, under `VYRN_WASM_NAMES`, names it for the `name`
/// section.
fn fill_named(m: &mut Module, index: u32, body: Frame, name: &str) -> Result<(), String> {
    m.fill(index, body)?;
    if std::env::var_os("VYRN_WASM_NAMES").is_some() {
        m.name(index, name);
    }
    Ok(())
}

/// `f` is the declaration, `key` the name the core built the body under. `f` and `body` are
/// separate because a specialization borrows the callee's statements ([`Pending::body`]).
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
        Body::Value => None,
    };
    let sig = sig.clone();
    let (params, results) = cx.wasm_sig(&sig, f.line)?;
    // An aggregate result goes out through the hidden leading pointer, or stays in the
    // parameter [`Sig::in_place`] names.
    let shift = u32::from(sig.ret.agg().is_some() && sig.in_place.is_none());
    let dest = sig.ret.agg().map(|_| sig.in_place.map_or(0, |k| k as u32));
    // A lifted lambda's rows are the enclosing function's (see `f_shell`).
    let owner = f
        .name
        .strip_prefix(LAMBDA)
        .map(|rest| rest.trim_start().to_string())
        .filter(|o| !o.is_empty())
        .unwrap_or_else(|| f.name.clone());

    let mut b = Frame::new(&params, &results, &[], 0);
    let core = core_body(key, f, &binds, cx);
    let mut cx_fn = Fn_ {
        ret: sig.ret.clone(),
        // As declared, not resolved: a function returning `Age` validates at its `return`,
        // and `Age` resolved to `Int64` would not.
        ret_ty: sig.ret_ty.clone(),
        dest,
        // The release order, decided in `own::place_body`.
        placed: (cx.world.fn_id(&owner))
            .and_then(|id| cx.world.ownership.releases.get(&id))
            .map(|steps| vyrn_frontend::own::placed(steps))
            .unwrap_or_default(),
        fn_binds: binds,
        owner,
        core_key: key.to_string(),
        core: core.as_ref(),
        ..top_level(cx)
    };
    if let Some(core) = cx_fn.core {
        cx_fn.core_enter(core);
    }

    // An aggregate parameter arrives as the caller's address. The one [`Sig::in_place`] names is
    // used there, and under [`Cx::args_in_place`] so is a `read` or `modify` one. Otherwise the
    // prologue copies it into a slot of its own, and a `modify` parameter is copy-in/copy-out:
    // copied in here and back out at the epilogue, so the caller sees no write before the call
    // returns.
    let mut copy_out: Vec<(u32, Place, Repr, Type)> = Vec::new();
    for (i, p) in f.params.iter().enumerate() {
        let local = shift + i as u32;
        // The declared type, for the same reason as `ret_ty`.
        let ty = p.ty.clone();
        let r = cx.repr(&p.ty, f.line)?;
        let in_place = matches!(r, Repr::Agg(_)) && p.capability != Capability::Consume;
        let place = if sig.in_place == Some(i) || in_place && cx.args_in_place {
            Place::Local(local)
        } else if p.capability == Capability::Modify {
            let place = match &r {
                Repr::Agg(l) => {
                    let off = b.alloc(l.size, l.align);
                    b.slot(off);
                    b.ins(&Instruction::LocalGet(local));
                    b.copy(l.size);
                    Place::Slot(off)
                }
                Repr::Scalar(v) => {
                    let own = b.local(*v);
                    b.ins(&Instruction::LocalGet(local));
                    b.ins(&cx.load(&p.ty, 0));
                    b.ins(&Instruction::LocalSet(own));
                    Place::Local(own)
                }
                Repr::Unit => return unsupported("a `modify` parameter of Unit", f.line),
            };
            copy_out.push((local, place, r.clone(), p.ty.clone()));
            place
        } else {
            match &r {
                Repr::Agg(l) => {
                    let off = b.alloc(l.size, l.align);
                    b.slot(off);
                    b.ins(&Instruction::LocalGet(local));
                    b.copy(l.size);
                    Place::Slot(off)
                }
                _ => Place::Local(local),
            }
        };
        // An owned `consume` parameter the body neither moves nor drops is released at exit.
        if p.capability == Capability::Consume {
            // A stored value's capture stands for the `fn` parameter it binds.
            let name = (cx_fn.fn_binds.iter())
                .find(|(_, bnd)| bnd.cap_srcs == [p.name.as_str()])
                .map_or(&p.name, |(n, _)| n);
            let key = cx.declared_param(f, name, p).id();
            if cx_fn.releases_whole(key) {
                if let Some(r) = cx_fn.rel_for(&ty, f.line)? {
                    cx_fn.register_rel(key, place, r);
                }
            }
        }
        cx_fn.scope.push((p.name.clone(), place, ty));
    }
    // The core's parameters are the declaration's, in order.
    if let Some(core) = cx_fn.core.clone() {
        for (i, n) in core.params.iter().enumerate() {
            if let Some((_, place, ty)) = cx_fn.scope.get(i) {
                cx_fn.core_w.at[n.index()] = Some((*place, ty.clone()));
            }
        }
    }

    // One frame of the call-depth budget. A lifted lambda cannot recurse without a named
    // function, so it is not counted; nor is a `std/runtime` function. Every engine must trap
    // at the same call.
    let counted =
        !f.name.starts_with(LAMBDA) && !f.name.starts_with(vyrn_frontend::loader::RUNTIME_PREFIX);
    if counted {
        call_depth_enter(&mut b, cx);
    }
    // The trap site: a failed check stores its trap-table row and value in these two locals
    // and branches out of the trap block to the one `trapAt` call. The call-depth check stands
    // before the block and calls `trapAt` itself. One call site per function, not per check,
    // halved nbody's time under Cranelift.
    cx_fn.trap_site = Some((b.local(ValType::I32), b.local(ValType::I64)));
    // The block every `return` targets. It carries a scalar result; an aggregate travels
    // through `dest`.
    b.ins(&Instruction::Block(match &sig.ret {
        Repr::Scalar(v) => BlockType::Result(*v),
        _ => BlockType::Empty,
    }));
    // The trap block: a failed check branches out of it to the trap call, while a `return`
    // branches out of the function block, past the call, to the epilogue. A body never emits
    // `return` (see `Frame::ins`), or the frame's stack pop is skipped.
    b.ins(&Instruction::Block(BlockType::Empty));
    cx_fn.depth += 1;
    // A body the screen reads whole is walked from the core; any other body is refused.
    if let Some(core) = cx_fn.core {
        cx_fn.core_lift_targets(m, &core.stmts);
    }
    if cx_fn.core.is_none() || !cx_fn.core_walkable(stmts) {
        let what = format!("the body of `{}` the core did not state", cx_fn.owner);
        return unsupported(&what, f.line);
    }
    cx_fn.core_body(m, &mut b)?;
    // The checker proves a value-returning body never falls off its end; the validator needs
    // `unreachable`. Any other body leaves the trap block as a `return` does.
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
    // `trapAt` exits the process.
    b.ins(&Instruction::Unreachable);
    b.ins(&Instruction::End);

    // The `modify` copy-out, once, at the one exit. Stack-neutral, so a scalar result on the
    // stack survives it.
    for (arg, place, r, ty) in &copy_out {
        match (place, r) {
            (Place::Local(own), _) => {
                b.ins(&Instruction::LocalGet(*arg));
                b.ins(&Instruction::LocalGet(*own));
                b.ins(&cx.store(ty));
            }
            (Place::Slot(off), Repr::Agg(l)) => {
                b.ins(&Instruction::LocalGet(*arg));
                b.slot(*off);
                b.copy(l.size);
            }
            _ => return unsupported("a `modify` parameter of this shape", f.line),
        }
    }

    // Stack-neutral, like the copy-out.
    if counted {
        call_depth_bump(&mut b, cx, -1);
    }

    frame_fits(&b, &f.name, f.line)?;
    Ok(b)
}

/// What a `fn` parameter bound to `b` calls, as the core names it. `None` for a lambda key
/// [`vyrn_lower::World::body_of`] names no body for.
fn core_target_of(cx: &Cx<'_>, b: &FnBinding) -> Option<Target> {
    if let Some(f) = cx.named(&b.target) {
        return Some(Target::Fn(f));
    }
    if cx.is_dispatcher(b.target.sig.index) {
        return match b.cap_srcs.as_slice() {
            [value] => Some(Target::Value(value.clone())),
            _ => None,
        };
    }
    let key = cx.lambda_key(b.target.sig.index)?;
    cx.world.body_of(&key)?;
    let (tys, params) = b.target.sig.params.split_at_checked(b.target.ncaps)?;
    let slot = Type::Fn(params.to_vec(), Box::new(b.target.sig.ret_ty.clone()));
    let caps: Vec<_> = (b.cap_srcs.iter().cloned())
        .zip(tys.iter().cloned())
        .collect();
    (b.cap_srcs.len() == tys.len()).then(|| Target::Lambda(key, caps, slot))
}

/// A higher-order call row's arguments in its instance's order. The row omits each `fn`
/// argument and [`vyrn_lower::core::specialize`] appends the forwarded captures after its own;
/// the instance takes them where the `fn` parameter stood. `None` where the counts disagree.
fn ho_args(
    f: &Function,
    targets: &[Target],
    args: &[(Arg, vyrn_frontend::ast::Capability)],
) -> Option<Vec<(Arg, vyrn_frontend::ast::Capability)>> {
    let caps = |t: &Target| match t {
        Target::Lambda(_, c, _) => c.len(),
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

/// The core body a queued function reads, specialized for its targets where the core names
/// them. `None` unless the body's parameters are `f`'s, by name and in order.
fn core_body(
    key: &str,
    f: &Function,
    binds: &HashMap<String, FnBinding>,
    cx: &Cx<'_>,
) -> Option<vyrn_frontend::core::Body> {
    let body = cx.world.body_of(key)?;
    let bound: Option<Vec<(Name, Target)>> = (body.params.iter())
        .filter_map(|&n| Some((n, binds.get(&body.names[n.index()].source)?)))
        .map(|(n, b)| Some((n, core_target_of(cx, b)?)))
        .collect();
    let body = match bound {
        Some(bound) if !binds.is_empty() && bound.len() == binds.len() => {
            vyrn_lower::core::specialize(body, &bound).unwrap_or_else(|| body.clone())
        }
        _ => body.clone(),
    };
    let sources = body.params.iter().map(|&n| &body.names[n.index()].source);
    (body.params.len() == f.params.len() && sources.eq(f.params.iter().map(|p| &p.name)))
        .then_some(body)
}

/// Refuses a frame larger than [`vyrn_frontend::trap::FRAME_LIMIT`], the shadow stack divided
/// by the call-depth limit, so a deep call cannot overrun the stack. The wording follows
/// [`crate::check_inst_depth`].
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

/// Take one call frame, or trap. Inline rather than a `std/runtime` call: a call pair per
/// user call cost nbody 7 percent and fannkuch 12 percent.
fn call_depth_enter(b: &mut Frame, cx: &Cx<'_>) {
    let at = cx.rt.call_depth;
    // The prologue stands before the trap block, so it calls `trapAt` itself.
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

/// The derived deep copy of a stored `fn` value's capture block: `(tag, block) -> block`.
/// Only the tag says the block's size and captures, so the copy reads them off the registry.
/// It mirrors [`lower_fnval_free`].
fn lower_fnval_copy(m: &mut Module, cx: &Cx<'_>) -> Result<Frame, String> {
    let mut b = Frame::new(&[ValType::I64, ValType::I32], &[ValType::I32], &[], 0);
    let mut f = top_level(cx);
    let (tag, pay) = (0u32, 1u32);
    let vals = cx.fnvals.borrow().clone();
    for (i, v) in vals.iter().enumerate() {
        let cap_tys = v.target.sig.params[..v.target.ncaps].to_vec();
        // No captures means payload 0, which copies to itself.
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
        b.copy(bl.size);
        fnval_captures(m, &mut f, &mut b, dst, &bl, &cap_tys, false)?;
        b.ins(&Instruction::LocalGet(dst));
        b.ins(&Instruction::Return);
        b.ins(&Instruction::End);
    }
    // Every other tag has payload 0, and the copy is the value.
    b.ins(&Instruction::LocalGet(pay));
    Ok(b)
}

/// The derived release of a stored `fn` value's capture block: `(tag, block) -> ()`. The block
/// owns its captures' heap, and only the tag says which captures it holds.
fn lower_fnval_free(m: &mut Module, cx: &Cx<'_>) -> Result<Frame, String> {
    let mut b = Frame::new(&[ValType::I64, ValType::I32], &[], &[], 0);
    let mut f = top_level(cx);
    let (tag, pay) = (0u32, 1u32);
    let vals = cx.fnvals.borrow().clone();
    for (i, v) in vals.iter().enumerate() {
        let cap_tys = v.target.sig.params[..v.target.ncaps].to_vec();
        // The tail below frees the block itself.
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
    // A payload of 0 means no captures, and `free` refuses it.
    b.ins(&Instruction::LocalGet(pay));
    b.ins(&Instruction::Call(cx.rt.free));
    Ok(b)
}

/// Walk the captures of the block at local `at`: release (`rel`) or deep-copy each one that
/// owns heap.
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

/// One signature's dispatcher: switch on the tag, unpack the capture block, and call the
/// target directly. Nested `if`/`else` end in an `unreachable` arm, which types the chain.
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

    // Parameters: the aggregate-return destination if any, the fn value's address, then the
    // signature's own.
    let dest = dsig.ret.agg().map(|_| 0u32);
    let shift = u32::from(dest.is_some());
    let fv = shift;
    for (i, pty) in ptys.iter().enumerate() {
        let local = shift + 1 + i as u32;
        // `Place` cannot name an address in a local, so an aggregate is copied into a slot,
        // as in `lower_body`'s prologue.
        let place = match cx.repr(pty, 0)? {
            Repr::Agg(l) => {
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                b.ins(&Instruction::LocalGet(local));
                b.copy(l.size);
                Place::Slot(off)
            }
            Repr::Scalar(_) => Place::Local(local),
            Repr::Unit => return unsupported("a Unit parameter of a stored `fn`", 0),
        };
        args.push((place, pty.clone()));
    }

    let fl = cx.layout(sig_ty, 0)?;
    let tag = b.local(ValType::I64);
    let pl = b.local(ValType::I32);
    b.ins(&Instruction::LocalGet(fv));
    b.ins(&Instruction::I64Load(at(fl.fields[0])));
    b.ins(&Instruction::LocalSet(tag));
    load_wrapped(&mut b, fv, fl.fields[1]);
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
        // The capture block, copied into a frame slot so each capture has a `Place`.
        let cap_tys = v.target.sig.params[..v.target.ncaps].to_vec();
        let mut all: Vec<(Place, Type)> = Vec::new();
        if !cap_tys.is_empty() {
            let bl = f.cap_block(&cap_tys)?;
            let blk = b.alloc(bl.size, bl.align);
            b.slot(blk);
            b.ins(&Instruction::LocalGet(pl));
            b.copy(bl.size);
            for (ci, ct) in cap_tys.iter().enumerate() {
                let at_off = blk + bl.fields[ci];
                let place = match cx.repr(ct, 0)? {
                    Repr::Scalar(vt) => {
                        let loc = b.local(vt);
                        b.slot(at_off);
                        b.ins(&cx.load(ct, 0));
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
        // An aggregate result is written through this function's destination.
        if let Some(d) = dest {
            b.ins(&Instruction::LocalGet(d));
        }
        // Each value crosses at the target's parameter type, as an argument
        // does ([`Fn_::expr_as`]).
        let got = f.emit_call_with(m, &mut b, &v.target.sig, &all, None)?;
        match (&dsig.ret, cx.repr(&got, 0)?) {
            // The target's declared result may differ from the signature's, so it coerces.
            (Repr::Scalar(_), _) => f.coerce(m, &mut b, &got, ret, 0)?,
            (Repr::Agg(l), Repr::Agg(_)) => {
                f.coerce(m, &mut b, &got, ret, 0)?;
                b.copy(l.size);
            }
            // A Unit-signature slot may hold a value-returning function; the result is dropped.
            (Repr::Unit, Repr::Scalar(_) | Repr::Agg(_)) => {
                b.ins(&Instruction::Drop);
            }
            (Repr::Unit, Repr::Unit) => {}
            _ => return unsupported("a stored `fn` whose result shape is not its signature's", 0),
        }
        b.ins(&Instruction::Else);
    }
    // Unreachable: every tag comes from a registered construction.
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
    /// Scratch local `n` of type `t`, taken on first use. Every use is a set followed by its
    /// reads, and a nested expression completes before the outer one touches scratch.
    fn scratch(&mut self, b: &mut Frame, t: ValType, n: u8) -> u32 {
        *self.scratch.entry((t, n)).or_insert_with(|| b.local(t))
    }

    /// Record how one owned binding is released; the placement decides where and when.
    fn register_rel(&mut self, key: NodeId, place: Place, rel: Rel) {
        self.rel_slots.insert(key, RelSlot { place, rel });
    }

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
                // A `String` is a scalar, so a `Place::Local` holds the pointer, not an
                // aggregate's address. The block base is the pointer less its header; `free`
                // refuses a literal below `HEAP_BASE`.
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
            // The walk needs an address; an aggregate in a local is its address.
            Rel::Deep(ty, holes) => {
                let a = self.addr_local(b, p, 0);
                let t = ty.clone();
                // The places a `consume` took; empty for most bindings.
                self.rel_holes = holes.clone();
                let r = self.rel_at(m, b, a, &t, line);
                self.rel_holes.clear();
                r
            }
            Rel::Call(f, ty) => {
                self.release_call(m, b, f, ty, p, line)?;
                // A user enum's payload boxes are its own storage, which the declared release
                // cannot reach.
                let ty = ty.clone();
                if !matches!(self.cx.resolve(&ty), Type::Enum(_)) {
                    return Ok(());
                }
                let a = self.addr_local(b, p, 0);
                self.free_declared_boxes(b, a, &ty, line)
            }
        }
    }

    /// Call the `release` a type declares (`impl Owned`) on the value at `p`, instantiated at
    /// `ty` where the impl is generic.
    fn release_call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        f: &str,
        ty: &Type,
        p: Place,
        line: usize,
    ) -> Result<(), String> {
        // Inside a monomorphized instance `ty` names the instance's parameters.
        let sig = match self.cx.generics.get(f).copied() {
            Some(g) => self.generic_sig(m, g, &[self.cx.sub(ty).into_owned()], line)?,
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
        self.coerce(m, b, ty, &param, line)?;
        b.ins(&Instruction::Call(sig.index));
        Ok(())
    }

    /// Push the value of type `t` at `place`; an aggregate is pushed as its address.
    fn push_place(&self, b: &mut Frame, place: Place, t: &Type, line: usize) -> Result<(), String> {
        match place {
            Place::Local(l) => {
                b.ins(&Instruction::LocalGet(l));
            }
            Place::Slot(off) => {
                b.slot(off);
            }
            // A global scalar is loaded from memory.
            Place::Static(at) => {
                b.ins(&Instruction::I32Const(at as i32));
                if let Repr::Scalar(_) = self.cx.repr(t, line)? {
                    b.ins(&self.cx.load(t, 0));
                }
            }
        }
        Ok(())
    }

    /// Free only the payload boxes of an enum with a declared release. The declared release
    /// frees the payloads, but no Vyrn surface names the box a wide payload rides in. A
    /// non-enum answers `Ok(())`.
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
        let l = self.cx.layout(ty, line)?;
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
                load_wrapped(b, a, l.fields[j]);
                b.ins(&Instruction::Call(self.cx.rt.free));
            }
            self.depth -= 1;
            b.ins(&Instruction::End);
        }
        Ok(())
    }

    /// Whether `own` gives `ty` an element-walking release rather than a buffer-only one.
    fn deep_row(&self, ty: &Type) -> bool {
        matches!(
            self.cx.world.ownership.proto.release_kind(ty),
            Some(DropKind::Deep(_))
        )
    }

    /// The address of a `SmallArray`'s live slots: the inline block while `cap == N`, the
    /// spilled buffer otherwise. Copy and release share it.
    fn sa_base(&mut self, b: &mut Frame, a: u32, ty: &Type, line: usize) -> Result<u32, String> {
        let Type::SmallArray(_, cap_n) = self.cx.resolve(ty) else {
            return Err(gap("a SmallArray base", line));
        };
        let l = self.cx.layout(ty, line)?;
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

    /// The release a value of `ty` bound at node `key` owes: [`Fn_::rel_for`]'s, or the buffer
    /// alone where `key` is a `for` whose elements all left through the loop variable
    /// ([`Cx::loop_buffer_only`]).
    fn rel_owed(&mut self, key: NodeId, ty: &Type, line: usize) -> Result<Option<Rel>, String> {
        let buffer = self.cx.loop_buffer_only(key);
        Ok(self
            .rel_for(ty, line)?
            .map(|r| if buffer { Rel::Buffers(vec![0]) } else { r }))
    }

    /// How a value of `ty` is reclaimed, or `None` for one that owns no heap: the
    /// [`vyrn_frontend::declared::Owned::release_kind`] row plus `layout`'s byte offsets.
    /// A declared row is keyed by name, so it is asked of `ty`; every other row is asked of the
    /// resolved type, because a generic body's `Param` element always answers `Deep`.
    fn rel_for(&mut self, ty: &Type, line: usize) -> Result<Option<Rel>, String> {
        if let Some(DropKind::Release(f, _)) = self.cx.world.ownership.proto.release_kind(ty) {
            return Ok(Some(Rel::Call(f, ty.clone())));
        }
        let t = self.cx.resolve(ty);
        let bufs = |which: &[usize]| -> Result<Rel, String> {
            let l = self.cx.layout(&t, line)?;
            Ok(Rel::Buffers(which.iter().map(|i| l.fields[*i]).collect()))
        };
        Ok(match self.cx.world.ownership.proto.release_kind(&t) {
            Some(DropKind::Release(f, _)) => Some(Rel::Call(f, t.clone())),
            Some(DropKind::FreeStr) => Some(Rel::Str),
            Some(DropKind::FreeArr) => Some(bufs(&[0])?),
            Some(DropKind::FreeMap) => Some(bufs(&[0, 1, 4])?),
            // `{ i64 len, i64 cap, ptr data, [N x T] inline }`: `data` is null until the array
            // spills, and `free` refuses null.
            Some(DropKind::FreeSmallArr) => Some(bufs(&[2])?),
            Some(DropKind::Deep(d)) => Some(Rel::Deep(d, Vec::new())),
            // A `Stream<T>` is released by the stream lowering; a row here would release it twice.
            Some(DropKind::CloseStream) | None => match t {
                Type::Stream(i) => Some(Rel::Stream(*i)),
                _ => None,
            },
        })
    }

    /// The index of the release (or copy) of `ty`, reserved and queued on first request.
    /// The type is substituted here because the body is written later, under another instance.
    fn shape_fn(&self, m: &mut Module, rel: bool, ty: &Type, line: usize) -> u32 {
        let key: ShapeKey = (
            rel,
            self.cx.sub(ty).into_owned(),
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

    /// Release the heap the value at `a` holds, as a call to the type's [`Fn_::rel_body`]
    /// function. A declared release is called directly.
    fn rel_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        a: u32,
        ty: &Type,
        line: usize,
    ) -> Result<(), String> {
        // A declared release runs instead of a field walk, which would skip its order and
        // side effects.
        if let Some(DropKind::Release(f, _)) = self.cx.world.ownership.proto.release_kind(ty) {
            self.rel_holes.clear();
            // `emit_rel`'s `Rel::Call` arm also frees the payload boxes.
            return self.emit_rel(m, b, Place::Local(a), &Rel::Call(f, ty.clone()), line);
        }
        if !self.owns_heap(ty) {
            self.rel_holes.clear();
            return Ok(());
        }
        let f = self.shape_fn(m, true, ty, line);
        // The holes are part of the shape key, so every later reader starts empty.
        self.rel_holes.clear();
        b.ins(&Instruction::LocalGet(a)).ins(&Instruction::Call(f));
        Ok(())
    }

    /// The fields of the record `ty` that own heap, with their offsets: what
    /// [`Fn_::copy_body`] copies and [`Fn_::rel_body`] frees.
    fn owning_fields(&self, ty: &Type, line: usize) -> Result<Vec<(u32, Field)>, String> {
        let l = self.cx.layout(ty, line)?;
        let fields =
            (self.cx.fields(ty)).ok_or_else(|| gap(&format!("the fields of `{ty}`"), line))?;
        Ok((fields.into_iter().enumerate())
            .filter(|(_, f)| self.owns_heap(&f.ty))
            .map(|(i, f)| (l.fields[i], f))
            .collect())
    }

    /// Per variant of the sum `ty` that has any, its tag, its name, and the payloads that own
    /// heap or ride in a box, as `(index, offset, type, word)`. A boxed payload is listed even
    /// when it owns nothing (`Option<Handle<Node>>`), so this and `own::owns_heap` must agree.
    /// [`Fn_::copy_body`] copies exactly these and [`Fn_::rel_body`] frees them.
    #[allow(clippy::type_complexity)]
    fn owning_payloads(
        &self,
        ty: &Type,
        line: usize,
    ) -> Result<Vec<(i64, String, Vec<(usize, u32, Type, Word)>)>, String> {
        let l = self.cx.layout(ty, line)?;
        let mut out = Vec::new();
        for (tag, var) in self
            .cx
            .sum_vs(ty)
            .unwrap_or_default()
            .into_iter()
            .enumerate()
        {
            let mut slots = Vec::new();
            for (j, pty) in var.payload.iter().enumerate() {
                let w = self.word2(pty)?;
                if self.owns_heap(pty) || w == Word::Boxed {
                    let off = l.fields[self.cx.payload_slot(&var.payload, j)];
                    slots.push((j, off, pty.clone(), w));
                }
            }
            if !slots.is_empty() {
                out.push((tag as i64, var.name, slots));
            }
        }
        Ok(out)
    }

    /// The release walk of `ty`, the mirror of [`Fn_::copy_body`] with `free` for `malloc`.
    /// It frees exactly the storage the copy allocates, so the two must agree on every shape.
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
            // No region test here: `free` refuses an arena block by its class word, so the
            // walk frees every block and the arena keeps its own.
            Type::Str => {
                b.ins(&Instruction::LocalGet(a))
                    .ins(&Instruction::I32Load(word()));
                str_hdr(b);
                b.ins(&Instruction::Call(self.cx.rt.free));
                Ok(())
            }
            // The elements first, then their buffer, which the walk still reads.
            Type::Array(inner) if self.deep_row(&self.cx.resolve(ty)) => {
                let l = self.cx.layout(ty, line)?;
                let stride = self.stride(&inner, line)?;
                let (n, data) = (b.local(ValType::I32), b.local(ValType::I32));
                load_wrapped(b, a, l.fields[1]);
                b.ins(&Instruction::LocalSet(n));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I32Load(word_at(l.fields[0])));
                b.ins(&Instruction::LocalSet(data));
                self.each(m, b, true, data, n, stride, &inner, line)?;
                b.ins(&Instruction::LocalGet(data));
                b.ins(&Instruction::Call(self.cx.rt.free));
                Ok(())
            }
            // `sa_base` finds the live slots. `data` is null while inline, and `free` refuses
            // null.
            Type::SmallArray(inner, _) if self.deep_row(ty) => {
                let l = self.cx.layout(ty, line)?;
                let stride = self.stride(&inner, line)?;
                let n = b.local(ValType::I32);
                load_wrapped(b, a, l.fields[0]);
                b.ins(&Instruction::LocalSet(n));
                let base = self.sa_base(b, a, ty, line)?;
                self.each(m, b, true, base, n, stride, &inner, line)?;
                b.ins(&Instruction::LocalGet(a))
                    .ins(&Instruction::I32Load(word_at(l.fields[2])))
                    .ins(&Instruction::Call(self.cx.rt.free));
                Ok(())
            }
            // Two parallel buffers, elements first. String keys are released per entry; Int64
            // and packed keys go with their buffer.
            Type::Map(kt, vt) if self.deep_row(ty) => {
                let mk = self.map_key(&kt, line)?;
                let l = self.cx.layout(ty, line)?;
                let vstride = self.stride(&vt, line)?;
                let n = b.local(ValType::I32);
                load_wrapped(b, a, l.fields[2]);
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
                // The index holds positions, so it is freed and not walked.
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
                for (off, f) in self.owning_fields(ty, line)? {
                    // A `consume` took this field, so another owner frees it.
                    if holes.iter().any(|h| *h == f.name) {
                        continue;
                    }
                    let p = b.local(ValType::I32);
                    b.ins(&Instruction::LocalGet(a));
                    b.ins(&Instruction::I32Const(off as i32));
                    b.ins(&Instruction::I32Add);
                    b.ins(&Instruction::LocalSet(p));
                    self.rel_holes = vyrn_frontend::declared::holes_under(holes, &f.name);
                    self.rel_at(m, b, p, &f.ty, line)?;
                }
                Ok(())
            }
            // A fixed `[N x T]` is inline, so only its slots are released.
            Type::ArrayN(inner, n) => {
                let stride = self.stride(&inner, line)?;
                let count = b.local(ValType::I32);
                b.ins(&Instruction::I32Const(n as i32));
                b.ins(&Instruction::LocalSet(count));
                self.each(m, b, true, a, count, stride, &inner, line)
            }
            Type::Enum(_) => {
                for (tag, name, slots) in self.owning_payloads(ty, line)? {
                    tag_eq(b, a, tag);
                    b.ins(&Instruction::If(BlockType::Empty));
                    self.depth += 1;
                    for (j, off, pty, w) in slots {
                        // A `consume` took the payload (`Elem.1`); its box is still the sum's.
                        let key = format!("{name}.{j}");
                        if holes.contains(&key) {
                            if w == Word::Boxed {
                                load_wrapped(b, a, off);
                                b.ins(&Instruction::Call(self.cx.rt.free));
                            }
                            continue;
                        }
                        self.rel_holes = vyrn_frontend::declared::holes_under(holes, &key);
                        self.rel_word(m, b, a, off, &pty, w, line)?;
                    }
                    self.depth -= 1;
                    b.ins(&Instruction::End);
                }
                Ok(())
            }
            // A stored `fn` value is `{ i64 tag, i64 captures }`; only the tag says what the
            // capture block holds, so [`lower_fnval_free`] releases it.
            Type::Fn(..) => {
                let l = self.cx.layout(ty, line)?;
                b.ins(&Instruction::LocalGet(a))
                    .ins(&Instruction::I64Load(at(l.fields[0])))
                    .ins(&Instruction::LocalGet(a))
                    .ins(&Instruction::I32Load(word_at(l.fields[1])))
                    .ins(&Instruction::Call(self.cx.fnval_free));
                Ok(())
            }
            // A handle names something another owner reclaims.
            _ => Ok(()),
        }
    }

    /// Release the map key or value at `a` before its slot is overwritten or shifted away,
    /// under the rule [`Fn_::replaced_releases`] states.
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

    /// Whether a displaced value of `ty` is released: every value that owns heap except a
    /// stream, whose consumer ends it. A declared `release` runs too (record `m7-box`).
    fn replaced_releases(&self, ty: &Type) -> bool {
        !matches!(
            self.cx.world.ownership.proto.release_kind(ty),
            None | Some(DropKind::CloseStream)
        )
    }

    /// Release the sum payload word at `a + off`, the mirror of [`Fn_::copy_word`]. A `String`
    /// rides in the word; a wider payload is a pointer to a box.
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
            Word::Ext(ValType::I32) if matches!(self.cx.resolve(pty), Type::Str) => {
                load_wrapped(b, a, off);
                str_hdr(b);
                b.ins(&Instruction::Call(self.cx.rt.free));
                Ok(())
            }
            Word::Boxed => {
                let p = b.local(ValType::I32);
                load_wrapped(b, a, off);
                b.ins(&Instruction::LocalSet(p));
                self.rel_at(m, b, p, pty, line)?;
                b.ins(&Instruction::LocalGet(p))
                    .ins(&Instruction::Call(self.cx.rt.free));
                Ok(())
            }
            _ => Ok(()),
        }
    }

    /// The address of `p` plus `off`, in a fresh local. A `Place::Local` must hold an
    /// aggregate's address, which [`Place::addr`] cannot answer.
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

    /// Keep the value at `a` before a store overwrites it, with its release, or `None` when
    /// displacing it releases nothing. [`Fn_::free_snap`] releases it after the store.
    /// The snapshot comes first because an aggregate is built destination-first; callers
    /// reach here only where the new value does not name the place
    /// ([`vyrn_frontend::ast::mentions`]).
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
                b.slot(at).ins(&Instruction::LocalGet(a)).copy(l.size);
                Place::Slot(at)
            }
            Repr::Scalar(v) => {
                let t = b.local(v);
                b.ins(&Instruction::LocalGet(a))
                    .ins(&self.cx.load(ty, 0))
                    .ins(&Instruction::LocalSet(t));
                Place::Local(t)
            }
            Repr::Unit => return Ok(None),
        };
        Ok(Some((place, rel)))
    }

    /// [`Fn_::snap_at`] for a scalar local, which has no address.
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

    /// Push a region scope: trap past `REGION_MAX`, the bound every engine shares, else bump
    /// the counter and keep the arena's mark. The mark takes a fresh local because every
    /// region open at an exit edge needs its own ([`Fn_::exit_regions_above`]).
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

    /// Pop a region scope and free its arena back to the mark [`Fn_::region_enter`] took.
    /// Stack-neutral, so a return value may sit on the operand stack.
    ///
    /// Routing is lexical ([`Fn_::arena_route`]), not by open region depth: depth would put a
    /// callee's `String` or a global's `Array` buffer in the arena, freed under a live binding.
    fn region_exit(&mut self, b: &mut Frame, mark: u32) {
        b.ins(&Instruction::LocalGet(mark))
            .ins(&Instruction::Call(self.cx.rt.region_exit));
        self.region_bump(b, -1);
    }

    /// `stringFromBytes(b)`: the bytes checked by `std/text`'s `stringFault`, then copied into
    /// a fresh `String`, as a `Result<String, String>` written through a slot allocated here.
    fn string_from_bytes(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[(Val, Capability)],
        line: usize,
    ) -> Result<Type, String> {
        let ty = Type::result(Type::Str, Type::Str);
        let Repr::Agg(l) = self.cx.repr(&ty, line)? else {
            return unsupported("`stringFromBytes` returning a non-aggregate", line);
        };
        // Typed by position, so a literal like `['h', 'i']` or `[]` is bytes.
        let bytes = Type::Array(Box::new(Type::IntN {
            bits: 8,
            signed: false,
        }));
        self.core_arg(m, b, "stringFromBytes", args, 0, Some(&bytes), line)?;
        let src = self.scratch(b, ValType::I32, 0);
        let al = self.cx.layout(&bytes, line)?;
        b.ins(&Instruction::LocalSet(src));
        let off = b.alloc(l.size, l.align);
        self.str_from_bytes(b, off, src, &al, line)?;
        b.slot(off);
        Ok(ty)
    }

    /// `bytes(s)` and `bytes(s, start, end)`: a call to `std/runtime`'s `bytesOf`, which checks
    /// the range and copies. The one-argument form is the range `0..s.byteLength`.
    ///
    /// `rule` is the three-argument form's check row's; `None` is the one-argument form, whose
    /// range cannot trap.
    fn bytes_of(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        rule: Option<vyrn_frontend::trap::Rule>,
        args: &[(Val, Capability)],
        line: usize,
    ) -> Result<Type, String> {
        let ty = Type::Array(Box::new(Type::IntN {
            bits: 8,
            signed: false,
        }));
        let l = self.cx.layout(&ty, line)?;
        let off = b.alloc(l.size, l.align);
        b.slot(off);
        self.core_arg(m, b, "bytes", args, 0, Some(&Type::Str), line)?;
        if rule.is_some() {
            self.core_arg(m, b, "bytes", args, 1, Some(&Type::Int), line)?;
            self.core_arg(m, b, "bytes", args, 2, Some(&Type::Int), line)?;
        } else {
            let s = self.scratch(b, ValType::I32, 0);
            b.ins(&Instruction::LocalTee(s));
            b.ins(&Instruction::I64Const(0));
            b.ins(&Instruction::LocalGet(s));
            str_len(b);
            b.ins(&Instruction::I64ExtendI32U);
        }
        let rule = rule.unwrap_or(vyrn_frontend::trap::Rule::StringIndex);
        b.ins(&Instruction::I32Const(rule.index() as i32));
        b.ins(&Instruction::I32Const(self.cx.rt.trap_table as i32));
        b.ins(&Instruction::Call(self.cx.rt.bytes_of));
        b.slot(off);
        Ok(ty)
    }

    /// Call `std/runtime`'s `strFromBytes` for the `Array<UInt8>` header in local `src`, writing
    /// the `Result<String, String>` into the frame slot at `dest`. The whole argument list,
    /// including `stringFault`'s answer, is built here, so no caller can miss one.
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
            // The loader injects `std/text` for these builtins; this means no std root.
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
        load_wrapped(b, src, al.fields[1]);
        b.ins(&Instruction::LocalGet(fault));
        b.ins(&Instruction::I32Const(self.cx.rt.bnul as i32))
            .ins(&Instruction::I32Const(self.cx.rt.butf8 as i32))
            .ins(&Instruction::Call(self.cx.rt.str_from_bytes));
        Ok(())
    }

    /// Inside a `region`, raise (`on`) or lower (`!on`) `std/runtime`'s arena flag around a call
    /// that allocates a `String`. `strNew` reads the flag; `malloc` never does.
    ///
    /// Stack-neutral. The window covers the whole call, so a `String` the callee makes on the
    /// way is the arena's too; none of these callees runs user code, so it cannot escape.
    /// The call sites are `rt.concat`, [`Fn_::str_dup`] and `rt.int_str`. They must stay equal
    /// to `Gen::str_alloc`'s, or the two backends own a block differently.
    fn arena_route(&mut self, b: &mut Frame, on: bool) {
        if self.region_depth > 0 {
            b.ins(&Instruction::GlobalGet(HEAP_BASE))
                .ins(&Instruction::I32Const(ARENA_ON as i32))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Const(i32::from(on)))
                .ins(&Instruction::I32Store(word()));
        }
    }

    /// Leaves a region without freeing its blocks, for a `return` or `?` that carries one out.
    /// Only the nesting counter moves, so the frame's other blocks leak.
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

    /// Closes every region scope open past `depth`. `frees` is false on the edge that hands a
    /// block out ([`Fn_::region_pop`]).
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
            // Globals come after every scope frame, so a local shadows a global.
            .or_else(|| self.cx.globals.get(name).cloned())
            .ok_or_else(|| gap(&format!("the name `{name}` (not a local)"), line))
    }

    /// The module state the core names `name` ([`vyrn_frontend::core::Place::Global`]),
    /// which no local shadows.
    fn global(&self, name: &str, line: usize) -> Result<(Place, Type), String> {
        (self.cx.globals.get(name).cloned())
            .ok_or_else(|| gap(&format!("the module state `{name}`"), line))
    }

    fn place_for(&mut self, b: &mut Frame, r: &Repr, line: usize) -> Result<Place, String> {
        Ok(match r {
            Repr::Scalar(v) => Place::Local(b.local(*v)),
            Repr::Agg(l) => Place::Slot(b.alloc(l.size, l.align)),
            Repr::Unit => return unsupported("a binding of a Unit value", line),
        })
    }

    /// Writes the ownership word of the accumulator in wasm local `l` to the frame slot at `at`.
    /// The core walk calls it at the accumulator's `let`, keyed by its node `site`.
    ///
    /// The word says whether this path allocated the buffer. `0` means no (a data-segment
    /// literal, a `concat` or call result), so the buffer may be aliased and is not grown in
    /// place. It lives in the frame because the runtime helper writes it back and a wasm local
    /// has no address. Emitting it at the `let` resets it on every trip through a loop.
    ///
    /// It starts owned only when this `let` owns its initializer: `let mut s = r.name` is a
    /// borrow, and a `literal` initializer is a data-segment address that must not grow.
    fn str_append_shadow(&mut self, b: &mut Frame, l: u32, at: u32, site: NodeId, literal: bool) {
        let owns = !literal && self.releases_whole(site);
        self.str_append.insert(l, at);
        b.slot(at)
            .ins(&Instruction::I32Const(owns as i32))
            .ins(&Instruction::I32Store(word()));
    }

    /// Grows `s = s + a + b` in place: one runtime `strAppend` per part into `place`, whose
    /// ownership word is at `own`. `parts` are the operands.
    ///
    /// If the word is 0, the first append copies out of the buffer and abandons it. A general
    /// store resets the word, so an owned buffer would leak. Where `owned_here`, the word and the
    /// old pointer are saved first, and the old buffer is freed afterwards if the word was 0 and
    /// its `cap` is not `u32::MAX` (an interned literal).
    ///
    /// The helper returns the pointer to store, because a wasm local has no address. A global's
    /// fixed address is pushed before the call so the result lands on top of it.
    #[allow(clippy::too_many_arguments)]
    fn append_in_place(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        place: Place,
        own: Place,
        owned_here: bool,
        parts: &[(Val, Capability)],
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
        for (v, _) in parts {
            match place {
                Place::Local(l) => {
                    own.addr(b, 0)
                        .ok_or_else(|| gap("an append flag with no address", line))?;
                    b.ins(&Instruction::LocalGet(l));
                    self.core_val(m, b, v, &Type::Str, line)?;
                    b.ins(&Instruction::Call(self.cx.rt.str_append));
                    b.ins(&Instruction::LocalSet(l));
                }
                Place::Static(at) => {
                    b.ins(&Instruction::I32Const(at as i32));
                    own.addr(b, 0)
                        .ok_or_else(|| gap("an append flag with no address", line))?;
                    b.ins(&Instruction::I32Const(at as i32))
                        .ins(&Instruction::I32Load(word()));
                    self.core_val(m, b, v, &Type::Str, line)?;
                    b.ins(&Instruction::Call(self.cx.rt.str_append));
                    b.ins(&Instruction::I32Store(word()));
                }
                Place::Slot(_) => return unsupported("an in-place append into a slot", line),
            }
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

    fn field_of(&self, ty: &Type, field: &str, line: usize) -> Result<(u32, Type), String> {
        let fs = self
            .cx
            .fields(ty)
            .ok_or_else(|| gap(&format!("a field of the non-record type `{ty}`"), line))?;
        let i = fs
            .iter()
            .position(|f| f.name == field)
            .ok_or_else(|| gap(&format!("the field `{field}`"), line))?;
        let l = self.cx.layout(ty, line)?;
        Ok((l.fields[i], fs[i].ty.clone()))
    }

    /// Converts the value on the stack from `from` to `to` by the rung [`crate::coerce_plan`]
    /// picks. Every flow site reaches here: a typed `let`, an assignment, a field or element
    /// store, a call argument, a return, a join arm, an enum payload. The plan owns the rung
    /// order; this is one arm per rung.
    fn coerce(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        from: &Type,
        to: &Type,
        line: usize,
    ) -> Result<(), String> {
        // Substituted, not resolved: a declared name must survive to the plan, but a `Param`
        // must not, or `T = Age` would silently skip `Age`'s validation.
        let (from, to) = (&self.cx.sub(from), &self.cx.sub(to));
        let rung = crate::coerce_plan(from, to, &self.cx.types);
        crate::observe::note_rung(from, to, rung);
        match rung {
            // A `Never` comes from a `panic` that ended in `unreachable`; the polymorphic stack
            // satisfies `to`.
            crate::Rung::Never => Ok(()),
            crate::Rung::Validate => {
                let Some(decl) = crate::validation_required(from, to, &self.cx.types).cloned()
                else {
                    return Err(crate::plan_disagrees(from, to, rung));
                };
                // The predicate reads the base's representation. The recursion ends because a
                // base is one step nearer a builtin than the name it backs.
                self.coerce(m, b, from, &decl.base, line)?;
                self.emit_validation(b, &decl, line)
            }
            // Widening reads the source's signedness; narrowing renormalizes into the target's.
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
                    // Both `i64`: only the signedness changes.
                    (_, true, true) => {}
                    // Both `i32`: only the target's normalization is owed.
                    (_, false, false) => renorm(b, t),
                }
                Ok(())
            }
            // Int to float, float to int, and between float widths. `trunc_sat`, not `trunc`,
            // because plain `trunc` traps out of range and the interpreter saturates (Rust `as`).
            // Float to a sized int saturates to 64 bits and then narrows, as the interpreter
            // does: `Int8(1e10)` is 0 that way and -1 through an `i32`.
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
            // Every `fn` spelling shares the `{ i64, i64 }` shape.
            crate::Rung::FnRetag => Ok(()),
            // Only a pair whose elements share a shape lowers here.
            crate::Rung::Elementwise => {
                if self.cx.shape(from) == self.cx.shape(to) {
                    return Ok(());
                }
                unsupported(
                    &format!("an element-wise conversion from `{from}` to `{to}`"),
                    line,
                )
            }
            // A fixed `[N x T]` literal into an `Array<T>`, the growable triple.
            crate::Rung::Heapify => {
                let Type::ArrayN(inner, n) = self.cx.resolve(from) else {
                    return Err(crate::plan_disagrees(from, to, rung));
                };
                self.heapify(b, &inner, n, to, line)
            }
            // A literal into a `SmallArray<T, N>` stays inline, with `cap` set to `N` as the
            // state discriminant. The checker proved `len <= N`.
            crate::Rung::Inline => {
                let (Type::ArrayN(inner, len), Type::SmallArray(_, n)) =
                    (self.cx.resolve(from), self.cx.resolve(to))
                else {
                    return Err(crate::plan_disagrees(from, to, rung));
                };
                self.sa_from_fixed(b, &inner, len, to, n, line)
            }
            crate::Rung::Identity => Ok(()),
            // Record width subtyping. A rebuild rather than a prefix copy, because the two
            // field orders need not agree.
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
                let sl = self.cx.layout(got, line)?;
                for (i, f) in tf.iter().enumerate() {
                    let j = ff
                        .iter()
                        .position(|g| g.name == f.name)
                        .ok_or_else(|| gap(&format!("the field `{}`", f.name), line))?;
                    if self.cx.shape(&ff[j].ty) != self.cx.shape(&f.ty) {
                        return unsupported(
                            "a record conversion that changes a field's shape",
                            line,
                        );
                    }
                    match self.cx.repr(&f.ty, line)? {
                        Repr::Scalar(_) => {
                            b.slot(off + dl.fields[i]);
                            b.ins(&Instruction::LocalGet(src));
                            b.ins(&self.cx.load(&f.ty, sl.fields[j]));
                            b.ins(&self.cx.store(&f.ty));
                        }
                        Repr::Agg(fl) => {
                            b.slot(off + dl.fields[i]);
                            b.ins(&Instruction::LocalGet(src));
                            b.ins(&Instruction::I32Const(sl.fields[j] as i32));
                            b.ins(&Instruction::I32Add);
                            b.copy(fl.size);
                        }
                        Repr::Unit => return unsupported("a Unit field", line),
                    }
                }
                b.slot(off);
                Ok(())
            }
            // Two shapes of one sum. The tag is at offset 0 and the slots follow,
            // so the shared bytes are a prefix: zero the destination, then copy the prefix.
            crate::Rung::Reshape => {
                let src = self.scratch(b, ValType::I32, 0);
                b.ins(&Instruction::LocalSet(src));
                let Repr::Agg(dl) = self.cx.repr(to, line)? else {
                    return unsupported("a sum that is not an aggregate", line);
                };
                let sl = self.cx.layout(from, line)?;
                let off = b.alloc(dl.size, dl.align);
                b.slot(off);
                b.ins(&Instruction::I32Const(0));
                b.ins(&Instruction::I32Const(dl.size as i32));
                b.ins(&Instruction::MemoryFill(0));
                b.slot(off);
                b.ins(&Instruction::LocalGet(src));
                b.copy(dl.size.min(sl.size));
                b.slot(off);
                Ok(())
            }
            crate::Rung::Refuse => {
                unsupported(&format!("a conversion from `{from}` to `{to}`"), line)
            }
        }
    }

    /// Checks the value on the stack against `decl`'s `where` predicate by calling the type's
    /// generated constructor ([`vyrn_frontend::ctor`]), which traps if it fails. The value stays
    /// on the stack.
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

    /// Replaces the value on the stack with `decl`'s `where` predicate's Bool, for a fallible
    /// construction, and returns the local holding the value. `None`, stack untouched, for a
    /// type with no refinement. Scalar bases only: the one caller needs a single-word payload.
    fn predicate_holds(
        &mut self,
        b: &mut Frame,
        decl: &TypeDecl,
        line: usize,
    ) -> Result<Option<u32>, String> {
        let name = vyrn_frontend::ctor::pred_name(&decl.name);
        self.call_generated(b, decl, &name, "predicate", line)
    }

    /// Parks the value on the stack and passes it to the generated function `name`, returning
    /// the parking local, or `None`, stack untouched, for a type with no refinement. `what`
    /// names the function in the refusal.
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

    /// Moves the value on the stack into a local. An aggregate base is its address, which is
    /// how a `read` parameter is passed, so one `i32` local holds it.
    fn park_for_predicate(
        &mut self,
        b: &mut Frame,
        decl: &TypeDecl,
        line: usize,
    ) -> Result<u32, String> {
        let Some(v) = self.cx.repr(&decl.base, line)?.val() else {
            return unsupported(
                &format!("a `where` clause over the Unit base `{}`", decl.base),
                line,
            );
        };
        let held = b.local(v);
        b.ins(&Instruction::LocalSet(held));
        Ok(held)
    }

    /// A `=~` pattern's DFA in the data segment: `(table, accept, start)`. Interned at the use
    /// site; [`Module::data`] shares identical bytes, so no pre-pass is needed.
    ///
    /// ponytail: a full 256-wide `u32` table is 1 KB per state (`twdemo`'s `Tw`: 781 states,
    /// 799,744 bytes). Byte equivalence classes would cut it if size matters.
    fn regex_dfa(
        &mut self,
        m: &mut Module,
        pat: &str,
        line: usize,
    ) -> Result<(u32, u32, u32), String> {
        // The checker compiled every pattern, so a failure here is a gap, not a panic.
        let dfa = vyrn_frontend::regex::compile(pat)
            .map_err(|e| gap(&format!("the pattern `{pat}` ({e})"), line))?;
        let mut table = Vec::with_capacity(dfa.table.len() * 4);
        for n in &dfa.table {
            table.extend_from_slice(&n.to_le_bytes());
        }
        let accept: Vec<u8> = dfa.accepting.iter().map(|a| u8::from(*a)).collect();
        Ok((m.data(&table, 4), m.data(&accept, 1), dfa.start))
    }

    /// A String operator on two stacked operands: `+` concatenates into the `region`'s arena,
    /// and a comparison tests the sign of a byte compare.
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

    /// `s =~ pat` with `s` on the stack; leaves a `Bool`.
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

    /// Emits a unary operator over the operand on the stack.
    fn un_ins(&mut self, b: &mut Frame, op: UnOp, t: &Type, line: usize) -> Result<Type, String> {
        let rt = self.cx.resolve(t);
        match (op, Num::of(&rt)) {
            // `-x` is `x * -1`, so the minimum wraps to itself; `~x` is `x ^ -1`. Both
            // renormalize because a narrow carrier holds more bits than the width.
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
            // A sign-bit flip, not `splat(0.0) - v`, so a zero keeps its sign.
            (UnOp::Neg, None) if rt == Type::F32x4 => {
                b.ins(&Instruction::F32x4Neg);
            }
            (UnOp::Neg, None) if rt == Type::F64x2 => {
                b.ins(&Instruction::F64x2Neg);
            }
            // Wraps: `-Int32.min` is `Int32.min`, as at scalar width.
            (UnOp::Neg, None) if rt == Type::I32x4 => {
                b.ins(&Instruction::I32x4Neg);
            }
            // `v128.not` has no lane width, and a mask lane is all-ones or all-zeros.
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

    /// The width an integer operator runs at: a plain-`Int` operand adopts a sized sibling's
    /// width. The left alone would run `0 - eight` (an `Int32`) in 64 bits, which differs for
    /// `/`, `>>` and comparisons.
    ///
    /// `rt` is `None` where the right operand is a literal, which adapts to its position:
    /// `'A' - 'a'` is `Int64` -32, not an 8-bit 224.
    fn op_width(&self, lt: &Type, rt: Option<&Type>) -> Type {
        let Some(rt) = rt else { return lt.clone() };
        let rt = self.cx.resolve(rt);
        match Num::of(&rt) {
            Some(rn) if rn != Num::PLAIN => rt,
            _ => lt.clone(),
        }
    }

    /// Traps where the row's divisor `d` is zero or its shift amount is outside `0..bits`.
    fn divisor_check(&mut self, b: &mut Frame, n: Num, d: u32, row: &Check) {
        b.ins(&Instruction::LocalGet(d));
        if let Guard::Shift(_, bits) = row.guard {
            // A shift by `>= bits` or a negative amount traps. One unsigned `>=` covers
            // both, because a negative amount reads as a huge unsigned.
            if n.wide() {
                b.ins(&Instruction::I64Const(i64::from(bits)));
                b.ins(&Instruction::I64GeU);
            } else {
                b.ins(&Instruction::I32Const(i32::from(bits)));
                b.ins(&Instruction::I32GeU);
            }
        } else if n.wide() {
            b.ins(&Instruction::I64Eqz);
        } else {
            b.ins(&Instruction::I32Eqz);
        }
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        self.check_trap(b, row, None);
        self.depth -= 1;
        b.ins(&Instruction::End);
    }

    /// Emits a binary operator over two operands on the stack at `opty`. `&&`, `||`, `=~` and
    /// `String` or `Code` operators are absent: they interleave work between the operands.
    ///
    /// `spell` is the left operand's type as written, for a gap's wording. `rows` are the kept
    /// check rows of an integer `/`, `%`, `<<` or `>>`: the divisor's or the amount's, then the
    /// quotient's ([`Guard::NoOverflow`]).
    fn bin_ins(
        &mut self,
        b: &mut Frame,
        op: BinOp,
        opty: &Type,
        spell: &Type,
        rows: [Option<Check>; 2],
        line: usize,
    ) -> Result<Type, String> {
        let lt = opty.clone();
        let l = spell;
        if lt == Type::Bool {
            b.ins(&cmp_i32(op).ok_or_else(|| gap(&format!("`{op:?}` on booleans"), line))?);
            return Ok(Type::Bool);
        }
        // wasm's `f32x4.lt`..`ge` and `eq` are ordered (false on NaN) and `ne` is unordered,
        // the pairing scalar floats use.
        if lt == Type::F32x4 {
            let mask = op.compare().is_some();
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
        if lt == Type::F64x2 {
            let mask = op.compare().is_some();
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
        // Width-agnostic `v128` bit ops are exact on masks, because a mask lane is all-ones or
        // all-zeros.
        if matches!(lt, Type::Mask32x4 | Type::Mask64x2) {
            b.ins(&match op {
                BinOp::BitAnd => Instruction::V128And,
                BinOp::BitOr => Instruction::V128Or,
                BinOp::BitXor => Instruction::V128Xor,
                _ => return unsupported(&format!("`{op:?}` on `{l}`"), line),
            });
            return Ok(lt);
        }
        // No `Div`: wasm has no SIMD integer divide. Comparisons are signed, for the `Int32`
        // lane. Arithmetic wraps, as scalar `Int32` does.
        if lt == Type::I32x4 {
            let mask = op.compare().is_some();
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
                // IEEE: `/0.0` is an infinity or a NaN, never a trap.
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
            return Ok(if op.compare().is_some() {
                Type::Bool
            } else {
                lt
            });
        }
        let Some(n) = Num::of(opty) else {
            return unsupported(&format!("`{op:?}` on `{l}`"), line);
        };
        // Every trap case is checked here rather than left to wasm, so stderr carries our
        // wording, not wasmtime's. The operands go to scratch so the checks can read them.
        if rows.iter().any(Option::is_some) {
            let [first, over] = rows;
            let c = if n.wide() { ValType::I64 } else { ValType::I32 };
            let (d, num) = (self.scratch(b, c, 0), self.scratch(b, c, 1));
            b.ins(&Instruction::LocalSet(d));
            b.ins(&Instruction::LocalSet(num));
            if let Some(first) = first {
                self.divisor_check(b, n, d, &first);
            }
            // The minimum over -1 has no representable answer. `%` is exempt: wasm's `rem_s`
            // gives 0 there, as the interpreter does.
            if let Some(
                over @ Check {
                    guard: Guard::NoOverflow(_, _, bits),
                    ..
                },
            ) = over
            {
                let min = i64::MIN >> (64 - bits);
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
                self.check_trap(b, &over, None);
                self.depth -= 1;
                b.ins(&Instruction::End);
            }
            b.ins(&Instruction::LocalGet(num));
            b.ins(&Instruction::LocalGet(d));
        }
        b.ins(&int_op(op, n).ok_or_else(|| gap(&format!("`{op:?}` on `{opty}`"), line))?);
        if op.compare().is_some() {
            return Ok(Type::Bool);
        }
        // `&`, `|`, `^` and `>>` keep the carrier normalized, but `<<` shifts foreign bits in, so
        // every operator that yields an integer renormalizes as one rule.
        renorm(b, n);
        Ok(lt)
    }

    /// Calls a generator host import ([`Spec::Host`]). `args` are the call's operands.
    fn host(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[(Val, Capability)],
        line: usize,
    ) -> Result<Type, String> {
        let Some(g) = self.cx.gen else {
            return unsupported(&format!("`{name}` outside a generator"), line);
        };
        let code = Type::Named("Code".to_string());
        match (name, args.len()) {
            // `raw(s)` is `@codeText(s)`: one verbatim piece, no origin.
            ("@codeText", 1) | ("raw", 1) => {
                self.core_arg(m, b, name, args, 0, Some(&Type::Str), line)?;
                b.ins(&Instruction::Call(g.text));
                Ok(code)
            }
            ("rawAt", 4) => {
                self.core_arg(m, b, name, args, 0, Some(&Type::Str), line)?;
                self.core_arg(m, b, name, args, 1, Some(&Type::Str), line)?;
                self.core_arg(m, b, name, args, 2, Some(&Type::Int), line)?;
                self.core_arg(m, b, name, args, 3, Some(&Type::Int), line)?;
                b.ins(&Instruction::Call(g.raw_at));
                Ok(code)
            }
            // The host renders and stashes; the guest allocates and fetches ([`Fn_::fetch_str`]).
            ("render", 1) => {
                self.core_arg(m, b, name, args, 0, Some(&code), line)?;
                b.ins(&Instruction::Call(g.render));
                self.fetch_str(b, g);
                Ok(Type::Str)
            }
            // `reflect` leaves the value host-side as atoms; the `next` calls pull them back.
            // The synthesized decoder and the host both walk the type.
            (crate::GEN_REFLECT, 2) => {
                self.core_arg(m, b, name, args, 0, Some(&Type::Int), line)?;
                self.core_arg(m, b, name, args, 1, Some(&Type::Str), line)?;
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
            // The value crosses as a compile-time tag naming the `Val` the host rebuilds, a
            // 64-bit word and a String pointer. The tag goes first, so the type is asked first.
            ("@codeSplice", 2) => {
                let Some((v, _)) = args.first() else {
                    return unsupported(&format!("`{name}` with too few operands"), line);
                };
                let vty = self.cx.resolve(&self.core_ty(v, &Type::Int));
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
                // `bits` then `ptr`: a String is a zero word and the pointer, anything else
                // the word and a null pointer.
                if vty == Type::Str {
                    b.ins(&Instruction::I64Const(0));
                    self.core_arg(m, b, name, args, 0, Some(&Type::Str), line)?;
                } else {
                    self.core_arg(m, b, name, args, 0, Some(&vty), line)?;
                    match &vty {
                        // Lossless; the host formats.
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
                        // A sized integer's carrier is extended for its signedness, so widening
                        // agrees with the tag. A `Code` handle is already the word.
                        _ => {
                            if let Some(n) = Num::of(&vty) {
                                widen(b, n);
                            }
                        }
                    }
                    b.ins(&Instruction::I32Const(0));
                }
                self.core_arg(m, b, name, args, 1, Some(&Type::Int), line)?;
                b.ins(&Instruction::Call(g.splice));
                Ok(code)
            }
            _ => unsupported(&format!("`{name}` at {} operands", args.len()), line),
        }
    }

    /// The guest half of the stash protocol: with the host's length on the stack, allocates a
    /// `String` and fetches the stashed bytes into it. The host writes only into memory the
    /// guest hands it.
    fn fetch_str(&mut self, b: &mut Frame, g: Gen) {
        // The host's length is 64-bit and unbounded by memory; `str_new` judges it.
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

    /// Formats the float on the stack with `std/num`'s `f64Str`, leaving a String. `print` and
    /// `@str` both use it. A `Float32` promotes first, because the interpreter formats it as
    /// `f64`.
    fn f64_str(&mut self, b: &mut Frame, ty: &Type, line: usize) -> Result<(), String> {
        if *ty == Type::Float32 {
            b.ins(&Instruction::F64PromoteF32);
        }
        let f = vyrn_frontend::loader::F64_STR;
        let Some(sig) = self.cx.sigs.get(f) else {
            // `std/num` is injected wherever `print` or `@str` appears; this is a build with no
            // std root.
            return unsupported("formatting a `Float64` with no `std/num` in the link", line);
        };
        b.ins(&Instruction::Call(sig.index));
        Ok(())
    }

    /// Whether `name` is an `extern fn` or a host-boundary name.
    /// Whether `name` is a declared `extern fn`, a host-boundary one included.
    fn is_extern(&self, name: &str) -> bool {
        self.cx.externs.contains_key(name)
    }

    /// Calls an `extern fn` or a host-boundary name; each operand crosses at its parameter's
    /// type.
    fn extern_call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[(Val, Capability)],
        line: usize,
    ) -> Result<Type, String> {
        // The host boundary is not a `vyrn` import: the emitted runtime reads WASI's
        // clock, random and environ calls, and honours `VYRN_FIXED_TIME`/`VYRN_FIXED_SEED`.
        if let Some(sym) = crate::host_boundary_extern(name) {
            // Each reader takes the interned key of its fixing variable.
            let (f, key) = match sym {
                "__vyrn_now_millis" => (self.cx.rt.now_millis, self.cx.rt.fixed_time),
                "__vyrn_monotonic_nanos" => (self.cx.rt.mono_nanos, self.cx.rt.fixed_time),
                _ => (self.cx.rt.random_seed, self.cx.rt.fixed_seed),
            };
            if !args.is_empty() {
                return unsupported(&format!("the call `{name}` at this arity"), line);
            }
            b.ins(&Instruction::I32Const(key as i32));
            // The boundary returns an `i64`; any other declared return would read wrong bytes.
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
        // A call through the `vyrn` import declared from the `extern fn`. A `Bool` and every
        // narrow int already ride an extended `i32`, which is the ABI; only `String` converts.
        if let Some(ext) = self.cx.externs.get(name).cloned() {
            let Some(index) = ext.index else {
                // A declaration with no import.
                return unsupported(&format!("the call `{name}`"), line);
            };
            if ext.params.len() != args.len() {
                return unsupported(&format!("the call `{name}` at this arity"), line);
            }
            for (i, p) in ext.params.iter().enumerate() {
                self.core_arg(m, b, name, args, i, Some(p), line)?;
                if matches!(self.cx.resolve(p), Type::Str) {
                    // (ptr, len): the host decodes UTF-8 and needs the length. One scratch per
                    // argument, or a later string's length would overwrite an earlier one's.
                    let s = self.scratch(b, ValType::I32, 20 + i as u8);
                    b.ins(&Instruction::LocalTee(s))
                        .ins(&Instruction::LocalGet(s));
                    str_len(b);
                    b.ins(&Instruction::I64ExtendI32U);
                }
            }
            b.ins(&Instruction::Call(index));
            // The host returns an `i32` for every narrow width, and an out-of-range JS number
            // would break the carrier invariant.
            if let Some(n) = Num::of(&self.cx.resolve(&ext.ret)) {
                renorm(b, n);
            }
            return Ok(ext.ret.clone());
        }
        unsupported(&format!("the call `{name}`"), line)
    }

    /// Prints the value on the stack by its type `t`.
    fn print_value(&mut self, b: &mut Frame, t: &Type, line: usize) -> Result<(), String> {
        match self.cx.resolve(t) {
            // One `i64` printer for every width, told whether the value is signed.
            ref it if Num::of(it).is_some() => {
                let n = Num::of(it).unwrap();
                widen(b, n);
                b.ins(&Instruction::I32Const(n.signed as i32));
                b.ins(&Instruction::Call(self.cx.rt.print_i64));
            }
            // `f64Str` always returns a fresh allocation (its doc pins that), so it is freed
            // after the write.
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

    /// Renders the value on the stack by its type `t` into a String the caller owns.
    fn str_value(&mut self, b: &mut Frame, t: &Type, line: usize) -> Result<(), String> {
        match self.cx.resolve(t) {
            // Copy, so the result owns its storage: `let t = "\{s}"` is `@str(s)` alone, and
            // passing the pointer through would free one buffer twice.
            Type::Str => self.str_dup(b),
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
            // Copy: `bool_str` returns the interned literal, and an owner of it would append
            // into the data segment. The copy is not in `bool_str`, because `print` would leak it.
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

    /// Emits a `std/mem` primitive from a core row, by [`mem_ins`]'s table.
    fn core_mem(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        prim: &str,
        args: &[(Val, vyrn_frontend::ast::Capability)],
        at: Option<&Type>,
        line: usize,
    ) -> Result<Type, String> {
        // `addr` and `adopt` change the type and nothing else: the operand's
        // wasm value is already the answer.
        if let ("addr" | "adopt", [(v, _)]) = (prim, args) {
            let t = self.core_ty(v, &Type::Int);
            self.core_val(m, b, v, &t, line)?;
            return match (prim, at) {
                ("addr", _) => Ok(INT32),
                (_, Some(t)) => Ok(t.clone()),
                _ => unsupported("an `adopt` the checker did not type", line),
            };
        }
        let (Some(decl), Some((under, after))) =
            (self.cx.mem.get(prim).copied(), mem_ins(self.cx, prim))
        else {
            return unsupported(&format!("the `std/mem` primitive `{prim}`"), line);
        };
        if args.len() != decl.params.len() {
            return unsupported(&format!("`std/mem.{prim}` at this arity"), line);
        }
        for i in &under {
            b.ins(i);
        }
        for ((v, _), p) in args.iter().zip(&decl.params) {
            self.core_val(m, b, v, &p.ty, line)?;
        }
        for i in &after {
            b.ins(i);
        }
        Ok(decl.ret.clone())
    }

    /// Writes one log line, `[LEVEL] name: message\n`, to the configured descriptor, with
    /// `name` and `message` on the stack. Only a call whose level clears the threshold gets
    /// here. Five `write_all`s avoid allocating a joined string.
    fn log_write(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        level: &str,
        line: usize,
    ) -> Result<(), String> {
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
        // WASI's stdout (1) or stderr (2), or the descriptor `_start` opened for a file sink.
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

    /// Pushes the out-pointer for an aggregate result before the arguments; `None` for a
    /// non-aggregate. It is the consumer's storage when that holds this type, else a new slot.
    /// A result left in a parameter ([`Sig::in_place`]) pushes nothing here: [`Fn_::moved_in`]
    /// passes the destination as that argument. Pair with [`Fn_::out_ptr_back`] after the call.
    fn out_ptr(
        &mut self,
        b: &mut Frame,
        sig: &Sig,
        hint: Option<(Dest, Type)>,
    ) -> Option<(Dest, bool)> {
        let l = sig.ret.agg()?;
        let (d, used) = match hint {
            Some((d, t)) if self.cx.shape(&t) == self.cx.shape(&sig.ret_ty) => (d, true),
            _ => (Dest::Slot(b.alloc(l.size, l.align)), false),
        };
        if sig.in_place.is_none() {
            d.addr(b, 0);
        }
        Some((d, used))
    }

    /// Passes argument `i` of a call to `sig` through `push`. The argument a result is left in
    /// ([`Sig::in_place`]) is moved into the destination `dest` first and passed as it, unless
    /// `home` says it is already there.
    fn moved_in(
        &mut self,
        b: &mut Frame,
        sig: &Sig,
        dest: Option<(Dest, bool)>,
        i: usize,
        home: bool,
        push: impl FnOnce(&mut Self, &mut Frame) -> Result<(), String>,
    ) -> Result<(), String> {
        let (Some((d, _)), Some(l)) = (dest.filter(|_| sig.in_place == Some(i)), sig.ret.agg())
        else {
            return push(self, b);
        };
        if !home {
            d.addr(b, 0);
            push(self, b)?;
            b.copy(l.size);
        }
        d.addr(b, 0);
        Ok(())
    }

    /// Pushes the out-pointer again as the result and sets `dest_used` if it was the
    /// consumer's storage. Pairs with [`Fn_::out_ptr`].
    fn out_ptr_back(&mut self, b: &mut Frame, dest: Option<(Dest, bool)>) {
        if let Some((d, used)) = dest {
            d.addr(b, 0);
            self.dest_used = used;
        }
    }

    /// The instance of generic `f` that `arg_tys` solves.
    fn generic_sig(
        &mut self,
        m: &mut Module,
        f: &'p Function,
        arg_tys: &[Type],
        line: usize,
    ) -> Result<Sig, String> {
        let (subst, solved) = crate::solve_type_args(
            &f.type_params,
            &f.params.iter().map(|p| p.ty.clone()).collect::<Vec<_>>(),
            arg_tys,
        );
        let mut type_args = Vec::new();
        for (tp, got) in f.type_params.iter().zip(solved) {
            match got {
                Some(t) => type_args.push(t),
                // Defaulting to `Unit` would drop a wasm parameter and change the function.
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

    /// Emits a call to `sig`. Each of `args` is pushed from its place and crosses at the
    /// parameter's type.
    fn emit_call_with(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        sig: &Sig,
        args: &[(Place, Type)],
        hint: Option<(Dest, Type)>,
    ) -> Result<Type, String> {
        // A `fn` type reads every argument, so no target takes a `consume` parameter
        // (`Checker::reads_every_param`) and no result is left in one.
        debug_assert!(sig.in_place.is_none(), "a stored `fn` value consumes");
        let dest = self.out_ptr(b, sig, hint);
        for ((place, ty), p) in args.iter().zip(&sig.params) {
            self.push_place(b, *place, ty, 0)?;
            self.coerce(m, b, ty, p, 0)?;
        }
        b.ins(&Instruction::Call(sig.index));
        self.out_ptr_back(b, dest);
        // The declared type, not resolved: a caller solves generics against it, and
        // `Pair<Int64, Int64>` as a bare record would not match `Pair<A, B>`.
        Ok(sig.ret_ty.clone())
    }

    /// Spills the scalar in local `l` to a slot and pushes its address for a `modify`
    /// parameter. [`reload`] writes it back into `l` after the call.
    fn spill(&self, b: &mut Frame, l: u32, ty: &Type, line: usize) -> Result<Spill, String> {
        let Repr::Scalar(_) = self.cx.repr(ty, line)? else {
            return unsupported("a `modify` argument in a local", line);
        };
        let l2 = self.cx.layout(ty, line)?;
        let off = b.alloc(l2.size, l2.align);
        b.slot(off);
        b.ins(&Instruction::LocalGet(l));
        b.ins(&self.cx.store(ty));
        b.slot(off);
        Ok((off, l, self.cx.load(ty, 0)))
    }

    /// This body's lambda literal under a closure row's key
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

    /// Lifts a lambda literal to a top-level function `(captures.., params..) -> ret`, indexed
    /// here and lowered when the queue reaches it. Captures are ordinary read parameters, so
    /// they are by-value snapshots.
    #[allow(clippy::too_many_arguments)]
    fn lift_lambda(
        &mut self,
        m: &mut Module,
        at: &'p Expr,
        ptys: &[Type],
        expected_ret: &Type,
        caps: Option<&[(String, Type)]>,
        line: usize,
    ) -> Result<FnTarget, String> {
        assert_ne!(
            at.id(),
            NodeId::NONE,
            "a lambda lifted from an unnumbered tree"
        );
        let Expr::Lambda {
            params,
            body,
            line: at_line,
            col: at_col,
            id: _,
        } = at
        else {
            return unsupported("a lambda lifted from another expression", line);
        };
        if params.len() != ptys.len() {
            return unsupported("a lambda with the wrong number of parameters", line);
        }
        // The free locals in first-seen order, from the shared walk: the capture list is part
        // of the lifted signature. A core row names its captures, because the frame lifts the
        // literal before its first statement, where no capture is in scope.
        let (cap_names, mut cap_tys) = match caps {
            Some(c) => (
                c.iter().map(|(n, _)| n.clone()).collect(),
                c.iter().map(|(_, t)| self.cx.sub(t).into_owned()).collect(),
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
            // A captured `fn`-typed parameter lives in `fn_binds`, not a slot. It is captured
            // as a `fn` value, which the site builds with `fnval_binding` and the lifted
            // function calls through the dispatcher.
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
            cap_tys.push(self.cx.sub(&t).into_owned());
        }
        let ret = expected_ret.clone();
        // The queue walks the literal's own nodes, so every answer is about a program node.
        let queued = match body {
            LambdaBody::Block(b) => Body::Block(b),
            LambdaBody::Expr(_) => Body::Value,
        };
        let mut sf = f_shell(line);
        sf.name = format!("{LAMBDA} {}", self.owner);
        sf.params = cap_names
            .iter()
            .zip(&cap_tys)
            .map(|(n, t)| Param::synth(n, t.clone()))
            .chain(params.iter().zip(ptys).map(|(n, t)| Param {
                id: Id::NEW,
                name: n.name.clone(),
                capability: Capability::Read,
                ty: t.clone(),
                line: n.line,
                col: n.col,
            }))
            .collect();
        sf.ret = ret.clone();
        // Keyed by node, shape and substitution: a literal in a generic body lifts once per
        // instantiation, even when the type parameter appears only in a statement.
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
        let key = Key::Lambda(at.id(), shape, under);
        let sig = self.cx.enqueue(
            m,
            key,
            Rc::new(sf),
            queued,
            self.cx.subst.clone(),
            HashMap::new(),
            vyrn_lower::core::lambda_spelling(&self.core_key, *at_line, *at_col),
        )?;
        Ok(FnTarget {
            sig,
            ncaps: cap_names.len(),
        })
    }

    /// Returns the tag of a stored function value, registering it once per signature and target.
    /// The tag indexes the module-global list, so it means the same in every body; the
    /// dispatcher filters by signature and does not renumber.
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

    /// Lays out a capture block: the captures packed by value, in order.
    fn cap_block(&self, cap_tys: &[Type]) -> Result<Rc<Layout>, String> {
        Shape::Struct(cap_tys.iter().map(|t| self.cx.shape(t)).collect())
            .layout()
            .map(Rc::new)
            .map_err(|e| format!("direct backend: layout of a capture block: {e}"))
    }

    /// Writes a stored function value's tag and payload into `dest`.
    /// `caps` are the captures in the target's order.
    #[allow(clippy::too_many_arguments)]
    fn fnval_into(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        sig_ty: &Type,
        target: FnTarget,
        caps: &[Val],
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
                // The block owns its heap: a heap capture read out of a place is
                // duplicated, so the release twin can walk it and the captured
                // binding still releases its own value at block exit. A capture the
                // source mints is already the block's, and duplicating it orphans it:
                // a `fn`-typed parameter in a specialization ([`Fn_::fnval_binding`]).
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
                        b.ins(&self.cx.store(ty));
                    }
                    Repr::Agg(fl) => {
                        b.copy(fl.size);
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
        b.ins(&Instruction::I64Store(at(0)));
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
        b.ins(&Instruction::I64Store(at(0)));
        Ok(())
    }

    /// Returns the target a stored lambda calls.
    fn lift_stored(
        &mut self,
        m: &mut Module,
        e: &'p Expr,
        sig_ty: &Type,
    ) -> Result<FnTarget, String> {
        let Expr::Lambda { line, .. } = e else {
            return unsupported("a function value from a non-lambda", Expr::line(e));
        };
        let Type::Fn(ptys, ret) = sig_ty else {
            return unsupported("a lambda in a non-function position", *line);
        };
        self.lift_lambda(m, e, ptys, ret, None, *line)
    }

    /// Returns the dispatcher for one signature, reserving its index on first use.
    /// Its shape is `fn(fv: <sig>, a0: P0, ..) -> R`: a [`FnTarget`] with `ncaps == 1`,
    /// whose one capture is the fn value itself.
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
            in_place: None,
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

    /// Emits `.length` or `.byteLength`, consuming the receiver already on the stack.
    /// A fixed array's length is a constant, so its address is dropped.
    fn length_of(
        &mut self,
        b: &mut Frame,
        base: &Type,
        field: &str,
        line: usize,
    ) -> Result<Option<Type>, String> {
        if length_ty(field, &self.cx.resolve(base)).is_none() {
            return Ok(None);
        }
        match (field, self.cx.resolve(base)) {
            ("byteLength", Type::Str) => {
                str_len(b);
                b.ins(&Instruction::I64ExtendI32U);
            }
            ("length", Type::Array(_)) => {
                let l = self.cx.layout(base, line)?;
                b.ins(&Instruction::I64Load(at(l.fields[1])));
            }
            ("length", Type::ArrayN(_, n)) => {
                b.ins(&Instruction::Drop);
                b.ins(&Instruction::I64Const(n as i64));
            }
            // A `SmallArray` keeps its length in field 0, where an Array keeps its data.
            ("length", Type::SmallArray(..)) => {
                let l = self.cx.layout(base, line)?;
                b.ins(&Instruction::I64Load(at(l.fields[0])));
            }
            // A Map keeps the shared length of its two buffers in field 2, not 1.
            ("length", Type::Map(..)) => {
                let l = self.cx.layout(base, line)?;
                b.ins(&Instruction::I64Load(at(l.fields[2])));
            }
            _ => return Ok(None),
        }
        Ok(Some(Type::Int))
    }
}

/// An indexable value's parts in locals: where its elements start, how many there are, and
/// what one is. The parts are a snapshot, so a `for` that grows its own array keeps walking
/// the buffer it started on; this must agree with the interpreter, which iterates a copy.
#[derive(Clone)]
struct Walk {
    /// `i32` local: the address of element 0.
    data: u32,
    /// `i64` local: the element count.
    len: u32,
    elem: Type,
    stride: u32,
    /// A `String`'s elements are bytes widened to `Int`, not stored values.
    byte: bool,
}

impl<'p> Fn_<'_, 'p> {
    /// A layout's size is rounded up to its alignment, so it is a stride.
    fn stride(&self, elem: &Type, line: usize) -> Result<u32, String> {
        Ok(self.cx.layout(elem, line)?.size)
    }

    /// Returns the size in bytes of `n` elements of `elem`, for every count-times-stride
    /// allocation and `memory.copy` length. The bound is [`vyrn_frontend::trap::LENGTH_LIMIT`],
    /// not `u32::MAX`: every consumer is an `i32`, and a product in `[2^31, 2^32)` goes
    /// negative, so `malloc` returns a small block that `memory.copy` then overruns.
    fn extent(&self, elem: &Type, n: usize, line: usize) -> Result<u32, String> {
        let bytes = self.stride(elem, line)? as u64 * n as u64;
        if bytes > u64::from(vyrn_frontend::trap::LENGTH_LIMIT) {
            return Err(too_big(&format!("{n} × `{elem}`"), bytes, line));
        }
        Ok(bytes as u32)
    }

    /// Every `Stream<T>` shares one six-word header layout, whatever `T` is.
    fn stream_layout(&self, line: usize) -> Result<Rc<Layout>, String> {
        self.cx.layout(&Type::Stream(Box::new(Type::Int)), line)
    }

    /// Emits `fromArray(xs)`: the array's words into a buffer-tagged header.
    fn stream_from_array(
        &mut self,
        b: &mut Frame,
        inner: &Type,
        line: usize,
    ) -> Result<Type, String> {
        let arr = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(arr));
        let al = self
            .cx
            .layout(&Type::Array(Box::new(inner.clone())), line)?;
        let sl = self.stream_layout(line)?;
        let off = b.alloc(sl.size, sl.align);
        b.slot(off + sl.fields[0]);
        b.ins(&Instruction::LocalGet(arr));
        b.ins(&Instruction::I32Load(word_at(al.fields[0])));
        b.ins(&Instruction::I32Store(word()));
        b.slot(off + sl.fields[1]);
        b.ins(&Instruction::LocalGet(arr));
        b.ins(&Instruction::I64Load(at(al.fields[1])));
        b.ins(&Instruction::I64Store(at(0)));
        // Tag -1 marks a buffer for the stream's whole life; a buffer leaves words 3-5 zero.
        for (i, v) in [(2usize, -1i64), (3, 0), (4, 0), (5, 0)] {
            b.slot(off + sl.fields[i]);
            b.ins(&Instruction::I64Const(v));
            b.ins(&Instruction::I64Store(at(0)));
        }
        b.slot(off);
        Ok(Type::Stream(Box::new(inner.clone())))
    }

    /// Emits `fromStep(slot, gen, step)` into a header without allocating: `std/stream`
    /// mints the cursor.
    fn stream_from_step(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[(Val, Capability)],
        line: usize,
    ) -> Result<Type, String> {
        // Arguments evaluate left to right, as in the interpreter, so effects and
        // traps keep their order; the cursor words wait in locals.
        self.core_arg(m, b, "fromStep", args, 0, Some(&Type::Int), line)?;
        let c0 = b.local(ValType::I64);
        b.ins(&Instruction::LocalSet(c0));
        self.core_arg(m, b, "fromStep", args, 1, Some(&Type::Int), line)?;
        let c1 = b.local(ValType::I64);
        b.ins(&Instruction::LocalSet(c1));
        let fty = self.core_arg(m, b, "fromStep", args, 2, None, line)?;
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
        // The loop rebuilds this signature from the element type alone; a step under
        // any other spelling would dispatch through a table it is not in.
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
        b.ins(&Instruction::I64Store(at(0)));
        b.slot(off + sl.fields[2]);
        b.ins(&Instruction::LocalGet(fv));
        b.copy(fl.size);
        b.slot(off + sl.fields[4]);
        b.ins(&Instruction::LocalGet(c0));
        b.ins(&Instruction::I64Store(at(0)));
        b.slot(off + sl.fields[5]);
        b.ins(&Instruction::LocalGet(c1));
        b.ins(&Instruction::I64Store(at(0)));
        b.slot(off);
        Ok(Type::Stream(Box::new(elem)))
    }

    /// The first word of every boxed stream. A program can spell any address, so
    /// `unboxStream` and `pullAt` check it, and `unboxStream` clears it before freeing:
    /// a second `unboxStream` of one address traps.
    const BOX_MAGIC: i64 = 3735928559;

    /// Checks the box whose `Int64` address is `addr`, or traps; returns the local holding it.
    fn stream_box_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        addr: &Val,
        line: usize,
    ) -> Result<u32, String> {
        let w = b.local(ValType::I64);
        let a = b.local(ValType::I32);
        self.core_val(m, b, addr, &Type::Int, line)?;
        // The address is checked as an `Int64` before it is wrapped: a nonzero
        // 32-bit address is `1..=u32::MAX`, so `w - 1 <u u32::MAX`.
        b.ins(&Instruction::LocalTee(w));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Sub);
        b.ins(&Instruction::I64Const(u32::MAX as i64));
        b.ins(&Instruction::I64GeU);
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
        b.ins(&Instruction::LocalGet(w));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::LocalTee(a));
        b.ins(&Instruction::I64Load(at(0)));
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

    /// Emits `boxStream(s)`: moves the stream into a heap box and returns its address.
    /// A `Stream<T>` cannot be a field, so a lazy combinator's source lives here and
    /// `std/stream` keeps the address in its cursor slot.
    fn stream_box(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        args: &[(Val, Capability)],
        line: usize,
    ) -> Result<Type, String> {
        let sl = self.stream_layout(line)?;
        let p = b.local(ValType::I32);
        b.ins(&Instruction::I64Const((8 + sl.size) as i64));
        b.ins(&Instruction::Call(self.cx.rt.malloc));
        b.ins(&Instruction::LocalTee(p));
        b.ins(&Instruction::I64Const(Self::BOX_MAGIC));
        b.ins(&Instruction::I64Store(at(0)));
        b.ins(&Instruction::LocalGet(p));
        b.ins(&Instruction::I32Const(8));
        b.ins(&Instruction::I32Add);
        let got = self.core_arg(m, b, "boxStream", args, 0, None, line)?;
        if !matches!(self.cx.resolve(&got), Type::Stream(_)) {
            return unsupported(&format!("`boxStream` of `{got}`"), line);
        }
        b.copy(sl.size);
        b.ins(&Instruction::LocalGet(p));
        b.ins(&Instruction::I64ExtendI32U);
        Ok(Type::Int)
    }

    /// Emits `unboxStream(a)`: takes the stream out and frees the box.
    fn stream_unbox(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        elem: &Type,
        addr: &Val,
        line: usize,
    ) -> Result<Type, String> {
        let sl = self.stream_layout(line)?;
        let a = self.stream_box_at(m, b, addr, line)?;
        let off = b.alloc(sl.size, sl.align);
        b.slot(off);
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I32Const(8));
        b.ins(&Instruction::I32Add);
        b.copy(sl.size);
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I64Const(0));
        b.ins(&Instruction::I64Store(at(0)));
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::Call(self.cx.rt.free));
        b.slot(off);
        Ok(Type::Stream(Box::new(elem.clone())))
    }

    /// Emits `pullAt(a)`: one element from the stream in that box, as an `Option`.
    /// The element type comes from the annotation, because an address is only an `Int64`.
    fn stream_pull_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        elem: &Type,
        addr: &Val,
        line: usize,
    ) -> Result<Type, String> {
        let opt = Type::option(elem.clone());
        let Repr::Agg(ol) = self.cx.repr(&opt, line)? else {
            return unsupported("an Option that is not an aggregate", line);
        };
        let a = self.stream_box_at(m, b, addr, line)?;
        let src = b.local(ValType::I32);
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I32Const(8));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalSet(src));

        let r = self.cx.repr(&elem, line)?;
        let place = self.place_for(b, &r, line)?;
        let has = self.stream_next(m, b, src, place, &elem, line)?;
        let ooff = b.alloc(ol.size, ol.align);
        b.slot(ooff);
        b.ins(&Instruction::LocalGet(has));
        b.ins(&Instruction::I64ExtendI32U);
        b.ins(&Instruction::I64Store(at(0)));
        for f in &ol.fields[1..] {
            b.slot(ooff + f);
            b.ins(&Instruction::I64Const(0));
            b.ins(&Instruction::I64Store(at(0)));
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

    /// Writes a value of type `t`, held in `place`, into a sum's payload words at `w0`.
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
            b.copy(16);
            return Ok(());
        }
        let push = |b: &mut Frame| -> Result<(), String> {
            match place {
                Place::Local(l) => b.ins(&Instruction::LocalGet(l)),
                _ => {
                    place
                        .addr(b, 0)
                        .ok_or_else(|| gap("a payload with no address", line))?;
                    b.ins(&self.cx.load(t, 0))
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
        b.ins(&Instruction::I64Store(at(0)));
        Ok(())
    }

    /// Releases a stream. A buffer frees the array data it was given. A producer calls
    /// its step once with `closing` true; the step frees its `std/stream` slot and, if it
    /// wraps a source, unboxes and `close`s it, so a chain unwinds as recursion in Vyrn.
    fn stream_release(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        place: Place,
        elem: &Type,
        line: usize,
    ) -> Result<(), String> {
        // A `Place::Local` holding a stream holds its address, so `place.addr` is wrong here.
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
        // A buffer owns the array data it was handed, and the elements its
        // cursor has not reached: a pull hands each element to its puller.
        if self.cx.world.ownership.proto.release_kind(elem).is_some() {
            let stride = self.stride(elem, line)?;
            let (n, data) = (b.local(ValType::I32), b.local(ValType::I32));
            b.ins(&Instruction::LocalGet(a));
            b.ins(&Instruction::I64Load(at(sl.fields[1])));
            b.ins(&Instruction::LocalGet(a));
            b.ins(&Instruction::I64Load(at(sl.fields[4])));
            b.ins(&Instruction::I64Sub);
            b.ins(&Instruction::I32WrapI64);
            b.ins(&Instruction::LocalSet(n));
            b.ins(&Instruction::LocalGet(a));
            b.ins(&Instruction::I32Load(word_at(sl.fields[0])));
            load_wrapped(b, a, sl.fields[4]);
            b.ins(&Instruction::I32Const(stride as i32));
            b.ins(&Instruction::I32Mul);
            b.ins(&Instruction::I32Add);
            b.ins(&Instruction::LocalSet(data));
            self.each(m, b, true, data, n, stride, elem, line)?;
        }
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
        // A step's tag is registered under `normalize_fn_sig`, so the key must be
        // normalized too; a raw key misses the table.
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
        // Frees the step's capture block: the stream owns its fn value. An empty
        // capture set is payload 0, which `free` refuses.
        b.ins(&Instruction::LocalGet(a));
        b.ins(&Instruction::I32Load(word_at(sl.fields[3])));
        b.ins(&Instruction::Call(self.cx.rt.free));
        self.depth -= 1;
        b.ins(&Instruction::End);
        Ok(())
    }

    /// Reads one element from the stream at `s` into `place`; returns an `i32` local, 1 if
    /// there was one. Both `for ... in` and `pull` read through here. It writes a place, not
    /// an `Option<T>`, because a wide payload is boxed and a `for` must not allocate.
    /// Neither arm branches out of itself, so `self.depth` stays right for a caller's `break`.
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
        // A negative tag is a buffer.
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
        load_wrapped(b, s, sl.fields[4]);
        b.ins(&Instruction::I32Const(stride as i32));
        b.ins(&Instruction::I32Mul);
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalSet(addr));
        match (place, &r) {
            (Place::Local(l), _) => {
                b.ins(&Instruction::LocalGet(addr));
                b.ins(&self.cx.load(elem, 0));
                b.ins(&Instruction::LocalSet(l));
            }
            (Place::Slot(off), Repr::Agg(el)) => {
                b.slot(off);
                b.ins(&Instruction::LocalGet(addr));
                b.copy(el.size);
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
        // A step's tag is registered under `normalize_fn_sig`, so the key must be
        // normalized too; a raw key misses the table.
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
                b.copy(el.size);
            }
            _ => return unsupported("a stream element of this shape", line),
        }
        // A boxed payload was copied out above, so free the box but not its
        // contents, which `place` owns (`rel_word`'s boxed arm without the walk).
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

    /// Takes the indexable value on the stack apart into fresh locals, not scratch:
    /// a `for` holds its [`Walk`] across its body, and nested walks must not share.
    fn walk(&mut self, b: &mut Frame, ty: &Type, line: usize) -> Result<Walk, String> {
        let addr = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(addr));
        let len = b.local(ValType::I64);
        Ok(match self.cx.resolve(ty) {
            Type::Array(inner) => {
                let l = self.cx.layout(ty, line)?;
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
            // A fixed array's slot address is element 0.
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
            // A `SmallArray` has an inline buffer and two live states, and its first
            // field is a length, not a pointer. The state branch happens here, once,
            // so every element access downstream sees an ordinary base and count.
            Type::SmallArray(inner, n) => {
                let ty = self.cx.resolve(ty);
                let l = self.cx.layout(&ty, line)?;
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

    /// Writes a `panic` line (`error: `, the message `msg` pushes, the site if `at` names
    /// one) and traps. [`Fn_::core_call`] calls it; the `unreachable` after it is the
    /// core's [`St::Trap`]. The message is evaluated before any byte is written, as in the
    /// other engines, and waits in a local because `write_all` takes three operands.
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

    /// Traps at a trap-table row: pushes the row's number and its
    /// value, and `std/runtime`'s `trapAt` supplies the wording. A function with a trap
    /// site parks the pair and branches to it, so a check costs a branch, not a call.
    /// The caller has counted its own `if` in `depth`, so the trap block is `depth - 1`.
    /// `val` is an `i64` local; a row without one passes zero, which `trapAt` never reads.
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

    /// The check a construct runs for `c`: the row when it is kept, `None` when a pass proved
    /// it. The oracle ([`vyrn_lower::check::Mode::Count`]) counts every row here and runs a
    /// proved one too, which [`Fn_::check_trap`] turns into a failed run.
    fn row(&mut self, b: &mut Frame, c: Check) -> Option<Check> {
        let Some(o) = &self.cx.oracle else {
            return (c.verdict == Verdict::Kept).then_some(c);
        };
        let key = (self.core_key.clone(), c.site.line, c.site.ordinal);
        let mut labels = o.labels.borrow_mut();
        let id = labels.len() as i32;
        let verdict = match c.verdict {
            Verdict::Kept => "kept",
            Verdict::Proved => "proved",
        };
        labels.push(format!(
            "{}\t{}\t{}\t{}\t{verdict}",
            key.0,
            key.1,
            key.2,
            c.rule.census()
        ));
        o.ids.borrow_mut().insert(key, id);
        b.ins(&Instruction::I32Const(id));
        b.ins(&Instruction::Call(o.hit));
        Some(c)
    }

    /// The `where` check of record name `n` of type `decl`: its constructor
    /// when the check is kept; nothing when a pass proved it, and under the
    /// oracle the predicate over the record's fields, whose failure fails the
    /// run.
    #[allow(clippy::too_many_arguments)]
    fn core_rule_check(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        decl: &TypeDecl,
        n: Name,
        line: usize,
    ) -> Result<(), String> {
        let row = core_check(
            &mut self.core_w,
            line,
            |g| matches!(g, Guard::Rule(m) if *m == n),
        )?;
        let Some(row) = self.row(b, row) else {
            return Ok(());
        };
        if row.verdict == Verdict::Kept {
            let ty = Type::Named(decl.name.clone());
            self.core_val(m, b, &Val::Name(n), &ty, line)?;
            self.emit_validation(b, decl, line)?;
            b.ins(&Instruction::Drop);
            return Ok(());
        }
        let pred = vyrn_frontend::ctor::pred_name(&decl.name);
        let Some(index) = self.cx.sigs.get(&pred).map(|s| s.index) else {
            return unsupported(&format!("the predicate `{pred}` outside the link"), line);
        };
        // Each field as a `read` argument: a layout crosses as its address.
        for (f, ty, _) in vyrn_frontend::types::predicate_binds(decl) {
            let at =
                vyrn_frontend::core::Place::Field(Box::new(vyrn_frontend::core::Place::Name(n)), f);
            if let Repr::Agg(_) = self.cx.repr(&ty, line)? {
                let (_, off) = self.core_addr(m, b, &at, line)?;
                self.core_step(b, off);
            } else {
                self.core_read(m, b, &at, line)?;
            }
        }
        b.ins(&Instruction::Call(index));
        b.ins(&Instruction::I32Eqz);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        self.check_trap(b, &row, None);
        self.depth -= 1;
        b.ins(&Instruction::End);
        Ok(())
    }

    /// The failing branch of `row`'s check: its trap, or under the oracle the failure of a
    /// proved row.
    fn check_trap(&mut self, b: &mut Frame, row: &Check, val: Option<u32>) {
        match &self.cx.oracle {
            Some(o) if row.verdict == Verdict::Proved => {
                let key = (self.core_key.clone(), row.site.line, row.site.ordinal);
                b.ins(&Instruction::I32Const(o.ids.borrow()[&key]));
                b.ins(&Instruction::Call(o.fail));
            }
            _ => match row.rule {
                Raises::Row(rule) => self.trap_row(b, rule, val),
                // A kept `where` check traps inside its type's constructor
                // ([`Fn_::core_rule_check`]).
                Raises::Where => {
                    b.ins(&Instruction::Unreachable);
                }
            },
        }
    }

    /// Traps unless `idx` is in `0..len`; the compare is unsigned, so it catches a negative.
    fn bounds_check(&mut self, b: &mut Frame, w: &Walk, idx: u32, row: &Check) {
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::LocalGet(w.len));
        b.ins(&Instruction::I64GeU);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        self.check_trap(b, row, Some(idx));
        self.depth -= 1;
        b.ins(&Instruction::End);
    }

    /// Traps unless all of `idx..idx+span-1` are in `0..len`, with one branch per vector.
    /// `span` is 4 for the four-lane shapes and 2 for `@f64x2`. It needs two compares:
    /// `idx + span` wraps for a huge `idx`, but `len - span` cannot, because `len >= 0`.
    fn bounds_check_span(&mut self, b: &mut Frame, w: &Walk, idx: u32, row: &Check) {
        let Guard::Span(_, _, span) = row.guard else {
            unreachable!("`core_check` matched a span row")
        };
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
        // Report the first lane out of range: `idx` if negative, else `idx + span - 1`.
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
        self.check_trap(b, row, Some(at));
        self.depth -= 1;
        b.ins(&Instruction::End);
    }

    /// Turns the element address on the stack into the element: a value for a scalar,
    /// the address unchanged for an aggregate.
    fn load_elem(&mut self, b: &mut Frame, w: &Walk, line: usize) -> Result<(), String> {
        if w.byte {
            b.ins(&Instruction::I32Load8U(byte()));
            b.ins(&Instruction::I64ExtendI32U);
            return Ok(());
        }
        match self.cx.repr(&w.elem, line)? {
            Repr::Scalar(_) => {
                b.ins(&self.cx.load(&w.elem, 0));
            }
            Repr::Agg(_) => {}
            Repr::Unit => return unsupported("an array of Unit", line),
        }
        Ok(())
    }

    /// Writes a literal's elements one after another from `dest`. An aggregate element
    /// is built in place, so a nested literal costs no frame.
    fn fixed_elems(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        elem: &Type,
        elems: &[Val],
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
                    b.ins(&self.cx.store(elem));
                }
                Repr::Agg(_) => self.agg_part(m, b, elems, i, dest.at(at), stride, elem, line)?,
                Repr::Unit => return unsupported("an array of Unit", line),
            }
        }
        Ok(())
    }

    /// Writes a record literal's fields at their layout offsets. `order[i]` is the part
    /// that fills the `i`th declared field; the caller joins by name on
    /// [`vyrn_frontend::core::Ctor::Record`].
    #[allow(clippy::too_many_arguments)]
    fn record_into(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        decl: &[Field],
        l: &Layout,
        order: &[usize],
        parts: &[Val],
        line: usize,
    ) -> Result<(), String> {
        for (i, f) in decl.iter().enumerate() {
            match self.cx.repr(&f.ty, line)? {
                Repr::Scalar(_) => {
                    dest.addr(b, l.fields[i]);
                    self.part(m, b, parts, order[i], &f.ty, line)?;
                    b.ins(&self.cx.store(&f.ty));
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

    /// Pushes part `i` of a literal at type `want`.
    fn part(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        parts: &[Val],
        i: usize,
        want: &Type,
        line: usize,
    ) -> Result<(), String> {
        self.core_val(m, b, &parts[i], want, line)
    }

    /// Leaves aggregate part `i` at `dest`, the parent's storage at the part's offset.
    /// A part its own row already wrote there ([`Fn_::core_part_at`]) is skipped.
    #[allow(clippy::too_many_arguments)]
    fn agg_part(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        parts: &[Val],
        i: usize,
        dest: Dest,
        size: u32,
        ty: &Type,
        line: usize,
    ) -> Result<(), String> {
        if self.part_built(&parts[i]).is_some() {
            return Ok(());
        }
        dest.addr(b, 0);
        self.core_val(m, b, &parts[i], ty, line)?;
        agg_landed(b, size, false);
        Ok(())
    }

    /// Builds `[a, b, c]` in an `Array<T>` position straight on the heap and writes the
    /// `{ptr, len, cap}` triple at `dest`, with `len == cap == n` as in [`Fn_::heapify`].
    /// The empty literal has a null `data`. `taken` is a buffer a part already took
    /// ([`Fn_::core_part_dest`]).
    #[allow(clippy::too_many_arguments)]
    fn array_lit_heap(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        inner: &Type,
        elems: &[Val],
        taken: Option<u32>,
        line: usize,
        used: bool,
    ) -> Result<Type, String> {
        let ty = Type::Array(Box::new(inner.clone()));
        let l = self.cx.layout(&ty, line)?;
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
            b.ins(&Instruction::I64Store(at(0)));
        }
        dest.addr(b, 0);
        self.dest_used = used;
        Ok(ty)
    }

    fn heap_buf(&mut self, b: &mut Frame, bytes: u32) -> u32 {
        let buf = b.local(ValType::I32);
        b.ins(&Instruction::I64Const(bytes.max(1) as i64));
        b.ins(&Instruction::Call(self.cx.rt.malloc));
        b.ins(&Instruction::LocalSet(buf));
        buf
    }

    /// Converts `[N x T]` to a `{ptr, len, cap}` triple over a heap copy of the elements.
    /// It copies because the triple outlives the frame and `push` reallocates its buffer.
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
        b.copy(bytes as u32);
        let l = self.cx.layout(want, line)?;
        let off = b.alloc(l.size, l.align);
        b.slot(off + l.fields[0]);
        b.ins(&Instruction::LocalGet(buf));
        b.ins(&Instruction::I32Store(word()));
        // A literal's buffer is exactly full, so the first `push` grows it.
        for f in [l.fields[1], l.fields[2]] {
            b.slot(off + f);
            b.ins(&Instruction::I64Const(n as i64));
            b.ins(&Instruction::I64Store(at(0)));
        }
        b.slot(off);
        Ok(())
    }

    /// Parks the receiver of a `std/runtime` array operation: returns the element type,
    /// the header layout, the stride and the address local. The address is both `dst`
    /// and `src`, which holds because every runtime array function reads all of `src`
    /// before it stores. A separate destination slot halves the speed of store-heavy code.
    fn arr_recv(
        &mut self,
        b: &mut Frame,
        aty: &Type,
        verb: &str,
        line: usize,
    ) -> Result<(Type, Rc<Layout>, i32, u32), String> {
        let Type::Array(elem) = self.cx.resolve(aty) else {
            return unsupported(&format!("`{verb}` on `{aty}`"), line);
        };
        let l = self.cx.layout(aty, line)?;
        let stride = self.stride(&elem, line)? as i32;
        let src = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(src));
        Ok((*elem, l, stride, src))
    }

    /// Emits a `std/runtime` array operation on the receiver address on the stack, and
    /// leaves that address: the runtime writes the new triple in place ([`Fn_::arr_recv`]).
    /// `rest` holds the second argument; `clear` has none. An operand goes to a local first, so
    /// no call operand is on the stack while user code runs.
    ///
    /// `push` stores the element here, because the runtime knows the stride but not the type.
    /// The old buffer is freed only after the element is stored: the element expression may
    /// read the array through the caller's header, which still names the old buffer
    /// (`std/hash`'s SHA-1 `w.push(rot1(w[t - 3] ^ ...))` does).
    fn arr_rebuild(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        aty: &Type,
        rest: &[(Val, Capability)],
        line: usize,
    ) -> Result<Type, String> {
        let verb = name.trim_start_matches('@');
        let operand = || match rest {
            [(v, _)] => Ok(v),
            _ => unsupported(&format!("`{name}` with no operand"), line),
        };
        if let (Type::SmallArray(inner, n), "push") = (self.cx.resolve(aty), verb) {
            let l = self.cx.layout(aty, line)?;
            let stride = self.stride(&inner, line)? as i32;
            let hdr = b.local(ValType::I32);
            b.ins(&Instruction::LocalTee(hdr));
            b.ins(&Instruction::I32Const(stride));
            b.ins(&Instruction::I64Const(n as i64));
            b.ins(&Instruction::I32Const(l.fields[3] as i32));
            b.ins(&Instruction::Call(self.cx.rt.sa_push));
            let stale = b.local(ValType::I32);
            b.ins(&Instruction::LocalSet(stale));
            let (last, _, data) = self.sa_parts(b, hdr, &l, n);
            b.ins(&Instruction::LocalGet(last));
            b.ins(&Instruction::I64Const(1));
            b.ins(&Instruction::I64Sub);
            b.ins(&Instruction::LocalSet(last));
            self.push_elem(m, b, data, last, stride, &inner, stale, operand()?, line)?;
            b.ins(&Instruction::LocalGet(hdr));
            return Ok(aty.clone());
        }
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
                self.core_val(m, b, operand()?, &t, line)?;
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
            let (data, last) = (b.local(ValType::I32), b.local(ValType::I64));
            b.ins(&Instruction::LocalGet(src));
            b.ins(&Instruction::I32Load(word_at(l.fields[0])));
            b.ins(&Instruction::LocalSet(data));
            b.ins(&Instruction::LocalGet(src));
            b.ins(&Instruction::I64Load(at(l.fields[1])));
            b.ins(&Instruction::I64Const(1));
            b.ins(&Instruction::I64Sub);
            b.ins(&Instruction::LocalSet(last));
            self.push_elem(m, b, data, last, stride, &elem, stale, operand()?, line)?;
        }
        b.ins(&Instruction::LocalGet(src));
        Ok(Type::Array(Box::new(elem)))
    }

    /// Stores `push`'s element at index `last` of the grown buffer `data`, then frees
    /// `stale`, the old buffer ([`Fn_::arr_rebuild`] says why in that order).
    #[allow(clippy::too_many_arguments)]
    fn push_elem(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        data: u32,
        last: u32,
        stride: i32,
        elem: &Type,
        stale: u32,
        v: &Val,
        line: usize,
    ) -> Result<(), String> {
        let w = Walk {
            data,
            len: last,
            stride: stride as u32,
            elem: elem.clone(),
            byte: false,
        };
        self.elem_addr(b, &w, last);
        let r = self.cx.repr(elem, line)?;
        self.core_val(m, b, v, elem, line)?;
        match &r {
            Repr::Scalar(_) => {
                b.ins(&self.cx.store(elem));
            }
            Repr::Agg(_) => {
                b.copy(stride as u32);
            }
            Repr::Unit => return unsupported("an array of Unit", line),
        }
        // After the store, nothing reads the old buffer through the caller's header.
        b.ins(&Instruction::LocalGet(stale));
        b.ins(&Instruction::Call(self.cx.rt.free));
        Ok(())
    }

    fn pop_at(
        &mut self,
        b: &mut Frame,
        slot: u32,
        aty: &Type,
        line: usize,
    ) -> Result<Type, String> {
        let (elem, len_at) = match self.cx.resolve(aty) {
            Type::Array(elem) => (elem, 1),
            Type::SmallArray(elem, _) => (elem, 0),
            _ => return unsupported(&format!("`pop` on `{aty}`"), line),
        };
        let elem = *elem;
        let al = self.cx.layout(aty, line)?;
        let opt = Type::option(elem.clone());
        let ol = self.cx.layout(&opt, line)?;
        let out = b.alloc(ol.size, ol.align);
        // Write `None` first; the `Some` arm overwrites the tag and payload in place.
        b.slot(out + ol.fields[0]);
        b.ins(&Instruction::I64Const(0));
        b.ins(&Instruction::I64Store(at(0)));
        for f in &ol.fields[1..] {
            b.slot(out + f);
            b.ins(&Instruction::I64Const(0));
            b.ins(&Instruction::I64Store(at(0)));
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
        b.ins(&Instruction::I32Const(al.fields[len_at] as i32));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalGet(w.len));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Sub);
        b.ins(&Instruction::LocalTee(last));
        b.ins(&Instruction::I64Store(at(0)));
        b.slot(out + ol.fields[0]);
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Store(at(0)));
        b.slot(out + ol.fields[1]);
        self.elem_addr(b, &w, last);
        self.load_elem(b, &w, line)?;
        // A two-word element is already two payload words: one copy, no encoding.
        if self.word2(&elem)? == Word::Inline2 {
            b.copy(16);
        } else {
            self.encode_word2(b, &elem, line)?;
            b.ins(&Instruction::I64Store(at(0)));
        }
        self.depth -= 1;
        b.ins(&Instruction::End);
        b.slot(out);
        Ok(opt)
    }

    /// Emits `swapRemove` on the array whose address is in local `slot`, at the index `index` names.
    fn swap_remove_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        slot: u32,
        aty: &Type,
        row: Option<Check>,
        index: &Val,
        line: usize,
    ) -> Result<Type, String> {
        // An `Array` keeps its pointer first, a `SmallArray` its length.
        let len_at = match self.cx.resolve(aty) {
            Type::Array(_) => 1,
            Type::SmallArray(..) => 0,
            _ => return unsupported(&format!("`swapRemove` on `{aty}`"), line),
        };
        let al = self.cx.layout(aty, line)?;
        b.ins(&Instruction::LocalGet(slot));
        let w = self.walk(b, aty, line)?;
        self.core_val(m, b, index, &Type::Int, line)?;
        let idx = b.local(ValType::I64);
        b.ins(&Instruction::LocalSet(idx));
        if let Some(row) = row {
            self.bounds_check(b, &w, idx, &row);
        }
        let elem = w.elem.clone();
        // Take the removed element before the last one overwrites it; they can be one address.
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
                b.copy(el.size);
            }
            _ => return unsupported("an array of Unit", line),
        }
        let last = b.local(ValType::I64);
        b.ins(&Instruction::LocalGet(slot));
        b.ins(&Instruction::I32Const(al.fields[len_at] as i32));
        b.ins(&Instruction::I32Add);
        b.ins(&Instruction::LocalGet(w.len));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Sub);
        b.ins(&Instruction::LocalTee(last));
        b.ins(&Instruction::I64Store(at(0)));
        self.elem_addr(b, &w, idx);
        self.elem_addr(b, &w, last);
        b.copy(w.stride);
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

    /// Encodes the value on the stack into an `Option`'s first payload word.
    fn encode_word2(&mut self, b: &mut Frame, t: &Type, line: usize) -> Result<(), String> {
        match self.word2(t)? {
            Word::Direct => {}
            Word::Ext(_) => {
                b.ins(&Instruction::I64ExtendI32U);
            }
            Word::Float(v) => float_into_word(b, v),
            Word::Boxed => self.box_value(b, t, line)?,
            // `build_sum2` copies a two-word payload whole.
            Word::Inline2 => return unsupported("an Option of a two-word payload", line),
        }
        Ok(())
    }
}

/// The variants of a sum type. `Option` and `Result` are `{ i1 tag, i64 w0, i64 w1 }`,
/// so a `Ref` fits inline; a user enum is `{ i64 tag, i64 p0, .. }`, one word per payload
/// slot of its widest variant.
type Sum = Vec<EnumVariant>;

/// Returns the tag an arm tests, or `None` for `Pattern::Other`, which tests nothing
/// ([`crate::two_way`]).
fn tag_of(sum: &Sum, pat: &Pattern, line: usize) -> Result<Option<usize>, String> {
    Ok(match pat {
        Pattern::Other => None,
        Pattern::Variant(name, _) => Some(
            sum.iter()
                .position(|v| v.name == *name)
                .ok_or_else(|| gap(&format!("the variant `{name}`"), line))?,
        ),
        // `??`'s parser desugar names a tag, not a variant, because it has no type.
        // Tag 1 is the success side of every built-in sum.
        _ => Some(usize::from(matches!(pat, Pattern::Success(_)))),
    })
}

/// How one payload travels inside a sum's `i64` word.
#[derive(PartialEq)]
enum Word {
    /// It is the word.
    Direct,
    /// A narrower integer scalar, zero-extended into the word.
    Ext(ValType),
    /// A float's bits, reinterpreted (and zero-extended for `f32`). `Ext` would emit
    /// `i64.extend_i32_u` on an `f64`, which fails validation.
    Float(ValType),
    /// Two words side by side, no heap (a `Ref` or a stored `fn`).
    Inline2,
    /// The word is a pointer to it.
    Boxed,
}

/// Turns the float on the stack into a sum payload's `i64` word by its bits, not its value.
fn float_into_word(b: &mut Frame, v: ValType) {
    if v == ValType::F32 {
        b.ins(&Instruction::I32ReinterpretF32);
        b.ins(&Instruction::I64ExtendI32U);
    } else {
        b.ins(&Instruction::I64ReinterpretF64);
    }
}

impl<'p> Fn_<'_, 'p> {
    /// Whether a value of `ty` transitively owns heap, by the frontend's predicate.
    fn owns_heap(&self, ty: &Type) -> bool {
        vyrn_frontend::declared::owns_heap(&self.cx.sub(ty), &self.cx.types)
    }

    /// Emits `x.copy()`: replaces the value on the stack with one that shares no heap.
    /// A `String` is the only owning value in a wasm local; any other is an aggregate,
    /// copied as bytes and then by [`Fn_::copy_at`].
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
                b.copy(l.size);
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

    /// Replaces the `String` pointer on the stack with a fresh buffer of the same bytes.
    /// Every `String` a `copy` reaches comes through here, as do `@str` of a `String`
    /// and of a `Bool`.
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
        b.ins(&MEMORY_COPY);
        b.ins(&Instruction::LocalGet(d));
    }

    /// Allocates `bytes + 1` bytes, copies `live` bytes from `src` into them, and returns
    /// the local holding the block. The extra byte keeps an empty copy from asking for zero.
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
        b.ins(&MEMORY_COPY);
        nb
    }

    /// Releases (`rel`) or deep-copies each of the first `count` elements of `buf`.
    /// A release walks only an element with a release row, the proof that the container
    /// owns it. A copy walks any element that reaches heap, released or not.
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
            self.cx.world.ownership.proto.release_kind(elem).is_some()
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

    /// Gives the copy of a `ty` value at `a` its own heap. The walk is a call, as a release is
    /// ([`Fn_::rel_at`]); [`Fn_::copy_body`] states it once per type.
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

    /// Emits the walk that duplicates the storage a `ty` value at `a` owns.
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
                let l = self.cx.layout(ty, line)?;
                let stride = self.stride(&inner, line)?;
                let (n, bytes) = (b.local(ValType::I32), b.local(ValType::I32));
                load_wrapped(b, a, l.fields[1]);
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
                let l = self.cx.layout(ty, line)?;
                let stride = self.stride(&inner, line)?;
                let (n, base) = (b.local(ValType::I32), b.local(ValType::I32));
                load_wrapped(b, a, l.fields[0]);
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
                load_wrapped(b, a, l.fields[1]);
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
                // with the buffer.
                let mk = self.map_key(&kt, line)?;
                let l = self.cx.layout(ty, line)?;
                let vstride = self.stride(&vt, line)?;
                let (n, cap) = (b.local(ValType::I32), b.local(ValType::I32));
                load_wrapped(b, a, l.fields[2]);
                b.ins(&Instruction::LocalSet(n));
                load_wrapped(b, a, l.fields[3]);
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
                // The index is copied, not rebuilt: it holds positions, and the
                // copy keeps capacity and order. `cap * 2` buckets of 8 bytes.
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
                for (off, f) in self.owning_fields(ty, line)? {
                    let p = b.local(ValType::I32);
                    b.ins(&Instruction::LocalGet(a));
                    b.ins(&Instruction::I32Const(off as i32));
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
            // A copy that skipped a boxed payload would share the box, and both copies would
            // release it.
            Type::Enum(_) => {
                for (tag, _, slots) in self.owning_payloads(ty, line)? {
                    tag_eq(b, a, tag);
                    b.ins(&Instruction::If(BlockType::Empty));
                    self.depth += 1;
                    for (_, off, pty, w) in slots {
                        self.copy_word(m, b, a, off, &pty, w, line)?;
                    }
                    self.depth -= 1;
                    b.ins(&Instruction::End);
                }
                Ok(())
            }
            // A stored `fn` value is `{ tag, captures }`. The capture block's size
            // is per tag, so the module's derived copy (`fnval_copy`) walks it.
            Type::Fn(..) => {
                let l = self.cx.layout(ty, line)?;
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::LocalGet(a));
                b.ins(&Instruction::I64Load(at(l.fields[0])));
                load_wrapped(b, a, l.fields[1]);
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

    /// Gives the sum payload word at `a + off` its own heap: a `String` in the word, or a
    /// boxed block, copied then walked.
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
                load_wrapped(b, a, off);
                self.str_dup(b);
                b.ins(&Instruction::I64ExtendI32U);
                b.ins(&Instruction::I64Store(at(off)));
                Ok(())
            }
            Word::Boxed => {
                let size = self.cx.layout(pty, line)?.size;
                let (src, bytes) = (b.local(ValType::I32), b.local(ValType::I32));
                load_wrapped(b, a, off);
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
            // A 128-bit vector is a wasm value, not an address, so `Inline2`'s
            // `memory.copy` cannot copy it, and `i64.extend_i32_u` on a `v128`
            // fails validation. It boxes.
            Repr::Scalar(ValType::V128) => Word::Boxed,
            Repr::Scalar(v) => Word::Ext(v),
            // Test `words(t) == 2`, not the shape: a one-slot sum is also two `I64`s,
            // and a nested sum rides in one boxed slot.
            Repr::Agg(_) if self.cx.words(t) == 2 => Word::Inline2,
            _ => Word::Boxed,
        })
    }

    /// Copy the value on the stack (a scalar, or an aggregate's address) onto
    /// the heap, leaving its address as an `i64` word.
    fn box_value(&mut self, b: &mut Frame, t: &Type, line: usize) -> Result<(), String> {
        let malloc = self.cx.rt.malloc;
        match self.cx.repr(t, line)? {
            Repr::Scalar(v) => {
                let size = self.cx.layout(t, line)?.size;
                // Two distinct scratch slots: `scratch` is keyed on (type, n), so for an
                // i32 scalar one `n` would give one local, and the `LocalTee` below
                // would overwrite the value with the box's address.
                let val = self.scratch(b, v, 2);
                let p = self.scratch(b, ValType::I32, 3);
                b.ins(&Instruction::LocalSet(val));
                b.ins(&Instruction::I64Const(size as i64));
                b.ins(&Instruction::Call(malloc));
                b.ins(&Instruction::LocalTee(p));
                b.ins(&Instruction::LocalGet(val));
                b.ins(&self.cx.store(t));
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
                b.copy(l.size);
                b.ins(&Instruction::LocalGet(p));
            }
            Repr::Unit => return unsupported("a Unit payload", line),
        }
        b.ins(&Instruction::I64ExtendI32U);
        Ok(())
    }

    /// Builds a sum value: the tag, then the live variant's payloads in their slots.
    /// Built-in sums and declared enums share one tag width and encoding.
    #[allow(clippy::too_many_arguments)]
    fn build_variant(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        ty: &Type,
        tag: u64,
        args: &[Val],
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
        // Build into the consumer's storage when it holds this same type.
        let (dest, used) = match hint {
            Some((d, t)) if self.cx.shape(&t) == self.cx.shape(ty) => (d, true),
            _ => (Dest::Slot(b.alloc(l.size, l.align)), false),
        };
        dest.addr(b, 0);
        b.ins(&Instruction::I64Const(tag as i64));
        b.ins(&Instruction::I64Store(at(0)));
        // Every slot this variant does not fill is zeroed: a `None` and a
        // narrower variant must not leave the widest one's words behind.
        let mut filled = 1;
        for (i, t) in payload.iter().enumerate() {
            let slot = self.cx.payload_slot(payload, i);
            if self.word2(t)? == Word::Inline2 {
                // Two words already side by side: one copy, no encoding.
                dest.addr(b, l.fields[slot]);
                self.part(m, b, args, i, t, line)?;
                b.copy(16);
            } else {
                dest.addr(b, l.fields[slot]);
                match self.part_built(&args[i]) {
                    Some(boxed) => {
                        boxed.addr(b, 0);
                        b.ins(&Instruction::I64ExtendI32U);
                    }
                    None => {
                        self.part(m, b, args, i, t, line)?;
                        self.encode_word2(b, t, line)?;
                    }
                }
                b.ins(&Instruction::I64Store(at(0)));
            }
            filled = slot + self.cx.words(t);
        }
        for slot in filled..l.fields.len() {
            dest.addr(b, l.fields[slot]);
            b.ins(&Instruction::I64Const(0));
            b.ins(&Instruction::I64Store(at(0)));
        }
        dest.addr(b, 0);
        self.dest_used = used;
        Ok(ty.clone())
    }

    /// `Age?(n)`: a validated construction that yields `Option<Age>`, a tag instead of a
    /// trap. `operand` pushes the value at the base type; `dest` names the storage
    /// the `Option` is written into, whose address is left on the stack.
    ///
    /// This bypasses the coercion seam on purpose: `expr_as(n, Age)` would emit the aborting
    /// validation. The argument is evaluated at the base type, and the answer of
    /// `predicate_holds` becomes the tag. The interpreter runs the same declaration, and the
    /// two must agree on every value.
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
            // Only the record arm of `emit_validation` binds an aggregate base, by field,
            // so no single local can become the payload word. Refuse rather than guess.
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
        let d = dest(b, &l);
        d.addr(b, l.fields[0]);
        b.ins(&Instruction::LocalGet(tag));
        b.ins(&Instruction::I64ExtendI32U);
        b.ins(&Instruction::I64Store(at(0)));
        d.addr(b, l.fields[1]);
        b.ins(&Instruction::LocalGet(held));
        self.encode_word2(b, &base, line)?;
        b.ins(&Instruction::I64Store(at(0)));
        for f in &l.fields[2..] {
            d.addr(b, *f);
            b.ins(&Instruction::I64Const(0));
            b.ins(&Instruction::I64Store(at(0)));
        }
        d.addr(b, 0);
        Ok(ty)
    }

    /// Pushes whether the sum at `addr` is `pat`'s variant. `match`, `if let` and `?` share
    /// it, so the tag is read at one width in one place.
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

    /// Opens a switch chain: one `block` whose arms each branch out of it. A switch of one tag
    /// and a default becomes an `if`/`else` instead ([`crate::two_way`]). `bt` is the join's
    /// type. [`Fn_::core_switch`] drives it over the row's arms.
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

    /// Returns the arms in emit order. In a two-way branch the tagged arm goes first, because
    /// the `if` tests a tag and the `else` takes the rest.
    fn chain_order(c: &Chain, arms: usize) -> Vec<usize> {
        match c.two {
            Some((at, _)) => vec![at, 1 - at],
            None => (0..arms).collect(),
        }
    }

    /// Enters arm `slot`: its test and its block. `probe` pushes the test's `i32`; the second
    /// side of a two-way branch does not call it.
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

    /// Leaves arm `slot`: a branch out of the chain, or the `end` of a two-way branch after
    /// its second side.
    fn chain_leave(&mut self, b: &mut Frame, c: &Chain, slot: usize) {
        match c.two {
            Some(_) if slot == 0 => return,
            Some(_) => {
                // Drop an empty `else`. Only an empty-result `if` can have one,
                // because a branch that carries a value always writes it.
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

    /// Closes the chain. The checker proves the arms exhaustive; the `unreachable` tells
    /// the validator.
    fn chain_close(&mut self, b: &mut Frame, c: &Chain) {
        if c.two.is_some() {
            return;
        }
        b.ins(&Instruction::Unreachable);
        self.depth -= 1;
        b.ins(&Instruction::End);
    }

    /// Pushes whether the sum at `addr` carries `tag`, or constant truth for an arm no tag
    /// chooses. Pattern arms ([`Fn_::tag_test`]) and the core walk
    /// ([`Test`]) share it, so the tag is read in one place.
    fn tag_is(b: &mut Frame, addr: u32, tag: Option<u64>) {
        b.ins(&Instruction::LocalGet(addr));
        // `None` is the refutable-`let` default arm: drop the address and
        // push constant truth.
        let Some(tag) = tag else {
            b.ins(&Instruction::Drop);
            b.ins(&Instruction::I32Const(1));
            return;
        };
        // Every sum's tag is an `i64` in its first word.
        b.ins(&Instruction::I64Load(at(0)));
        b.ins(&Instruction::I64Const(tag as i64));
        b.ins(&Instruction::I64Eq);
    }

    /// Binds payload `i` of the matched variant out of the sum at `addr`. `ptys` is the whole
    /// payload list, because a payload's slot depends on the widths before it.
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
                load_wrapped(b, addr, off);
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
                b.copy(16);
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
                        b.ins(&self.cx.load(t, 0));
                        b.ins(&Instruction::LocalSet(l));
                        Place::Local(l)
                    }
                    Repr::Agg(l) => {
                        let slot = b.alloc(l.size, l.align);
                        b.slot(slot);
                        b.ins(&Instruction::LocalGet(p));
                        b.copy(l.size);
                        Place::Slot(slot)
                    }
                    Repr::Unit => return unsupported("a Unit payload", line),
                };
                // A consumed scrutinee's box is this construct's to free once
                // the value is out.
                if free_box {
                    b.ins(&Instruction::LocalGet(p));
                    b.ins(&Instruction::Call(self.cx.rt.free));
                }
                place
            }
        })
    }

    /// Whether a placed row releases the value at `key` whole. The type's release table says
    /// a release exists; only a row says one runs here. A construct that took its scrutinee
    /// has no row, so it frees the boxes its binders came out of (`releaseacrossexit`'s
    /// `overIfLet`).
    fn releases_whole(&self, key: NodeId) -> bool {
        self.placed
            .values()
            .any(|rows| rows.iter().any(|(binding, _)| *binding == key))
    }
}

/// A Map is `{ ptr keys, ptr vals, i64 len, i64 cap, ptr index }`: two parallel growable
/// buffers in first-insertion order sharing one length, plus a hash index. Field 2 is the
/// length, where an `Array`'s is field 1. Code here reads the header through its address,
/// never a snapshot, because an insert may reallocate the buffers.
impl<'p> Fn_<'_, 'p> {
    /// Builds a map literal of type `mty` at `dest`: a zeroed header, then each key-value pair
    /// of `parts` inserted in written order. A duplicate key updates in place and keeps its
    /// slot, and the insert releases the shadowed value; `free` leaves an arena block to its
    /// `region`.
    /// [`Fn_::core_make`] calls this.
    fn map_into(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        mty: &Type,
        parts: &[Val],
        line: usize,
    ) -> Result<(), String> {
        let Type::Map(key_t, val) = self.cx.resolve(mty) else {
            return unsupported(&format!("a map literal of `{mty}`"), line);
        };
        let l = self.cx.layout(mty, line)?;
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

    /// `m.tallyBytes(w, n)`: kind 3 of `mapFind` compares the byte window in place,
    /// so a hit builds, validates and allocates nothing. A miss builds the key with
    /// `str_from_bytes`, whose `Err` traps, and stores it without a copy.
    ///
    /// `hdr` holds the header address; `window` is the bytes and `count` is `n`.
    fn map_tally_bytes(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        mty: &Type,
        window: &Val,
        count: &Val,
        line: usize,
    ) -> Result<Type, String> {
        let Type::Map(..) = self.cx.resolve(mty) else {
            return unsupported(&format!("`tallyBytes` on `{mty}`"), line);
        };
        let l = self.cx.layout(mty, line)?;
        let bytes = Type::Array(Box::new(Type::IntN {
            bits: 8,
            signed: false,
        }));
        let wsrc = b.local(ValType::I32);
        self.core_val(m, b, window, &bytes, line)?;
        b.ins(&Instruction::LocalSet(wsrc));
        let al = self.cx.layout(&bytes, line)?;
        let (wdata, wlen) = (b.local(ValType::I32), b.local(ValType::I32));
        b.ins(&Instruction::LocalGet(wsrc));
        b.ins(&Instruction::I32Load(word_at(al.fields[0])));
        b.ins(&Instruction::LocalSet(wdata));
        load_wrapped(b, wsrc, al.fields[1]);
        b.ins(&Instruction::LocalSet(wlen));
        let n = b.local(ValType::I64);
        self.core_val(m, b, count, &Type::Int, line)?;
        b.ins(&Instruction::LocalSet(n));
        // One probe, before any key exists: kind 3, the window's length as
        // `klen`, the window's address as the key.
        let idx = b.local(ValType::I32);
        b.ins(&Instruction::I32Const(3));
        b.ins(&Instruction::LocalGet(wlen));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[0])));
        load_wrapped(b, hdr, l.fields[2]);
        b.ins(&Instruction::LocalGet(wdata));
        b.ins(&Instruction::I64ExtendI32U);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[4])));
        load_wrapped(b, hdr, l.fields[3]);
        b.ins(&Instruction::Call(self.cx.rt.map_find));
        b.ins(&Instruction::LocalSet(idx));
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32LtS);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        // Miss: build the key; an `Err` from `str_from_bytes` traps.
        let rty = Type::result(Type::Str, Type::Str);
        let rl = self.cx.layout(&rty, line)?;
        let dest = b.alloc(rl.size, rl.align);
        self.str_from_bytes(b, dest, wsrc, &al, line)?;
        b.slot(dest + rl.fields[0]);
        b.ins(&Instruction::I64Load(at(0)));
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
        // The probe found no entry and the key is ours, so it is stored, not copied.
        self.map_reserve(b, hdr, &l, 8, MapKey::Str);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[0])));
        load_wrapped(b, hdr, l.fields[2]);
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

    /// `m.tally(k, n)`: insert-or-add with one probe. A hit adds in place; a miss
    /// stores a copy of the key, so the caller owns the key on both paths.
    ///
    /// `hdr` holds the header address; `count` is `n`.
    fn map_tally(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        mty: &Type,
        key: &Val,
        count: &Val,
        line: usize,
    ) -> Result<Type, String> {
        if !matches!(self.cx.resolve(mty), Type::Map(..)) {
            return unsupported(&format!("`tally` on `{mty}`"), line);
        }
        let (k, l, mk) = self.map_key_local(m, b, mty, key, line)?;
        let n = b.local(ValType::I64);
        self.core_val(m, b, count, &Type::Int, line)?;
        b.ins(&Instruction::LocalSet(n));
        let idx = b.local(ValType::I32);
        self.map_scan(b, hdr, &l, k, idx, mk);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I32Const(0));
        b.ins(&Instruction::I32LtS);
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        // Miss: reserve, append the key, index it, start the count at `n`. Int64 and
        // packed keys are stored by value, never dup'd or freed.
        self.map_reserve(b, hdr, &l, 8, mk);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[0])));
        load_wrapped(b, hdr, l.fields[2]);
        b.ins(&Instruction::LocalTee(idx));
        b.ins(&Instruction::I32Const(mk.stride()));
        b.ins(&Instruction::I32Mul);
        b.ins(&Instruction::I32Add);
        match mk {
            MapKey::I64 => {
                b.ins(&Instruction::LocalGet(k));
                b.ins(&Instruction::I64Store(at(0)));
            }
            MapKey::Pack(stride) => {
                b.ins(&Instruction::LocalGet(k));
                b.copy(stride);
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
        // Hit: add in place.
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

    /// `m[k] = v`: update in place on a hit, append on a miss. `hdr` holds the header address.
    /// The caller decides `drop_old`, whether the store may release the old value, because a
    /// value that names the map could name the bytes this frees.
    fn map_set(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        l: &Layout,
        parts: &[Val],
        key: usize,
        key_t: &Type,
        val: &Type,
        drop_old: bool,
        line: usize,
    ) -> Result<(), String> {
        // Int64 keys are stored by value; packed user keys are canonical bytes in a
        // fixed-stride column. Neither is dup'd or freed.
        let mk = self.map_key(key_t, line)?;
        let esz = self.stride(val, line)? as i32;
        let r = self.cx.repr(val, line)?;
        // Evaluate key then value before the scan, so a side-effecting value runs
        // before the map is probed.
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
        let Some(v) = r.val() else {
            return unsupported("a Map of Unit", line);
        };
        let v = b.local(v);
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
        load_wrapped(b, hdr, l.fields[2]);
        b.ins(&Instruction::LocalTee(idx));
        b.ins(&Instruction::I32Const(mk.stride()));
        b.ins(&Instruction::I32Mul);
        b.ins(&Instruction::I32Add);
        match mk {
            MapKey::I64 => {
                b.ins(&Instruction::LocalGet(k));
                b.ins(&Instruction::I64Store(at(0)));
            }
            MapKey::Pack(stride) => {
                b.ins(&Instruction::LocalGet(k));
                b.copy(stride);
            }
            MapKey::Str => {
                b.ins(&Instruction::LocalGet(k));
                b.ins(&Instruction::I32Store(word()));
            }
        }
        // `map_reserve` grew and rebuilt the index, so only this entry is missing
        // from it, which keeps the append O(1).
        self.map_put(b, hdr, l, idx, mk);
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I64Load(at(l.fields[2])));
        b.ins(&Instruction::I64Const(1));
        b.ins(&Instruction::I64Add);
        b.ins(&Instruction::I64Store(at(l.fields[2])));
        // Hit: the map keeps its key, so it frees the surplus one. If `drop_old`, the old
        // value is released too; no reserve ran here, so `vals` still holds it.
        b.ins(&Instruction::Else);
        if drop_old {
            self.map_val_addr(b, hdr, l, idx, esz);
            let old = b.local(ValType::I32);
            b.ins(&Instruction::LocalSet(old));
            self.rel_entry(m, b, old, val, line)?;
        }
        // Freed inside a `region` too: an arena block's class word is 0, so `free` leaves it
        // to the region (`std/runtime`'s ownership test). Int64 keys own nothing.
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
                b.ins(&self.cx.store(val));
            }
            Repr::Agg(vl) => {
                b.copy(vl.size);
            }
            Repr::Unit => return unsupported("a Map of Unit", line),
        }
        Ok(())
    }

    /// `mapFind(kind, klen, keys, len, key, idx, cap)` into `idx`. `k` holds the key: an `i64`
    /// for an Int64 key, else an `i32` address, widened to `i64` for the runtime.
    fn map_scan(&mut self, b: &mut Frame, hdr: u32, l: &Layout, k: u32, idx: u32, mk: MapKey) {
        let (kind, klen) = mk.kind();
        b.ins(&Instruction::I32Const(kind));
        b.ins(&Instruction::I32Const(klen));
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[0])));
        load_wrapped(b, hdr, l.fields[2]);
        b.ins(&Instruction::LocalGet(k));
        if mk != MapKey::I64 {
            b.ins(&Instruction::I64ExtendI32U);
        }
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[4])));
        load_wrapped(b, hdr, l.fields[3]);
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
        load_wrapped(b, hdr, l.fields[3]);
        b.ins(&Instruction::I32Const(2));
        b.ins(&Instruction::I32Mul);
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::Call(self.cx.rt.map_put));
    }

    /// Which key family a map runs on: `String` pointers, `Int64` values, or packed user keys
    /// of a fixed stride.
    fn map_key(&mut self, key_t: &Type, line: usize) -> Result<MapKey, String> {
        Ok(match self.cx.resolve(key_t) {
            Type::Int => MapKey::I64,
            Type::Record(_) | Type::Enum(_) => MapKey::Pack(self.cx.layout(key_t, line)?.size),
            _ => MapKey::Str,
        })
    }

    /// Packs a user key into a zeroed frame slot, so a byte compare is field-wise equality.
    /// `src` holds the key's address; returns a local holding the slot's.
    fn pack_key(
        &mut self,
        b: &mut Frame,
        src: u32,
        key_t: &Type,
        line: usize,
    ) -> Result<u32, String> {
        let l = self.cx.layout(key_t, line)?;
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

    /// Copies one level of a pack: a record field by field, recursively, so padding stays
    /// zero; a scalar or fieldless enum whole.
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
            let l = self.cx.layout(ty, line)?;
            for (i, f) in fs.iter().enumerate() {
                self.pack_fields(b, src, dst, off + l.fields[i], &f.ty, line)?;
            }
            return Ok(());
        }
        let sz = self.cx.layout(ty, line)?.size;
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
        b.copy(sz);
        Ok(())
    }

    fn map_val_addr(&mut self, b: &mut Frame, hdr: u32, l: &Layout, idx: u32, esz: i32) {
        b.ins(&Instruction::LocalGet(hdr));
        b.ins(&Instruction::I32Load(word_at(l.fields[1])));
        b.ins(&Instruction::LocalGet(idx));
        b.ins(&Instruction::I32Const(esz));
        b.ins(&Instruction::I32Mul);
        b.ins(&Instruction::I32Add);
    }

    /// `mapReserve(kind, klen, hdr, esz)`: room for one more entry in both columns and the
    /// index. The `len + 1 > cap` test stays inline so an insert that fits makes no call.
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

    /// Returns the index of the key `key` names in the map at `hdr`, negative on a miss,
    /// with the map's layout and key family.
    fn map_find(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        mty: &Type,
        key: &Val,
        line: usize,
    ) -> Result<(u32, Rc<Layout>, MapKey), String> {
        let (k, l, mk) = self.map_key_local(m, b, mty, key, line)?;
        let idx = b.local(ValType::I32);
        self.map_scan(b, hdr, &l, k, idx, mk);
        Ok((idx, l, mk))
    }

    /// The key `key` names, in a local at the form [`Fn_::map_scan`] probes
    /// with, with the map's layout and key family.
    fn map_key_local(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        mty: &Type,
        key: &Val,
        line: usize,
    ) -> Result<(u32, Rc<Layout>, MapKey), String> {
        let key_t = match self.cx.resolve(mty) {
            Type::Map(k, _) => *k,
            _ => Type::Str,
        };
        let mk = self.map_key(&key_t, line)?;
        let l = self.cx.layout(mty, line)?;
        let k = match mk {
            MapKey::I64 => {
                let k = b.local(ValType::I64);
                self.core_val(m, b, key, &Type::Int, line)?;
                b.ins(&Instruction::LocalSet(k));
                k
            }
            MapKey::Pack(_) => {
                let raw = b.local(ValType::I32);
                self.core_val(m, b, key, &key_t, line)?;
                b.ins(&Instruction::LocalSet(raw));
                self.pack_key(b, raw, &key_t, line)?
            }
            MapKey::Str => {
                let k = b.local(ValType::I32);
                self.core_val(m, b, key, &Type::Str, line)?;
                b.ins(&Instruction::LocalSet(k));
                k
            }
        };
        Ok((k, l, mk))
    }

    /// `m[k]`: an `Option<V>`, never a trap. The map's address is on the stack.
    fn map_at(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        mty: &Type,
        val: &Type,
        key: &Val,
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
        b.ins(&Instruction::I64Store(at(0)));
        match self.word2(val)? {
            // Two words side by side in the value buffer: one copy, no encoding.
            Word::Inline2 => {
                b.slot(off + ol.fields[1]);
                self.map_val_addr(b, hdr, &l, idx, esz);
                b.copy(16);
            }
            // A value wider than a word (an aggregate, a vector): the payload word points
            // at the entry in the map's buffer, whose bytes are the box's. The `Option`
            // is a borrow of the entry, and every write to the map ends it before the
            // buffer can move, so nothing allocates and nothing frees a box (#463).
            Word::Boxed => {
                b.slot(off + ol.fields[1]);
                self.map_val_addr(b, hdr, &l, idx, esz);
                b.ins(&Instruction::I64ExtendI32U);
                b.ins(&Instruction::I64Store(at(0)));
            }
            _ => {
                b.slot(off + ol.fields[1]);
                self.map_val_addr(b, hdr, &l, idx, esz);
                b.ins(&self.cx.load(val, 0));
                self.encode_word2(b, val, line)?;
                b.ins(&Instruction::I64Store(at(0)));
            }
        }
        self.depth -= 1;
        b.ins(&Instruction::End);
        b.slot(off);
        Ok(oty)
    }

    /// `assert(c)` and `assertEq(a, b)`, the builtins [`Spec::Asserts`] names.
    fn asserts(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[(Val, Capability)],
        line: usize,
    ) -> Result<Type, String> {
        match name {
            // `assert(c)`: traps with the interpreter's message.
            "assert" => {
                self.core_arg(m, b, name, args, 0, Some(&Type::Bool), line)?;
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
            // `assertEq(a, b)`: each operand is evaluated once, compared by its type, and on a
            // mismatch rendered as the interpreter's `scalar_to_string` renders it, around
            // ` != `. [`Fn_::call`] releases an allocated operand after this returns.
            _ => {
                let t = self.core_arg(m, b, name, args, 0, None, line)?;
                let t = self.cx.resolve(&t);
                let Some(vt) = self.cx.repr(&t, line)?.val() else {
                    return unsupported("`assertEq` on a non-scalar", line);
                };
                let la = b.local(vt);
                b.ins(&Instruction::LocalSet(la));
                self.core_arg(m, b, name, args, 1, Some(&t), line)?;
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
                    // Rendered as `@str` renders; the program exits after this
                    // print, so nothing rendered is released.
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

    /// `close(s)`, `boxStream(s)` and `serveStream(s)`, the builtins
    /// [`Spec::Effect`] names.
    fn effect(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[(Val, Capability)],
        line: usize,
    ) -> Result<Type, String> {
        match name {
            "boxStream" => return self.stream_box(m, b, args, line),
            "serveStream" => {
                let msg = self.cx.rt.intern(m, vyrn_frontend::trap::SERVE_STREAM);
                self.panic_line(m, b, None, |_, _, b| {
                    b.ins(&Instruction::I32Const(msg as i32));
                    Ok(())
                })?;
                b.ins(&Instruction::Unreachable);
            }
            // A stepped stream owns a cell from a fixed slab of 65536, which a leak would
            // exhaust; the tag tells a stepped stream from a buffer one.
            _ => {
                let got = self.core_arg(m, b, name, args, 0, None, line)?;
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

    /// The builtins [`Spec::Logs`] names.
    ///
    /// A `Logger` is its name string, so `logger(name)` is the identity. A level call
    /// evaluates both operands whatever the threshold, as the interpreter does.
    /// Below the compile-time threshold it emits no write, so a disabled site costs nothing.
    fn logs(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[(Val, Capability)],
        line: usize,
    ) -> Result<Type, String> {
        if name == "logger" {
            self.core_arg(m, b, name, args, 0, Some(&Type::Str), line)?;
            return Ok(Type::Logger);
        }
        self.core_arg(m, b, name, args, 0, Some(&Type::Logger), line)?;
        self.core_arg(m, b, name, args, 1, Some(&Type::Str), line)?;
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

    /// A SIMD builtin: a lane constructor, a lane read or write at
    /// a constant index, a mask reduction, or a load or store of consecutive
    /// array elements. The vector operand's own type chooses the opcode.
    fn lanes(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[(Val, Capability)],
        span: Result<Option<Check>, String>,
        line: usize,
    ) -> Result<Type, String> {
        match name {
            // Construction starts from `v128.const 0` and replaces each lane in order.
            "F32x4" | "I32x4" | "F64x2" if !args.is_empty() => {
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
                for i in 0..args.len() {
                    self.core_arg(m, b, name, args, i, Some(&lane), line)?;
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
            "@lane" if args.len() == 2 => {
                let vt = self.core_arg(m, b, name, args, 0, None, line)?;
                let vt = self.cx.resolve(&vt);
                let lanes = if matches!(vt, Type::F64x2 | Type::Mask64x2) {
                    2
                } else {
                    4
                };
                let Some(k) = core_lane(args, 1, lanes) else {
                    return unsupported("a lane index that is not a constant", line);
                };
                // A mask lane is all-ones or all-zeros and `Bool` must be 0 or 1, so
                // two `eqz` normalise it. The wide mask's first `eqz` is `i64.eqz`.
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
            // `v.replaceLane(k, x)`: vectors only; the checker refuses a mask receiver.
            "@replaceLane" if args.len() == 3 => {
                let vt = self.core_arg(m, b, name, args, 0, None, line)?;
                let vt = self.cx.resolve(&vt);
                let int = vt == Type::I32x4;
                let wide = vt == Type::F64x2;
                let Some(k) = core_lane(args, 1, if wide { 2 } else { 4 }) else {
                    return unsupported("a lane index that is not a constant", line);
                };
                let lane = if int {
                    &INT32
                } else if wide {
                    &Type::Float
                } else {
                    &Type::Float32
                };
                self.core_arg(m, b, name, args, 2, Some(lane), line)?;
                b.ins(&if int {
                    Instruction::I32x4ReplaceLane(k)
                } else if wide {
                    Instruction::F64x2ReplaceLane(k)
                } else {
                    Instruction::F32x4ReplaceLane(k)
                });
                return Ok(vt);
            }
            // Mask reductions push an `i32` that is already 0 or 1. `v128.any_true` tests
            // any bit, which equals per-lane any-true because a mask lane is all-ones or
            // all-zeros. `all_true` needs the lane width: `i32x4.all_true` on a
            // `Mask64x2` differs on a mixed mask.
            "@anyTrue" | "@allTrue" if args.len() == 1 => {
                let mt = self.core_arg(m, b, name, args, 0, None, line)?;
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
            // Four consecutive elements (two for `F64x2`) as one 16-byte access behind one
            // bounds check. `elem_addr` scales by the array's element size, so it needs no
            // lane knowledge.
            "@f32x4Load" | "@f32x4Store" | "@i32x4Load" | "@i32x4Store" | "@f64x2Load"
            | "@f64x2Store"
                if args.len() == 2 + usize::from(name.ends_with("Store")) =>
            {
                let span = span?;
                let vec = if name.starts_with("@i32x4") {
                    Type::I32x4
                } else if name.starts_with("@f64x2") {
                    Type::F64x2
                } else {
                    Type::F32x4
                };
                let aty = self.core_arg(m, b, name, args, 0, None, line)?;
                let w = self.walk(b, &aty, line)?;
                self.core_arg(m, b, name, args, 1, Some(&Type::Int), line)?;
                let idx = b.local(ValType::I64);
                b.ins(&Instruction::LocalSet(idx));
                if let Some(row) = span {
                    self.bounds_check_span(b, &w, idx, &row);
                }
                if name.ends_with("Load") {
                    self.elem_addr(b, &w, idx);
                    // `align: 0` is a log2 exponent: one byte. Nothing guarantees 16-byte
                    // alignment, and an overstated hint lets the engine assume it.
                    b.ins(&Instruction::V128Load(mem_arg(0, 0)));
                    return Ok(vec);
                }
                self.elem_addr(b, &w, idx);
                self.core_arg(m, b, name, args, 2, Some(&vec), line)?;
                b.ins(&Instruction::V128Store(mem_arg(0, 0)));
                return Ok(Type::Unit);
            }
            _ => unsupported(&format!("`{name}` at this arity"), line),
        }
    }

    /// `m.keys()` with the map's address in `hdr`: a snapshot `Array<K>` in its own buffer.
    /// String keys are dup'd per element, because the array owns its elements; Int64 and
    /// packed keys copy with the buffer.
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
        let l = self.cx.layout(mty, line)?;
        let aty = Type::Array(key_t);
        let al = self.cx.layout(&aty, line)?;
        let (len, buf) = (b.local(ValType::I32), b.local(ValType::I32));
        load_wrapped(b, hdr, l.fields[2]);
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
            b.ins(&Instruction::I64Store(at(0)));
        }
        b.slot(off);
        Ok(aty)
    }

    /// `m.remove(k)` on the map at `hdr`: releases and drops the entry of the key `key` names,
    /// and leaves whether it existed on the stack.
    fn map_remove(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        hdr: u32,
        mty: &Type,
        key: &Val,
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
        // Survivors shift down, so insertion order holds and a re-inserted key moves
        // to the end.
        let esz = self.stride(&val, line)? as i32;
        b.ins(&Instruction::LocalGet(found));
        b.ins(&Instruction::If(BlockType::Empty));
        self.depth += 1;
        // The map owns the key and value, so release both before `mapRemoveAt`
        // shifts survivors over them; the runtime has no types. Int64 and packed keys
        // own nothing. Nothing aliases the entry, because `keys()` copies. No `region`
        // check: the arena refuses `String` keys, and a map built in one holds buffers
        // the arena never had.
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
        // `mapRemoveAt(kind, klen, hdr, esz, i)` shifts both columns, updates the
        // length and rebuilds the index, whose buckets the shift made stale.
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

/// Writes the inline state of a `SmallArray<T, N>` header at `dest`: `len`, `cap == n`
/// and a null `data`.
fn sa_head(b: &mut Frame, dest: Dest, l: &Layout, len: usize, n: usize) {
    dest.addr(b, l.fields[0]);
    b.ins(&Instruction::I64Const(len as i64));
    b.ins(&Instruction::I64Store(at(0)));
    dest.addr(b, l.fields[1]);
    b.ins(&Instruction::I64Const(n as i64));
    b.ins(&Instruction::I64Store(at(0)));
    dest.addr(b, l.fields[2]);
    b.ins(&Instruction::I32Const(0));
    b.ins(&Instruction::I32Store(word()));
}

/// A `SmallArray<T, N>` is `{ i64 len, i64 cap, ptr data, [N x T] inline }` with two states:
/// `cap == N` is inline (`data` is null and never read), `cap > N` is spilled. Its first
/// field is a length where an `Array`'s is a pointer, so reading one as the other
/// validates and indexes garbage. Every element access goes through [`Walk`], so the
/// state branch lives only in [`Fn_::walk`]; only `push`, `pop`, `swapRemove` and
/// `toArray` write the header, and only `push` spills.
impl<'p> Fn_<'_, 'p> {
    /// `(len, cap, base)` of the SmallArray at `hdr`. `base` is the inline field while
    /// `cap == N`, else `data`; this branch is why a read-heavy loop loses to `Array`.
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

    /// A contextual `[a, b, c]` or `[]` in a `SmallArray<T, N>` position: the elements copied
    /// into the inline buffer, `cap == N`, `data` null. The checker proved `len <= N`; slots
    /// `len..N` stay unwritten because `len` bounds every read.
    fn sa_from_fixed(
        &mut self,
        b: &mut Frame,
        inner: &Type,
        len: usize,
        want: &Type,
        n: usize,
        line: usize,
    ) -> Result<(), String> {
        // When `len` is 0 nothing is on the stack: `array_lit` reaches here directly for `[]`.
        let src = b.local(ValType::I32);
        if len > 0 {
            b.ins(&Instruction::LocalSet(src));
        }
        let l = self.cx.layout(want, line)?;
        let off = b.alloc(l.size, l.align);
        sa_head(b, Dest::Slot(off), &l, len, n);
        if len > 0 {
            b.slot(off + l.fields[3]);
            b.ins(&Instruction::LocalGet(src));
            b.copy(self.extent(inner, len, line)?);
        }
        b.slot(off);
        Ok(())
    }

    /// A `SmallArray<T, N>` literal from a core row: the [`sa_head`] header at `dest`, and the
    /// parts written into the inline buffer. The checker proved `parts.len() <= N`.
    #[allow(clippy::too_many_arguments)]
    fn sa_into(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        ty: &Type,
        inner: &Type,
        n: usize,
        parts: &[Val],
        line: usize,
    ) -> Result<(), String> {
        let l = self.cx.layout(ty, line)?;
        sa_head(b, dest, &l, parts.len(), n);
        self.fixed_elems(m, b, dest.at(l.fields[3]), inner, parts, line)
    }

    /// The layout of the `SmallArray` at header address `hdr`, and a walk over its live slots.
    fn sa_open(
        &mut self,
        b: &mut Frame,
        hdr: u32,
        aty: &Type,
        line: usize,
    ) -> Result<(Rc<Layout>, Walk), String> {
        let Type::SmallArray(inner, n) = self.cx.resolve(aty) else {
            return unsupported(&format!("a SmallArray operation on `{aty}`"), line);
        };
        let l = self.cx.layout(aty, line)?;
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

    /// `xs.toArray()` on the `SmallArray` at header address `hdr`: a fresh `Array<T>` with
    /// copies of the live elements. An array owns its elements, so each copy gets its own heap.
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
        let al = self.cx.layout(&want, line)?;
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
        b.ins(&MEMORY_COPY);
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
            b.ins(&Instruction::I64Store(at(0)));
        }
        b.slot(off);
        Ok(want)
    }
}

/// An 8-byte access at a static offset.
fn at(off: u32) -> MemArg {
    mem_arg(off, 3)
}

/// A 4-byte access at a static offset.
fn word_at(off: u32) -> MemArg {
    mem_arg(off, 2)
}

/// A Vyrn integer type: a width, a signedness, and the wasm carrier both imply.
///
/// wasm has only `i32` and `i64` arithmetic, so an `Int8` rides an `i32`. The invariant: a value
/// is correctly represented in its carrier, sign-extended when signed and zero-extended when
/// not. So [`renorm`] is the only place a width is enforced, and signedness picks an opcode
/// rather than a fixup.
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

    /// The integer type `ty` is, or `None`. Takes a resolved type, so a validated name has
    /// already become its base. `vyrn_frontend::validate::width` owns the answer.
    fn of(ty: &Type) -> Option<Num> {
        vyrn_frontend::validate::width(ty).map(|(bits, signed)| Num { bits, signed })
    }

    /// Whether the carrier is an `i64`. Everything 32 bits and under rides an `i32`, per
    /// `wasm::abi`.
    fn wide(self) -> bool {
        self.bits == 64
    }
}

/// Put a value back in range after an operator that could have left it.
///
/// A no-op where the carrier is the width (32 and 64 bits). Called after every wrapping
/// operator, not only where overflow looks possible, because every other site reads the
/// invariant.
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

/// The wasm opcode for `op` at width `n`, or `None` for a non-arithmetic operator.
///
/// The carrier picks `i32` or `i64`. Signedness picks only where wasm has two opcodes (divide,
/// remainder, the orderings, the right shift); two's complement makes the rest signedness-blind.
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
        // Signed `>>` is arithmetic and unsigned is logical; both keep the representation
        // invariant, since sign bits and masked zeroes survive a right shift.
        BinOp::Shr => match (w, s) {
            (true, true) => Instruction::I64ShrS,
            (true, false) => Instruction::I64ShrU,
            (false, true) => Instruction::I32ShrS,
            (false, false) => Instruction::I32ShrU,
        },
        // `&&`, `||` and `=~` are not arithmetic; the caller lowers them before this.
        BinOp::And | BinOp::Or | BinOp::Match => return None,
    })
}

/// The comparison instruction for an `i32`-shaped operand pair.
fn cmp_i32(op: BinOp) -> Option<Instruction<'static>> {
    op.compare()?;
    int_op(
        op,
        Num {
            bits: 32,
            signed: true,
        },
    )
}

vyrn_frontend::body_scope_descent!(HoistVisit, hoist_block, hoist_stmt, hoist_expr);

/// The hoist's visitor: hands each node to `fe` or `fs` and stops at a lambda. It ignores scope.
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
        // A lambda is a leaf: its body lowers as its own function, and `header_invariant`
        // refuses a lambda that mentions the binding.
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

/// A scalar spilled for a `modify` call: its slot, its local and its load ([`Fn_::spill`]).
type Spill = (u32, u32, Instruction<'static>);

/// Write each spilled scalar back into its local after the call.
fn reload(b: &mut Frame, spilled: &[Spill]) {
    for (off, l, load) in spilled {
        b.slot(*off);
        b.ins(load);
        b.ins(&Instruction::LocalSet(*l));
    }
}

/// Declares the runtime functions written in Vyrn (`std/runtime`): each by the name the module
/// declares it under, with the wasm signature this emitter calls it with.
///
/// Every index is reserved up front, because runtime bodies call each other (`trap` calls
/// `strLen`). A body arrives when the module's function lowers like any user function, and
/// [`VyrnRt::take`] checks its declared signature against this one. [`VyrnRt::check`] refuses a
/// link with no `std/runtime` rather than letting `Module::sweep` panic on an unfilled body.
/// `Module::sweep` drops the functions no export reaches, with their interned data.
macro_rules! runtime_fns {
    ($($(#[$m:meta])* $field:ident = $name:literal ($($p:tt)*) -> ($($r:tt)*);)*) => {
        const VYRN_RUNTIME: &[(&str, &[ValType], &[ValType])] = {
            use ValType::{I32, I64};
            &[$(($name, &[$($p)*], &[$($r)*])),*]
        };

        /// The index of every function in [`VYRN_RUNTIME`], one field per row.
        #[derive(Clone, Copy, Default)]
        struct Fns {
            $($(#[$m])* $field: u32,)*
        }

        impl Fns {
            fn bind(v: &VyrnRt) -> Fns {
                Fns { $($field: v.get($name),)* }
            }
        }
    };
}

// Each row states a function once: the field call sites read, the name
// `std/runtime` declares, and the signature. A misspelt field fails the build;
// a misspelt name fails `VyrnRt::check` on every program.
runtime_fns! {
    // The request is an `i64` so `cap * stride` cannot wrap at an `i32` call site; it is the
    // signature `__vyrn_malloc` exports.
    /// A segregated free list whose heads and bump offset live in the heap's first 480 bytes.
    malloc = "malloc" (I64) -> (I32);
    free = "free" (I32) -> ();
    // `strFromBytes` returns an aggregate `Result<Int64, Int64>`, so the hidden destination
    // leads; the check's answer and the two interned messages are its last three arguments.
    /// Allocate a `String` buffer: its `{ len, cap }` header, `cap` bytes, and the NUL.
    /// Returns the address of the bytes, an ordinary NUL-terminated pointer.
    str_new = "strNew" (I32, I32) -> (I32);
    concat = "strConcat" (I32, I32) -> (I32);
    /// Grow a `String` accumulator in place; called at every `s = s + ...`.
    str_append = "strAppend" (I32, I32, I32) -> (I32);
    /// Its two failure messages, interned from `io_message`, are its last two arguments, so the
    /// wording stays `trap.rs`'s. `std/text`'s `stringFault`, called at the site, decides
    /// between them.
    str_from_bytes = "strFromBytes" (I32; 6) -> ();
    strlen = "strLen" (I32) -> (I32);
    strcmp = "strCmp" (I32, I32) -> (I32);
    /// Called only from `std/runtime`; the row reserves its index.
    #[allow(dead_code)]
    starts = "starts" (I32, I32) -> (I32);
    int_str = "intStr" (I64, I32) -> (I32);
    /// Called only from `std/runtime`; the row reserves its index.
    #[allow(dead_code)]
    str_i64 = "strI64" (I32) -> (I64);
    /// Called only from `std/runtime`; the row reserves its index.
    #[allow(dead_code)]
    utf8_valid = "utf8Valid" (I32, I32, I32) -> (I32);
    /// `=~`: walk a complete DFA over a NUL-terminated string. One helper serves every
    /// pattern, because the pattern is entirely in the table it is handed.
    regex_run = "regexRun" (I32, I32, I32, I32) -> (I32);
    // The maps: one body over the three key layouts. `kind` and `klen` lead (see
    // [`MapKey::kind`]). `mapFind`'s key is an `i64`: the value for an `Int64` key, a
    // zero-extended address otherwise.
    map_find = "mapFind" (I32, I32, I32, I32, I64, I32, I32) -> (I32);
    map_put = "mapPut" (I32; 6) -> ();
    map_reserve = "mapReserve" (I32; 4) -> ();
    map_remove_at = "mapRemoveAt" (I32; 5) -> ();
    map_keys_copy = "mapKeysCopy" (I32; 3) -> (I32);
    // The arrays: `dst` and `src` are header addresses and `stride` the element size. `arrPush`
    // returns the buffer a growth left behind; the emitter frees it after storing the element.
    // `a[i]` stays inline, by measurement.
    arr_push = "arrPush" (I32; 3) -> (I32);
    // `SmallArray` `push`: the header, the stride, `N` and the inline slots' offset.
    sa_push = "saPush" (I32, I32, I64, I32) -> (I32);
    arr_reserve = "arrReserve" (I32, I32, I32, I64) -> ();
    arr_append = "arrAppend" (I32; 4) -> ();
    arr_copy_from = "arrCopyFrom" (I32; 4) -> ();
    arr_clear = "arrClear" (I32; 2) -> ();
    // `bytes(s, from, to)`: the triple's slot leads, the trap row and table trail.
    bytes_of = "bytesOf" (I32, I32, I64, I64, I32, I32) -> ();
    // The I/O family this emitter calls; the builtins route to their own functions
    // (`loader::RT_MODULES`). The fixed-clock keys trail.
    /// The ONE place bytes leave a program, with the stdout buffer behind it.
    write_all = "writeAll" (I32; 3) -> (I32);
    print_str = "printStr" (I32) -> ();
    now_millis = "nowMillis" (I32) -> (I64);
    mono_nanos = "monoNanos" (I32) -> (I64);
    random_seed = "randomSeedV" (I32) -> (I64);
    open_at = "openAt" (I32, I32, I64) -> (I32);
    // The traps and renderers. `boolStr` takes the two interned literals; `trapAt`'s table is
    // `trap::Rule`'s, laid out by `runtime`.
    /// Write the canonical line on descriptor 2 and exit 1; every refusal this backend emits
    /// calls it.
    trap = "trapV" (I32) -> ();
    /// Every check the emitter inserts pushes its row number in [`Rt::trap_table`] and calls
    /// this; the module spells no wording.
    trap_at = "trapAt" (I32, I64, I32) -> ();
    print_i64 = "printI64" (I64, I32) -> ();
    bool_str = "boolStr" (I32; 3) -> (I32);
    // The region arena: `regionEnter` returns the bump top and `regionExit` restores it. The
    // nesting counter and its trap stay inline (see [`Fn_::region_enter`]).
    /// A `return` out of a region calls neither: the value it carries is arena memory the
    /// caller owns, so the bump stays.
    region_enter = "regionEnter" () -> (I32);
    region_exit = "regionExit" (I32) -> ();
}

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
    /// The runtime functions, which call sites read through `Deref`.
    fns: Fns,
    /// `wasi_snapshot_preview1.proc_exit`, so a lowering outside `runtime` can end the
    /// process: `std/mem`'s `trap` primitive is a write to descriptor 2 and this call.
    proc_exit: u32,
    /// The host-import table, so [`mem_ins`] can lower a `std/mem` import to its `call`.
    wasi: Wasi,
    /// The address of the UTF-8 DFA table `utf8Valid` walks, interned by
    /// `runtime`; every caller passes it as the third argument.
    utf8d: u32,
    /// `boolStr`'s last two arguments, so the module holds no wording of its own.
    str_true: u32,
    str_false: u32,
    /// `trapAt`'s table: eight rows of two interned addresses, laid out by `runtime` from
    /// `trap::Rule`.
    trap_table: u32,
    /// `strFromBytes`'s two failure messages.
    bnul: u32,
    butf8: u32,
    /// The injected clock and seed keys, `VYRN_FIXED_TIME=` and `VYRN_FIXED_SEED=`. The name
    /// carries its own `=`, so a lookup is one prefix test.
    fixed_time: u32,
    fixed_seed: u32,
    /// `trap.rs`'s `IO` table, read by `std/mem`'s `ioTable`: for each wording
    /// with a path, the interned halves around its `%s`, in the table's order.
    io: u32,
    /// The region nesting counter: four reserved bytes, because the depth is dynamic (a callee's
    /// `region` nests inside its caller's). Entering one past [`REGION_MAX`] traps.
    region_sp: u32,
    /// The call-depth counter: four reserved bytes holding how many Vyrn calls are in flight.
    /// Every named function's prologue bumps it and its one exit gives it back; past
    /// [`vyrn_frontend::trap::CALL_DEPTH_LIMIT`] it traps.
    call_depth: u32,
}

impl std::ops::Deref for Rt {
    type Target = Fns;
    fn deref(&self) -> &Fns {
        &self.fns
    }
}

impl Rt {
    /// Intern a string literal in the data segment, 4-aligned: the `{ len, cap }` header, the
    /// bytes, and the NUL. Returns the address of the bytes, so a literal is an ordinary `String`.
    ///
    /// An all-ones `cap` marks it static. A run-time empty `String`
    /// has `cap` 0, so 0 cannot be the mark.
    fn intern(&self, m: &mut Module, s: &str) -> u32 {
        let mut bytes = (s.len() as u32).to_le_bytes().to_vec();
        bytes.extend_from_slice(&u32::MAX.to_le_bytes());
        bytes.extend_from_slice(s.as_bytes());
        bytes.push(0);
        m.data(&bytes, 4) + SHDR
    }
}

/// Size of the `{ i32 len, i32 cap }` header in front of every `String`'s bytes.
/// An all-ones `cap` marks a data-segment literal, which a drop site must not free.
const SHDR: u32 = 8;

/// How many `region` scopes may be open at once; the language owns the number.
use vyrn_frontend::trap::REGION_MAX;

/// Offset from `heapBase()` of `std/runtime`'s region routing flag, the one heap address both
/// the module and the emitter name. The module owns the heap's first 480 bytes (the arena's
/// words are 468, 472 and 476); the emitter writes only this word, to route the next `String`
/// allocation to the open region's arena. See [`Fn_::arena_route`] and `std/runtime.vyrn`.
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
    mem_arg(0, 0)
}

/// Push `1` when the sum at address local `a` carries variant `tag`, else `0`. Every sum's tag
/// is an `i64`, built-in and declared alike.
fn tag_eq(b: &mut Frame, a: u32, tag: i64) {
    b.ins(&Instruction::LocalGet(a));
    b.ins(&Instruction::I64Load(at(0)));
    b.ins(&Instruction::I64Const(tag));
    b.ins(&Instruction::I64Eq);
}

fn word() -> MemArg {
    word_at(0)
}

/// The `cap` word of a String's `{ len, cap }` header. Named so an offset of 0 where 4 was
/// meant cannot pass silently.
fn cap_at() -> MemArg {
    word_at(4)
}

fn runtime(m: &mut Module, wasi: &Wasi, v: &VyrnRt) -> Rt {
    let proc_exit = wasi.at("proc_exit");
    // Every field is an index `VyrnRt` reserved or an interned address. No wasm function is
    // emitted here, so nothing needs numbering.
    let mut rt = Rt {
        fns: Fns::bind(v),
        ..Rt::default()
    };
    rt.proc_exit = proc_exit;
    rt.wasi = *wasi;
    rt.fixed_time = rt.intern(m, "VYRN_FIXED_TIME=");
    rt.fixed_seed = rt.intern(m, "VYRN_FIXED_SEED=");
    let mut io = Vec::new();
    for (pre, post) in crate::IO_MESSAGES
        .iter()
        .filter_map(|(_, w)| w.split_once("%s"))
    {
        io.extend_from_slice(&rt.intern(m, pre).to_le_bytes());
        io.extend_from_slice(&rt.intern(m, post).to_le_bytes());
    }
    rt.str_true = rt.intern(m, "true");
    rt.str_false = rt.intern(m, "false");
    // The trap table: for each `trap::Rule` row, the interned text before the value and after
    // it (zero where the row has no value). Trap sites push a row number; `trapAt` reads it.
    let mut table = Vec::with_capacity(8 * 8);
    for r in vyrn_frontend::trap::Rule::ALL {
        let (pre, post) = r.parts();
        let pre = rt.intern(m, &pre);
        let post = post.map_or(0, |p| rt.intern(m, &p));
        table.extend_from_slice(&pre.to_le_bytes());
        table.extend_from_slice(&post.to_le_bytes());
    }
    rt.trap_table = m.data(&table, 4);
    // The region nesting word, then the call-depth word: one reservation, because a host
    // restores both after a trap ([`Module::export_entry_state`]). The region limit and its
    // wording match the other engines; the region counter stays inline (see
    // [`Fn_::region_enter`]). The call-depth trap row uses the constant the prologue compares
    // against, so the limit in the message and the one enforced agree.
    rt.region_sp = m.reserve(8, 4);
    rt.call_depth = rt.region_sp + 4;
    // After the reserves, so the trap table and the fixed cells keep their
    // addresses.
    rt.io = m.data(&io, 4);

    // Hoehrmann's UTF-8 DFA, `crate::utf8d_table`. `std/runtime`'s `utf8Valid` takes its address as the third
    // argument.
    let utf8d = m.data(&crate::utf8d_table(), 1);
    rt.utf8d = utf8d;
    // `strFromBytes`'s two failure wordings, from `io_message`: an embedded NUL is refused
    // before the UTF-8 check. Every call passes them, so the module never spells them.
    rt.bnul = rt.intern(m, crate::io_message("bnul"));
    rt.butf8 = rt.intern(m, crate::io_message("butf8"));

    rt
}

/// The `path_open` right and flags `_start` opens a `file(..)` log sink with, from the
/// `wasi_snapshot_preview1` witx: `oflags::creat | trunc` is `fopen(path, "w")`, and
/// `right::fd_write` is the one right the sink needs.
const RIGHT_FD_WRITE: i64 = 1 << 6;
const OFLAGS_CREAT_TRUNC: i32 = 1 | 8;

/// The type of `.length` or `.byteLength` on `base`, or `None`. `base` is already resolved.
///
/// Neither name is a field, so every site that reads one asks this list.
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

/// A core-row builtin's operand types, instruction and result type, or `None` where the name
/// has no row or `argc` is not the row's arity.
///
/// `vyrn_lower::core::builtin_row` states the types; this states the instruction.
/// `builtin_rows_all_emit` refuses a row with no instruction.
fn builtin_spec(
    name: &str,
    argc: usize,
    gen: bool,
) -> Option<(&'static [Type], Instruction<'static>, &'static Type)> {
    let Some(Spec::Typed(params, ret)) = vyrn_lower::core::builtin_row(name, gen) else {
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
        // wasm's `min` and `max` propagate a NaN in either operand and order `-0.0` below
        // `+0.0`; `f32x4.nearest` rounds ties to even. The other engines follow these rules.
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

/// The arguments, where every one is a value.
fn arg_vals(
    args: &[(Arg, vyrn_frontend::ast::Capability)],
) -> Option<Vec<(Val, vyrn_frontend::ast::Capability)>> {
    args.iter()
        .map(|(a, c)| Some((a.val()?.clone(), *c)))
        .collect()
}

/// Argument `i` of a call row as a lane index below `lanes`; the checker proved it constant
/// and in range.
fn core_lane(args: &[(Val, vyrn_frontend::ast::Capability)], i: usize, lanes: i64) -> Option<u8> {
    match args.get(i) {
        Some((Val::Lit(Lit::Int(k)), _)) if (0..lanes).contains(k) => Some(*k as u8),
        _ => None,
    }
}

/// The module-state binding a receiver reads, where `n` is a read of one that a `@strAppend`
/// row grows ([`NameInfo::grows`]).
fn core_global(body: &vyrn_frontend::core::Body, n: Name) -> Option<&str> {
    if !body.names[n.index()].grows {
        return None;
    }
    let mut lets = Vec::new();
    for s in &body.stmts {
        core_lets(s, &mut lets);
    }
    lets.iter().find_map(|(m, rhs)| match rhs {
        Rhs::Read(vyrn_frontend::core::Place::Global(g)) if *m == n => Some(g.as_str()),
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

/// The specification row of the builtin a call row names, or `None` where the row names a
/// function this program declares or a callee with no row.
fn core_builtin(cx: &Cx, callee: &str, kind: Callee) -> Option<&'static Spec> {
    matches!(kind, Callee::Builtin | Callee::Reserved)
        .then(|| vyrn_lower::core::builtin_row(callee, cx.gen.is_some()))
        .flatten()
}

/// Where the core's names live while one body is walked, and what the wasm operand stack holds.
///
/// The core names every value, but wasm has an operand stack: `held` is the one name the stack
/// carries, bound by the statement just walked and read by this one. Any other name gets a
/// place of its own.
#[derive(Default)]
struct Walked {
    /// The wasm place of each of the core's names, by [`Name`].
    at: Vec<Option<(Place, Type)>>,
    /// How many times each name is READ over the whole body.
    reads: Vec<u32>,
    /// [`vyrn_frontend::core::Body::occurrences`], for the slot's extent.
    occurs: Vec<u32>,
    /// The frame slot each name took, as the mark before it and its end,
    /// until [`Fn_::core_give_back`] hands it back.
    slot: Vec<Option<(u32, u32)>>,
    /// The name the operand stack is holding, if any.
    held: Option<Name>,
    /// The temporary built in the caller's storage, which the `return` after
    /// it hands back without a copy ([`Fn_::core_lands`]).
    landed: Option<Name>,
    /// The header of each borrow a loop walks, taken apart where the borrow
    /// is bound ([`NameInfo::walked`](NameInfo::walked)).
    walks: Vec<Option<Walk>>,
    /// The storage a name's value is built in when it was taken before the
    /// name's own row: a parent's, at the first of its parts whose own row
    /// writes it, and a part's, at its offset in the parent's
    /// ([`Fn_::core_part_at`]).
    built: Vec<Option<Dest>>,
    /// A heap array's element buffer, taken at the first of its parts whose
    /// own row writes it.
    bufs: Vec<Option<u32>>,
    /// The headers the loops open around the row walk, and the borrow each
    /// is walked through ([`vyrn_frontend::core::Walk::While`]): a store into
    /// an element of one reads the borrow's parts.
    over: Vec<(vyrn_frontend::core::Place, Name)>,
    /// The place a stream's pull wrote its element to, by the pull's name,
    /// until the read at that name binds it ([`Spec::Pulls`]).
    pulled: Vec<Option<(Place, Type)>>,
    /// The check rows walked so far, in row order, and whether a construct ran each
    /// ([`core_check`]).
    checks: Vec<(Check, bool)>,
}

impl<'a, 'p> Fn_<'a, 'p> {
    /// Emit the rows held back for the read an exit hands back, in the order
    /// the core stated them.
    fn core_releases(&mut self, m: &mut Module, b: &mut Frame) -> Result<(), String> {
        for (name, holes) in std::mem::take(&mut self.core_rows) {
            self.core_release(m, b, name, &holes)?;
        }
        Ok(())
    }

    /// Emit the release a core row states, where the row stands, around the row's holes.
    ///
    /// A name with no slot releases nothing: the walk registers a slot for every layout it
    /// makes, and a body it takes holds no other heap-owning value ([`Fn_::core_walkable`]).
    fn core_release(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: Name,
        holes: &[String],
    ) -> Result<(), String> {
        let body = self.body();
        let Some(step) = body.names[name.index()].binding else {
            return unsupported("a release row whose binding the plan does not key", 0);
        };
        let Some(r) = self.rel_slots.get(&step) else {
            return Ok(());
        };
        let (place, rel) = (r.place, around(r.rel.clone(), holes));
        self.emit_rel(m, b, place, &rel, 0)
    }

    /// Emit a release the core states as a statement (a `drop`, a temporary its reading site
    /// frees, a payload binder at its arm's end, an edge's release) at the name's place,
    /// around the row's holes.
    #[allow(clippy::too_many_arguments)]
    fn core_drop(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        n: Name,
        holes: &Option<Vec<String>>,
        line: usize,
    ) -> Result<(), String> {
        let body = self.body();
        let Some((place, ty)) = self.core_place(n) else {
            return unsupported("a release of a name with no place", line);
        };
        let info = &body.names[n.index()];
        let rel = match info.binding {
            Some(key) => self.rel_owed(key, &ty, line)?,
            None => self.rel_for(&ty, line)?,
        };
        let Some(rel) = rel else {
            return Ok(());
        };
        let rel = around(rel, body.drop_holes(n, holes));
        self.emit_rel(m, b, place, &rel, line)
    }

    /// Read a tag and choose an arm, off the row.
    ///
    /// The arms are a chain of `if`s in one `block`, tested by [`Fn_::tag_is`]. There is no
    /// join: the switch carries no value, and each arm stores its own into the name the reader
    /// bound. A layout the arm reads out of the scrutinee binds as its address inside the
    /// scrutinee; any other payload moves into a slot.
    #[allow(clippy::too_many_arguments)]
    fn core_switch(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        on: &Val,
        arms: &[Arm],
        owns: bool,
        line: usize,
    ) -> Result<(), String> {
        let body = self.body();
        let Val::Name(n) = on else {
            return unsupported("a switch on a value the row does not name", line);
        };
        let Some((place, sty)) = self.core_place(*n) else {
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
        // The arm frees the boxes the binders come out of when the construct owns the value
        // ([`Fn_::frees_boxes`]); no row releases it whole afterwards.
        let free_box = owns;
        // A named `Bool` is the arm's own probe: the local that holds it.
        let mut probes = Vec::new();
        for a in arms {
            probes.push(match a.test {
                Test::Tag(t) => (Some(t as usize), None),
                Test::Else => (None, None),
                Test::Holds(h) => match self.core_place(h) {
                    Some((Place::Local(l), _)) => (Some(0), Some(l)),
                    _ => return unsupported("a switch on a predicate with no local", line),
                },
            });
        }
        let tags: Vec<Option<usize>> = probes.iter().map(|p| p.0).collect();
        // The switch carries no value, so the join is empty.
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
            // A payload's slot depends on the widths before it, so the
            // binder needs the variant's whole payload list. The row binds in payload order.
            let ptys: Vec<Type> = match tags[ix] {
                Some(t) => sum[t].payload.clone(),
                None => Vec::new(),
            };
            let from = b.mark();
            let scope = self.scope.len();
            for (i, bn) in arm.binds.iter().enumerate() {
                let ty = body.names[bn.index()].ty.clone();
                let layout = matches!(self.cx.repr(&ty, line)?, Repr::Agg(_));
                let read = arm
                    .reads(on)
                    .iter()
                    .any(|r| matches!(r, St::Let(x, _) if x == bn));
                let at = match self.word2(&ty)? {
                    // A layout the row reads out of the scrutinee is read
                    // where it lies ([`Arm::reads`]).
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
                self.core_bind(*bn, at, ty)?;
            }
            let to = b.mark();
            self.core_stmts(m, b, &arm.body[arm.reads(on).len()..])?;
            // A binder's scope is its arm, so its name and its slots go back
            // at the arm's end.
            self.scope.truncate(scope);
            if from < to {
                b.give_back(from, to);
            }
            self.chain_leave(b, &chain, slot);
        }
        self.chain_close(b, &chain);
        Ok(())
    }

    /// The frame's core body.
    ///
    /// Every caller runs under [`lower_body`]'s or [`lower_globals_init`]'s screen, which refuses
    /// a frame whose core is `None`.
    fn body(&self) -> &'a vyrn_frontend::core::Body {
        self.core.expect("a walk runs only in a frame with a core")
    }

    /// Reset the walk state for `core`, one entry per name; [`Fn_::core_body`] walks it.
    fn core_enter(&mut self, core: &vyrn_frontend::core::Body) {
        self.facts = Default::default();
        self.lists = Default::default();
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
            checks: Vec::new(),
        };
    }

    /// Emit one function body from the core's statements; call [`Fn_::core_enter`] first. The
    /// emitter reads [`vyrn_frontend::core::Body`] and decides nothing.
    ///
    /// Runs only where [`Fn_::core_walkable`] says the rows carry the whole body;
    /// [`lower_body`] refuses any other.
    fn core_body(&mut self, m: &mut Module, b: &mut Frame) -> Result<(), String> {
        self.core_stmts(m, b, &self.body().stmts)?;
        match self.core_w.checks.iter().find(|(_, ran)| !ran) {
            Some((c, _)) => unsupported("a check row no construct ran", c.site.line),
            None => Ok(()),
        }
    }

    /// Lift each lambda literal a call row in `rows` targets, at its slot's type, so the screen
    /// finds its signature ([`Cx::lambda_sig`]). [`lower_body`] calls it once, before either
    /// screen.
    fn core_lift_targets(&mut self, m: &mut Module, rows: &[St]) {
        let mut lambdas = Vec::new();
        for (x, _) in rows.iter().flat_map(St::rows) {
            let (St::Let(_, rhs) | St::Do { rhs, .. }) = x else {
                continue;
            };
            if let Rhs::Call {
                targets,
                kind: Callee::Fn(_) | Callee::Bound,
                ..
            } = rhs
            {
                lambdas.extend(targets.iter().filter_map(|t| match t {
                    Target::Lambda(key, caps, slot) => Some((key, caps, slot)),
                    _ => None,
                }));
            }
        }
        for (key, caps, slot) in lambdas {
            let Type::Fn(ptys, ret) = slot else { continue };
            if self.cx.lambda_sig(key).is_some() || vyrn_frontend::types::mentions_param(slot) {
                continue;
            }
            let Some(lit) = self.literal(key) else {
                continue;
            };
            let _ = self.lift_lambda(m, lit, ptys, ret, Some(caps), lit.line());
        }
    }

    /// Where the row that wrote part `v` built it, taken once ([`Fn_::core_part_at`]).
    fn part_built(&mut self, v: &Val) -> Option<Dest> {
        match v {
            Val::Name(n) => self.core_w.built[n.index()].take(),
            Val::Lit(_) => None,
        }
    }

    /// Where one of the core's names lives: the place this walk bound it at, or
    /// the scope's.
    fn core_place(&self, n: Name) -> Option<(Place, Type)> {
        let body = self.body();
        if let Some(p) = self.core_w.at[n.index()].clone() {
            return Some(p);
        }
        let info = &body.names[n.index()];
        // `@` starts only a naming-pass temporary, which this walk binds or nobody does.
        if info.source.starts_with('@') {
            return None;
        }
        self.lookup(&info.source, info.line).ok()
    }

    /// Under the profile instrument, makes row `s`'s site the current one before the row runs,
    /// so a block the row makes, or a callee outside the root file makes for it, counts at the
    /// row's line. A row that costs nothing leaves the last site as it was.
    fn core_site(&self, b: &mut Frame, s: &St) {
        let (Some(p), body) = (&self.cx.profile, self.body()) else {
            return;
        };
        let Some((line, kind)) = vyrn_lower::insight::row(s, body, &self.cx.world) else {
            return;
        };
        if body.file.is_some() {
            return;
        }
        // A kept check makes no block; counting its runs is not part of this instrument.
        let Some(verb) = kind
            .verb()
            .filter(|v| *v != vyrn_lower::insight::Verb::Keeps)
        else {
            return;
        };
        let key = (self.owner.clone(), line, verb);
        let id = *p.ids.borrow_mut().entry(key.clone()).or_insert_with(|| {
            let mut sites = p.sites.borrow_mut();
            sites.push(key);
            sites.len() as u32
        });
        b.ins(&Instruction::I32Const(id as i32));
        b.ins(&Instruction::GlobalSet(SITE));
    }

    fn core_stmts(&mut self, m: &mut Module, b: &mut Frame, ss: &[St]) -> Result<(), String> {
        let body = self.body();
        let ends = vyrn_lower::core::extent_ends(ss, &self.core_w.occurs);
        let mut due = Vec::new();
        let (mut last, mut mark): (Option<usize>, u32) = (None, b.mark());
        for (i, s) in ss.iter().enumerate() {
            if let St::Check(c) = s {
                self.core_w.checks.push((c.clone(), false));
                continue;
            }
            if let Some(j) = last {
                core_row_done(b, &self.core_w, &ss[j], mark);
                self.core_give_back(b, &mut due, &ends[j]);
            }
            (last, mark) = (Some(i), b.mark());
            self.core_site(b, s);
            if let St::Store {
                place: vyrn_frontend::core::Place::Name(n),
                line,
                ..
            } = s
            {
                if self.core_w.at[n.index()].is_none() && self.core_joins(*n) {
                    let ty = body.names[n.index()].ty.clone();
                    let r = self.cx.repr(&ty, *line)?;
                    let off = self.core_slot(b, *n, &r, *line)?;
                    self.core_bind(*n, Place::Slot(off), ty)?;
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
                ) if self.core_rebuild(rhs) => {
                    let line = body.names[n.index()].line;
                    let Some(((Arg::Val(Val::Name(x)), _), rest)) = args.split_first() else {
                        return unsupported("a rebuild of no named receiver", line);
                    };
                    let Some(rest) = arg_vals(rest) else {
                        return unsupported("a rebuild with a place argument", line);
                    };
                    if body.names[x.index()].grows {
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
                            Some(g) => match (self.global(g, line)?.0, self.cx.gappend.get(g)) {
                                (at @ Place::Static(_), Some(&word)) => (at, Place::Static(word)),
                                _ => return unsupported("an append with no ownership word", line),
                            },
                            None => {
                                let Some((Place::Local(l), _)) = self.core_place(*x) else {
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
                        self.append_in_place(m, b, place, own, *releases, &rest, line)?;
                    } else {
                        self.core_call(m, b, callee, *kind, &[], &[], args, None, None, line)?;
                        b.ins(&Instruction::Drop);
                    }
                    self.core_w.at[n.index()] = self.core_place(*x);
                    // With no store to put it back, the result holds the
                    // receiver's slot for its own extent, unless the receiver
                    // lives on to be stored again: a join after the rebuild
                    // puts the result back into it.
                    if self
                        .core_rebuilt(ss, i + 1 + drops_ahead(ss[i + 1..].iter()))
                        .is_none()
                        && !self.core_restored(*x)
                    {
                        self.core_w.slot[n.index()] = self.core_w.slot[x.index()].take();
                    }
                }
                // The head of a `for` over a stream: the element goes into a
                // place of its own, which the read at this row's name binds,
                // and the name is whether one came.
                St::Let(
                    n,
                    Rhs::Call {
                        callee, kind, args, ..
                    },
                ) if matches!(core_builtin(self.cx, callee, *kind), Some(Spec::Pulls)) => {
                    let line = body.names[n.index()].line;
                    let [(Arg::Val(Val::Name(s)), _)] = args.as_slice() else {
                        return unsupported("a pull of no named stream", line);
                    };
                    let Some(elem) = self.core_stream_elem(*s) else {
                        return unsupported("a pull of no stream", line);
                    };
                    let src = self.core_addr_local(b, *s, line)?;
                    let r = self.cx.repr(&elem, line)?;
                    let place = self.place_for(b, &r, line)?;
                    let has = self.stream_next(m, b, src, place, &elem, line)?;
                    self.core_w.pulled[n.index()] = Some((place, elem));
                    self.core_bind(*n, Place::Local(has), Type::Bool)?;
                }
                St::Let(n, Rhs::Read(vyrn_frontend::core::Place::Elem(s, c)))
                    if self.core_pulls(s) =>
                {
                    let line = body.names[n.index()].line;
                    let Some((place, ty)) = (match c {
                        Val::Name(c) => self.core_w.pulled[c.index()].take(),
                        Val::Lit(_) => None,
                    }) else {
                        return unsupported("an element read of a stream no pull wrote", line);
                    };
                    self.core_bind(*n, place, ty)?;
                }
                // The store that puts the rebuilt receiver back, which the
                // rebuild already wrote.
                St::Store { .. } if self.core_rebuilt(ss, i).is_some() => {}
                // A module-state receiver is read at its address by the
                // append, and by nothing else.
                St::Let(n, _) if core_global(body, *n).is_some() => {}
                // A layout made, built into the binding's own slot. The slot must exist before
                // the parts are written, and a record or array is never on the operand stack.
                St::Let(n, rhs) if self.core_makes(&self.core_made_ty(*n), rhs) => {
                    let info = &body.names[n.index()];
                    let line = info.line;
                    let taken = self.core_w.bufs[n.index()].take();
                    if let Some(dest) = self.core_part_dest(b, ss, i, line)? {
                        let Some(at) = self.core_part_at(ss, i) else {
                            return unsupported("a part with no parent", line);
                        };
                        let ty = at.ty;
                        mark = b.mark();
                        self.core_make(m, b, dest, &ty, rhs, taken, line)?;
                        continue;
                    }
                    // The storage a part took before this row.
                    let pre = self.core_w.built[n.index()].take();
                    if self.core_lands(ss, i, &self.core_w.reads) {
                        let dest = Dest::Addr(self.core_out(line)?, 0);
                        let ty = self.ret_ty.clone();
                        self.core_make(m, b, dest, &ty, rhs, taken, line)?;
                        self.core_w.landed = Some(*n);
                        continue;
                    }
                    let ty = self.core_made_ty(*n);
                    let r = self.cx.repr(&ty, line)?;
                    if !matches!(r, Repr::Agg(_)) {
                        return unsupported("a made layout with no layout", line);
                    }
                    let place = match pre {
                        Some(Dest::Slot(off)) => Place::Slot(off),
                        _ => Place::Slot(self.core_slot(b, *n, &r, line)?),
                    };
                    let dest = Dest::of(place).expect("a slot is a destination");
                    self.core_make(m, b, dest, &ty, rhs, taken, line)?;
                    self.core_bind(*n, place, ty)?;
                }
                // A layout taken out of a field in part position: the header moves to the part's
                // offset, as `consume t.d` in a literal does, and the field is the hole the
                // root's release carries.
                St::Let(n, Rhs::Take(p)) if self.core_part_at(ss, i).is_some() => {
                    let line = body.names[n.index()].line;
                    let Some(dest) = self.core_part_dest(b, ss, i, line)? else {
                        return unsupported("a taken layout with no parent", line);
                    };
                    let Repr::Agg(l) = self.cx.repr(&body.names[n.index()].ty, line)? else {
                        return unsupported("a taken layout with no layout", line);
                    };
                    dest.addr(b, 0);
                    let (_, off) = self.core_addr(m, b, p, line)?;
                    self.core_step(b, off);
                    agg_landed(b, l.size, false);
                }
                // A header a loop walks, taken apart once so every element and length read in
                // the loop reads the parts. The kernel ends the borrow at any write under the
                // container, so the parts cannot go stale.
                St::Let(n, Rhs::Read(p)) if self.core_walked(*n) => {
                    // An outer loop walks this header too, and nothing in it moves the header.
                    if let Some(walk) = core_header(&self.core_w, p) {
                        self.core_w.walks[n.index()] = Some(walk);
                        continue;
                    }
                    let info = &body.names[n.index()];
                    let (line, ty) = (info.line, info.ty.clone());
                    if let Repr::Agg(_) = self.cx.repr(&ty, line)? {
                        let (_, off) = self.core_addr(m, b, p, line)?;
                        self.core_step(b, off);
                    } else {
                        self.core_read(m, b, p, line)?;
                    }
                    self.core_w.walks[n.index()] = Some(self.walk(b, &ty, line)?);
                }
                // A layout read out of a place, or taken and handed back: the place's address in
                // a local, as for a layout parameter.
                St::Let(n, Rhs::Read(p) | Rhs::Take(p)) if self.core_alias(*n).is_some() => {
                    let line = body.names[n.index()].line;
                    let (ty, off) = self.core_addr(m, b, p, line)?;
                    self.core_step(b, off);
                    let Place::Local(l) = self.place_for(b, &Repr::Scalar(ValType::I32), line)?
                    else {
                        return unsupported("an address with no local", line);
                    };
                    b.ins(&Instruction::LocalSet(l));
                    self.core_bind(*n, Place::Local(l), ty)?;
                }
                // A literal's growable array: `@list` takes the literal, which was made in its
                // heap buffer, so the name takes its place.
                St::Let(n, _) if self.core_lists().iter().any(|(_, a)| a == n) => {
                    let info = &body.names[n.index()];
                    let Some(&(x, _)) = self.core_lists().iter().find(|(_, a)| a == n) else {
                        return unsupported("a list of no literal", info.line);
                    };
                    // A literal made as its parent's part has no place of its
                    // own: the part the parent reads is this name.
                    if let Some(d) = self.core_w.built[x.index()].take() {
                        self.core_w.built[n.index()] = Some(d);
                        continue;
                    }
                    let Some((place, _)) = self.core_place(x) else {
                        return unsupported("a list of a literal with no place", info.line);
                    };
                    self.core_w.slot[n.index()] = self.core_w.slot[x.index()].take();
                    self.core_bind(*n, place, info.ty.clone())?;
                }
                // A move of a layout: the name takes the moved name's place and slot extent.
                St::Let(n, Rhs::Val(Val::Name(x))) if self.core_renames(*n).is_some() => {
                    let info = &body.names[n.index()];
                    let Some((place, _)) = self.core_place(*x) else {
                        return unsupported("a move of a name with no place", info.line);
                    };
                    self.core_w.slot[n.index()] = self.core_w.slot[x.index()].take();
                    self.core_bind(*n, place, info.ty.clone())?;
                }
                St::Let(n, _) if self.core_copies(*n).is_some() => {
                    let line = body.names[n.index()].line;
                    let Some(p) = self.core_copies(*n) else {
                        return unsupported("a copy of no place", line);
                    };
                    let ty = body.names[n.index()].ty.clone();
                    let r = self.cx.repr(&ty, line)?;
                    let Repr::Agg(l) = &r else {
                        return unsupported("a copy of no layout", line);
                    };
                    let slot = self.core_slot(b, *n, &r, line)?;
                    b.slot(slot);
                    let (_, off) = self.core_addr(m, b, &p, line)?;
                    self.core_step(b, off);
                    agg_landed(b, l.size, false);
                    self.core_bind(*n, Place::Slot(slot), ty)?;
                }
                // An aggregate call result, written through the out-pointer ([`Fn_::out_ptr`])
                // into the binding's slot, or into the caller's storage when the next `return`
                // hands it back. Any other temporary keeps the storage the call wrote, handed on
                // to the reader or to the `match` or `for` the plan keys it by.
                St::Let(
                    n,
                    rhs @ (Rhs::Call { .. } | Rhs::Read(vyrn_frontend::core::Place::Key(..))),
                ) if self.core_agg_call(rhs) => {
                    let line = body.names[n.index()].line;
                    let lands = self.core_lands(ss, i, &self.core_w.reads);
                    let ty = if lands {
                        self.ret_ty.clone()
                    } else {
                        body.names[n.index()].ty.clone()
                    };
                    let r = self.cx.repr(&ty, line)?;
                    let Repr::Agg(l) = &r else {
                        return unsupported("an aggregate call with no layout", line);
                    };
                    let part = self.core_part_dest(b, ss, i, line)?;
                    mark = b.mark();
                    let back = self.core_back(ss, i, &ends[i]);
                    let (dest, place) = if lands {
                        (Some(Dest::Addr(self.core_out(line)?, 0)), None)
                    } else if part.is_some() {
                        (part, None)
                    } else if let Some((d, p, from)) = back {
                        if let Some(a) = from {
                            self.core_w.slot[n.index()] = self.core_w.slot[a.index()].take();
                        }
                        (Some(d), Some(p))
                    } else if body.names[n.index()].source.starts_with('@') {
                        (None, None)
                    } else {
                        let off = self.core_slot(b, *n, &r, line)?;
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
                        Rhs::Read(vyrn_frontend::core::Place::Key(base, k)) => {
                            let (mty, off) = self.core_addr(m, b, base, line)?;
                            self.core_step(b, off);
                            let mty = self.cx.resolve(&mty);
                            let Type::Map(_, val) = &mty else {
                                return unsupported("a key read of no map", line);
                            };
                            let val = (**val).clone();
                            self.map_at(m, b, &mty, &val, k, line)?;
                        }
                        _ => return unsupported("an aggregate row that is no call", line),
                    }
                    let used = std::mem::take(&mut self.dest_used);
                    match (dest, place) {
                        // The name holds the call's own slot, to the end of
                        // its extent.
                        (None, _) => {
                            self.core_w.slot[n.index()] = Some((from, b.mark()));
                            let a = b.local(ValType::I32);
                            b.ins(&Instruction::LocalSet(a));
                            self.core_bind(*n, Place::Local(a), ty)?;
                        }
                        (Some(_), place) => {
                            agg_landed(b, l.size, used);
                            match place {
                                Some(place) => self.core_bind(*n, place, ty)?,
                                None if part.is_some() => {}
                                None => self.core_w.landed = Some(*n),
                            }
                        }
                    }
                }
                St::Let(n, rhs) => {
                    let info = &body.names[n.index()];
                    let line = info.line;
                    match self.core_unmasked(ss, i, *n, rhs) {
                        Some(e) => self.core_val(m, b, e, &info.ty, line)?,
                        None => self.core_rhs(m, b, rhs, &info.ty, line)?,
                    }
                    if self.core_unit(&info.ty) {
                        continue;
                    }
                    // `blackBox` of a layout returns the operand's own address, so the name
                    // takes over the operand's slot, as a rename does.
                    let barrier = match rhs {
                        Rhs::Call {
                            callee, kind, args, ..
                        } if matches!(
                            core_builtin(self.cx, callee, *kind),
                            Some(Spec::Barrier)
                        ) =>
                        {
                            Some(args.as_slice())
                        }
                        _ => None,
                    };
                    if let Some([(Arg::Val(Val::Name(x)), _)]) = barrier {
                        self.core_w.slot[n.index()] = self.core_w.slot[x.index()].take();
                    }
                    // A temporary the next statement reads once, first, and nothing else reads
                    // stays on the operand stack. Every other name takes a local, in row order.
                    if info.binding.is_none()
                        && self.core_w.reads[n.index()] == 1
                        && self.core_first_read(next_row(ss, i)) == Some(*n)
                    {
                        self.core_w.held = Some(*n);
                        continue;
                    }
                    let r = self.cx.repr(&info.ty, line)?;
                    let place = match r {
                        Repr::Agg(_) if barrier.is_some() => Place::Local(b.local(ValType::I32)),
                        _ => self.place_for(b, &r, line)?,
                    };
                    let Place::Local(l) = place else {
                        return unsupported("a core `let` of an aggregate", line);
                    };
                    b.ins(&Instruction::LocalSet(l));
                    self.core_bind(*n, place, info.ty.clone())?;
                    if let (true, Some(site)) = (info.grows, info.binding) {
                        let literal = matches!(rhs, Rhs::Val(Val::Lit(Lit::Str(_))));
                        self.core_word(b, *n, l, site, literal);
                    }
                }
                St::Store {
                    place: vyrn_frontend::core::Place::Name(n),
                    value,
                    line,
                    ..
                } if self.core_unit(&body.names[n.index()].ty) => {
                    self.core_val(m, b, value, &Type::Unit, *line)?;
                }
                St::Store {
                    place: vyrn_frontend::core::Place::Name(n),
                    value,
                    line,
                    releases,
                    ..
                } if self.core_framed(&body.names[n.index()].ty) => {
                    // The temporary an `if` expression joins through is stored
                    // by each branch and bound by the `let` after them,
                    // so the first store it meets takes its slot.
                    let (l, ty) = match self.core_place(*n) {
                        Some((Place::Local(l), ty)) => (l, ty),
                        Some(_) => {
                            return unsupported("a core store into a place with no local", *line)
                        }
                        None => {
                            let ty = body.names[n.index()].ty.clone();
                            let r = self.cx.repr(&ty, *line)?;
                            let Place::Local(l) = self.place_for(b, &r, *line)? else {
                                return unsupported("a core store of an aggregate", *line);
                            };
                            self.core_bind(*n, Place::Local(l), ty.clone())?;
                            (l, ty)
                        }
                    };
                    // The store releases what the name held where the row says
                    // so: the old value aside, the new one in, the old one
                    // freed.
                    let snap = match (*releases, self.cx.repr(&ty, *line)?) {
                        (false, _) => None,
                        (true, Repr::Scalar(v)) => self.snap_word(b, l, v, &ty, *line)?,
                        (true, _) => {
                            return unsupported("a core store that releases an aggregate", *line)
                        }
                    };
                    self.core_val(m, b, value, &ty, *line)?;
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
                    place: vyrn_frontend::core::Place::Key(base, k),
                    value,
                    line,
                    releases,
                    ..
                } => {
                    let (mty, off) = self.core_addr(m, b, base, *line)?;
                    self.core_step(b, off);
                    let Type::Map(key_t, val) = self.cx.resolve(&mty) else {
                        return unsupported("a key store into no map", *line);
                    };
                    let hdr = b.local(ValType::I32);
                    b.ins(&Instruction::LocalSet(hdr));
                    let l = self.cx.layout(&mty, *line)?;
                    let kv = [k.clone(), value.clone()];
                    let parts = &kv;
                    self.map_set(m, b, hdr, &l, parts, 0, &key_t, &val, *releases, *line)?;
                }
                // `x = @t` after a call that ran in `x`'s storage ([`Fn_::core_back`]).
                St::Store {
                    place: vyrn_frontend::core::Place::Name(x),
                    value: Val::Name(t),
                    releases: false,
                    ..
                } if (self.core_place(*x))
                    .is_some_and(|p| Some(p.0) == self.core_place(*t).map(|q| q.0)) => {}
                // A place with an address, for `x = v`, `r.f = v` and `a[i] = v`: the address,
                // the old value kept aside where the row releases it, the value landed, the old
                // value freed. A layout lands as a byte copy. A module-state String has its
                // ownership word cleared.
                St::Store {
                    place,
                    value,
                    line,
                    releases,
                    holes,
                    ..
                } => {
                    let (ty, off) = self.core_addr(m, b, place, *line)?;
                    self.core_step(b, off);
                    let snap = if *releases {
                        let a = b.local(ValType::I32);
                        b.ins(&Instruction::LocalTee(a));
                        self.snap_at(b, a, &ty, *line)?
                            .map(|(p, rel)| (p, around(rel, holes)))
                    } else {
                        None
                    };
                    self.core_val(m, b, value, &ty, *line)?;
                    match self.cx.repr(&ty, *line)? {
                        Repr::Agg(l) => agg_landed(b, l.size, false),
                        _ => {
                            b.ins(&self.cx.store(&ty));
                        }
                    }
                    self.free_snap(m, b, snap, *line)?;
                    if let vyrn_frontend::core::Place::Global(g) = place {
                        if let Some(&word) = self.cx.gappend.get(g) {
                            disown(b, Place::Static(word));
                        }
                    }
                }
                // A loop's exit. The pass makes up one `break` (`site: 0`) in the else arm of a
                // two-way branch it also makes up at the loop's head, so this row is a
                // conditional branch out of the loop. The reader's own `if c { break }` has the
                // reader's site and is emitted as a plain two-way branch.
                St::If {
                    cond,
                    then,
                    els,
                    site: NodeId::NONE,
                } if then.is_empty()
                    && matches!(
                        els.as_slice(),
                        [St::Break {
                            site: NodeId::NONE,
                            ..
                        }]
                    )
                    && !self.loops.is_empty() =>
                {
                    self.core_val(m, b, cond, &Type::Bool, 0)?;
                    b.ins(&Instruction::I32Eqz);
                    let brk = self.loops.last().expect("a loop is open").0;
                    let out = self.br_to(brk);
                    b.ins(&Instruction::BrIf(out));
                }
                // `block { loop { .. br 0 } }`: `St::Break` targets the block and `St::Continue`
                // the loop. `St::Loop` is infinite, so the back edge is unconditional.
                St::Loop { body: inner, .. } => {
                    let over = self.core_w.over.len();
                    for p in ss[..i].iter().rev() {
                        match p {
                            St::Let(h, Rhs::Read(r))
                                if body.names[h.index()].walked
                                    == Some(vyrn_frontend::core::Walk::While) =>
                            {
                                self.core_w.over.push((r.clone(), *h))
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
                    let r = self.core_stmts(m, b, inner);
                    self.scope.truncate(scope);
                    self.loops.pop();
                    self.core_w.over.truncate(over);
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
                    self.core_val(m, b, cond, &Type::Bool, 0)?;
                    b.ins(&Instruction::If(BlockType::Empty));
                    self.depth += 1;
                    let mark = self.scope.len();
                    self.core_stmts(m, b, then)?;
                    self.scope.truncate(mark);
                    if !els.is_empty() {
                        b.ins(&Instruction::Else);
                        self.core_stmts(m, b, els)?;
                        self.scope.truncate(mark);
                    }
                    self.depth -= 1;
                    b.ins(&Instruction::End);
                }
                // `region { .. }` is the same block inside an arena scope. The depth is counted
                // here because it is a fact about the code being written.
                St::Block {
                    body: inner,
                    region,
                    ..
                } => {
                    let scope = self.scope.len();
                    if *region {
                        self.region_enter(b);
                        self.region_depth += 1;
                        let r = self.core_stmts(m, b, inner);
                        self.region_depth -= 1;
                        let mark = self.region_marks.pop().expect("one mark per open region");
                        r?;
                        self.region_exit(b, mark);
                    } else {
                        self.core_stmts(m, b, inner)?;
                    }
                    self.scope.truncate(scope);
                }
                St::Return { value, line, .. } => {
                    match (value, self.ret.agg().map(|l| l.size)) {
                        // The caller's storage, which the `let` before this
                        // row built into or which the value is copied into:
                        // [`Fn_::ret_value`]'s two cases. A value in the
                        // parameter it is left in ([`Sig::in_place`]) is there.
                        (Some(Val::Name(n)), Some(size)) => {
                            let out = self.core_out(*line)?;
                            let home =
                                self.core_place(*n).map(|(p, _)| p) == Some(Place::Local(out));
                            if self.core_w.landed.take() != Some(*n) && !home {
                                Dest::Addr(out, 0).addr(b, 0);
                                self.core_addr_of(b, *n, *line)?;
                                agg_landed(b, size, false);
                            }
                        }
                        (Some(v), _) => {
                            let want = self.ret_ty.clone();
                            self.core_val(m, b, v, &want, *line)?;
                        }
                        (None, _) if matches!(self.ret, Repr::Unit) => {}
                        (None, _) => {
                            return unsupported(
                                "a return whose value does not match the signature",
                                *line,
                            )
                        }
                    }
                    self.core_releases(m, b)?;
                    // Every region scope this return leaves pops rather than frees: a value
                    // built in a region points into the arena, and the caller owns it.
                    self.exit_regions_above(b, 0, false);
                    b.ins(&Instruction::Br(self.depth));
                }
                // An exit's releases run after the read it hands back: the value is on the
                // operand stack and a release does not disturb it, so the two commute.
                St::Row { name, holes, .. } => {
                    self.core_rows.push((*name, holes.clone()));
                    if !matches!(
                        next_row(ss, i),
                        Some(St::Row { .. } | St::Return { value: Some(_), .. })
                    ) {
                        self.core_releases(m, b)?;
                    }
                }
                St::Switch {
                    on,
                    arms,
                    owns,
                    line,
                    ..
                } => self.core_switch(m, b, on, arms, *owns, *line)?,
                St::Drop(n, _, line, holes) => self.core_drop(m, b, *n, holes, *line)?,
                St::Trap => {
                    b.ins(&Instruction::Unreachable);
                }
                // Pushed to `w.checks` at the head of the loop.
                St::Check(_) => {}
                // An expression for its effect. What it leaves on the stack
                // is dropped, or the enclosing block's type will not check.
                St::Do { rhs, line, .. } if self.core_checks_made(rhs).is_some() => {
                    let (decl, n) = self.core_checks_made(rhs).expect("the guard's");
                    self.core_rule_check(m, b, &decl, n, *line)?;
                }
                // A discarded layout read: its address, for the checks on the way.
                St::Do {
                    rhs: rhs @ Rhs::Read(p),
                    line,
                    ..
                } if !self.core_rhs_readable(rhs) => {
                    self.core_addr(m, b, core_discarded(p), *line)?;
                    b.ins(&Instruction::Drop);
                }
                St::Do { rhs, line, .. } => {
                    let got = self.core_rhs_ty(rhs, *line)?;
                    self.core_rhs(m, b, rhs, &got, *line)?;
                    if self.cx.repr(&got, *line)? != Repr::Unit {
                        b.ins(&Instruction::Drop);
                    }
                }
            }
        }
        if let Some(j) = last {
            core_row_done(b, &self.core_w, &ss[j], mark);
            self.core_give_back(b, &mut due, &ends[j]);
        }
        Ok(())
    }

    /// Places core name `n` in a slot and records the frame marks around it until
    /// [`Fn_::core_give_back`] hands the slot back.
    fn core_slot(&mut self, b: &mut Frame, n: Name, r: &Repr, line: usize) -> Result<u32, String> {
        let from = b.mark();
        let Place::Slot(off) = self.place_for(b, r, line)? else {
            return unsupported("a layout with no slot", line);
        };
        self.core_w.slot[n.index()] = Some((from, b.mark()));
        Ok(off)
    }

    /// Holds a slot for the ownership word of accumulator `n` (in local `l`) for `n`'s
    /// extent, as [`Fn_::core_slot`] does for a layout.
    fn core_word(&mut self, b: &mut Frame, n: Name, l: u32, site: NodeId, literal: bool) {
        let from = b.mark();
        let at = b.alloc(4, 4);
        self.core_w.slot[n.index()] = Some((from, b.mark()));
        self.str_append_shadow(b, l, at, site, literal);
    }

    /// Gives back the slots of names whose extent ended at the row just walked. A release
    /// deferred to the row's exit still reads its slot, so `due` waits until `core_rows` is empty.
    fn core_give_back(&mut self, b: &mut Frame, due: &mut Vec<Name>, ended: &[Name]) {
        due.extend_from_slice(ended);
        if self.core_rows.is_empty() {
            for n in due.drain(..) {
                if let Some((from, to)) = self.core_w.slot[n.index()].take() {
                    b.give_back(from, to);
                }
            }
        }
    }

    /// Emits a `let`'s right-hand side at type `want`.
    fn core_rhs(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        rhs: &Rhs,
        want: &Type,
        line: usize,
    ) -> Result<(), String> {
        match rhs {
            Rhs::Val(v) => self.core_val(m, b, v, want, line),
            Rhs::Prim(op, vs, ret) => {
                let got = self.core_prim(m, b, op, vs, line)?;
                if let (true, Some(r)) = (crate::observe::on(), ret) {
                    crate::observe::note_typing("prim", &format!("{op:?}"), &got, r);
                }
                let got = ret.clone().unwrap_or(got);
                self.coerce(m, b, &got, want, line)
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
                    callee,
                    *kind,
                    solved,
                    targets,
                    args,
                    ret.as_ref(),
                    None,
                    line,
                )?;
                if let (true, Some(r)) = (crate::observe::on(), ret) {
                    crate::observe::note_typing("call", callee, &got, r);
                }
                self.coerce(m, b, &got, want, line)
            }
            // A read and a take load the same address. The driver's release row walks around
            // the hole a take leaves.
            Rhs::Read(p) | Rhs::Take(p) => {
                let got = self.core_read(m, b, p, line)?;
                self.coerce(m, b, &got, want, line)
            }
            _ => unsupported("a core right-hand side this walk does not read", line),
        }
    }

    /// The operand `e` of `n = e & (bits - 1)` where `n` is read once, as the amount of a shift
    /// whose range check a pass proved. wasm's `shl` and `shr` mask their amount by the carrier's
    /// width, so the shift reads `e` as it reads `n`. Under the check oracle the proved check
    /// still runs on `n`, so the mask stays.
    fn core_unmasked<'r>(&self, ss: &[St], i: usize, n: Name, rhs: &'r Rhs) -> Option<&'r Val> {
        let Rhs::Prim(Op::Bin(BinOp::BitAnd), vs, _) = rhs else {
            return None;
        };
        let [e, Val::Lit(Lit::Int(c))] = vs.as_slice() else {
            return None;
        };
        let proved = |s: &St| match s {
            St::Check(k) => match k.guard {
                Guard::Shift(Val::Name(d), bits) => {
                    d == n
                        && k.verdict == Verdict::Proved
                        && matches!(bits, 32 | 64)
                        && *c == i64::from(bits) - 1
                }
                _ => false,
            },
            _ => false,
        };
        (self.cx.oracle.is_none()
            && self.core_w.reads[n.index()] == 1
            && ss[i + 1..]
                .iter()
                .flat_map(St::rows)
                .any(|(s, _)| proved(s)))
        .then_some(e)
    }

    /// The type a right-hand side produces, without emitting it. A `St::Do` needs it; a
    /// `St::Let` has the checker's type on its name.
    fn core_rhs_ty(&self, rhs: &Rhs, line: usize) -> Result<Type, String> {
        match rhs {
            Rhs::Call {
                callee,
                kind,
                args,
                ret: at,
                solved,
                targets,
                ..
            } => match core_builtin(self.cx, callee, *kind) {
                Some(Spec::Typed(_, ret) | Spec::Renders(ret) | Spec::Effect(ret)) => {
                    Ok(ret.clone())
                }
                Some(Spec::Traps) => Ok(Type::Never),
                Some(Spec::Asserts) => Ok(Type::Unit),
                Some(Spec::Logs) if callee == "logger" => Ok(Type::Logger),
                Some(Spec::Logs) => Ok(Type::Unit),
                // [`Fn_::lanes`] types a lane builtin as it emits; the row carries the
                // checker's type for the site.
                Some(Spec::Lanes) => at
                    .clone()
                    .ok_or_else(|| gap("a lane builtin the checker did not type", line)),
                // A removal returns the element, or an `Option` of it.
                Some(Spec::Removes) => at
                    .clone()
                    .ok_or_else(|| gap("a removal the checker did not type", line)),
                Some(Spec::Finds) => Ok(Type::Bool),
                Some(Spec::Barrier) => at
                    .clone()
                    .ok_or_else(|| gap("a `blackBox` the checker did not type", line)),
                Some(Spec::Host) => at
                    .clone()
                    .ok_or_else(|| gap("a host import the checker did not type", line)),
                _ => match self.core_mem_ty(callee, args.len()) {
                    Some(t) => Ok(t),
                    None => match self.core_sig(callee, *kind, solved, targets) {
                        Some(s) => Ok(s.ret_ty),
                        None if kind.direct() && self.is_extern(callee) => Ok(self
                            .cx
                            .externs
                            .get(callee)
                            .map_or(Type::Unit, |e| e.ret.clone())),
                        None => unsupported("a core call this walk does not read", line),
                    },
                },
            },
            Rhs::Read(p) => self
                .core_place_ty(p)
                .ok_or_else(|| gap("a discarded read the walk does not type", line)),
            _ => unsupported("a discarded value the row does not type", line),
        }
    }

    /// Emits one call. The ABI is the declaration's; the row states only who the callee is
    /// ([`Callee`]).
    fn core_call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
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
            return self.core_call_vals(m, b, callee, kind, solved, targets, &vs, ret, hint, line);
        }
        if self.core_user_callee(callee, kind) {
            return self.core_user_call(m, b, callee, kind, solved, targets, args, hint, line);
        }
        // A place receiver shrinks where it lies ([`Fn_::core_removes`]).
        let ([(Arg::Place(p), _), rest @ ..], Some(Spec::Removes)) =
            (args, core_builtin(self.cx, callee, kind))
        else {
            return unsupported("a place argument to a builtin other than a removal", line);
        };
        let Some(rest) = arg_vals(rest) else {
            return unsupported("a removal with two places", line);
        };
        let (aty, off) = self.core_addr(m, b, p, line)?;
        self.core_step(b, off);
        let slot = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(slot));
        self.core_remove(m, b, callee, slot, &aty, &rest, line)
    }

    /// [`Fn_::core_call`] with every argument a value.
    fn core_call_vals(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        callee: &str,
        kind: Callee,
        solved: &[(String, Type)],
        targets: &[Target],
        args: &[(Val, vyrn_frontend::ast::Capability)],
        ret: Option<&Type>,
        hint: Option<(Dest, Type)>,
        line: usize,
    ) -> Result<Type, String> {
        let body = self.body();
        // A `std/mem` primitive is its instructions, never a call; [`mem_ins`] holds the table.
        if let Some(prim) = callee.strip_prefix(vyrn_frontend::loader::MEM_PREFIX) {
            return self.core_mem(m, b, prim, args, ret, line);
        }
        // Every [`Spec`] kind is answered here, so a kind added to it fails to compile
        // until this match handles it.
        match core_builtin(self.cx, callee, kind) {
            Some(Spec::Typed(..)) => {
                let Some((params, ins, ret)) =
                    builtin_spec(callee, args.len(), self.cx.gen.is_some())
                else {
                    return unsupported("a specified builtin at another arity", line);
                };
                for ((v, _), p) in args.iter().zip(params) {
                    self.core_val(m, b, v, p, line)?;
                }
                b.ins(&ins);
                return Ok(ret.clone());
            }
            // `x.copy()`: the operand at its own type, then the duplication it asks for. A
            // type with `impl Copy for T` never reaches here; the core calls its declaration.
            Some(Spec::OwnType) => {
                let [(v, _)] = args else {
                    return unsupported("`copy` of other than one value", line);
                };
                let ty = self.core_ty(v, &Type::Int);
                self.core_val(m, b, v, &ty, line)?;
                self.copy_stack(m, b, &ty, line)?;
                return Ok(ty);
            }
            // `blackBox(v)`: a store to and a load from 16 bytes nothing else names, so clang
            // `-O2` on the native route cannot fold the measured work. Each site reserves its
            // own bytes: wasm2c forwards a global, and `data` shares identical contents.
            Some(Spec::Barrier) => {
                let [(v, _)] = args else {
                    return unsupported("`blackBox` of other than one value", line);
                };
                let Some(ty) = ret else {
                    return unsupported("a `blackBox` the checker did not type", line);
                };
                self.core_val(m, b, v, ty, line)?;
                let Some(t) = self.cx.repr(ty, line)?.val() else {
                    return Ok(ty.clone());
                };
                let addr = m.reserve(16, 16) as i32;
                let tmp = self.scratch(b, t, 9);
                let at = |align| mem_arg(0, align);
                let (store, load) = match t {
                    ValType::I32 => (Instruction::I32Store(at(2)), Instruction::I32Load(at(2))),
                    ValType::I64 => (Instruction::I64Store(at(3)), Instruction::I64Load(at(3))),
                    ValType::F32 => (Instruction::F32Store(at(2)), Instruction::F32Load(at(2))),
                    ValType::F64 => (Instruction::F64Store(at(3)), Instruction::F64Load(at(3))),
                    _ => (Instruction::V128Store(at(4)), Instruction::V128Load(at(4))),
                };
                b.ins(&Instruction::LocalSet(tmp))
                    .ins(&Instruction::I32Const(addr))
                    .ins(&Instruction::LocalGet(tmp))
                    .ins(&store)
                    .ins(&Instruction::I32Const(addr))
                    .ins(&load);
                return Ok(ty.clone());
            }
            Some(Spec::Renders(ret)) => {
                let [(v, _)] = args else {
                    return unsupported("a rendering of other than one value", line);
                };
                let ty = self.core_ty(v, &Type::Int);
                self.core_val(m, b, v, &ty, line)?;
                match ret {
                    Type::Unit => self.print_value(b, &ty, line)?,
                    _ => self.str_value(b, &ty, line)?,
                }
                return Ok(ret.clone());
            }
            // `panic(msg)` and `@panicAt(msg, site)`: the next row emits the `unreachable`.
            Some(Spec::Traps) => {
                let [(v, _), site @ ..] = args else {
                    return unsupported("`panic` with other than one argument", line);
                };
                let at = match site {
                    [(Val::Lit(Lit::Str(at)), _)] => Some(at.as_str()),
                    _ => None,
                };
                self.panic_line(m, b, at, |s, m, b| s.core_val(m, b, v, &Type::Str, line))?;
                return Ok(Type::Never);
            }
            Some(Spec::Asserts) => {
                return self.asserts(m, b, callee, args, line);
            }
            // `xs.push(v)` and its siblings, and `m.tally(k, n)`: the call rebuilds the
            // receiver in place at its address.
            Some(Spec::Rebuilds) => {
                let [(Val::Name(x), _), rest @ ..] = args else {
                    return unsupported("a rebuild of no named receiver", line);
                };
                let aty = body.names[x.index()].ty.clone();
                self.core_addr_of(b, *x, line)?;
                if let ("@tally" | "@tallyBytes", [(k, _), (n, _)]) = (callee, rest) {
                    let hdr = b.local(ValType::I32);
                    b.ins(&Instruction::LocalSet(hdr));
                    return match callee {
                        "@tally" => self.map_tally(m, b, hdr, &aty, k, n, line),
                        _ => self.map_tally_bytes(m, b, hdr, &aty, k, n, line),
                    };
                }
                return self.arr_rebuild(m, b, callee, &aty, rest, line);
            }
            // A SIMD builtin's lane index is the literal the row carries.
            Some(Spec::Lanes) => {
                let span = match args {
                    [(Val::Name(x), _), (i, _), ..] => core_check(
                        &mut self.core_w,
                        line,
                        |g| matches!(g, Guard::Span(vyrn_frontend::core::Place::Name(p), v, _) if p == x && v == i),
                    )
                    .map(|c| self.row(b, c)),
                    _ => unsupported("a runtime check the core did not state", line),
                };
                return self.lanes(m, b, callee, args, span, line);
            }
            // A generator host import: `@codeSplice` takes its tag from the type the row put
            // on its operand.
            Some(Spec::Host) => {
                return self.host(m, b, callee, args, line);
            }
            // `xs.pop()`, `xs.swapRemove(i)` and `m.remove(k)`: the call shrinks the
            // receiver in place at its address.
            Some(Spec::Removes) => {
                let [(Val::Name(x), _), rest @ ..] = args else {
                    return unsupported("a removal from no named receiver", line);
                };
                let aty = body.names[x.index()].ty.clone();
                let slot = b.local(ValType::I32);
                self.core_addr_of(b, *x, line)?;
                b.ins(&Instruction::LocalSet(slot));
                return self.core_remove(m, b, callee, slot, &aty, rest, line);
            }
            Some(Spec::Finds) => {
                let [(mv, _), (kv, _)] = args else {
                    return unsupported(&format!("`{callee}` at this arity"), line);
                };
                let mty = self.core_ty(mv, &Type::Int);
                self.core_val(m, b, mv, &mty, line)?;
                let hdr = b.local(ValType::I32);
                b.ins(&Instruction::LocalSet(hdr));
                let (idx, ..) = self.map_find(m, b, hdr, &mty, kv, line)?;
                b.ins(&Instruction::LocalGet(idx));
                b.ins(&Instruction::I32Const(0));
                b.ins(&Instruction::I32GeS);
                return Ok(Type::Bool);
            }
            // Built in a slot of the call's own, which [`agg_landed`] copies into the
            // destination.
            Some(Spec::Builds(_)) => {
                let range = match (callee, args) {
                    ("bytes", [(s, _), (from, _), (to, _)]) => {
                        let c = core_check(
                            &mut self.core_w,
                            line,
                            |g| matches!(g, Guard::Range(x, y, z) if (x, y, z) == (s, from, to)),
                        )?;
                        if c.verdict == Verdict::Proved {
                            return Err(gap(
                                "a proved `bytes` range: `bytesOf` checks inside",
                                line,
                            ));
                        }
                        match self.row(b, c).map(|c| c.rule) {
                            Some(Raises::Row(rule)) => Some(rule),
                            Some(Raises::Where) => {
                                return Err(gap("a `bytes` range check with no trap row", line))
                            }
                            None => None,
                        }
                    }
                    _ => None,
                };
                return match (callee, args) {
                    ("bytes", _) => self.bytes_of(m, b, range, args, line),
                    ("stringFromBytes", _) => self.string_from_bytes(m, b, args, line),
                    ("@toArray", [(v, _)]) => {
                        let aty = self.core_ty(v, &Type::Int);
                        self.core_val(m, b, v, &aty, line)?;
                        let hdr = b.local(ValType::I32);
                        b.ins(&Instruction::LocalSet(hdr));
                        self.sa_to_array(m, b, hdr, &aty, line)
                    }
                    ("fromArray", [(v, _)]) => {
                        let aty = self.core_ty(v, &Type::Int);
                        let Type::Array(inner) = self.cx.resolve(&aty) else {
                            return unsupported(&format!("`fromArray` of `{aty}`"), line);
                        };
                        self.core_val(m, b, v, &aty, line)?;
                        self.stream_from_array(b, &inner, line)
                    }
                    ("fromStep", [_, _, _]) => self.stream_from_step(m, b, args, line),
                    // The element type is the row's result: an address is an `Int64` and
                    // carries none.
                    ("unboxStream" | "pullAt", [(v, _)]) => {
                        let ret = ret.map(|t| self.cx.resolve(t));
                        match (callee, ret) {
                            ("unboxStream", Some(Type::Stream(elem))) => {
                                self.stream_unbox(m, b, &elem, v, line)
                            }
                            ("pullAt", Some(opt)) => match ftypes::option_payload(&opt) {
                                Some(elem) => self.stream_pull_at(m, b, elem, v, line),
                                None => unsupported("a `pullAt` of no Option", line),
                            },
                            _ => unsupported(&format!("`{callee}` with no result type"), line),
                        }
                    }
                    ("@keys", [(v, _)]) => {
                        let mty = self.core_ty(v, &Type::Int);
                        self.core_val(m, b, v, &mty, line)?;
                        let hdr = b.local(ValType::I32);
                        b.ins(&Instruction::LocalSet(hdr));
                        self.map_keys(m, b, hdr, &mty, line)
                    }
                    _ => unsupported(&format!("`{callee}` as a built value"), line),
                };
            }
            Some(Spec::Effect(_)) => {
                let [_] = args else {
                    return unsupported(&format!("`{callee}` of other than one value"), line);
                };
                return self.effect(m, b, callee, args, line);
            }
            Some(Spec::Logs) => {
                return self.logs(m, b, callee, args, line);
            }
            // A pull binds two names, and [`Fn_::core_stmts`] emits it.
            Some(Spec::Pulls) => return unsupported("a pull apart from its loop head", line),
            // A routed builtin is a declared call, through the signature
            // [`Fn_::core_sig`] answers for the function it names.
            Some(Spec::Routes(_)) | None => {}
        }
        // `T(v)` of a validated type: the operand at the base, then the check, which does
        // not run on a crossing the checker proved (`Callee::Proven`).
        if let Some(decl) = self.core_named(callee, kind) {
            let [(v, _)] = args else {
                return unsupported(&format!("`{callee}` at this arity"), line);
            };
            self.core_val(m, b, v, &decl.base, line)?;
            if kind == Callee::Named {
                self.emit_validation(b, &decl, line)?;
            }
            return Ok(Type::Named(decl.name));
        }
        if kind.direct() && self.is_extern(callee) {
            return self.extern_call(m, b, callee, args, line);
        }
        let args: Vec<_> = args
            .iter()
            .map(|(v, c)| (Arg::Val(v.clone()), *c))
            .collect();
        self.core_user_call(m, b, callee, kind, solved, targets, &args, hint, line)
    }

    /// Calls a declared function. A place argument ([`Arg`]) crosses as its address, as a
    /// layout name does.
    #[allow(clippy::too_many_arguments)]
    fn core_user_call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        callee: &str,
        kind: Callee,
        solved: &[(String, Type)],
        targets: &[Target],
        args: &[(Arg, vyrn_frontend::ast::Capability)],
        hint: Option<(Dest, Type)>,
        line: usize,
    ) -> Result<Type, String> {
        // A call through a stored value calls the signature's dispatcher, with the value as
        // the leading argument.
        let through: Vec<(Arg, vyrn_frontend::ast::Capability)>;
        let mut spliced = Vec::new();
        let (sig, args) = match self.core_through(kind) {
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
                    (None, None) => match self.core_sig(callee, kind, solved, targets) {
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
        for (i, ((a, c), p)) in args.iter().zip(&sig.params).enumerate() {
            // `x = f(x)` passes `x`'s own storage as the destination ([`Fn_::core_back`]).
            let home = match (a, dest) {
                (Arg::Val(Val::Name(x)), Some((d, _))) => {
                    (self.core_place(*x)).is_some_and(|(pl, _)| d.holds(pl))
                }
                _ => false,
            };
            self.moved_in(b, &sig, dest, i, home, |s, b| {
                match (a, c) {
                    (Arg::Val(Val::Name(n)), vyrn_frontend::ast::Capability::Modify)
                        if !matches!(s.cx.repr(p, line)?, Repr::Agg(_)) =>
                    {
                        let Some((Place::Local(l), ty)) = s.core_place(*n) else {
                            return unsupported("a `modify` argument with no local", line);
                        };
                        spilled.push(s.spill(b, l, &ty, line)?);
                    }
                    (Arg::Val(v), _) => s.core_val(m, b, v, p, line)?,
                    (Arg::Place(pl), _) => {
                        let (_, off) = s.core_addr(m, b, pl, line)?;
                        s.core_step(b, off);
                    }
                }
                Ok(())
            })?;
        }
        b.ins(&Instruction::Call(sig.index));
        reload(b, &spilled);
        self.out_ptr_back(b, dest);
        Ok(sig.ret_ty)
    }

    /// Reads a place: the address from [`Fn_::core_addr`], then one scalar load at the
    /// value's type with the offset folded into the access. A place is never copied to
    /// read a field of it.
    ///
    /// A take reads the same address. The driver places the hole and its release, so a
    /// body holding one stands down at [`Fn_::core_walkable`].
    fn core_read(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        p: &vyrn_frontend::core::Place,
        line: usize,
    ) -> Result<Type, String> {
        let body = self.body();
        // A scalar name lives in a wasm local and has no address, so the read is the local.
        if let vyrn_frontend::core::Place::Name(n) = p {
            let ty = body.names[n.index()].ty.clone();
            self.core_val(m, b, &Val::Name(*n), &ty, line)?;
            return Ok(ty);
        }
        // A length is a header read ([`Fn_::length_of`]) of the base's value: an address
        // for a layout, the pointer for a String.
        if let vyrn_frontend::core::Place::Field(base, f) = p {
            if let Some(walk) =
                core_header(&self.core_w, base).filter(|_| f == "length" || f == "byteLength")
            {
                b.ins(&Instruction::LocalGet(walk.len));
                return Ok(Type::Int);
            }
            if let Some(bty) = self
                .core_place_ty(base)
                .filter(|t| length_ty(f, &self.cx.resolve(t)).is_some())
            {
                if let Repr::Agg(_) = self.cx.repr(&bty, line)? {
                    let (_, off) = self.core_addr(m, b, base, line)?;
                    self.core_step(b, off);
                } else {
                    self.core_read(m, b, base, line)?;
                }
                return self
                    .length_of(b, &bty, f, line)?
                    .ok_or_else(|| gap("a length of no container", line));
            }
        }
        let (ty, off) = self.core_addr(m, b, p, line)?;
        let Repr::Scalar(_) = self.cx.repr(&ty, line)? else {
            return unsupported("a read of a place this walk does not load", line);
        };
        b.ins(&self.cx.load(&ty, off.unwrap_or(0)));
        Ok(ty)
    }

    /// A removal from the receiver of type `aty` whose address is in `slot`.
    fn core_remove(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        callee: &str,
        slot: u32,
        aty: &Type,
        rest: &[(Val, vyrn_frontend::ast::Capability)],
        line: usize,
    ) -> Result<Type, String> {
        match (callee, rest) {
            ("@pop", []) => self.pop_at(b, slot, aty, line),
            ("@swapRemove", [(i, _)]) => {
                let row = core_check(
                    &mut self.core_w,
                    line,
                    |g| matches!(g, Guard::Index(_, v) if v == i),
                )?;
                let row = self.row(b, row);
                self.swap_remove_at(m, b, slot, aty, row, i, line)
            }
            ("@remove", [(k, _)]) => self.map_remove(m, b, slot, aty, k, line),
            _ => unsupported(&format!("`{callee}` at this arity"), line),
        }
    }

    /// Pushes a place's address and returns its type and the offset the load still owes.
    /// `Some(off)` is a field step: a scalar load folds `off` into its access, an aggregate
    /// step adds it ([`Fn_::core_step`]). `None` means the address is the place's own.
    fn core_addr(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        p: &vyrn_frontend::core::Place,
        line: usize,
    ) -> Result<(Type, Option<u32>), String> {
        use vyrn_frontend::core::Place as At;
        match p {
            At::Name(n) => {
                let Some((place, ty)) = self.core_place(*n) else {
                    return unsupported("a core place with no storage", line);
                };
                match place {
                    // An aggregate in a local holds its address.
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
            // Module state is storage at a fixed address.
            At::Global(name) => {
                let (place, ty) = self.global(name, line)?;
                let Place::Static(at) = place else {
                    return unsupported("module state that is not static", line);
                };
                b.ins(&Instruction::I32Const(at as i32));
                Ok((ty, None))
            }
            At::Field(base, f) => {
                let (bty, off) = self.core_addr(m, b, base, line)?;
                self.core_step(b, off);
                let (at, fty) = self.field_of(&bty, f, line)?;
                Ok((thunk_of(fty), Some(at)))
            }
            // The header read, the bounds check, and one stride multiply. A String is walked
            // from its pointer, which is its value. Its element is a `UInt8`, where the walk
            // hands a `for` the byte widened.
            At::Elem(base, i) => {
                let walk = match core_header(&self.core_w, base) {
                    Some(walk) => walk,
                    None => {
                        let bty = match self.core_place_ty(base) {
                            Some(t) if self.cx.resolve(&t) == Type::Str => {
                                self.core_read(m, b, base, line)?
                            }
                            _ => {
                                let (bty, off) = self.core_addr(m, b, base, line)?;
                                self.core_step(b, off);
                                bty
                            }
                        };
                        self.walk(b, &bty, line)?
                    }
                };
                let row = core_check(
                    &mut self.core_w,
                    line,
                    |g| matches!(g, Guard::Index(p, v) if p == &**base && v == i),
                )?;
                let row = self.row(b, row);
                self.core_val(m, b, i, &Type::Int, line)?;
                let ix = b.local(ValType::I64);
                b.ins(&Instruction::LocalSet(ix));
                if let Some(row) = row {
                    self.bounds_check(b, &walk, ix, &row);
                }
                self.elem_addr(b, &walk, ix);
                let byte = Type::IntN {
                    bits: 8,
                    signed: false,
                };
                Ok((if walk.byte { byte } else { walk.elem }, None))
            }
            // A key has no address; the aggregate `let` arm of [`Fn_::core_stmts`] emits its
            // lookup.
            At::Key(..) => unsupported("a read of a key", line),
        }
    }

    /// Adds a field step's offset to the address on the stack. An aggregate field needs
    /// it; a scalar load folds it.
    fn core_step(&self, b: &mut Frame, off: Option<u32>) {
        if let Some(off) = off {
            b.ins(&Instruction::I32Const(off as i32));
            b.ins(&Instruction::I32Add);
        }
    }

    /// The type of a place, without emitting it; asks what [`Fn_::core_addr`] walks.
    fn core_place_ty(&self, p: &vyrn_frontend::core::Place) -> Option<Type> {
        let body = self.body();
        use vyrn_frontend::core::Place as At;
        match p {
            At::Name(n) => Some(body.names[n.index()].ty.clone()),
            At::Global(name) => {
                let (place, ty) = self.global(name, 0).ok()?;
                matches!(place, Place::Static(_)).then_some(ty)
            }
            At::Field(base, f) => {
                let bty = self.core_place_ty(base)?;
                if let Some(t) = length_ty(f, &self.cx.resolve(&bty)) {
                    return Some(t);
                }
                Some(thunk_of(self.field_of(&bty, f, 0).ok()?.1))
            }
            At::Elem(base, _) => match self.cx.resolve(&self.core_place_ty(base)?) {
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

    /// Whether `n` is a header a loop walks: a borrow of an array, a small array or a
    /// String that a `for` binds, or that a `while` indexes and never rebuilds.
    fn core_walked(&self, n: Name) -> bool {
        let body = self.body();
        let info = &body.names[n.index()];
        info.walked.is_some()
            && info.borrow
            && matches!(
                self.cx.resolve(&info.ty),
                Type::Array(_) | Type::SmallArray(..) | Type::Str
            )
    }

    /// The place a layout name holds the address of, or `None` where the program can
    /// observe the copy.
    ///
    /// A place is never copied to read it, so a borrow bound by a read of a layout is that
    /// place for the read's extent. The kernel ends the alias at every store, take and drop
    /// of the place, and refuses a read after one.
    ///
    /// A layout that owns no heap is a borrow only where the core minted the name (a `for`
    /// head, a scrutinee or an argument). A `let` the reader wrote binds a value, and so does a
    /// minted name this refuses ([`Fn_::core_copies`]): `x = f(x, x.p)` reads a copy of `x.p`.
    /// An owned name read out of an element is the element: the container's release frees
    /// only the buffer ([`Fn_::rel_owed`]), and the name's release is its own row.
    ///
    /// `None` where the body stores into the binding or hands it to `modify`, where a store
    /// into an Array element writes the buffer ([`core_written`]), or where a row of the
    /// extent hands a root on the chain to `consume` or `modify`
    /// ([`vyrn_lower::kernel::modifies`]) or rebuilds it as a receiver (`out.push(v)`).
    fn core_alias(&self, n: Name) -> Option<&'a vyrn_frontend::core::Place> {
        let body = self.body();
        let info = &body.names[n.index()];
        if !matches!(self.cx.repr(&info.ty, 0), Ok(Repr::Agg(_))) {
            return None;
        }
        // A take a rebuild hands back is the place it was taken from: the arm rebuilds the
        // field in place (`s.keys.push(k)`). So is a take the next row moves on
        // ([`core_moves_on`]).
        if let Some(p) = core_taken(self.lets(), n) {
            let moves_on = || core_moves_on(body, self.lets(), &self.core_w.occurs, n);
            return (self.core_hands_back(&body.stmts, n) || moves_on()).then_some(p);
        }
        let minted = info.source.starts_with('@') && !info.heap && !self.owns_heap(&info.ty);
        let owned = info.releases && !info.borrow;
        if !(info.borrow || minted || owned) {
            return None;
        }
        let (lets, written) = (self.lets(), self.written());
        let read = |m: Name| {
            let mut at = lets.iter().filter(|(b, _)| *b == m);
            match (at.next(), at.next()) {
                (Some((_, Rhs::Read(p))), None) => Some(p),
                _ => None,
            }
        };
        let place = read(n)?;
        let extent = core_extent(&body.stmts, n, &self.core_w.occurs)?;
        // The name is read through itself and through what still points into it. A scalar
        // copied out that owns no heap holds nothing of it, so the judged rows end at the
        // last row that reads a holder: `xs[0] + eat(b)` reads `xs[0]` before the call.
        let mut holders = vec![n];
        let mut inner = Vec::new();
        extent.iter().for_each(|s| core_lets(s, &mut inner));
        for (m, rhs) in &inner {
            let Rhs::Read(p) = rhs else { continue };
            let ty = &body.names[m.index()].ty;
            if vyrn_lower::kernel::root_of(p).is_some_and(|(r, _)| holders.contains(&r))
                && (self.owns_heap(ty)
                    || !matches!(self.cx.repr(ty, 0), Ok(Repr::Scalar(_) | Repr::Unit)))
            {
                holders.push(*m);
            }
        }
        let last = extent.iter().rposition(|s| {
            let mut ns = Vec::new();
            vyrn_frontend::core::names_in(s, &mut ns);
            ns.iter().any(|x| holders.contains(x))
        })?;
        let extent = &extent[..=last];
        let mut rebuilt = Vec::new();
        extent
            .iter()
            .for_each(|s| core_written(&body.names, s, &mut rebuilt));
        if written
            .iter()
            .any(|(m, c)| *m == n && !(owned && *c == Some(Capability::Consume)))
            || matches!(place, vyrn_frontend::core::Place::Key(..))
            || (owned && !matches!(place, vyrn_frontend::core::Place::Elem(..)))
            || self
                .core_place_ty(place)
                .is_none_or(|t| !self.core_as_is(&t, &info.ty))
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
            if rebuilt.iter().any(|(m, c)| {
                *m == root && matches!(c, Some(Capability::Consume | Capability::Modify))
            }) || vyrn_lower::kernel::modifies(
                extent,
                vyrn_lower::kernel::Root::N(root),
                &body.names,
            ) {
                return None;
            }
            match read(root) {
                Some(p) => on = p,
                None => return Some(place),
            }
        }
        None
    }

    /// The place the layout name `n` holds a copy of: a layout that owns no heap, a take,
    /// or a move that is no rename. A store may fill that place while the name lives
    /// ([`Fn_::core_alias`]), so the name takes a slot and the bytes.
    fn core_copies(&self, n: Name) -> Option<vyrn_frontend::core::Place> {
        let body = self.body();
        let info = &body.names[n.index()];
        if !matches!(self.cx.repr(&info.ty, 0), Ok(Repr::Agg(_))) {
            return None;
        }
        let value = !info.heap && !self.owns_heap(&info.ty);
        let mut at = self.lets().iter().filter(|(b, _)| *b == n);
        let p = match (at.next(), at.next()) {
            (Some((_, Rhs::Take(p))), None) => p.clone(),
            (Some((_, Rhs::Read(p))), None) if value => p.clone(),
            // A move [`Fn_::core_renames`] refuses takes the bytes, as a take
            // does: the kernel refuses a read of `x` before its next store.
            (Some((_, Rhs::Val(Val::Name(x)))), None)
                if value || (info.heap && !info.borrow && !body.names[x.index()].borrow) =>
            {
                vyrn_frontend::core::Place::Name(*x)
            }
            _ => return None,
        };
        (!matches!(p, vyrn_frontend::core::Place::Key(..))
            && self
                .core_place_ty(&p)
                .is_some_and(|t| self.core_as_is(&t, &info.ty)))
        .then_some(p)
    }

    /// The name whose place the layout name `n` takes over: a move is a rename.
    ///
    /// `let y = x` of an owned layout that owns heap moves `x`, and the kernel refuses a
    /// read of `x` after it. So `y` is `x`'s place, with no slot or copy, and the driver's
    /// release for the value is `y`'s. `None` where the body stores into `x` after the
    /// move ([`core_after`]), because that store writes the storage `y` holds.
    fn core_renames(&self, n: Name) -> Option<Name> {
        let body = self.body();
        let info = &body.names[n.index()];
        if !matches!(self.cx.repr(&info.ty, 0), Ok(Repr::Agg(_))) {
            return None;
        }
        let lets = self.lets();
        let mut written = Vec::new();
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
        let from = &body.names[x.index()];
        let unwritten = |m: Name| !written.iter().any(|(w, _)| *w == m);
        // A join's stores are its branches', which run before the rename, so a borrow
        // takes over a join's place and neither name releases it. A borrowed layout
        // parameter holds the caller's address for the whole body, so a second name for it
        // is that address while neither is written: a scrutinee's temporary (a declared
        // release's `match consume self`) or a reader's `let data = d`. A bound `fn`
        // parameter that [`vyrn_lower::core::specialize`] makes is still that parameter.
        let joins = self.core_joins(*x);
        let made = |r: &Rhs| matches!(r, Rhs::Make(Ctor::Closure(_), _));
        let param = from.borrow
            && (body.params.contains(x) || lets.iter().any(|(b, r)| b == x && made(r)))
            && (info.source.starts_with('@') || info.borrow)
            && unwritten(n);
        ((joins || param || (!info.borrow && self.owns_heap(&info.ty) && !from.borrow))
            && self.core_as_is(&from.ty, &info.ty)
            && (joins || unwritten(*x)))
        .then_some(*x)
    }

    /// Whether `n` is the temporary a layout `if` or `match` expression joins through: a
    /// minted name that no `let` binds and each branch stores whole. The first store takes
    /// its slot ([`Fn_::core_slot`]); the renaming `let` holds it to the end of its own
    /// extent ([`Fn_::core_renames`]).
    fn core_joins(&self, n: Name) -> bool {
        let body = self.body();
        let info = &body.names[n.index()];
        if !info.source.starts_with('@')
            || body.params.contains(&n)
            || !matches!(self.cx.repr(&info.ty, 0), Ok(Repr::Agg(_)))
        {
            return false;
        }
        let (lets, binders, written) = (self.lets(), self.binders(), self.written());
        let mut whole = 0;
        each_list(&body.stmts, &mut |ss| {
            whole += ss
                .iter()
                .filter(|r| {
                    matches!(r, St::Store { place: vyrn_frontend::core::Place::Name(m), .. } if *m == n)
                })
                .count();
        });
        whole > 0
            && !lets.iter().any(|(b, _)| *b == n)
            && !binders.contains(&n)
            && written.iter().filter(|(m, _)| *m == n).count() == whole
    }

    /// Whether [`Fn_::core_switch`] gives a payload binder of `ty` a place. A scalar loads
    /// into one local. A layout the row reads out of the scrutinee holds its address, and
    /// the kernel refuses a write to the scrutinee while the binder lives. Any other
    /// layout moves out.
    fn core_payload(&self, ty: &Type) -> bool {
        self.core_framed(ty) || matches!(self.cx.repr(ty, 0), Ok(Repr::Agg(_)))
    }

    /// Builds a made layout into the binding's own slot. The two drops at the end stand
    /// for the copy an in-place build does not make.
    #[allow(clippy::too_many_arguments)]
    fn core_make(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        dest: Dest,
        ty: &Type,
        rhs: &Rhs,
        taken: Option<u32>,
        line: usize,
    ) -> Result<(), String> {
        if let Rhs::Make(Ctor::Closure(t), vs) = rhs {
            let Some((sig_ty, target)) = self.core_closure(ty, t, vs) else {
                return unsupported("a function value this walk does not make", line);
            };
            return self.fnval_into(m, b, dest, &sig_ty, target, vs, line);
        }
        // A lambda: lifted as the arm lifts it; the row lists its captures in the lifted
        // signature's order.
        if let Rhs::Prim(Op::Closure(key), vs, _) = rhs {
            let sig_ty = crate::normalize_fn_sig(&self.cx.sub(ty), &self.cx.types);
            let Some(at) = self.literal(key) else {
                return unsupported("a lambda this walk does not find", line);
            };
            let target = self.lift_stored(m, at, &sig_ty)?;
            return self.fnval_into(m, b, dest, &sig_ty, target, vs, line);
        }
        dest.addr(b, 0);
        match rhs {
            // A variant: its tag, then its payload in the sum's slots. The builder takes this
            // walk's destination as its hint, because the slot holds the type being built.
            Rhs::Call { args, .. } => {
                let Some((tag, payload)) = self
                    .core_ctor_name(rhs)
                    .and_then(|v| self.core_variant(ty, v))
                else {
                    return unsupported("a variant the row states of no sum", line);
                };
                let Some(vs) = arg_vals(args) else {
                    return unsupported("a variant built from a place", line);
                };
                let vs: Vec<Val> = vs.into_iter().map(|(v, _)| v).collect();
                let parts = &vs;
                let hint = Some((dest, ty.clone()));
                self.build_variant(m, b, ty, tag, parts, &payload, line, hint)?;
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
                let parts = vs;
                self.record_into(m, b, dest, &decl, &l, &order, parts, line)?;
            }
            Rhs::Make(Ctor::Array, vs) => match self.cx.resolve(ty) {
                Type::Array(inner) => {
                    let parts = vs;
                    self.array_lit_heap(m, b, dest, &inner, parts, taken, line, true)?;
                }
                Type::ArrayN(inner, n) if n == vs.len() => {
                    let parts = vs;
                    self.fixed_elems(m, b, dest, &inner, parts, line)?;
                    dest.addr(b, 0);
                }
                Type::SmallArray(inner, n) if vs.len() <= n => {
                    let parts = vs;
                    self.sa_into(m, b, dest, ty, &inner, n, parts, line)?;
                    dest.addr(b, 0);
                }
                _ => return unsupported("an array literal the row does not place", line),
            },
            Rhs::Make(Ctor::Map, vs) => {
                let parts = vs;
                self.map_into(m, b, dest, ty, parts, line)?;
                dest.addr(b, 0);
            }
            Rhs::Make(Ctor::Try(name), vs) => {
                let [v] = vs.as_slice() else {
                    return unsupported(&format!("`{name}?` at this arity"), line);
                };
                let operand = |s: &mut Self, m: &mut Module, b: &mut Frame, base: &Type| {
                    s.core_val(m, b, v, base, line)
                };
                self.try_construct(m, b, name, line, operand, |_, _| dest)?;
            }
            _ => return unsupported("a made layout this walk does not build", line),
        }
        // This walk writes the in-place build's bytes itself.
        self.dest_used = false;
        b.ins(&Instruction::Drop);
        b.ins(&Instruction::Drop);
        Ok(())
    }

    /// Whether this walk stores a part of a made layout through [`Fn_::part`]: a value in
    /// one wasm local. [`Fn_::agg_part`] stores a layout part.
    fn core_part_ty(&self, t: &Type) -> bool {
        self.core_framed(t)
    }

    /// Whether a name of `t` gets a place of its own: a value in one wasm local, as a
    /// `String`, a stream cursor and a vector are. [`Fn_::place_for`] allocates it; a
    /// layout takes its slot in the make arm. A `where` type has its base's place.
    fn core_framed(&self, t: &Type) -> bool {
        matches!(self.cx.repr(t, 0), Ok(Repr::Scalar(_)))
    }

    /// Whether `t` is Unit: a name of it needs no place, a read of it writes nothing, and
    /// its `let` or store emits only the right-hand side's effects.
    fn core_unit(&self, t: &Type) -> bool {
        self.cx.repr(t, 0) == Ok(Repr::Unit)
    }

    /// Whether `v` names a layout, which a read position takes as its address
    /// ([`Fn_::core_val`]); [`Fn_::pack_key`] packs a map key from there. A layout of a
    /// validated type was checked where it was made.
    fn core_layout_name(&self, v: &Val) -> bool {
        let body = self.body();
        matches!(v, Val::Name(n) if {
            let t = &body.names[n.index()].ty;
            matches!(self.cx.repr(t, 0), Ok(Repr::Agg(_)))
        })
    }

    /// Whether this walk can push `v`: a name it frames or a literal it writes
    /// ([`Fn_::core_val`]). [`Fn_::core_operand`] answers what an arithmetic row computes
    /// with.
    fn core_val_readable(&self, v: &Val) -> bool {
        let body = self.body();
        match v {
            Val::Name(n) => {
                let ty = &body.names[n.index()].ty;
                self.core_framed(ty) || self.core_unit(ty)
            }
            Val::Lit(l) => !matches!(l, Lit::Opaque(_)),
        }
    }

    /// Whether a row calls a variant constructor, which is a made layout, not a `call`.
    fn core_ctor(&self, rhs: &Rhs) -> bool {
        self.core_ctor_name(rhs).is_some()
    }

    /// The variant a constructor row builds: a [`Callee::Ctor`] row's callee, or for the
    /// `value(x)` box the `Value` variant its operand's type picks ([`value_scalar`]).
    fn core_ctor_name<'r>(&self, rhs: &'r Rhs) -> Option<&'r str> {
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
                [(Arg::Val(v), _)] => value_scalar(&self.cx.resolve(&self.core_ty(v, &Type::Int))),
                _ => None,
            },
            _ => None,
        }
    }

    /// Every `let` row of the body, in source order. Built once per body: the per-name
    /// screens in [`Fn_::core_walkable`] read it, and a rebuild per name made the screen
    /// quadratic in the body.
    fn lets(&self) -> &[(Name, &'a Rhs)] {
        &self.facts().lets
    }

    /// Every name a row writes, hands to `consume` or `modify`, with the capability.
    fn written(&self) -> &[(Name, Option<Capability>)] {
        &self.facts().written
    }

    /// The scrutinees and arm binders of every `switch` row ([`core_switched`]).
    fn binders(&self) -> &[Name] {
        &self.facts().binders
    }

    fn facts(&self) -> &BodyFacts<'a> {
        self.facts.get_or_init(|| {
            let body = self.body();
            let mut f = BodyFacts::default();
            for s in &body.stmts {
                core_lets(s, &mut f.lets);
                core_written(&body.names, s, &mut f.written);
                core_switched(s, true, &mut f.binders);
            }
            f
        })
    }

    /// Each fixed literal a `@list` row takes, with the name the row binds: `let l = [..]`
    /// at `[T; n]`, then `let a = @list(l)` at `Array<T>`. The literal is made at the
    /// growable type and `a` takes its place, so no fixed copy exists.
    fn core_lists(&self) -> &[(Name, Name)] {
        self.lists.get_or_init(|| self.core_lists_of_body())
    }

    fn core_lists_of_body(&self) -> Vec<(Name, Name)> {
        let body = self.body();
        let lets = self.lets();
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
                        (self.cx.resolve(&body.names[l.index()].ty), self.cx.resolve(&body.names[a.index()].ty)),
                        (Type::ArrayN(e, _), Type::Array(g)) if e == g
                    )
            })
            .collect()
    }

    /// The type a made name is built at: the growable array a `@list` row
    /// takes it into ([`Fn_::core_lists`]), and its own type otherwise.
    fn core_made_ty(&self, n: Name) -> Type {
        let body = self.body();
        match self.core_lists().iter().find(|(l, _)| *l == n) {
            Some((_, a)) => body.names[a.index()].ty.clone(),
            None => body.names[n.index()].ty.clone(),
        }
    }

    /// The tag and payload types of the variant `name` of the sum `ty`, or `None` where
    /// there is none. The row's binding type picks the variant.
    fn core_variant(&self, ty: &Type, name: &str) -> Option<(u64, Vec<Type>)> {
        let Type::Enum(vs) = self.cx.resolve(ty) else {
            return None;
        };
        let i = vs.iter().position(|v| v.name == name)?;
        Some((i as u64, vs[i].payload.clone()))
    }

    /// Whether this walk builds the layout a row makes: a [`Rhs::Make`] literal, a lambda,
    /// or a [`Callee::Ctor`] call, which is the same build with a tag in front.
    fn core_makes(&self, ty: &Type, rhs: &Rhs) -> bool {
        match rhs {
            Rhs::Make(c, vs) => self.core_made(ty, c, vs),
            Rhs::Prim(Op::Closure(key), vs, _) => self.core_lambda(key, ty, vs),
            Rhs::Call { args, .. } => {
                let Some(callee) = self.core_ctor_name(rhs) else {
                    return false;
                };
                matches!(self.cx.repr(ty, 0), Ok(Repr::Agg(_)))
                    && self.core_variant(ty, callee).is_some_and(|(_, p)| {
                        p.len() == args.len()
                            && args.iter().zip(&p).all(|((a, _), t)| {
                                let Arg::Val(v) = a else {
                                    return false;
                                };
                                (self.core_val_readable(v) && self.core_part_ty(t))
                                    || self.core_payload_layout(v, t)
                            })
                    })
            }
            _ => false,
        }
    }

    /// Whether `v` is a layout name that fills a payload or part of `t` from its address.
    fn core_payload_layout(&self, v: &Val, t: &Type) -> bool {
        let body = self.body();
        matches!(v, Val::Name(n) if {
            let nt = &body.names[n.index()].ty;
            matches!(self.cx.repr(nt, 0), Ok(Repr::Agg(_)))
                && self.core_unchecked(nt, t)
                && self.cx.shape(nt) == self.cx.shape(t)
        })
    }

    /// Whether this walk builds the layout a [`Rhs::Make`] row states: an offset for every
    /// part, parts this walk emits, and no construction check the row does not carry.
    /// [`Fn_::core_built`] decides where a layout part's bytes come from.
    fn core_made(&self, ty: &Type, ctor: &Ctor, vs: &[Val]) -> bool {
        let body = self.body();
        let agg = |t: &Type| matches!(self.cx.repr(t, 0), Ok(Repr::Agg(_)));
        // A record of a validated type is its own: the constructor row after it checks its
        // cross-field `where`, or the checker proved it.
        let own =
            matches!(ctor, Ctor::Record(name, _) if *self.cx.sub(ty) == Type::Named(name.clone()));
        if !(agg(ty) && (own || !self.checks(ty))) {
            return false;
        }
        if let Ctor::Closure(t) = ctor {
            return self.core_closure(ty, t, vs).is_some();
        }
        self.core_part_tys(ty, ctor, vs.len()).is_some_and(|tys| {
            vs.iter().zip(&tys).all(|(v, t)| {
                (self.core_val_readable(v) && self.core_part_ty(t))
                    || (agg(t)
                        && matches!(v, Val::Name(n)
                            if agg(&body.names[n.index()].ty)
                                && self.core_unchecked(&body.names[n.index()].ty, t)))
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

    /// Whether this walk builds the layout the row at `ss[i]` makes at `ty`. A layout part
    /// is written at its offset by its own row ([`Fn_::core_part_at`]), or copied from a
    /// name of its layout that a reader bound or that holds a place's address. Any other
    /// temporary's slot would stay live beside the parent's storage, so this walk refuses it.
    fn core_built(&self, ss: &[St], i: usize, ty: &Type, rhs: &Rhs) -> bool {
        let body = self.body();
        if !matches!(ss[i], St::Let(..)) || !self.core_makes(ty, rhs) {
            return false;
        }
        // A function value's captures are read into its capture box ([`Fn_::core_closure`]).
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
                || (self.core_payload_layout(v, t)
                    && (matches!(ctor, Ctor::Map)
                        || matches!(v, Val::Name(n)
                            if body.names[n.index()].binding.is_some()
                                || self.core_alias(*n).is_some())))
                || self.core_part_of(ss, i, v)
        })
    }

    /// Whether the part `v` of the layout made at `ss[i]` is written at its
    /// offset by its own row ([`Fn_::core_part_at`]).
    fn core_part_of(&self, ss: &[St], i: usize, v: &Val) -> bool {
        let Val::Name(n) = v else {
            return false;
        };
        let lists = self.core_lists();
        let n = &lists.iter().find(|(_, a)| a == n).map_or(*n, |(l, _)| *l);
        ss[..i]
            .iter()
            .rposition(|s| matches!(s, St::Let(l, _) if l == n))
            .and_then(|k| self.core_part_at(ss, k))
            .is_some_and(|at| at.row == i)
    }

    /// Where the row at `ss[i]` makes a part of a record or array literal, or of a variant's
    /// boxed payload. A variant or a literal is built at the part's type.
    ///
    /// The parent's storage is taken before the part's row ([`Fn_::core_part_dest`]), and
    /// every row stays in place, so no effect moves. No row between leaves the list, so an
    /// early part never sits in storage no row owns. The temporary occurs twice, in its
    /// `let` and in the parent, so no release row reads it.
    fn core_part_at(&self, ss: &[St], i: usize) -> Option<PartAt> {
        let body = self.body();
        let St::Let(t, rhs) = &ss[i] else {
            return None;
        };
        let made =
            matches!(rhs, Rhs::Make(..) | Rhs::Prim(Op::Closure(_), ..)) || self.core_ctor(rhs);
        let taken = self.core_take_part(rhs);
        if body.names[t.index()].binding.is_some()
            || self.core_w.occurs.get(t.index()) != Some(&2)
            || !(made || taken || self.core_agg_call(rhs))
        {
            return None;
        }
        // A literal is made at the part's type, and a call's result or a
        // taken field must already have its layout.
        let fits = |part: &Type| {
            if made {
                !self.checks(part) && self.core_makes(part, rhs)
            } else {
                self.core_payload_layout(&Val::Name(*t), part)
            }
        };
        // A literal a `@list` row takes is a part under the name that row
        // binds ([`Fn_::core_lists`]).
        let named = self
            .core_lists()
            .iter()
            .find(|(l, _)| l == t)
            .map_or(*t, |(_, a)| *a);
        let j = (i + 1..ss.len()).find(|&j| match &ss[j] {
            St::Let(_, Rhs::Make(_, ps)) => ps.contains(&Val::Name(named)),
            St::Let(_, r @ Rhs::Call { args, .. }) if self.core_ctor(r) => {
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
        let made = self.core_made_ty(*parent);
        let ty = if self.core_lands(ss, j, &self.core_w.reads) {
            &self.ret_ty
        } else {
            &made
        };
        let (ctor, ps) = match prhs {
            Rhs::Make(c, ps) => (c, ps),
            Rhs::Call { args, .. } => {
                let (_, payload) = self.core_variant(ty, self.core_ctor_name(prhs)?)?;
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
    /// the hole ([`vyrn_frontend::core::Body::drop_holes`]).
    fn core_take_part(&self, rhs: &Rhs) -> bool {
        matches!(rhs, Rhs::Take(p @ vyrn_frontend::core::Place::Field(..))
        if self.core_place_ty( p).is_some_and(|t| {
            matches!(self.cx.repr(&t, 0), Ok(Repr::Agg(_)))
        }))
    }

    /// Where the row at `ss[i]` writes a part of its parent ([`Fn_::core_part_at`]): a box
    /// for a variant's payload, a heap array's buffer, or the parent's storage (the
    /// caller's where the parent lands there, its offset in its own parent, or a slot).
    /// The first such part takes the parent's storage or buffer.
    fn core_part_dest(
        &mut self,
        b: &mut Frame,
        ss: &[St],
        i: usize,
        line: usize,
    ) -> Result<Option<Dest>, String> {
        let body = self.body();
        let Some(PartAt {
            row: j,
            parent: p,
            off,
            into,
            ..
        }) = self.core_part_at(ss, i)
        else {
            return Ok(None);
        };
        let St::Let(t, _) = &ss[i] else {
            return Ok(None);
        };
        if let Some(d) = self.core_w.built[t.index()] {
            return Ok(Some(d));
        }
        let d = match into {
            PartIn::Box(bytes) => Dest::Addr(self.heap_buf(b, bytes), 0),
            PartIn::Buffer(bytes) => {
                let buf = match self.core_w.bufs[p.index()] {
                    Some(buf) => buf,
                    None => self.heap_buf(b, bytes),
                };
                self.core_w.bufs[p.index()] = Some(buf);
                Dest::Addr(buf, off)
            }
            PartIn::Parent => {
                let base = match self.core_w.built[p.index()] {
                    Some(d) => d,
                    None if self.core_lands(ss, j, &self.core_w.reads) => {
                        Dest::Addr(self.core_out(line)?, 0)
                    }
                    None => match self.core_part_dest(b, ss, j, line)? {
                        Some(d) => d,
                        None => {
                            let r = self.cx.repr(&body.names[p.index()].ty, line)?;
                            Dest::Slot(self.core_slot(b, p, &r, line)?)
                        }
                    },
                };
                self.core_w.built[p.index()] = Some(base);
                base.at(off)
            }
        };
        self.core_w.built[t.index()] = Some(d);
        Ok(Some(d))
    }

    /// The storage of `x` where row `i` is `@t = f(.., x, ..)` and `f` leaves its result in the
    /// parameter `x` goes to ([`Sig::in_place`]), and the name whose frame slot the result takes
    /// over. The call runs in `x`'s own storage and moves nothing in where either:
    /// - the next row is `x = @t`. The store moves nothing, and `x` keeps its slot.
    /// - `x`'s extent ends at row `i` (`ended`) and `x` holds a frame slot of its own
    ///   ([`Walked::slot`]). No row reads `x` after the call, so `@t` takes the slot.
    ///
    /// Every other argument is a scalar or a layout in frame bytes apart from `x`'s, so none
    /// reads that storage while the callee writes it: `x = step(x, x.p)` reads a copy of `x.p`.
    fn core_back(
        &self,
        ss: &[St],
        i: usize,
        ended: &[Name],
    ) -> Option<(Dest, Place, Option<Name>)> {
        let body = self.body();
        let St::Let(
            t,
            Rhs::Call {
                callee,
                kind,
                solved,
                targets,
                args,
                ..
            },
        ) = &ss[i]
        else {
            return None;
        };
        if !targets.is_empty() {
            return None;
        }
        let k = self.core_sig(callee, *kind, solved, targets)?.in_place?;
        let (Arg::Val(Val::Name(x)), Capability::Consume) = args.get(k)? else {
            return None;
        };
        let stored = matches!(ss.get(i + 1), Some(St::Store {
                place: vyrn_frontend::core::Place::Name(s),
                value: Val::Name(v),
                releases: false,
                ..
            }) if s == x && v == t
                && body.names[t.index()].source.starts_with('@')
                && self.core_w.reads[t.index()] == 1);
        if !stored && !(ended.contains(x) && self.core_w.slot[x.index()].is_some()) {
            return None;
        }
        let (place, ty) = self.core_place(*x)?;
        // The frame bytes a slot name holds, which no other held slot overlaps.
        let span = |p: Place, t: &Type| match p {
            Place::Slot(off) => (self.cx.layout(t, 0).ok()).map(|l| off..off + l.size),
            _ => None,
        };
        let apart = |y: &Name| {
            let (py, ty_y) = self.core_place(*y)?;
            let (sx, sy) = (span(place, &ty)?, span(py, &ty_y)?);
            Some(sx.end <= sy.start || sy.end <= sx.start)
        };
        let others = args.iter().enumerate().all(|(j, (arg, _))| match arg {
            _ if j == k => true,
            Arg::Val(v) if !self.core_layout_name(v) => true,
            Arg::Val(Val::Name(y)) => apart(y) == Some(true),
            _ => false,
        });
        let d = match place {
            Place::Slot(off) => Dest::Slot(off),
            Place::Local(l) => Dest::Addr(l, 0),
            Place::Static(_) => return None,
        };
        others.then_some((d, place, (!stored).then_some(*x)))
    }

    /// The local holding the caller's out-pointer.
    fn core_out(&self, line: usize) -> Result<u32, String> {
        self.dest
            .ok_or_else(|| gap("an aggregate result with no out-pointer", line))
    }

    fn core_stream_elem(&self, n: Name) -> Option<Type> {
        let body = self.body();
        match self.cx.resolve(&body.names[n.index()].ty) {
            Type::Stream(t) => Some(*t),
            _ => None,
        }
    }

    /// Whether an element read of `base` is the read a stream's pull answers:
    /// a stream is pulled and never indexed.
    fn core_pulls(&self, base: &vyrn_frontend::core::Place) -> bool {
        matches!(base, vyrn_frontend::core::Place::Name(s) if self.core_stream_elem( *s).is_some())
    }

    /// A local holding the address of the layout `n`: its own, where its
    /// place is one, or a fresh one set from [`Fn_::core_addr_of`].
    fn core_addr_local(&self, b: &mut Frame, n: Name, line: usize) -> Result<u32, String> {
        if let Some((Place::Local(l), _)) = self.core_place(n) {
            return Ok(l);
        }
        self.core_addr_of(b, n, line)?;
        let l = b.local(ValType::I32);
        b.ins(&Instruction::LocalSet(l));
        Ok(l)
    }

    /// Pushes the address of the aggregate `n` holds: a slot's, or the one a parameter's
    /// local holds.
    fn core_addr_of(&self, b: &mut Frame, n: Name, line: usize) -> Result<(), String> {
        let Some((place, _)) = self.core_place(n) else {
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

    /// Binds the core name `n` at `place`. A name with a
    /// [`NameInfo::binding`] also goes on the scope with the release it
    /// owes, keyed by that binding and not by its spelling: a projection inlined at its
    /// access site renames its bindings to `@b<tag>.<name>`.
    fn core_bind(&mut self, n: Name, place: Place, ty: Type) -> Result<(), String> {
        let body = self.body();
        let info = &body.names[n.index()];
        self.core_w.at[n.index()] = Some((place, ty.clone()));
        let Some(key) = info.binding else {
            return Ok(());
        };
        self.scope.push((info.source.clone(), place, ty.clone()));
        if self.releases_whole(key) {
            if let Some(r) = self.rel_owed(key, &ty, info.line)? {
                self.register_rel(key, place, r);
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

    /// The `where` check a validated record owes once made (`core::Builder`'s `bind`): the
    /// declaration, and the made layout `n` its constructor reads in place.
    fn core_checks_made(&self, rhs: &Rhs) -> Option<(TypeDecl, Name)> {
        let body = self.body();
        let n = rhs.checks_rule(&body.names)?;
        let Rhs::Call { callee, .. } = rhs else {
            return None;
        };
        let decl = self
            .cx
            .types
            .get(callee)
            .filter(|d| d.predicate.is_some())?;
        matches!(self.cx.repr(&decl.base, 0), Ok(Repr::Agg(_))).then(|| (decl.clone(), n))
    }

    /// The signature this walk calls a [`Callee::Fn`] through; `None` for a callee whose
    /// emission is more than a `call`. `Cx::sigs` holds only the functions this module defines,
    /// without generics, higher-order shells and `std/mem` declarations.
    fn core_sig(
        &self,
        callee: &str,
        kind: Callee,
        solved: &[(String, Type)],
        targets: &[Target],
    ) -> Option<Sig> {
        if let Some((_, t)) = self.core_through(kind) {
            return self.value_sig(&t);
        }
        if !targets.is_empty() {
            let (f, targs, subst, bound) = self.core_ho(callee, kind, solved, targets)?;
            let key = vyrn_lower::spell(&f.name, &targs);
            return (self.cx)
                .signature(&ho_shell(self.cx, f, &subst, &bound).0, &key)
                .ok();
        }
        // A routed builtin is a call to the function its row names.
        let (callee, kind) = match core_builtin(self.cx, callee, kind) {
            Some(Spec::Routes(f)) => (*f, Callee::Bound),
            _ => (callee, kind),
        };
        if !kind.direct() {
            return None;
        }
        // A `modify` parameter crosses as the address of the caller's binding
        // ([`Fn_::core_args_readable`]); an aggregate result, through [`Fn_::out_ptr`].
        match self.core_instance(callee, kind, solved) {
            Some((f, targs, subst)) => {
                let key = vyrn_lower::spell(&f.name, &targs);
                self.cx.signature(&instance_shell(f, &subst), &key).ok()
            }
            None => (self.cx.sigs.get(callee).cloned()).or_else(|| self.cx.lambda_sig(callee)),
        }
    }

    /// The generic function a call row names and the instance the checker solved for it.
    /// `None` for a callee with no type parameters.
    fn core_instance(
        &self,
        callee: &str,
        kind: Callee,
        solved: &[(String, Type)],
    ) -> Option<(&'p Function, Vec<Type>, HashMap<String, Type>)> {
        let f = (self.cx.generics.get(callee).copied()).filter(|_| kind.direct())?;
        let (targs, subst) = solved_instance(f, solved)?;
        Some((f, targs, subst))
    }

    /// The specialization a call row names: the higher-order function, its type arguments and
    /// substitution, and the target of each `fn` parameter. `None` where a target has no
    /// [`FnTarget`] or a type parameter is unsolved.
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
        let f = (self.cx.higher_order.get(callee).copied()).filter(|_| kind.direct())?;
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

    /// The function a [`Target`] calls. `None` for a pass-through or a stored value, and for a
    /// function with a `modify` parameter, which no function value may name.
    fn core_target(&self, t: &Target) -> Option<FnTarget> {
        match t {
            Target::Fn(name) => {
                let sig = (self.cx.sigs.get(name)).filter(|s| !s.modify.iter().any(|m| *m))?;
                Some(FnTarget {
                    sig: sig.clone(),
                    ncaps: 0,
                })
            }
            Target::Lambda(key, caps, _) => Some(FnTarget {
                sig: self.cx.lambda_sig(key)?,
                ncaps: caps.len(),
            }),
            Target::Param(_) | Target::Value(_) => None,
        }
    }

    /// The target a stored value of the `fn` type `p` is: its signature's dispatcher, with the
    /// value as its one capture. The index is 0 until [`Fn_::core_call`] registers the dispatcher.
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
                in_place: None,
            },
            ncaps: 1,
        })
    }

    /// The stored value a call row calls through, and its signature. `None`
    /// also for a parameter a specialization bound.
    fn core_through(&self, kind: Callee) -> Option<(Name, Type)> {
        let body = self.body();
        let n = kind.value()?;
        let info = &body.names[n.index()];
        // A call through a bound `fn` parameter goes to its binding, and a local
        // that shadows the parameter is a value like any other.
        if body.params.contains(&n) && self.fn_binds.contains_key(&info.source) {
            return None;
        }
        let sig_ty = crate::normalize_fn_sig(&self.cx.sub(&info.ty), &self.cx.types);
        matches!(sig_ty, Type::Fn(..)).then_some((n, sig_ty))
    }

    /// The signature a call through a value of `sig_ty` sees. Its index names no function; the
    /// call goes to the dispatcher ([`Fn_::core_call`]).
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
            in_place: None,
        })
    }

    /// The function type and the target of a function value a row makes from the captures
    /// `parts`. `None` where `ty` is no function type.
    fn core_closure(&self, ty: &Type, t: &Target, parts: &[Val]) -> Option<(Type, FnTarget)> {
        let body = self.body();
        let sig_ty = crate::normalize_fn_sig(&self.cx.sub(ty), &self.cx.types);
        if !matches!(sig_ty, Type::Fn(..)) {
            return None;
        }
        let target = self.core_target(t)?;
        let readable = parts.iter().all(|v| {
            matches!(v, Val::Name(c)
                if self.core_val_readable( v)
                    || self.core_payload_layout( v, &body.names[c.index()].ty))
        });
        (target.ncaps == parts.len() && readable).then_some((sig_ty, target))
    }

    /// Whether this walk makes the lambda `key` at `ty` ([`Fn_::core_make`]'s screen). The row
    /// lists the captures in the lifted signature's order
    /// ([`vyrn_lower::core::lambda_captures`]), and each is a name this walk reads.
    fn core_lambda(&self, key: &str, ty: &Type, vs: &[Val]) -> bool {
        let body = self.body();
        let sig_ty = crate::normalize_fn_sig(&self.cx.sub(ty), &self.cx.types);
        let (Type::Fn(ptys, _), Some(Expr::Lambda { params, .. })) = (&sig_ty, self.literal(key))
        else {
            return false;
        };
        matches!(self.cx.repr(&sig_ty, 0), Ok(Repr::Agg(_)))
            && params.len() == ptys.len()
            && vs.iter().all(|v| {
                matches!(v, Val::Name(c)
                    if self.core_val_readable( v)
                        || self.core_payload_layout( v, &body.names[c.index()].ty))
            })
    }

    /// An operator with its operands read off the row. [`Fn_::bin_ins`] and [`Fn_::un_ins`]
    /// choose the instruction.
    fn core_prim(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        op: &Op,
        vs: &[Val],
        line: usize,
    ) -> Result<Type, String> {
        match (op, vs) {
            (Op::Un(u), [v]) => {
                let t = self.cx.resolve(&self.core_ty(v, &Type::Int));
                self.core_val(m, b, v, &t, line)?;
                self.un_ins(b, *u, &t, line)
            }
            // A conversion is the operand at its own type, then the coercion plan's rungs.
            (Op::Conv(to), [v]) => {
                let from = self.core_ty(v, &Type::Int);
                self.core_val(m, b, v, &from, line)?;
                self.coerce(m, b, &from, to, line)?;
                Ok(to.clone())
            }
            // A String operator is a call; the builder states its operand releases as rows.
            (Op::Bin(o), [l, r]) if self.core_str_op(*o, l, r) => {
                self.core_val(m, b, l, &Type::Str, line)?;
                if let (BinOp::Match, Val::Lit(Lit::Str(pat))) = (o, r) {
                    self.str_match(m, b, pat, line)?;
                    return Ok(Type::Bool);
                }
                self.core_val(m, b, r, &Type::Str, line)?;
                self.str_bin(b, *o, line)
            }
            (Op::Bin(o), [l, r]) if self.core_code_concat(*o, l, r) => {
                let Some(g) = self.cx.gen else {
                    return unsupported("`+` outside a generator", line);
                };
                let code = Type::Named("Code".to_string());
                self.core_val(m, b, l, &code, line)?;
                self.core_val(m, b, r, &code, line)?;
                b.ins(&Instruction::Call(g.concat));
                Ok(code)
            }
            (Op::Bin(o), [l, r]) => {
                // A float literal takes its sibling's type: `0.0 - o` with `o: Float32` runs
                // at `Float32`. An integer literal widens by [`Fn_::op_width`] below.
                let lt = match (l, r) {
                    (Val::Lit(Lit::Float(_)), Val::Name(_)) => self.core_ty(r, &Type::Int),
                    _ => self.core_ty(l, &Type::Int),
                };
                let lt = self.cx.resolve(&lt);
                self.core_val(m, b, l, &lt, line)?;
                let opty = match Num::of(&lt) {
                    Some(n) if n == Num::PLAIN && matches!(r, Val::Name(_)) => {
                        let rt = self.core_ty(r, &lt);
                        self.op_width(&lt, Some(&rt))
                    }
                    _ => lt.clone(),
                };
                if opty != lt {
                    self.coerce(m, b, &lt, &opty, line)?;
                }
                self.core_val(m, b, r, &opty, line)?;
                let mut rows = [None, None];
                if !o.row().traps.is_empty() && Num::of(&opty).is_some() {
                    let Some(k) = self.core_w.checks.iter().rposition(|(c, _)| {
                        matches!(&c.guard, Guard::NonZero(d) | Guard::Shift(d, _) if d == r)
                    }) else {
                        return unsupported("a runtime check the core did not state", line);
                    };
                    // A signed quotient's row follows its divisor's.
                    let quotient = |c: &Check| matches!(&c.guard, Guard::NoOverflow(x, d, _) if x == l && d == r);
                    for (j, row) in [k, k + 1].into_iter().zip(&mut rows) {
                        if let Some((c, ran)) = self
                            .core_w
                            .checks
                            .get_mut(j)
                            .filter(|(c, _)| j == k || quotient(c))
                        {
                            *ran = true;
                            let check = c.clone();
                            *row = self.row(b, check);
                        }
                    }
                }
                self.bin_ins(b, *o, &opty, &lt, rows, line)
            }
            _ => unsupported("an operator of this arity", line),
        }
    }

    /// Emits operand `i` of the call to `name` at `want`, or at the type it carries where `want` is
    /// `None`, and returns that type. A call with no operand `i` is refused.
    #[allow(clippy::too_many_arguments)]
    fn core_arg(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[(Val, Capability)],
        i: usize,
        want: Option<&Type>,
        line: usize,
    ) -> Result<Type, String> {
        let Some((v, _)) = args.get(i) else {
            return unsupported(&format!("`{name}` with too few operands"), line);
        };
        let t = want.cloned().unwrap_or_else(|| self.core_ty(v, &Type::Int));
        self.core_val(m, b, v, &t, line)?;
        Ok(t)
    }

    /// One value: the name's own place, or the literal the row names.
    fn core_val(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        v: &Val,
        want: &Type,
        line: usize,
    ) -> Result<(), String> {
        let body = self.body();
        let got = match v {
            Val::Name(n) => {
                // The operand stack already holds it.
                if self.core_w.held == Some(*n) {
                    self.core_w.held = None;
                    let ty = body.names[n.index()].ty.clone();
                    return self.coerce(m, b, &ty, want, line);
                }
                if self.core_unit(&body.names[n.index()].ty) {
                    return Ok(());
                }
                let Some((place, ty)) = self.core_place(*n) else {
                    return unsupported("a core name with no place", line);
                };
                match place {
                    Place::Local(l) => {
                        b.ins(&Instruction::LocalGet(l));
                    }
                    // A layout pushes its address, as the `Expr::Var` arm does.
                    _ if matches!(self.cx.repr(&ty, line)?, Repr::Agg(_))
                        && place.addr(b, 0).is_some() => {}
                    _ => return unsupported("a core name that is not a local", line),
                }
                ty
            }
            // A literal is emitted at its own type and coerced: an integer literal is an
            // `Int64` that its destination narrows.
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
        self.coerce(m, b, &got, want, line)
    }

    /// The result of a `std/mem` primitive call; `None` for another callee or an arity the
    /// table does not state.
    fn core_mem_ty(&self, callee: &str, args: usize) -> Option<Type> {
        let prim = callee.strip_prefix(vyrn_frontend::loader::MEM_PREFIX)?;
        match (prim, args) {
            ("addr", 1) => return Some(INT32),
            ("adopt", 1) => return Some(Type::Param("T".into())),
            _ => {}
        }
        let decl = self.cx.mem.get(prim)?;
        mem_ins(self.cx, prim)?;
        (decl.params.len() == args).then(|| decl.ret.clone())
    }

    /// The checker's type of `v`; `lit` for an integer or byte literal.
    fn core_ty(&self, v: &Val, lit: &Type) -> Type {
        let body = self.body();
        match v {
            Val::Name(n) => body.names[n.index()].ty.clone(),
            Val::Lit(Lit::Bool(_)) => Type::Bool,
            Val::Lit(Lit::Float(_)) => Type::Float,
            Val::Lit(Lit::Str(_)) => Type::Str,
            Val::Lit(_) => lit.clone(),
        }
    }

    /// Whether the core's rows carry this whole body, so this walk may emit it. A screen, not a
    /// judgement: a refused body is emitted from the AST.
    fn core_walkable(&self, stmts: Option<&Block>) -> bool {
        let body = self.body();
        // An aggregate result crosses through the caller's out-pointer ([`Fn_::core_lands`]).
        // A result checked where it is returned is refused, because the row states no check;
        // a value of the result's own validated type was checked where it was made.
        if matches!(self.ret, Repr::Agg(_)) && self.checks(&self.ret_ty) {
            let crosses = body.stmts.iter().flat_map(St::rows).any(|(x, _)| {
                matches!(x, St::Return { value: Some(v), .. }
                    if !matches!(v, Val::Name(r) if body.names[r.index()].ty == self.ret_ty))
            });
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
        // A made layout is built into the annotation's layout, and this walk builds into the
        // value's type. A made layout whose `let` annotates another type stays in the arm.
        // The key is the node the plan keys the binding by, that `Stmt::Let`.
        let annotated = match stmts {
            Some(blk) => self.annotations(|fs| each_block(blk, &mut |_| {}, fs)),
            None => Vec::new(),
        };
        let occurs = body.occurrences();
        for (n, info) in body.names.iter().enumerate() {
            // Every name a row names needs a place. It is one of: a wasm local (a `where` type
            // is not one, it is a `check` row the core does not carry); a layout parameter,
            // whose local holds the caller's address; a layout this walk makes; a layout read
            // out of a place ([`Fn_::core_alias`], [`Fn_::core_copies`], [`Fn_::core_walked`]);
            // a layout bound by a move ([`Fn_::core_renames`]) or by `blackBox` of a name; a
            // payload binder ([`Fn_::core_payload`]); an aggregate call result
            // ([`Fn_::out_ptr`]); a layout `if` or `match` join ([`Fn_::core_joins`]).
            // [`Fn_::core_val_readable`] refuses reading a layout as a value. A Unit name
            // and a name no row names need no place.
            if occurs[n] != 0
                && !(self.core_framed(&info.ty)
                    || self.core_unit(&info.ty)
                    || (body.params.contains(&Name(n as u32))
                        && matches!(self.cx.repr(&info.ty, 0), Ok(Repr::Agg(_)))))
                && !(!self.annotated_apart(&annotated, info)
                    && lets.iter().any(|(b, rhs)| {
                        b.index() == n
                            && (self.core_makes( &self.core_made_ty( *b), rhs)
                                || self.core_agg_call( rhs)
                                || self.core_take_part( rhs)
                                || self.core_rebuild( rhs)
                                || matches!(rhs, Rhs::Call { callee, kind, args, .. }
                                    if matches!(core_builtin(self.cx, callee, *kind), Some(Spec::Barrier))
                                        && matches!(args.as_slice(), [(Arg::Val(Val::Name(_)), _)]))
                                || matches!(rhs, Rhs::Read(vyrn_frontend::core::Place::Elem(s, _))
                                    if self.core_pulls( s)))
                    }))
                && self.core_alias(Name(n as u32)).is_none()
                && self.core_copies( Name(n as u32)).is_none()
                && self.core_renames( Name(n as u32)).is_none()
                && !self.core_lists().iter().any(|(_, a)| a.index() == n)
                && !binders.contains(&Name(n as u32))
                && !self.core_walked( Name(n as u32))
                && !self.core_joins( Name(n as u32))
            {
                return false;
            }
        }
        // The core types a `let` by its value except where it states the annotation's check,
        // so an annotated check on a binding of another type is one the rows do not state.
        if stmts.is_some_and(|blk| self.annotates_a_check(blk)) {
            return false;
        }
        let reads = body.reads();
        self.core_readable(&body.stmts, &reads, &[])
    }

    /// The annotation of each annotated `let` `walk` reaches, keyed as the plan keys a binding.
    fn annotations(&self, walk: impl FnOnce(&mut dyn FnMut(&Stmt))) -> Vec<(NodeId, Type)> {
        let mut out = Vec::new();
        walk(&mut |s| {
            if let Stmt::Let { ty: Some(t), .. } = s {
                let at = s.id();
                out.push((at, t.clone()));
            }
        });
        out
    }

    /// Whether a name is bound by a `let` of `annotated` whose annotation is not the name's type
    /// as is ([`Fn_::core_as_is`]). The row then does not state the annotation's layout.
    fn annotated_apart(&self, annotated: &[(NodeId, Type)], info: &NameInfo) -> bool {
        info.binding.is_some_and(|at| {
            annotated
                .iter()
                .any(|(a, t)| *a == at && !self.core_as_is(&info.ty, t))
        })
    }

    /// Whether a `let` of `blk` is annotated with a `where` type and binds a name of another
    /// type: a check the rows do not state.
    fn annotates_a_check(&self, blk: &Block) -> bool {
        let body = self.body();
        let mut found = false;
        each_block(blk, &mut |_| {}, &mut |s| {
            if let Stmt::Let { ty: Some(t), .. } = s {
                let at = s.id();
                found |= self.checks(t)
                    && !body
                        .names
                        .iter()
                        .any(|i| i.binding == Some(at) && i.ty == *self.cx.sub(t));
            }
        });
        found
    }

    /// Whether a value of `from` lands in a place of `to` with no check: `to` has no `where`
    /// clause, or it is `from`, whose value was checked where it was made.
    fn core_unchecked(&self, from: &Type, to: &Type) -> bool {
        !self.checks(to) || self.cx.sub(from) == self.cx.sub(to)
    }

    /// Whether `t`, under this instance's type arguments, names a declaration with a `where`
    /// clause, so a value of it is checked where it is made or stored.
    fn checks(&self, t: &Type) -> bool {
        matches!(&*self.cx.sub(t), Type::Named(n)
            if self.cx.types.get(n).is_some_and(|d| d.predicate.is_some()))
    }

    /// Whether every statement of `ss` is one [`Fn_::core_stmts`] reads.
    /// `bound` holds the names with a place on the path to `ss`: the payload
    /// binders of the arms it is inside, and the names an enclosing list made
    /// before it, whose slots the walk holds to the end of their extent.
    fn core_readable(&self, ss: &[St], reads: &[u32], bound: &[Name]) -> bool {
        let body = self.body();
        // The names rows made, with their row. Built on the first compound row, so a list of
        // plain rows pays nothing; asking per row would scan `ss[..i]` each time.
        let made = std::cell::OnceCell::new();
        let path = |i: usize| -> Vec<Name> {
            let made = made.get_or_init(|| {
                (ss.iter().enumerate())
                    .filter_map(|(j, p)| match p {
                        St::Let(l, rhs)
                            if matches!(rhs, Rhs::Make(..))
                                || self.core_ctor(rhs)
                                || self.core_agg_call(rhs) =>
                        {
                            Some((j, *l))
                        }
                        _ => None,
                    })
                    .collect::<Vec<_>>()
            });
            let before = made.partition_point(|&(j, _)| j < i);
            let names = made[..before].iter().map(|&(_, l)| l);
            bound.iter().copied().chain(names).collect()
        };
        ss.iter().enumerate().all(|(i, s)| match s {
            // A made layout is built into the name's slot, held to the end of its extent, or
            // into the caller's storage when the `return` after it hands it back
            // ([`Fn_::core_lands`]).
            St::Let(n, rhs)
                if matches!(rhs, Rhs::Make(..) | Rhs::Prim(Op::Closure(_), ..))
                    || self.core_ctor( rhs) =>
            {
                let part = self.core_part_at( ss, i);
                let made = self.core_made_ty( *n);
                let ty = match &part {
                    Some(at) => &at.ty,
                    None if self.core_lands( ss, i, reads) => &self.ret_ty,
                    None => &made,
                };
                self.core_built( ss, i, ty, rhs)
            }
            // An aggregate call result has its own slot: one the `let` takes before the call,
            // the storage the call wrote, or the caller's storage.
            St::Let(_, rhs) if self.core_agg_call( rhs) => true,
            St::Let(_, Rhs::Take(_)) if self.core_part_at( ss, i).is_some() => {
                true
            }
            // An accumulator's append is read with the store after it.
            St::Let(_, rhs @ Rhs::Call { args, .. }) if self.core_rebuild( rhs) => {
                !matches!(args.first(), Some((Arg::Val(Val::Name(x)), _)) if body.names[x.index()].grows)
                    || self
                        .core_rebuilt( ss, i + 1 + drops_ahead(ss[i + 1..].iter()))
                        .is_some()
            }
            St::Let(n, Rhs::Read(p)) if self.core_walked( *n) => {
                self.core_place_ty( p).is_some()
            }
            // The head of a `for` over a stream: the element goes into a
            // place of its own, a local or a slot ([`Spec::Pulls`]).
            St::Let(
                _,
                Rhs::Call {
                    callee, kind, args, ..
                },
            ) if matches!(core_builtin(self.cx, callee, *kind), Some(Spec::Pulls)) => {
                matches!(args.as_slice(), [(Arg::Val(Val::Name(s)), _)]
                    if self.core_stream_elem( *s).is_some())
            }
            St::Let(_, Rhs::Read(vyrn_frontend::core::Place::Elem(s, _)))
                if self.core_pulls( s) =>
            {
                true
            }
            St::Let(n, _)
                if self.core_alias(*n).is_some()
                    || self.core_copies( *n).is_some()
                    || self.core_renames( *n).is_some()
                    || self.core_lists().iter().any(|(_, a)| a == n) =>
            {
                true
            }
            St::Let(_, rhs) => self.core_rhs_readable( rhs),
            St::Store { .. } if self.core_rebuilt( ss, i).is_some() => true,
            // A store into a place with an address, a name's slot or module state's static one.
            // A layout's value is a name of its type, whose bytes are copied.
            St::Store { place, value, .. } => {
                use vyrn_frontend::core::Place as At;
                let ty = match place {
                    At::Name(n) => Some(body.names[n.index()].ty.clone()),
                    At::Key(_, k)
                        if !self.core_val_readable( k) && !self.core_layout_name( k) =>
                    {
                        None
                    }
                    At::Key(m, _) => {
                        match self.core_place_ty( m).map(|t| self.cx.resolve(&t)) {
                            Some(Type::Map(_, v)) => Some(*v),
                            _ => None,
                        }
                    }
                    p => self.core_place_ty( p),
                };
                // A value of the place's own validated type crosses nothing;
                // any other one is a check the row does not state.
                ty.is_some_and(|t| {
                    let r = self.cx.resolve(&t);
                    let fits = match self.cx.repr(&t, 0) {
                        Ok(Repr::Unit | Repr::Scalar(_)) => self.core_val_readable( value),
                        Ok(Repr::Agg(_)) => {
                            matches!(value, Val::Name(v)
                                    if self.cx.resolve(&body.names[v.index()].ty) == r)
                        }
                        _ => false,
                    };
                    fits && match value {
                        Val::Name(n) => self.core_unchecked(&body.names[n.index()].ty, &t),
                        _ => !self.checks(&t),
                    }
                })
            }
            St::If {
                cond, then, els, ..
            } => {
                self.core_val_readable( cond)
                    && self.core_readable( then, reads, &path(i))
                    && self.core_readable( els, reads, &path(i))
            }
            St::Block { body: inner, .. } => self.core_readable( inner, reads, &path(i)),
            St::Loop { body: inner, .. } => self.core_readable( inner, reads, &path(i)),
            St::Break { .. } | St::Continue { .. } => true,
            // [`Fn_::core_switch`] needs a scrutinee this walk can take the address of, a tag
            // on every arm, and a place for every payload binder ([`Fn_::core_payload`]).
            St::Switch { on, arms, site, .. } => {
                let Val::Name(n) = on else {
                    return false;
                };
                // The place is one this walk bound (a made layout, or an enclosing switch's
                // payload binder) or the scope's.
                let path = path(i);
                let placed = path.contains(n)
                    || self.core_place( *n).is_some()
                    || self.core_alias(*n).is_some()
                    || self.core_copies( *n).is_some()
                    || self.core_renames( *n).is_some();
                placed
                    && self.sum_of(&body.names[n.index()].ty).is_some()
                    // A construct the plan releases WHOLE is one the arm gives
                    // a slot and a copy of its own, because an arm may build
                    // over the scratch the scrutinee was left in. The row
                    // states the release and not the copy.
                    && !self.releases_whole(*site)
                    && arms.iter().all(|a| {
                        a.test
                            .reads()
                            .is_none_or(|h| self.core_framed(&body.names[h.index()].ty))
                            && a.binds
                                .iter()
                                .all(|bn| self.core_payload(&body.names[bn.index()].ty))
                            && self.core_readable(
                                &a.body[a.reads(on).len()..],
                                reads,
                                &[&path[..], &a.binds[..]].concat(),)
                    })
            }
            // A release is the row's, at every exit, and the walk emits it
            // where it stands ([`Fn_::core_release`]). What it needs is the
            // node the plan keys the slot by, which is the name's binding.
            St::Row { name, .. } => body.names[name.index()].binding.is_some(),
            // A `?` on an `Option` returns `None` with no value on the row,
            // which is the return type's tag and no value this walk writes.
            St::Return { value, .. } => match value {
                None => matches!(self.ret, Repr::Unit),
                Some(Val::Name(n)) if matches!(self.ret, Repr::Agg(_)) => {
                    self.core_as_is(&body.names[n.index()].ty, &self.ret_ty)
                }
                Some(v) => self.core_val_readable( v),
            },
            // A discarded read drops its place's value, or a layout's address.
            St::Do {
                rhs: Rhs::Read(p), ..
            } => self.core_place_ty( core_discarded(p)).is_some(),
            // Any other discarded value drops at the type its call row states
            // ([`Fn_::core_rhs_ty`]). A discarded layout is a slot of the row's own, given
            // back at the row's end.
            St::Do { rhs, line, .. } => {
                self.core_checks_made( rhs).is_some()
                    || (self.core_rhs_readable( rhs) || self.core_agg_call( rhs))
                        && self.core_rhs_ty( rhs, *line).is_ok()
            }
            // A release ([`Fn_::core_drop`]) needs the name's place, which
            // [`Fn_::core_walkable`]'s name clause has already asked about.
            St::Drop(n, ..) => {
                let ty = &body.names[n.index()].ty;
                self.core_framed(ty) || matches!(self.cx.repr(ty, 0), Ok(Repr::Agg(_)))
            }
            St::Trap | St::Check(_) => true,
        })
    }

    /// Whether this walk writes every argument of a call row.
    ///
    /// A heap value crosses as its pointer; the call arm decides who owns it after. A layout
    /// crosses as the address of its slot, whatever the capability. [`Fn_::spill`] gives a
    /// scalar `modify` argument an address for the call. A name holding another place's
    /// address ([`Fn_::core_alias`]) is not written through. A place argument is a removal's
    /// receiver ([`Fn_::core_removes`]) or a layout a declared function modifies.
    fn core_args_readable(&self, args: &[(Arg, vyrn_frontend::ast::Capability)]) -> bool {
        let body = self.body();
        use vyrn_frontend::ast::Capability as Cap;
        args.iter().all(|(a, c)| {
            let v = match a {
                Arg::Val(v) => v,
                // A place a `modify` parameter writes crosses as its address,
                // as a layout name does; a scalar one has no local to reload.
                Arg::Place(p) => {
                    return *c == Cap::Modify
                        && self
                            .core_place_ty(p)
                            .is_some_and(|t| matches!(self.cx.repr(&t, 0), Ok(Repr::Agg(_))));
                }
            };
            let layout = self.core_layout_name(v);
            match c {
                Cap::Read | Cap::Consume => self.core_val_readable(v) || layout,
                Cap::Modify => match v {
                    Val::Name(n) if layout => self.core_alias(*n).is_none(),
                    // A temporary may ride the operand stack, which has no
                    // local to reload into.
                    Val::Name(n) => {
                        self.core_val_readable(v)
                            && (body.names[n.index()].binding.is_some() || body.params.contains(n))
                    }
                    Val::Lit(_) => false,
                },
            }
        })
    }

    /// Whether a row is a call whose aggregate result crosses through an out-pointer
    /// ([`Fn_::out_ptr`]).
    ///
    /// A map key read is one: the lookup builds the `Option` in its own slot ([`Fn_::map_at`])
    /// and the binding copies it. The row keeps the place because the kernel's alias of `m[..]`
    /// refuses a write to the map while the read is live. A boxed value stays in the arm: no
    /// row releases the box the lookup copies it into.
    fn core_agg_call(&self, rhs: &Rhs) -> bool {
        let body = self.body();
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
                self.core_removes(callee, *kind, args) == Some(true)
                    || self.core_args_readable(args)
                        && (arg_vals(args).is_some() || self.core_user_callee(callee, *kind))
                        && (match core_builtin(self.cx, callee, *kind) {
                            // The result type is the row's, which a stream
                            // reader's operands do not carry.
                            Some(Spec::Builds(_)) => ret.is_some(),
                            // `x.copy()` of a layout: [`Fn_::copy_stack`] builds
                            // the copy in a slot of its own, as `Builds` does.
                            Some(Spec::OwnType) => {
                                matches!(args.as_slice(), [(Arg::Val(Val::Name(n)), _)]
                                if matches!(self.cx.repr(&body.names[n.index()].ty, 0), Ok(Repr::Agg(_))))
                            }
                            // `std/mem`'s `adopt` hands its operand's slot on.
                            _ => {
                                callee.strip_prefix(vyrn_frontend::loader::MEM_PREFIX)
                                    == Some("adopt")
                                    && ret.as_ref().is_some_and(|t| {
                                        matches!(self.cx.repr(t, 0), Ok(Repr::Agg(_)))
                                    })
                            }
                        } || self
                            .core_sig(callee, *kind, solved, targets)
                            .is_some_and(|s| s.params.len() == args.len() && s.ret.agg().is_some()))
            }
            Rhs::Read(vyrn_frontend::core::Place::Key(base, k)) => {
                (self.core_val_readable(k) || self.core_layout_name(k))
                    && matches!(
                        self.core_place_ty(base).map(|t| self.cx.resolve(&t)),
                        Some(Type::Map(..))
                    )
            }
            _ => false,
        }
    }

    /// Whether a row rebuilds a named `Array` or `Map` receiver, a
    /// `SmallArray` one it pushes to, or a String accumulator in place
    /// ([`Spec::Rebuilds`]), with operands this walk writes.
    fn core_rebuild(&self, rhs: &Rhs) -> bool {
        let body = self.body();
        let Rhs::Call {
            callee, kind, args, ..
        } = rhs
        else {
            return false;
        };
        matches!(core_builtin(self.cx, callee, *kind), Some(Spec::Rebuilds))
            && matches!(args.split_first(), Some(((Arg::Val(Val::Name(x)), _), rest))
                if (body.names[x.index()].grows
                    || matches!(
                        (callee.as_str(), self.cx.resolve(&body.names[x.index()].ty)),
                        (_, Type::Array(_))
                            | ("@push", Type::SmallArray(..))
                            | ("@tally" | "@tallyBytes", Type::Map(..))
                    ))
                    && core_global(body, *x).is_none_or(|g| self.cx.gappend.contains_key(g))
                    && self.core_args_readable( rest))
    }

    /// Whether `callee` is a declared function, whose arguments
    /// [`Fn_::core_user_call`] writes, place arguments included, and not a
    /// builtin, a validated type or a host import, whose readers take values.
    fn core_user_callee(&self, callee: &str, kind: Callee) -> bool {
        matches!(
            core_builtin(self.cx, callee, kind),
            None | Some(Spec::Routes(_))
        ) && self.core_named(callee, kind).is_none()
            && !(kind.direct() && self.is_extern(callee))
            && !callee.starts_with(vyrn_frontend::loader::MEM_PREFIX)
    }

    /// Whether a row removes from a receiver, a name or a place
    /// ([`Spec::Removes`]), with operands this walk writes, and if so whether
    /// what it hands back is an aggregate, which lands through a slot.
    fn core_removes(
        &self,
        callee: &str,
        kind: Callee,
        args: &[(Arg, vyrn_frontend::ast::Capability)],
    ) -> Option<bool> {
        let body = self.body();
        if !matches!(core_builtin(self.cx, callee, kind), Some(Spec::Removes)) {
            return None;
        }
        let [recv, rest @ ..] = args else {
            return None;
        };
        let (ty, readable) = match &recv.0 {
            Arg::Val(Val::Name(x)) => (
                body.names[x.index()].ty.clone(),
                self.core_args_readable(std::slice::from_ref(recv)),
            ),
            Arg::Place(p) => (self.core_place_ty(p)?, true),
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
        (readable && self.core_args_readable(rest) && rest.len() == usize::from(callee != "@pop"))
            .then_some(agg)
    }

    /// The receiver and result of the rebuild before `ss[i]`, when `ss[i]` stores the result back
    /// into the receiver or the place it was taken from, which [`Fn_::arr_rebuild`] has already
    /// written. Only releases of the call's argument temporaries stand between the two.
    fn core_rebuilt(&self, ss: &[St], i: usize) -> Option<(Name, Name)> {
        let body = self.body();
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
            vyrn_frontend::core::Place::Name(x) => x == r,
            vyrn_frontend::core::Place::Global(g) if core_global(body, *r) == Some(g.as_str()) => {
                true
            }
            p => core_taken(self.lets(), *r) == Some(p),
        };
        (t == v && back && self.core_rebuild(rhs)).then_some((*r, *t))
    }

    /// Whether some store writes `x` whole other than the one that puts a
    /// rebuilt `x` back ([`Fn_::core_rebuilt`]).
    fn core_restored(&self, x: Name) -> bool {
        let body = self.body();
        let mut found = false;
        each_list(&body.stmts, &mut |ss| {
            found |= (0..ss.len()).any(|i| {
                matches!(ss[i], St::Store { place: vyrn_frontend::core::Place::Name(m), .. } if m == x)
                    && self.core_rebuilt( ss, i).is_none()
            });
        });
        found
    }

    /// Whether a rebuild in `ss` hands `n` back to the place it was taken
    /// from ([`Fn_::core_rebuilt`]).
    fn core_hands_back(&self, ss: &[St], n: Name) -> bool {
        (1..ss.len()).any(|i| {
            matches!(ss[i], St::Store { .. })
                && self.core_rebuilt(ss, i).is_some_and(|(r, _)| r == n)
        }) || (ss.iter().flat_map(St::lists)).any(|l| self.core_hands_back(l, n))
    }

    /// Whether a value of `from` is one of `to` with no instruction ([`crate::coerce_plan`]):
    /// an alias's type, or a function type under another spelling.
    fn core_as_is(&self, from: &Type, to: &Type) -> bool {
        matches!(
            crate::coerce_plan(&self.cx.sub(from), &self.cx.sub(to), &self.cx.types),
            crate::Rung::Identity | crate::Rung::FnRetag
        )
    }

    /// Whether the `let` at `ss[i]` binds a temporary the next `return` hands back, read once at
    /// the result's type with only other names' releases between, so it is built in `dest`.
    fn core_lands(&self, ss: &[St], i: usize, reads: &[u32]) -> bool {
        let body = self.body();
        let St::Let(n, _) = &ss[i] else {
            return false;
        };
        let info = &body.names[n.index()];
        self.dest.is_some()
            && info.binding.is_none()
            && reads[n.index()] == 1
            && self.core_as_is(&info.ty, &self.ret_ty)
            && matches!(ss[i + 1..].iter().find(|s| {
                    !matches!(s, St::Row { .. } | St::Check(_))
                        && !matches!(s, St::Drop(d, ..) if d != n)
                }),
                Some(St::Return { value: Some(Val::Name(r)), .. }) if r == n)
    }

    /// The first name the statement `s` reads, which is the only one the
    /// operand stack can be carrying for it. None for an aggregate call, a
    /// variant and a store into a place, whose destination goes on the stack
    /// before their parts, for `@codeSplice`, whose tag goes before its
    /// value, and for a call through a stored value, whose value goes there
    /// first.
    fn core_first_read(&self, s: Option<&St>) -> Option<Name> {
        let body = self.body();
        match s? {
            St::Let(_, Rhs::Call { kind, .. })
            | St::Do {
                rhs: Rhs::Call { kind, .. },
                ..
            } if self.core_through(*kind).is_some() => None,
            St::Let(_, rhs)
                if self.core_ctor(rhs)
                    || self.core_agg_call(rhs)
                    || self.core_rebuild(rhs)
                    || matches!(rhs, Rhs::Call { callee, kind, args, .. }
                        if self.core_removes( callee, *kind, args).is_some()
                            || callee == "@codeSplice") =>
            {
                None
            }
            St::Store { place, .. }
                if !matches!(place, vyrn_frontend::core::Place::Name(n)
                    if self.core_framed(&body.names[n.index()].ty)) =>
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

    /// Whether a call row's builtin is one [`Fn_::core_call`] emits. The checker counted the
    /// operands against the row ([`vyrn_frontend::prelude::Arity`]); the arms below read only
    /// what it did not state.
    fn core_builtin_readable(
        &self,
        callee: &str,
        kind: Callee,
        args: &[(Arg, vyrn_frontend::ast::Capability)],
    ) -> bool {
        let body = self.body();
        match core_builtin(self.cx, callee, kind) {
            Some(
                Spec::Typed(..)
                | Spec::OwnType
                | Spec::Barrier
                | Spec::Renders(_)
                | Spec::Effect(_)
                | Spec::Logs
                | Spec::Traps
                | Spec::Asserts,
            ) => true,
            // A lane index is an immediate, so the row carries it as a literal.
            Some(Spec::Lanes) => {
                !matches!(callee, "@lane" | "@replaceLane")
                    || arg_vals(args).is_some_and(|vs| core_lane(&vs, 1, 4).is_some())
            }
            Some(Spec::Host) => self.cx.gen.is_some(),
            // A removal that hands back a scalar leaves it on the stack; one
            // that hands back an aggregate is an aggregate call.
            Some(Spec::Removes) => self.core_removes(callee, kind, args) == Some(false),
            Some(Spec::Finds) => matches!(args, [(Arg::Val(Val::Name(x)), _), _]
                if matches!(self.cx.resolve(&body.names[x.index()].ty), Type::Map(..))),
            // A rebuild is read together with the store after it
            // ([`Fn_::core_rebuilt`]), a built aggregate as an aggregate call
            // ([`Fn_::core_agg_call`]), and a routed builtin as the declared
            // call [`Fn_::core_sig`] answers for; none alone.
            Some(Spec::Rebuilds | Spec::Builds(_) | Spec::Routes(_) | Spec::Pulls) | None => false,
        }
    }

    /// Whether `l o r` is `Code + Code`, the host's concatenation
    /// ([`Fn_::host`]), which exists only while a generator runs.
    fn core_code_concat(&self, o: BinOp, l: &Val, r: &Val) -> bool {
        let is_code = |v: &Val| matches!(self.cx.resolve(&self.core_ty( v, &Type::Int)), Type::Named(n) if n == "Code");
        self.cx.gen.is_some() && o == BinOp::Add && is_code(l) && is_code(r)
    }

    /// Whether `l o r` is a String operator [`Fn_::str_bin`] or
    /// [`Fn_::str_match`] writes: a String on the left, and a String or, for
    /// `=~`, the pattern literal on the right.
    fn core_str_op(&self, o: BinOp, l: &Val, r: &Val) -> bool {
        let is_str = |v: &Val| self.cx.resolve(&self.core_ty(v, &Type::Int)) == Type::Str;
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

    fn core_rhs_readable(&self, rhs: &Rhs) -> bool {
        match rhs {
            Rhs::Val(v) => self.core_val_readable(v),
            Rhs::Prim(Op::Closure(_), ..) => false,
            Rhs::Prim(Op::Bin(o), vs, _) if matches!(vs.as_slice(), [l, r] if self.core_str_op( *o, l, r)) => {
                vs.iter().all(|v| self.core_val_readable(v))
            }
            Rhs::Prim(Op::Bin(o), vs, _) if matches!(vs.as_slice(), [l, r] if self.core_code_concat( *o, l, r)) => {
                vs.iter().all(|v| self.core_val_readable(v))
            }
            Rhs::Prim(_, vs, _) => vs.iter().all(|v| self.core_operand(v)),
            // A handed-back receiver asks nothing extra: the builder states the call and the
            // store back as two rows, each read where it stands.
            Rhs::Call {
                callee,
                args,
                kind,
                solved,
                targets,
                ..
            } => {
                self.core_removes(callee, *kind, args) == Some(false)
                    || self.core_args_readable(args)
                        && (arg_vals(args).is_some() || self.core_user_callee(callee, *kind))
                        && (self.core_builtin_readable(callee, *kind, args)
                            || (kind.direct() && self.is_extern(callee))
                            || (kind.direct() && self.cx.skipped.contains(callee))
                            || self.core_named(callee, *kind).is_some()
                            || self.core_mem_ty(callee, args.len()).is_some()
                            || self
                                .core_sig(callee, *kind, solved, targets)
                                .is_some_and(|s| {
                                    // A plain `call` has no out-pointer for an aggregate
                                    // result; [`Fn_::core_agg_call`] admits those.
                                    s.params.len() == args.len()
                                        && matches!(
                                            self.cx.repr(&s.ret_ty, 0),
                                            Ok(Repr::Scalar(_) | Repr::Unit)
                                        )
                                }))
            }
            // A place whose value fits one wasm local, a String pointer as much as an `Int64`
            // ([`Fn_::core_read`]). A take loads the same value and leaves a hole, which the
            // place's release rows carry.
            Rhs::Read(p) | Rhs::Take(p) => {
                self.core_place_ty(p).is_some_and(|t| self.core_framed(&t))
            }
            _ => false,
        }
    }

    /// Whether `v` is an operand this walk's arithmetic rows compute with, which is narrower
    /// than [`Fn_::core_val_readable`]. A `where` type computes as its base, and a vector as one
    /// `v128`. A String literal is refused here: `"a" < "b"` has no name for another screen.
    fn core_operand(&self, v: &Val) -> bool {
        let body = self.body();
        match v {
            Val::Name(n) => {
                let t = self.cx.resolve(&body.names[n.index()].ty);
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

/// Gives back the slots row `s` took for its own work once it is written: every slot above
/// `mark`, the frame before the row, and above each slot the row took for a name, which may be
/// a parent's ([`Fn_::core_part_dest`]). A row that holds rows gives back through each of them.
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
fn first_read(s: &St) -> Option<Name> {
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
/// closure, `fn() -> T`, which the core reads and calls.
fn thunk_of(ty: Type) -> Type {
    match ty {
        Type::Lazy(inner) => Type::Fn(Vec::new(), inner),
        ty => ty,
    }
}

/// The place the name `n` was taken from, when its one `let` is a take.
fn core_taken<'r>(lets: &[(Name, &'r Rhs)], n: Name) -> Option<&'r vyrn_frontend::core::Place> {
    let mut at = lets.iter().filter(|(b, _)| *b == n);
    match (at.next(), at.next()) {
        (Some((_, Rhs::Take(p))), None) => Some(p),
        _ => None,
    }
}

/// Whether the take `n` moves on at the next row and nowhere else: the value a store writes,
/// or a `consume` argument no other argument's root shares. The part then moves straight to
/// its destination and needs no slot. The store does not write the take's root, so no row
/// between the take and the move writes the field.
fn core_moves_on(
    body: &vyrn_frontend::core::Body,
    lets: &[(Name, &Rhs)],
    occurs: &[u32],
    n: Name,
) -> bool {
    let Some((root, _)) = core_taken(lets, n).and_then(vyrn_lower::kernel::root_of) else {
        return false;
    };
    let rooted = |p: &vyrn_frontend::core::Place| {
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
        found = match next_row(ss, i) {
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
    found && occurs[n.index()] == 2
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
fn core_lets<'r>(s: &'r St, out: &mut Vec<(Name, &'r Rhs)>) {
    out.extend(s.rows().filter_map(|(r, _)| match r {
        St::Let(n, rhs) => Some((*n, rhs)),
        _ => None,
    }));
}

/// `ss` and every list of rows inside it, each before the lists inside it.
fn each_list(ss: &[St], f: &mut dyn FnMut(&[St])) {
    f(ss);
    ss.iter().flat_map(St::lists).for_each(|l| each_list(l, f));
}

/// The place a discarded read of `p` emits: a key read emits its map's, because a
/// lookup cannot trap and nothing reads its `Option`.
fn core_discarded(p: &vyrn_frontend::core::Place) -> &vyrn_frontend::core::Place {
    match p {
        vyrn_frontend::core::Place::Key(map, _) => map,
        p => p,
    }
}

/// The parts of the header `base` names, when it is a borrow a loop walks.
fn core_header(w: &Walked, base: &vyrn_frontend::core::Place) -> Option<Walk> {
    if let vyrn_frontend::core::Place::Name(n) = base {
        if let Some(Some(walk)) = w.walks.get(n.index()) {
            return Some(walk.clone());
        }
    }
    let (_, h) = w.over.iter().rev().find(|(r, _)| r == base)?;
    w.walks[h.index()].clone()
}

/// The names a statement writes, with the capability: a store's root with `None`, and a
/// `modify` or `consume` argument. [`Fn_::core_alias`] reads it.
///
/// A store into an element of an Array writes none: it lands in the buffer,
/// which a name and a copy of its header share, and it moves no header
/// ([`vyrn_lower::kernel::in_element`]).
fn core_written(names: &[NameInfo], s: &St, out: &mut Vec<(Name, Option<Capability>)>) {
    let args = |r: &Rhs, out: &mut Vec<(Name, Option<Capability>)>| {
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
    for (s, _) in s.rows() {
        match s {
            St::Let(_, r) | St::Do { rhs: r, .. } => args(r, out),
            St::Store { place, .. } => out.extend(
                vyrn_lower::kernel::root_of(place)
                    .filter(|(n, path)| {
                        !(vyrn_lower::kernel::in_element(path)
                            && matches!(names[n.index()].ty, Type::Array(_)))
                    })
                    .map(|(n, _)| (n, None)),
            ),
            _ => {}
        }
    }
}

/// The rows that run after the `let` of `n`: its later siblings and those of
/// every statement around it. `None` where no list binds `n`. A loop's
/// earlier rows run again only after `n`'s extent ends, since the `let` in
/// the loop binds `n` anew each turn.
fn core_after(ss: &[St], n: Name) -> Option<Vec<&St>> {
    ss.iter().enumerate().find_map(|(i, s)| {
        let mut after = match s {
            St::Let(b, _) if *b == n => Vec::new(),
            s => s.lists().find_map(|l| core_after(l, n))?,
        };
        after.extend(&ss[i + 1..]);
        Some(after)
    })
}

/// The rows of `ss`, or of a list inside it, from the `let` of `n` to the row
/// its extent ends at ([`vyrn_lower::core::extent_ends`]). `None` where no
/// list both binds `n` and holds every row that names it.
fn core_extent<'r>(ss: &'r [St], n: Name, occurs: &[u32]) -> Option<&'r [St]> {
    if let Some(at) = ss
        .iter()
        .position(|s| matches!(s, St::Let(m, _) if *m == n))
    {
        let ends = vyrn_lower::core::extent_ends(ss, occurs);
        let end = ends.iter().position(|e| e.contains(&n))?;
        return Some(&ss[at..=end]);
    }
    (ss.iter().flat_map(St::lists)).find_map(|l| core_extent(l, n, occurs))
}

/// The signature of one instance of the generic `f`: its parameters and its
/// result under `subst`, with no type parameters and no body.
fn instance_shell(f: &Function, subst: &HashMap<String, Type>) -> Function {
    let mut sf = shell_of(f);
    for p in &f.params {
        sf.params.push(Param {
            id: Id::NEW,
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

/// The signature of `f`'s specialization per `targets`, and what each `fn` parameter is bound
/// to inside it. A `fn` parameter becomes its target's captures at the same place, so
/// arguments evaluate in the interpreter's order. A capture parameter's name holds an `@`,
/// which no Vyrn identifier can, so nothing the body names shadows it.
fn ho_shell(
    cx: &Cx<'_>,
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
                id: Id::NEW,
                name: p.name.clone(),
                capability: p.capability,
                ty: ftypes::substitute(&p.ty, subst),
                line: p.line,
                col: p.col,
            });
            continue;
        }
        let Some(target) = bound.next() else { break };
        // A stored value's one capture is the value itself, so a `consume` parameter consumes
        // it and the prologue releases it ([`Cx::declared_param`]). A lambda's captures are read.
        let value = cx.is_dispatcher(target.sig.index);
        let mut cap_srcs = Vec::new();
        for t in &target.sig.params[..target.ncaps] {
            let n = format!("@cap{}", sf.params.len());
            sf.params.push(Param {
                capability: match p.capability {
                    Capability::Consume if value => Capability::Consume,
                    _ => Capability::Read,
                },
                ..Param::synth(n.clone(), t.clone())
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

/// The names a run's switches place themselves: every payload binder, and each scrutinee when
/// `ons`. [`Fn_::core_switch`] binds a payload binder on entering the arm and reads a
/// scrutinee as an address, so the screen's place clause asks about neither.
fn core_switched(s: &St, ons: bool, out: &mut Vec<Name>) {
    for (r, _) in s.rows() {
        let St::Switch { on, arms, .. } = r else {
            continue;
        };
        if let (Val::Name(n), true) = (on, ons) {
            out.push(*n);
        }
        arms.iter().for_each(|a| out.extend(&a.binds));
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

/// Whether a run leaves the list it stands in anywhere under it: a `return`,
/// or a `break` or a `continue` outside a loop of its own.
fn core_leaves(s: &St) -> bool {
    s.rows().any(|(r, depth)| match r {
        St::Return { .. } => true,
        St::Break { .. } | St::Continue { .. } => depth == 0,
        _ => false,
    })
}

/// Pushes the `i64` word at `off` past the address in local `a` as an `i32`: a length, a
/// capacity or a box pointer, which memory holds as `i64` and every address computes with as
/// `i32`.
fn load_wrapped(b: &mut Frame, a: u32, off: u32) {
    b.ins(&Instruction::LocalGet(a))
        .ins(&Instruction::I64Load(at(off)))
        .ins(&Instruction::I32WrapI64);
}

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
/// between a rebuild and its store ([`Fn_::core_rebuilt`]). A check row emits
/// nothing where it stands, so it counts with them.
fn drops_ahead<'s>(ss: impl Iterator<Item = &'s St>) -> usize {
    ss.take_while(|s| matches!(s, St::Drop(..) | St::Check(_)))
        .count()
}

/// The row after `ss[i]` that emits where it stands: a check row runs inside
/// the row it guards.
fn next_row(ss: &[St], i: usize) -> Option<&St> {
    ss[i + 1..].iter().find(|s| !matches!(s, St::Check(_)))
}

/// The latest check row walked whose guard `hit` accepts: the row a check the
/// emitter is about to run stands for ([`Fn_::row`] says whether it runs). A row
/// stands before the row it guards, and a store that puts a taken element back
/// follows the take's row.
fn core_check(w: &mut Walked, line: usize, hit: impl Fn(&Guard) -> bool) -> Result<Check, String> {
    match w.checks.iter_mut().rev().find(|(c, _)| hit(&c.guard)) {
        Some((c, ran)) => {
            *ran = true;
            Ok(c.clone())
        }
        None => unsupported("a runtime check the core did not state", line),
    }
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

    /// A single-source program loaded as the CLI loads it, with the `std/runtime` the loader
    /// injects into every program.
    fn linked(src: &str) -> Result<Program, String> {
        let files = vyrn_frontend::loader::MapResolver(
            [
                ("main.vyrn", src),
                (
                    "std/runtime.vyrn",
                    include_str!("../../../std/runtime.vyrn"),
                ),
                ("std/mem.vyrn", include_str!("../../../std/mem.vyrn")),
                // The runtime's `intStr` makes a `String` from bytes, whose check is
                // `std/text`'s, so every linked program needs the module.
                ("std/text.vyrn", include_str!("../../../std/text.vyrn")),
            ]
            .into_iter()
            .map(|(k, v)| (k.to_string(), v.to_string()))
            .collect(),
        );
        let opts = vyrn_frontend::loader::LoadOptions {
            std_root: Some("std".into()),
            expansions: vyrn_frontend::project::Expansions::shared(),
            ..Default::default()
        };
        vyrn_lower::load(src, "main.vyrn", &opts, &files, None)
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
                builtin_spec(name, params.len(), false).is_some(),
                "`{name}` has a row and no instruction"
            );
        }
    }

    /// `addr` and `adopt` change a type and emit nothing, so [`Fn_::core_mem`] answers them
    /// before the table.
    #[test]
    fn every_mem_declaration_has_a_row() {
        let program = linked("fn main() -> Int64 {\n    return 0\n}\n").unwrap();
        let c = cx();
        for f in program.functions.iter().filter(|f| f.exported) {
            if let Some(prim) = f.name.strip_prefix(vyrn_frontend::loader::MEM_PREFIX) {
                let emits = mem_ins(&c, prim).is_some();
                assert_eq!(emits, !matches!(prim, "addr" | "adopt"), "`std/mem.{prim}`");
            }
        }
    }

    /// A runtime name is declared once, so [`VyrnRt::reserve`] hands out one index per row and
    /// [`VyrnRt::take`] finds the row a body belongs to. [`VyrnRt::check`] refuses a link where a
    /// row has no body.
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
            world: Default::default(),
            types: HashMap::new(),
            lambdas: HashMap::new(),
            layouts: RefCell::default(),
            reprs: RefCell::default(),
            oracle: None,
            sigs: HashMap::new(),
            gen: None,
            generics: HashMap::new(),
            higher_order: HashMap::new(),
            skipped: std::collections::HashSet::new(),
            mem: HashMap::new(),
            subst: HashMap::new(),
            mono: RefCell::new(Mono::default()),
            fnvals: RefCell::new(Vec::new()),
            fnval_copy: 0,
            fnval_free: 0,
            dispatch: RefCell::new(Dispatch::default()),
            shapes: RefCell::new(Shapes::default()),
            globals: HashMap::new(),
            args_in_place: false,
            gappend: HashMap::new(),
            externs: HashMap::new(),
            // `Program`'s defaults: nothing here logs.
            log_level: DEFAULT_LOG_LEVEL,
            log_sink: LogSink::Stderr,
            log_fd: None,
            audit: false,
            profile: None,
            // Every index 0: a `Cx` for a type-level test never emits a call.
            rt: Rt::default(),
        }
    }

    /// The whole aggregate ABI in one assertion: a scalar is a wasm value, an
    /// aggregate is an `i32` address.
    #[test]
    fn an_aggregate_travels_as_the_address_of_its_slot() {
        let c = cx();
        assert_eq!(c.repr(&Type::Int, 0).unwrap(), Repr::Scalar(ValType::I64));
        assert_eq!(c.repr(&Type::Bool, 0).unwrap(), Repr::Scalar(ValType::I32));
        // A String is a pointer, so it is a scalar.
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
        // `{ i1, i64 }`: the byte, then seven bytes of padding.
        assert_eq!(
            r.unwrap(),
            Repr::Agg(Rc::new(Layout {
                size: 16,
                align: 8,
                fields: vec![0, 8]
            }))
        );
        assert_eq!(
            c.repr(&Type::option(Type::Int), 0).unwrap().val(),
            Some(ValType::I32)
        );
    }

    /// `shape_of` gives `Void` for an escaped type parameter, which would shrink a function
    /// silently. Every type goes through [`Cx::sub`] first; this asserts the refusal outside an
    /// instance and the substitution inside one.
    #[test]
    fn a_type_parameter_is_substituted_before_it_can_reach_a_layout() {
        let t = Type::Param("T".into());
        let mut c = cx();
        // Outside a monomorphization: refused, and the shape is `Void`.
        assert!(c.repr(&t, 0).is_err());
        assert_eq!(c.shape(&t), Shape::Void);
        // Inside one: the type the instantiation fixed, at every entry point.
        c.subst.insert("T".into(), Type::Int);
        assert_eq!(c.repr(&t, 0).unwrap(), Repr::Scalar(ValType::I64));
        let i64 = Shape::Leaf(layout::Leaf::I64);
        assert_eq!(c.shape(&t), i64);
        assert_eq!(c.resolve(&t), Type::Int);
        assert!(c.ty_gap(&t, 0).is_none());
        // Through a constructor too: the element stride depends on `T`.
        assert_eq!(
            c.shape(&Type::ArrayN(Box::new(t.clone()), 3)),
            Shape::Array(3, Box::new(i64.clone()))
        );
        assert_eq!(
            c.shape(&Type::option(t)),
            Shape::Struct(vec![i64.clone(), i64])
        );
    }

    /// A validated type has its base's representation, so a lowering that forgets the check
    /// passes every refinement example. This asserts the check is emitted, bare and inside a
    /// record field.
    #[test]
    fn a_validated_type_is_checked_wherever_it_is_reached() {
        let msg = "validation failed for `Age`";
        let bare = "type Age = Int64 where value >= 18 \
                    fn f(n: Int64) -> Int64 { let a: Age = n return a }
                    fn main() -> Int64 { return f(20) }";
        // Inside a record field: nothing about `{ i64 }` says the word is refined.
        let hidden = "type Age = Int64 where value >= 18 \
                      type U = { age: Age } \
                      fn f(n: Int64) -> Int64 { let u = U { age: n } return u.age }
                      fn main() -> Int64 { return f(20) }";
        for (what, src) in [("bare", bare), ("in a record", hidden)] {
            let p = linked(src).expect(what);
            let bytes = compile(&p, vyrn_lower::analyze(&p)).expect(what);
            assert!(
                bytes.windows(msg.len()).any(|w| w == msg.as_bytes()),
                "{what}: no `where` check was emitted"
            );
        }
        // The message is the constructor's `panic` string, so any module declaring the type
        // carries it; the check is the call. A constant the checker proved emits no call,
        // so its module is the smaller.
        let proved = "type Age = Int64 where value >= 18                       fn f(n: Int64) -> Int64 { let a = Age(20) return a }
                      fn main() -> Int64 { return f(20) }";
        let p = linked(proved).unwrap();
        let small = compile(&p, vyrn_lower::analyze(&p)).unwrap();
        let p = linked(bare).unwrap();
        let big = compile(&p, vyrn_lower::analyze(&p)).unwrap();
        assert!(
            big.len() > small.len(),
            "a proven constant emitted a check: {} against {}",
            big.len(),
            small.len()
        );
    }

    /// A branch yielding `Ok(..)`/`Err(..)`, the shape `std/json` re-wraps a `stringFromBytes`
    /// result with, is typed by its position.
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
        assert!(compile(&p, vyrn_lower::analyze(&p)).is_ok());
    }

    /// `.length` in a branch, on every receiver that has one ([`length_ty`]).
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
                compile(&p, vyrn_lower::analyze(&p)).is_ok(),
                "{what}: {:?}",
                compile(&p, vyrn_lower::analyze(&p)).unwrap_err()
            );
        }
    }

    /// Each shape below compiles outside a branch and must type inside one.
    #[test]
    fn a_branch_types_every_shape_the_emitting_path_lowers() {
        let cases = [
            // `Int32(n)`: the frontend's conversion table, which `call` also reads.
            (
                "a numeric conversion",
                "fn main() -> Int64 { let o: Option<Int64> = Some(1)                      let x: Int32 = match o { Some(n) => Int32(n), None => Int32(0) } return 0 }",
            ),
            // An empty `[]` is typed by the position, in an arm like anywhere.
            (
                "an empty array",
                "fn main() -> Int64 { let o: Option<Int64> = Some(1)                      let a: Array<Int64> = match o { Some(n) => [], None => [] }                      return a.length }",
            ),
            // `?` in an arm: the sum's success half.
            (
                "a propagation",
                "fn f(n: Int64) -> Option<Int64> { return Some(n) }                  fn g() -> Option<Int64> { let o: Option<Int64> = Some(1)                      return Some(match o { Some(n) => f(n)?, None => 0 }) }                  fn main() -> Int64 { return 0 }",
            ),
            // `Age?(n)` in an arm: an `Option` of the named type.
            (
                "a fallible construction",
                "type Age = Int64 where value >= 0                  fn main() -> Int64 { let o: Option<Int64> = Some(1)                      let r: Option<Age> = match o { Some(n) => Age?(n), None => Age?(0) }                      return 0 }",
            ),
        ];
        for (what, src) in cases {
            let p = linked(src).expect(what);
            let r = compile(&p, vyrn_lower::analyze(&p));
            assert!(r.is_ok(), "{what}: {}", r.unwrap_err());
        }
    }
}
