//! Abstract syntax tree for Vyrn source.

/// The internal name of a `panic` stamped with its site: `@panicAt(msg, "file:line")`.
///
/// The loader stamps every `panic`, because only it knows both the file and the
/// line. `@` does not lex, so no source can write this call; the same reason
/// [`crate::project::ELEM`] is spelled `@slot`.
pub const PANIC_AT: &str = "@panicAt";

/// Returns whether a call name is a `panic`, stamped or not.
///
/// Both spellings stay live: the single-file `analyze` path the LSP uses never
/// runs the loader, so its `panic` calls are unstamped.
pub fn is_panic(name: &str) -> bool {
    name == "panic" || name == PANIC_AT
}

/// Names one syntax node for the side tables that passes key by node.
///
/// An id is the node's unit and its index in that unit. [`Program::number`]
/// makes each declaration it walks a unit, in declaration order, and numbers
/// a unit's nodes from 1 in pre-order. So an edit inside one unit renumbers no
/// other, and no numbered node is [`NodeId::NONE`]. An id survives a clone and
/// a move of the tree. It is a storage index, never an order, so it has no
/// `Ord`. A projection expansion is not in the program: [`crate::project`]
/// numbers it in its site's [`NodeId::expansion_unit`].
#[derive(Clone, Copy, Debug, Default, PartialEq, Eq, Hash)]
pub struct NodeId {
    unit: u32,
    local: u32,
}

impl NodeId {
    /// No node: the site of a row the core synthesizes.
    pub const NONE: NodeId = NodeId { unit: 0, local: 0 };

    /// The first unit of the expansion range, above every program's own units.
    pub const EXPANDED: u32 = 1 << 31;

    /// The unit the expansions anchored at this node are numbered in: its own
    /// unit's image in the expansion range. An expansion's node maps to its
    /// own unit, so a nested site shares its outer site's unit.
    pub fn expansion_unit(self) -> u32 {
        Self::EXPANDED | self.unit
    }

    /// The unit the node was numbered in ([`Numbering::unit`]).
    pub fn unit(self) -> u32 {
        self.unit
    }

    /// The same node of its body numbered as unit `unit`. A body's local
    /// numbering depends on its text alone.
    pub fn in_unit(self, unit: u32) -> NodeId {
        NodeId { unit, ..self }
    }
}

/// Names one function for the tables keyed by function: an index into
/// `vyrn_lower::World`'s function rows, never reused within one World.
/// [`Program::source_id`] numbers the source bodies, `Program::functions[i]`
/// first as `FnId(i)`; the lowering numbers its instances and lambda frames
/// after them. A storage index, never an order, so it has no `Ord`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct FnId(pub u32);

impl FnId {
    /// The id of row `i`.
    ///
    /// # Panics
    ///
    /// Past `u32::MAX` rows, more functions than a program's memory holds.
    pub fn nth(i: usize) -> FnId {
        FnId(u32::try_from(i).expect("more than u32::MAX functions"))
    }

    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// One source body of a linked program, by its position in its own list.
/// [`Program::source_id`] numbers it. Stable when synthesis appends
/// functions, which the id of every body after the functions is not, so the
/// checker records a read against this.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum SourceBody {
    /// `Program::functions[i]`.
    Fn(u32),
    /// The `i`th `impl` projection, in [`crate::project::all`] order.
    Place(u32),
    /// `Program::tests[i]`.
    Test(u32),
    /// `Program::benches[i]`.
    Bench(u32),
    /// The initializer of `Program::globals[i]`.
    Global(u32),
    /// The check of `Program::type_decls[i]`: its base, the types it names
    /// and its predicate.
    TypeDecl(u32),
}

/// Names one declaration of a linked program: its kind and its position in
/// that kind's list, which the loader fixes when it links. A function's
/// index is its [`FnId`]; a variant's counts every enum's variants in
/// `Program::type_decls` order.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct DeclId {
    pub kind: DeclKind,
    pub index: u32,
}

#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub enum DeclKind {
    /// `Program::functions`.
    Fn,
    /// `Program::type_decls`.
    Type,
    /// `Program::globals`.
    Global,
    /// `Program::protocols`.
    Protocol,
    /// An enum's variant.
    Variant,
}

impl DeclId {
    /// Declaration `i` of `kind`.
    ///
    /// # Panics
    ///
    /// Past `u32::MAX` declarations of one kind.
    pub fn nth(kind: DeclKind, i: usize) -> DeclId {
        let index = u32::try_from(i).expect("more than u32::MAX declarations");
        DeclId { kind, index }
    }

    pub fn index(self) -> usize {
        self.index as usize
    }
}

/// What one name lookup read: the declaration it found, or the scope and
/// name it missed in. A miss is a dependency too: a declaration of that name
/// turns it into a hit.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub enum Key {
    Decl(DeclId),
    Miss(ScopeId, String),
}

/// Where a lookup that missed looked: the reading module, `None` for the
/// root. Every table a lookup records is module-scoped, so no block path.
#[derive(Clone, Debug, PartialEq, Eq, Hash)]
pub struct ScopeId {
    pub module: Option<String>,
}

/// Names one protocol member: `Program::protocols[protocol].methods[member]`.
#[derive(Clone, Copy, Debug, PartialEq, Eq, Hash)]
pub struct MethodId {
    pub protocol: u32,
    pub member: u32,
}

/// A node's slot for its [`NodeId`]; [`NodeId::NONE`] until numbered. Any two
/// slots compare equal, so two trees compare by structure alone. `{:?}`
/// prints every slot alike, so a fingerprint over a tree's debug text ignores
/// ids too; `{:#?}` prints the id as `#unit.local`.
#[derive(Clone, Copy, Default)]
pub struct Id(pub NodeId);

impl Id {
    /// The slot a node is built with.
    pub const NEW: Id = Id(NodeId::NONE);
}

impl PartialEq for Id {
    fn eq(&self, _: &Id) -> bool {
        true
    }
}

impl std::fmt::Debug for Id {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if f.alternate() {
            write!(f, "#{}.{}", self.0.unit, self.0.local)
        } else {
            f.write_str("_")
        }
    }
}

/// The `{:?}` text of `x` with the number of every `line` and `col` field
/// and the value of every `doc` field outside a string literal left out.
/// With [`Id`]'s text, two trees that differ only in where they sit or in
/// their doc comments print alike, so a fingerprint over it keys what a check
/// reads. The AST has no `char` field, so a `"` always opens or closes a
/// literal; a `doc` value is `None` or `Some("..")`, so it ends at the next
/// `,` or `}` outside one.
pub fn unplaced(x: &impl std::fmt::Debug) -> String {
    let s = format!("{x:?}");
    let mut out = String::with_capacity(s.len());
    let (mut quoted, mut escaped, mut position, mut doc) = (false, false, false, false);
    for c in s.chars() {
        if position && c.is_ascii_digit() {
            continue;
        }
        position = false;
        let open = !quoted;
        match c {
            _ if escaped => escaped = false,
            '\\' if quoted => escaped = true,
            '"' => quoted = !quoted,
            _ => {}
        }
        if doc && !(open && matches!(c, ',' | '}')) {
            continue;
        }
        doc = false;
        out.push(c);
        if c == ' ' && open {
            position = out.ends_with(" line: ") || out.ends_with(" col: ");
            doc = out.ends_with(" doc: ");
        }
    }
    out
}

/// A whole program. `main` is the entry point.
#[derive(Debug, Clone, PartialEq)]
pub struct Program {
    /// Consumed by the loader, which links every imported module into this
    /// program; no later stage sees them.
    pub imports: Vec<ImportDecl>,
    pub type_decls: Vec<TypeDecl>,
    pub functions: Vec<Function>,
    pub protocols: Vec<ProtocolDecl>,
    /// Comptime-only: a contract reaches the emitted program only through the
    /// `contractOf(Name)` reflection literal.
    pub contracts: Vec<ContractDecl>,
    pub impls: Vec<ImplBlock>,
    /// Module-state bindings, root module only. Initialized once, in declaration
    /// order, before `main`.
    pub globals: Vec<GlobalDecl>,
    /// `(module, name)` for every module that can see its own or an imported
    /// declaration of a [`SURFACE_BUILTINS`] name.
    ///
    /// The loader fills this and the checker reads it: the checker never sees
    /// `imports`, and the loader does not resolve calls. Asking the linked
    /// program instead lets one module's `fn raw` disable the builtin in every
    /// module (`examples/shadowbuiltin.vyrn`). `None` is the root module, as on
    /// [`Function::module`].
    pub surface_shadows: std::collections::HashSet<(Option<String>, String)>,
    /// The logging threshold ordinal; a log call below it is dropped at compile
    /// time. [`DEFAULT_LOG_LEVEL`] without a `logging` block.
    pub log_level: usize,
    pub log_sink: LogSink,
    /// Kept out of `functions`, so a shipped binary contains no tests and the
    /// string pool and regex collection skip them. See [`NamedBlock`].
    pub tests: Vec<NamedBlock>,
    /// Kept out of `functions`, like [`Program::tests`]. `vyrn bench` lowers
    /// them to functions and a synthesized harness `main`.
    pub benches: Vec<NamedBlock>,
    /// The number of units [`Program::number`] and
    /// [`Program::number_appended`] gave.
    pub units: u32,
    /// What the program is compiled as, beyond an ordinary build.
    pub host: Host,
    /// `module key -> content hash` for every module the loader parsed into
    /// this program but the root, whose declarations name no module. The
    /// kernel's judgment memo and the checker's per-body reuse key a body on
    /// it ([`crate::movecheck::Judgments`]). Empty for a program no load
    /// linked.
    /// Ordered, because a generator program's `Debug` text keys its compiled
    /// module (`vyrn_genwasm`).
    pub module_hashes: std::collections::BTreeMap<String, String>,
    /// The projection expansions every pass over this program reads. The
    /// loader stamps the load's; a parsed program has an unshared one.
    pub expansions: std::sync::Arc<crate::project::Expansions>,
    /// How a sentence spells a declaration the loader renamed apart.
    pub spellings: std::sync::Arc<Spellings>,
    /// The session of the host that analyses this program. The loader stamps
    /// the load's; a parsed program has none.
    pub session: crate::session::SessionRef,
}

