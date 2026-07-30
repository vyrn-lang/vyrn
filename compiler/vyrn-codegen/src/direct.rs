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

use vyrn_frontend::ast::*;
use vyrn_frontend::types as ftypes;

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

/// Whether the emitted module stands alone or links against the shared runtime
/// shim (RFC-0077 M2i).
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Link {
    /// One self-contained module: it defines its own memory, its own heap and its
    /// own runtime. What `vyrn build --target wasm` produces, and the only shape
    /// that needs no C toolchain to exist.
    Standalone,
    /// Memory and the C runtime come from RFC-0076's pre-compiled shim, linked at
    /// instantiation — the same split its generator artifacts use, so one
    /// `malloc` heap and one address space.
    Shim,
}

/// The shim entry points this backend calls when there is a shim beside it.
///
/// NAMES only. Every signature comes from [`wasm::boundary`] — the textual
/// emitter's own `declare` line for the same function, which is the side
/// `tests/imports_vs_shim.rs` proves agrees with the C. Writing a signature down
/// here would be a second chance to get one wrong, and a wrong import signature
/// is a misread argument rather than a link error.
const SHIM_IMPORTS: &[&str] = &["__vyrn_malloc"];

/// The `wasi_snapshot_preview1` calls a directly-emitted module makes.
///
/// **All of them, unconditionally.** An import has to be declared before the
/// first body (M1: one index space, imports at the bottom), and nothing knows
/// which builtins a program reaches until the bodies are walked — so the
/// alternative was a pre-scan over the AST, i.e. a second traversal that has to
/// agree with lowering about what it needs. That is the failure mode `llt_of`
/// (M2b) and `predicate_binds` (M2d) exist to prevent, and M2e refused a
/// standalone instantiation walker for the same reason.
///
/// Cheap because the set is already implemented twice over: wasmtime provides all
/// of preview1, and `web/wasi-min.js` implements exactly these for the browser —
/// with RFC-0014's graceful degradation (no argv, EOF on stdin, no preopens, every
/// `path_open` NOENT), which is what a page's `readFile` is supposed to be.
#[derive(Clone, Copy)]
struct Wasi {
    fd_write: u32,
    fd_read: u32,
    fd_close: u32,
    proc_exit: u32,
    path_open: u32,
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
        fd_prestat_get: im("fd_prestat_get", &[I32, I32], &[I32]),
        args_sizes_get: im("args_sizes_get", &[I32, I32], &[I32]),
        args_get: im("args_get", &[I32, I32], &[I32]),
        environ_sizes_get: im("environ_sizes_get", &[I32, I32], &[I32]),
        environ_get: im("environ_get", &[I32, I32], &[I32]),
        clock_time_get: im("clock_time_get", &[I32, I64, I32], &[I32]),
        random_get: im("random_get", &[I32, I32], &[I32]),
    }
}

/// Compile a whole program to a standalone `wasm32-wasi` module.
pub fn compile(program: &Program) -> Result<Vec<u8>, String> {
    compile_linked(program, Link::Standalone)
}

