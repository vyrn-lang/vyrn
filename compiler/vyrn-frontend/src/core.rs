//! The core's data: the names, places, values, rows and bodies every pass
//! between the checker and an emitter reads. The passes that build, judge
//! and emit them live in `vyrn-lower` and `vyrn-codegen`.

use crate::ast::{BinOp, Capability, NodeId, Type, UnOp};
use crate::own::{DropKind, EdgeRow, Exit, Linear};

pub mod check;

/// A name in a body: an index into [`Body::names`].
#[derive(Clone, Copy, PartialEq, Eq, Hash, PartialOrd, Ord)]
pub struct Name(pub u32);

impl Name {
    /// The name's slot in [`Body::names`] and in every table sized by it.
    pub fn index(self) -> usize {
        self.0 as usize
    }
}

/// Prints the bare index, so a `{:?}` dump of the core shows a name as a number.
impl std::fmt::Debug for Name {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        std::fmt::Debug::fmt(&self.0, f)
    }
}

#[derive(Debug, Clone)]
pub struct NameInfo {
    /// The source spelling, or `@tN` for a temporary the naming pass minted.
    pub source: String,
    pub ty: Type,
    /// Whether a held value of this name owes a release at an exit: its type
    /// owns heap or carries a must-use obligation, and the name is not a
    /// borrow. This differs from ownership: a value with no heap has no
    /// release and is still owned (`kernel::Kernel::releases`, `owned`).
    pub releases: bool,
    pub heap: bool,
    /// Whether the name is a borrow: its type owns heap,
    /// the body does not own it, and it is not static data. The kernel keeps
    /// what a borrow bound to a place reads, and refuses a take of it.
    pub borrow: bool,
    /// The borrow a `read` or `modify` parameter, or a second name for one,
    /// makes, in the checker's words (`movecheck::Borrow::what`). A borrow
    /// bound by a place read carries none: the kernel's alias table words it
    /// from the place. The kernel needs this to refuse a take of a parameter.
    pub borrow_kind: Option<BorrowKind>,
    pub line: usize,
    /// The node the plan keys this binding by (a `Stmt::Let`, a parameter).
    /// `None` for a temporary this pass minted.
    pub binding: Option<NodeId>,
    /// For the unnamed receiver of a field read (`parse(q).sels`): the
    /// `Expr::Field` node that keys the plan's receiver-free row. The placer
    /// frees the receiver right after the read, minus the field it took.
    pub receiver: Option<NodeId>,
    /// For the unnamed receiver of a heap field or element read the consumer
    /// borrows (`f(x).rhs.startsWith("{")`): the producing node, which keys
    /// the argument-temporary drop. Set only where the consumer is a call or
    /// an operator, the two sites the compiled backends drain temporaries at.
    pub producer: Option<NodeId>,
    /// For a call-argument temporary the caller releases after the call: the
    /// argument's node, the key of the plan's `arg_drops` row. The release is
    /// the `St::Drop` the binding after the call queues.
    pub arg_drop: Option<NodeId>,
    /// The holes the plan's release walk skips for this binding, spelled
    /// `.f.g`. A `Drop` of the name walks around exactly these; a placed row
    /// may carry its own.
    pub holes: Vec<String>,
    /// For a payload binder read out of its scrutinee ([`Arm::reads`]): what a
    /// `consume` handed the binder leaves in the scrutinee.
    pub payload: Option<Payload>,
    /// For a receiver ([`NameInfo::receiver`]): whether a callee allocated
    /// the block. A callee's block is malloc-side whatever `region` is open
    /// at the call site, so its free stands there; the `@`-spelled producers
    /// (`@concat`, `@str`, `@copy`) use the arena and stay region-gated.
    pub receiver_malloc: bool,
    /// A parameter of a must-use type carries the obligation into the
    /// callee, so a take of it is the callee's (`boxStream(s)` does not take
    /// the caller's value). This is the ownership half only;
    /// [`NameInfo::borrow_kind`] keeps the capability's words.
    pub must_use_param: bool,
    /// A String accumulator `vyrn_lower::append::append_candidates` admits:
    /// `s = s + e` on it is one `@strAppend` row ([`crate::prelude::Spec::Rebuilds`]). A read
    /// of a module-state accumulator
    /// (`vyrn_lower::append::global_append_candidates`) is that row's receiver
    /// for `g = g + e`.
    pub grows: bool,
    /// For a temporary holding a place read: the path the reader wrote
    /// (`p.name`, `xs[i]`), which a refusal about the temporary quotes. `None`
    /// for a name the program spells, whose `source` is the reader's.
    pub path: Option<String>,
    /// The temporary a `for x in consume xs` loop binds its container to. The
    /// core keeps no statement kinds, so the refusal's wording of that loop
    /// rides on the name.
    pub for_consume: bool,
    /// For a name a record literal binds: where each part goes, in the
    /// checker's words ("the field `R.s`"), one per field in order.
    /// `Rhs::Make` carries no names. Empty for an array, a map and a variant.
    pub fields: Vec<String>,
    /// For the variable of a `for` over a container the loop does not own:
    /// the container's root, which the way out names. This is not a
    /// [`BorrowKind`] on purpose: a kind refuses every take, which refuses
    /// `std/vyx.vyrn`'s `for s in kids`; this only words a refusal the alias
    /// table already found.
    pub loop_var: Option<String>,
    /// The loop that walks this borrow, if one does. A `modify` argument over
    /// the container ends it whatever the container holds (`vyrn_lower::kernel`).
    pub walked: Option<Walk>,
    /// Whether the type is linear: a `Stream`, a `Task`, or `impl MustUse`.
    /// Its disposing builtin (`close`, `@join`, `boxStream`)
    /// takes no move, so a use after it is worded as a `consume` parameter's,
    /// not as a move into a sink.
    pub linear: bool,
    /// Whether a `let` the reader wrote bound this name, which makes it a
    /// binding the memory report is about.
    pub bound_by_let: bool,
    /// Whether the reader may store into the name: a `let mut` or a `modify`
    /// parameter (`vyrn_lower::typed::stores`).
    pub mutable: bool,
    /// For a name a lambda literal binds: the captures the closure reads as
    /// values, where the closure may outlive its call. `None`
    /// where it may not, and for a name no lambda binds.
    ///
    /// A lambda at an argument whose parameter only borrows it
    /// (`map(xs, x -> ..)`) cannot outlive the call and captures freely. A
    /// capture the body only calls is not a held value: it names a function
    /// (`examples/capturefn.vyrn`, `std/stream.vyrn`).
    pub closure_reads: Option<Vec<Name>>,
    /// Why the frame does not own the value this name binds, as
    /// `vyrn_lower::core::Builder::owned_binding` decides it. `None` for an owned name and
    /// every temporary. Only the memory report reads it, so the report and
    /// the ownership rule are one statement.
    pub not_owned: Option<NotOwned>,
    /// The declared `release` bodies a release of this name may call
    /// ([`crate::declared::Owned::declared_releases`]). The effect
    /// judgment joins them for every name the frame does not borrow, since a
    /// row the placer has yet to write may release it; the kernel reads them
    /// where a row releases the name (`vyrn_lower::core::runs`).
    pub runs: Vec<String>,
}