/// What a program is compiled as, beyond an ordinary build. A flag only
/// enables names; nothing reads it to refuse.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct Host {
    /// A generator host: the program the engine compiles to run a `gen fn`
    /// as wasm. Its functions have `is_gen` cleared, so this flag marks the
    /// whole program as generation code. `vyrn_genwasm::prepare` sets it.
    pub gen: bool,
    /// A test host: the program `vyrn test` and `vyrn bench` compile, whose
    /// functions are lifted `test` and `bench` bodies. It enables test-only
    /// names such as `assert`.
    pub test: bool,
}

/// A `test "name" { body }` or `bench "name" { body }` declaration.
///
/// Both are checked as a Unit-returning function body under an unspellable name
/// (`test@<index>`, `bench@<index>`), so every body analysis applies unchanged.
/// The keyword decides the [`Program`] field, and the field decides which
/// subcommand runs it. Only the root module's blocks run; an imported module's
/// still type-check.
#[derive(Debug, Clone, PartialEq)]
pub struct NamedBlock {
    /// The string literal after `test` or `bench`. Unique per file.
    pub name: String,
    /// Timed under `vyrn bench`, run once under `--check`.
    pub body: Block,
    pub doc: Option<String>,
    /// `None` for the root module. Set by the loader.
    pub module: Option<String>,
    pub line: usize,
}

/// A logging destination.
#[derive(Debug, Clone, PartialEq)]
pub enum LogSink {
    /// The default: keeps logs off the program's stdout.
    Stderr,
    Stdout,
    /// Truncated and opened for writing at program start.
    File(String),
}

/// Returns whether a binding is a place desugar's move-out temp.
///
/// `t.xs[k] = v` becomes `let mut t.xs[] = t.xs` / `t.xs[][k] = v` /
/// `t.xs = t.xs[]`. `[` cannot appear in an identifier, and only the container
/// moved out and written back ends in `[]`. Hoisted operand temps carry a
/// further suffix (`[]idx`, `#idx`, `#val`, `[]arg1`); the `#` keeps a hoisted
/// operand from reading as derived from the container under [`mentions_place`].
///
/// `parser::place_receiver` states the naming; this states the reading.
/// `parser::tests::the_desugars_temps_answer_the_one_predicate` pins both.
pub fn is_place_temp(name: &str) -> bool {
    name.ends_with("[]")
}

/// `Info`: `trace` and `debug` are dropped unless a `logging` block lowers it.
pub const DEFAULT_LOG_LEVEL: usize = 2;

/// The builtins spelled as ordinary identifiers rather than with an `@` prefix.
///
/// They are not reserved: a module that declares or imports one means its own.
/// The question is asked of the calling module's scope, never of the linked
/// program (see [`Program::surface_shadows`]).
pub const SURFACE_BUILTINS: [&str; 4] = ["render", "rawAt", "raw", "lex"];

pub fn is_surface_builtin(name: &str) -> bool {
    SURFACE_BUILTINS.contains(&name)
}

/// The five log levels, lowest first, in the spelling a call carries: the sugar
/// turns `log.info(m)` into `@info(log, m)`. The index is the ordinal a
/// `logging` block compares against. The words are not reserved: a module that
/// declares or imports `info` gets the word back.
pub const LOG_LEVELS: [&str; 5] = ["@trace", "@debug", "@info", "@warn", "@error"];

/// Returns the ordinal of a log-level name, or `None` for an unknown name.
pub fn log_level_ordinal(name: &str) -> Option<usize> {
    LOG_LEVELS.iter().position(|l| l[1..] == *name)
}

/// Returns the ordinal of a log call's internal spelling (`@info` is 2), or
/// `None` for any other call name.
pub fn log_internal(name: &str) -> Option<usize> {
    LOG_LEVELS.iter().position(|l| *l == name)
}

/// A top-level module-state binding: `let [mut] name [: Type] = init`. It lives
/// for the whole program, is shared by every function, and is never dropped.
#[derive(Debug, Clone, PartialEq)]
pub struct GlobalDecl {
    pub name: String,
    pub mutable: bool,
    /// `None` infers the type from the initializer.
    pub ty: Option<Type>,
    /// The checker refuses a call to an extern, a protocol method or a
    /// function of the same module, and a read of a later global. A function
    /// imported from another module is legal: its module initializes first.
    pub init: Expr,
    pub doc: Option<String>,
    /// `None` for the root module. Globals are root-only; the field keeps
    /// diagnostics uniform.
    pub module: Option<String>,
    pub line: usize,
}

/// One imported binding: `original`, bound locally as `alias` when written
/// `original as alias`.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportName {
    pub original: String,
    pub alias: Option<String>,
}

impl ImportName {
    pub fn bare(name: impl Into<String>) -> Self {
        ImportName {
            original: name.into(),
            alias: None,
        }
    }
    /// Returns the name this binding has in the importing module: the alias if
    /// present, else the original.
    pub fn local(&self) -> &str {
        self.alias.as_deref().unwrap_or(&self.original)
    }
}

/// One `import` declaration.
#[derive(Debug, Clone, PartialEq)]
pub struct ImportDecl {
    /// Also holds `import type { .. }` (JSON Schema); the loader dispatches on
    /// the path's extension. Empty for a namespace import.
    pub names: Vec<ImportName>,
    /// `ns` in `import * as ns from ..`. The loader folds each `ns.member` to the
    /// foreign declaration's symbol, so no later stage knows namespaces.
    pub namespace: Option<String>,
    pub source: ImportSource,
    pub line: usize,
}

/// The right-hand side of `import { .. } from <source>`.
#[derive(Debug, Clone, PartialEq)]
pub enum ImportSource {
    /// A module specifier as written: relative (`./lib`), `std/name`, a manifest
    /// alias, or a remote specifier.
    Path(String),
    /// `from gen(args...)`: the loader runs the `gen fn` at compile time on
    /// constant `args` and links the returned `String` as a module.
    Generator {
        name: String,
        args: Vec<Expr>,
        line: usize,
    },
}

/// A named type: a refinement over a scalar (`base` with a `predicate`) or a
/// structural type such as a record or an enum.
#[derive(Debug, Clone, PartialEq)]
pub struct TypeDecl {
    pub name: String,
    pub exported: bool,
    /// `None` for the root module. Set by the loader.
    pub module: Option<String>,
    pub doc: Option<String>,
    pub type_params: Vec<String>,
    pub base: Type,
    /// A predicate over the variable `value`.
    pub predicate: Option<Expr>,
    pub line: usize,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct Field {
    pub name: String,
    pub ty: Type,
}

#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub struct EnumVariant {
    pub name: String,
    /// Empty for a nullary variant.
    pub payload: Vec<Type>,
}

/// A protocol: a named set of method signatures. A type provides them with
/// `impl P for T`; a generic bounded by `<X: P>` may call them.
#[derive(Debug, Clone, PartialEq)]
pub struct ProtocolDecl {
    pub name: String,
    pub exported: bool,
    /// `None` for the root module. Set by the loader.
    pub module: Option<String>,
    pub doc: Option<String>,
    /// Associated types, in declaration order. A method signature names one as
    /// a [`Type::Param`] that the implementing type binds.
    pub assoc: Vec<String>,
    pub methods: Vec<MethodSig>,
    pub line: usize,
}

/// One method signature in a [`ProtocolDecl`]. `params` exclude the `self`
/// receiver.
#[derive(Debug, Clone, PartialEq)]
pub struct MethodSig {
    pub name: String,
    /// Read by `vyrn doc` and LSP hover.
    pub doc: Option<String>,
    /// An impl must match it. A caller reads only this when the receiver is a
    /// bounded type parameter.
    pub recv: Capability,
    pub params: Vec<Type>,
    /// Parallel to `params`. `movecheck` reads the discipline here, because the
    /// call site names the protocol member and cannot select the impl.
    pub param_caps: Vec<Capability>,
    pub ret: Type,
    /// `Some` for a projection requirement, satisfied by an impl's `places`
    /// member. The parser makes it equal to `recv`.
    pub result_cap: Option<Capability>,
    pub line: usize,
}

/// A module contract: the exports a module may have, with their types and
/// defaults.
///
/// A contract is to a module what a [`ProtocolDecl`] is to a type. It is
/// comptime-only: `contractOf(Page)` reflects one into a `ContractInfo` record,
/// and `std/contract:checkContract` compares in Vyrn code.
#[derive(Debug, Clone, PartialEq)]
pub struct ContractDecl {
    pub name: String,
    pub exported: bool,
    /// `None` for the root module. Set by the loader.
    pub module: Option<String>,
    pub doc: Option<String>,
    pub members: Vec<ContractMember>,
    pub line: usize,
}

impl ContractDecl {
    /// Returns the open rule (`fn *(..) -> ..`), if any. A contract without one
    /// is closed: an export it does not name is a diagnostic.
    pub fn open_rule(&self) -> Option<&ContractMember> {
        self.members.iter().find(|m| m.is_open_rule())
    }
}

/// One member of a [`ContractDecl`]. Its type parameters are open per member:
/// `let data: Query<T>` admits any instantiation of `Query`.
#[derive(Debug, Clone, PartialEq)]
pub struct ContractMember {
    /// The export's name, or [`OPEN_RULE_NAME`] for the open rule.
    pub name: String,
    /// Read by LSP completion and hover.
    pub doc: Option<String>,
    pub kind: ContractMemberKind,
    pub line: usize,
}

pub const OPEN_RULE_NAME: &str = "*";

