//! Abstract syntax tree for the Vyrn v0 subset.

/// `panic(msg)` with its source site attached: `@panicAt(msg, "file:line")`
/// (RFC-0079, census U5). The loader rewrites every `panic` into this form as
/// it enters the module, because that is the one pass that knows both halves —
/// the parser knows the line and not the file, and every stage after the loader
/// knows neither once a body has been cloned.
///
/// The site travels as an ordinary string literal in the argument list, so all
/// three engines read it the same way and a projection inlined into another
/// module carries its own site with it. A second **name** rather than a second
/// **argument** to `panic`: `@` does not lex, so no source can write this call
/// and no user gains an undocumented two-argument `panic`. Same reason
/// [`crate::project::ELEM`] is spelled `@slot`.
pub const PANIC_AT: &str = "@panicAt";

/// Is this call name a `panic` — written by the user, or stamped with its site?
///
/// Every pass that asks "does this statement diverge" asks through here, so a
/// stamped `panic` diverges exactly as an unstamped one does. Both spellings
/// stay live: the single-file `analyze` path the LSP uses never runs the loader,
/// and it must still type-check and still diverge.
pub fn is_panic(name: &str) -> bool {
    name == "panic" || name == PANIC_AT
}

/// A whole program: top-level type declarations plus functions. `main` is the
/// entry point.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    /// `import { a, b } from "path"` declarations (RFC-0010 modules). Consumed
    /// by the loader, which links every imported module into this one Program;
    /// downstream stages (checker/interp/codegen) never see them.
    pub imports: Vec<ImportDecl>,
    pub type_decls: Vec<TypeDecl>,
    pub functions: Vec<Function>,
    /// Protocol declarations (RFC-0002 §5 / traits): a named set of method
    /// signatures a type can implement and a generic can be bounded by.
    pub protocols: Vec<ProtocolDecl>,
    /// Module contract declarations (RFC-0071): a named set of exports a module
    /// may have. `contract` is to a module what `protocol` is to a type.
    /// Comptime-only — nothing about a contract reaches the emitted program
    /// except through the `contractOf(Name)` reflection literal.
    pub contracts: Vec<ContractDecl>,
    /// `impl P for T { .. }` blocks — a type's methods for a protocol.
    pub impls: Vec<ImplBlock>,
    /// Top-level `let [mut] name [: Type] = initializer` module-state bindings
    /// (RFC-0013). Root-module-only (the loader rejects them in imported
    /// modules); initialized once, in declaration order, before `main`. Every
    /// function sees them as an outermost scope frame below its parameters.
    pub globals: Vec<GlobalDecl>,
    /// Where each of RFC-0054's surface builtins is shadowed: `(module, name)`
    /// for every module that can SEE a declaration of `render`/`rawAt`/`raw`/
    /// `lex` — its own, or one it imported.
    ///
    /// The loader fills this and the checker reads it, because the question
    /// cannot be answered on either side alone. The checker never sees
    /// `imports` (they are consumed above), and the loader does not resolve
    /// calls. Asking the linked program instead — which is what both engines
    /// used to do — makes one module's `fn raw` disable the builtin for every
    /// module, `std/vyx` included; `examples/shadowbuiltin.vyrn` is that defect.
    ///
    /// `None` in the module position is the root module, spelled as the loader
    /// spells it on [`Function::module`].
    pub surface_shadows: std::collections::HashSet<(Option<String>, String)>,
    /// The logging threshold ordinal (RFC-0008), set by a `logging { level: X }`
    /// block. A log call below it is dropped at compile time. Defaults to
    /// [`DEFAULT_LOG_LEVEL`] (Info) when there is no config block.
    pub log_level: usize,
    /// Where log records go (RFC-0008), set by `logging { sink: .. }`. Defaults
    /// to [`LogSink::Stderr`].
    pub log_sink: LogSink,
    /// `test "name" { body }` declarations (RFC-0015). A separate field so the
    /// run/build/emit-ir paths (which only walk `functions`) never see them: a
    /// shipped binary contains no tests, and the string pool / regex collection
    /// skip them by construction. Checked as Unit-returning function bodies;
    /// executed only by `vyrn test`.
    pub tests: Vec<TestDecl>,
    /// `bench "name" { body }` declarations (RFC-0055). A separate field, exactly
    /// like [`Program::tests`]: `run`/`build`/`emit-ir` walk only `functions`, so a
    /// shipped binary contains no benches and the string pool / regex collection
    /// skip them by construction. Checked as Unit-returning function bodies
    /// (`blackBox` legal inside); executed only by `vyrn bench` (which lowers them
    /// to ordinary functions + a synthesized harness `main` before the backends).
    pub benches: Vec<BenchDecl>,
}

/// A `bench "name" { body }` declaration (RFC-0055): a named block checked exactly
/// like a Unit-returning function body under a synthetic name (`bench@<index>`) so
/// movecheck/ownership/spawn analyses apply unchanged. Structurally identical to
/// [`TestDecl`]; `vyrn bench` runs only the *root* module's (`None`-module) benches.
#[derive(Debug, Clone, PartialEq)]
pub struct BenchDecl {
    /// The bench's display name (the string literal after `bench`).
    pub name: String,
    /// The block body — timed under `vyrn bench`, run once under `--check`.
    pub body: Block,
    /// `///` documentation (markdown), attached by the parser; `None` if absent.
    pub doc: Option<String>,
    /// The module (file) this bench came from; `None` for the root. Set by the
    /// loader. `vyrn bench` runs only `None`-module (root) benches.
    pub module: Option<String>,
    pub line: usize,
}

/// A `test "name" { body }` declaration (RFC-0015): a named block checked exactly
/// like a Unit-returning function body and run by `vyrn test`. The `name` is a
/// plain string (unique per file). Only the *root* module's tests are run by
/// `vyrn test <root>`; an imported module's tests still type-check but do not
/// run (they run when that module is itself the argument).
#[derive(Debug, Clone, PartialEq)]
pub struct TestDecl {
    /// The test's display name (the string literal after `test`).
    pub name: String,
    /// The block body — checked/analysed under a synthetic unspellable function
    /// name (`test@<index>`) so movecheck/ownership/spawn analyses apply unchanged.
    pub body: Block,
    /// `///` documentation (markdown), attached by the parser; `None` if absent.
    pub doc: Option<String>,
    /// The module (file) this test came from; `None` for the root. Set by the
    /// loader. `vyrn test` runs only `None`-module (root) tests.
    pub module: Option<String>,
    pub line: usize,
}

/// A logging destination (RFC-0008). One sink in this phase; fan-out is future.
#[derive(Debug, Clone, PartialEq)]
pub enum LogSink {
    /// Standard error (the default) — keeps logs off the program's stdout.
    Stderr,
    /// Standard output.
    Stdout,
    /// A file, truncated and opened for writing at program start.
    File(String),
}

/// Is this binding a place desugar's move-out temp (RFC-0082)?
///
/// `t.xs[k] = v` becomes `let mut t.xs[] = t.xs` / `t.xs[][k] = v` /
/// `t.xs = t.xs[]`. The name is unspellable — `[` cannot appear in an identifier
/// — and by convention ONLY the container that is moved out and written back
/// ends in `[]`; the operand temps the desugar hoists ahead of the move carry a
/// further suffix (`[]i`, `[]v`) so they do not match. The interpreter keys its
/// take on this (`Interp::take_place`); `symbols.rs` filters all of them out of
/// the completion index on the looser `contains('[')`.
pub fn is_place_temp(name: &str) -> bool {
    name.ends_with("[]")
}

/// The default logging threshold — `Info`: `trace`/`debug` are suppressed unless
/// a `logging { level: .. }` block lowers it.
pub const DEFAULT_LOG_LEVEL: usize = 2;

/// The four SURFACE builtins (RFC-0054): the ones spelled as ordinary
/// identifiers rather than with an unspellable `@` prefix.
///
/// They are deliberately NOT reserved — they are common words, and a program
/// that wants a function called `render` should have one. A module that
/// declares one means its own; a module that does not means the builtin.
///
/// WHOSE DECLARATION COUNTS IS THE WHOLE SUBTLETY. Both engines used to ask
/// whether the LINKED PROGRAM held a function of the name, which is a different
/// question with a much wider answer: a two-argument `rawAt` in one module took
/// the four-argument builtin away from `std/vyx`, in a module that neither
/// imports nor knows about it. The question is now asked of the calling
/// module's own scope, which is what the RFC's wording always described.
pub const SURFACE_BUILTINS: [&str; 4] = ["render", "rawAt", "raw", "lex"];