/// A loop that reads its container through a borrow ([`NameInfo::walked`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    /// A `for` reads its container through the borrow from head to end.
    For,
    /// A `while` reads the header of a container it indexes and never
    /// rebuilds (`vyrn_lower::core::Builder::hoist_headers`). An element store moves no
    /// header, so it ends no such borrow.
    While,
}

/// Why a `let` binds a value the frame does not own, in the order
/// `vyrn_lower::core::Builder::owned_binding` asks.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotOwned {
    /// The type releases nothing. `heap` separates "nothing to reclaim" from
    /// "no release rule yet".
    NoRelease { heap: bool },
    /// The type carries a must-use obligation, and the construct that
    /// discharges it reclaims the value.
    MustUse(Linear),
    /// A literal in the module's data segment.
    Static,
    /// Somebody else owns the storage; the words are
    /// `movecheck::Borrow::what`'s.
    Borrow(String),
}

/// A borrow the surface named, rather than one the kernel reads out of a
/// place (`movecheck::Borrow`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BorrowKind {
    /// A `read` or `modify` parameter, or a second name for one: the
    /// capability, and the parameter's own spelling.
    Param { cap: &'static str, of: String },
    /// A name of the enclosing frame that a lambda frame reads.
    /// The closure observes it; the frame that made it still owns it.
    Capture,
    /// The variable of a `for` over a container the loop does not own. `of`
    /// is the container's root, which the way out names
    /// (`movecheck::Borrow::Element`).
    LoopVar { of: String },
    /// A payload binder of a non-consuming `match` whose scrutinee reads a
    /// place (`movecheck::Borrow::Projection`). The alias beside it names the
    /// place; this lets a refusal quote the binder instead.
    Place,
}

impl BorrowKind {
    /// What this borrow is, in words, for a refusal about the name `at`, as
    /// `movecheck::Borrow::what` words it.
    pub fn what(&self, at: &str) -> String {
        // A path under the parameter (`h.meta[0]`) is the parameter; a name
        // bound to one (`let t = r.s`) is a second name. Compare roots, as
        // `movecheck::Borrow::what` does.
        let at = crate::ast::root_of(at);
        match self {
            BorrowKind::Param { cap, of } if at == of => format!("a `{cap}` parameter"),
            BorrowKind::Param { cap, of } => {
                format!("a second name for the `{cap}` parameter `{of}`")
            }
            BorrowKind::Capture => "a captured binding".to_string(),
            BorrowKind::LoopVar { .. } => "a loop variable".to_string(),
            BorrowKind::Place => "read out of a place that owns it".to_string(),
        }
    }

    /// The named ways out, in `movecheck::Borrow::fixes`'s
    /// order and words. `path` is what was read out of the binding. A capture
    /// has none: the checker answers that shape at the capture.
    pub fn fixes(&self, path: &str) -> Vec<String> {
        match self {
            BorrowKind::Param { of, .. } => vec![
                format!("declare the parameter `{of}: consume ..` if this function should own it"),
                format!("`{path}.copy()` if both sides need a value"),
            ],
            BorrowKind::Capture => Vec::new(),
            // `for .. in consume` helps only when the whole element is handed
            // on; a field of one is a partial move, so a path gets the copy.
            BorrowKind::LoopVar { of } if crate::ast::root_of(path) == path => vec![
                format!("`for {path} in consume {of}` if the loop should take the elements"),
                format!("`{path}.copy()` if both sides need a value"),
            ],
            BorrowKind::LoopVar { .. } => {
                vec![format!("`{path}.copy()` if both sides need a value")]
            }
            BorrowKind::Place => vec![format!("`{path}.copy()` if both sides need a value")],
        }
    }
}

/// The AST node the ownership plan keys a statement's row by, so a reader
/// holding the node can look the row up. Not a source position.
///
/// Carried by [`St::Store`], a [`St::Drop`] of a discarded result, of a
/// reader's `drop`, or owed by a join edge ([`Site::Edge`]), [`St::Row`],
/// [`St::If`], [`St::Block`], [`St::Break`], [`St::Continue`], [`St::Return`]
/// and [`Arm`]. An argument temporary's key rides on the name
/// ([`NameInfo::arg_drop`]). Every other statement states [`Site::None`], and
/// a reader falls back to the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Site {
    #[default]
    None,
    Node(NodeId),
    /// The join whose edge owes this release, and the edge: 0/1 for an
    /// `if`'s then/else, the arm's source index for a `match`.
    Edge(NodeId, u32),
}

/// The value of a literal. The width is not here: an integer literal's type
/// is its destination's, which `vyrn_lower::NodeTypes::types` states.
#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Int(i64),
    /// A byte literal `'c'`.
    Byte(u8),
    Float(f64),
    Bool(bool),
    /// A string literal, decoded. Where the bytes land is the emitter's
    /// question; the two compiled backends answer it differently.
    Str(String),
    /// Not a value a reader wrote, and nothing an emitter loads.
    Opaque(Opaque),
}

/// What a row stands on where it names no value; one producer per kind.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opaque {
    /// A function's name used as a value, or a type's name as an argument
    /// (`fromJson(Bag, src)`). A `fn`-typed argument is monomorphized and no
    /// value stands there; a stored fn value is the tag the
    /// defunctionalizer chose, which the emitter holds.
    Static,
    /// The result of a call that traps (`panic`). Only a row after the
    /// `St::Trap` reads it, and no finished body holds one (`vyrn_lower::core::cut`).
    Trapped,
    /// The value a `?` stores where its ok arm binds nothing.
    Unbound,
}