impl ContractMember {
    pub fn is_open_rule(&self) -> bool {
        self.name == OPEN_RULE_NAME
    }
    /// Returns the member's default: the value a generator substitutes when the
    /// module omits the export. A function member's default is its call's
    /// value: `fn head() -> Head = noHead()`.
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
            // The `Type::Fn` spelling, so a contract member and a stored
            // function value read alike.
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
    /// Returns the member's type parameters, sorted. The parser marks them as
    /// [`Type::Param`].
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

fn collect_params(ty: &Type, out: &mut Vec<String>) {
    match ty {
        Type::Param(n) => out.push(n.clone()),
        Type::App(_, args) => {
            for a in args {
                collect_params(a, out);
            }
        }
        Type::Array(a)
        | Type::Stream(a)
        | Type::Partial(a)
        | Type::ArrayN(a, _)
        | Type::SmallArray(a, _)
        | Type::Omit(a, _)
        | Type::Pick(a, _) => collect_params(a, out),
        Type::Merge(a, b) | Type::Map(a, b) => {
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

/// Returns ` -> R`, or "" for `Unit`, as `Type::Fn`'s spelling does.
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
    /// `let name: Type [= default]`. A `default` makes the member optional.
    Value {
        ty: Type,
        default: Option<Box<Expr>>,
    },
    /// `fn name(params) -> Ret [= default]`. Parameter names are not part of
    /// the contract. The `default` has the RETURN type: `fn head() -> Head =
    /// noHead()` means a module without `head` has `noHead()` for its head.
    Fn {
        params: Vec<Type>,
        ret: Type,
        default: Option<Box<Expr>>,
        /// `fn *(..) -> R`: constrains the return type only and admits any
        /// arity. Legal on the open rule alone, because a named member's arity
        /// is part of what its name promises.
        variadic: bool,
    },
}

/// `impl P for T { .. }`: the methods a type provides for a protocol. Each
/// method is a [`Function`] whose first parameter is the `self` receiver.
#[derive(Debug, Clone, PartialEq)]
pub struct ImplBlock {
    pub protocol: String,
    /// The type variables of `impl<T> P for C<T>`; empty for a concrete impl.
    /// Each method inherits them, so monomorphization specializes it per
    /// receiver instantiation.
    pub type_params: Vec<String>,
    pub type_bounds: std::collections::HashMap<String, Vec<String>>,
    pub ty: Type,
    /// The associated type names this impl binds, in declaration order. The
    /// parser has already substituted each binding into the methods; a stored
    /// bound [`Type`] would be a second copy no loader walk rewrites.
    pub assoc: Vec<String>,
    pub methods: Vec<Function>,
    /// Place projections: members whose result carries a capability, such as
    /// `fn at(read self, i: Int64) -> read T`. A projection is never called,
    /// flattened into [`Program::functions`] or emitted: every access site
    /// inlines its body, so the borrow cannot outlive the access.
    ///
    /// The body ends in a [`Stmt::Return`]. The result's capability is not
    /// stored: the parser makes it equal `params[0].capability`.
    pub places: Vec<Function>,
    pub line: usize,
    /// 1-based column of the `impl` keyword, in Unicode scalar values; `0` for a
    /// synthesized block. See [`crate::diagnostics`] for the convention.
    pub col: usize,
}

impl ImplBlock {
    /// Returns the `impl` keyword's column span, or `(0, 0)` (the whole line)
    /// for a synthesized block.
    pub fn head_span(&self) -> (usize, usize) {
        match self.col {
            0 => (0, 0),
            c => (c, c + "impl".len()),
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Function {
    pub name: String,
    pub exported: bool,
    /// `None` for the root module. Set by the loader.
    pub module: Option<String>,
    pub doc: Option<String>,
    pub type_params: Vec<String>,
    /// Bounds per type parameter: `<T: Ord>` gives `{ "T": ["Ord"] }`.
    pub type_bounds: std::collections::HashMap<String, Vec<String>>,
    pub params: Vec<Param>,
    pub ret: Type,
    pub body: Block,
    pub line: usize,
    /// 1-based column of the NAME, in Unicode scalar values; `0` for a
    /// synthesized function. See [`crate::diagnostics`] for the convention.
    pub col: usize,
    /// `extern fn`: a body-less import the wasm host supplies from the `vyrn`
    /// namespace. The checker skips the body analyses and enforces the extern
    /// ABI type domain.
    pub is_extern: bool,
    /// `export extern fn`: an ordinary function with a checked body, also
    /// exported to JS on the wasm target. Exclusive with `is_extern`; the
    /// checker enforces the extern ABI type domain on its signature.
    pub is_export_extern: bool,
    /// `gen fn`: an ordinary function that may also be an `import .. from
    /// gen(args)` target, run by the loader at compile time. The checker holds
    /// it and its transitive callees to comptime purity: no `extern`, module
    /// state, `writeFile`, `readLine`, `args` or logging sinks.
    pub is_gen: bool,
    /// `mut fn`: declares that the function changes state. Nothing checks it.
    /// Reflected as `FnInfo.mutates`, which `std/graphql` reads as Query versus
    /// Mutation.
    pub is_mut: bool,
}

impl Function {
    /// Returns the name's column span, or `(0, 0)` (the whole line) for a
    /// synthesized function.
    pub fn name_span(&self) -> (usize, usize) {
        match self.col {
            0 => (0, 0),
            c => (c, c + self.name.chars().count()),
        }
    }
}

/// What a function does with a parameter.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Capability {
    /// The default. The value stays usable by the caller.
    Read,
    Modify,
    /// The caller may not use the value afterward.
    Consume,
}

impl Capability {
    /// The contextual word that spells this capability.
    pub fn word(self) -> &'static str {
        match self {
            Capability::Read => "read",
            Capability::Modify => "modify",
            Capability::Consume => "consume",
        }
    }

    /// The capability a word spells, or `None`.
    pub fn from_word(word: &str) -> Option<Capability> {
        [Capability::Read, Capability::Modify, Capability::Consume]
            .into_iter()
            .find(|c| c.word() == word)
    }
}

/// A name a binding form introduces, and where the source spells it.
///
/// `col` is the 1-based column of the name in Unicode scalar values, `0` when a
/// desugar made the binder. The editor indexes a local by its position and
/// skips a binder with no column.
///
/// The line is here because a binder's line is not always its node's: a `match`
/// arm is spelled below the `match`. A `let` and a `for` variable sit on their
/// statement's line, so they carry a column only.
#[derive(Debug, Clone, PartialEq)]
pub struct Binder {
    pub name: String,
    pub line: usize,
    pub col: usize,
    pub id: Id,
}

/// The kind of a binding, as the editor shows it. `body_scope_descent!` reports
/// it, so a reader of binding sites does not list the forms again.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum LocalKind {
    /// A function parameter. No body descent reports it; a signature reader does.
    Param,
    /// A `let`, a pattern binder or a lambda parameter.
    Let { mutable: bool },
    /// A `for` loop variable.
    ForVar,
}

impl Binder {
    /// Returns a binder no source token spells.
    pub fn synthetic(name: impl Into<String>) -> Self {
        Binder {
            id: Id::NEW,
            name: name.into(),
            line: 0,
            col: 0,
        }
    }
}

impl std::ops::Deref for Binder {
    type Target = str;
    fn deref(&self) -> &str {
        &self.name
    }
}

impl std::fmt::Display for Binder {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(&self.name)
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Param {
    pub name: String,
    pub capability: Capability,
    pub ty: Type,
    /// 1-based line of the parameter's name, since a signature spans lines. `0`
    /// for a synthesized parameter.
    pub line: usize,
    /// See [`Binder::col`].
    pub col: usize,
    pub id: Id,
}

/// A type. A validated type is a [`Type::Named`] whose [`TypeDecl`] carries the
/// predicate.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum Type {
    /// `Int64`, the default integer.
    Int,
    /// A sized integer other than `Int64`; `bits` is 8, 16, 32 or 64. Arithmetic
    /// wraps at that width.
    IntN {
        bits: u8,
        signed: bool,
    },
    /// `Float64`.
    Float,
    /// Rounds to single precision at each step.
    Float32,
    /// Four `Float32` lanes as one value, with nothing to drop. Each
    /// lane-wise operation is an independent IEEE-754 operation, so no
    /// reassociation happens.
    F32x4,
    /// Four signed `Int32` lanes; arithmetic wraps at 32 bits. It has no `/`, no
    /// `sqrt` and no rounding, and it has `& | ^ ~` directly. Unsigned lanes
    /// would need a second type, because the choice belongs to the operand.
    I32x4,
    /// Two `Float64` lanes, with every [`Type::F32x4`] operation.
    F64x2,
    /// Four `Bool` lanes: a lane-wise comparison of two [`Type::F32x4`]s or
    /// [`Type::I32x4`]s. A type of its own, although its bits are an `I32x4` of
    /// all-ones or all-zeros, so no program can hand `select` other lane values.
    Mask32x4,
    /// Two `Bool` lanes: a comparison of two [`Type::F64x2`]s. A mask is
    /// characterized by lane count and width, and Vyrn has no const generics for
    /// a `Mask<N>`.
    Mask64x2,
    Bool,
    Str,
    Unit,
    /// Resolved against the program's [`TypeDecl`]s.
    Named(String),
    /// Ordered named fields. Compatibility is by shape, not name.
    Record(Vec<Field>),
    /// `Omit<T, f, ...>`: record `T` without the named fields.
    Omit(Box<Type>, Vec<String>),
    /// `Pick<T, f, ...>`: record `T` with only the named fields.
    Pick(Box<Type>, Vec<String>),
    /// `Merge<A, B>`: the fields of both; `B` wins on a conflict.
    Merge(Box<Type>, Box<Type>),
    /// `Partial<T>`: record `T` with every field made an `Option`.
    Partial(Box<Type>),
    /// Ordered variants.
    Enum(Vec<EnumVariant>),
    /// A type parameter: opaque in the body, substituted at each call site.
    Param(String),
    /// A generic named type applied to arguments, such as `Box<Int64>`.
    App(String, Vec<Type>),
    /// A growable heap array: `{ ptr data, i64 len, i64 cap }`.
    Array(Box<Type>),
    /// `Array<T, N>`: the value aggregate `[N x T]`, with no heap.
    ArrayN(Box<Type>, usize),
    /// `SmallArray<T, N>`: the API of `Array<T>`, with the first `N` elements
    /// inline. Lowers to `{ i64 len, i64 cap, ptr data, [N x T] inline }`;
    /// `cap == N` means inline, `cap > N` spilled.
    SmallArray(Box<Type>, usize),
    /// An integer literal as a type argument, such as the `8` in
    /// `SmallArray<Int64, 8>`. Only `SmallArray` consumes it; the checker
    /// refuses it anywhere else.
    ConstInt(u64),
    /// An insertion-ordered dictionary of key and value types. An update keeps
    /// the slot; a remove then insert moves the key to the end.
    Map(Box<Type>, Box<Type>),
    /// A linear sequence: produced once and disposed exactly once, which
    /// `movecheck` checks.
    ///
    /// A type of its own so a consumed stream cannot answer `Array` methods, and
    /// so `let a: Array<T> = s` cannot launder the obligation away. Its layout
    /// is a six-word header tagged over the producers, not `Array<T>`'s.
    Stream(Box<Type>),
    /// A handle from `logger(name)` that the five level methods are called on.
    /// Lowers to a `ptr` to its name.
    Logger,
    /// `fn(T, U) -> R`, a function value type.
    Fn(Vec<Type>, Box<Type>),
    /// A deferred record field, `body: lazy String`, legal in field position
    /// only. At runtime it is `fn() -> T`: [`crate::types::resolve`] answers
    /// `Fn([], T)`. Construction takes a thunk; each read forces it, with no
    /// memo.
    ///
    /// Distinct from `std/ui`'s `lazy(..)` function; type position keeps `lazy`
    /// from becoming a keyword.
    Lazy(Box<Type>),
    /// The bottom type: only `panic(msg)` has it. `assignable(Never, _)` holds
    /// and the reverse does not, so `match x { A => panic(".."), B => 5 }` is an
    /// `Int64`. It cannot be spelled in a signature.
    Never,
    /// The type of a subexpression that failed to check, so the checker reports
    /// the next real error instead of a cascade. Assignable both ways. It never
    /// reaches codegen, because a program holding one has a diagnostic.
    Err,
}

impl Type {
    /// Returns `Option<T>` as its variant list: `None` is tag 0 and `Some` tag 1,
    /// the order [`Pattern::Failure`] and [`Pattern::Success`] name.
    /// [`Display`](std::fmt::Display) prints it as `Option<T>`, and
    /// [`crate::types::option_payload`] reads it back.
    pub fn option(t: Type) -> Type {
        Type::Enum(vec![
            EnumVariant {
                name: "None".to_string(),
                payload: Vec::new(),
            },
            EnumVariant {
                name: "Some".to_string(),
                payload: vec![t],
            },
        ])
    }

    /// Returns `Result<T, E>` as its variant list: `Err` is tag 0, as for
    /// [`Type::option`].
    pub fn result(ok: Type, err: Type) -> Type {
        Type::Enum(vec![
            EnumVariant {
                name: "Err".to_string(),
                payload: vec![err],
            },
            EnumVariant {
                name: "Ok".to_string(),
                payload: vec![ok],
            },
        ])
    }

    /// Every variant's name, as data for coverage tests.
    ///
    /// [`Type::variant_name`] is an exhaustive `match`, so a new variant stops
    /// the compile until it is named; this list lets a test fail when a variant
    /// has no case (PR #173). `vyrn-codegen` asserts every variant has a layout,
    /// and [`crate::codec`] that every variant has one wire verdict.
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
        "Logger",
        "Fn",
        "Lazy",
        "Never",
        "Err",
    ];

    /// Returns the variant's name. The `match` fails to compile when a variant
    /// is added (see [`Type::VARIANTS`]).
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
            Type::Logger => "Logger",
            Type::Fn(..) => "Fn",
            Type::Lazy(_) => "Lazy",
            Type::Never => "Never",
            Type::Err => "Err",
        }
    }
}