/// Whether `name` is one of the four surface builtins.
pub fn is_surface_builtin(name: &str) -> bool {
    SURFACE_BUILTINS.contains(&name)
}

/// The five log levels (RFC-0008), in order, lowest first.
///
/// ONE TABLE. These five names were written out in eleven places across three
/// crates — the ordinal map below, the checker's effect lists, the interpreter's
/// dispatch, both code generators, and the editor's index. A sixth level added
/// to some of them and not others is a level that logs but does not count as an
/// effect, so `spawn` would let it cross a task boundary.
///
/// The ORDER is the meaning: the index is the ordinal a `logging { level: .. }`
/// block compares against, so this is a list and not a set.
///
/// THREE SITES DELIBERATELY STILL SPELL THE FIVE OUT, and they are not
/// duplication:
///
/// - `interp.rs`'s dispatch arm and `vyrn-codegen/src/direct.rs`'s two arms are
///   READ AS DATA by `vyrn-frontend/tests/primitives.rs`, which scans both files
///   for literals to enumerate what each engine implements and compares that to
///   RFC-0078's census. A predicate is invisible to a text scan.
/// - `checker.rs`'s `RESERVED`, `SPAWN_FORBIDDEN` and `COMPTIME_FORBIDDEN` hold
///   the five among dozens of unrelated names, where splicing a const array in
///   costs more than it saves. `every_log_level_is_reserved_and_forbidden_where_effects_are`
///   compares them to this table instead.
pub const LOG_LEVELS: [&str; 5] = ["trace", "debug", "info", "warn", "error"];

/// Whether `name` is one of the five log levels.
pub fn is_log_level(name: &str) -> bool {
    LOG_LEVELS.contains(&name)
}

/// The ordinal of a log-level name (RFC-0008), `trace` lowest → `error` highest.
/// Shared by the config-block parser, the interpreter, and the codegen so they
/// filter identically. Returns `None` for an unknown name.
pub fn log_level_ordinal(name: &str) -> Option<usize> {
    LOG_LEVELS.iter().position(|l| *l == name)
}

/// A top-level module-state binding (RFC-0013): `let [mut] name [: Type] = init`.
/// The initializer is required. Unlike a `let` statement it lives for the whole
/// module lifetime, is shared by every function, and is never dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalDecl {
    pub name: String,
    /// `let mut ..` — reassignable via `name = value` in any function body.
    pub mutable: bool,
    /// An explicit type annotation, or `None` to infer from the initializer.
    pub ty: Option<Type>,
    /// The initializer expression (required). Restricted by the checker: no user
    /// or extern calls, and no read of a global declared later.
    pub init: Expr,
    /// `///` documentation (markdown), attached by the parser; `None` if absent.
    pub doc: Option<String>,
    /// The module (file) this decl came from; `None` for the root. Set by the
    /// loader — though globals are root-only, this keeps diagnostics uniform.
    pub module: Option<String>,
    pub line: usize,
}

/// A named type declaration. Two shapes exist in v0.1:
/// - a validated (refinement) scalar, e.g. `type Age = Int where value >= 18;`
///   (RFC-0003) — `base` is `Int`/`Bool` with an optional `predicate`;
/// - a structural record, e.g. `type User = { name: Int, age: Int };`
///   (RFC-0002) — `base` is a [`Type::Record`] and `predicate` is `None`.
/// One imported binding: `original` as exported by the source module, bound
/// locally under `alias` when written `original as alias` (RFC-0022). A bare
/// `import { User }` has `alias: None` — the local name equals `original`.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportName {
    /// The name as exported by the source module.
    pub original: String,
    /// The local name it is bound to here, when different (`... as alias`).
    pub alias: Option<String>,
}

impl ImportName {
    /// A bare (unaliased) import of `name`.
    pub fn bare(name: impl Into<String>) -> Self {
        ImportName {
            original: name.into(),
            alias: None,
        }
    }
    /// The name this binding is known by in the importing module — the alias if
    /// present, else the original. This is what visibility, collision, and
    /// movecheck all key on.
    pub fn local(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.original)
    }
}

/// One `import { names } from "path"` declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportDecl {
    /// The bindings brought into scope, each an `original`/`alias` pair
    /// (RFC-0022). `import type { .. }` (JSON Schema imports) also lands here;
    /// the loader dispatches on the path's extension. Empty for a namespace
    /// import (`import * as ns from ..`, RFC-0027), which binds no flat names.
    pub names: Vec<ImportName>,
    /// `import * as ns from <source>` (RFC-0027): the ONE namespace name `ns`
    /// bound in this module. `None` for an ordinary selective/aliased import.
    /// None of the target's exports enter the flat namespace — the loader
    /// reinterprets each `ns.member` use into a qualified reference and folds it
    /// to the foreign decl's program-wide symbol, so everything downstream stays
    /// namespace-unaware.
    pub namespace: Option<String>,
    /// Where the names come from: an ordinary module specifier, or a compile-time
    /// generator call (RFC-0021).
    pub source: ImportSource,
    pub line: usize,
}

/// The right-hand side of an `import { .. } from <source>` (RFC-0010 / RFC-0021).
#[derive(Debug, Clone, PartialEq)]
pub enum ImportSource {
    /// `from "path"` — a module specifier as written: relative (`./lib`),
    /// `std/name`, a manifest alias, or a remote specifier.
    Path(String),
    /// `from gen(args...)` — a generator-call import target (RFC-0021). `name`
    /// is the `gen fn` to invoke (an imported or locally-declared generator);
    /// `args` are its arguments, which must be consteval-provable constants. The
    /// loader runs the call in the compiler's interpreter and links the returned
    /// `String` as a synthesized module.
    Generator {
        name: String,
        args: Vec<Expr>,
        line: usize,
    },
}

#[derive(Debug, Clone, PartialEq)]
pub struct TypeDecl {
    pub name: String,
    /// `export type ..` — importable from other modules (RFC-0010).
    pub exported: bool,
    /// The module (file) this decl came from; `None` for the root / single-file
    /// programs. Set by the loader; used to attribute diagnostics to files.
    pub module: Option<String>,
    /// `///` documentation (markdown), attached by the parser; `None` if absent.
    pub doc: Option<String>,
    /// Generic parameters, e.g. `type Box<T> = { value: T }`; empty otherwise.
    pub type_params: Vec<String>,
    /// The underlying representation type.
    pub base: Type,
    /// Optional refinement predicate over the special variable `value`.
    pub predicate: Option<Expr>,
    pub line: usize,
}

/// A field of a structural record type.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Field {
    pub name: String,
    pub ty: Type,
}

/// A variant of a user-defined enum (sum type), e.g. `Circle(Int)`,
/// `Rect(Int, Int)`, or `Empty`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnumVariant {
    pub name: String,
    /// Payload types (empty for a nullary variant).
    pub payload: Vec<Type>,
}

/// A protocol declaration (RFC-0002 §5): a named set of method signatures.
/// A type provides them via `impl P for T`; a generic bounded `<X: P>` may call
/// them. The receiver is written `self` and is elided from `params` here.
#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolDecl {
    pub name: String,
    /// `export protocol ..` — importable from other modules (RFC-0010).
    pub exported: bool,
    /// Source module for diagnostics; `None` for the root. Set by the loader.
    pub module: Option<String>,
    /// `///` documentation (markdown), attached by the parser; `None` if absent.
    pub doc: Option<String>,
    /// `type Output` — the associated types the protocol declares (RFC-0080 M2),
    /// in declaration order. A method signature may name one; it arrives as
    /// [`Type::Param`], because that is exactly what it is — a type variable the
    /// *implementing type* binds rather than the caller.
    pub assoc: Vec<String>,
    pub methods: Vec<MethodSig>,
    pub line: usize,
}