#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Name(Name),
    /// A literal: nothing to own. A string literal is static data
    /// ([`NotOwned::Static`]).
    Lit(Lit),
}

/// One argument of a call: a value, or the place a `modify` parameter
/// writes. A place argument is a move-out window, whose extent is
/// the call (`vyrn_lower::core::Builder::nested_store`).
#[derive(Debug, Clone, PartialEq)]
pub enum Arg {
    Val(Val),
    Place(Place),
}

impl Arg {
    pub fn val(&self) -> Option<&Val> {
        match self {
            Arg::Val(v) => Some(v),
            Arg::Place(_) => None,
        }
    }
}

#[derive(Debug, Clone, PartialEq)]
pub enum Place {
    Name(Name),
    Global(String),
    Field(Box<Place>, String),
    Elem(Box<Place>, Val),
    /// A map's entry at a key. A store into it takes the key: the map keeps
    /// it, or releases the surplus one when an equal key is there.
    /// A read borrows the key.
    Key(Box<Place>, Val),
}

/// A right-hand side: what produced the value a `let` binds or a store puts
/// away.
///
/// `Call::ret` and `Prim`'s last field are the producer type: the checker's
/// type for the value before any coercion the destination asks for
/// (`vyrn_lower::NodeTypes::produced`). The typed judgment
/// (`typed.rs`) reads it; without it `a + b` and `UInt8(n)` read alike.
#[derive(Debug, Clone)]
pub enum Rhs {
    /// The value of a name: a move when the name is owned, a copy otherwise.
    Val(Val),
    /// A read of a place that yields a value the kernel does not own: a scalar
    /// field, an element of a heapless type, a borrowed payload.
    Read(Place),
    /// A move out of a sub-place (`consume x.f`; the receiver a
    /// rebuilding builtin hands back). The base keeps a hole: a later release
    /// of the base walks the rest, and a later read of the hole is refused.
    Take(Place),
    Call {
        callee: String,
        args: Vec<(Arg, Capability)>,
        /// Argument 0 is the receiver of a rebuilding builtin passed by name
        /// (`out.push(v)`): the store after the call puts the buffer back, so
        /// the take changes no owner. Only the kernel reads it
        /// (`vyrn_lower::kernel::Kernel::take_arg`). Which builtins rebuild is
        /// [`crate::prelude::rebuilds`].
        write_back: bool,
        kind: Callee,
        /// The producer type, with the call's type arguments substituted.
        /// `None` for a call the checker did not type.
        ret: Option<Type>,
        /// A generic callee's type arguments as the checker solved them here,
        /// by parameter name in the callee's order (`vyrn_lower::NodeTypes::solved`).
        /// Empty for a non-generic callee or an unsolved call.
        solved: Vec<(String, Type)>,
        /// For a callee with `fn`-typed parameters: the target each
        /// is bound to, in the callee's order. An argument no [`Target`] names
        /// stays in `args`.
        targets: Vec<Target>,
    },
    /// Arithmetic, comparison, interpolation, conversion: reads its operands.
    /// The last field is the producer type: the operator's own result.
    Prim(Op, Vec<Val>, Option<Type>),
    /// A record, array, map or variant literal: takes its parts.
    Make(Ctor, Vec<Val>),
}

/// What a `fn`-typed parameter of a specialization calls: one
/// instance per target, and a call through the parameter is a direct call
/// (`vyrn_lower::core::specialize`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A function the program declares, called with no captures.
    Fn(String),
    /// The target this body's own `fn`-typed parameter is bound to: a
    /// pass-through, which `vyrn_lower::core::specialize` replaces by the instance's target.
    Param(Name),
    /// A lambda lifted under this key (`vyrn_lower::core::lambda_spelling`), called with its
    /// captures first. Each capture is the instance parameter that carries
    /// it, by name and type, in forwarding order. The type is the callee's
    /// `fn` parameter under the call's solved instance; it may name a type
    /// parameter where the call solved none.
    Lambda(String, Vec<(String, Type)>, Type),
    /// A stored value, taken as the named parameter: a call
    /// through it stays a call through the value. A call site passes the
    /// value after its own arguments, as a lambda's captures.
    Value(String),
}

/// Who a [`Rhs::Call`]'s name resolves to, one case per branch of
/// `vyrn_lower::core::Builder::call`. The kernel asks [`Callee::declared`] and
/// [`Callee::ctor`]; an emitter asks which case it is.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callee {
    /// A function this program declares.
    Fn,
    /// A method of an `impl` block, dispatched on its receiver's concrete
    /// type.
    Method,
    /// A projection, dispatched through the places table.
    Projection,
    /// A seeded builtin: a row of [`crate::prelude::signature`].
    Builtin,
    /// A variant of an enum, or `Some`, `Ok`, `Err`.
    Ctor,
    /// A declared type's constructor: `T(v)` for a record or a
    /// `where`-checked type. Its arguments are taken.
    Named,
    /// A validated type's constructor at a crossing the checker proved
    /// (`vyrn_lower::core::Builder::proven`): its argument is taken at the base, unchecked.
    Proven,
    /// A reserved name with no seeded row: `fromJson`, `value`, a log level,
    /// a generation-time surface builtin, a `@`-spelled operation, `print`.
    Reserved,
    /// A call through a function value in scope. The call reads
    /// the name, so every walker that counts a row's names counts it
    /// ([`Callee::value`]).
    Value(Name),
}

impl Callee {
    /// The name a call through a value reads.
    pub fn value(self) -> Option<Name> {
        match self {
            Callee::Value(n) => Some(n),
            _ => None,
        }
    }

    /// Whether the author wrote the callee's capabilities, so a `consume`
    /// takes whatever it is handed, heap or not. A callee
    /// whose capabilities `vyrn_lower::core::Builder::call` synthesizes stores its argument
    /// instead, and storing a heapless value copies it.
    pub fn declared(self) -> bool {
        matches!(self, Callee::Fn | Callee::Method | Callee::Projection)
    }