/// Compile a whole program, choosing whether it stands alone or imports the
/// shared shim.
pub fn compile_linked(program: &Program, link: Link) -> Result<Vec<u8>, String> {
    let mut m = Module::new();
    if link == Link::Shim {
        m.import_memory();
    }
    // Imports first — they share the function index space with definitions, so
    // `wasm::Module` panics if one arrives late. Which is also why the shim's
    // imports are a fixed list rather than discovered as bodies need them: an
    // index handed out after the first definition would renumber every call.
    let wasi = wasi_imports(&mut m);
    let mut shim: HashMap<&'static str, u32> = HashMap::new();
    if link == Link::Shim {
        for name in SHIM_IMPORTS {
            let Some(Some((params, ret))) = wasm::boundary().get(*name) else {
                return Err(format!("direct backend: `{name}` is not a boundary signature"));
            };
            // A `void` parameter is not a thing any `declare` has; a `void` result
            // is, and is the empty result list.
            let params: Vec<ValType> = params.iter().flatten().copied().collect();
            let ret: Vec<ValType> = ret.iter().copied().collect();
            let index = m.import("env", name, &params, &ret);
            shim.insert(name, index);
        }
    }

    let rt = runtime(&mut m, &wasi, shim.get("__vyrn_malloc").copied());

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
    let mut higher_order: HashMap<String, usize> = HashMap::new();
    let mut user: Vec<&Function> = Vec::new();
    let mut externs: HashMap<String, Type> = HashMap::new();
    for f in &program.functions {
        // An `extern` is an import; a `gen fn` (RFC-0021) runs only in the
        // compiler's own interpreter and may use builtins with no lowering at all.
        if f.is_extern || f.is_gen {
            if f.is_extern {
                externs.insert(f.name.clone(), f.ret.clone());
            }
            continue;
        }
        // RFC-0023: a function taking a `fn`-typed parameter exists only as
        // higher-order specializations — a SECOND worklist, which this milestone
        // does not build. Recorded by name so the ladder can say RFC-0023 rather
        // than reporting the call as an unknown one.
        if f.params.iter().any(|p| matches!(p.ty, Type::Fn(..))) {
            higher_order.insert(f.name.clone(), f.line);
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
        variants,
        generics,
        higher_order,
        protocol_methods,
        subst: HashMap::new(),
        mono: RefCell::new(Mono::default()),
        globals: HashMap::new(),
        externs,
    };

    // Every function the module will define, in the order they are defined, so a
    // call can name an index before the callee's body exists. Recursion and
    // forward references both need this; there is no fixup pass.
    let first = m.n_imports() + rt.count;
    for (i, f) in user.iter().enumerate() {
        let s = cx.signature(f)?;
        cx.sigs.insert(f.name.clone(), Sig { index: first + i as u32, ..s });
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

    // The initializer sits between the ordinary definitions and the
    // specializations, because an index is only ever handed out ahead of a body
    // that will be added in the same order. Specializations come last, since all
    // of those are indexed before any body is walked and none can be discovered
    // late.
    let init_index = first + user.len() as u32;
    let has_globals = !program.globals.is_empty();
    cx.mono.borrow_mut().base = init_index + u32::from(has_globals);

    for f in &user {
        let sig = cx.sigs[&f.name].clone();
        lower_fn(&mut m, f, &sig, &cx)?;
    }

    // The initializers, in DECLARATION order — which the loader has already made
    // linker order, dependencies first, so `statemod`'s diamond initializes its
    // shared store before either arm reads it. One function, called once from
    // `_start`, so nothing runs per call and nothing runs twice.
    if has_globals {
        let init = lower_globals_init(&mut m, program, &cx)?;
        assert_eq!(init, init_index, "the globals initializer took an index a caller did not name");
    }

    // Drain the specializations the bodies discovered. A specialization's body
    // may discover more — including of a generic it is itself an instance of —
    // so this reads `insts` afresh every turn rather than iterating a snapshot.
    loop {
        let inst = {
            let mono = cx.mono.borrow();
            mono.insts.get(mono.done).cloned()
        };
        let Some(inst) = inst else { break };
        let f = cx.generics[&inst.name].clone();
        cx.subst = inst.subst;
        lower_fn(&mut m, &f, &inst.sig, &cx)?;
        cx.subst = HashMap::new();
        cx.mono.borrow_mut().done += 1;
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
    let start = m.func(&[], &[], &[], 0, |b| {
        if has_globals {
            b.ins(&Instruction::Call(init_index));
        }
        b.ins(&Instruction::Call(main))
            .ins(&Instruction::I64Const(255))
            .ins(&Instruction::I64And)
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::Call(wasi.proc_exit));
    });
    m.export("_start", start);
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
#[derive(Clone)]
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

/// One specialization of a generic function.
///
/// The signature's parameter and return types are **already substituted**: a
/// caller has its own `subst` and must not apply it to a callee's concrete types.
#[derive(Clone)]
struct Inst {
    name: String,
    type_args: Vec<Type>,
    subst: HashMap<String, Type>,
    sig: Sig,
}

/// The monomorphization worklist (RFC-0077 M2e).
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
#[derive(Default)]
struct Mono {
    /// Where the first specialization's function index lands.
    base: u32,
    insts: Vec<Inst>,
    done: usize,
}

struct Cx {
    types: HashMap<String, TypeDecl>,
    sigs: HashMap<String, Sig>,
    rt: Rt,
    /// Variant name → every enum that declares it, with the variant's tag and
    /// payload types. A name may belong to two enums, which is why the
    /// expectation decides and an ambiguous one is a gap rather than a guess.
    variants: HashMap<String, Vec<(String, u64, Vec<Type>)>>,
    /// Generic functions by name. They have no index and no body of their own —
    /// only specializations do — so a call to one is a discovery.
    generics: HashMap<String, Function>,
    /// Functions with a `fn`-typed parameter (RFC-0023), and where each is
    /// declared. Refused by name rather than by symptom.
    higher_order: HashMap<String, usize>,
    /// Protocol method name → its protocol (RFC-0002 §5). A bounded generic is
    /// what protocols are for, so `x.show()` inside one has to resolve.
    protocol_methods: HashMap<String, String>,
    /// The monomorphization whose body is being lowered; empty for an ordinary
    /// function.
    subst: HashMap<String, Type>,
    mono: RefCell<Mono>,
    /// Module state (RFC-0013): name → its fixed address and declared type. Every
    /// body sees all of them, which is the textual backend's `globals` fallback in
    /// [`Gen::lookup`] — the checker already forbids an initializer reading a
    /// global declared after it, so there is nothing for a partial view to catch.
    globals: HashMap<String, (Place, Type)>,
    /// `extern fn` declarations by name, with the return type they declared. The
    /// bodies are skipped, but RFC-0043's host boundary is reached BY NAME, and
    /// what it returns is the declaration's business rather than this file's.
    externs: HashMap<String, Type>,
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

    /// The signature of `f` specialized at `type_args`, discovering it — and
    /// handing out its function index — if this is the first site to ask.
    fn instantiate(
        &self,
        f: &Function,
        type_args: Vec<Type>,
        subst: HashMap<String, Type>,
    ) -> Result<Sig, String> {
        // Keyed on the type arguments themselves rather than on a mangled name.
        // `mangle_name` is the textual backend's SYMBOL, and it is not injective
        // (every record mangles as `Rec`); a wasm function has no symbol at all,
        // so there is nothing to gain by narrowing the key to a string that two
        // distinct specializations could share.
        if let Some(i) =
            self.mono.borrow().insts.iter().find(|i| i.name == f.name && i.type_args == type_args)
        {
            return Ok(i.sig.clone());
        }
        // The signature is built under the SPECIALIZATION's substitution, not
        // whatever the discovering body happens to be inside. `signature` then
        // does the rest — the `modify` refusal, a representation per parameter —
        // so an instance and an ordinary function are checked by one function.
        let mut sf = f.clone();
        sf.type_params.clear();
        for p in &mut sf.params {
            p.ty = ftypes::substitute(&p.ty, &subst);
        }
        sf.ret = ftypes::substitute(&f.ret, &subst);
        let s = self.signature(&sf)?;

        let mut mono = self.mono.borrow_mut();
        let sig = Sig { index: mono.base + mono.insts.len() as u32, ..s };
        mono.insts.push(Inst {
            name: f.name.clone(),
            type_args,
            subst,
            sig: sig.clone(),
        });
        Ok(sig)
    }

    fn repr(&self, ty: &Type, line: usize) -> Result<Repr, String> {
        if let Some(why) = self.ty_gap(ty, 0) {
            return unsupported(&why, line);
        }
        let ll = self.ll(ty);
        Ok(match ll.as_str() {
            "void" => Repr::Unit,
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
            Type::Result(a, b) => {
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

/// One function being lowered.
struct Fn_<'a> {
    cx: &'a Cx,
    /// Name → where it lives and what it is. A scope stack rather than a map per
    /// block: shadowing pushes, and leaving a block truncates.
    scope: Vec<(String, Place, Type)>,
    /// wasm blocks open between here and the function's outermost one. A
    /// `return` is `br depth`.
    depth: u32,
    /// (break target, continue target) per enclosing loop, as the depth each was
    /// opened at; `br` distance is `depth - opened - 1`.
    loops: Vec<(u32, u32)>,
    ret: Repr,
    ret_ty: Type,
    /// The wasm local holding the hidden aggregate-return pointer, if any.
    dest: Option<u32>,
    /// Reusable scratch, taken on first use. Every use is a set immediately
    /// followed by the reads that consume it, so one pair suffices however
    /// deeply expressions nest.
    scratch: HashMap<(ValType, u8), u32>,
    /// The type a value is being built FOR, innermost last.
    ///
    /// `None` and `Some(x)` do not say what they are — an `Option<T>`'s `T`
    /// comes from the position, not the constructor — so the sum constructors
    /// read it back off here. Same mechanism the LLVM emitter uses, for the
    /// same reason.
    expect: Vec<Type>,
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
        expect: Vec::new(),
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
fn lower_globals_init(m: &mut Module, program: &Program, cx: &Cx) -> Result<u32, String> {
    let mut b = Frame::new(0, &[], 0);
    let mut f = top_level(cx);
    for g in &program.globals {
        let (place, ty) = cx.globals[&g.name].clone();
        let r = cx.repr(&ty, g.line)?;
        f.store_into(m, &mut b, place, &r, &g.init, &ty)?;
    }
    Ok(m.add(&[], &[], b))
}

/// Lower one body. `sig` is passed rather than looked up because a generic
/// specialization has no entry in `Cx::sigs` — it is keyed on its type arguments,
/// not on its name, and several instances share one `Function`.
fn lower_fn(m: &mut Module, f: &Function, sig: &Sig, cx: &Cx) -> Result<(), String> {
    let sig = sig.clone();
    let (params, results) = cx.wasm_sig(&sig, f.line)?;
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
        expect: Vec::new(),
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

    m.add(&params, &results, b);
    Ok(())
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
        for s in &blk.stmts {
            self.stmt(m, b, s)?;
        }
        self.scope.truncate(mark);
        Ok(())
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
                self.scope.push((name.clone(), place, bound));
            }
            Stmt::Assign { name, value, line } => {
                let (place, ty) = self.lookup(name, *line)?;
                let r = self.cx.repr(&ty, *line)?;
                self.store_into(m, b, place, &r, value, &ty.clone())?;
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
                self.loops.push((brk, cont));
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
                self.loops.push((brk, cont));
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
                let &(brk, _) = self.loops.last().ok_or_else(|| gap("`break` outside a loop", *line))?;
                let d = self.br_to(brk);
                b.ins(&Instruction::Br(d));
            }
            Stmt::Continue { line } => {
                let &(_, cont) =
                    self.loops.last().ok_or_else(|| gap("`continue` outside a loop", *line))?;
                let d = self.br_to(cont);
                b.ins(&Instruction::Br(d));
            }
            // `drop` is reclamation, and reclamation is not observable: this
            // backend's allocator never reuses (see `runtime`). When it does,
            // this is where the free goes.
            Stmt::Drop { .. } => {}
            Stmt::Expr(e) => {
                // A call for its effect leaves its result on the stack; drop it,
                // or the block's type will not check.
                if !matches!(self.cx.repr(&self.expr(m, b, e)?, Expr::line(e))?, Repr::Unit) {
                    b.ins(&Instruction::Drop);
                }
            }
            other => return unsupported(&stmt_name(other), stmt_line(other)),
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
        let Some(pred) = decl.predicate.clone() else { return Ok(()) };
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
                    (UnOp::Not, _) if rt == Type::Bool => {
                        b.ins(&Instruction::I32Eqz);
                    }
                    _ => return unsupported("a unary operator on this type", *line),
                }
                t
            }
            Expr::ArrayLit { elems, line } => self.array_lit(m, b, elems, *line)?,
            Expr::Match { scrutinee, arms, line } => self.match_expr(m, b, scrutinee, arms, *line)?,
            Expr::Binary { op, lhs, rhs, line } => self.binary(m, b, *op, lhs, rhs, *line)?,
            Expr::Call { name, args, line } => self.call(m, b, name, args, *line)?,
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
        let want = self.peek(then_e, line)?;
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
            Repr::Unit => return unsupported("an `if` expression yielding Unit", line),
        }
        Ok(want)
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
            Expr::IfExpr { then_branch, .. } => self.peek(then_branch, line)?,
            // A `match` is typed by its first arm, like an `if` expression —
            // and like one, every other arm is checked against that.
            Expr::Match { scrutinee, arms, .. } => {
                let st = self.peek(scrutinee, line)?;
                let sum = self
                    .sum_of(&st)
                    .ok_or_else(|| gap(&format!("a `match` on `{st}`"), line))?;
                let first = arms.first().ok_or_else(|| gap("an empty `match`", line))?;
                let binds = self.pattern_binds(&sum, &first.pattern, line)?;
                // `peek` does not emit, so a scope frame it cannot mutate is
                // enough to type an arm that mentions its bindings.
                let mark = self.scope.len();
                for (n, t) in binds {
                    self.scope.push((n, Place::Local(u32::MAX), t));
                }
                let got = self.peek(&first.body, line);
                self.scope.truncate(mark);
                got?
            }
            Expr::Unary { expr, .. } => self.peek(expr, line)?,
            Expr::Binary { op, lhs, .. } => match op {
                BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
                | BinOp::And | BinOp::Or | BinOp::Match => Type::Bool,
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
                "@str" | "@concat" | "jsonSchema" | "slice" => Type::Str,
                "@charCount" => Type::Int,
                "stringFromBytes" => Type::Result(Box::new(Type::Str), Box::new(Type::Str)),
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
                // An arm that only prints: the join carries nothing, which the
                // `match` lowering already handles — it just has to be told.
                "print" => Type::Unit,
                "at" | "@swapRemove" if args.len() == 2 => {
                    let a = self.peek(&args[0], line)?;
                    match self.cx.resolve(&a) {
                        Type::Array(i) | Type::ArrayN(i, _) => *i,
                        Type::Str => Type::IntN { bits: 8, signed: false },
                        other => return unsupported(&format!("a branch indexing `{other}`"), line),
                    }
                }
                "push" | "@list" if !args.is_empty() => match self.peek(&args[0], line)? {
                    t => match self.cx.resolve(&t) {
                        Type::ArrayN(i, _) => Type::Array(i),
                        _ => t,
                    },
                },
                "@pop" if args.len() == 1 => {
                    let a = self.peek(&args[0], line)?;
                    match self.cx.resolve(&a) {
                        Type::Array(i) => Type::Option(i),
                        other => return unsupported(&format!("a branch popping `{other}`"), line),
                    }
                }
                _ if self.cx.types.get(name).is_some_and(|d| d.predicate.is_some()) => {
                    Type::Named(name.clone())
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
                // A user enum's variant constructor in a branch (`One(a)`). Its type
                // is the enum that declares the name; an ambiguous name — two enums
                // with one variant spelling — is a gap rather than a guess, exactly
                // as `sum_ctor` treats it when it emits.
                _ if self.cx.variants.contains_key(name) => {
                    let cands = &self.cx.variants[name];
                    match (cands.len(), cands.first()) {
                        (1, Some((e, _, _))) => Type::Named(e.clone()),
                        _ => {
                            return unsupported(
                                &format!("a branch yielding the ambiguous variant `{name}`"),
                                line,
                            )
                        }
                    }
                }
                _ => match self.cx.sigs.get(name) {
                    Some(s) => s.ret_ty.clone(),
                    None => return unsupported(&format!("a branch yielding `{name}`"), line),
                },
            },
            other => return unsupported(&format!("a branch yielding {}", expr_name(other)), line),
        })
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

    fn call(
        &mut self,
        m: &mut Module,
        b: &mut Frame,
        name: &str,
        args: &[Expr],
        line: usize,
    ) -> Result<Type, String> {
        match name {
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
                    // Fixed six decimals, which `f64_str` owns; a `Float32`
                    // promotes first because the interpreter formats `*f as f64`.
                    ref f if matches!(f, Type::Float | Type::Float32) => {
                        if *f == Type::Float32 {
                            b.ins(&Instruction::F64PromoteF32);
                        }
                        b.ins(&Instruction::Call(self.cx.rt.f64_str));
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
                        if *f == Type::Float32 {
                            b.ins(&Instruction::F64PromoteF32);
                        }
                        b.ins(&Instruction::Call(self.cx.rt.f64_str));
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
            "slice" if args.len() == 3 => {
                self.expr_as(m, b, &args[0], &Type::Str)?;
                self.expr_as(m, b, &args[1], &Type::Int)?;
                self.expr_as(m, b, &args[2], &Type::Int)?;
                b.ins(&Instruction::Call(self.cx.rt.slice));
                return Ok(Type::Str);
            }
            // RFC-0014's input I/O. Every one of these is a runtime function that
            // writes its whole result through a slot allocated here — the same
            // hidden destination an aggregate-returning Vyrn call gets, which is
            // why none of them needed a case outside M2b's four ABI rules.
            "args" if args.is_empty() => {
                let ty = Type::Array(Box::new(Type::Str));
                let l = self.layout_of(&ty, line)?;
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                b.ins(&Instruction::Call(self.cx.rt.args));
                b.slot(off);
                return Ok(ty);
            }
            "readLine" if args.is_empty() => {
                let ty = Type::Option(Box::new(Type::Str));
                let l = self.layout_of(&ty, line)?;
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                b.ins(&Instruction::Call(self.cx.rt.read_line));
                b.slot(off);
                return Ok(ty);
            }
            "readFile" | "readFileBytes" if args.len() == 1 => {
                let ok = if name == "readFile" {
                    Type::Str
                } else {
                    Type::Array(Box::new(Type::IntN { bits: 8, signed: false }))
                };
                let ty = Type::Result(Box::new(ok), Box::new(Type::Str));
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
            "writeFile" if args.len() == 2 => {
                let ty = Type::Result(Box::new(Type::Bool), Box::new(Type::Str));
                let l = self.layout_of(&ty, line)?;
                self.expr_as(m, b, &args[0], &Type::Str)?;
                self.expr_as(m, b, &args[1], &Type::Str)?;
                let off = b.alloc(l.size, l.align);
                b.slot(off);
                b.ins(&Instruction::Call(self.cx.rt.write_file));
                b.slot(off);
                return Ok(ty);
            }
            "@charCount" if args.len() == 1 => {
                self.expr_as(m, b, &args[0], &Type::Str)?;
                b.ins(&Instruction::Call(self.cx.rt.charcount));
                return Ok(Type::Int);
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
            "at" if args.len() == 2 => return self.at(m, b, args, line),
            "push" if args.len() == 2 => return self.push(m, b, args, line),
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
        // RFC-0023's higher-order specialization is a second worklist, and this
        // milestone builds only the generic one. Refused by name so the ladder
        // groups these as one feature rather than as N unknown calls.
        if self.cx.higher_order.contains_key(name) {
            return unsupported("a `fn`-typed parameter (RFC-0023 specialization)", line);
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
            let sig = self.cx.instantiate(&f, type_args, subst)?;
            return self.emit_call(m, b, &sig, args);
        }
        // RFC-0043's host boundary. These three are not `vyrn` host imports like
        // an ordinary RFC-0012 `extern`: the C shim defines them on every target,
        // honouring `VYRN_FIXED_TIME`/`VYRN_FIXED_SEED`, which is what makes a
        // clock example a three-way parity citizen instead of a browser-only one.
        //
        // M2i got them by reaching the shim, and M2j took that back out: the shim
        // link can never be the default (M5 needs no clang), so a shape that only
        // works linked does not advance this RFC. WASI has `clock_time_get` and
        // `random_get`, the env injection is `environ_get`, and wasi-libc's
        // `timespec_get`/`getentropy` are thin wrappers over the first two — so
        // the emitted runtime reads the same syscalls by a shorter route, in BOTH
        // link shapes rather than one.
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
            let ret = self.cx.externs.get(name).cloned().unwrap_or(Type::Unit);
            if self.cx.repr(&ret, line)? != Repr::Scalar(ValType::I64) {
                return unsupported(&format!("`{name}` declared as returning `{ret}`"), line);
            }
            b.ins(&Instruction::Call(f));
            return Ok(ret);
        }
        let Some(sig) = self.cx.sigs.get(name).cloned() else {
            return unsupported(&format!("the call `{name}`"), line);
        };
        if sig.params.len() != args.len() {
            return unsupported(&format!("the call `{name}` at this arity"), line);
        }
        self.emit_call(m, b, &sig, args)
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
            // A `SmallArray` is a four-field header with an inline buffer and
            // two live states (RFC-0056); reading it as a triple would be a
            // silent miscompile rather than a missing one.
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
            Some(Type::Array(i)) | Some(Type::ArrayN(i, _)) => Some((**i).clone()),
            _ => None,
        };
        if elems.is_empty() {
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
        b.ins(&Instruction::I32Const(bytes.max(1)));
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
        // M4 hands this to the shim's real allocator.
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
        b.ins(&Instruction::LocalGet(cap));
        b.ins(&Instruction::I32WrapI64);
        b.ins(&Instruction::I32Const(stride));
        b.ins(&Instruction::I32Mul);
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
    /// A narrower scalar, zero-extended into the word.
    Ext(ValType),
    /// Two words, side by side, no heap (a `Ref` or a stored `fn`).
    Inline2,
    /// The word is a pointer to it.
    Boxed,
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
                b.ins(&Instruction::I32Const(size as i32));
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
                b.ins(&Instruction::I32Const(l.size as i32));
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
                let cands = self.cx.variants[name].clone();
                if cands.len() != 1 {
                    return unsupported(&format!("the ambiguous variant `{name}`"), line);
                }
                let (e, tag, declared) = cands.into_iter().next().unwrap();
                // A generic enum has no type until a use site fixes it, and a
                // bare constructor's use site is its PAYLOAD — the rule
                // `Gen::applied_enum_type` gives the textual emitter, shared.
                let actual = self.arg_types(&declared, args, line)?;
                let decl = self.cx.types.get(&e).cloned();
                let ty = crate::applied_type(decl.as_ref(), &e, &declared, &actual);
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
        let first = arms.first().ok_or_else(|| gap("an empty `match`", line))?;

        // The arms' common type, read off the first one with its bindings in
        // scope. `expr_as` re-checks every arm against it, so a wrong guess here
        // is a compile error rather than a miscompile. The place is a dummy:
        // `peek` reads types and never emits.
        let mark = self.scope.len();
        for (n, t) in self.pattern_binds(&sum, &first.pattern, line)? {
            self.scope.push((n, Place::Local(u32::MAX), t));
        }
        let want = self.peek(&first.body, line);
        self.scope.truncate(mark);
        let want = want?;
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
            // The tag test. `Option`/`Result` carry a one-byte tag; a user enum
            // carries an i64 one.
            b.ins(&Instruction::LocalGet(addr));
            match (&sum, &arm.pattern) {
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
                    let one = matches!(p, Pattern::Some(_) | Pattern::Ok(_));
                    b.ins(&Instruction::I32Load8U(byte()));
                    b.ins(&Instruction::I32Const(i32::from(one)));
                    b.ins(&Instruction::I32Eq);
                }
            }
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
        Ok(want)
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
/// RFC-0076's shim owns `malloc` and the string runtime for the split build, but
/// `vyrn build --target wasm` produces ONE module with no shim beside it, so
/// these are emitted. M4 is where the prelude moves into the shim and most of
/// this goes away; until then it is about 400 bytes in every module.
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
    trap_idx: u32,
    charcount: u32,
    utf8valid: u32,
    str_from_bytes: u32,
    slice: u32,
    f64_str: u32,
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
    count: u32,
    msg_div0: u32,
    msg_rem0: u32,
    msg_divovf: u32,
    msg_shift: u32,
    msg_aoob: u32,
    msg_soob: u32,
    msg_oob_end: u32,
}

impl Rt {
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

fn runtime(m: &mut Module, wasi: &Wasi, shim_malloc: Option<u32>) -> Rt {
    let (fd_write, proc_exit) = (wasi.fd_write, wasi.proc_exit);
    let base = m.n_imports();
    let mut rt = Rt {
        write_all: base,
        malloc: base + 1,
        strlen: base + 2,
        strcmp: base + 3,
        trap: base + 4,
        print_str: base + 5,
        print_i64: base + 6,
        int_str: base + 7,
        bool_str: base + 8,
        concat: base + 9,
        trap_idx: base + 10,
        charcount: base + 11,
        utf8valid: base + 12,
        str_from_bytes: base + 13,
        slice: base + 14,
        f64_str: base + 15,
        starts: base + 16,
        env_get: base + 17,
        str_i64: base + 18,
        now_millis: base + 19,
        mono_nanos: base + 20,
        random_seed: base + 21,
        args: base + 22,
        getbyte: base + 23,
        read_line: base + 24,
        open_at: base + 25,
        read_all: base + 26,
        err3: base + 27,
        read_file: base + 28,
        read_file_bytes: base + 29,
        write_file: base + 30,
        count: 31,
        msg_div0: 0,
        msg_rem0: 0,
        msg_divovf: 0,
        msg_shift: 0,
        msg_aoob: 0,
        msg_soob: 0,
        msg_oob_end: 0,
    };
    let nl = rt.intern(m, "\n");
    let t = rt.intern(m, "true");
    let f = rt.intern(m, "false");
    rt.msg_div0 = rt.intern(m, "error: division by zero\n");
    rt.msg_rem0 = rt.intern(m, "error: remainder by zero\n");
    rt.msg_divovf = rt.intern(m, "error: integer overflow in division\n");
    rt.msg_shift = rt.intern(m, "error: shift amount out of range\n");
    // The three spellings `{:.6}` gives a non-finite double. The native build
    // selects them on an `fcmp uno` rather than letting UCRT's `%f` say
    // `-nan(ind)`, and `f64_str` picks them for the same reason.
    let (nan, inf, ninf) = (rt.intern(m, "NaN"), rt.intern(m, "inf"), rt.intern(m, "-inf"));
    // The bounds message has the offending index in the MIDDLE, so it is three
    // pieces rather than one interned string — see `trap_idx` below.
    rt.msg_aoob = rt.intern(m, "error: array index ");
    rt.msg_soob = rt.intern(m, "error: string index ");
    rt.msg_oob_end = rt.intern(m, " out of bounds\n");

    // write_all(fd, ptr, len) — the ONE place bytes leave this module.
    //
    // A `fd_write` is allowed to write fewer bytes than it was given and say so
    // in `nwritten`; a caller that drops that number prints a prefix and calls it
    // a day. This backend found that out the direct way — two iovecs, only the
    // first of which arrived — so the retry is here rather than at three call
    // sites that would each have to remember it.
    let nw = 4;
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

    // malloc(n) — the shim's real allocator when there is a shim, and a bump
    // pointer over `HEAP` when there is not.
    //
    // A wrapper rather than a rewrite of ~20 call sites, and the shape is the
    // reason: the wrapper's signature is the one the emitted runtime already
    // calls, so the whole difference between a standalone module and a linked one
    // is these five instructions. It is also where the split proves itself — a
    // pointer that came out of the shim's dlmalloc heap, written by us, read back
    // by C, is shared linear memory or nothing.
    //
    // ponytail: the standalone bump allocator never frees. Vyrn's ownership
    // analysis knows exactly where every value dies (`Stmt::Drop` is already in
    // the AST), so a real allocator belongs here eventually; nothing observable
    // depends on it, because a free is not a thing a program can print.
    let p = 2;
    if let Some(shim_malloc) = shim_malloc {
        m.func(&[ValType::I32], &[ValType::I32], &[], 0, |b| {
            // The shim takes an `unsigned long long`, so the size widens; a Vyrn
            // allocation is bounded by an i32 address space either way.
            b.ins(&Instruction::LocalGet(0))
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::Call(shim_malloc));
        });
    } else {
    m.func(&[ValType::I32], &[ValType::I32], &[ValType::I32], 0, |b| {
        b.ins(&Instruction::GlobalGet(HEAP))
            .ins(&Instruction::LocalTee(p))
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32Const(7))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Const(-8))
            .ins(&Instruction::I32And)
            .ins(&Instruction::I32Add)
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
            .ins(&Instruction::Drop)
            .ins(&Instruction::Br(0))
            .ins(&Instruction::End)
            .ins(&Instruction::End)
            .ins(&Instruction::LocalGet(p));
    });
    }

    // strlen(s)
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

    print_i64(m, write_all);

    // int_str(v, signed) — the same digit loop as `print_i64`, into a fresh
    // 24-byte block. The digits are written backwards from the end, so the result
    // pointer is wherever they stopped.
    let (pp, neg) = (3, 4);
    let malloc = rt.malloc;
    m.func(
        &[ValType::I64, ValType::I32],
        &[ValType::I32],
        &[ValType::I32, ValType::I32],
        0,
        |b| {
            b.ins(&Instruction::I32Const(24))
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

    // trap_idx(pre, i, post) — `error: array index 7 out of bounds`, which the
    // interpreter and the native build both print with the index interpolated.
    // Three writes rather than a `printf`: varargs are M3, and this is the only
    // runtime message with a number in it.
    let int_str = rt.int_str;
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

    // charcount(s) — Unicode scalar values, i.e. bytes that are not UTF-8
    // continuation bytes (RFC-0058). The interpreter counts the same thing the
    // same way; the shim's `__vyrn_charcount` is this loop in C.
    //
    // A String is NUL-terminated and an interior NUL is rejected at construction,
    // so scanning to the zero is exact — the same argument `strlen` rests on.
    let n = 3;
    m.func(&[ValType::I32], &[ValType::I64], &[ValType::I32, ValType::I32], 0, |b| {
        b.ins(&Instruction::Block(BlockType::Empty))
            .ins(&Instruction::Loop(BlockType::Empty))
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32Load8U(byte()))
            .ins(&Instruction::LocalTee(p))
            .ins(&Instruction::I32Eqz)
            .ins(&Instruction::BrIf(1))
            // n += (b & 0xC0) != 0x80
            .ins(&Instruction::LocalGet(n))
            .ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(0xC0))
            .ins(&Instruction::I32And)
            .ins(&Instruction::I32Const(0x80))
            .ins(&Instruction::I32Ne)
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalSet(n))
            .ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalSet(0))
            .ins(&Instruction::Br(0))
            .ins(&Instruction::End)
            .ins(&Instruction::End)
            .ins(&Instruction::LocalGet(n))
            .ins(&Instruction::I64ExtendI32U);
    });

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
    m.func(
        &[ValType::I32, ValType::I32, ValType::I32],
        &[],
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        0,
        |b| {
            b.ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
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

    // slice(s, from, to) — RFC-0046's byte-range substring, with the two traps
    // that make it safe rather than merely fast.
    //
    // Both bounds are SIGNED against the byte length, and both cut points must
    // land on a leading byte: slicing a multi-byte character in half would produce
    // a `String` that is not UTF-8, which every later check assumes it cannot be.
    // `to == len` reads the terminator, which is 0 and therefore never a
    // continuation byte — the reason no special case is needed at the end.
    let oob = rt.intern(m, "error: slice index out of range\n");
    let split = rt.intern(m, "error: slice splits a UTF-8 character\n");
    let (strlen, trap) = (rt.strlen, rt.trap);
    let (slen, sub) = (5, 6);
    m.func(
        &[ValType::I32, ValType::I64, ValType::I64],
        &[ValType::I32],
        &[ValType::I32, ValType::I32, ValType::I32],
        0,
        |b| {
            b.ins(&Instruction::LocalGet(0))
                .ins(&Instruction::Call(strlen))
                .ins(&Instruction::LocalSet(slen));
            // from < 0 || to > len || from > to
            b.ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I64Const(0))
                .ins(&Instruction::I64LtS)
                .ins(&Instruction::LocalGet(2))
                .ins(&Instruction::LocalGet(slen))
                .ins(&Instruction::I64ExtendI32U)
                .ins(&Instruction::I64GtS)
                .ins(&Instruction::I32Or)
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::LocalGet(2))
                .ins(&Instruction::I64GtS)
                .ins(&Instruction::I32Or)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(oob as i32))
                .ins(&Instruction::Call(trap))
                .ins(&Instruction::End);
            let cont = |b: &mut Frame, which: u32| {
                b.ins(&Instruction::LocalGet(0))
                    .ins(&Instruction::LocalGet(which))
                    .ins(&Instruction::I32WrapI64)
                    .ins(&Instruction::I32Add)
                    .ins(&Instruction::I32Load8U(byte()))
                    .ins(&Instruction::I32Const(0xC0))
                    .ins(&Instruction::I32And)
                    .ins(&Instruction::I32Const(0x80))
                    .ins(&Instruction::I32Eq);
            };
            cont(b, 1);
            cont(b, 2);
            b.ins(&Instruction::I32Or)
                .ins(&Instruction::If(BlockType::Empty))
                .ins(&Instruction::I32Const(split as i32))
                .ins(&Instruction::Call(trap))
                .ins(&Instruction::End);
            b.ins(&Instruction::LocalGet(2))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I64Sub)
                .ins(&Instruction::I32WrapI64)
                .ins(&Instruction::LocalSet(sub))
                .ins(&Instruction::LocalGet(sub))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::LocalTee(buf))
                .ins(&Instruction::LocalGet(0))
                .ins(&Instruction::LocalGet(1))
                .ins(&Instruction::I32WrapI64)
                .ins(&Instruction::I32Add)
                .ins(&Instruction::LocalGet(sub))
                .ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 })
                .ins(&Instruction::LocalGet(buf))
                .ins(&Instruction::LocalGet(sub))
                .ins(&Instruction::I32Add)
                .ins(&Instruction::I32Const(0))
                .ins(&Instruction::I32Store8(byte()))
                .ins(&Instruction::LocalGet(buf));
        },
    );

    float_str(m, malloc, nan, inf, ninf);
    io_runtime(m, &rt, wasi);

    rt
}