/// One method signature inside a [`ProtocolDecl`]: `fn name(self, p: T, ..) -> R`.
/// `params` are the parameters *after* the `self` receiver.
#[derive(Debug, Clone, PartialEq)]
pub struct MethodSig {
    pub name: String,
    /// `///` documentation (markdown), attached by the parser; `None` if absent.
    /// Retained for the same reason [`ContractMember::doc`] is: `vyrn doc` and
    /// LSP hover are the only readers a signature-only declaration has.
    pub doc: Option<String>,
    /// The receiver's capability: `read` for a bare `self`, or whatever
    /// `modify self` / `consume self` wrote. Part of the signature, so an impl
    /// must match it — a caller reads this and nothing else when the receiver
    /// is a bounded type parameter.
    pub recv: Capability,
    pub params: Vec<Type>,
    /// Each parameter's capability, parallel to `params`. Carried for the same
    /// reason `recv` is: the call site sees the SURFACE name (`s.insert(v)`),
    /// so `movecheck` reads the discipline here rather than off the impl it
    /// cannot select.
    pub param_caps: Vec<Capability>,
    pub ret: Type,
    pub line: usize,
}

/// A module contract declaration (RFC-0071): the exports a module may have,
/// with their types, optionality, and documentation.
///
/// `contract Page { let head: Head = Head { } … }` is to a *module* what
/// [`ProtocolDecl`] is to a *type*, and the implementation deliberately mirrors
/// it: a named, exportable, importable declaration carrying member signatures
/// and nothing else. Contracts are comptime-only — the checker validates their
/// member types, `contractOf(Page)` reflects one into a `ContractInfo` record,
/// and `std/contract:checkContract` does the actual comparing in ordinary Vyrn
/// code. The compiler knows nothing about any particular contract.
#[derive(Debug, Clone, PartialEq)]
pub struct ContractDecl {
    pub name: String,
    /// `export contract ..` — importable from other modules (RFC-0010).
    pub exported: bool,
    /// Source module for diagnostics; `None` for the root. Set by the loader.
    pub module: Option<String>,
    /// `///` documentation (markdown), attached by the parser; `None` if absent.
    pub doc: Option<String>,
    pub members: Vec<ContractMember>,
    pub line: usize,
}

impl ContractDecl {
    /// The open rule (`fn *(..) -> ..`), if this contract has one. A contract
    /// without one is **closed**: an export it does not name is a diagnostic.
    pub fn open_rule(&self) -> Option<&ContractMember> {
        self.members.iter().find(|m| m.is_open_rule())
    }
}

/// One member of a [`ContractDecl`].
///
/// Member type parameters are open **per member** (RFC-0071): `let data:
/// Query<T>` admits any instantiation of `Query`. See
/// [`ContractMember::type_params`] for how they are recognized.
#[derive(Debug, Clone, PartialEq)]
pub struct ContractMember {
    /// The export's name, or [`OPEN_RULE_NAME`] (`"*"`) for the open rule.
    pub name: String,
    /// `///` documentation (markdown). Retained because the LSP surfaces it on
    /// completion and hover (RFC-0071 M4).
    pub doc: Option<String>,
    pub kind: ContractMemberKind,
    pub line: usize,
}

/// The member name that marks a contract's open rule (`fn *(..) -> ..`).
pub const OPEN_RULE_NAME: &str = "*";

impl ContractMember {
    /// Whether this member is the open rule rather than a named export.
    pub fn is_open_rule(&self) -> bool {
        self.name == OPEN_RULE_NAME
    }
    /// Whether the module may omit this export (it has a default).
    ///
    /// Both member forms carry one (RFC-0071 M2): a value member defaults to a
    /// value, a function member to the value its call would have produced
    /// (`fn head() -> Head = noHead()` reads "absent means `noHead()`"), which is
    /// what a generator substitutes when the module says nothing.
    pub fn default(&self) -> Option<&Expr> {
        match &self.kind {
            ContractMemberKind::Value { default, .. } | ContractMemberKind::Fn { default, .. } => {
                default.as_deref()
            }
        }
    }
    /// Whether the module may omit this export (it has a default).
    pub fn optional(&self) -> bool {
        self.default().is_some()
    }
    /// The member's type spelling, as `checkContract` compares it: a value
    /// member spells its type, a function member spells `fn(A, B) -> R`.
    pub fn spelling(&self) -> String {
        match &self.kind {
            ContractMemberKind::Value { ty, .. } => ty.to_string(),
            // Reuse the ordinary `Type::Fn` spelling (`fn(A, B) -> R`, with a
            // `Unit` return omitted) so a contract member and a stored function
            // value are described in exactly the same words.
            ContractMemberKind::Fn {
                params,
                ret,
                variadic: true,
                ..
            } => {
                let _ = params;
                format!("fn(..){}", ret_suffix(ret))
            }
            ContractMemberKind::Fn { params, ret, .. } => {
                Type::Fn(params.clone(), Box::new(ret.clone())).to_string()
            }
        }
    }
    /// The type parameters this member binds. RFC-0071 makes them implicit and
    /// per-member; the parser marks them at parse time (see
    /// [`crate::parser`]'s contract-member handling) so they arrive here already
    /// as [`Type::Param`]. This collects them for diagnostics and reflection.
    pub fn type_params(&self) -> Vec<String> {
        let mut out = Vec::new();
        let mut push = |t: &Type| collect_params(t, &mut out);
        match &self.kind {
            ContractMemberKind::Value { ty, .. } => push(ty),
            ContractMemberKind::Fn { params, ret, .. } => {
                for p in params {
                    push(p);
                }
                push(ret);
            }
        }
        out.sort();
        out.dedup();
        out
    }
}

/// Collect every [`Type::Param`] name appearing anywhere in `ty`.
fn collect_params(ty: &Type, out: &mut Vec<String>) {
    match ty {
        Type::Param(n) => out.push(n.clone()),
        Type::App(_, args) => {
            for a in args {
                collect_params(a, out);
            }
        }
        Type::Option(a)
        | Type::Array(a)
        | Type::Task(a)
        | Type::Stream(a)
        | Type::Partial(a)
        | Type::ArrayN(a, _)
        | Type::SmallArray(a, _)
        | Type::Omit(a, _)
        | Type::Pick(a, _) => collect_params(a, out),
        Type::Result(a, b) | Type::Merge(a, b) | Type::Map(a, b) => {
            collect_params(a, out);
            collect_params(b, out);
        }
        Type::Record(fields) => {
            for f in fields {
                collect_params(&f.ty, out);
            }
        }
        Type::Enum(variants) => {
            for v in variants {
                for p in &v.payload {
                    collect_params(p, out);
                }
            }
        }
        Type::Fn(params, ret) => {
            for p in params {
                collect_params(p, out);
            }
            collect_params(ret, out);
        }
        _ => {}
    }
}

/// ` -> R` for a member's return type, or "" for `Unit` — the same elision
/// `Type::Fn`'s own spelling uses, so both forms read alike in a diagnostic.
fn ret_suffix(ret: &Type) -> String {
    if *ret == Type::Unit {
        String::new()
    } else {
        format!(" -> {ret}")
    }
}

/// What a [`ContractMember`] declares.
#[derive(Debug, Clone, PartialEq)]
pub enum ContractMemberKind {
    /// `let name: Type [= default]` — a value export. A `default` makes the
    /// member **optional**: the module may omit it and the generator uses the
    /// default instead.
    Value {
        ty: Type,
        default: Option<Box<Expr>>,
    },
    /// `fn name(params) -> Ret [= default]` — a function export. Parameter
    /// *names* are not part of the contract (only arity and types are), exactly
    /// as in [`MethodSig`].
    ///
    /// The `default` is an expression of the member's RETURN type, not of its
    /// function type: `fn head() -> Head = noHead()` reads "a module that does
    /// not export `head` has `noHead()` for its head", which is the question a
    /// generator actually asks. It makes the member **optional**.
    Fn {
        params: Vec<Type>,
        ret: Type,
        default: Option<Box<Expr>>,
        /// `fn *(..) -> R` — the parameter list is `..`, so the member
        /// constrains the RETURN type only and admits any arity. Legal on the
        /// open rule alone: a named member's arity is part of what its name
        /// promises, while an open rule describes a family of exports whose
        /// shapes genuinely differ (a components module's views take 0, 1 or 4
        /// props, and enumerating that would say nothing).
        variadic: bool,
    },
}