    /// Whether the callee is a constructor: it puts its argument into the
    /// value it makes, where a builtin stores it. The refusals word the two
    /// apart.
    pub fn ctor(self) -> bool {
        matches!(self, Callee::Ctor)
    }

    /// Whether the callee stores what it is handed: neither declared nor a
    /// constructor.
    pub fn stores(self) -> bool {
        !self.declared() && !self.ctor()
    }
}

/// The operation a [`Rhs::Prim`] row performs: the source's operator, not an
/// opcode. The operand's type, which the checker states at the operand's
/// node, picks the instruction.
///
/// `&&` and `||` short-circuit, so an emitter runs the right operand under a
/// branch. The row reads both operands because neither read owns anything.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Un(UnOp),
    Bin(BinOp),
    /// A conversion between two scalars (`Int32(n)`, `UInt8(n)`): the target;
    /// the source is the operand's type. Which names convert is
    /// [`crate::types::numeric_conv_target`]; what a conversion does
    /// between a pair is the coercion plan's (`vyrn_codegen::Rung`).
    Conv(Type),
    /// A lambda literal. Its operands are the captures the closure
    /// snapshots, read rather than taken, so it is a prim and not a [`Ctor`].
    /// The key names its lifted body (`vyrn_lower::core::lambda_spelling`).
    Closure(String),
}

/// What a [`Rhs::Make`] row constructs.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ctor {
    /// A record literal `T { f: .. }`: the type, and the field each part
    /// fills, in the order the reader wrote them. An emitter joins this to
    /// the declaration's (layout) order by name.
    Record(String, Vec<String>),
    Array,
    /// A map literal `{k: v}`: key then value, one pair per entry, in
    /// insertion order.
    Map,
    /// `T?(v)`: the constructor of a `where`-checked type, which
    /// answers an `Option<T>` rather than a `T`.
    Try(String),
    /// A function value made for storage: the variant of the
    /// signature's closure enum its target names, with the captures as parts.
    Closure(Target),
}

/// What a store displaces.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Old {
    /// The place held nothing that owns heap.
    Nothing,
    /// The plan placed a release of the old value before this store.
    Released,
    /// The old value owns heap and nothing released it. The kernel refuses.
    Unreleased,
    /// The stored value was built from the place's own value
    /// (`xs = xs.push(v)`), so the name keeps holding without a release.
    Transferred,
    /// The first build's word at every other store: the place may hold a
    /// value, and its release row is still to be written. `old` decides a
    /// refusal and never a state, so a first build that refuses nothing here
    /// leaves every later statement's judgment unchanged. The kernel reports
    /// the store (`vyrn_lower::kernel::MissingKind::Store`), the placer writes
    /// the row, and the second build states [`Old::Released`] or
    /// [`Old::Nothing`].
    Pending,
}

#[derive(Debug, Clone)]
pub enum St {
    Let(Name, Rhs),
    Store {
        place: Place,
        value: Val,
        old: Old,
        /// The source line, which a refusal names as the checker does.
        line: usize,
        /// The statement the plan keys its store decision by. [`Site::None`]
        /// for a store this pass made up (a global's initializer, a desugar's
        /// temporary) and for a user container's `c[i] = v`, whose store
        /// statements the `place at` rewrite builds; a reader falls
        /// back to the plan there.
        site: Site,
        /// Whether an emitter releases the displaced value: the plan's
        /// decision, with the mention guard and its exceptions folded in for a
        /// name store. `old` is what the kernel sees at the place instead: a
        /// place holding nothing that owns heap displaces nothing, whatever
        /// the plan decided.
        releases: bool,
        /// The holes inside the place, relative to it (`.f`), which the
        /// release of the displaced value walks around: a `consume` took
        /// them, so the store fills them without freeing them again.
        holes: Vec<String>,
    },
    /// A release. `site` is the plan's key where it has one: the `Stmt::Expr`
    /// of a discarded result, the `drop` statement, or the join and edge of a
    /// Rule N release. [`Site::None`] for a scope's own release, an argument
    /// temporary and a payload binder ([`NameInfo::arg_drop`],
    /// [`NameInfo::receiver`], [`Arm::frees`]).
    ///
    /// The line is the reader's `drop` statement's, 0 for a placed release,
    /// which a refusal cannot name.
    ///
    /// The holes are the parts that left the value on this path
    /// ([`Body::drop_holes`]). `None` means the name's own set
    /// ([`NameInfo::holes`]); an edge release carries its edge's set, because
    /// one name is holed on one arm and whole on the next.
    Drop(Name, Site, usize, Option<Vec<String>>),
    /// A release row the plan placed at an exit, walking the name around
    /// `holes`. The kernel checks the set against its state: a row that skips
    /// a held place is a leak, and one that walks a taken place is a double
    /// free.
    Row {
        name: Name,
        holes: Vec<String>,
        exit: Exit,
        site: NodeId,
    },
    If {
        cond: Val,
        then: Vec<St>,
        els: Vec<St>,
        /// The `if` statement, the plan's key for its edge releases; 0 for
        /// an `if` this pass made up.
        site: NodeId,
    },
    /// A loop, and the `while` or `for` it came from; `0` for a loop this pass
    /// made up. A `while`'s exit is a two-way branch at the head.
    Loop {
        body: Vec<St>,
        site: NodeId,
    },
    /// A source block: its own scope, and the site the plan keys its
    /// fall-through release rows by.
    Block {
        site: NodeId,
        body: Vec<St>,
        /// `region { .. }`: an arena scope whose values the exit
        /// frees together. The kernel judges it as a plain block; an emitter
        /// takes a mark on entry and returns it on exit.
        region: bool,
    },
    /// `site` is the statement's node and `line` its line; both 0 for a break
    /// this pass made up.
    Break {
        site: NodeId,
        line: usize,
    },
    Continue {
        site: NodeId,
        line: usize,
    },
    Return {
        value: Option<Val>,
        /// The `return` statement, or the `?` expression when `is_try`.
        site: NodeId,
        is_try: bool,
        line: usize,
    },
    Switch {
        on: Val,
        arms: Vec<Arm>,
        /// The construct took the value: its payloads moved into the arms'
        /// binders, so nothing releases the scrutinee itself afterwards.
        consuming: bool,
        /// Every arm ends with the `return` this `match` was the operand of
        /// (`vyrn_lower::core::Builder::return_through`), and the switch has no join. What is
        /// held there is one arm's, so the kernel keys the row by the arm.
        carries: bool,
        /// The scrutinee is a value the frame made, not a place it reads (a
        /// `consume`, a call's result, a literal, a taken name, a `Map`
        /// lookup), so the boxes the binders come out of are the construct's
        /// to free. `consuming` is about the name; this is about the boxes.
        /// Never set on a declared release's receiver, whose caller frees
        /// them (`vyrn_lower::core::Builder::owns_boxes`).
        owns: bool,
        /// The node the plan keys this switch and its [`Arm`]s by: the `if
        /// let` statement, or the `match` or `?` expression. `0` for a switch
        /// this pass made up.
        site: NodeId,
        line: usize,
    },
    /// An expression for its effect, its line, and the statement it came
    /// from; `0` for a row this pass made up (the `panic` call before a
    /// [`St::Trap`]).
    Do {
        rhs: Rhs,
        line: usize,
        site: NodeId,
    },
    /// A refusal or a `panic`: the path ends here and owes nothing.
    Trap,
    /// A runtime check of the row after it ([`crate::check`]). Only
    /// `vyrn_lower::check::state` writes one, into the bodies an emitter reads;
    /// the kernel never judges one.
    Check(check::Check),
}