/// The name a type parameter was written with. The checker renames a
/// callee's parameters apart as `T'n` while it solves one call, and no
/// identifier holds a `'`.
pub fn written_param(n: &str) -> &str {
    n.split_once('\'').map_or(n, |(w, _)| w)
}

impl std::fmt::Display for Type {
    /// Writes the type as Vyrn source spells it, by linked names. A sentence
    /// writes it through [`Speech::ty`] instead.
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        Said(self, None).fmt(f)
    }
}

/// A type written for a sentence: [`Type`]'s `Display`, with each declared
/// name spelled by the [`Speech`] when there is one.
pub struct Said<'a>(&'a Type, Option<&'a Speech<'a>>);

impl<'a> std::fmt::Display for Said<'a> {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let say = |t: &'a Type| Said(t, self.1);
        let name = |n: &str| match self.1 {
            Some(s) => s.name(n),
            None => n.to_string(),
        };
        match self.0 {
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
            Type::Named(n) => write!(f, "{}", name(n)),
            Type::Param(n) => write!(f, "{}", written_param(n)),
            Type::Record(fields) => {
                write!(f, "{{ ")?;
                for (i, fld) in fields.iter().enumerate() {
                    if i > 0 {
                        write!(f, ", ")?;
                    }
                    write!(f, "{}: {}", fld.name, say(&fld.ty))?;
                }
                write!(f, " }}")
            }
            Type::Omit(b, keys) => write!(f, "Omit<{}, {}>", say(b), keys.join(", ")),
            Type::Pick(b, keys) => write!(f, "Pick<{}, {}>", say(b), keys.join(", ")),
            Type::Merge(a, b) => write!(f, "Merge<{}, {}>", say(a), say(b)),
            Type::Partial(b) => write!(f, "Partial<{}>", say(b)),
            // `resolve` turns `Option<T>` and `Result<T, E>` into variant lists;
            // a diagnostic still names them as the user wrote them.
            Type::Enum(vs) => match vs.as_slice() {
                [n, s] if n.name == "None" && n.payload.is_empty() && s.name == "Some" => {
                    match s.payload.first() {
                        Some(t) => write!(f, "Option<{}>", say(t)),
                        None => write!(f, "enum {{ None | Some }}"),
                    }
                }
                [e, o]
                    if e.name == "Err"
                        && o.name == "Ok"
                        && e.payload.len() == 1
                        && o.payload.len() == 1 =>
                {
                    write!(f, "Result<{}, {}>", say(&o.payload[0]), say(&e.payload[0]))
                }
                _ => {
                    let names: Vec<String> = vs.iter().map(|v| name(&v.name)).collect();
                    write!(f, "enum {{ {} }}", names.join(" | "))
                }
            },
            Type::App(n, args) => {
                let rendered: Vec<String> = args.iter().map(|a| say(a).to_string()).collect();
                write!(f, "{}<{}>", name(n), rendered.join(", "))
            }
            Type::Array(t) => write!(f, "Array<{}>", say(t)),
            Type::ArrayN(t, n) => write!(f, "Array<{}, {n}>", say(t)),
            Type::SmallArray(t, n) => write!(f, "SmallArray<{}, {n}>", say(t)),
            Type::ConstInt(n) => write!(f, "{n}"),
            Type::Map(k, v) => write!(f, "Map<{}, {}>", say(k), say(v)),
            Type::Stream(t) => write!(f, "Stream<{}>", say(t)),
            Type::Logger => write!(f, "Logger"),
            Type::Fn(params, ret) => {
                let ps: Vec<String> = params.iter().map(|p| say(p).to_string()).collect();
                write!(f, "fn({})", ps.join(", "))?;
                if **ret != Type::Unit {
                    write!(f, " -> {}", say(ret))?;
                }
                Ok(())
            }
            Type::Lazy(inner) => write!(f, "lazy {}", say(inner)),
            Type::Never => write!(f, "Never"),
            Type::Err => write!(f, "<type error>"),
        }
    }
}

/// Every declaration whose name two modules declare, and every declaration and
/// variant of an injected runtime module, keyed by its linked name: the name
/// its module wrote, and that module.
/// The loader fills it where it renames apart (`log` becomes `log__from1`, and
/// `Json` of `std/json` becomes `json$Json`).
/// A sentence reads it through a [`Speech`], so a refusal never names a
/// linked name.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct Spellings {
    /// Linked name -> (written name, module key).
    pub decls: std::collections::BTreeMap<String, (String, String)>,
    /// The path each module wrote to import another, keyed
    /// `(importer, imported)` by module key.
    pub paths: std::collections::BTreeMap<(String, String), String>,
    /// The root module's key, which a diagnostic's `None` file means.
    pub root: String,
}

impl Spellings {
    /// How a sentence in module `from` spells a declaration.
    pub fn speech<'a>(&'a self, from: &'a Option<String>) -> Speech<'a> {
        Speech {
            table: self,
            from,
            qualified: Vec::new(),
        }
    }

    /// The name the declaration `linked` was written as.
    pub fn written<'a>(&'a self, linked: &'a str) -> &'a str {
        self.decls.get(linked).map_or(linked, |(w, _)| w)
    }

    /// The linked name of a renamed declaration that `text` shows as a word:
    /// a sentence that names one bypassed its [`Speech`].
    pub fn linked_in(&self, text: &str) -> Option<&str> {
        let word = |c: char| c.is_alphanumeric() || c == '_';
        let renamed = self.decls.iter().filter(|(l, (w, _))| *l != w);
        renamed.map(|(l, _)| l.as_str()).find(|l| {
            (text.match_indices(l))
                .any(|(i, _)| !text[..i].ends_with(word) && !text[i + l.len()..].starts_with(word))
        })
    }
}

/// How one sentence spells declarations. A declaration reads as its module
/// wrote it. Where the sentence shows two declarations of one spelling, each
/// one declared outside the sentence's module adds the path the sentence's
/// module imports it by (`Cfg from "./lib/cfg"`), else its module key.
pub struct Speech<'a> {
    table: &'a Spellings,
    from: &'a Option<String>,
    qualified: Vec<String>,
}