/// `impl P for T { fn m(self, ..) { .. } }` — the methods a type provides for a
/// protocol. Each method is an ordinary [`Function`] whose first parameter is the
/// `self` receiver (typed to `ty`).
#[derive(Debug, Clone, PartialEq)]
pub struct ImplBlock {
    pub protocol: String,
    /// `impl<T> P for C<T>` — the type variables the head binds (RFC-0080 M1).
    /// Empty for a concrete impl. Each method inherits them, so the flattened
    /// impl function is an ordinary generic function and the existing
    /// monomorphization path specializes it per receiver instantiation.
    pub type_params: Vec<String>,
    /// Bounds per bound variable, e.g. `impl<T: Show> Show for Option<T>`.
    pub type_bounds: std::collections::HashMap<String, Vec<String>>,
    pub ty: Type,
    /// The associated types this impl binds (RFC-0080 M2), in declaration order.
    /// The NAMES only: the parser has already substituted each binding into the
    /// methods below, so the sole remaining question is whether this set matches
    /// the protocol's declarations. Keeping the bound [`Type`] here as well would
    /// be a second copy that no loader walk rewrites — inert today and wrong the
    /// first time someone believed it.
    pub assoc: Vec<String>,
    pub methods: Vec<Function>,
    /// `place at(read self, i: Int64) -> T { yield self.data[i] }` — the place
    /// projections this impl declares (RFC-0091 M2). A projection is NOT a
    /// function: it is never called, never flattened into
    /// [`Program::functions`], and never emitted. Every access site inlines its
    /// body, so the borrow it yields cannot outlive the access — rule 2 of
    /// RFC-0089 holds by construction rather than by a check.
    ///
    /// The body is an ordinary [`Block`] whose last statement is a
    /// [`Stmt::Return`]: that node IS the `yield`. Keeping the `yield` out of
    /// the `Stmt` enum keeps ten exhaustive matches across the frontend and the
    /// three backends unchanged, and the two forms never mix — the parser
    /// accepts `yield` only inside a projection and `return` only outside one.
    pub places: Vec<Function>,
    pub line: usize,
    /// 1-based column of the `impl` keyword, in Unicode scalar values — `0` when
    /// the block was synthesized rather than parsed. A diagnostic about the
    /// impl HEAD (a missing method, a clashing head, an unbound associated type)
    /// points here; see [`crate::diagnostics`] for the column convention.
    pub col: usize,
}

impl ImplBlock {
    /// The `impl` keyword's column span, for a diagnostic about the impl HEAD
    /// (`(0, 0)` — "whole line" — for a synthesized block with no column).
    pub fn head_span(&self) -> (usize, usize) {
        match self.col {
            0 => (0, 0),
            c => (c, c + "impl".len()),
        }
    }
}

/// A function definition. `type_params` holds any generic parameters
/// (`fn id<T>(...)`); empty for ordinary functions.
#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: String,
    /// `export fn ..` — importable from other modules (RFC-0010).
    pub exported: bool,
    /// Source module for diagnostics; `None` for the root. Set by the loader.
    pub module: Option<String>,
    /// `///` documentation (markdown), attached by the parser; `None` if absent.
    pub doc: Option<String>,
    pub type_params: Vec<String>,
    /// Built-in bounds per type parameter, e.g. `<T: Ord>` → `{ "T": ["Ord"] }`.
    /// A bound (`Eq`/`Ord`/`Num`) unlocks the matching operators on `T`.
    pub type_bounds: std::collections::HashMap<String, Vec<String>>,
    pub params: Vec<Param>,
    pub ret: Type,
    pub body: Block,
    pub line: usize,
    /// 1-based column of the function's NAME, in Unicode scalar values — `0`
    /// when the function was synthesized rather than parsed. A diagnostic about
    /// the declaration (a reserved or duplicated name, a signature that does not
    /// match its protocol, a malformed projection) points here; see
    /// [`crate::diagnostics`] for the column convention.
    pub col: usize,
    /// `extern fn ..` — a JS-interop import (RFC-0012). A body-less declaration
    /// whose implementation the wasm host supplies from the `vyrn` import
    /// namespace; on native/interpreter a *call* traps (declaring is fine). The
    /// `body` is empty for an extern declaration (M1); `export extern fn` with a
    /// body is M2 (exports). Distinguishes externs everywhere functions are
    /// iterated (codegen emits a `declare`, not a `define`; the checker skips the
    /// body analyses and enforces the extern ABI type domain).
    pub is_extern: bool,
    /// `export extern fn ..` — a Vyrn function ADDITIONALLY exported to JS on the
    /// wasm target (RFC-0012 M2). Unlike an `is_extern` import this is a *normal*
    /// function in every respect: it has a body that is fully checked, runs under
    /// the interpreter, participates in spawn-purity analysis by that body, and
    /// is a plain `define` in codegen — it only gains a `wasm-export-name`
    /// attribute so wasm-ld exports it under its Vyrn name. `is_extern` and
    /// `is_export_extern` are mutually exclusive (import vs. exported impl); the
    /// checker additionally enforces the extern ABI type domain on its signature.
    pub is_export_extern: bool,
    /// `gen fn ..` — a compile-time module generator (RFC-0021). A contextual
    /// modifier (the `extern`/`test` precedent): an ordinary function in every
    /// respect (has a body, callable at runtime, testable, formatted, exportable)
    /// EXCEPT that it may be used as an `import { .. } from gen(args)` target, in
    /// which case the loader runs it in the compiler's interpreter to synthesize a
    /// module. Because it can run at generation time, the checker holds every `gen
    /// fn` (and its transitive callees) to the **comptime-purity** discipline —
    /// the spawn-isolation sibling: no `extern`, `spawn`, module state,
    /// `writeFile`, `readLine`, `args`, or logging sinks.
    pub is_gen: bool,
    /// `mut fn ..` — this procedure changes state (RFC-0074 M4a). A declaration,
    /// never an inference: nothing checks that a `mut fn` writes anything or that
    /// a plain one does not, because Vyrn does not track effects. Its whole value
    /// is that the fact is stated in a real symbol — it renames, it hovers, and it
    /// cannot be misspelled into silence the way a `get*`/`list*` naming
    /// convention can. One bit with per-transport spellings, reflected as
    /// `FnInfo.mutates`: `std/graphql` reads it as Query vs Mutation, an HTTP
    /// projection as not-a-`GET`, gRPC ignores it. `mut` is not a new reserved
    /// word (`let mut` already has it); `export mut fn` is the only combination,
    /// with `export` outermost like `export gen fn` / `export extern fn`.
    pub is_mut: bool,
}

impl Function {
    /// The declared name's column span, for a diagnostic about the DECLARATION
    /// (`(0, 0)` — "whole line" — for a synthesized function with no column).
    pub fn name_span(&self) -> (usize, usize) {
        match self.col {
            0 => (0, 0),
            c => (c, c + self.name.chars().count()),
        }
    }
}

/// A capability declares what a function does with a parameter (RFC-0004):
/// the programmer's *intent*, from which the compiler enforces usage rules.
/// v0.1 gives `Consume` real semantics (move / use-after-consume checking);
/// `Read`/`Modify`/`Share` are accepted but currently behave like `Read`.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// Observe the value; it remains usable by the caller. (Default.)
    Read,
    /// Mutate in place (surface-only in v0.1; treated as `Read`).
    Modify,
    /// Take ownership; the caller may not use the value afterward.
    Consume,
    /// Share concurrent read access (surface-only in v0.1; treated as `Read`).
    Share,
}

/// A single parameter (name + capability + declared type).
#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub capability: Capability,
    pub ty: Type,
}