#[derive(Debug, Clone)]
pub struct Arm {
    /// The payload binders. Owned when the match consumed its scrutinee.
    pub binds: Vec<Name>,
    /// The binders this arm releases at its end, in order. Each is a
    /// `St::Drop` at the end of `body` with the binder's own holes; the list
    /// names them because the join's edge drops follow and a position is not
    /// a key. `None` where this pass states no answer.
    pub frees: Option<Vec<Name>>,
    pub body: Vec<St>,
    pub test: Test,
    /// The `match`, `if let` or `?` this arm belongs to, and which arm: the
    /// plan's key for an arm payload free and an edge release.
    pub site: NodeId,
    pub index: u32,
}

impl Arm {
    /// The rows at the head of the arm that bind a binder as a read out of
    /// the scrutinee `on`. The kernel judges each binder so bound as a read
    /// binding for the arm's extent, and an emitter writes nothing for them.
    pub fn reads(&self, on: &Val) -> &[St] {
        let n = self
            .body
            .iter()
            .take_while(|s| {
                matches!(s, St::Let(b, Rhs::Read(Place::Name(m)))
                    if self.binds.contains(b) && matches!(on, Val::Name(o) if o == m))
            })
            .count();
        &self.body[..n]
    }
}

/// What a payload binder read out of its scrutinee leaves there when a
/// `consume` parameter takes it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    /// A hole the scrutinee's release walks around, spelled `.Variant.i`.
    Hole(String),
    /// Nothing may leave: the scrutinee's type, spelled here, declares a
    /// `release` that reads every payload.
    Sealed(String),
}

/// How one [`Arm`] of a [`St::Switch`] is chosen. Arms are tried in order.
/// The arm index is the source's order and the tag is the variant's position
/// in its enum; the two differ in general.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Test {
    Tag(u64),
    /// No arm before it was chosen: `_`, and the `else` of an `if let`.
    Else,
    /// The `Bool` this name holds is true: a predicate the program states,
    /// called before the switch. `?` on a declared `Fallible` asks
    /// the impl's `isSuccess`.
    Holds(Name),
}

impl Test {
    pub fn reads(&self) -> Option<Name> {
        match self {
            Test::Holds(n) => Some(*n),
            Test::Tag(_) | Test::Else => None,
        }
    }
}

/// What a row does with a value it names ([`St::operands`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Use {
    /// Reads the value and leaves it where it is.
    Read,
    /// Hands the value on: a `consume` argument, a part of a literal, a
    /// stored or returned value.
    Hand,
    /// The key a store writes at, which the container keeps.
    Key,
    /// The name a place is rooted at.
    Root,
    /// An index or a key that selects a place.
    Index,
    Bind,
    Release,
}

impl St {
    /// The lists of rows this row holds, in program order: an `if`'s then
    /// and else, a loop's or a block's body, each arm's body.
    pub fn lists(&self) -> impl Iterator<Item = &Vec<St>> {
        let (a, b, arms): (Option<&Vec<St>>, Option<&Vec<St>>, &[Arm]) = match self {
            St::If { then, els, .. } => (Some(then), Some(els), &[]),
            St::Loop { body, .. } | St::Block { body, .. } => (Some(body), None, &[]),
            St::Switch { arms, .. } => (None, None, arms),
            St::Let(..)
            | St::Store { .. }
            | St::Drop(..)
            | St::Row { .. }
            | St::Break { .. }
            | St::Continue { .. }
            | St::Return { .. }
            | St::Do { .. }
            | St::Trap
            | St::Check(_) => (None, None, &[]),
        };
        a.into_iter().chain(b).chain(arms.iter().map(|a| &a.body))
    }

    /// [`St::lists`], to write to.
    pub fn lists_mut(&mut self) -> impl Iterator<Item = &mut Vec<St>> {
        let (a, b, arms): (Option<&mut Vec<St>>, Option<&mut Vec<St>>, &mut [Arm]) = match self {
            St::If { then, els, .. } => (Some(then), Some(els), &mut []),
            St::Loop { body, .. } | St::Block { body, .. } => (Some(body), None, &mut []),
            St::Switch { arms, .. } => (None, None, arms),
            St::Let(..)
            | St::Store { .. }
            | St::Drop(..)
            | St::Row { .. }
            | St::Break { .. }
            | St::Continue { .. }
            | St::Return { .. }
            | St::Do { .. }
            | St::Trap
            | St::Check(_) => (None, None, &mut []),
        };
        a.into_iter()
            .chain(b)
            .chain(arms.iter_mut().map(|a| &mut a.body))
    }