/// Rights and flags from the `wasi_snapshot_preview1` witx, named rather than
/// spelled at the call: a wrong bit in `path_open` is an `ENOTCAPABLE` that reads
/// exactly like a missing file, i.e. a canonical `Err` for the wrong reason.
const RIGHT_FD_READ: i64 = 1 << 1;
const RIGHT_FD_WRITE: i64 = 1 << 6;
const OFLAGS_CREAT_TRUNC: i32 = 1 | 8;
/// `lookupflags`: follow a symlink, which is what `fopen` does.
const LOOKUP_SYMLINK_FOLLOW: i32 = 1;

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
fn io_runtime(m: &mut Module, rt: &Rt, wasi: &Wasi) {
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
                .ins(&Instruction::Call(malloc))
                .ins(&Instruction::LocalSet(ptrs))
                .ins(&Instruction::LocalGet(ptrs));
            b.slot(4)
                .ins(&Instruction::I32Load(word()))
                .ins(&Instruction::I32Const(1))
                .ins(&Instruction::I32Add)
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
            .ins(&Instruction::Call(malloc))
            .ins(&Instruction::LocalSet(ptrs))
            .ins(&Instruction::LocalGet(ptrs));
        b.slot(4);
        b.ins(&Instruction::I32Load(word()))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
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
    m.func(
        &[ValType::I32, ValType::I32],
        &[ValType::I32],
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        16,
        |b| {
            let (buf, cap, len, nb, got) = (3, 4, 5, 6, 7);
            b.ins(&Instruction::I32Const(1024))
                .ins(&Instruction::LocalTee(cap))
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
    // The open-and-slurp both readers start with, leaving the buffer in `buf`, the
    // length in `len`, and branching to `err` (a depth) with the read message set
    // when either step fails.
    let slurp = move |b: &mut Frame,
                      base_off: u32,
                      fd: u32,
                      buf: u32,
                      len: u32,
                      err_msg: u32,
                      err_depth: u32| {
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
    m.func(
        &[ValType::I32, ValType::I32],
        &[],
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        8,
        |b| {
            let (fd, buf, len, emsg, i) = (3, 4, 5, 6, 7);
            b.ins(&Instruction::Block(BlockType::Empty)) // 1: fin
                .ins(&Instruction::Block(BlockType::Empty)); // 0: err
            slurp(b, 0, fd, buf, len, emsg, 0);
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
    m.func(
        &[ValType::I32, ValType::I32],
        &[],
        &[ValType::I32, ValType::I32, ValType::I32, ValType::I32, ValType::I32],
        8,
        |b| {
            let (fd, buf, len, emsg, boxed) = (3, 4, 5, 6, 7);
            b.ins(&Instruction::Block(BlockType::Empty)) // 1: fin
                .ins(&Instruction::Block(BlockType::Empty)); // 0: err
            slurp(b, 0, fd, buf, len, emsg, 0);
            b.ins(&Instruction::I32Const(triple.size as i32))
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

/// `f64_str(x)` — a `Float64` as its fixed six-decimal spelling, exactly.
///
/// This is the one runtime function in this backend that is an algorithm rather
/// than a loop, and the reason is that `%f` has no shortcut. The interpreter
/// formats with `{:.6}` and the native build with `printf("%f")`; both convert
/// the double's **exact** decimal value and round half-to-even at the sixth
/// place, so `0.0078125` is `0.007812` (the kept `2` is even) and `1e300` is 301
/// integer digits. Anything that computes six decimals in floating point is
/// wrong somewhere, and wrong here is a different digit rather than a crash.
///
/// So: a double is `M × 2^E` with `M < 2^53`, and `x × 10^6` is an exact
/// rational whose numerator is `M × 10^6 × 2^E`. Written in base 10^6 limbs that
/// becomes one loop with two parameters:
///
/// - `E >= 0`: the numerator is `M × 10^6 × 2^E`, an integer — multiply by 2,
///   `E` times, and no digit is dropped.
/// - `E < 0`: `M × 10^6 × 5^k / 10^k` with `k = -E` — multiply by 5, `k` times,
///   and the last `k` digits are the fraction to round away.
///
/// Base 10^6 rather than 10^9 for one reason: the `× 10^6` the six places need
/// is then a shift by one whole limb, i.e. a zero limb at the bottom, so there
/// is a single multiply loop instead of a scaling pass and a power pass. Every
/// limb operation stays inside an `i32` (`999999 × 5 + 5` is small), so no part
/// of this needs 64-bit arithmetic after the mantissa is unpacked.
///
/// `k` reaches 1074 for a subnormal, which is why the digit buffer is left-padded
/// to `k + 1`: after that there is always at least one kept digit, and the
/// rounding, the carry and the point insertion have no edge case of their own.
fn float_str(m: &mut Module, malloc: u32, nan: u32, inf: u32, ninf: u32) -> u32 {
    // The limbs at frame offset 0 — the multiply loop indexes `base + 4j` and
    // says so by not adding anything — for 160 of them, i.e. 960 digits against
    // the 773 the worst case (`M × 10^6 × 5^1074`) produces. The digit buffer is
    // the rest, right-aligned at `DIG_END`, with room for max(6·limbs, k + 1)
    // plus the byte a rounding carry may claim.
    const DIG_END: u32 = 1744;

    let (bits, mant, e, reps) = (2u32, 3u32, 4u32, 5u32);
    let (n, j, limb, carry, t) = (6u32, 7u32, 8u32, 9u32, 10u32);
    let (k, st, ke, p, q) = (11u32, 12u32, 13u32, 14u32, 15u32);
    let (up, out, mul, d, neg) = (16u32, 17u32, 18u32, 19u32, 20u32);
    let locals = [
        ValType::I64,
        ValType::I64,
        ValType::I64,
        ValType::I64,
        ValType::I32,
        ValType::I32,
        ValType::I32,
        ValType::I32,
        ValType::I32,
        ValType::I32,
        ValType::I32,
        ValType::I32,
        ValType::I32,
        ValType::I32,
        ValType::I32,
        ValType::I32,
        ValType::I32,
        ValType::I32,
        ValType::I32,
    ];
    m.func(&[ValType::F64], &[ValType::I32], &locals, DIG_END, |b| {
        let bs = b.base();
        let dig_end = |b: &mut Frame| {
            b.ins(&Instruction::LocalGet(bs))
                .ins(&Instruction::I32Const(DIG_END as i32))
                .ins(&Instruction::I32Add);
        };
        // One exit, because a body must not `return` past the epilogue (M1).
        b.ins(&Instruction::Block(BlockType::Result(ValType::I32)));

        // Unpack: sign, the 11-bit exponent field (parked in `e`), the fraction.
        b.ins(&Instruction::LocalGet(0))
            .ins(&Instruction::I64ReinterpretF64)
            .ins(&Instruction::LocalTee(bits))
            .ins(&Instruction::I64Const(0))
            .ins(&Instruction::I64LtS)
            .ins(&Instruction::LocalSet(neg));
        b.ins(&Instruction::LocalGet(bits))
            .ins(&Instruction::I64Const(0x000F_FFFF_FFFF_FFFF))
            .ins(&Instruction::I64And)
            .ins(&Instruction::LocalSet(mant));
        b.ins(&Instruction::LocalGet(bits))
            .ins(&Instruction::I64Const(52))
            .ins(&Instruction::I64ShrU)
            .ins(&Instruction::I64Const(0x7FF))
            .ins(&Instruction::I64And)
            .ins(&Instruction::LocalSet(e));

        // Non-finite: interned words, because `{:.6}` prints `NaN`/`inf`/`-inf`
        // and UCRT's `%f` would say `-nan(ind)` — which is why the native build
        // selects a format string on `fcmp uno` rather than trusting printf.
        b.ins(&Instruction::LocalGet(e))
            .ins(&Instruction::I64Const(0x7FF))
            .ins(&Instruction::I64Eq)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::LocalGet(mant))
            .ins(&Instruction::I64Eqz)
            .ins(&Instruction::If(BlockType::Result(ValType::I32)))
            .ins(&Instruction::LocalGet(neg))
            .ins(&Instruction::If(BlockType::Result(ValType::I32)))
            .ins(&Instruction::I32Const(ninf as i32))
            .ins(&Instruction::Else)
            .ins(&Instruction::I32Const(inf as i32))
            .ins(&Instruction::End)
            .ins(&Instruction::Else)
            .ins(&Instruction::I32Const(nan as i32))
            .ins(&Instruction::End)
            .ins(&Instruction::Br(1))
            .ins(&Instruction::End);

        // `M × 2^E`: a subnormal has no implicit bit and a fixed exponent.
        b.ins(&Instruction::LocalGet(e))
            .ins(&Instruction::I64Eqz)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I64Const(-1074))
            .ins(&Instruction::LocalSet(e))
            .ins(&Instruction::Else)
            .ins(&Instruction::LocalGet(mant))
            .ins(&Instruction::I64Const(1i64 << 52))
            .ins(&Instruction::I64Or)
            .ins(&Instruction::LocalSet(mant))
            .ins(&Instruction::LocalGet(e))
            .ins(&Instruction::I64Const(1075))
            .ins(&Instruction::I64Sub)
            .ins(&Instruction::LocalSet(e))
            .ins(&Instruction::End);
        // Zero would otherwise multiply nothing by 5 a thousand times. It also
        // keeps its sign: `{:.6}` of `-0.0` is `-0.000000`, and so is `%f`'s.
        b.ins(&Instruction::LocalGet(mant))
            .ins(&Instruction::I64Eqz)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I64Const(0))
            .ins(&Instruction::LocalSet(e))
            .ins(&Instruction::End);

        // Which direction the exponent goes decides the multiplier and how many
        // digits end up on the far side of the point.
        b.ins(&Instruction::LocalGet(e))
            .ins(&Instruction::I64Const(0))
            .ins(&Instruction::I64LtS)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I64Const(0))
            .ins(&Instruction::LocalGet(e))
            .ins(&Instruction::I64Sub)
            .ins(&Instruction::LocalTee(reps))
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::LocalSet(k))
            .ins(&Instruction::I32Const(5))
            .ins(&Instruction::LocalSet(mul))
            .ins(&Instruction::Else)
            .ins(&Instruction::LocalGet(e))
            .ins(&Instruction::LocalSet(reps))
            .ins(&Instruction::I32Const(0))
            .ins(&Instruction::LocalSet(k))
            .ins(&Instruction::I32Const(2))
            .ins(&Instruction::LocalSet(mul))
            .ins(&Instruction::End);

        // The limbs: a zero at the bottom, which IS the `× 10^6`, then the
        // mantissa in base 10^6 (three limbs hold its sixteen digits).
        b.slot(0).ins(&Instruction::I32Const(0)).ins(&Instruction::I32Store(word()));
        b.slot(4)
            .ins(&Instruction::LocalGet(mant))
            .ins(&Instruction::I64Const(1_000_000))
            .ins(&Instruction::I64RemU)
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::I32Store(word()));
        b.slot(8)
            .ins(&Instruction::LocalGet(mant))
            .ins(&Instruction::I64Const(1_000_000))
            .ins(&Instruction::I64DivU)
            .ins(&Instruction::I64Const(1_000_000))
            .ins(&Instruction::I64RemU)
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::I32Store(word()));
        b.slot(12)
            .ins(&Instruction::LocalGet(mant))
            .ins(&Instruction::I64Const(1_000_000_000_000))
            .ins(&Instruction::I64DivU)
            .ins(&Instruction::I32WrapI64)
            .ins(&Instruction::I32Store(word()));
        b.ins(&Instruction::I32Const(4)).ins(&Instruction::LocalSet(n));

        // `reps` multiplications by `mul`. The carry out is at most 5, so one
        // extra limb always absorbs it.
        b.ins(&Instruction::Block(BlockType::Empty)).ins(&Instruction::Loop(BlockType::Empty));
        b.ins(&Instruction::LocalGet(reps)).ins(&Instruction::I64Eqz).ins(&Instruction::BrIf(1));
        b.ins(&Instruction::I32Const(0)).ins(&Instruction::LocalSet(carry));
        b.ins(&Instruction::I32Const(0)).ins(&Instruction::LocalSet(j));
        b.ins(&Instruction::Block(BlockType::Empty)).ins(&Instruction::Loop(BlockType::Empty));
        b.ins(&Instruction::LocalGet(j))
            .ins(&Instruction::LocalGet(n))
            .ins(&Instruction::I32GeU)
            .ins(&Instruction::BrIf(1));
        b.ins(&Instruction::LocalGet(bs))
            .ins(&Instruction::LocalGet(j))
            .ins(&Instruction::I32Const(4))
            .ins(&Instruction::I32Mul)
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalTee(p))
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::LocalGet(mul))
            .ins(&Instruction::I32Mul)
            .ins(&Instruction::LocalGet(carry))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalSet(t));
        b.ins(&Instruction::LocalGet(p))
            .ins(&Instruction::LocalGet(t))
            .ins(&Instruction::I32Const(1_000_000))
            .ins(&Instruction::I32RemU)
            .ins(&Instruction::I32Store(word()));
        b.ins(&Instruction::LocalGet(t))
            .ins(&Instruction::I32Const(1_000_000))
            .ins(&Instruction::I32DivU)
            .ins(&Instruction::LocalSet(carry));
        b.ins(&Instruction::LocalGet(j))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalSet(j))
            .ins(&Instruction::Br(0))
            .ins(&Instruction::End)
            .ins(&Instruction::End);
        b.ins(&Instruction::LocalGet(carry)).ins(&Instruction::If(BlockType::Empty));
        b.ins(&Instruction::LocalGet(bs))
            .ins(&Instruction::LocalGet(n))
            .ins(&Instruction::I32Const(4))
            .ins(&Instruction::I32Mul)
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalGet(carry))
            .ins(&Instruction::I32Store(word()));
        b.ins(&Instruction::LocalGet(n))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalSet(n))
            .ins(&Instruction::End);
        b.ins(&Instruction::LocalGet(reps))
            .ins(&Instruction::I64Const(1))
            .ins(&Instruction::I64Sub)
            .ins(&Instruction::LocalSet(reps))
            .ins(&Instruction::Br(0))
            .ins(&Instruction::End)
            .ins(&Instruction::End);

        // Six digits per limb, most significant limb first, right-aligned at the
        // end of the buffer — so leading zeros cost nothing and nothing is
        // stripped until the very end.
        dig_end(b);
        b.ins(&Instruction::LocalSet(p));
        b.ins(&Instruction::I32Const(0)).ins(&Instruction::LocalSet(j));
        b.ins(&Instruction::Block(BlockType::Empty)).ins(&Instruction::Loop(BlockType::Empty));
        b.ins(&Instruction::LocalGet(j))
            .ins(&Instruction::LocalGet(n))
            .ins(&Instruction::I32GeU)
            .ins(&Instruction::BrIf(1));
        b.ins(&Instruction::LocalGet(bs))
            .ins(&Instruction::LocalGet(j))
            .ins(&Instruction::I32Const(4))
            .ins(&Instruction::I32Mul)
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Load(word()))
            .ins(&Instruction::LocalSet(limb));
        b.ins(&Instruction::I32Const(6)).ins(&Instruction::LocalSet(d));
        b.ins(&Instruction::Loop(BlockType::Empty));
        b.ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::LocalTee(p))
            .ins(&Instruction::LocalGet(limb))
            .ins(&Instruction::I32Const(10))
            .ins(&Instruction::I32RemU)
            .ins(&Instruction::I32Const(b'0' as i32))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Store8(byte()));
        b.ins(&Instruction::LocalGet(limb))
            .ins(&Instruction::I32Const(10))
            .ins(&Instruction::I32DivU)
            .ins(&Instruction::LocalSet(limb));
        b.ins(&Instruction::LocalGet(d))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::LocalTee(d))
            .ins(&Instruction::BrIf(0))
            .ins(&Instruction::End);
        b.ins(&Instruction::LocalGet(j))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalSet(j))
            .ins(&Instruction::Br(0))
            .ins(&Instruction::End)
            .ins(&Instruction::End);
        b.ins(&Instruction::LocalGet(p)).ins(&Instruction::LocalSet(st));

        // Left-pad to `k + 1` digits. That is what removes every edge case from
        // what follows: there is always a kept digit to round, a digit before the
        // one being examined, and a spare byte in front for a carry.
        dig_end(b);
        b.ins(&Instruction::LocalGet(k))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::LocalSet(q));
        b.ins(&Instruction::LocalGet(st))
            .ins(&Instruction::LocalGet(q))
            .ins(&Instruction::I32GtU)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::LocalGet(q))
            .ins(&Instruction::I32Const(b'0' as i32))
            .ins(&Instruction::LocalGet(st))
            .ins(&Instruction::LocalGet(q))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::MemoryFill(0))
            .ins(&Instruction::LocalGet(q))
            .ins(&Instruction::LocalSet(st))
            .ins(&Instruction::End);
        dig_end(b);
        b.ins(&Instruction::LocalGet(k)).ins(&Instruction::I32Sub).ins(&Instruction::LocalSet(ke));

        // Round half to EVEN at `ke`, which is what both references do: greater
        // than five rounds up, less rounds down, and an exact half goes to the
        // even kept digit. `0.0078125` is the case that tells the two apart.
        b.ins(&Instruction::I32Const(0)).ins(&Instruction::LocalSet(up));
        b.ins(&Instruction::LocalGet(k)).ins(&Instruction::If(BlockType::Empty));
        b.ins(&Instruction::LocalGet(ke))
            .ins(&Instruction::I32Load8U(byte()))
            .ins(&Instruction::LocalSet(t));
        b.ins(&Instruction::LocalGet(t))
            .ins(&Instruction::I32Const(b'5' as i32))
            .ins(&Instruction::I32GtU)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::LocalSet(up))
            .ins(&Instruction::Else);
        b.ins(&Instruction::LocalGet(t))
            .ins(&Instruction::I32Const(b'5' as i32))
            .ins(&Instruction::I32Eq)
            .ins(&Instruction::If(BlockType::Empty));
        b.ins(&Instruction::LocalGet(ke))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalSet(q));
        b.ins(&Instruction::Block(BlockType::Empty)).ins(&Instruction::Loop(BlockType::Empty));
        b.ins(&Instruction::LocalGet(q));
        dig_end(b);
        b.ins(&Instruction::I32GeU).ins(&Instruction::BrIf(1));
        b.ins(&Instruction::LocalGet(q))
            .ins(&Instruction::I32Load8U(byte()))
            .ins(&Instruction::I32Const(b'0' as i32))
            .ins(&Instruction::I32Ne)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::LocalSet(up))
            .ins(&Instruction::Br(2))
            .ins(&Instruction::End);
        b.ins(&Instruction::LocalGet(q))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalSet(q))
            .ins(&Instruction::Br(0))
            .ins(&Instruction::End)
            .ins(&Instruction::End);
        b.ins(&Instruction::LocalGet(up))
            .ins(&Instruction::I32Eqz)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::LocalGet(ke))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::I32Load8U(byte()))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32And)
            .ins(&Instruction::LocalSet(up))
            .ins(&Instruction::End);
        b.ins(&Instruction::End).ins(&Instruction::End).ins(&Instruction::End);

        // Carry it in, digit by digit, growing the string by one if it escapes.
        b.ins(&Instruction::LocalGet(up)).ins(&Instruction::If(BlockType::Empty));
        b.ins(&Instruction::LocalGet(ke))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::LocalSet(q));
        b.ins(&Instruction::Block(BlockType::Empty)).ins(&Instruction::Loop(BlockType::Empty));
        b.ins(&Instruction::LocalGet(q))
            .ins(&Instruction::LocalGet(st))
            .ins(&Instruction::I32LtU)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::LocalGet(st))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::LocalTee(st))
            .ins(&Instruction::I32Const(b'1' as i32))
            .ins(&Instruction::I32Store8(byte()))
            .ins(&Instruction::Br(2))
            .ins(&Instruction::End);
        b.ins(&Instruction::LocalGet(q))
            .ins(&Instruction::I32Load8U(byte()))
            .ins(&Instruction::LocalTee(t))
            .ins(&Instruction::I32Const(b'9' as i32))
            .ins(&Instruction::I32Ne)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::LocalGet(q))
            .ins(&Instruction::LocalGet(t))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Store8(byte()))
            .ins(&Instruction::Br(2))
            .ins(&Instruction::End);
        b.ins(&Instruction::LocalGet(q))
            .ins(&Instruction::I32Const(b'0' as i32))
            .ins(&Instruction::I32Store8(byte()));
        b.ins(&Instruction::LocalGet(q))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::LocalSet(q))
            .ins(&Instruction::Br(0))
            .ins(&Instruction::End)
            .ins(&Instruction::End)
            .ins(&Instruction::End);

        // Seven digits minimum, so the point always has an integer digit in front
        // of it, and then the integer part's leading zeros go.
        b.ins(&Instruction::LocalGet(ke))
            .ins(&Instruction::LocalGet(st))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::I32Const(7))
            .ins(&Instruction::I32LtS)
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::LocalGet(ke))
            .ins(&Instruction::I32Const(7))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::LocalTee(q))
            .ins(&Instruction::I32Const(b'0' as i32))
            .ins(&Instruction::LocalGet(st))
            .ins(&Instruction::LocalGet(q))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::MemoryFill(0))
            .ins(&Instruction::LocalGet(q))
            .ins(&Instruction::LocalSet(st))
            .ins(&Instruction::End);
        b.ins(&Instruction::Block(BlockType::Empty)).ins(&Instruction::Loop(BlockType::Empty));
        b.ins(&Instruction::LocalGet(st))
            .ins(&Instruction::LocalGet(ke))
            .ins(&Instruction::I32Const(7))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::I32GeU)
            .ins(&Instruction::BrIf(1));
        b.ins(&Instruction::LocalGet(st))
            .ins(&Instruction::I32Load8U(byte()))
            .ins(&Instruction::I32Const(b'0' as i32))
            .ins(&Instruction::I32Ne)
            .ins(&Instruction::BrIf(1));
        b.ins(&Instruction::LocalGet(st))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalSet(st))
            .ins(&Instruction::Br(0))
            .ins(&Instruction::End)
            .ins(&Instruction::End);

        // The string: an optional sign, the integer digits, the point, six more.
        b.ins(&Instruction::LocalGet(ke))
            .ins(&Instruction::I32Const(6))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::LocalGet(st))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::LocalSet(t));
        b.ins(&Instruction::LocalGet(t))
            .ins(&Instruction::I32Const(8))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalGet(neg))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::Call(malloc))
            .ins(&Instruction::LocalTee(out))
            .ins(&Instruction::LocalSet(p));
        b.ins(&Instruction::LocalGet(neg))
            .ins(&Instruction::If(BlockType::Empty))
            .ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(b'-' as i32))
            .ins(&Instruction::I32Store8(byte()))
            .ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalSet(p))
            .ins(&Instruction::End);
        b.ins(&Instruction::LocalGet(p))
            .ins(&Instruction::LocalGet(st))
            .ins(&Instruction::LocalGet(t))
            .ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
        b.ins(&Instruction::LocalGet(p))
            .ins(&Instruction::LocalGet(t))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalTee(p))
            .ins(&Instruction::I32Const(b'.' as i32))
            .ins(&Instruction::I32Store8(byte()));
        b.ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(1))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::LocalTee(p))
            .ins(&Instruction::LocalGet(ke))
            .ins(&Instruction::I32Const(6))
            .ins(&Instruction::I32Sub)
            .ins(&Instruction::I32Const(6))
            .ins(&Instruction::MemoryCopy { src_mem: 0, dst_mem: 0 });
        b.ins(&Instruction::LocalGet(p))
            .ins(&Instruction::I32Const(6))
            .ins(&Instruction::I32Add)
            .ins(&Instruction::I32Const(0))
            .ins(&Instruction::I32Store8(byte()));
        b.ins(&Instruction::LocalGet(out));
        b.ins(&Instruction::End);
    })
}

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