impl<'a> Speech<'a> {
    /// The speech for one sentence that shows `types` and the declarations
    /// `names`.
    pub fn sentence(&self, types: &[&Type], names: &[&str]) -> Speech<'a> {
        let mut shown: Vec<String> = names.iter().map(|n| n.to_string()).collect();
        for t in types {
            crate::loader::type_heads(t, &mut |n| shown.push(n.clone()));
        }
        shown.sort();
        shown.dedup();
        let table = self.table;
        let qualified = (shown.iter())
            .filter(|n| (shown.iter()).any(|m| m != *n && table.written(m) == table.written(n)))
            .cloned()
            .collect();
        Speech { qualified, ..*self }
    }

    /// The declaration `linked` as this sentence spells it.
    pub fn name(&self, linked: &str) -> String {
        let written = self.table.written(linked);
        let from = self.from.as_ref().unwrap_or(&self.table.root);
        match self.table.decls.get(linked) {
            Some((_, m)) if m != from && self.qualified.iter().any(|q| q == linked) => {
                let path = self.table.paths.get(&(from.clone(), m.clone()));
                format!("{written} from \"{}\"", path.unwrap_or(m))
            }
            _ => written.to_string(),
        }
    }

    /// `ty` as this sentence spells it.
    pub fn ty<'b>(&'b self, ty: &'b Type) -> Said<'b> {
        Said(ty, Some(self))
    }

    /// The `types` and the declarations `names` of one sentence, spelled.
    pub fn say<const T: usize, const N: usize>(
        &self,
        types: [&Type; T],
        names: [&str; N],
    ) -> ([String; T], [String; N]) {
        let s = self.sentence(&types, &names);
        (types.map(|t| s.ty(t).to_string()), names.map(|n| s.name(n)))
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct Block {
    pub stmts: Vec<Stmt>,
    pub id: Id,
}

/// A statement. `if` also has an expression form, [`Expr::IfExpr`]; `match` is
/// an [`Expr::Match`] whose position the checker reads.
#[derive(Debug, Clone, PartialEq)]
pub enum Stmt {
    /// `let [mut] name [: Type] = value`.
    Let {
        name: String,
        mutable: bool,
        ty: Option<Type>,
        value: Expr,
        line: usize,
        /// See [`Binder::col`].
        col: usize,
        id: Id,
    },
    /// Legal only for a `mut` binding.
    Assign {
        name: String,
        value: Expr,
        line: usize,
        id: Id,
    },
    /// `name.field = value` on a `mut` record binding.
    SetField {
        name: String,
        field: String,
        value: Expr,
        line: usize,
        id: Id,
    },
    /// `name[index] = value` on a `mut` array binding; the read `a[i]` is
    /// `@at(a, i)`.
    IndexSet {
        name: String,
        index: Expr,
        value: Expr,
        line: usize,
        id: Id,
    },
    /// `return [expr]`.
    Return {
        value: Option<Expr>,
        line: usize,
        id: Id,
    },
    /// Exits the innermost loop. Unlabeled; the checker refuses it outside a
    /// loop.
    Break { line: usize, id: Id },
    /// Skips to the innermost loop's next iteration. Unlabeled; the checker
    /// refuses it outside a loop.
    Continue { line: usize, id: Id },
    /// `if cond { .. } [else { .. }]`.
    If {
        cond: Expr,
        then_block: Block,
        else_block: Option<Block>,
        line: usize,
        id: Id,
    },
    /// `while cond { .. }`.
    While {
        cond: Expr,
        body: Block,
        line: usize,
        id: Id,
    },
    /// `for var in iter { .. }` over an array; `var` is immutable and scoped to
    /// the body.
    ForIn {
        var: String,
        /// See [`Binder::col`].
        col: usize,
        iter: Expr,
        body: Block,
        line: usize,
        /// `for x in consume xs`: each `x` is an owned element, and the container
        /// is dead after the loop. A loop over a temporary,
        /// such as `for o in diff(..)`, consumes without the word.
        consuming: bool,
        id: Id,
    },
    /// `drop name`: frees a heap value the compiler cannot prove dead and
    /// consumes the binding.
    Drop { name: String, line: usize, id: Id },
    /// An expression evaluated for its effects.
    Expr(Expr, Id),
    /// `region { .. }`: an arena scope. Its allocations are freed when the block
    /// exits, and the checker refuses a value that escapes it.
    Region { body: Block, line: usize, id: Id },
}

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
    /// `s =~ "pattern"`: a full regular-expression match. The pattern is a
    /// string literal, compiled to a DFA at compile time.
    Match,
    // Operands share one integer type. `Shr` is arithmetic on a signed operand
    // and logical on an unsigned one. An out-of-range shift traps, or is a
    // compile error for a constant amount.
    BitAnd, // &
    BitOr,  // |
    BitXor, // ^
    Shl,    // <<
    Shr,    // >>
}

#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnOp {
    Neg,
    Not,
    /// `~`, within the operand's width.
    BitNot,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Expr {
    Int(i64, Id),
    /// A byte literal `'c'`: an integer literal the checker defaults to `UInt8`.
    /// Backends treat it as [`Expr::Int`].
    Byte(u8, Id),
    Float(f64, Id),
    Bool(bool, Id),
    /// Already decoded.
    Str(String, Id),
    Var {
        name: String,
        line: usize,
        id: Id,
    },
    Unary {
        op: UnOp,
        expr: Box<Expr>,
        line: usize,
        id: Id,
    },
    Binary {
        op: BinOp,
        lhs: Box<Expr>,
        rhs: Box<Expr>,
        line: usize,
        id: Id,
    },
    /// A call. `None` is an [`Expr::Var`].
    Call {
        name: String,
        args: Vec<Expr>,
        /// Written `recv.name(..)`: `args[0]` is the receiver, and a refusal
        /// numbers the arguments after it (#577).
        dot: bool,
        /// Explicit type arguments, as in `fromJson<Shape>(s)`; usually empty.
        /// The checker seeds the solve with them and infers the rest, so a
        /// partial list is legal and an over-long one is refused. Backends read
        /// them only where the arguments cannot answer.
        type_args: Vec<Type>,
        line: usize,
        id: Id,
    },
    /// An arm is a single expression, or a block when `stmt_pos` holds.
    Match {
        scrutinee: Box<Expr>,
        arms: Vec<MatchArm>,
        /// Set by `Stmt::Expr` and the `if let` desugar: the `match` stands
        /// directly in statement position, where a block arm is legal. Every
        /// other synthesized `match` is false.
        stmt_pos: bool,
        line: usize,
        id: Id,
    },
    /// `if` in expression position; each branch is one expression, and an
    /// `else if` is a nested `IfExpr`. `else_branch` is `None` only when the
    /// source omits `else`, which the checker refuses, so backends may assume
    /// `Some`.
    IfExpr {
        cond: Box<Expr>,
        then_branch: Box<Expr>,
        else_branch: Option<Box<Expr>>,
        line: usize,
        id: Id,
    },
    /// `expr?`: unwraps an `Option` or `Result`, or returns its `None` or `Err`
    /// from the enclosing function.
    Try {
        expr: Box<Expr>,
        line: usize,
        id: Id,
    },
    /// `User { name: 1, age: 30 }`.
    StructLit {
        name: String,
        fields: Vec<(String, Expr)>,
        line: usize,
        id: Id,
    },
    Field {
        expr: Box<Expr>,
        field: String,
        line: usize,
        id: Id,
    },
    /// `Age?(n)`: yields `None` when the refinement fails instead of aborting.
    TryConstruct {
        name: String,
        args: Vec<Expr>,
        line: usize,
        id: Id,
    },
    /// `[a, b, c]`, typed `Array<T, N>`.
    ArrayLit {
        elems: Vec<Expr>,
        line: usize,
        id: Id,
    },
    /// `[:]` or `["a": 1, "b": 2]`, entries in written order. The value type
    /// comes from the expected `Map` type.
    MapLit {
        entries: Vec<(Expr, Expr)>,
        line: usize,
        id: Id,
    },
    /// `x -> expr`, `(x, y) -> expr` or `x -> { block }`. The parameter types
    /// come from the expected `fn` type; outer locals are captured by read.
    Lambda {
        params: Vec<Binder>,
        body: LambdaBody,
        line: usize,
        /// The column of the first token. With `line` it keys the lifted
        /// function, so two lambdas on one line are two functions (#459).
        col: usize,
        id: Id,
    },
    /// `consume place`: moves the value out; the place is dead from here.
    /// `movecheck` refuses a `place` that is not a `Var` or a
    /// `Field` chain. Backends lower it as its operand, without a copy.
    Consume {
        place: Box<Expr>,
        line: usize,
        id: Id,
    },
}

/// A lambda's body: one expression, or a block that uses `return`.
#[derive(Debug, Clone, PartialEq)]
pub enum LambdaBody {
    Expr(Box<Expr>),
    Block(Block),
}

/// A match arm's body: one expression, or a block that yields nothing. The
/// checker allows a block in statement position only.
#[derive(Debug, Clone, PartialEq)]
pub enum ArmBody {
    Expr(Expr),
    Block(Block),
}