    /// This row and every row under it, as [`rows`] walks them.
    pub fn rows(&self) -> Rows<'_> {
        rows(std::slice::from_ref(self))
    }

    /// Every value this row names, in the order it evaluates them, with what
    /// it does with each. The rows it holds name their own, and an arm's
    /// binders are the arm's ([`Arm::binds`]).
    pub fn operands(&self, f: &mut dyn FnMut(&Val, Use)) {
        match self {
            St::Let(n, rhs) => {
                rhs.operands(f);
                f(&Val::Name(*n), Use::Bind);
            }
            St::Do { rhs, .. } => rhs.operands(f),
            St::Store { place, value, .. } => {
                place.stored(f);
                f(value, Use::Hand);
            }
            St::Drop(n, ..) | St::Row { name: n, .. } => f(&Val::Name(*n), Use::Release),
            St::If { cond, .. } => f(cond, Use::Read),
            St::Switch { on, arms, .. } => {
                for n in arms.iter().filter_map(|a| a.test.reads()) {
                    f(&Val::Name(n), Use::Read);
                }
                f(on, Use::Read);
            }
            St::Return { value: Some(v), .. } => f(v, Use::Hand),
            St::Return { value: None, .. }
            | St::Loop { .. }
            | St::Block { .. }
            | St::Break { .. }
            | St::Continue { .. }
            | St::Trap => {}
            // A check reads what the row it guards reads, so no walker counts
            // its guard as a second read.
            St::Check(_) => {}
        }
    }
}

impl Rhs {
    fn operands(&self, f: &mut dyn FnMut(&Val, Use)) {
        match self {
            Rhs::Val(v) => f(v, Use::Read),
            Rhs::Read(p) | Rhs::Take(p) => p.operands(f),
            Rhs::Call { args, kind, .. } => {
                if let Some(n) = kind.value() {
                    f(&Val::Name(n), Use::Read);
                }
                for (a, c) in args {
                    match a {
                        Arg::Place(p) => p.operands(f),
                        Arg::Val(v) if *c == Capability::Consume => f(v, Use::Hand),
                        Arg::Val(v) => f(v, Use::Read),
                    }
                }
            }
            Rhs::Prim(_, vs, _) => vs.iter().for_each(|v| f(v, Use::Read)),
            Rhs::Make(_, vs) => vs.iter().for_each(|v| f(v, Use::Hand)),
        }
    }
}

impl Place {
    fn operands(&self, f: &mut dyn FnMut(&Val, Use)) {
        match self {
            Place::Name(n) => f(&Val::Name(*n), Use::Root),
            Place::Global(_) => {}
            Place::Field(b, _) => b.operands(f),
            Place::Elem(b, v) | Place::Key(b, v) => {
                b.operands(f);
                f(v, Use::Index);
            }
        }
    }

    /// [`Place::operands`] of a place a store writes: the key it writes at,
    /// below any fields and elements, is [`Use::Key`].
    fn stored(&self, f: &mut dyn FnMut(&Val, Use)) {
        match self {
            Place::Key(b, v) => {
                b.operands(f);
                f(v, Use::Key);
            }
            Place::Field(b, _) => b.stored(f),
            Place::Elem(b, v) => {
                b.stored(f);
                f(v, Use::Index);
            }
            Place::Name(_) | Place::Global(_) => self.operands(f),
        }
    }
}

/// Every row of `ss` and every row under it, in program order, each before
/// the rows it holds, with the number of loops between the row and `ss`.
pub fn rows(ss: &[St]) -> Rows<'_> {
    Rows(vec![(ss.iter(), 0)])
}

/// The iterator [`rows`] returns: a stack of the lists still being walked,
/// the innermost last, each with its loop depth.
pub struct Rows<'a>(Vec<(std::slice::Iter<'a, St>, u32)>);

impl<'a> Iterator for Rows<'a> {
    type Item = (&'a St, u32);

    fn next(&mut self) -> Option<(&'a St, u32)> {
        loop {
            let (list, depth) = self.0.last_mut()?;
            let depth = *depth;
            let Some(s) = list.next() else {
                self.0.pop();
                continue;
            };
            let inner = depth + u32::from(matches!(s, St::Loop { .. }));
            let at = self.0.len();
            self.0.extend(s.lists().map(|l| (l.iter(), inner)));
            self.0[at..].reverse();
            return Some((s, depth));
        }
    }
}
#[derive(Debug, Clone)]
pub struct Body {
    pub name: String,
    /// The module file the function came from; `None` for the root.
    pub file: Option<String>,
    /// `export extern fn`: the JS caller releases every String the call hands
    /// back, so returning a borrow has its own refusal
    /// and `.copy()` is the only way out. A lambda frame carries its holder's
    /// flag, as `movecheck::refuse_return` reads it.
    pub export: bool,
    pub names: Vec<NameInfo>,
    pub params: Vec<Name>,
    pub stmts: Vec<St>,
    /// The bodies of the lambdas this body holds, each its own frame: its
    /// parameters and captures are borrowed inputs, and the plan keys its
    /// rows by its own nodes under the enclosing function's name.
    pub lambdas: Vec<Body>,
    /// Every construct that may take the value it was handed: its node, the
    /// value's name here, and its shape. `vyrn_lower::core::last_owner` decides whether it
    /// does, and the second build acts on that.
    pub cands: Vec<(NodeId, Name, Cand)>,
    /// The `for` statements whose container release frees the buffer alone,
    /// keyed by the loop's node: every element left through the loop
    /// variable ([`Cand::Elem`]), so a deep walk would free values somebody
    /// else owns. The buffer is field 0 of the growable array's triple.
    pub loop_buffers: Vec<NodeId>,
    /// A `drop` whose name no binding in scope answers: the name and the
    /// line. The core has no row for it; `vyrn_lower::typed::drops` refuses it.
    pub unbound_drops: Vec<(String, usize)>,
    /// A rule the checker let through that the builder met while lowering
    /// its construct: the line and the sentence. An unknown name is one; the
    /// builder binds the checker's `Err` there. `vyrn_lower::typed::refused`
    /// refuses each.
    pub refused: Vec<(usize, String)>,
    /// The same for a rule about the checker's types that holds as written,
    /// not per instance: a non-Bool condition, a `for` over what no loop walks.
    pub mistyped: Vec<(usize, String)>,
}

/// The shape of a candidate construct, which `vyrn_lower::core::last_owner` asks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub enum Cand {
    /// A `match`, an `if let` or a `?`: the last owner where nothing reads
    /// the name after the switch.
    Switch,
    /// A `for`: the last owner where the name's last read is inside the loop.
    Loop,
    /// A `for`'s variable handed on in the body (into a `consume` position,
    /// a literal, a store, a map key or a `return`), not only read: each turn
    /// owns its element and the container's release frees the buffer alone.
    Elem,
}