/// The v0.1 type universe. Structural records and unions (RFC-0002) are not
/// represented yet; validated types are represented by [`Type::Named`] plus a
/// [`TypeDecl`] carrying the predicate.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Type {
    /// The default 64-bit signed integer, written `Int64` (there is no
    /// unsized `Int` in the surface language).
    Int,
    /// A sized integer: `Int8`/`Int16`/`Int32` signed, `UInt8`/`UInt16`/`UInt32`/
    /// `UInt64` unsigned. `bits` ∈ {8, 16, 32, 64}; arithmetic wraps at that width
    /// (two's complement). `Int`/`Int64` stays the distinct default [`Type::Int`].
    IntN {
        bits: u8,
        signed: bool,
    },
    /// 64-bit IEEE-754 floating point (`Float64`, also spelled `Float`).
    Float,
    /// 32-bit IEEE-754 floating point (`Float32`). Arithmetic rounds to single
    /// precision at each step; the default float literal is [`Type::Float`] (f64).
    Float32,
    /// Four `Float32` lanes as one value (RFC-0083). A *value*, not a container:
    /// it has no heap, no ownership beyond a scalar's, and nothing to drop, which
    /// is why it needs no entry in `own.rs` or `movecheck.rs`. Lane-wise `+ - * /`
    /// are four INDEPENDENT IEEE-754 single-precision operations — lane *i* of the
    /// result reads lane *i* of the operands and nothing else — so no
    /// reassociation happens and an interpreter emulating it in a loop is
    /// bit-identical to a hardware `f32x4.add`. That is the whole reason explicit
    /// SIMD is expressible here when auto-vectorised float reductions are not.
    /// Lowers to `<4 x float>` textually and to a wasm `v128`.
    F32x4,
    /// Four `Int32` lanes as one value (RFC-0083 M3). A value exactly as
    /// [`Type::F32x4`] is, and lowered the same way — `<4 x i32>` textually, a
    /// wasm `v128` — but NOT the same type with a different lane: it has no `/`
    /// (no hardware has SIMD integer divide, and the wasm encoder has no
    /// `I32x4Div` to emit), no `sqrt` or rounding, and it gains `& | ^ ~`
    /// directly, which `F32x4` reaches only through a [`Type::Mask32x4`].
    ///
    /// The lanes are SIGNED, and that is what answers the one question the width
    /// raises: wasm has `i32x4.min_s`/`min_u` and four ordered-comparison pairs,
    /// and a lane type of `Int32` picks the signed half of every one. The
    /// unsigned half would need a `U32x4` — a second type, since the choice
    /// belongs to the operand and not to the operation — and nothing asks for
    /// one. Arithmetic WRAPS at 32 bits, as scalar `Int32` does.
    I32x4,
    /// Two `Float64` lanes as one value (RFC-0083 M4). [`Type::F32x4`]'s table
    /// with the lane widened and the count halved — `<2 x double>` textually, a
    /// wasm `v128` — and it keeps everything the float width has, `/` and `sqrt`
    /// included, because `f64x2` has every one of those opcodes.
    ///
    /// This is the width explicit SIMD is actually FOR, and the reason is in the
    /// RFC's opening: a `Float64` reduction is the one an optimiser may not
    /// vectorise, since float addition does not associate and reassociating it
    /// changes bits. The integer width measured *slower* than the scalar loop
    /// LLVM was allowed to vectorise; this one is 1.9x faster than its scalar
    /// twin, measured before it shipped.
    F64x2,
    /// Four `Bool` lanes as one value — what a lane-wise comparison of two
    /// [`Type::F32x4`]s (or two [`Type::I32x4`]s) yields (RFC-0083 M2).
    ///
    /// A type of its OWN rather than an `I32x4` of all-ones/all-zeros, which is
    /// what wasm and LLVM both call a mask at the machine level. The bit pattern
    /// is still exactly that — `<4 x i32>` and a `v128` — but nothing in the
    /// language can name it, so no program can hand `select` a vector of `7`s and
    /// ask what happens. There is no answer to that question the three engines
    /// would agree on for free, and a type is cheaper than a normalisation.
    Mask32x4,
    /// Two `Bool` lanes — what a comparison of two [`Type::F64x2`]s yields
    /// (RFC-0083 M4). A SECOND type rather than a generalisation of
    /// [`Type::Mask32x4`], and the argument is short: a mask is N booleans about
    /// N lanes and nothing in it remembers what compared them, so what
    /// characterises one is the lane COUNT and the lane WIDTH — which is exactly
    /// what this width changes and what `I32x4` did not. Vyrn has no const
    /// generics, so there is no `Mask<N>` to write, and inventing one for two
    /// inhabitants would be machinery serving a count.
    ///
    /// `<2 x i64>` of all-ones/all-zeros, and a `v128` again.
    Mask64x2,
    Bool,
    /// An immutable, statically-allocated string (v0.1: literals only).
    Str,
    /// The type of statements / functions returning nothing.
    Unit,
    /// A named validated type; resolved against the program's [`TypeDecl`]s.
    Named(String),
    /// A built-in optional (RFC-0005). The inner type is a scalar or validated
    /// scalar in v0.1.
    Option(Box<Type>),
    /// A built-in result (RFC-0005): `Result<T, E>`. Both payloads are scalar or
    /// validated scalars in v0.1.
    Result(Box<Type>, Box<Type>),
    /// A structural record type (RFC-0002): an ordered set of named fields.
    /// Compatibility is by shape (width subtyping), not name.
    Record(Vec<Field>),
    /// `Omit<T, f, ...>` — the record `T` with the named fields removed (a
    /// compile-time type transformer; RFC-0002 §7).
    Omit(Box<Type>, Vec<String>),
    /// `Pick<T, f, ...>` — the record `T` keeping only the named fields.
    Pick(Box<Type>, Vec<String>),
    /// `Merge<A, B>` — the fields of `A` and `B` combined (`B` wins on conflict).
    Merge(Box<Type>, Box<Type>),
    /// `Partial<T>` — the record `T` with every field made `Option<field>`.
    Partial(Box<Type>),
    /// A user-defined enum / sum type (RFC-0002 §4): an ordered set of variants.
    Enum(Vec<EnumVariant>),
    /// A generic type parameter (`T` inside `fn id<T>(..)`) — opaque while
    /// checking the body, substituted with a concrete type at each call site.
    Param(String),
    /// An application of a generic named type, e.g. `Box<Int>` — resolved by
    /// substituting the declaration's parameters with these arguments.
    App(String, Vec<Type>),
    /// A growable heap array of `T` (RFC-0002-ish; a `Vec`). Lowers to
    /// `{ ptr data, i64 len, i64 cap }`. Used linearly: `push` returns the
    /// updated array (the backing buffer may be reallocated).
    Array(Box<Type>),
    /// A fixed-size array `Array<T, N>` (const generic). Lowers to the value
    /// aggregate `[N x T]` — stack-allocated, no heap.
    ArrayN(Box<Type>, usize),
    /// A small-buffer array `SmallArray<T, N>` (RFC-0056): API-identical to
    /// `Array<T>` but its first `N` elements live inline (no heap allocation),
    /// spilling to the heap only past `N`. Lowers to
    /// `{ i64 len, i64 cap, ptr data, [N x T] inline }` with `cap` as the state
    /// discriminant (`cap == N` inline, `cap > N` spilled). `N` is a
    /// [`Type::ConstInt`] literal in the source, extracted here to a `usize`.
    SmallArray(Box<Type>, usize),
    /// A non-negative integer literal used as a *type argument* (RFC-0056), e.g.
    /// the `8` in `SmallArray<Int64, 8>`. This is a scoped grammar addition, not
    /// general const generics: it may only appear as a type argument, and only
    /// `SmallArray` consumes it — any other type constructor carrying one is a
    /// checker error. It has no runtime lowering of its own (`llt` never sees a
    /// bare `ConstInt`).
    ConstInt(u64),
    /// A growable, insertion-ordered dictionary `Map<String, V>` (RFC-0028). The
    /// two boxes are the key and value types; keys are `String` in v1 (the
    /// checker rejects a non-`String` key spelling with a named diagnostic).
    /// Lowers to `{ ptr keys, ptr values, i64 len, i64 cap }` — two parallel
    /// growable buffers sharing one length/capacity, preserving first-insertion
    /// order (an update keeps the slot; remove-then-insert moves to the end).
    Map(Box<Type>, Box<Type>),
    /// A **linear** sequence of `T` (RFC-0075): produced once, disposed exactly
    /// once, and the disposal is checked at compile time by `movecheck`.
    ///
    /// A type of its own rather than an `Array<T>` with an attribute, and rather
    /// than a library type, for one reason each: an attribute on `Array<T>`
    /// would leave `at`/`push`/`.length` applicable to a stream (a consumed
    /// stream would still answer `.length`), and a library type would have to be
    /// spelled with an `Array` base, so `let a: Array<T> = s` would launder the
    /// obligation away. Neither is a runtime concern — both are about what the
    /// checker will accept — so the *lowering* is `Array<T>`'s exactly, in all
    /// three engines: `{ ptr data, i64 len, i64 cap }`, `Val::Array`, the same
    /// indexed walk for `for … in`. RFC-0083's `F32x4` is the opposite case (a
    /// value with a new representation and no ownership); this is a resource
    /// with a new *rule* and no new representation.
    ///
    /// M1's only producer is `fromArray`, so the sequence is eager. M2's
    /// `unfold`/`channel` are what make it pull-based; nothing in the obligation
    /// depends on which, which is why the checking half could land first.
    Stream(Box<Type>),
    /// A handle to a concurrent task's result (RFC-0004 §Q4). Lowers to the
    /// result type `T` itself (a deterministic fork-join needs no boxing).
    Task(Box<Type>),
    /// A logger handle (RFC-0008). An opaque value obtained from `logger(name)`;
    /// the five level methods (`trace`/`debug`/`info`/`warn`/`error`) are called
    /// on it. Lowers to a `ptr` (its name string).
    Logger,
    /// A function value type (RFC-0023): `fn(T, U) -> R`. Legal ONLY as a
    /// top-level function-parameter type ("function types are parameter-only in
    /// v1" — enforced by the checker). Never storable, returnable, or escapable:
    /// every use is monomorphized away, so no function value exists at runtime in
    /// any backend and this type has no runtime lowering (`llt` never sees it).
    Fn(Vec<Type>, Box<Type>),
    /// A **deferred** record field: `type Book = { body: lazy String }` (RFC-0085
    /// M4a). Field position only — the parser accepts the modifier nowhere else,
    /// which is what keeps it from meeting `std/ui`'s `lazy(..)` *function*
    /// (RFC-0070, lazy PAGES): one is in type position, the other is a call, so
    /// `lazy` never has to become a keyword. Two different mechanisms one layer
    /// apart; prose that mentions both must name them apart.
    ///
    /// **The representation is a stored nullary closure** — `lazy T` IS
    /// `fn() -> T` at runtime, which is why RFC-0037 is the whole of the
    /// lowering: [`crate::types::resolve`] answers `Fn([], T)` for it, so
    /// layout, ownership, movecheck and every backend see a function value they
    /// already know. What the marker buys is what happens at the two ends:
    /// **construction takes the thunk** (`body: || load(id)`, so the deferral is
    /// visible where the work is written) and **a read FORCES it** (`b.body` is a
    /// `String`, not a `fn`), which is the whole difference from an ordinary
    /// fn-typed field.
    ///
    /// **Recomputed per read, not memoized** — see the RFC's "M4a — as landed".
    Lazy(Box<Type>),
    /// The type of an expression that never produces a value (RFC-0079). The
    /// only thing that has it is `panic(msg)`, which writes the message and
    /// exits 1, so no context downstream of one ever runs. It is the bottom
    /// type: `assignable(Never, _)` is true and the reverse is not, which is
    /// what lets `match x { A => panic(".."), B => 5 }` be an `Int64`.
    ///
    /// Unspellable in a signature — RFC-0079 leaves divergent functions open —
    /// so it arises only from that one call. Both backends lower it to `void`
    /// and neither produces a value for it: the textual one has already left
    /// through `unreachable` and the direct one through wasm's `unreachable`.
    Never,
    /// A compile-time "type-check failed here" sentinel used for inside-body
    /// error recovery (RFC-0006 accumulation). When a `let` initializer or a
    /// sub-expression fails to type-check, the binding / hole is filled with
    /// `Err` so the checker can keep going and report the *next* real error
    /// instead of a cascade of "unknown variable" / spurious-mismatch follow-ons.
    /// Permissive: `assignable(_, Err)` and `assignable(Err, _)` are both true,
    /// so an `Err`-typed value flows through any context without manufacturing a
    /// second diagnostic. Never reaches codegen — it only arises from a check
    /// error, and a program with any `Err` has at least one diagnostic.
    Err,
}

