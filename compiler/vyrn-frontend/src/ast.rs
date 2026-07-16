//! Abstract syntax tree for the Vyrn v0 subset.

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
    /// `impl P for T { .. }` blocks — a type's methods for a protocol.
    pub impls: Vec<ImplBlock>,
    /// Top-level `let [mut] name [: Type] = initializer` module-state bindings
    /// (RFC-0013). Root-module-only (the loader rejects them in imported
    /// modules); initialized once, in declaration order, before `main`. Every
    /// function sees them as an outermost scope frame below its parameters.
    pub globals: Vec<GlobalDecl>,
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

/// The default logging threshold — `Info`: `trace`/`debug` are suppressed unless
/// a `logging { level: .. }` block lowers it.
pub const DEFAULT_LOG_LEVEL: usize = 2;

/// The ordinal of a log-level name (RFC-0008), `trace` lowest → `error` highest.
/// Shared by the config-block parser, the interpreter, and the codegen so they
/// filter identically. Returns `None` for an unknown name.
pub fn log_level_ordinal(name: &str) -> Option<usize> {
    match name {
        "trace" => Some(0),
        "debug" => Some(1),
        "info" => Some(2),
        "warn" => Some(3),
        "error" => Some(4),
        _ => None,
    }
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
        ImportName { original: name.into(), alias: None }
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
    Generator { name: String, args: Vec<Expr>, line: usize },
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
    pub methods: Vec<MethodSig>,
    pub line: usize,
}

/// One method signature inside a [`ProtocolDecl`]: `fn name(self, p: T, ..) -> R`.
/// `params` are the parameters *after* the `self` receiver.
#[derive(Debug, Clone, PartialEq)]
pub struct MethodSig {
    pub name: String,
    pub params: Vec<Type>,
    pub ret: Type,
    pub line: usize,
}

/// `impl P for T { fn m(self, ..) { .. } }` — the methods a type provides for a
/// protocol. Each method is an ordinary [`Function`] whose first parameter is the
/// `self` receiver (typed to `ty`).
#[derive(Debug, Clone, PartialEq)]
pub struct ImplBlock {
    pub protocol: String,
    pub ty: Type,
    pub methods: Vec<Function>,
    pub line: usize,
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
    IntN { bits: u8, signed: bool },
    /// 64-bit IEEE-754 floating point (`Float64`, also spelled `Float`).
    Float,
    /// 32-bit IEEE-754 floating point (`Float32`). Arithmetic rounds to single
    /// precision at each step; the default float literal is [`Type::Float`] (f64).
    Float32,
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
    /// A generational reference to a mutable heap cell holding a `T` (RFC-0004
    /// §4, Path B). Freely copyable; a stale reference is caught by a generation
    /// check at each access instead of dangling. Lowers to `{ i64 slot, i64 gen }`
    /// regardless of `T` (the payload is boxed), so `Ref<T>` is a fixed-size
    /// handle — a record may hold a `Ref` to its own type without becoming
    /// infinite.
    Ref(Box<Type>),
    /// A growable heap array of `T` (RFC-0002-ish; a `Vec`). Lowers to
    /// `{ ptr data, i64 len, i64 cap }`. Used linearly: `push` returns the
    /// updated array (the backing buffer may be reallocated).
    Array(Box<Type>),
    /// A fixed-size array `Array<T, N>` (const generic). Lowers to the value
    /// aggregate `[N x T]` — stack-allocated, no heap.
    ArrayN(Box<Type>, usize),
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
            Type::Ref(t) => write!(f, "Ref<{t}>"),
            Type::Array(t) => write!(f, "Array<{t}>"),
            Type::ArrayN(t, n) => write!(f, "Array<{t}, {n}>"),
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
    /// to `at(a, i)`, so a trailing `=` on it becomes this in-place store.
    IndexSet {
        name: String,
        index: Expr,
        value: Expr,
        line: usize,
    },
    /// `return [expr];`
    Return { value: Option<Expr>, line: usize },
    /// `if cond { .. } [else { .. }]`
    If {
        cond: Expr,
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
}

/// Unary operators.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
}

/// An expression.
#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64),
    /// A floating-point literal, e.g. `1.5` (`Float64`).
    Float(f64),
    Bool(bool),
    /// A string literal (already decoded).
    Str(String),
    Var { name: String, line: usize },
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
    /// `expr?` — unwrap an `Option`/`Result`, or propagate `None`/`Err` by
    /// returning it from the enclosing function (RFC-0005).
    Try { expr: Box<Expr>, line: usize },
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
    ArrayLit { elems: Vec<Expr>, line: usize },
    /// `spawn f(args)` — run a *pure* function as a concurrent task, yielding a
    /// `Task<T>` (RFC-0004 §Q4). The callee must be isolated (no I/O, no shared
    /// mutable state); the result is deterministic regardless of scheduling.
    Spawn { name: String, args: Vec<Expr>, line: usize },
    /// A lambda literal (RFC-0023): `|x| expr` or `|x, y| { block }`. The
    /// parameters are untyped in the literal — their types flow from the expected
    /// `fn(..) -> R` type of the parameter position it is passed to. Legal ONLY as
    /// a call argument in a function-typed parameter position (enforced by the
    /// checker). Captures outer locals by read; monomorphized away in codegen.
    Lambda { params: Vec<String>, body: LambdaBody, line: usize },
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
}

impl Expr {
    /// The source line this expression starts on (best effort).
    pub fn line(&self) -> usize {
        match self {
            Expr::Int(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => 0,
            Expr::Var { line, .. }
            | Expr::Unary { line, .. }
            | Expr::Binary { line, .. }
            | Expr::Call { line, .. }
            | Expr::Match { line, .. }
            | Expr::Try { line, .. }
            | Expr::StructLit { line, .. }
            | Expr::Field { line, .. }
            | Expr::TryConstruct { line, .. }
            | Expr::ArrayLit { line, .. }
            | Expr::Spawn { line, .. }
            | Expr::Lambda { line, .. } => *line,
        }
    }
}