impl Body {
    /// This body and every lambda body under it, outermost first.
    pub fn frames(&self) -> Vec<&Body> {
        let mut out = vec![self];
        for l in &self.lambdas {
            out.extend(l.frames());
        }
        out
    }

    /// The holes a release row of `n` walks around: the row's own set, else
    /// the name's.
    pub fn drop_holes<'b>(&'b self, n: Name, row: &'b Option<Vec<String>>) -> &'b [String] {
        row.as_deref().unwrap_or(&self.names[n.index()].holes)
    }

    /// How many times each name is read, which decides whether an emitter
    /// may leave a value on the operand stack rather than in a local.
    pub fn reads(&self) -> Vec<u32> {
        let mut out = vec![0u32; self.names.len()];
        count_reads(&self.stmts, &mut out);
        out
    }

    /// How many times a row names each name, bindings and releases included.
    /// `vyrn_lower::core::extent_ends` compares a list against it to know that no row outside
    /// the list names a name.
    pub fn occurrences(&self) -> Vec<u32> {
        let mut ns = Vec::new();
        self.stmts.iter().for_each(|s| names_in(s, &mut ns));
        let mut out = vec![0u32; self.names.len()];
        for n in ns {
            out[n.index()] += 1;
        }
        out
    }

    /// The body as text, one statement per line, for reading a refusal.
    pub fn render(&self) -> String {
        let mut out = format!("fn {}(", self.name);
        for (i, p) in self.params.iter().enumerate() {
            if i > 0 {
                out.push_str(", ");
            }
            out.push_str(&self.spell(*p));
        }
        out.push_str(")\n");
        self.render_stmts(&self.stmts, 1, &mut out);
        for l in &self.lambdas {
            out.push('\n');
            out.push_str(&l.render());
        }
        out
    }

    fn spell(&self, n: Name) -> String {
        let i = &self.names[n.index()];
        if i.releases {
            format!("{}!", i.source)
        } else {
            i.source.clone()
        }
    }

    fn val(&self, v: &Val) -> String {
        match v {
            Val::Name(n) => self.spell(*n),
            Val::Lit(l) => match l {
                Lit::Int(v) => format!("lit {v}"),
                Lit::Byte(v) => format!("lit byte {v}"),
                Lit::Float(v) => format!("lit {v:?}"),
                Lit::Bool(v) => format!("lit {v}"),
                Lit::Str(s) => format!("lit {s:?}"),
                Lit::Opaque(_) => "lit".into(),
            },
        }
    }

    fn place(&self, p: &Place) -> String {
        match p {
            Place::Name(n) => self.spell(*n),
            Place::Global(g) => format!("global {g}"),
            Place::Field(b, f) => format!("{}.{f}", self.place(b)),
            Place::Elem(b, i) => format!("{}[{}]", self.place(b), self.val(i)),
            Place::Key(b, k) => format!("{}[key {}]", self.place(b), self.val(k)),
        }
    }

    fn check(&self, c: &check::Check) -> String {
        use check::Guard as G;
        let what = match &c.guard {
            G::Index(p, i) => format!("{}[{}]", self.place(p), self.val(i)),
            G::Span(p, i, n) => format!("{}[{} +{n}]", self.place(p), self.val(i)),
            G::Range(s, a, b) => format!("{}[{}..{}]", self.val(s), self.val(a), self.val(b)),
            G::NonZero(d) => format!("{} != 0", self.val(d)),
            G::NoOverflow(n, d, bits) => {
                format!("({}, {}) != (min{bits}, -1)", self.val(n), self.val(d))
            }
            G::Shift(k, bits) => format!("{} in 0..{bits}", self.val(k)),
        };
        let at = c.site;
        let word = match c.verdict {
            check::Verdict::Kept => "check",
            check::Verdict::Proved => "proved",
        };
        format!(
            "{word} {} {what}  (line {} #{})",
            c.rule.census(),
            at.line,
            at.ordinal
        )
    }