impl Type {
    /// Every variant of this enum, as a value.
    ///
    /// The lock a coverage test needs, and it has two halves that only work
    /// together: [`Type::variant_name`] is an exhaustive `match`, so a new
    /// variant stops the compile; this list is the same set as data, so a
    /// variant that gained a `match` arm but no test case still fails. Neither
    /// half alone is a guard, and PR #173 is what happens without both —
    /// `layout::SHAPES` claimed to be the whole type universe while `Stream` and
    /// four vector spellings had never once been checked.
    ///
    /// It lives on the type rather than in a test module because two crates ask
    /// the same question of it: `vyrn-codegen` asserts every variant has a
    /// layout, and [`crate::codec`] asserts every variant has one wire verdict.
    /// A second copy of this list is a second thing to keep complete.
    pub const VARIANTS: &'static [&'static str] = &[
        "Int",
        "IntN",
        "Float",
        "Float32",
        "F32x4",
        "I32x4",
        "F64x2",
        "Mask32x4",
        "Mask64x2",
        "Bool",
        "Str",
        "Unit",
        "Named",
        "Option",
        "Result",
        "Record",
        "Omit",
        "Pick",
        "Merge",
        "Partial",
        "Enum",
        "Param",
        "App",
        "Array",
        "ArrayN",
        "SmallArray",
        "ConstInt",
        "Map",
        "Stream",
        "Task",
        "Logger",
        "Fn",
        "Lazy",
        "Never",
        "Err",
    ];

    /// The name of this value's variant. The match computes nothing; its only job
    /// is to fail to compile when a variant is added (see [`Type::VARIANTS`]).
    pub fn variant_name(&self) -> &'static str {
        match self {
            Type::Int => "Int",
            Type::IntN { .. } => "IntN",
            Type::Float => "Float",
            Type::Float32 => "Float32",
            Type::F32x4 => "F32x4",
            Type::I32x4 => "I32x4",
            Type::F64x2 => "F64x2",
            Type::Mask32x4 => "Mask32x4",
            Type::Mask64x2 => "Mask64x2",
            Type::Bool => "Bool",
            Type::Str => "Str",
            Type::Unit => "Unit",
            Type::Named(_) => "Named",
            Type::Option(_) => "Option",
            Type::Result(..) => "Result",
            Type::Record(_) => "Record",
            Type::Omit(..) => "Omit",
            Type::Pick(..) => "Pick",
            Type::Merge(..) => "Merge",
            Type::Partial(_) => "Partial",
            Type::Enum(_) => "Enum",
            Type::Param(_) => "Param",
            Type::App(..) => "App",
            Type::Array(_) => "Array",
            Type::ArrayN(..) => "ArrayN",
            Type::SmallArray(..) => "SmallArray",
            Type::ConstInt(_) => "ConstInt",
            Type::Map(..) => "Map",
            Type::Stream(_) => "Stream",
            Type::Task(_) => "Task",
            Type::Logger => "Logger",
            Type::Fn(..) => "Fn",
            Type::Lazy(_) => "Lazy",
            Type::Never => "Never",
            Type::Err => "Err",
        }
    }
}

impl std::fmt::Display for Type {
    /// The user-facing spelling of a type, exactly as it is written in Vyrn
    /// source: `Int64`, `UInt8`, `Float64`, `String`, `Option<T>`, a named
    /// type by its name, a record by its shape. Diagnostics use this — never
    /// the `Debug` form (`IntN { bits: 8, .. }` / `Named("Age")`).
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        match self {
            Type::Int => write!(f, "Int64"),
            Type::IntN { bits, signed } => {
                write!(f, "{}Int{bits}", if *signed { "" } else { "U" })
            }
            Type::Float => write!(f, "Float64"),
            Type::Float32 => write!(f, "Float32"),
            Type::F32x4 => write!(f, "F32x4"),
            Type::I32x4 => write!(f, "I32x4"),
            Type::Mask32x4 => write!(f, "Mask32x4"),
            Type::F64x2 => write!(f, "F64x2"),
            Type::Mask64x2 => write!(f, "Mask64x2"),
            Type::Bool => write!(f, "Bool"),
            Type::Str => write!(f, "String"),
            Type::Unit => write!(f, "Unit"),
            Type::Named(n) | Type::Param(n) => write!(f, "{n}"),
            Type::Option(t) => write!(f, "Option<{t}>"),
            Type::Result(t, e) => write!(f, "Result<{t}, {e}>"),
            Type::Record(fields) => {
                write!(f, "{{ ")?;
                for (i, fld) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", fld.name, fld.ty)?;
                }
                write!(f, " }}")
            }
            Type::Omit(b, keys) => write!(f, "Omit<{b}, {}>", keys.join(", ")),
            Type::Pick(b, keys) => write!(f, "Pick<{b}, {}>", keys.join(", ")),
            Type::Merge(a, b) => write!(f, "Merge<{a}, {b}>"),
            Type::Partial(b) => write!(f, "Partial<{b}>"),
            Type::Enum(vs) => {
                let names: Vec<&str> = vs.iter().map(|v| v.name.as_str()).collect();
                write!(f, "enum {{ {} }}", names.join(" | "))
            }
            Type::App(n, args) => {
                let rendered: Vec<String> = args.iter().map(|a| a.to_string()).collect();
                write!(f, "{n}<{}>", rendered.join(", "))
            }
            Type::Array(t) => write!(f, "Array<{t}>"),
            Type::ArrayN(t, n) => write!(f, "Array<{t}, {n}>"),
            Type::SmallArray(t, n) => write!(f, "SmallArray<{t}, {n}>"),
            Type::ConstInt(n) => write!(f, "{n}"),
            Type::Map(k, v) => write!(f, "Map<{k}, {v}>"),
            Type::Stream(t) => write!(f, "Stream<{t}>"),
            Type::Task(t) => write!(f, "Task<{t}>"),
            Type::Logger => write!(f, "Logger"),
            Type::Fn(params, ret) => {
                let ps: Vec<String> = params.iter().map(|p| p.to_string()).collect();
                write!(f, "fn({})", ps.join(", "))?;
                if **ret != Type::Unit {
                    write!(f, " -> {ret}")?;
                }
                Ok(())
            }
            Type::Lazy(inner) => write!(f, "lazy {inner}"),
            Type::Never => write!(f, "Never"),
            Type::Err => write!(f, "<type error>"),
        }
    }
}