impl ArmBody {
    /// Returns the body's expression, or `None` for a block body.
    pub fn as_expr(&self) -> Option<&Expr> {
        match self {
            ArmBody::Expr(e) => Some(e),
            ArmBody::Block(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub struct MatchArm {
    pub pattern: Pattern,
    pub body: ArmBody,
}

/// A pattern in a `match` arm. Only [`Pattern::Variant`] can be written; a
/// desugar builds the others.
#[derive(Debug, Clone, PartialEq)]
pub enum Pattern {
    /// `Circle(r)`, `Empty`, `Some(x)`, `None`, `Ok(x)`, `Err(e)`. The scrutinee
    /// decides what the name means.
    Variant(String, Vec<Binder>),
    /// The tag-1 arm: `Some` or `Ok`. The parser's `??` desugar has no types to
    /// choose with, so it builds this and `??` inherits `match`'s drops,
    /// ownership and validation.
    Success(Binder),
    /// The tag-0 arm: `None` or `Err`. The binder takes a `Result`'s error; on
    /// an `Option` the checker binds nothing.
    Failure(Binder),
    /// Matches any value and binds nothing. The refutable-`let` and `if let`
    /// desugars place it last, because the parser cannot see the enum's
    /// variants.
    Other,
}

impl Pattern {
    /// Returns the names the pattern binds, in order.
    ///
    /// A desugar's binder is `@`-prefixed, so a reader that indexes source
    /// positions must skip it.
    pub fn bindings(&self) -> Vec<&str> {
        self.binders()
            .into_iter()
            .map(|b| b.name.as_str())
            .collect()
    }

    pub fn binders(&self) -> Vec<&Binder> {
        match self {
            Pattern::Success(b) | Pattern::Failure(b) => vec![b],
            Pattern::Variant(_, binds) => binds.iter().collect(),
            Pattern::Other => Vec::new(),
        }
    }

    pub fn binders_mut(&mut self) -> Vec<&mut Binder> {
        match self {
            Pattern::Success(b) | Pattern::Failure(b) => vec![b],
            Pattern::Variant(_, binds) => binds.iter_mut().collect(),
            Pattern::Other => Vec::new(),
        }
    }
}

macro_rules! slot {
    ($e:expr, $($ref_:tt)+) => {
        match $e {
            Expr::Int(_, id)
            | Expr::Byte(_, id)
            | Expr::Float(_, id)
            | Expr::Bool(_, id)
            | Expr::Str(_, id)
            | Expr::Var { id, .. }
            | Expr::Unary { id, .. }
            | Expr::Binary { id, .. }
            | Expr::Call { id, .. }
            | Expr::Match { id, .. }
            | Expr::IfExpr { id, .. }
            | Expr::Try { id, .. }
            | Expr::StructLit { id, .. }
            | Expr::Field { id, .. }
            | Expr::TryConstruct { id, .. }
            | Expr::ArrayLit { id, .. }
            | Expr::MapLit { id, .. }
            | Expr::Lambda { id, .. }
            | Expr::Consume { id, .. } => $($ref_)+ *id,
        }
    };
}

macro_rules! stmt_slot {
    ($s:expr, $($ref_:tt)+) => {
        match $s {
            Stmt::Let { id, .. }
            | Stmt::Assign { id, .. }
            | Stmt::SetField { id, .. }
            | Stmt::IndexSet { id, .. }
            | Stmt::Return { id, .. }
            | Stmt::Break { id, .. }
            | Stmt::Continue { id, .. }
            | Stmt::If { id, .. }
            | Stmt::While { id, .. }
            | Stmt::ForIn { id, .. }
            | Stmt::Drop { id, .. }
            | Stmt::Expr(_, id)
            | Stmt::Region { id, .. } => $($ref_)+ *id,
        }
    };
}

impl Expr {
    pub fn id(&self) -> NodeId {
        slot!(self, &).0
    }

    /// The pattern, scrutinee and two blocks of a parsed `if let`: a statement
    /// `match` whose last arm is [`Pattern::Other`], which no source writes.
    pub fn as_if_let(&self) -> Option<(&Pattern, &Expr, &Block, &Block)> {
        let Expr::Match {
            scrutinee,
            arms,
            stmt_pos: true,
            ..
        } = self
        else {
            return None;
        };
        match &arms[..] {
            [MatchArm {
                pattern,
                body: ArmBody::Block(then),
            }, MatchArm {
                pattern: Pattern::Other,
                body: ArmBody::Block(els),
            }] => Some((pattern, scrutinee, then, els)),
            _ => None,
        }
    }
}

impl Stmt {
    pub fn id(&self) -> NodeId {
        stmt_slot!(self, &).0
    }
}

impl Block {
    pub fn id(&self) -> NodeId {
        self.id.0
    }
}

impl Param {
    pub fn id(&self) -> NodeId {
        self.id.0
    }
}

impl Binder {
    pub fn id(&self) -> NodeId {
        self.id.0
    }
}

impl Program {
    /// The id of `body`: the functions, then the projections, tests,
    /// benches, module-state initializers and type-declaration checks, each
    /// in its list's order. The one numbering of source bodies; the lowering
    /// and the World take it.
    pub fn source_id(&self, body: SourceBody) -> FnId {
        let fns = self.functions.len();
        let places = fns + crate::project::all(self).count();
        let tests = places + self.tests.len();
        let benches = tests + self.benches.len();
        let globals = benches + self.globals.len();
        let (from, i) = match body {
            SourceBody::Fn(i) => (0, i),
            SourceBody::Place(i) => (fns, i),
            SourceBody::Test(i) => (places, i),
            SourceBody::Bench(i) => (tests, i),
            SourceBody::Global(i) => (benches, i),
            SourceBody::TypeDecl(i) => (globals, i),
        };
        FnId::nth(from + i as usize)
    }

    /// The name each [`Program::source_id`] is emitted and looked up under,
    /// in id order: a function's or projection's own, `test@i`, `bench@i`,
    /// `global@i` and `type@i`.
    pub fn source_names(&self) -> Vec<String> {
        let own = self.functions.iter().map(|f| f.name.clone());
        let places = crate::project::all(self).map(|(_, f)| f.name.clone());
        let nth = |kind: &'static str, n: usize| (0..n).map(move |i| format!("{kind}@{i}"));
        (own.chain(places))
            .chain(nth("test", self.tests.len()))
            .chain(nth("bench", self.benches.len()))
            .chain(nth("global", self.globals.len()))
            .chain(nth("type", self.type_decls.len()))
            .collect()
    }

    /// Numbers every node of the program, overwriting any id it held. The
    /// parser and the loader call it once the tree is whole; a side table
    /// built before it is stale.
    pub fn number(&mut self) {
        let mut units = 0;
        let mut unit = || {
            units += 1;
            Numbering::unit(units - 1)
        };
        for t in &mut self.type_decls {
            let mut n = unit();
            if let Some(p) = &mut t.predicate {
                n.expr(p);
            }
        }
        for f in &mut self.functions {
            unit().function(f);
        }
        for i in &mut self.impls {
            for f in i.methods.iter_mut().chain(&mut i.places) {
                unit().function(f);
            }
        }
        for g in &mut self.globals {
            unit().expr(&mut g.init);
        }
        for m in self.contracts.iter_mut().flat_map(|c| &mut c.members) {
            let mut n = unit();
            if let ContractMemberKind::Value {
                default: Some(d), ..
            }
            | ContractMemberKind::Fn {
                default: Some(d), ..
            } = &mut m.kind
            {
                n.expr(d);
            }
        }
        for b in self.tests.iter_mut().chain(&mut self.benches) {
            unit().block(&mut b.body);
        }
        self.units = units;
    }

    /// Numbers `functions[from..]` as units after [`Program::units`], for a
    /// synthesis that appends functions to a numbered program.
    pub fn number_appended(&mut self, from: usize) {
        for f in &mut self.functions[from..] {
            Numbering::unit(self.units).function(f);
            self.units += 1;
        }
    }
}

/// The walk behind [`Program::number`]: numbers the nodes of one unit.
pub struct Numbering(NodeId);

impl Numbering {
    /// Starts unit `unit`, whose first node gets index 1.
    pub fn unit(unit: u32) -> Numbering {
        Numbering::resume(unit, 0)
    }

    /// Continues unit `unit` after its first `used` nodes.
    pub fn resume(unit: u32, used: u32) -> Numbering {
        Numbering(NodeId { unit, local: used })
    }

    /// How many nodes the unit holds so far.
    pub fn used(&self) -> u32 {
        self.0.local
    }

    fn next(&mut self, slot: &mut Id) {
        self.0.local += 1;
        *slot = Id(self.0);
    }

    pub fn function(&mut self, f: &mut Function) {
        for p in &mut f.params {
            self.next(&mut p.id);
        }
        self.block(&mut f.body);
    }

    fn binders(&mut self, bs: Vec<&mut Binder>) {
        for b in bs {
            self.next(&mut b.id);
        }
    }

    pub fn block(&mut self, b: &mut Block) {
        self.next(&mut b.id);
        for s in &mut b.stmts {
            self.stmt(s);
        }
    }

    pub fn stmt(&mut self, s: &mut Stmt) {
        self.next(stmt_slot!(s, &mut));
        match s {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::SetField { value, .. } => {
                self.expr(value)
            }
            Stmt::IndexSet { index, value, .. } => {
                self.expr(index);
                self.expr(value);
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    self.expr(v);
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Drop { .. } => {}
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                self.expr(cond);
                self.block(then_block);
                if let Some(b) = else_block {
                    self.block(b);
                }
            }
            Stmt::While { cond, body, .. } => {
                self.expr(cond);
                self.block(body);
            }
            Stmt::ForIn { iter, body, .. } => {
                self.expr(iter);
                self.block(body);
            }
            Stmt::Expr(e, _) => self.expr(e),
            Stmt::Region { body, .. } => self.block(body),
        }
    }

    pub fn expr(&mut self, e: &mut Expr) {
        self.next(slot!(e, &mut));
        match e {
            Expr::Int(..)
            | Expr::Byte(..)
            | Expr::Float(..)
            | Expr::Bool(..)
            | Expr::Str(..)
            | Expr::Var { .. } => {}
            Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
                self.expr(expr)
            }
            Expr::Consume { place, .. } => self.expr(place),
            Expr::Binary { lhs, rhs, .. } => {
                self.expr(lhs);
                self.expr(rhs);
            }
            Expr::Call { args, .. }
            | Expr::TryConstruct { args, .. }
            | Expr::ArrayLit { elems: args, .. } => {
                for a in args {
                    self.expr(a);
                }
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                self.expr(scrutinee);
                for a in arms {
                    self.binders(a.pattern.binders_mut());
                    match &mut a.body {
                        ArmBody::Expr(x) => self.expr(x),
                        ArmBody::Block(b) => self.block(b),
                    }
                }
            }
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                self.expr(cond);
                self.expr(then_branch);
                if let Some(x) = else_branch {
                    self.expr(x);
                }
            }
            Expr::StructLit { fields, .. } => {
                for (_, x) in fields {
                    self.expr(x);
                }
            }
            Expr::MapLit { entries, .. } => {
                for (k, v) in entries {
                    self.expr(k);
                    self.expr(v);
                }
            }
            Expr::Lambda { params, body, .. } => {
                self.binders(params.iter_mut().collect());
                match body {
                    LambdaBody::Expr(x) => self.expr(x),
                    LambdaBody::Block(b) => self.block(b),
                }
            }
        }
    }
}

impl Stmt {
    /// Returns the line the statement starts on. `Stmt::Expr` over a literal
    /// answers 0, as [`Expr::line`] does.
    pub fn line(&self) -> usize {
        match self {
            Stmt::Let { line, .. }
            | Stmt::Assign { line, .. }
            | Stmt::SetField { line, .. }
            | Stmt::IndexSet { line, .. }
            | Stmt::Return { line, .. }
            | Stmt::Break { line, id: _ }
            | Stmt::Continue { line, id: _ }
            | Stmt::If { line, .. }
            | Stmt::While { line, .. }
            | Stmt::ForIn { line, .. }
            | Stmt::Drop { line, .. }
            | Stmt::Region { line, .. } => *line,
            Stmt::Expr(e, _) => e.line(),
        }
    }
}

impl Expr {
    /// Returns the line the expression starts on, or 0 for a literal, which
    /// carries no line.
    pub fn line(&self) -> usize {
        match self {
            Expr::Int(_, _)
            | Expr::Byte(_, _)
            | Expr::Float(_, _)
            | Expr::Bool(_, _)
            | Expr::Str(_, _) => 0,
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
            | Expr::Consume { line, .. }
            | Expr::Lambda { line, .. } => *line,
        }
    }
}

/// Defines a visitor trait and a walk over every statement and expression of a
/// `Block`, in source order, with the local names in scope at each one. It is
/// the one descent the workspace makes over a body.
///
/// The walk owns the descent and the scope stack; a reader writes only its
/// hooks. It is a macro because some readers hold a shared borrow and some a
/// unique one: the plain form carries the body's lifetime so a reader may keep
/// what it is handed, and the `mut` form assigns through a unique borrow.
///
/// * `stmt` and `expr` see a node before its children; `expr` returns `false`
///   to skip them.
/// * `after_expr` sees an expression after its children, so a substituted
///   expression is never walked again. It does not fire when `expr` skipped.
/// * `arm_pattern` sees an arm's pattern before its bindings join the scope.
/// * `SCOPED` is `false` for a reader that never reads `locals`; the walk then
///   binds no name.
#[macro_export]
macro_rules! body_scope_descent {
    ($visit:ident, $blk:ident, $st:ident, $ex:ident) => {
        $crate::body_scope_descent!(@walk $visit, $blk, $st, $ex, ('a), ());
    };
    ($visit:ident, $blk:ident, $st:ident, $ex:ident, mut) => {
        $crate::body_scope_descent!(@walk $visit, $blk, $st, $ex, (), (mut));
    };
    (@walk $visit:ident, $blk:ident, $st:ident, $ex:ident, ($($lt:lifetime)?), ($($mut_:tt)?)) => {
        trait $visit$(<$lt>)? {
            const SCOPED: bool = true;

            /// Sees a statement before its children. A `let`'s own name joins
            /// `locals` after it.
            fn stmt(
                &mut self,
                s: &$($lt)? $($mut_)? $crate::ast::Stmt,
                locals: &::std::collections::HashSet<String>,
            ) {
                let _ = (s, locals);
            }

            /// Sees a statement after its children.
            fn after_stmt(
                &mut self,
                s: &$($lt)? $($mut_)? $crate::ast::Stmt,
                locals: &::std::collections::HashSet<String>,
            ) {
                let _ = (s, locals);
            }

            /// Sees an expression before its children; `false` skips them.
            fn expr(
                &mut self,
                e: &$($lt)? $($mut_)? $crate::ast::Expr,
                locals: &::std::collections::HashSet<String>,
            ) -> bool {
                let _ = (e, locals);
                true
            }

            /// Sees an expression after its children; not called when `expr`
            /// skipped them.
            fn after_expr(
                &mut self,
                e: &$($lt)? $($mut_)? $crate::ast::Expr,
                locals: &::std::collections::HashSet<String>,
            ) {
                let _ = (e, locals);
            }

            /// Sees each name the body binds, at its spelled position; a
            /// desugar's binder has column 0. `declared` is `Some` only for an
            /// annotated `let`.
            fn bind(
                &mut self,
                name: &str,
                line: usize,
                col: usize,
                kind: $crate::ast::LocalKind,
                declared: Option<&$crate::ast::Type>,
            ) {
                let _ = (name, line, col, kind, declared);
            }

            /// Sees an arm's pattern, at the `match`'s line, before the arm's
            /// bindings join the scope.
            fn arm_pattern(
                &mut self,
                p: &$($lt)? $($mut_)? $crate::ast::Pattern,
                line: usize,
                locals: &::std::collections::HashSet<String>,
            ) {
                let _ = (p, line, locals);
            }
        }

        fn $blk<$($lt,)? V: $visit$(<$lt>)? + ?Sized>(
            b: &$($lt)? $($mut_)? $crate::ast::Block,
            locals: &mut ::std::collections::HashSet<String>,
            v: &mut V,
        ) {
            for s in &$($mut_)? b.stmts {
                $st(s, locals, v);
            }
        }

        fn $st<$($lt,)? V: $visit$(<$lt>)? + ?Sized>(
            s: &$($lt)? $($mut_)? $crate::ast::Stmt,
            locals: &mut ::std::collections::HashSet<String>,
            v: &mut V,
        ) {
            use $crate::ast::Stmt;
            v.stmt(&$($mut_)? *s, locals);
            match s {
                Stmt::Let {
                    name,
                    value,
                    mutable,
                    ty,
                    line,
                    col,
                    ..
                } => {
                    $ex(value, locals, v);
                    v.bind(
                        name.as_str(),
                        *line,
                        *col,
                        $crate::ast::LocalKind::Let { mutable: *mutable },
                        ty.as_ref(),
                    );
                    // Shadows a like-named declaration from here on.
                    if V::SCOPED {
                        locals.insert(name.clone());
                    }
                }
                Stmt::Assign { value, .. } | Stmt::SetField { value, .. } => $ex(value, locals, v),
                Stmt::IndexSet { index, value, .. } => {
                    $ex(index, locals, v);
                    $ex(value, locals, v);
                }
                Stmt::Return { value: Some(e), .. } => $ex(e, locals, v),
                Stmt::Return { value: None, .. } => {}
                Stmt::If {
                    cond,
                    then_block,
                    else_block,
                    ..
                } => {
                    $ex(cond, locals, v);
                    let mut inner = locals.clone();
                    $blk(then_block, &mut inner, v);
                    if let Some(eb) = else_block {
                        let mut inner2 = locals.clone();
                        $blk(eb, &mut inner2, v);
                    }
                }
                Stmt::While { cond, body, .. } => {
                    $ex(cond, locals, v);
                    let mut inner = locals.clone();
                    $blk(body, &mut inner, v);
                }
                Stmt::ForIn {
                    var,
                    iter,
                    body,
                    line,
                    col,
                    ..
                } => {
                    $ex(iter, locals, v);
                    v.bind(var.as_str(), *line, *col, $crate::ast::LocalKind::ForVar, None);
                    let mut inner = locals.clone();
                    if V::SCOPED {
                        inner.insert(var.clone());
                    }
                    $blk(body, &mut inner, v);
                }
                Stmt::Drop { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => {}
                Stmt::Expr(e, _) => $ex(e, locals, v),
                Stmt::Region { body, .. } => {
                    let mut inner = locals.clone();
                    $blk(body, &mut inner, v);
                }
            }
            v.after_stmt(&$($mut_)? *s, locals);
        }

        fn $ex<$($lt,)? V: $visit$(<$lt>)? + ?Sized>(
            e: &$($lt)? $($mut_)? $crate::ast::Expr,
            locals: &::std::collections::HashSet<String>,
            v: &mut V,
        ) {
            use $crate::ast::{ArmBody, Expr, LambdaBody};
            if !v.expr(&$($mut_)? *e, locals) {
                return;
            }
            match e {
                // A call's args are walked whatever the visitor made of the
                // callee, including one the namespace pass removed.
                Expr::Call { args, .. }
                | Expr::TryConstruct { args, .. }
                | Expr::ArrayLit { elems: args, .. } => {
                    for a in args {
                        $ex(a, locals, v);
                    }
                }
                Expr::StructLit { fields, .. } => {
                    for (_, val) in fields {
                        $ex(val, locals, v);
                    }
                }
                Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
                    $ex(expr, locals, v)
                }
                Expr::Consume { place, .. } => $ex(place, locals, v),
                Expr::Binary { lhs, rhs, .. } => {
                    $ex(lhs, locals, v);
                    $ex(rhs, locals, v);
                }
                Expr::Match {
                    scrutinee,
                    arms,
                    line,
                    ..
                } => {
                    let l = *line;
                    $ex(scrutinee, locals, v);
                    for arm in arms {
                        let mut inner = locals.clone();
                        v.arm_pattern(&$($mut_)? arm.pattern, l, &inner);
                        for b in arm.pattern.binders() {
                            v.bind(
                                b.name.as_str(),
                                b.line,
                                b.col,
                                $crate::ast::LocalKind::Let { mutable: false },
                                None,
                            );
                        }
                        if V::SCOPED {
                            for b in arm.pattern.bindings() {
                                inner.insert(b.to_string());
                            }
                        }
                        match &$($mut_)? arm.body {
                            ArmBody::Expr(e2) => $ex(e2, &inner, v),
                            ArmBody::Block(b2) => $blk(b2, &mut inner, v),
                        }
                    }
                }
                Expr::IfExpr {
                    cond,
                    then_branch,
                    else_branch,
                    ..
                } => {
                    $ex(cond, locals, v);
                    $ex(then_branch, locals, v);
                    if let Some(eb) = else_branch {
                        $ex(eb, locals, v);
                    }
                }
                Expr::MapLit { entries, .. } => {
                    for (k, val) in entries {
                        $ex(k, locals, v);
                        $ex(val, locals, v);
                    }
                }
                // A lambda's params shadow a declaration as a `let` does.
                Expr::Lambda { params, body, .. } => {
                    for p in params.iter() {
                        v.bind(
                            p.name.as_str(),
                            p.line,
                            p.col,
                            $crate::ast::LocalKind::Let { mutable: false },
                            None,
                        );
                    }
                    let mut inner = locals.clone();
                    if V::SCOPED {
                        for p in params.iter() {
                            inner.insert(p.name.clone());
                        }
                    }
                    match body {
                        LambdaBody::Expr(e2) => $ex(e2, &inner, v),
                        LambdaBody::Block(b2) => $blk(b2, &mut inner, v),
                    }
                }
                Expr::Var { .. }
                | Expr::Int(_, _)
                | Expr::Byte(_, _)
                | Expr::Float(_, _)
                | Expr::Bool(_, _)
                | Expr::Str(_, _) => {}
            }
            v.after_expr(&$($mut_)? *e, locals);
        }
    };
}

crate::body_scope_descent!(AstVisit, ast_block, ast_stmt, ast_expr);

/// Returns every lambda literal in function bodies and global initializers, by
/// node, with the name of the function that holds it ("" for module state).
///
/// A backend walk has erased the program's lifetime; this gives it a borrow a
/// worklist can hold, without copying the body.
pub fn lambdas<'a>(p: &'a Program) -> std::collections::HashMap<NodeId, (&'a str, &'a Expr)> {
    struct Lambdas<'a>(
        &'a str,
        std::collections::HashMap<NodeId, (&'a str, &'a Expr)>,
    );
    impl<'a> AstVisit<'a> for Lambdas<'a> {
        const SCOPED: bool = false;
        fn expr(&mut self, e: &'a Expr, _: &std::collections::HashSet<String>) -> bool {
            if let Expr::Lambda { .. } = e {
                self.1.insert(e.id(), (self.0, e));
            }
            true
        }
    }
    let mut v = Lambdas("", std::collections::HashMap::new());
    let mut locals = std::collections::HashSet::new();
    for f in &p.functions {
        v.0 = &f.name;
        ast_block(&f.body, &mut locals, &mut v);
    }
    v.0 = "";
    for g in &p.globals {
        ast_expr(&g.init, &locals, &mut v);
    }
    v.1
}

struct Ids<'o>(&'o mut Vec<NodeId>);