    fn rhs(&self, r: &Rhs) -> String {
        match r {
            Rhs::Val(v) => self.val(v),
            Rhs::Read(p) => format!("read {}", self.place(p)),
            Rhs::Take(p) => format!("take {}", self.place(p)),
            Rhs::Call {
                callee, args, kind, ..
            } => format!(
                "{} {callee}({})",
                format!("{kind:?}").to_lowercase(),
                args.iter()
                    .map(|(a, c)| {
                        let a = match a {
                            Arg::Val(v) => self.val(v),
                            Arg::Place(p) => self.place(p),
                        };
                        format!("{c:?} {a}").to_lowercase()
                    })
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Rhs::Prim(op, vs, _) => format!(
                "prim {}({})",
                match op {
                    Op::Un(o) => format!("{o:?}").to_lowercase(),
                    Op::Bin(o) => format!("{o:?}").to_lowercase(),
                    Op::Conv(t) => format!("conv {t}"),
                    Op::Closure(_) => "closure".into(),
                },
                vs.iter()
                    .map(|v| self.val(v))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Rhs::Make(c, vs) => format!(
                "make {}({})",
                match c {
                    Ctor::Record(t, fs) => format!("{t} {{{}}}", fs.join(", ")),
                    Ctor::Array => "array".into(),
                    Ctor::Map => "map".into(),
                    Ctor::Try(t) => format!("{t}?"),
                    Ctor::Closure(t) => format!("closure {t:?}"),
                },
                vs.iter()
                    .map(|v| self.val(v))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
        }
    }

    fn render_stmts(&self, stmts: &[St], depth: usize, out: &mut String) {
        let pad = "  ".repeat(depth);
        for s in stmts {
            match s {
                St::Let(n, r) => {
                    out.push_str(&format!("{pad}let {} = {}\n", self.spell(*n), self.rhs(r)))
                }
                St::Store {
                    place,
                    value,
                    old,
                    holes,
                    ..
                } => out.push_str(&format!(
                    "{pad}{} = {}  ({:?}{})\n",
                    self.place(place),
                    self.val(value),
                    old,
                    if holes.is_empty() {
                        String::new()
                    } else {
                        format!(" minus {holes:?}")
                    }
                )),
                St::Drop(n, _, _, _) => out.push_str(&format!("{pad}drop {}\n", self.spell(*n))),
                St::Row { name, holes, .. } => out.push_str(&format!(
                    "{pad}drop {} minus {:?}\n",
                    self.spell(*name),
                    holes
                )),
                St::If {
                    cond, then, els, ..
                } => {
                    out.push_str(&format!("{pad}if {}\n", self.val(cond)));
                    self.render_stmts(then, depth + 1, out);
                    out.push_str(&format!("{pad}else\n"));
                    self.render_stmts(els, depth + 1, out);
                }
                St::Loop { body: b, .. } => {
                    out.push_str(&format!("{pad}loop\n"));
                    self.render_stmts(b, depth + 1, out);
                }
                St::Block { body, .. } => {
                    out.push_str(&format!("{pad}{{\n"));
                    self.render_stmts(body, depth + 1, out);
                    out.push_str(&format!("{pad}}}\n"));
                }
                St::Break { .. } => out.push_str(&format!("{pad}break\n")),
                St::Continue { .. } => out.push_str(&format!("{pad}continue\n")),
                St::Return { value, is_try, .. } => out.push_str(&format!(
                    "{pad}return {}{}\n",
                    value.as_ref().map(|v| self.val(v)).unwrap_or_default(),
                    if *is_try { "  (?)" } else { "" }
                )),
                St::Switch {
                    on,
                    arms,
                    consuming,
                    ..
                } => {
                    out.push_str(&format!(
                        "{pad}switch {}{}\n",
                        self.val(on),
                        if *consuming { " (taken)" } else { "" }
                    ));
                    for a in arms {
                        out.push_str(&format!(
                            "{pad}  arm({})\n",
                            a.binds
                                .iter()
                                .map(|b| self.spell(*b))
                                .collect::<Vec<_>>()
                                .join(", ")
                        ));
                        self.render_stmts(&a.body, depth + 2, out);
                    }
                }
                St::Do { rhs: r, .. } => out.push_str(&format!("{pad}do {}\n", self.rhs(r))),
                St::Trap => out.push_str(&format!("{pad}trap\n")),
                St::Check(c) => out.push_str(&format!("{pad}{}\n", self.check(c))),
            }
        }
    }
}

/// The core's statements folded into side tables keyed by AST node, as the
/// plan keys them, so an emitter walking the AST can look them up. Built once
/// per compile; `compiler/vyrn-cli/tests/coretables.rs` counts every row over
/// the corpus.
#[derive(Default, Clone, Debug)]
pub struct Facts {
    /// The `Expr::Field` node of an unnamed receiver the core releases after
    /// the read, and the holes the release walks around.
    pub receivers: std::collections::HashMap<NodeId, Vec<String>>,
    /// `(match, arm) -> [(binder, holes, kind)]`: the payload binders the
    /// arm's body releases at its end. The kind is the binder type's release
    /// rule, which the interpreter needs; the compiled backends read the type.
    pub arms:
        std::collections::HashMap<(NodeId, u32), Vec<(String, Vec<String>, Option<DropKind>)>>,
    /// The store statements and whether each releases its old value: a
    /// `St::Store`'s `releases` at a [`Site::Node`]. An absent site has no
    /// answer here, and a reader falls back to the plan.
    pub stores: std::collections::HashMap<NodeId, bool>,
    /// The stores the core stands down at whatever the judgment says: the
    /// value hands the place back (`xs = xs.push(v)`), or the place owns no
    /// heap (`w = 2`). No emitter reads it (`stores` folds both in); the
    /// corpus test uses it to check that every plan row is either released
    /// or stood down.
    pub stood_down: std::collections::HashSet<NodeId>,
    /// The statement-position calls whose unbound owned result the core
    /// releases: a `St::Drop` at the statement's [`Site::Node`].
    pub discarded: std::collections::HashSet<NodeId>,
    /// The `for x in consume xs` loops that release their container where the
    /// loop ends: a `St::Drop` of a `for_consume` name at the loop's
    /// [`Site::Node`]. No row names this release, and it excludes an exit row
    /// for the container.
    pub loop_gives_back: std::collections::HashSet<NodeId>,
    /// The `for` statements whose container release frees the buffer alone
    /// ([`Body::loop_buffers`]), keyed by the loop's node. The release is a
    /// placed row at the loop, or [`Facts::loop_gives_back`].
    pub loop_buffer_only: std::collections::HashSet<NodeId>,
    /// The call-argument nodes whose temporary the caller releases after the
    /// call ([`NameInfo::arg_drop`]).
    pub arg_drops: std::collections::HashSet<NodeId>,
    /// Per join node, the `(name, edge, holes)` releases one edge owes
    /// because another edge took the name: a `St::Drop` at
    /// a [`Site::Edge`].
    pub edges: std::collections::HashMap<NodeId, Vec<EdgeRow>>,
    /// The receivers a callee allocated ([`NameInfo::receiver_malloc`]). An
    /// emitter still asks its own region depth.
    pub receiver_malloc: std::collections::HashSet<NodeId>,
    /// Per `match`, `if let` or `?` node: whether the construct took its
    /// scrutinee ([`St::Switch`]'s `consuming`). An absent site has no
    /// answer here.
    pub consuming: std::collections::HashMap<NodeId, bool>,
    /// The `match`, `if let` or `?` nodes that own the boxes their binders
    /// come out of ([`St::Switch`]'s `owns`, `vyrn_lower::core::Builder::owns_boxes`).
    pub owns_scrutinee: std::collections::HashSet<NodeId>,
}

pub fn count_reads(ss: &[St], out: &mut [u32]) {
    // A release reads the name, so the name holds a place until then. A
    // release row (`St::Row`) and a place's names are not counted.
    for (s, _) in rows(ss).filter(|(s, _)| !matches!(s, St::Row { .. })) {
        s.operands(&mut |v, u| {
            if let (Val::Name(n), Use::Read | Use::Hand | Use::Release) = (v, u) {
                out[n.index()] += 1;
            }
        });
    }
}

/// Every name a statement names, itself and everything under it: what it binds
/// and what it reads.
pub fn names_in(s: &St, out: &mut Vec<Name>) {
    for (r, _) in s.rows() {
        r.operands(&mut |v, _| {
            if let Val::Name(n) = v {
                out.push(*n);
            }
        });
    }
}