fn stmt_name(s: &Stmt) -> String {
    match s {
        Stmt::Let { .. } => "`let`",
        Stmt::Assign { .. } => "an assignment",
        Stmt::SetField { .. } => "a field assignment",
        Stmt::IndexSet { .. } => "an element assignment",
        Stmt::Return { .. } => "`return`",
        Stmt::Break { .. } => "`break`",
        Stmt::Continue { .. } => "`continue`",
        Stmt::If { .. } => "`if`",
        Stmt::IfLet { .. } => "`if let`",
        Stmt::While { .. } => "`while`",
        Stmt::ForIn { .. } => "`for`",
        Stmt::Drop { .. } => "`drop`",
        Stmt::Expr(_) => "an expression statement",
        Stmt::Region { .. } => "`region`",
    }
    .to_string()
}

fn stmt_line(s: &Stmt) -> usize {
    match s {
        Stmt::Let { line, .. }
        | Stmt::Assign { line, .. }
        | Stmt::SetField { line, .. }
        | Stmt::IndexSet { line, .. }
        | Stmt::Return { line, .. }
        | Stmt::Break { line }
        | Stmt::Continue { line }
        | Stmt::If { line, .. }
        | Stmt::IfLet { line, .. }
        | Stmt::While { line, .. }
        | Stmt::ForIn { line, .. }
        | Stmt::Drop { line, .. }
        | Stmt::Region { line, .. } => *line,
        Stmt::Expr(e) => Expr::line(e),
    }
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

    fn cx() -> Cx {
        Cx {
            types: HashMap::new(),
            sigs: HashMap::new(),
            variants: HashMap::new(),
            generics: HashMap::new(),
            higher_order: HashMap::new(),
            protocol_methods: HashMap::new(),
            subst: HashMap::new(),
            mono: RefCell::new(Mono::default()),
            globals: HashMap::new(),
            externs: HashMap::new(),
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