impl AstVisit<'_> for Ids<'_> {
    const SCOPED: bool = false;

    fn stmt(&mut self, s: &Stmt, _: &std::collections::HashSet<String>) {
        self.0.push(s.id());
    }

    fn expr(&mut self, e: &Expr, _: &std::collections::HashSet<String>) -> bool {
        self.0.push(e.id());
        true
    }
}

/// Returns whether `b` calls, reads, stores into or drops one of `names` where
/// no parameter or earlier binding shadows it.
pub fn names_any(b: &Block, params: &[Param], names: &std::collections::HashSet<String>) -> bool {
    let mut v = Names(names, false);
    ast_block(
        b,
        &mut params.iter().map(|p| p.name.clone()).collect(),
        &mut v,
    );
    v.1
}

/// [`names_any`] for a module-state initializer, which binds nothing around it.
pub fn expr_names_any(e: &Expr, names: &std::collections::HashSet<String>) -> bool {
    let mut v = Names(names, false);
    ast_expr(e, &std::collections::HashSet::new(), &mut v);
    v.1
}

struct Names<'n>(&'n std::collections::HashSet<String>, bool);

impl AstVisit<'_> for Names<'_> {
    fn stmt(&mut self, s: &Stmt, locals: &std::collections::HashSet<String>) {
        if let Stmt::Assign { name, .. }
        | Stmt::SetField { name, .. }
        | Stmt::IndexSet { name, .. }
        | Stmt::Drop { name, .. } = s
        {
            self.1 |= self.0.contains(name) && !locals.contains(name);
        }
    }