/// A brace-delimited sequence of statements.
#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
}

/// A statement. In v0, `if`/`while` are statements (not expressions).
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `let [mut] name [: Type] = value;`
    Let {
        name: String,
        mutable: bool,
        ty: Option<Type>,
        value: Expr,
        line: usize,
    },
    /// `name = value;` (only legal for `mut` bindings)
    Assign {
        name: String,
        value: Expr,
        line: usize,
    },
    /// `name.field = value;` — mutate a field of a `mut` record binding.
    SetField {
        name: String,
        field: String,
        value: Expr,
        line: usize,
    },
    /// `name[index] = value` — store `value` into element `index` of a `mut`
    /// array binding (RFC-0011). `name` must be a plain array binding (v1
    /// restriction, like `Assign`/`SetField`); the read form `a[i]` desugars
    /// to `@at(a, i)`, so a trailing `=` on it becomes this in-place store.
    IndexSet {
        name: String,
        index: Expr,
        value: Expr,
        line: usize,
    },
    /// `return [expr];`
    Return { value: Option<Expr>, line: usize },
    /// `break` — exit the innermost enclosing `for`/`while` loop (RFC-0060).
    /// A checker error outside a loop. Unlabeled only.
    Break { line: usize },
    /// `continue` — skip to the innermost enclosing loop's next iteration
    /// (RFC-0060). A checker error outside a loop. Unlabeled only.
    Continue { line: usize },
    /// `if cond { .. } [else { .. }]`
    If {
        cond: Expr,
        then_block: Block,
        else_block: Option<Block>,
        line: usize,
    },
    /// `if let PAT = SCRUTINEE { .. } [else { .. }]` (RFC-0060) — a statement
    /// form (not an expression in v1). `PAT` is a refutable `match`-arm pattern
    /// (`Some(x)`, `Ok(v)`, `Err(e)`, a user enum variant, incl. multi-payload):
    /// its binders are in scope in `then_block` only. `else_block` chains via
    /// `else if` / `else if let` exactly like `Stmt::If`. `while let` is a parser
    /// desugar onto `while true { if let PAT = e { body } else { break } }`, so
    /// only this node needs backend support; the scrutinee is evaluated once per
    /// probe (no double-eval), and diagnostics use the source `line`.
    IfLet {
        pattern: Pattern,
        scrutinee: Expr,
        then_block: Block,
        else_block: Option<Block>,
        line: usize,
    },
    /// `while cond { .. }`
    While {
        cond: Expr,
        body: Block,
        line: usize,
    },
    /// `for name in iter { .. }` — iterate an array, binding each element to
    /// `name` (a fresh immutable binding scoped to the body). `iter` must be an
    /// array (`Array<T>` or `Array<T, N>`); `name` takes the element type `T`.
    ForIn {
        var: String,
        iter: Expr,
        body: Block,
        line: usize,
        /// `for x in consume xs` (RFC-0089 rule 2): the loop takes ownership of
        /// the container, so each `x` is an **owned** element and storing one is
        /// a move rather than a copy. After the loop the container is dead.
        ///
        /// A loop over a value that is not a place (`for o in diff(..)`) is
        /// consuming without the word: nobody else can hold a temporary.
        consuming: bool,
    },
    /// `drop name;` — explicitly reclaim a heap value (string / array / reference)
    /// and consume the binding. Most reclamation is inferred; this is the escape
    /// hatch for handoff/aliased values the compiler can't prove. Using `name`
    /// after `drop name;` is a compile error.
    Drop { name: String, line: usize },
    /// An expression used for its side effects, e.g. `print(x);`
    Expr(Expr),
    /// `region { .. }` — an arena scope. Heap allocations made while it is on
    /// the stack are freed deterministically when the block exits (RFC-0004 §4,
    /// the "region / arena" strategy). Introduces its own variable scope; values
    /// allocated inside must not escape it (enforced by the checker).
    Region { body: Block, line: usize },
}

/// Binary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum BinOp {
    Add,
    Sub,
    Mul,
    Div,
    Rem,
    Eq,
    NotEq,
    Lt,
    LtEq,
    Gt,
    GtEq,
    And,
    Or,
    /// `=~` — regular-expression full match: `String =~ "pattern"`. The pattern
    /// must be a string literal (compiled to a DFA at compile time).
    Match,
    // Bitwise operators (RFC-0045). Defined on the sized integer types (and the
    // literal `Int`); operands share one integer type. `Shl`/`Shr` take a
    // same-typed shift amount; `Shr` is arithmetic on a signed operand and
    // logical on an unsigned one. An out-of-range shift traps at runtime (or is
    // a compile error for a constant amount).
    BitAnd, // &
    BitOr,  // |
    BitXor, // ^
    Shl,    // <<
    Shr,    // >>
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    /// `~` — bitwise complement within the operand's width (RFC-0045).
    BitNot,
}

/// An expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    /// A byte literal `'c'` (RFC-0057) — one ASCII byte. Semantically an integer
    /// literal whose value is the byte; the checker defaults it to `UInt8` and it
    /// coerces from there exactly as an integer literal does. Backends treat it
    /// identically to [`Expr::Int`] with the same value.
    Byte(u8),
    /// A floating-point literal, e.g. `1.5` (`Float64`).
    Float(f64),
    Bool(bool),
    /// A string literal (already decoded).
    Str(String),
    Var {
        name: String,
        line: usize,
    },
    Unary {
        op: UnOp,
        expr: Box<Expr>,
        line: usize,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        line: usize,
    },
    /// A call: a user function, the built-in `print`, or an `Option`
    /// constructor (`Some`). `None` is parsed as a bare [`Expr::Var`].
    Call {
        name: String,
        args: Vec<Expr>,
        line: usize,
    },
    /// `match scrutinee { Some(x) => e, None => e }` — an expression yielding a
    /// value (RFC-0005). Arms are single expressions in v0.1.
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
        line: usize,
    },
    /// `if cond { expr } else if cond2 { expr } else { expr }` used in an
    /// EXPRESSION position (RFC-0030). Each branch is a single expression (no
    /// statements); an `else if` chain is the nested `IfExpr` in `else_branch`.
    /// `else_branch` is `None` only for an incomplete `if` with no `else` — the
    /// checker rejects that ("`if` used as an expression needs an `else`"), so
    /// every backend may assume `Some`. Lowers to the same branch+result
    /// machinery as a two-arm boolean `match`: the condition is evaluated, then
    /// only the taken branch; branches unify like match arms. The statement form
    /// (`Stmt::If`) is untouched and unrelated.
    IfExpr {
        cond: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Option<Box<Expr>>,
        line: usize,
    },
    /// `expr?` — unwrap an `Option`/`Result`, or propagate `None`/`Err` by
    /// returning it from the enclosing function (RFC-0005).
    Try {
        expr: Box<Expr>,
        line: usize,
    },
    /// A record literal, e.g. `User { name: 1, age: 30 }` (RFC-0002).
    StructLit {
        name: String,
        fields: Vec<(String, Expr)>,
        line: usize,
    },
    /// Field access, e.g. `user.name` (RFC-0002).
    Field {
        expr: Box<Expr>,
        field: String,
        line: usize,
    },
    /// Fallible construction of a validated type, `Age?(n)` — yields
    /// `Option<Age>` (`None` if the refinement fails) instead of aborting
    /// (RFC-0003).
    TryConstruct {
        name: String,
        args: Vec<Expr>,
        line: usize,
    },
    /// A fixed-size array literal `[a, b, c]` — type `Array<T, N>`.
    ArrayLit {
        elems: Vec<Expr>,
        line: usize,
    },
    /// A map literal (RFC-0028): the empty `[:]` (contextual, like `[]`) or a
    /// non-empty `["a": 1, "b": 2]`. Each entry is a `(key, value)` expression
    /// pair in written order; the value type comes from the expected `Map` type.
    MapLit {
        entries: Vec<(Expr, Expr)>,
        line: usize,
    },
    /// `spawn f(args)` — run a *pure* function as a concurrent task, yielding a
    /// `Task<T>` (RFC-0004 §Q4). The callee must be isolated (no I/O, no shared
    /// mutable state); the result is deterministic regardless of scheduling.
    Spawn {
        name: String,
        args: Vec<Expr>,
        line: usize,
    },
    /// A lambda literal (RFC-0023): `|x| expr` or `|x, y| { block }`. The
    /// parameters are untyped in the literal — their types flow from the expected
    /// `fn(..) -> R` type of the parameter position it is passed to. Legal ONLY as
    /// a call argument in a function-typed parameter position (enforced by the
    /// checker). Captures outer locals by read; monomorphized away in codegen.
    Lambda {
        params: Vec<String>,
        body: LambdaBody,
        line: usize,
    },
    /// `consume place` — a take: the value at `place` is moved out and the place
    /// is dead from that point (RFC-0093). The third position of the word:
    /// a parameter capability, `for x in consume xs`, and now a prefix on a
    /// place expression. `place` is a `Var` or a `Field` chain; every other
    /// shape is refused by `movecheck`, which owns the whole rule.
    ///
    /// Every engine below the frontend lowers it as its operand: a take is the
    /// load the read already emits, without the `.copy()` that used to follow.
    Consume {
        place: Box<Expr>,
        line: usize,
    },
}