    fn expr(&mut self, e: &Expr, locals: &std::collections::HashSet<String>) -> bool {
        if let Expr::Var { name, .. } | Expr::Call { name, .. } = e {
            self.1 |= self.0.contains(name) && !locals.contains(name);
        }
        true
    }
}

struct Exprs<'o>(&'o mut dyn FnMut(&Expr, &std::collections::HashSet<String>));

impl AstVisit<'_> for Exprs<'_> {
    fn expr(&mut self, e: &Expr, locals: &std::collections::HashSet<String>) -> bool {
        (self.0)(e, locals);
        true
    }
}

/// Calls `f` on every expression in `s`, with the names `s` itself binds
/// where the expression stands.
pub fn exprs_one(s: &Stmt, f: &mut dyn FnMut(&Expr, &std::collections::HashSet<String>)) {
    ast_stmt(s, &mut std::collections::HashSet::new(), &mut Exprs(f));
}

/// Appends the id of every statement and expression in `e`.
pub fn node_ids(e: &Expr, out: &mut Vec<NodeId>) {
    ast_expr(e, &std::collections::HashSet::new(), &mut Ids(out));
}

// Places and mentions: questions about the shape of the AST, not rules.

/// Returns the root of a place path: `r.a[0]` gives `r`. A message names a path
/// and a borrow names its parameter, so they compare only at the root.
pub fn root_of(path: &str) -> &str {
    match path.find(['.', '[']) {
        Some(i) => &path[..i],
        None => path,
    }
}

/// Returns whether `e` reads the place `base` or a name derived from it.
///
/// A store releases what the place held unless the new value names the place:
/// `acc = acc + x` reads the old buffer. Derived names count
/// because a place desugar's write-back `t.xs = t.xs[]` hands the same buffer
/// back (`placeorder.vyrn`).
///
/// A block-bodied lambda or a block match arm answers `true` unread: `true` can
/// cost a leak, `false` a use-after-free.
pub fn mentions_place(e: &Expr, base: &str) -> bool {
    struct Mentions<'a> {
        base: &'a str,
        found: bool,
    }

    impl Mentions<'_> {
        fn derived(&self, n: &str) -> bool {
            let base = self.base;
            n == base
                || (n.len() > base.len()
                    && n.starts_with(base)
                    && matches!(n.as_bytes()[base.len()], b'.' | b'['))
        }
    }

    impl AstVisit<'_> for Mentions<'_> {
        const SCOPED: bool = false;

        fn expr(&mut self, e: &Expr, _: &std::collections::HashSet<String>) -> bool {
            match e {
                Expr::Var { name, .. } if self.derived(name) => self.found = true,
                // Unread, for the reason the doc gives.
                Expr::Lambda {
                    body: LambdaBody::Block(_),
                    ..
                } => self.found = true,
                Expr::Match { arms, .. } if arms.iter().any(|a| a.body.as_expr().is_none()) => {
                    self.found = true
                }
                _ => {}
            }
            !self.found
        }
    }

    let mut v = Mentions { base, found: false };
    ast_expr(e, &std::collections::HashSet::new(), &mut v);
    v.found
}

/// Returns the blocks nested directly in a statement.
pub fn sub_blocks(s: &Stmt) -> Vec<&Block> {
    match s {
        Stmt::If {
            then_block,
            else_block,
            ..
        } => {
            let mut v = vec![then_block];
            v.extend(else_block.as_ref());
            v
        }
        Stmt::While { body, .. } | Stmt::ForIn { body, .. } | Stmt::Region { body, .. } => {
            vec![body]
        }
        Stmt::Expr(Expr::Match { arms, .. }, _) => arms
            .iter()
            .filter_map(|a| match &a.body {
                ArmBody::Block(b) => Some(b),
                ArmBody::Expr(_) => None,
            })
            .collect(),
        _ => Vec::new(),
    }
}

/// Returns whether the statement, nested blocks included, names the binding.
pub fn stmt_mentions(s: &Stmt, name: &str) -> bool {
    let here = match s {
        Stmt::Let { value, .. }
        | Stmt::Assign { value, .. }
        | Stmt::SetField { value, .. }
        | Stmt::Expr(value, _) => mentions(value, name),
        Stmt::IndexSet { index, value, .. } => mentions(index, name) || mentions(value, name),
        Stmt::If { cond: e, .. } | Stmt::While { cond: e, .. } => mentions(e, name),
        Stmt::ForIn { iter, .. } => mentions(iter, name),
        Stmt::Return { value, .. } => value.as_ref().is_some_and(|e| mentions(e, name)),
        Stmt::Drop { name: n, .. } => n == name,
        _ => false,
    };
    here || sub_blocks(s)
        .iter()
        .any(|b| b.stmts.iter().any(|s| stmt_mentions(s, name)))
}

/// Returns whether some path through `e` names the binding. A lambda body
/// counts though it may never run: narrowing it would widen what compiles.
pub fn mentions(e: &Expr, name: &str) -> bool {
    let block = |b: &Block| b.stmts.iter().any(|s| stmt_mentions(s, name));
    match e {
        Expr::Var { name: n, .. } => n == name,
        Expr::Int(_, _)
        | Expr::Byte(_, _)
        | Expr::Float(_, _)
        | Expr::Bool(_, _)
        | Expr::Str(_, _) => false,
        Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
            mentions(expr, name)
        }
        Expr::Consume { place, .. } => mentions(place, name),
        Expr::Binary { lhs, rhs, .. } => mentions(lhs, name) || mentions(rhs, name),
        // `x.copy()` makes a fresh value: its receiver is read, never aliased.
        Expr::Call { name: n, args, .. }
            if n == "@copy" && args.first().is_some_and(|a| place_path(a).is_some()) =>
        {
            false
        }
        Expr::Call { args, .. }
        | Expr::TryConstruct { args, .. }
        | Expr::ArrayLit { elems: args, .. } => args.iter().any(|a| mentions(a, name)),
        Expr::MapLit { entries, .. } => entries
            .iter()
            .any(|(k, v)| mentions(k, name) || mentions(v, name)),
        Expr::StructLit { fields, .. } => fields.iter().any(|(_, v)| mentions(v, name)),
        Expr::Match {
            scrutinee, arms, ..
        } => {
            mentions(scrutinee, name)
                || arms.iter().any(|a| match &a.body {
                    ArmBody::Expr(e) => mentions(e, name),
                    ArmBody::Block(b) => block(b),
                })
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            mentions(cond, name)
                || mentions(then_branch, name)
                || else_branch.as_ref().is_some_and(|b| mentions(b, name))
        }
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(e) => mentions(e, name),
            LambdaBody::Block(b) => block(b),
        },
    }
}

/// Returns the place `e` reads as `(root, path)`: `r.a.b` gives
/// `("r", "r.a.b")`. A move takes the root; a diagnostic quotes the path. `None`
/// for a non-place, which has no earlier owner to move from.
pub fn place_path(e: &Expr) -> Option<(String, String)> {
    match e {
        Expr::Var { name, .. } => Some((name.clone(), name.clone())),
        Expr::Field { expr, field, .. } => {
            let (root, path) = place_path(expr)?;
            Some((root, format!("{path}.{field}")))
        }
        // A take (`consume d.title`) is not a place: the frame cannot
        // reach its storage, so it is an owner everywhere.
        _ => None,
    }
}