/// A lambda's body (RFC-0023): a single expression (`|x| x * 2`) or a
/// brace-delimited block that uses `return` like an ordinary function body
/// (`|x| { ... return e }`).
#[derive(Debug, Clone, PartialEq)]
pub enum LambdaBody {
    Expr(Box<Expr>),
    Block(Block),
}

/// One arm of a `match`.
#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: Expr,
}

/// A pattern in a `match` arm. v0.1 supports the `Option` and `Result` variants.
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// `Some(name)` — binds the payload to `name`.
    Some(String),
    /// `None`.
    None,
    /// `Ok(name)` — binds the success payload.
    Ok(String),
    /// `Err(name)` — binds the error payload.
    Err(String),
    /// A user-enum variant pattern: `Circle(r)`, `Rect(w, h)`, or `Empty`.
    Variant(String, Vec<String>),
    /// The tag-1 arm — `Some` *or* `Ok`, whichever the scrutinee turns out to
    /// be. Unspellable in source; produced only by the `??` desugar (RFC-0079),
    /// which runs in the parser and so has no type information to choose with.
    /// This is the same trick `Expr::Try` plays for `?`, moved into `Pattern` so
    /// `??` can reach `match` and inherit its drops, ownership, validation and
    /// short-circuiting instead of restating any of them.
    Success(String),
    /// The tag-0 arm — `None` *or* `Err`. Carries a binder so a `Result`'s error
    /// payload is bound rather than dropped on the floor; on the `Option` path
    /// the checker binds nothing, since there is no payload.
    Failure(String),
}

impl Expr {
    /// The source line this expression starts on (best effort).
    pub fn line(&self) -> usize {
        match self {
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => 0,
            Expr::Var { line, .. }
            | Expr::Unary { line, .. }
            | Expr::Binary { line, .. }
            | Expr::Call { line, .. }
            | Expr::Match { line, .. }
            | Expr::IfExpr { line, .. }
            | Expr::Try { line, .. }
            | Expr::StructLit { line, .. }
            | Expr::Field { line, .. }
            | Expr::TryConstruct { line, .. }
            | Expr::ArrayLit { line, .. }
            | Expr::MapLit { line, .. }
            | Expr::Spawn { line, .. }
            | Expr::Consume { line, .. }
            | Expr::Lambda { line, .. } => *line,
        }
    }
}

/// Every lambda literal the program holds, by node address (RFC-0101 M6).
///
/// A backend walks a body through a recursion that has erased the program's
/// lifetime: at a lambda literal it has the node but not a borrow a worklist can
/// hold, so the direct wasm backend COPIED the body to put one there — 532 of
/// the corpus's off-program backend answers, because no recorded type can reach
/// a node the program does not hold. This gives the borrow back. A hit needs no
/// verification, which is what separates it from `project::Memo`'s keys: the
/// program outlives every walk over it, so nothing else can be living at one of
/// its addresses.
///
/// Function bodies and module-state initializers — what the backends lower. A
/// literal inside a leaked desugar is not here, and a caller that misses keeps
/// whatever it did before.
pub fn lambdas(p: &Program) -> std::collections::HashMap<usize, &LambdaBody> {
    let mut out = std::collections::HashMap::new();
    for f in &p.functions {
        lambdas_block(&f.body, &mut out);
    }
    for g in &p.globals {
        lambdas_expr(&g.init, &mut out);
    }
    out
}

fn lambdas_block<'a>(b: &'a Block, out: &mut std::collections::HashMap<usize, &'a LambdaBody>) {
    for s in &b.stmts {
        lambdas_stmt(s, out);
    }
}

fn lambdas_stmt<'a>(s: &'a Stmt, out: &mut std::collections::HashMap<usize, &'a LambdaBody>) {
    match s {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::SetField { value, .. }
        | Stmt::Expr(value) => lambdas_expr(value, out),
        Stmt::IndexSet { index, value, .. } => {
            lambdas_expr(index, out);
            lambdas_expr(value, out);
        }
        Stmt::Return { value, .. } => {
            if let Some(e) = value {
                lambdas_expr(e, out);
            }
        }
        Stmt::If {
            cond: scrutinee,
            then_block,
            else_block,
            ..
        }
        | Stmt::IfLet {
            scrutinee,
            then_block,
            else_block,
            ..
        } => {
            lambdas_expr(scrutinee, out);
            lambdas_block(then_block, out);
            if let Some(eb) = else_block {
                lambdas_block(eb, out);
            }
        }
        Stmt::While {
            cond: e, body: bl, ..
        }
        | Stmt::ForIn {
            iter: e, body: bl, ..
        } => {
            lambdas_expr(e, out);
            lambdas_block(bl, out);
        }
        Stmt::Region { body, .. } => lambdas_block(body, out),
        Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Drop { .. } => {}
    }
}

fn lambdas_expr<'a>(e: &'a Expr, out: &mut std::collections::HashMap<usize, &'a LambdaBody>) {
    match e {
        Expr::Lambda { body, .. } => {
            out.insert(e as *const Expr as usize, body);
            match body {
                LambdaBody::Expr(inner) => lambdas_expr(inner, out),
                LambdaBody::Block(b) => lambdas_block(b, out),
            }
        }
        Expr::Unary { expr, .. } | Expr::Field { expr, .. } | Expr::Try { expr, .. } => {
            lambdas_expr(expr, out)
        }
        Expr::Consume { place, .. } => lambdas_expr(place, out),
        Expr::Binary { lhs, rhs, .. } => {
            lambdas_expr(lhs, out);
            lambdas_expr(rhs, out);
        }
        Expr::Call { args, .. }
        | Expr::TryConstruct { args, .. }
        | Expr::Spawn { args, .. }
        | Expr::ArrayLit { elems: args, .. } => {
            for a in args {
                lambdas_expr(a, out);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            lambdas_expr(scrutinee, out);
            for a in arms {
                lambdas_expr(&a.body, out);
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            lambdas_expr(cond, out);
            lambdas_expr(then_branch, out);
            if let Some(eb) = else_branch {
                lambdas_expr(eb, out);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                lambdas_expr(v, out);
            }
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                lambdas_expr(k, out);
                lambdas_expr(v, out);
            }
        }
        Expr::Int(_)
        | Expr::Byte(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Str(_)
        | Expr::Var { .. } => {}
    }
}
