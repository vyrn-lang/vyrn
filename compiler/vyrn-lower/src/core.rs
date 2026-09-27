//! The named core: every intermediate value has a name, every
//! access is a place, and every release the ownership plan decided is an
//! explicit [`St::Drop`]. The kernel (`kernel.rs`) checks over it that every
//! owned name is consumed exactly once on every path.
//!
//! The core takes three inputs it does not derive: the checker's type for
//! every expression ([`crate::NodeTypes`]), the plan's decisions
//! ([`vyrn_frontend::own::ReleasePlan`] and the placed [`Release`] rows), and
//! [`vyrn_frontend::declared::Owned`]. Where the plan placed a release a `Drop`
//! stands; where it did not, nothing stands, and the kernel decides whether
//! that is a leak. A construct this pass does not lower returns a [`Gap`], and
//! the instance counts as unlowered.

use std::collections::HashMap;

use vyrn_frontend::ast::{
    ArmBody, BinOp, Binder, Block, Capability, Expr, Function, LambdaBody, MatchArm, Pattern,
    Program, Stmt, Type, TypeDecl, UnOp,
};
use vyrn_frontend::declared::Owned;
use vyrn_frontend::own::{Bucket, DropKind, Exit, Linear, MemoryRow, Ownership, Release};
use vyrn_frontend::prelude;
use vyrn_frontend::project::is_place_read;

use crate::kernel::{MissingKind, Root};
use crate::{Instance, NodeTypes};

/// A name in a body: an index into [`Body::names`].
pub type Name = u32;

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
    pub binding: Option<usize>,
    /// For the unnamed receiver of a field read (`parse(q).sels`): the
    /// `Expr::Field` node that keys the plan's receiver-free row. The placer
    /// frees the receiver right after the read, minus the field it took.
    pub receiver: Option<usize>,
    /// For the unnamed receiver of a heap field or element read the consumer
    /// borrows (`f(x).rhs.startsWith("{")`): the producing node, which keys
    /// the argument-temporary drop. Set only where the consumer is a call or
    /// an operator, the two sites the compiled backends drain temporaries at.
    pub producer: Option<usize>,
    /// For a call-argument temporary the caller releases after the call: the
    /// argument's node, the key of the plan's `arg_drops` row. The release is
    /// the `St::Drop` the binding after the call queues.
    pub arg_drop: Option<usize>,
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
    /// A String accumulator [`crate::append::append_candidates`] admits:
    /// `s = s + e` on it is one `@strAppend` row ([`Spec::Rebuilds`]). A read
    /// of a module-state accumulator
    /// ([`crate::append::global_append_candidates`]) is that row's receiver
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
    /// the container ends it whatever the container holds ([`crate::kernel`]).
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
    /// parameter ([`crate::typed::stores`]).
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
    /// [`Builder::owned_binding`] decides it. `None` for an owned name and
    /// every temporary. Only the memory report reads it, so the report and
    /// the ownership rule are one statement.
    pub not_owned: Option<NotOwned>,
}

/// A loop that reads its container through a borrow ([`NameInfo::walked`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    /// A `for` reads its container through the borrow from head to end.
    For,
    /// A `while` reads the header of a container it indexes and never
    /// rebuilds ([`Builder::hoist_headers`]). An element store moves no
    /// header, so it ends no such borrow.
    While,
}

/// Why a `let` binds a value the frame does not own, in the order
/// [`Builder::owned_binding`] asks.
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

/// The path a reader wrote for a place read, as the checker quotes it
/// (`p.name`, `xs[i]`). `None` where the expression names no place.
fn reader_path(e: &Expr) -> Option<String> {
    vyrn_frontend::ast::place_path(e)
        .or_else(|| vyrn_frontend::project::element_path(e))
        .map(|(_, p)| p)
}

/// The borrow a parameter's capability makes; `None` for `consume`.
fn param_borrow(cap: Capability, name: &str) -> Option<BorrowKind> {
    let cap = match cap {
        Capability::Read => "read",
        Capability::Modify => "modify",
        Capability::Consume => return None,
    };
    Some(BorrowKind::Param {
        cap,
        of: name.to_string(),
    })
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
        let at = vyrn_frontend::ast::root_of(at);
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
            BorrowKind::LoopVar { of } if vyrn_frontend::ast::root_of(path) == path => vec![
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
    Node(usize),
    /// The join whose edge owes this release, and the edge: 0/1 for an
    /// `if`'s then/else, the arm's source index for a `match`.
    Edge(usize, u32),
}

/// The value of a literal. The width is not here: an integer literal's type
/// is its destination's, which [`crate::NodeTypes::types`] states.
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
    /// `St::Trap` reads it, and no finished body holds one ([`cut`]).
    Trapped,
    /// The value a `?` stores where its ok arm binds nothing.
    Unbound,
}

/// The literal a literal expression is; `None` for any other expression.
/// This is the one statement of which forms are literals.
/// [`Builder::rhs_inner`] also names the five because its match is
/// exhaustive on purpose, and asks here for the answer.
fn lit_of(e: &Expr) -> Option<Lit> {
    Some(match e {
        Expr::Int(v) => Lit::Int(*v),
        Expr::Byte(v) => Lit::Byte(*v),
        Expr::Float(v) => Lit::Float(*v),
        Expr::Bool(v) => Lit::Bool(*v),
        Expr::Str(s) => Lit::Str(s.clone()),
        _ => return None,
    })
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
/// the call ([`Builder::nested_store`]).
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
/// ([`crate::NodeTypes::produced`]). The typed judgment
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
        /// ([`crate::kernel::Kernel::take_arg`]). Which builtins rebuild is
        /// [`vyrn_frontend::prelude::rebuilds`].
        write_back: bool,
        kind: Callee,
        /// The producer type, with the call's type arguments substituted.
        /// `None` for a call the checker did not type.
        ret: Option<Type>,
        /// A generic callee's type arguments as the checker solved them here,
        /// by parameter name in the callee's order ([`crate::NodeTypes::solved`]).
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
/// ([`specialize`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A function the program declares, called with no captures.
    Fn(String),
    /// The target this body's own `fn`-typed parameter is bound to: a
    /// pass-through, which [`specialize`] replaces by the instance's target.
    Param(Name),
    /// A lambda lifted under this key ([`lambda_spelling`]), called with its
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
/// [`Builder::call`]. The kernel asks [`Callee::declared`] and
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
    /// A seeded builtin: a row of [`prelude::signature`].
    Builtin,
    /// A variant of an enum, or `Some`, `Ok`, `Err`.
    Ctor,
    /// A declared type's constructor: `T(v)` for a record or a
    /// `where`-checked type. Its arguments are taken.
    Named,
    /// A validated type's constructor at a crossing the checker proved
    /// ([`Builder::proven`]): its argument is taken at the base, unchecked.
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
    /// whose capabilities [`Builder::call`] synthesizes stores its argument
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
    /// [`vyrn_frontend::types::numeric_conv_target`]; what a conversion does
    /// between a pair is the coercion plan's ([`vyrn_codegen::Rung`]).
    Conv(Type),
    /// A lambda literal. Its operands are the captures the closure
    /// snapshots, read rather than taken, so it is a prim and not a [`Ctor`].
    /// The key names its lifted body ([`lambda_spelling`]).
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
    /// the store ([`crate::kernel::MissingKind::Store`]), the placer writes
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
        site: usize,
    },
    If {
        cond: Val,
        then: Vec<St>,
        els: Vec<St>,
        /// The `if` statement, the plan's key for its edge releases; 0 for
        /// an `if` this pass made up.
        site: usize,
    },
    /// A loop, and the `while` or `for` it came from; `0` for a loop this pass
    /// made up. A `while`'s exit is a two-way branch at the head.
    Loop {
        body: Vec<St>,
        site: usize,
    },
    /// A source block: its own scope, and the site the plan keys its
    /// fall-through release rows by.
    Block {
        site: usize,
        body: Vec<St>,
        /// `region { .. }`: an arena scope whose values the exit
        /// frees together. The kernel judges it as a plain block; an emitter
        /// takes a mark on entry and returns it on exit.
        region: bool,
    },
    /// `site` is the statement's node and `line` its line; both 0 for a break
    /// this pass made up.
    Break {
        site: usize,
        line: usize,
    },
    Continue {
        site: usize,
        line: usize,
    },
    Return {
        value: Option<Val>,
        /// The `return` statement, or the `?` expression when `is_try`.
        site: usize,
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
        /// ([`Builder::return_through`]), and the switch has no join. What is
        /// held there is one arm's, so the kernel keys the row by the arm.
        carries: bool,
        /// The scrutinee is a value the frame made, not a place it reads (a
        /// `consume`, a call's result, a literal, a taken name, a `Map`
        /// lookup), so the boxes the binders come out of are the construct's
        /// to free. `consuming` is about the name; this is about the boxes.
        /// Never set on a declared release's receiver, whose caller frees
        /// them ([`Builder::owns_boxes`]).
        owns: bool,
        /// The node the plan keys this switch and its [`Arm`]s by: the `if
        /// let` statement, or the `match` or `?` expression. `0` for a switch
        /// this pass made up.
        site: usize,
        line: usize,
    },
    /// An expression for its effect, its line, and the statement it came
    /// from; `0` for a row this pass made up (the `panic` call before a
    /// [`St::Trap`]).
    Do {
        rhs: Rhs,
        line: usize,
        site: usize,
    },
    /// A refusal or a `panic`: the path ends here and owes nothing.
    Trap,
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
    pub site: usize,
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
    /// value's name here, and its shape. [`last_owner`] decides whether it
    /// does, and the second build acts on that.
    pub(crate) cands: Vec<(usize, Name, Cand)>,
    /// The `for` statements whose container release frees the buffer alone,
    /// keyed by the loop's node: every element left through the loop
    /// variable ([`Cand::Elem`]), so a deep walk would free values somebody
    /// else owns. The buffer is field 0 of the growable array's triple.
    pub(crate) loop_buffers: Vec<usize>,
    /// A `drop` whose name no binding in scope answers: the name and the
    /// line. The core has no row for it; [`crate::typed::drops`] refuses it.
    pub unbound_drops: Vec<(String, usize)>,
    /// A rule the checker let through that the builder met while lowering
    /// its construct: the line and the sentence. An unknown name is one; the
    /// builder binds the checker's `Err` there. [`crate::typed::refused`]
    /// refuses each.
    pub refused: Vec<(usize, String)>,
    /// The same for a rule about the checker's types that holds as written,
    /// not per instance: a non-Bool condition, a `for` over what no loop walks.
    pub mistyped: Vec<(usize, String)>,
}

/// The shape of a candidate construct, which [`last_owner`] asks.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cand {
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
        row.as_deref().unwrap_or(&self.names[n as usize].holes)
    }

    /// How many times each name is read, which decides whether an emitter
    /// may leave a value on the operand stack rather than in a local.
    pub fn reads(&self) -> Vec<u32> {
        let mut out = vec![0u32; self.names.len()];
        count_reads(&self.stmts, &mut out);
        out
    }

    /// How many times a row names each name, bindings and releases included.
    /// [`extent_ends`] compares a list against it to know that no row outside
    /// the list names a name.
    pub fn occurrences(&self) -> Vec<u32> {
        let mut ns = Vec::new();
        self.stmts.iter().for_each(|s| names_in(s, &mut ns));
        let mut out = vec![0u32; self.names.len()];
        for n in ns {
            out[n as usize] += 1;
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
        let i = &self.names[n as usize];
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
            }
        }
    }
}

/// A construct this pass does not lower. The instance is neither accepted nor
/// refused; the corpus test counts these by `what`.
#[derive(Debug, Clone)]
pub struct Gap {
    pub what: &'static str,
    /// A callee's or a binding's name; empty when `what` says it all.
    pub detail: String,
    pub line: usize,
    /// Set when this is a rule the program breaks rather than a gap: the
    /// checker's sentence, which the placer reports as a refusal. A rule
    /// about a keyword lives here because the kernel has none
    /// (`consume make()` and `make()` are one value to it).
    pub rule: Option<String>,
}

/// Whether `rhs` is a validated type's constructor over a literal, which
/// hands the literal back.
fn over_a_literal(rhs: &Rhs) -> bool {
    matches!(rhs, Rhs::Call { kind: Callee::Named | Callee::Proven, args, .. }
        if matches!(args.as_slice(), [(Arg::Val(Val::Lit(l)), _)] if !matches!(l, Lit::Opaque(_))))
}

/// A field read or an element read, whatever its receiver: the reads whose
/// receiver [`Builder::place`] binds to a temporary when it names no place.
fn reads_a_part(e: &Expr) -> bool {
    match e {
        Expr::Field { .. } => true,
        Expr::Call { name, args, .. } => name == "@at" && args.len() == 2,
        _ => false,
    }
}

/// Refuses a `consume` whose operand names no place. `by_loop` picks the
/// wording for `for x in consume xs` over a prefix `consume`
/// (`movecheck::TakeForm`). Both rules are syntactic, so they hold for
/// heapless types too.
fn take_names_a_place(e: &Expr, line: usize, by_loop: bool) -> Result<(), Gap> {
    if vyrn_frontend::ast::place_path(e).is_some() {
        return Ok(());
    }
    if let Some((root, path)) = vyrn_frontend::project::element_path(e) {
        return refuse(
            format!(
                "`{path}` may not be taken — an element is not a place a take reaches\n  fix: \
                 `{root}.swapRemove(..)` returns the element and leaves the container one shorter"
            ),
            line,
        );
    }
    let (says, drop_it) = if by_loop {
        (
            "`consume` here has nothing to take — the loop already owns a container that is \
             not a binding",
            "drop the `consume`: the elements are already owned",
        )
    } else {
        (
            "`consume` here has nothing to take — the value is already owned, so there is no \
             place to leave a hole in",
            "drop the `consume`: the value is already owned",
        )
    };
    refuse(format!("{says}\n  fix: {drop_it}"), line)
}

/// The scrutinee a binder borrows: its name, where the construct does not
/// own it. `None` where it does, whose payloads are the construct's to give.
fn borrow_root(sv: &Val, owns: bool) -> Option<Name> {
    match sv {
        Val::Name(n) if !owns => Some(*n),
        _ => None,
    }
}

/// The sites of the candidate constructs that are their value's last owner,
/// decided over the first build's core.
///
/// A construct takes its value where nothing reads the name after it (a
/// payload binder is a name of its own) and where the binding and the
/// construct stand under the same loops, so one value is not taken twice. A
/// [`Cand::Switch`] takes where the name's last read is the switch's own; a
/// [`Cand::Loop`] where the last read is deeper than the binding, so inside
/// the loop.
///
/// A release row is not a read. The rows are derived from the take; letting
/// a row decide the take made the answer depend on rows the placer had just
/// added, and the second build then seeded a different take.
fn last_owner(top: &Body) -> std::collections::HashSet<usize> {
    let mut out = std::collections::HashSet::new();
    for f in top.frames() {
        if f.cands.is_empty() {
            continue;
        }
        let mut w = Reads {
            last: vec![0; f.names.len()],
            deep: vec![0; f.names.len()],
            bound: vec![usize::MAX; f.names.len()],
            handed: vec![false; f.names.len()],
            switches: Vec::new(),
            order: 0,
            depth: 0,
        };
        w.stmts(&f.stmts, 0);
        for p in &f.params {
            w.bound[*p as usize] = 0;
        }
        for (site, n, kind) in &f.cands {
            let takes = match kind {
                Cand::Switch => {
                    let at = w.switches.iter().find(|(s, m, _, _)| s == site && m == n);
                    at.is_some_and(|(_, _, depth, order)| {
                        w.last[*n as usize] == *order && w.bound[*n as usize] == *depth
                    })
                }
                Cand::Loop => {
                    w.bound[*n as usize] != usize::MAX && w.deep[*n as usize] > w.bound[*n as usize]
                }
                Cand::Elem => w.handed[*n as usize],
            };
            if takes {
                out.insert(*site);
            }
        }
    }
    out
}

/// The name a place is rooted at, as a value. `None` for module state.
fn root_name(p: &Place) -> Option<Val> {
    match p {
        Place::Name(n) => Some(Val::Name(*n)),
        Place::Global(_) => None,
        Place::Field(b, _) | Place::Elem(b, _) | Place::Key(b, _) => root_name(b),
    }
}

/// One frame's last read of each name, the loop depth that read stood at,
/// the loop depth each name was bound at, and every switch over a bare name
/// with its depth and the order of its own read. See [`last_owner`].
struct Reads {
    last: Vec<usize>,
    deep: Vec<usize>,
    bound: Vec<usize>,
    /// Whether the name was handed on rather than only read ([`Cand::Elem`]).
    handed: Vec<bool>,
    switches: Vec<(usize, Name, usize, usize)>,
    order: usize,
    depth: usize,
}

impl Reads {
    /// A value in a position that transfers it: a declared `consume`
    /// argument, a part of a literal, a stored value, a map key a store
    /// writes at, a returned value.
    fn hand(&mut self, v: &Val) {
        if let Val::Name(n) = v {
            self.handed[*n as usize] = true;
        }
        self.val(v);
    }

    /// The place of a store: the key it writes at is handed to the container.
    fn store_place(&mut self, p: &Place) {
        match p {
            Place::Key(b, v) => {
                self.place(b);
                self.hand(v);
            }
            Place::Field(b, _) | Place::Elem(b, _) => self.store_place(b),
            _ => self.place(p),
        }
        if let Place::Elem(_, v) = p {
            self.val(v);
        }
    }

    fn val(&mut self, v: &Val) {
        self.order += 1;
        if let Val::Name(n) = v {
            self.last[*n as usize] = self.order;
            self.deep[*n as usize] = self.depth;
        }
    }

    fn place(&mut self, p: &Place) {
        match p {
            Place::Name(n) => {
                self.order += 1;
                self.last[*n as usize] = self.order;
                self.deep[*n as usize] = self.depth;
            }
            Place::Global(_) => {}
            Place::Field(b, _) => self.place(b),
            Place::Elem(b, v) | Place::Key(b, v) => {
                self.place(b);
                self.val(v);
            }
        }
    }

    fn rhs(&mut self, r: &Rhs) {
        match r {
            Rhs::Val(v) => self.val(v),
            Rhs::Read(p) => self.place(p),
            // A take out of a sub-place hands that part on, so what is left
            // is this turn's: `out.push(consume p.value)` in a `for p in ..`.
            Rhs::Take(p) => {
                self.place(p);
                if let Some(Val::Name(n)) = root_name(p) {
                    self.handed[n as usize] = true;
                }
            }
            Rhs::Call { args, kind, .. } => {
                if let Some(f) = kind.value() {
                    self.val(&Val::Name(f));
                }
                for (a, c) in args {
                    match a {
                        Arg::Place(p) => self.place(p),
                        Arg::Val(v) if *c == vyrn_frontend::ast::Capability::Consume => {
                            self.hand(v)
                        }
                        Arg::Val(v) => self.val(v),
                    }
                }
            }
            Rhs::Prim(_, vs, _) => {
                for v in vs {
                    self.val(v);
                }
            }
            Rhs::Make(_, vs) => {
                for v in vs {
                    self.hand(v);
                }
            }
        }
    }

    fn name(&mut self, n: Name) {
        self.order += 1;
        self.last[n as usize] = self.order;
        self.deep[n as usize] = self.depth;
    }

    fn stmts(&mut self, stmts: &[St], depth: usize) {
        for st in stmts {
            self.depth = depth;
            match st {
                St::Let(n, r) => {
                    self.rhs(r);
                    self.bound[*n as usize] = depth;
                }
                St::Store { place, value, .. } => {
                    self.store_place(place);
                    self.hand(value);
                }
                St::Drop(n, _, _, _) => self.name(*n),
                // A row is the plan's, not the core's: see [`last_owner`].
                St::Row { .. } => {}
                St::If {
                    cond, then, els, ..
                } => {
                    self.val(cond);
                    self.stmts(then, depth);
                    self.stmts(els, depth);
                }
                St::Loop { body: b, .. } => self.stmts(b, depth + 1),
                St::Block { body, .. } => self.stmts(body, depth),
                St::Return { value: Some(v), .. } => self.hand(v),
                St::Switch { on, arms, .. } => {
                    arms.iter()
                        .filter_map(|a| a.test.reads())
                        .for_each(|n| self.val(&Val::Name(n)));
                    self.val(on);
                    if let (Val::Name(n), Some(a)) = (on, arms.first()) {
                        self.switches.push((a.site, *n, depth, self.order));
                    }
                    for a in arms {
                        for b in &a.binds {
                            self.bound[*b as usize] = depth;
                        }
                        // The rows that read a binder out of the scrutinee
                        // are the binder's, not a read of the scrutinee.
                        self.stmts(&a.body[a.reads(on).len()..], depth);
                    }
                }
                St::Do { rhs: r, .. } => self.rhs(r),
                _ => {}
            }
        }
    }
}

/// A `match`'s head line and the last line an arm's value starts on, for
/// [`Builder::takes_scrutinee`]. A block arm yields no value and
/// adds no line.
fn arms_span(line: usize, arms: &[MatchArm]) -> (usize, usize) {
    let last = arms.iter().fold(line, |m, a| match &a.body {
        ArmBody::Expr(e) => m.max(e.line()),
        ArmBody::Block(_) => m,
    });
    (line, last)
}

/// The kind of an expression, for a gap's detail: [`crate::kind`], except that
/// the five literals answer as one and a builtin call is named apart.
fn expr_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => "literal",
        Expr::Call { name, .. } if name.starts_with('@') => "builtin call",
        _ => crate::kind(e),
    }
}

fn gap<T>(what: &'static str, line: usize) -> Result<T, Gap> {
    Err(Gap {
        what,
        detail: String::new(),
        line,
        rule: None,
    })
}

fn gap_d<T>(what: &'static str, detail: &str, line: usize) -> Result<T, Gap> {
    Err(Gap {
        what,
        detail: detail.to_string(),
        line,
        rule: None,
    })
}

/// A rule the program breaks. Lowering stops as at a gap, and the placer
/// reports `message` at `line` as it reports the kernel's refusals.
fn refuse<T>(message: String, line: usize) -> Result<T, Gap> {
    Err(Gap {
        what: "a rule the program breaks",
        detail: String::new(),
        line,
        rule: Some(message),
    })
}

/// What a builtin's specification row states about its operands and its
/// result. A builtin with such a row is a `call`, not a gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    /// Each operand at the stated type, in order, and a result at the stated
    /// type. The emitter writes one instruction between them.
    Typed(Vec<Type>, Type),
    /// One operand, at whatever type the row put on the name it reads, and a
    /// result of that same type.
    OwnType,
    /// One operand at its own type, handed back as the same value, not a
    /// copy, behind an optimization barrier (`blackBox`).
    Barrier,
    /// One operand at the type the row put on its name, and a result at the
    /// stated type (`print`, `@str`). The checker types the operand over a
    /// union; the emitter picks the rendering by the operand's type.
    Renders(Type),
    /// A message at `String`, and for `@panicAt` the site as a string
    /// literal. The call never returns; the [`St::Trap`] after it ends the
    /// path. `serveStream`'s message is the frontend's sentence, a literal in
    /// place of the stream, which a compiled build never pulls.
    Traps,
    /// `assert(c)` and `assertEq(a, b)`: a `Bool`, or two operands
    /// at one scalar type. A failure writes the interpreter's line and traps;
    /// the result is `Unit`.
    Asserts,
    /// A receiver first, rebuilt in place by the runtime; the store after the
    /// call puts the result back into the name.
    ///
    /// An array receiver takes at most one operand at its name's type; the
    /// result is the receiver's own storage. `@strAppend`'s receiver is a
    /// String accumulator ([`NameInfo::grows`]): the runtime grows the buffer
    /// when the ownership word says this path allocated it, and copies it
    /// otherwise. `@tally` takes a map, a key at its key type and an `Int64`
    /// count; it reads the key and never takes it (a miss stores a copy), so
    /// the caller releases its key on both paths. `@tallyBytes` reads bytes
    /// instead, and a miss stores a String built from them.
    Rebuilds,
    /// A SIMD operation. The vector operand's type chooses the
    /// instruction; a lane index is a literal the checker proved in range.
    Lanes,
    /// A host import a compiled generator calls:
    /// `@codeText`, `raw`, `rawAt` and `@codeSplice` answer a `Code` handle,
    /// `render` answers a String, and `__vyrnGen*` move a value out of the
    /// host as atoms. `@codeSplice`'s tag is its operand's type. Only a
    /// generator reaches one.
    Host,
    /// A call to the named function, which the program links: an entry a
    /// generator host synthesizes, or a `std` function a builtin routes to
    /// (`loader::RT_MODULES`). The emitter reads it as a declared callee.
    Routes(&'static str),
    /// A receiver the call shrinks in its own storage. `@pop` takes an array
    /// and hands back an `Option` of the last element, `@swapRemove` an
    /// array and an index at `Int64` and hands back the element there.
    /// `@remove` takes a map and a key at the map's key type, releases the
    /// key and value the entry held, and answers whether it held one.
    Removes,
    /// A map and a key at the map's key type. The answer is a `Bool`: whether
    /// the map holds an entry for the key.
    Finds,
    /// Operands at their names' types, and a result the call builds in its
    /// own storage, at the stated type with parameters the operands solve.
    Builds(Type),
    /// One operand at its name's type, and a result at the stated type. The
    /// call releases it (`close`), or boxes it and answers the box's address
    /// (`boxStream`).
    Effect(Type),
    /// The logging facade. `logger(name)` hands its String back as the
    /// `Logger`. A level takes a `Logger` and a message and writes the line,
    /// or nothing below the build's threshold; both operands are evaluated
    /// either way.
    Logs,
    /// The head of a `for` over a stream: advances the receiver in place and
    /// answers whether an element came. A read of the stream at the call's
    /// own name is that element.
    Pulls,
}

/// Every builtin the row specifies, by name.
///
/// The emitter answers each from the row alone
/// (`vyrn_codegen::direct::Fn_::core_call`), so a name added here needs an
/// emission there. The codegen test `builtin_rows_all_emit` refuses a
/// [`Spec::Typed`] row with no instruction.
pub fn builtin_rows() -> &'static [(&'static str, Spec)] {
    static ROWS: std::sync::OnceLock<Vec<(&'static str, Spec)>> = std::sync::OnceLock::new();
    ROWS.get_or_init(|| {
        let u64_ = Type::IntN {
            bits: 64,
            signed: false,
        };
        let i32_ = Type::IntN {
            bits: 32,
            signed: true,
        };
        let u8_ = Type::IntN {
            bits: 8,
            signed: false,
        };
        let one = |n, p: &Type, r: &Type| (n, Spec::Typed(vec![p.clone()], r.clone()));
        let two = |n, p: &Type, r: &Type| (n, Spec::Typed(vec![p.clone(), p.clone()], r.clone()));
        let (f4, d2) = (Type::F32x4, Type::F64x2);
        vec![
            // IEEE-754 bit views: the same 64 bits read at the other type, so
            // neither is a conversion.
            one("floatBits", &Type::Float, &u64_),
            one("floatFromBits", &u64_, &Type::Float),
            one("@f32x4Splat", &Type::Float32, &f4),
            one("@i32x4Splat", &i32_, &Type::I32x4),
            one("@f64x2Splat", &Type::Float, &d2),
            two("@f32x4Min", &f4, &f4),
            two("@f32x4Max", &f4, &f4),
            one("@f32x4Sqrt", &f4, &f4),
            one("@f32x4Ceil", &f4, &f4),
            one("@f32x4Floor", &f4, &f4),
            one("@f32x4Trunc", &f4, &f4),
            one("@f32x4Nearest", &f4, &f4),
            two("@f64x2Min", &d2, &d2),
            two("@f64x2Max", &d2, &d2),
            one("@f64x2Sqrt", &d2, &d2),
            ("@copy", Spec::OwnType),
            ("blackBox", Spec::Barrier),
            ("print", Spec::Renders(Type::Unit)),
            ("@str", Spec::Renders(Type::Str)),
            ("panic", Spec::Traps),
            ("assert", Spec::Asserts),
            ("assertEq", Spec::Asserts),
            (vyrn_frontend::ast::PANIC_AT, Spec::Traps),
            ("serveStream", Spec::Traps),
            ("close", Spec::Effect(Type::Unit)),
            ("boxStream", Spec::Effect(Type::Int)),
            ("logger", Spec::Logs),
            ("@trace", Spec::Logs),
            ("@debug", Spec::Logs),
            ("@info", Spec::Logs),
            ("@warn", Spec::Logs),
            ("@error", Spec::Logs),
            ("@push", Spec::Rebuilds),
            ("@reserve", Spec::Rebuilds),
            ("@clear", Spec::Rebuilds),
            ("@append", Spec::Rebuilds),
            ("@copyFrom", Spec::Rebuilds),
            ("@strAppend", Spec::Rebuilds),
            ("@tally", Spec::Rebuilds),
            ("@tallyBytes", Spec::Rebuilds),
            ("F32x4", Spec::Lanes),
            ("I32x4", Spec::Lanes),
            ("F64x2", Spec::Lanes),
            ("@lane", Spec::Lanes),
            ("@replaceLane", Spec::Lanes),
            ("@anyTrue", Spec::Lanes),
            ("@allTrue", Spec::Lanes),
            ("@f32x4Load", Spec::Lanes),
            ("@f32x4Store", Spec::Lanes),
            ("@i32x4Load", Spec::Lanes),
            ("@i32x4Store", Spec::Lanes),
            ("@f64x2Load", Spec::Lanes),
            ("@f64x2Store", Spec::Lanes),
            ("@codeText", Spec::Host),
            ("@codeSplice", Spec::Host),
            ("raw", Spec::Host),
            ("rawAt", Spec::Host),
            ("render", Spec::Host),
            (vyrn_frontend::checker::GEN_REFLECT, Spec::Host),
            (vyrn_frontend::checker::GEN_NEXT_INT, Spec::Host),
            (vyrn_frontend::checker::GEN_NEXT_STR, Spec::Host),
            (
                "moduleInterface",
                Spec::Routes(vyrn_frontend::checker::GEN_ENTRY_MODULE_INTERFACE),
            ),
            ("lex", Spec::Routes(vyrn_frontend::checker::GEN_ENTRY_LEX)),
            ("@pull", Spec::Pulls),
            (
                "pullAt",
                Spec::Builds(Type::option(Type::Param("T".into()))),
            ),
            ("@pop", Spec::Removes),
            ("@swapRemove", Spec::Removes),
            ("@remove", Spec::Removes),
            ("@has", Spec::Finds),
            ("bytes", Spec::Builds(Type::Array(Box::new(u8_.clone())))),
            (
                "stringFromBytes",
                Spec::Builds(Type::result(Type::Str, Type::Str)),
            ),
            (
                "@toArray",
                Spec::Builds(Type::Array(Box::new(Type::Param("T".into())))),
            ),
            // A stream's producers build its six-word header.
            (
                "fromArray",
                Spec::Builds(Type::Stream(Box::new(Type::Param("T".into())))),
            ),
            (
                "fromStep",
                Spec::Builds(Type::Stream(Box::new(Type::Param("T".into())))),
            ),
            (
                "unboxStream",
                Spec::Builds(Type::Stream(Box::new(Type::Param("T".into())))),
            ),
            // `K` is the prelude's own parameter name.
            (
                "@keys",
                Spec::Builds(Type::Array(Box::new(Type::Param("K".into())))),
            ),
        ]
        .into_iter()
        .chain(
            vyrn_frontend::loader::RT_MODULES
                .iter()
                .flat_map(|rt| rt.routes)
                .chain(vyrn_frontend::loader::GEN_ROUTES)
                .map(|(builtin, f)| (*builtin, Spec::Routes(*f))),
        )
        .collect()
    })
}

/// The row [`builtin_rows`] holds for `name`. A routed name has a row per
/// route; this build takes [`vyrn_frontend::loader::routed_builtin`]'s.
pub fn builtin_row(name: &str) -> Option<&'static Spec> {
    let routed = vyrn_frontend::loader::routed_builtin(name);
    builtin_rows()
        .iter()
        .find(|(n, s)| *n == name && routed.is_none_or(|f| matches!(s, Spec::Routes(g) if *g == f)))
        .map(|(_, s)| s)
}

/// The shapes among `body`'s rows that no emitter reads from the core, in
/// source order, each once; empty means the rows carry the body end to end.
/// Tags: `Call:<who>:<name>` for a callee the emitter's function table does
/// not answer, `Read:<kind>` and `Take:<kind>` for a place, `Opaque:<what>`
/// for a row that names no value, and `Lambda`. `tests/coredrive.rs` ranks
/// them, and `VYRN_GAP_TALLY` tables them.
pub fn gaps(body: &Body) -> Vec<String> {
    let mut out = Vec::new();
    gaps_of(body, &body.stmts, &mut out);
    let mut seen = std::collections::HashSet::new();
    out.retain(|t| seen.insert(t.clone()));
    out
}

/// Ends every list of `ss` at its `trap`, because nothing after one runs.
/// An `if` or a `switch` whose every arm ends at one is a `trap` too.
///
/// Builders extend a list after a `panic` in value position (the join's
/// store, the call it feeds, an arm's releases), and a builder cannot cut its
/// own list because its caller extends it afterwards.
fn cut(ss: &mut Vec<St>) {
    for s in ss.iter_mut() {
        match s {
            St::If { then, els, .. } => {
                cut(then);
                cut(els);
            }
            St::Loop { body, .. } | St::Block { body, .. } => cut(body),
            St::Switch { arms, .. } => arms.iter_mut().for_each(|a| cut(&mut a.body)),
            St::Let(..)
            | St::Do { .. }
            | St::Store { .. }
            | St::Drop(..)
            | St::Row { .. }
            | St::Return { .. }
            | St::Break { .. }
            | St::Continue { .. }
            | St::Trap => {}
        }
    }
    if let Some(i) = ss.iter().position(traps) {
        ss.truncate(i + 1);
    }
}

/// Whether every path through `s` ends at a `trap`. A `loop` or a `block`
/// may be left by a `break` before its last row, so neither is one.
fn traps(s: &St) -> bool {
    let ends = |ss: &[St]| ss.last().is_some_and(traps);
    match s {
        St::Trap => true,
        St::If { then, els, .. } => ends(then) && ends(els),
        St::Switch { arms, .. } => !arms.is_empty() && arms.iter().all(|a| ends(&a.body)),
        _ => false,
    }
}

/// Whether every path through `ss` ends in a `return` or a `trap`: the rule
/// "must return T on all paths", stated once for a function and a lambda.
/// A row after one that ends is never reached. A loop may run zero times or
/// be left by a `break`, so it ends nothing.
fn returns(ss: &[St]) -> bool {
    ss.iter().any(|s| match s {
        St::Return { .. } | St::Trap => true,
        St::If { then, els, .. } => returns(then) && returns(els),
        St::Switch { arms, .. } => !arms.is_empty() && arms.iter().all(|a| returns(&a.body)),
        St::Block { body, .. } => returns(body),
        St::Let(..)
        | St::Do { .. }
        | St::Store { .. }
        | St::Drop(..)
        | St::Row { .. }
        | St::Loop { .. }
        | St::Break { .. }
        | St::Continue { .. } => false,
    })
}

/// The refusal of a frame that can end without the value it owes.
fn falls_through(body: &mut Body, owes: &Type, line: usize, what: impl FnOnce() -> String) {
    if *owes != Type::Unit && !returns(&body.stmts) {
        body.refused
            .push((line, format!("{} must return {owes} on all paths", what())));
    }
}

fn gaps_of(body: &Body, ss: &[St], out: &mut Vec<String>) {
    for s in ss {
        match s {
            St::Let(_, r) | St::Do { rhs: r, .. } => gaps_rhs(body, r, out),
            St::Store { place, value, .. } => {
                gaps_place(place, out);
                gaps_val(value, out);
            }
            St::Drop(..) | St::Row { .. } => {}
            St::If {
                cond, then, els, ..
            } => {
                gaps_val(cond, out);
                gaps_of(body, then, out);
                gaps_of(body, els, out);
            }
            St::Loop { body: b, .. } | St::Block { body: b, .. } => gaps_of(body, b, out),
            St::Break { .. } | St::Continue { .. } | St::Trap => {}
            St::Return { value, .. } => {
                if let Some(v) = value {
                    gaps_val(v, out);
                }
            }
            St::Switch { on, arms, .. } => {
                gaps_val(on, out);
                for a in arms {
                    gaps_of(body, &a.body, out);
                }
            }
        }
    }
}

fn gaps_rhs(body: &Body, r: &Rhs, out: &mut Vec<String>) {
    let vals = |vs: &[Val], out: &mut Vec<String>| {
        for v in vs {
            gaps_val(v, out);
        }
    };
    match r {
        Rhs::Val(v) => gaps_val(v, out),
        // A take of a key has no reader; every other place read or take has.
        Rhs::Read(p) => gaps_place(p, out),
        Rhs::Take(p) => {
            if matches!(p, Place::Key(..)) {
                out.push("Take:Key".into());
            }
            gaps_place(p, out);
        }
        Rhs::Call {
            callee, args, kind, ..
        } => {
            // The emitter reads a declared function, a constructor, a builtin
            // with a row, and a call through a stored value (one call to its
            // signature's dispatcher). A call through a `fn`-typed parameter
            // waits on the specialization.
            if !matches!(
                kind,
                Callee::Fn | Callee::Ctor | Callee::Named | Callee::Proven
            ) && builtin_row(callee).is_none()
                && !kind.value().is_some_and(|n| !body.params.contains(&n))
            {
                let tag = match kind {
                    Callee::Value(_) => "Value".to_string(),
                    k => format!("{k:?}"),
                };
                out.push(format!("Call:{tag}:{callee}"));
            }
            for (a, _) in args {
                match a {
                    Arg::Val(v) => gaps_val(v, out),
                    Arg::Place(p) => gaps_place(p, out),
                }
            }
        }
        Rhs::Prim(Op::Closure(_), vs, _) => {
            out.push("Lambda".into());
            vals(vs, out);
        }
        Rhs::Prim(_, vs, _) => vals(vs, out),
        // A part the emitter cannot place is the emitter's own screen, not a gap.
        Rhs::Make(_, vs) => vals(vs, out),
    }
}

fn gaps_val(v: &Val, out: &mut Vec<String>) {
    if let Val::Lit(Lit::Opaque(k)) = v {
        out.push(format!("Opaque:{k:?}"));
    }
}

fn gaps_place(p: &Place, out: &mut Vec<String>) {
    match p {
        Place::Name(_) | Place::Global(_) => {}
        Place::Field(b, _) => gaps_place(b, out),
        Place::Elem(b, v) | Place::Key(b, v) => {
            gaps_place(b, out);
            gaps_val(v, out);
        }
    }
}

/// Where the gap tally is appended, or `None` when nothing asked for one.
fn gap_tally_at() -> Option<&'static std::path::Path> {
    static AT: std::sync::OnceLock<Option<std::path::PathBuf>> = std::sync::OnceLock::new();
    AT.get_or_init(|| std::env::var_os("VYRN_GAP_TALLY").map(std::path::PathBuf::from))
        .as_deref()
}

thread_local! {
    /// The lines already appended. A body is built more than once (seeded,
    /// and by every host), and the histogram counts bodies.
    static SAID: std::cell::RefCell<std::collections::HashSet<String>> =
        std::cell::RefCell::new(std::collections::HashSet::new());
}

/// Appends one line per body of `out`: the module, the function, the first gap,
/// every gap, `judged` for a body no emitter reads ([`Instance::judged_only`])
/// or `emitted`, and the command that ran. A body the rows carry whole reads
/// `-` in both gap fields.
fn tally_gaps(inst: &Instance<'_>, out: &Result<Body, Gap>) {
    let file = inst.func.module.as_deref().unwrap_or("(the root)");
    let reach = if inst.judged_only() {
        "judged"
    } else {
        "emitted"
    };
    let mut lines: Vec<String> = Vec::new();
    match out {
        Err(g) => {
            // The field is space-separated, so the gap's words are joined.
            let what = format!(
                "Gap:{}{}",
                g.what.replace(' ', "-"),
                if g.detail.is_empty() {
                    String::new()
                } else {
                    format!(":{}", g.detail)
                }
            );
            lines.push(format!("{file}\t{}\t{what}\t{what}", inst.spelling()));
        }
        Ok(body) => {
            for f in body.frames() {
                let g = gaps(f);
                // A body with no gap is a line too: it is in every table's
                // denominator.
                lines.push(format!(
                    "{file}\t{}\t{}\t{}",
                    f.name,
                    g.first().map_or("-", |t| t.as_str()),
                    if g.is_empty() {
                        "-".into()
                    } else {
                        g.join(" ")
                    }
                ));
            }
        }
    }
    static ARGV: std::sync::OnceLock<String> = std::sync::OnceLock::new();
    let argv = ARGV.get_or_init(|| std::env::args().collect::<Vec<_>>().join(" "));
    for line in lines {
        let line = format!("{line}\t{reach}\t{argv}\n");
        if !SAID.with(|s| s.borrow_mut().insert(line.clone())) {
            continue;
        }
        if let Ok(mut f) = std::fs::OpenOptions::new()
            .append(true)
            .create(true)
            .open(gap_tally_at().unwrap())
        {
            use std::io::Write;
            let _ = f.write_all(line.as_bytes());
        }
    }
}

/// Builds the core of one instance. The first build records candidates and
/// takes nothing; where [`last_owner`] names any, a second build takes them.
pub fn build(program: &Program, inst: &Instance<'_>, own: &Ownership) -> Result<Body, Gap> {
    let out = build_twice(program, inst, own);
    if gap_tally_at().is_some() {
        tally_gaps(inst, &out);
    }
    out
}

fn build_twice(program: &Program, inst: &Instance<'_>, own: &Ownership) -> Result<Body, Gap> {
    let none = std::collections::HashSet::new();
    let b1 = vyrn_frontend::prof::phase("placer: build: first");
    let first = build_seeded(program, inst, own, &none)?;
    drop(b1);
    let seed = last_owner(&first);
    if seed.is_empty() {
        return Ok(first);
    }
    let _b2 = vyrn_frontend::prof::phase("placer: build: seeded");
    build_seeded(program, inst, own, &seed)
}

/// The refusals the typed judgment states over the checker's answers at the
/// expressions of `facts`: a literal that does not fit, a shift by a constant out of
/// range, and at a node the checker typed `Err` whose operands it typed, the
/// rule the node breaks (an operator, a field, a construction, a variant, a
/// record literal, or a call's arity, type arguments and arguments).
fn judged(facts: &NodeTypes<'_>, decls: &HashMap<String, TypeDecl>) -> Vec<(usize, String)> {
    let recorded = |e: &Expr| {
        facts
            .types
            .get(&(e as *const Expr as usize))
            .filter(|t| **t != Type::Err)
    };
    let resolved = |e: &Expr| recorded(e).map(|t| vyrn_frontend::types::resolve(t, decls));
    // Rules over a literal as written, whatever the checker typed it, so that
    // `vyrn check` refuses what the build would.
    let constant = |e: &Expr| -> Option<String> {
        use vyrn_frontend::checker::{int_literal_value, literal_value, misfit};
        let sized = |e: &Expr| match resolved(e)? {
            Type::IntN { bits, signed } => Some((bits, signed)),
            _ => None,
        };
        match e {
            Expr::Int(n) => sized(e).and_then(|(b, s)| misfit("integer", literal_value(*n), b, s)),
            Expr::Byte(v) => sized(e).and_then(|(b, s)| misfit("byte", i128::from(*v), b, s)),
            Expr::Binary {
                op: BinOp::Match,
                rhs,
                ..
            } => Some(match &**rhs {
                Expr::Str(pat) => {
                    let err = vyrn_frontend::regex::compile(pat).err()?;
                    format!("invalid regex `{pat}`: {err}")
                }
                _ => "the right side of `=~` must be a string-literal pattern".to_string(),
            }),
            // A literal operand takes a sized sibling's type
            // (`Checker::expr`'s `adapt_int_literal`), the left one first.
            Expr::Binary { lhs, rhs, .. } => {
                [(lhs, rhs), (rhs, lhs)].into_iter().find_map(|(l, o)| {
                    let v = int_literal_value(l).filter(|_| resolved(l) == Some(Type::Int))?;
                    sized(o).and_then(|(b, s)| misfit("integer", v, b, s))
                })
            }
            Expr::ArrayLit { elems, .. } => {
                let limit = vyrn_frontend::trap::ARRAY_LIT_LIMIT;
                if elems.len() > limit {
                    return Some(format!(
                        "this array literal has {} elements, past the limit of {limit}\n  \
                         note: a literal is lowered element by element into one call frame, so \
                         its length is a compile-time cost on both backends\n  \
                         note: a table this long belongs in a file the program reads, not in \
                         the program",
                        elems.len()
                    ));
                }
                match resolved(e)? {
                    Type::SmallArray(_, n) if elems.len() > n => Some(format!(
                        "this literal has {} elements but the slot is SmallArray<_, {n}>",
                        elems.len()
                    )),
                    _ => None,
                }
            }
            _ => None,
        }
    };
    let refusal = |e: &Expr| -> Option<String> {
        if let Expr::Binary {
            op: BinOp::Shl | BinOp::Shr,
            rhs,
            ..
        } = e
        {
            let bits = match resolved(e)? {
                Type::IntN { bits, .. } => i64::from(bits),
                _ => 64,
            };
            let Some(vyrn_frontend::consteval::ConstVal::Int(amt)) =
                vyrn_frontend::consteval::eval(rhs, &HashMap::new())
            else {
                return None;
            };
            return (amt < 0 || amt >= bits).then(|| {
                format!(
                    "shift amount {amt} is out of range for a {bits}-bit value (valid range is 0..{bits})"
                )
            });
        }
        if let Some(s) = constant(e) {
            return Some(s);
        }
        if facts.types.get(&(e as *const Expr as usize)) != Some(&Type::Err) {
            return None;
        }
        match e {
            Expr::Unary { op, expr, .. } => {
                let t = resolved(expr)?;
                Some(match op {
                    UnOp::Neg => format!("unary `-` needs a numeric type, found {t}"),
                    UnOp::Not => format!("unary `!` needs Bool, found {t}"),
                    UnOp::BitNot => format!("unary `~` needs an integer type, found {t}"),
                })
            }
            Expr::Field { expr, field, .. } => Some(match resolved(expr)? {
                Type::Record(_) => format!("type {} has no field `{field}`", recorded(expr)?),
                Type::Str if field == "length" => "String has no `length`: use `byteLength` for \
                                                   bytes or `charCount()` for Unicode scalars"
                    .to_string(),
                other => format!("cannot access field `{field}` on non-record type {other}"),
            }),
            Expr::TryConstruct { name, args, .. } | Expr::Call { name, args, .. }
                if decls.contains_key(name) =>
            {
                let base = &decls[name].base;
                let tries = matches!(e, Expr::TryConstruct { .. });
                if tries && !matches!(base, Type::Int | Type::Bool | Type::Str) {
                    return Some(format!(
                        "`{name}?(..)` is only for validated/nominal scalar types"
                    ));
                }
                let [arg] = &args[..] else {
                    return Some(if tries {
                        format!("`{name}?` takes 1 argument, got {}", args.len())
                    } else {
                        format!("`{name}` construction takes 1 argument, got {}", args.len())
                    });
                };
                let aty = recorded(arg)?;
                Some(format!(
                    "`{name}` is built from {base}, but the argument is {aty}"
                ))
            }
            // Fields in written order, so the refusal names the first one the
            // reader sees; a missing field only once every value typed.
            Expr::StructLit { name, fields, .. } => {
                if !decls.contains_key(name) {
                    return Some(format!("unknown type `{name}`"));
                }
                let Some(declared) =
                    vyrn_frontend::types::record_fields(&Type::Named(name.clone()), decls)
                else {
                    return Some(format!("`{name}` is not a record type"));
                };
                for (k, (fname, _)) in fields.iter().enumerate() {
                    if !declared.iter().any(|f| &f.name == fname) {
                        return Some(format!("record `{name}` has no field `{fname}`"));
                    }
                    if fields[..k].iter().any(|(g, _)| g == fname) {
                        return Some(format!("field `{fname}` set twice"));
                    }
                }
                fields
                    .iter()
                    .try_for_each(|(_, v)| recorded(v).map(|_| ()))?;
                let f = declared
                    .iter()
                    .find(|f| fields.iter().all(|(g, _)| *g != f.name))?;
                Some(format!("missing field `{}` for `{name}`", f.name))
            }
            Expr::Var { name, .. } => {
                let payload = decls
                    .values()
                    .filter_map(|d| vyrn_frontend::types::declared_variants(&d.base))
                    .flatten()
                    .find(|v| &v.name == name)?
                    .payload
                    .len();
                Some(format!("variant `{name}` needs {payload} argument(s)"))
            }
            Expr::Call {
                args,
                type_args,
                dot,
                ..
            } => {
                let d = call_decl(e as *const Expr as usize)?;
                let shown = &d.shown;
                // Counts are of what the reader wrote after the dot (#577). A
                // callee with no parameters has no receiver slot.
                let dot = usize::from(*dot && !d.params.is_empty());
                if d.params.len() != args.len() {
                    return Some(format!(
                        "`{shown}` expects {} argument(s), got {}",
                        d.params.len() - dot,
                        args.len() - dot
                    ));
                }
                // A type argument to a callee that declares none would look
                // honoured if accepted. A method call reads none.
                if !d.recv && !type_args.is_empty() && d.type_params == 0 {
                    return Some(format!(
                        "`{shown}` declares no type parameters, so it takes no type arguments"
                    ));
                }
                if !d.recv && type_args.len() > d.type_params {
                    return Some(format!(
                        "`{shown}` takes {} type argument(s), got {}",
                        d.type_params,
                        type_args.len()
                    ));
                }
                // The first argument its parameter does not take. A `fn`
                // parameter's is the checker's `check_fn_arg`.
                args.iter()
                    .zip(&d.params)
                    .enumerate()
                    .skip(usize::from(d.recv))
                    .find_map(|(i, (a, pty))| {
                        let aty = recorded(a)?;
                        let fits = matches!(pty, Type::Fn(..))
                            || vyrn_frontend::types::coercible(aty, pty, decls);
                        (!fits).then(|| match i.checked_sub(dot) {
                            Some(k) => {
                                format!("`{shown}` argument {} expects {pty}, found {aty}", k + 1)
                            }
                            None => format!("the receiver of `{shown}` expects {pty}, found {aty}"),
                        })
                    })
            }
            _ => None,
        }
    };
    facts
        .exprs
        .iter()
        .filter_map(|(e, line)| Some((*line as usize, refusal(e)?)))
        .collect()
}

/// The calls in `facts` whose solved type argument fails a bound of the
/// callee's seeded row: `@Heapless` (`clear`, `append`, `copyFrom`),
/// `@Decodable` (`fromJson`) and `Show` (`print`, `toString`). A type
/// parameter of the body satisfies `Show` where `outer`, the body's bounds,
/// gives it. A type argument that fails as written (`Array<T>`) is named as
/// written, so every instance gives one sentence.
fn unbound(
    facts: &NodeTypes<'_>,
    decls: &HashMap<String, TypeDecl>,
    impls: &[vyrn_frontend::ast::ImplBlock],
    outer: &HashMap<String, Vec<String>>,
) -> Vec<(usize, String)> {
    use vyrn_frontend::prelude::{signature, DECODABLE, HEAPLESS};
    use vyrn_frontend::types::{self, SHOW};
    let fails = |t: &Type, bound: &str| {
        let base = types::resolve(t, decls);
        match bound {
            HEAPLESS => vyrn_frontend::declared::owns_heap(&base, decls),
            DECODABLE => vyrn_frontend::codec::decodable(&base, decls).is_err(),
            SHOW => match &base {
                Type::Param(p) => !outer.get(p).is_some_and(|bs| bs.iter().any(|b| b == SHOW)),
                _ => !types::renders(&base) && types::show_dispatch(impls, t, &base).is_none(),
            },
            _ => false,
        }
    };
    let sentence = |shown: &str, t: &Type, bound: &str| match bound {
        HEAPLESS => format!(
            "`{shown}` forgets or overwrites elements without releasing them, and \
             `{t}` owns heap \u{2014} move the elements one at a time instead"
        ),
        DECODABLE => {
            let off = vyrn_frontend::codec::decodable(t, decls)
                .err()
                .unwrap_or_else(|| t.to_string());
            format!("`{shown}` cannot decode into `{off}` (not a codable type)")
        }
        _ => types::needs_show(shown, t),
    };
    facts
        .exprs
        .iter()
        .filter_map(|(e, _)| {
            let Expr::Call { name, .. } = e else {
                return None;
            };
            let bounds = &signature(name)?.type_bounds;
            let written = node_solved(*e as *const Expr as usize).unwrap_or_default();
            let shown = vyrn_frontend::parser::method_surface(name).trim_start_matches('@');
            let solved = facts.solved.get(&(*e as *const Expr as usize))?;
            solved.iter().find_map(|(tp, t)| {
                let w = written.iter().find(|(p, _)| p == tp).map_or(t, |(_, w)| w);
                if [t, w].iter().any(|t| types::resolve(t, decls) == Type::Err) {
                    return None;
                }
                bounds.get(tp)?.iter().find_map(|b| {
                    let named = [w, t].into_iter().find(|t| fails(t, b))?;
                    Some((e.line(), sentence(shown, named, b)))
                })
            })
        })
        .collect()
}

/// How a function value misses the `fn` type of its slot, a call's parameter
/// or a stored slot; each words it for its slot.
enum Misfit {
    /// The value takes this many parameters, and the slot passes that many.
    Arity(usize, usize),
    /// The value's parameter, and what the slot passes it.
    Param(Type, Type),
    /// What the value returns, and what the slot returns.
    Returns(Type, Type),
}

/// The first way a value misses `slot`: its arity, then each of `params`
/// (`None` where the slot types them, as it does a lambda's), then `ret`
/// (`None` where the slot does not ask). A slot that returns `Unit` or a type
/// parameter takes any return.
fn misfit(
    arity: usize,
    params: Option<&[Type]>,
    ret: Option<&Type>,
    slot: &Type,
    decls: &HashMap<String, TypeDecl>,
) -> Option<Misfit> {
    use vyrn_frontend::types::{assignable, coercible, resolve};
    let Type::Fn(ptys, sret) = resolve(slot, decls) else {
        return None;
    };
    if arity != ptys.len() {
        return Some(Misfit::Arity(arity, ptys.len()));
    }
    let mut both = params.into_iter().flatten().zip(&ptys);
    if let Some((a, b)) = both.find(|(a, b)| !assignable(b, a, decls)) {
        return Some(Misfit::Param(a.clone(), b.clone()));
    }
    let got = ret.filter(|_| !matches!(*sret, Type::Unit | Type::Param(_)))?;
    (!coercible(got, &sret, decls)).then(|| Misfit::Returns(got.clone(), *sret))
}

/// The refusal of a call to `name` whose `fn` argument does not fit its
/// parameter. `solved` is the instance as far as the checker solved it; for
/// a body as written it names its own type parameters. `bound` answers
/// whether a name is a binding, which a function of that name does not
/// shadow.
#[allow(clippy::too_many_arguments)]
fn fn_slot(
    program: &Program,
    name: &str,
    args: &[Expr],
    line: usize,
    solved: &[(String, Type)],
    types: &HashMap<usize, Type>,
    decls: &HashMap<String, TypeDecl>,
    bound: &dyn Fn(&str) -> bool,
) -> Option<(usize, String)> {
    use vyrn_frontend::types::{resolve, substitute};
    let recorded = |e: &Expr| {
        types
            .get(&(e as *const Expr as usize))
            .filter(|t| **t != Type::Err)
    };
    let f = program.functions.iter().find(|f| f.name == name)?;
    let callee = vyrn_frontend::parser::method_surface(name).trim_start_matches('@');
    let subst: HashMap<String, Type> = solved.iter().cloned().collect();
    let mut slots = f.params.iter().zip(args).enumerate();
    slots.find_map(|(i, (p, arg))| {
        let slot = substitute(&p.ty, &subst);
        let Type::Fn(ptys, _) = &slot else {
            return None;
        };
        let n = i + 1;
        let want = ptys.len();
        // A value of `fn` type, named `subject` in the arity sentence and
        // `owner` in the parameter one.
        let value = |subject: &str, owner: &str, vptys: &[Type]| {
            Some(
                match misfit(vptys.len(), Some(vptys), None, &slot, decls)? {
                    Misfit::Arity(got, _) => format!(
                    "{subject} is a {got}-argument function value, but `{callee}` argument {n} \
                     expects {want}"
                ),
                    Misfit::Param(a, b) => {
                        format!("{owner} expects a {a} argument, but `{callee}` will pass it {b}")
                    }
                    Misfit::Returns(..) => return None,
                },
            )
        };
        let says = match arg {
            Expr::Lambda {
                params,
                body,
                line: at,
                ..
            } => {
                let got = match body {
                    LambdaBody::Expr(e) => recorded(e),
                    LambdaBody::Block(_) => None,
                };
                let says = match misfit(params.len(), None, got, &slot, decls)? {
                    Misfit::Arity(got, _) => format!(
                        "this lambda takes {got} parameter(s), but `{callee}` argument {n} \
                         expects {want}"
                    ),
                    Misfit::Returns(t, r) => {
                        format!("this lambda returns {t}, but `{callee}` expects it to return {r}")
                    }
                    Misfit::Param(..) => return None,
                };
                return Some((*at, says));
            }
            Expr::Var { name: vn, .. } if bound(vn) => match recorded(arg)
                .map(|t| resolve(t, decls))
            {
                Some(Type::Fn(vptys, _)) => value(&format!("`{vn}`"), &format!("`{vn}`"), &vptys),
                _ => None,
            },
            Expr::Var { name: vn, .. } => {
                let g = program.functions.iter().find(|g| g.name == *vn)?;
                if !g.type_params.is_empty() {
                    return Some((
                        line,
                        format!("`{vn}` is generic and cannot be passed as a function value in v1"),
                    ));
                }
                let vptys: Vec<Type> = g.params.iter().map(|p| p.ty.clone()).collect();
                match misfit(vptys.len(), Some(&vptys), None, &slot, decls)? {
                    Misfit::Arity(got, _) => Some(format!(
                        "`{vn}` takes {got} argument(s), but `{callee}` argument {n} expects a \
                         {want}-argument function"
                    )),
                    Misfit::Param(a, b) => Some(format!(
                        "`{vn}` expects a {a} argument, but `{callee}` will pass it {b}"
                    )),
                    Misfit::Returns(..) => None,
                }
            }
            other => {
                let aty = recorded(other)?;
                match resolve(aty, decls) {
                    Type::Fn(vptys, _) => value("this", "this function value", &vptys),
                    _ => {
                        let at = match other.line() {
                            0 => line,
                            l => l,
                        };
                        return Some((
                            at,
                            format!(
                                "`{callee}` argument {n} must be a lambda `|..| ..`, a function \
                                 name, or an expression of `fn` type; found {aty}"
                            ),
                        ));
                    }
                }
            }
        };
        says.map(|says| (line, says))
    })
}

/// The refusal of a value stored in a slot of `fn` type `exp` that it does
/// not fit: a lambda whose body returns `lambda`, or the function `f`. The
/// checker refuses a lambda's parameter count itself.
fn stored_slot(
    lambda: Option<&Type>,
    f: Option<&Function>,
    exp: &Type,
    decls: &HashMap<String, TypeDecl>,
    line: usize,
) -> Option<(usize, String)> {
    let says = match (lambda, f) {
        (Some(got), _) => {
            let Type::Fn(ptys, _) = vyrn_frontend::types::resolve(exp, decls) else {
                return None;
            };
            match misfit(ptys.len(), None, Some(got), exp, decls)? {
                Misfit::Returns(t, r) => format!(
                    "this lambda returns {t}, but the expected function type `{exp}` returns {r}"
                ),
                Misfit::Arity(..) | Misfit::Param(..) => return None,
            }
        }
        (None, Some(f)) => {
            let name = &f.name;
            let vptys: Vec<Type> = f.params.iter().map(|p| p.ty.clone()).collect();
            match misfit(vptys.len(), Some(&vptys), Some(&f.ret), exp, decls)? {
                Misfit::Arity(got, want) => format!(
                    "`{name}` takes {got} argument(s), but the expected function type `{exp}` \
                     takes {want}"
                ),
                Misfit::Param(a, b) => {
                    format!("`{name}` expects a {a} argument, but `{exp}` will pass it {b}")
                }
                Misfit::Returns(t, r) => format!(
                    "`{name}` returns {t}, but the expected function type `{exp}` returns {r}"
                ),
            }
        }
        (None, None) => return None,
    };
    Some((line, says))
}

fn build_seeded(
    program: &Program,
    inst: &Instance<'_>,
    own: &Ownership,
    seed: &std::collections::HashSet<usize>,
) -> Result<Body, Gap> {
    let (types, produced, solved) = (
        inst.facts.types.clone(),
        inst.facts.produced.clone(),
        inst.facts.solved.clone(),
    );
    let mistyped = judged(&inst.facts, own.proto.types());
    let refused = unbound(
        &inst.facts,
        own.proto.types(),
        &program.impls,
        &inst.func.type_bounds,
    );
    // The plan's own rows, not the instance's copy: the copy predates the
    // rows [`augment`] places. The copy adds only the substituted type a
    // `Deep` walks, and nothing below reads a kind.
    let no_steps: Vec<Release> = Vec::new();
    let mut placed: HashMap<(Exit, usize), Vec<&Release>> = HashMap::new();
    for r in own.releases.get(&inst.func.name).unwrap_or(&no_steps) {
        placed.entry((r.exit, r.site)).or_default().push(r);
    }
    let mut b = Builder {
        program,
        own,
        proto: &own.proto,
        types,
        produced,
        solved,
        placed,
        body: Body {
            name: inst.spelling(),
            file: inst.func.module.clone(),
            export: inst.func.is_export_extern,
            names: Vec::new(),
            params: Vec::new(),
            stmts: Vec::new(),
            lambdas: Vec::new(),
            cands: Vec::new(),
            loop_buffers: Vec::new(),
            unbound_drops: Vec::new(),
            refused,
            mistyped,
        },
        scope: Vec::new(),
        by_binding: HashMap::new(),
        temps: 0,
        pending_receiver: None,
        drain: 0,
        after: Vec::new(),
        after_of_rhs: Vec::new(),
        owed: None,
        stream_loops: Vec::new(),
        walks: Vec::new(),
        reading: Vec::new(),
        seed,
        loop_marks: Vec::new(),
        loop_aliased: HashMap::new(),
        rebinding: false,
        call_keeps: None,
        pending_closure: None,
        appends: std::collections::HashSet::new(),
        rebound: std::collections::HashSet::new(),
        region: 0,
        ret: None,
        released: None,
        closed: false,
    };
    let f: &Function = inst.func;
    // A parameter's type is the instance's, not the declaration's:
    // `map<Int64, Int64>`'s `f` is `fn(Int64) -> Int64`, the shape stored
    // sources are keyed by. Every other type comes substituted in the rows.
    let subst: HashMap<String, Type> = inst.subst.clone().into_iter().collect();
    b.ret = Some(vyrn_frontend::types::substitute(&f.ret, &subst));
    // A declared release (`impl Owned for T { fn release(consume self) }`)
    // frees `self`'s parts, and nothing releases `self` again, so the kernel
    // does not own it there.
    let is_release = b.proto.is_release_fn(&f.name);
    for p in &f.params {
        let pty = vyrn_frontend::types::substitute(&p.ty, &subst);
        let owned = p.capability == Capability::Consume && b.owns(&pty) && !is_release;
        let n = b.name(&p.name, pty, owned, f.line);
        if is_release {
            b.released = Some(n);
        }
        // A `read` or `modify` parameter is never taken; the
        // kernel needs the capability to word the refusal. A must-use
        // parameter is excepted from the take only: its capability
        // still words a refusal about a second name for it.
        b.body.names[n as usize].must_use_param =
            b.proto.must_use(&b.body.names[n as usize].ty.clone());
        b.body.names[n as usize].borrow_kind = param_borrow(p.capability, &p.name);
        b.body.names[n as usize].mutable = p.capability == Capability::Modify;
        b.scope.push((p.name.clone(), n));
        b.keyed(n, p as *const _ as usize);
        b.body.params.push(n);
    }
    b.appends = crate::append::append_candidates(&f.body);
    rebound(&f.body, &mut b.rebound);
    let mut out = Vec::new();
    b.block(&f.body, &mut out)?;
    cut(&mut out);
    b.body.stmts = out;
    falls_through(&mut b.body, &f.ret, f.line, || {
        format!("function `{}`", f.name)
    });
    Ok(b.body)
}

/// The module-state initializer as a body: every module-scope
/// `let` is a store into its global, run once at `_start` into an empty
/// place. Its name is empty, the name the checker records a lambda written in
/// it under (`StoredLambda::defined_in`), so a call through that lambda's type
/// is judged over this frame.
pub fn build_module_state<'a>(
    program: &'a Program,
    own: &'a Ownership,
    facts: &NodeTypes<'a>,
) -> Result<Body, Gap> {
    let seed = std::collections::HashSet::new();
    let mut b = Builder::bare(
        program,
        own,
        facts,
        &seed,
        String::new(),
        None,
        HashMap::new(),
    );
    let mut out = Vec::new();
    for g in &program.globals {
        // A crossing into a validated declared type is its constructor.
        let check = match &g.ty {
            Some(to) => b.checked(&b.ty_of(&g.init)?, to, &g.init),
            None => None,
        };
        let v = match check {
            Some(to) => Val::Name(b.checked_temp(&to, &g.init, g.line, &mut out)?),
            None => b.val(&g.init, &mut out)?,
        };
        out.push(St::Store {
            place: Place::Global(g.name.clone()),
            value: v,
            old: Old::Nothing,
            line: g.line,
            site: Site::None,
            releases: false,
            holes: Vec::new(),
        });
    }
    cut(&mut out);
    b.body.stmts = out;
    Ok(b.body)
}

/// The body of a `test` or a `bench`: a block with no
/// parameters, keyed in the release plan by the synthetic `test@<i>` or
/// `bench@<i>` name.
pub fn build_outside<'a>(
    program: &'a Program,
    own: &'a Ownership,
    name: &str,
    file: Option<String>,
    block: &Block,
    facts: &NodeTypes<'a>,
) -> Result<Body, Gap> {
    let none = std::collections::HashSet::new();
    let first = build_outside_seeded(program, own, name, file.clone(), block, facts, &none)?;
    let seed = last_owner(&first);
    if seed.is_empty() {
        return Ok(first);
    }
    build_outside_seeded(program, own, name, file, block, facts, &seed)
}

#[allow(clippy::too_many_arguments)]
fn build_outside_seeded<'a>(
    program: &'a Program,
    own: &'a Ownership,
    name: &str,
    file: Option<String>,
    block: &Block,
    facts: &NodeTypes<'a>,
    seed: &std::collections::HashSet<usize>,
) -> Result<Body, Gap> {
    // No substitution: the body has no type parameters.
    let no_steps: Vec<Release> = Vec::new();
    let steps = own.releases.get(name).unwrap_or(&no_steps);
    let mut placed: HashMap<(Exit, usize), Vec<&Release>> = HashMap::new();
    for r in steps {
        placed.entry((r.exit, r.site)).or_default().push(r);
    }
    let mut b = Builder::bare(program, own, facts, seed, name.to_string(), file, placed);
    // The checker types a `test` or `bench` body as a function returning Unit.
    b.ret = Some(Type::Unit);
    b.appends = crate::append::append_candidates(block);
    rebound(block, &mut b.rebound);
    let mut out = Vec::new();
    b.block(block, &mut out)?;
    cut(&mut out);
    b.body.stmts = out;
    Ok(b.body)
}

/// A `for` whose every element leaves through the loop variable
/// ([`Body::loop_buffers`]): the container, its length, and the counter,
/// which steps past an element as the turn binds it.
#[derive(Clone)]
struct Unreached {
    it: Name,
    n: Name,
    i: Name,
    elem: Type,
    line: usize,
}

/// A module-state initializer or a `where` predicate as a body, for the typed
/// judgment alone: it places no row and is never emitted. A predicate has
/// `binds` (its `value` or its record's fields) and sees no module state.
pub fn build_root<'a>(
    program: &'a Program,
    own: &'a Ownership,
    facts: &NodeTypes<'a>,
    file: Option<String>,
    binds: Option<&[(String, Type)]>,
    e: &'a Expr,
) -> Result<Body, Gap> {
    // `facts` holds every root's; a refusal belongs to the root it is in.
    let mut mine = Vec::new();
    vyrn_frontend::ast::node_addrs_val(e, &mut mine);
    let mine: std::collections::HashSet<usize> = mine.into_iter().collect();
    let facts = NodeTypes {
        exprs: facts
            .exprs
            .iter()
            .filter(|(x, _)| mine.contains(&(*x as *const Expr as usize)))
            .copied()
            .collect(),
        ..facts.clone()
    };
    let seed = std::collections::HashSet::new();
    let mut b = Builder::bare(
        program,
        own,
        &facts,
        &seed,
        String::new(),
        file,
        HashMap::new(),
    );
    b.closed = binds.is_some();
    for (name, ty) in binds.unwrap_or_default() {
        let n = b.name(name, ty.clone(), false, e.line());
        b.scope.push((name.clone(), n));
        b.body.params.push(n);
    }
    let mut out = Vec::new();
    // A gap under a refusal the builder states is that refusal, as in
    // [`Builder::stmt_list`].
    if let Err(g) = b.val(e, &mut out) {
        if b.body.refused.is_empty() && b.body.mistyped.is_empty() {
            return Err(g);
        }
    }
    b.body.stmts = out;
    Ok(b.body)
}

struct Builder<'a> {
    program: &'a Program,
    own: &'a Ownership,
    proto: &'a Owned,
    types: HashMap<usize, Type>,
    /// The producer type of every typed expression, before the destination's
    /// coercion (see [`Rhs`]); `types` holds what the value must end up as.
    produced: HashMap<usize, Type>,
    solved: HashMap<usize, Vec<(String, Type)>>,
    placed: HashMap<(Exit, usize), Vec<&'a Release>>,
    body: Body,
    scope: Vec<(String, Name)>,
    /// The plan keys a release by the node that owns the value: a `Stmt::Let`,
    /// a parameter, or the construct that owns a temporary.
    by_binding: HashMap<usize, Name>,
    temps: u32,
    /// An unnamed receiver minted for a field or element read, with the node
    /// that produced it, so the read can release it when the plan says the
    /// frame owns it.
    pending_receiver: Option<(Name, usize, bool)>,
    /// How many non-lending calls and operators enclose the expression being
    /// built. The compiled backends drain argument temporaries at each, so a
    /// receiver borrowed under one can be freed there.
    drain: u32,
    /// Temporaries the expression being built has read and must release once
    /// it is bound (`read_val`, `call`, `rhs`, `bind`).
    after: Vec<Name>,
    /// What `rhs` left for the binding that follows it.
    after_of_rhs: Vec<Name>,
    /// The check `rhs` owes a record literal of a validated type, with its
    /// line: [`Builder::bind`] states it after the literal's row.
    owed: Option<(String, usize)>,
    /// The streams the enclosing `for` loops walk, innermost last. A `return`
    /// or a `?` inside closes all of them (the direct backend's cursor
    /// stack); the loop's end closes its own.
    stream_loops: Vec<Name>,
    /// One entry per enclosing loop, innermost last: the `for` whose elements
    /// no turn reached yet, or `None`. A `return`, a `?` and a `break` release
    /// them ([`Builder::release_unreached`]).
    walks: Vec<Option<Unreached>>,
    /// The constructs this build may take their named scrutinee at, as
    /// [`last_owner`] decided over the previous build. Empty on the first.
    seed: &'a std::collections::HashSet<usize>,
    /// One entry per enclosing loop: the name count when its body opened. A
    /// name below the innermost entry is bound outside the loop, so handing
    /// it out of a join arm frees it once per turn ([`Builder::alias_out`]).
    loop_marks: Vec<usize>,
    /// A join's result, and the name an arm handed out of it from outside the
    /// enclosing loop. [`Builder::loop_alias`] refuses where something owns
    /// the result and releases it once per turn.
    loop_aliased: HashMap<Name, String>,
    /// Whether the value being lowered is a rebind's. The slot is released by
    /// its final value, so the temporary the value passes through owns
    /// nothing and the back edge repeats no release (`std/html.vyrn`'s
    /// `attrKey`).
    rebinding: bool,
    /// Whether the argument position being lowered may keep what it is
    /// handed: `Some(false)` where it provably only borrows, `Some(true)`
    /// where it may store it, `None` outside an argument. Only the lambda
    /// arms read it.
    call_keeps: Option<bool>,
    /// [`NameInfo::closure_reads`] for the lambda [`Builder::rhs`] has just
    /// built, waiting for the name [`Builder::bind`] gives it.
    pending_closure: Option<Vec<Name>>,
    /// The receivers of the projections being inlined, innermost last. A
    /// projection declares `read self`, so no construct of its body is its
    /// receiver's last owner ([`Builder::takes_scrutinee`]).
    reading: Vec<Name>,
    /// The body's String accumulators ([`crate::append::append_candidates`]).
    appends: std::collections::HashSet<String>,
    /// The names the body stores into whole ([`rebound`]).
    rebound: std::collections::HashSet<String>,
    /// How many `region`s enclose the statement. An arena buffer cannot
    /// grow, so an append inside one is the `concat` call.
    region: u32,
    /// The frame's declared result, which a `?` on an `Option` fails with
    /// `None` of. `None` for module state, an outside block and an untyped
    /// lambda.
    ret: Option<Type>,
    /// The receiver of a declared release, which the frame does not own
    /// ([`Builder::owns_boxes`]).
    released: Option<Name>,
    /// A `where` predicate's body: it sees its binds and no module state.
    closed: bool,
}

impl<'a> Builder<'a> {
    /// A builder for a body that is no instance: no substitution.
    fn bare(
        program: &'a Program,
        own: &'a Ownership,
        facts: &NodeTypes<'a>,
        seed: &'a std::collections::HashSet<usize>,
        name: String,
        file: Option<String>,
        placed: HashMap<(Exit, usize), Vec<&'a Release>>,
    ) -> Self {
        let (types, produced, solved) = (
            facts.types.clone(),
            facts.produced.clone(),
            facts.solved.clone(),
        );
        let mistyped = judged(facts, own.proto.types());
        let refused = unbound(facts, own.proto.types(), &program.impls, &HashMap::new());
        Builder {
            program,
            own,
            proto: &own.proto,
            types,
            produced,
            solved,
            placed,
            body: Body {
                name,
                file,
                export: false,
                names: Vec::new(),
                params: Vec::new(),
                stmts: Vec::new(),
                lambdas: Vec::new(),
                cands: Vec::new(),
                loop_buffers: Vec::new(),
                unbound_drops: Vec::new(),
                refused,
                mistyped,
            },
            scope: Vec::new(),
            by_binding: HashMap::new(),
            temps: 0,
            pending_receiver: None,
            drain: 0,
            after: Vec::new(),
            after_of_rhs: Vec::new(),
            owed: None,
            stream_loops: Vec::new(),
            walks: Vec::new(),
            reading: Vec::new(),
            seed,
            loop_marks: Vec::new(),
            loop_aliased: HashMap::new(),
            rebinding: false,
            call_keeps: None,
            pending_closure: None,
            appends: std::collections::HashSet::new(),
            rebound: std::collections::HashSet::new(),
            region: 0,
            ret: None,
            released: None,
            closed: false,
        }
    }

    fn owns(&self, ty: &Type) -> bool {
        self.proto.owns_heap(ty) || self.proto.must_use(ty) || self.proto.release_kind(ty).is_some()
    }

    fn name(&mut self, source: &str, ty: Type, releases: bool, line: usize) -> Name {
        let heap = self.proto.owns_heap(&ty);
        let linear = self.proto.linear_kind(&ty).is_some();
        self.body.names.push(NameInfo {
            source: source.to_string(),
            ty,
            releases,
            heap,
            borrow: heap && !releases,
            borrow_kind: None,
            line,
            binding: None,
            receiver: None,
            producer: None,
            arg_drop: None,
            holes: Vec::new(),
            payload: None,
            receiver_malloc: false,
            grows: false,
            must_use_param: false,
            path: None,
            for_consume: false,
            fields: Vec::new(),
            loop_var: None,
            walked: None,
            linear,
            bound_by_let: false,
            mutable: false,
            closure_reads: None,
            not_owned: None,
        });
        (self.body.names.len() - 1) as Name
    }

    /// The `@borrow` a read of a place binds, carrying the path the reader
    /// wrote ([`NameInfo::path`]).
    fn borrow_name(&mut self, e: &'a Expr, ty: Type, line: usize) -> Name {
        let n = self.name("@borrow", ty, false, line);
        self.body.names[n as usize].path = reader_path(e);
        n
    }

    /// [`Builder::keyed`] for a `let` the reader wrote.
    fn keyed_let(&mut self, n: Name, s: &Stmt) {
        self.keyed(n, s as *const Stmt as usize);
        self.body.names[n as usize].bound_by_let = true;
        self.body.names[n as usize].mutable = matches!(s, Stmt::Let { mutable: true, .. });
    }

    /// Records the plan's key for a name, and the name for the key.
    fn keyed(&mut self, n: Name, binding: usize) {
        self.body.names[n as usize].binding = Some(binding);
        self.by_binding.insert(binding, n);
    }

    /// Refuses binding, by a `let` or an argument temporary, a join whose arm
    /// handed out a name bound outside the enclosing loop: the binding
    /// releases it every turn ([`Builder::alias_out`]).
    fn loop_alias(&self, rhs: &Rhs, line: usize) -> Result<(), Gap> {
        let Rhs::Val(Val::Name(m)) = rhs else {
            return Ok(());
        };
        let Some(a) = self.loop_aliased.get(m) else {
            return Ok(());
        };
        refuse(
            format!(
                "`{a}` may not be handed out of an arm inside a loop — the result is \
                 released on every turn, and `{a}` is bound outside the loop\n  \
                 fix: `{a}.copy()` if the arm should hand out a value of its own"
            ),
            line,
        )
    }

    /// Whether a `let` binds a value this frame owns, read off the lowered
    /// `Rhs`. Where the type owns heap or carries an obligation, the frame
    /// still does not own:
    ///
    ///   - a static value (a literal, a nullary constructor) in an immutable
    ///     binding; a `mut` slot is released by its final value, so
    ///     `let mut acc: String = ""` owns;
    ///   - a read of a place, or a second name for a borrow.
    ///
    /// A rebind states the same rule at the store (`Stmt::Assign`).
    fn owned_binding(&self, rhs: &Rhs, ty: &Type, static_value: bool, mutable: bool) -> bool {
        if !self.owns(ty) {
            return false;
        }
        if static_value && !mutable {
            return false;
        }
        match rhs {
            Rhs::Read(_) => false,
            Rhs::Val(Val::Name(m)) => !self.body.names[*m as usize].borrow,
            _ => true,
        }
    }

    /// Whether the `let` `s` of type `ty` is a copy (#501): a `let mut` the
    /// body stores into whole, bound to a borrow of a type that owns heap.
    /// The binding owns its value, so its stores and exit release on every
    /// path, where a borrow would release the owner's value or nothing. A
    /// type with `impl Copy` keeps the borrow.
    fn copies(&self, s: &Stmt, ty: &Type) -> bool {
        let Stmt::Let {
            name,
            value,
            mutable,
            ..
        } = s
        else {
            return false;
        };
        let borrow = match value {
            Expr::Var { name: m, .. } => self
                .lookup(m)
                .is_some_and(|m| self.body.names[m as usize].borrow),
            e => is_place_read(e) && self.deferred_of(e).is_none(),
        };
        *mutable
            && borrow
            && self.rebound.contains(name)
            && self.owns(ty)
            && vyrn_frontend::types::copy_impl(&self.program.impls, ty).is_none()
    }

    /// Why a `let` binds a value this frame does not own. It asks
    /// [`Builder::owned_binding`]'s facts in the report's order: what the
    /// type releases first, then who owns the storage.
    fn report_reason(
        &self,
        rhs: &Rhs,
        ty: &Type,
        static_value: bool,
        mutable: bool,
        lends: bool,
    ) -> Option<NotOwned> {
        // A must-use type reaching a `let` is discharged on every path, so
        // "nothing reclaims it" is the wrong sentence about one.
        if self.proto.release_kind(ty).is_none() {
            return Some(match self.proto.linear_kind(ty) {
                Some(l) => NotOwned::MustUse(l),
                None => NotOwned::NoRelease {
                    heap: self.proto.owns_heap(ty),
                },
            });
        }
        // A `mut` slot is released by its final value.
        if static_value && !mutable {
            return Some(NotOwned::Static);
        }
        if lends {
            return Some(NotOwned::Borrow("a view into its argument".into()));
        }
        match rhs {
            Rhs::Read(_) => Some(NotOwned::Borrow("read out of a place that owns it".into())),
            Rhs::Val(Val::Name(m)) if self.body.names[*m as usize].borrow => {
                Some(NotOwned::Borrow("a borrow of somebody else's value".into()))
            }
            _ => None,
        }
    }

    /// Whether a call's result points into one of its arguments, so the name
    /// bound to it is a borrow: a lending prelude row (`at`, `bytes`) or a
    /// projection an `impl` declares.
    fn lends(&self, e: &Expr) -> bool {
        match e {
            Expr::Call { name, args, .. } => {
                (self.lends_name(name) && !self.copies_an_element(name, args, e))
                    || self.hands_back_a_borrow(name, args)
            }
            _ => false,
        }
    }

    /// Whether a call whose result is its argument (`blackBox`,
    /// `movecheck::hands_back`) hands back a borrow: it does when the
    /// argument is a place read or a lending call. An owned temporary is
    /// taken instead, and the result owns it. Either way one release stands
    /// for the value.
    fn hands_back_a_borrow(&self, name: &str, args: &[Expr]) -> bool {
        vyrn_frontend::movecheck::hands_back(name)
            && args
                .first()
                .is_some_and(|a| is_place_read(a) || self.lends(a))
    }

    /// Whether a call by this name lends: `a[i]` and its seeded element row, a
    /// lending prelude row, a projection. A call that hands its argument back
    /// depends on the argument ([`Self::lends`]).
    fn lends_name(&self, name: &str) -> bool {
        name == vyrn_frontend::project::AT
            || name == vyrn_frontend::project::ELEM
            || prelude::lends(name)
            || self.projection(name).is_some()
    }

    fn projection(&self, name: &str) -> Option<&'a Function> {
        self.program
            .impls
            .iter()
            .flat_map(|i| i.places.iter())
            .find(|p| p.name == name)
    }

    /// A projection's body, stated as rows at the access site. A
    /// projection is inlined where it is called, so its rows belong to the
    /// site. The tree is [`vyrn_frontend::project::site`]'s, the one the
    /// checker typed, so each row lands on the node an emitter asks about.
    ///
    /// Answers the yielded place, so `s[h].next` walks what `at` yields after
    /// its prologue. `None` leaves the site as it was, where:
    ///
    /// - no compile scope is open: outside one the expansion leaks its tree,
    ///   which the LSP would pay per keystroke;
    /// - no projection answers for the receiver's type;
    /// - the projection is optional, which
    ///   [`Builder::optional_if_let`] states;
    /// - the receiver has no recorded type.
    fn inlined(
        &mut self,
        method: &str,
        recv: &'a Expr,
        args: &'a [Expr],
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Option<Place>, Gap> {
        if !vyrn_frontend::project::memo_open() {
            return Ok(None);
        }
        let Ok(rty) = self.ty_of(recv) else {
            return Ok(None);
        };
        // By the receiver's type: two `impl`s may declare a projection of one
        // name.
        let Some(f) = vyrn_frontend::project::lookup_in(&self.program.impls, &rty, method) else {
            return Ok(None);
        };
        if vyrn_frontend::project::is_optional(f) {
            return Ok(None);
        }
        let p = match vyrn_frontend::project::site(
            &self.program.impls,
            Some(&rty),
            method,
            recv,
            args,
            line,
        ) {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(None),
            Err(e) => return gap_d("a projection this site cannot inline", &e, line),
        };
        for s in &p.prologue {
            self.stmt(s, out)?;
        }
        // The yield is a borrow of the receiver (`-> read T`), so read it
        // through `Builder::place`: `Builder::rhs`'s field arm would take a
        // heap field out of the receiver. `check_places` refuses a yield that
        // is no place, so reaching this gap is a defect in that rule.
        if !is_place_read(&p.place) {
            return gap_d("a projection whose yield is not a place", method, line);
        }
        self.place(&p.place, out).map(Some)
    }

    /// `if let Some(x) = s.tryAt(h)` over an optional projection,
    /// stated at the site. The body splits into four parts at a miss test
    /// ([`vyrn_frontend::project::optional_inline`]) and no `Option` exists on
    /// either path, so the site is a two-way branch on the miss test with the
    /// source's `else` on the true edge, not a switch.
    ///
    /// Answers whether this site is one, under [`Builder::inlined`]'s
    /// conditions.
    #[allow(clippy::too_many_arguments)]
    fn optional_if_let(
        &mut self,
        pattern: &Pattern,
        scrutinee: &'a Expr,
        then_block: &'a Block,
        else_block: Option<&'a Block>,
        sid: usize,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<bool, Gap> {
        if !vyrn_frontend::project::memo_open() {
            return Ok(false);
        }
        let Expr::Call { name, args, .. } = scrutinee else {
            return Ok(false);
        };
        let Some(recv) = args.first() else {
            return Ok(false);
        };
        let Ok(rty) = self.ty_of(recv) else {
            return Ok(false);
        };
        let Some(f) = vyrn_frontend::project::lookup_in(&self.program.impls, &rty, name) else {
            return Ok(false);
        };
        if !vyrn_frontend::project::is_optional(f) {
            return Ok(false);
        }
        let p = match vyrn_frontend::project::optional_site(
            &self.program.impls,
            Some(&rty),
            name,
            recv,
            &args[1..],
            line,
        ) {
            Ok(Some(p)) => p,
            Ok(None) => return Ok(false),
            Err(e) => return gap_d("an optional projection this site cannot inline", &e, line),
        };
        let held = match recv {
            Expr::Var { name, .. } => self.lookup(name),
            _ => None,
        };
        if let Some(n) = held {
            self.reading.push(n);
        }
        let r = self.optional_body(p, pattern, then_block, else_block, sid, name, line, out);
        if held.is_some() {
            self.reading.pop();
        }
        r
    }

    /// [`Builder::optional_if_let`]'s four parts, once the site is one.
    #[allow(clippy::too_many_arguments)]
    fn optional_body(
        &mut self,
        p: &'a vyrn_frontend::project::OptionalProjection,
        pattern: &Pattern,
        then_block: &'a Block,
        else_block: Option<&'a Block>,
        sid: usize,
        name: &str,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<bool, Gap> {
        for s in &p.prologue {
            self.stmt(s, out)?;
        }
        let cond = self.read_val(&p.miss, out)?;
        // The true edge is the miss: the source's `else`, the plan's edge 1.
        let mut miss = Vec::new();
        if let Some(blk) = else_block {
            self.block(blk, &mut miss)?;
        }
        self.edge_drops(sid, 1, &mut miss)?;
        let mut hit = Vec::new();
        let mark = self.scope.len();
        for s in &p.hit {
            self.stmt(s, &mut hit)?;
        }
        // The binder borrows the yielded place (`-> read Option<T>`); read it
        // through [`Builder::place`] for [`Builder::inlined`]'s reason.
        if let Pattern::Variant(_, binds) = pattern {
            if let Some(bind) = binds.first() {
                if !is_place_read(&p.place) {
                    return gap_d("a projection whose yield is not a place", name, line);
                }
                let ty = self.ty_of(&p.place)?;
                let place = self.place(&p.place, &mut hit)?;
                let n = self.name(bind, ty, false, line);
                hit.push(St::Let(n, Rhs::Read(place)));
                self.scope.push((bind.name.clone(), n));
            }
        }
        self.block(then_block, &mut hit)?;
        self.edge_drops(sid, 0, &mut hit)?;
        self.scope.truncate(mark);
        out.push(St::If {
            cond,
            then: miss,
            els: hit,
            site: sid,
        });
        Ok(true)
    }

    /// Makes the join `res` of an `if` or a `match` a borrow when an arm
    /// yields one and no arm yields an owned value
    /// (`if c { parts[0] } else { "Bool" }`). Where an arm owns its value the
    /// join owns it, and the kernel refuses a borrowed arm's store (#518).
    fn join_borrows(&mut self, res: Name, yields: &[Val]) {
        let owned = |v: &Val| matches!(v, Val::Name(n) if self.body.names[*n as usize].releases);
        if yields.iter().any(|v| self.borrows(v)) && !yields.iter().any(owned) {
            self.body.names[res as usize].releases = false;
            self.body.names[res as usize].borrow = true;
        }
    }

    /// Whether a value is a borrow.
    fn borrows(&self, v: &Val) -> bool {
        match v {
            Val::Name(n) => self.body.names[*n as usize].borrow,
            Val::Lit(_) => false,
        }
    }

    /// The validated type the value `e` of `from` crosses into at a
    /// destination of `to`, where the core states the crossing as `to`'s
    /// constructor (`validate::required`). A borrowed heap value is left out:
    /// the constructor would take what a plain binding borrows.
    fn checked(&self, from: &Type, to: &Type, e: &Expr) -> Option<String> {
        vyrn_frontend::validate::required(from, to, self.proto.types())
            .filter(|_| !(self.owns(from) && (is_place_read(e) || self.lends(e))))
            .map(|d| d.name.clone())
    }

    /// Whether the checker proved `e` a value of `to`
    /// ([`vyrn_frontend::validate::proven`]), with this body's scope resolving
    /// a name. The emitter reads the answer as [`Callee::Proven`].
    fn proven(&self, e: &Expr, to: &Type) -> bool {
        let resolve = |x: &Expr| match x {
            Expr::Var { name, .. } => self
                .lookup(name)
                .map(|n| self.body.names[n as usize].ty.clone()),
            _ => None,
        };
        vyrn_frontend::validate::proven(e, to, self.proto.types(), &resolve)
    }

    /// [`Builder::checked`] where the checker proved the crossing.
    fn proven_crossing(&self, e: &Expr, to: &Type) -> Option<String> {
        let from = self.ty_of(e).ok()?;
        self.checked(&from, to, e).filter(|_| self.proven(e, to))
    }

    /// The value `e` takes at a destination of `to`: where the checker proved
    /// the crossing ([`Builder::proven_crossing`]), the validated type's
    /// constructor bound to a temporary; otherwise `e`'s own value, whose
    /// check the emitter runs. `to` is `None` where the builder knows no type.
    fn proven_val(
        &mut self,
        e: &'a Expr,
        to: Option<&Type>,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Val, Gap> {
        match to.and_then(|to| self.proven_crossing(e, to)) {
            Some(t) => Ok(Val::Name(self.checked_temp(&t, e, line, out)?)),
            None => self.val(e, out),
        }
    }

    /// [`Builder::check`] bound to a temporary of the validated type `to`.
    /// A constructor hands its argument back, so over a literal the temporary
    /// is static data, as the literal is.
    fn checked_temp(
        &mut self,
        to: &str,
        value: &'a Expr,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Name, Gap> {
        let rhs = self.check(to, value, line, out)?;
        let t = self.temp(Type::Named(to.to_string()), line);
        if over_a_literal(&rhs) {
            self.body.names[t as usize].releases = false;
            self.body.names[t as usize].not_owned = Some(NotOwned::Static);
        }
        self.bind(t, rhs, out);
        Ok(t)
    }

    /// The constructor of the validated type `to` over `value`.
    fn check(
        &mut self,
        to: &str,
        value: &'a Expr,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Rhs, Gap> {
        let ty = Type::Named(to.to_string());
        self.call(to, std::slice::from_ref(value), line, Some(ty), out)
    }

    fn temp(&mut self, ty: Type, line: usize) -> Name {
        self.temps += 1;
        let owned = self.owns(&ty);
        let src = format!("@t{}", self.temps);
        self.name(&src, ty, owned, line)
    }

    /// Binds `n` to `rhs`, then releases the temporaries `rhs`'s reads queued:
    /// argument temporaries the caller frees, and String temporaries the
    /// reading site frees. They were the result's operands, so they
    /// go after it.
    fn bind(&mut self, n: Name, rhs: Rhs, out: &mut Vec<St>) {
        if matches!(rhs, Rhs::Prim(Op::Closure(_), ..)) {
            self.body.names[n as usize].closure_reads = self.pending_closure.take();
        }
        let owed = match (&rhs, self.owed.take()) {
            (Rhs::Make(Ctor::Record(r, _), _), Some((to, line))) if *r == to => Some((to, line)),
            _ => None,
        };
        out.push(St::Let(n, rhs));
        // A validated record literal the checker did not prove is checked
        // whole once it is made: its constructor reads it.
        if let Some((to, line)) = owed {
            out.push(St::Do {
                rhs: Rhs::Call {
                    ret: Some(Type::Named(to.clone())),
                    callee: to,
                    args: vec![(Arg::Val(Val::Name(n)), Capability::Read)],
                    write_back: false,
                    kind: Callee::Named,
                    solved: Vec::new(),
                    targets: Vec::new(),
                },
                line,
                site: 0,
            });
        }
        for t in std::mem::take(&mut self.after_of_rhs) {
            out.push(St::Drop(t, Site::None, 0, None));
        }
    }

    fn lookup(&self, name: &str) -> Option<Name> {
        self.scope
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, i)| *i)
    }

    /// The producer type of a node, before the destination's coercion (see
    /// [`Rhs`]). `None` where the checker typed no row: the judgment counts
    /// such a store rather than guessing.
    fn produced(&self, e: &Expr) -> Option<Type> {
        self.produced
            .get(&(e as *const Expr as usize))
            .cloned()
            // Fills only a projection expanded at the site, which has no row;
            // `ty_of` refuses everything else.
            .or_else(|| self.ty_of(e).ok())
    }

    fn ty_of(&self, e: &Expr) -> Result<Type, Gap> {
        match self.types.get(&(e as *const Expr as usize)) {
            Some(t) => Ok(t.clone()),
            // A call to a projection the checker expanded at the site
            // (`people.tryAt(h)`): its declared result, under the
            // receiver's type arguments.
            None if matches!(e, Expr::Call { name, args, .. }
                if !args.is_empty() && self.projection(name).is_some()) =>
            {
                let Expr::Call { name, args, .. } = e else {
                    unreachable!()
                };
                let p = self.projection(name).unwrap();
                let rty = self.ty_of(&args[0])?;
                Ok(self.under_impl(&p.ret, &rty))
            }
            None => gap_d(
                "an expression the checker did not type",
                &match e {
                    Expr::Var { name, .. } => format!("var {name}"),
                    Expr::Call { name, .. } => format!("call {name}"),
                    _ => expr_kind(e).to_string(),
                },
                e.line(),
            ),
        }
    }

    /// The releases the plan placed at one exit, as drops, in the plan's order.
    fn drops_at(&self, exit: Exit, site: usize, out: &mut Vec<St>) -> Result<(), Gap> {
        self.drops_at_but(exit, site, None, out)
    }

    /// [`Builder::drops_at`] with one binding left held: the place a `return`
    /// hands a read of ([`Builder::return_exit`]).
    fn drops_at_but(
        &self,
        exit: Exit,
        site: usize,
        keep: Option<Name>,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        let Some(rows) = self.placed.get(&(exit, site)) else {
            return Ok(());
        };
        for r in rows {
            match self.by_binding.get(&r.binding) {
                // The core states no row for a name it does not own: a payload
                // binder of a non-consuming construct names its scrutinee's
                // payload, released once at the scrutinee. A consuming loop's
                // container keeps its row, because the row is the loop's take,
                // which the kernel judges.
                Some(n)
                    if !self.body.names[*n as usize].releases
                        && !self.body.names[*n as usize].for_consume => {}
                Some(n) if keep == Some(*n) => {}
                Some(n) => {
                    // The row's own set (a placer row), else the binding's.
                    let holes = if let Some(h) = &r.holes {
                        h.iter().map(|h| format!(".{h}")).collect()
                    } else {
                        self.body.names[*n as usize].holes.clone()
                    };
                    out.push(St::Row {
                        name: *n,
                        holes,
                        exit,
                        site,
                    });
                }
                None => {
                    return gap_d(
                        "a placed release of a binding this slice did not name",
                        &r.name,
                        r.line as usize,
                    )
                }
            }
        }
        Ok(())
    }

    /// A `return`: the streams closed, the exit's releases, and the return.
    /// The releases skip the binding the returned value reads out of
    /// (`return d.s`), so the kernel refuses such a return as a return, not
    /// as a write around a live alias.
    fn return_exit(
        &mut self,
        v: Option<Val>,
        sid: usize,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        self.leave_loops(out);
        self.drops_at_but(Exit::Return, sid, self.reads_out_of(&v), out)?;
        out.push(St::Return {
            value: v,
            site: sid,
            is_try: false,
            line,
        });
        Ok(())
    }

    /// The binding of this frame a returned place read reads out of.
    fn reads_out_of(&self, v: &Option<Val>) -> Option<Name> {
        let Some(Val::Name(n)) = v else { return None };
        let info = &self.body.names[*n as usize];
        if !info.borrow {
            return None;
        }
        let path = info.path.as_deref()?;
        let end = path.find(['.', '[']).unwrap_or(path.len());
        self.lookup(&path[..end])
    }

    /// Lowers a `return` of an `if` or a `match` by ending each arm with the
    /// return, so the kernel judges each arm's value as returned, not as a
    /// store into a minted result no reader wrote.
    ///
    /// `Ok(false)` for any other shape. A `match` with a block arm
    /// is left whole: a block arm carries its own exits.
    fn return_through(
        &mut self,
        e: &'a Expr,
        sid: usize,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<bool, Gap> {
        match e {
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch: Some(else_branch),
                line: at,
            } => {
                let site = e as *const Expr as usize;
                let c = self.condition(cond, "if", *at, out)?;
                let mut t = Vec::new();
                self.arm_returns(then_branch, sid, line, &mut t)?;
                let mut f = Vec::new();
                self.arm_returns(else_branch, sid, line, &mut f)?;
                out.push(St::If {
                    cond: c,
                    then: t,
                    els: f,
                    site,
                });
                Ok(true)
            }
            Expr::Match {
                scrutinee,
                arms,
                line: mline,
                ..
            } if arms.iter().all(|a| matches!(a.body, ArmBody::Expr(_))) => {
                let sty = self.ty_of(scrutinee)?;
                let mid = e as *const Expr as usize;
                let (sv, consuming) =
                    self.scrutinee(scrutinee, mid, Some(arms_span(*mline, arms)), out)?;
                let owns = self.owns_boxes(scrutinee, consuming);
                let mut core_arms = Vec::new();
                for (i, arm) in arms.iter().enumerate() {
                    let mut body = Vec::new();
                    let mark = self.scope.len();
                    let binds = self.bind_pattern(
                        &arm.pattern,
                        &sty,
                        consuming,
                        *mline,
                        borrow_root(&sv, owns),
                        &mut body,
                    )?;
                    let ArmBody::Expr(ae) = &arm.body else {
                        return gap("a block arm under a returned match", *mline);
                    };
                    let v = self.val(ae, &mut body)?;
                    let frees = self.arm_frees(mid, i as u32, &binds, &mut body);
                    self.edge_drops(mid, i as u32, &mut body)?;
                    // Every arm returns, so the scrutinee's release goes here:
                    // nothing reaches the statement after the switch.
                    self.drops_at(Exit::Scrutinee, mid, &mut body)?;
                    self.return_exit(Some(v), sid, line, &mut body)?;
                    self.scope.truncate(mark);
                    core_arms.push(Arm {
                        binds,
                        frees: Some(frees),
                        body,
                        test: self.arm_test(&arm.pattern, &sty, *mline)?,
                        site: mid,
                        index: i as u32,
                    });
                }
                self.covers(&core_arms, &sty, *mline, out)?;
                out.push(St::Switch {
                    on: sv,
                    arms: core_arms,
                    consuming,
                    carries: true,
                    owns,
                    site: mid,
                    line: *mline,
                });
                Ok(true)
            }
            _ => Ok(false),
        }
    }

    /// One arm of a returned `if` or `match`: its value, and the return that
    /// carries it out. A nested `if`/`match` carries the exit on down.
    fn arm_returns(
        &mut self,
        e: &'a Expr,
        sid: usize,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        if self.return_through(e, sid, line, out)? {
            return Ok(());
        }
        let v = self.val(e, out)?;
        self.return_exit(Some(v), sid, line, out)
    }

    fn block(&mut self, blk: &'a Block, out: &mut Vec<St>) -> Result<(), Gap> {
        self.block_with(blk, Vec::new(), out)
    }

    /// A source block, opened with `head` already in it: a `for` binds its
    /// variable inside the body's block, so the variable's scope ends at
    /// the block's site and a row for it can be keyed there.
    fn block_with(&mut self, blk: &'a Block, head: Vec<St>, out: &mut Vec<St>) -> Result<(), Gap> {
        let mark = self.scope.len();
        let site = blk as *const Block as usize;
        let mut body = head;
        self.stmt_list(&blk.stmts, &mut body)?;
        self.drops_at(Exit::Block, site, &mut body)?;
        self.scope.truncate(mark);
        out.push(St::Block {
            site,
            body,
            region: false,
        });
        Ok(())
    }

    /// The loop `for var in iter` over a user container expands to
    /// ([`vyrn_frontend::project::iterate_loop`]), the block the
    /// checker types and every emitter walks. `None` for a container no
    /// `impl Iterate` answers for, one read out of a path or module state
    /// (which [`Builder::iterate`] cannot hold), and outside a compile scope.
    fn iterated(&self, var: &str, iter: &Expr, body: &Block, ity: &Type) -> Option<&'static Block> {
        let held = match iter {
            Expr::Var { name, .. } => self.lookup(name).is_some(),
            e => !is_place_read(e),
        };
        if !held || !vyrn_frontend::project::memo_open() {
            return None;
        }
        let (size_fn, nth) = vyrn_frontend::types::iterate_impl(&self.program.impls, ity)?;
        vyrn_frontend::project::iterate_loop(&size_fn, nth, var, iter, body, iter.line()).ok()
    }

    /// [`Builder::iterated`]'s block, with the source's `body` in place of the
    /// expansion's copy; [`vyrn_frontend::project::iterate_aliases`] maps the
    /// copy's nodes to the source's, so either tree finds the body's rows.
    ///
    /// The scaffolding reads a named container through a borrow taken before
    /// the loop, as the `Array` loop does, so the kernel refuses a store into
    /// the container inside the body.
    fn iterate(
        &mut self,
        blk: &'a Block,
        iter: &'a Expr,
        ity: Type,
        body: &'a Block,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        let Some((
            w @ Stmt::While {
                cond,
                body: inner,
                line,
            },
            head,
        )) = blk.stmts.split_last()
        else {
            return gap("an `Iterate` expansion that is no loop", 0);
        };
        let mark = self.scope.len();
        let site = blk as *const Block as usize;
        let mut rows = Vec::new();
        let lent = match iter {
            Expr::Var { name, .. } => self.lookup(name).map(|n| {
                let t = self.borrow_name(iter, ity, *line);
                self.body.names[t as usize].walked = Some(Walk::For);
                rows.push(St::Let(t, Rhs::Read(Place::Name(n))));
                (name.clone(), t)
            }),
            _ => None,
        };
        self.scaffold(head, &lent, &mut rows)?;
        let mut l = Vec::new();
        let c = self.read_val(cond, &mut l)?;
        l.push(St::If {
            cond: c,
            then: Vec::new(),
            els: vec![St::Break { site: 0, line: 0 }],
            site: 0,
        });
        self.loop_marks.push(self.body.names.len());
        self.walks.push(None);
        let turn = self.scope.len();
        let scaffold = &inner.stmts[..inner.stmts.len() - body.stmts.len()];
        let mut h = Vec::new();
        let r = self
            .scaffold(scaffold, &lent, &mut h)
            .and_then(|()| self.block_with(body, h, &mut l));
        self.scope.truncate(turn);
        self.walks.pop();
        self.loop_marks.pop();
        r?;
        self.hoist_headers(&mut l, *line, &mut rows);
        rows.push(St::Loop {
            body: l,
            site: w as *const Stmt as usize,
        });
        self.drops_at(Exit::Block, site, &mut rows)?;
        self.scope.truncate(mark);
        out.push(St::Block {
            site,
            body: rows,
            region: false,
        });
        Ok(())
    }

    /// Statements of [`Builder::iterate`]'s scaffolding, with the container's
    /// name bound to the loop's borrow of it while they are stated.
    fn scaffold(
        &mut self,
        ss: &'a [Stmt],
        lent: &Option<(String, Name)>,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        let at = self.scope.len();
        if let Some(l) = lent {
            self.scope.push(l.clone());
        }
        let r = self.stmt_list(ss, out);
        if lent.is_some() {
            self.scope.remove(at);
        }
        r
    }

    /// The statements of a list, where a store into a nested place is one
    /// store into the place's path.
    ///
    /// The parser writes `b[i].vx = v` as a move-out window: one temp
    /// per level, the store, and one store back per temp
    /// ([`vyrn_frontend::parser::store_stmts`]). The rows state the store
    /// alone, into `b[i].vx`, and the part's old value is its to release.
    fn stmt_list(&mut self, ss: &'a [Stmt], out: &mut Vec<St>) -> Result<(), Gap> {
        let mut k = 0;
        while k < ss.len() {
            let (scope, at) = (self.scope.len(), out.len());
            // A window spans its temps, its store and the stores back.
            let (span, r) = match self.nested_store(&ss[k..], out) {
                Ok(Some(n)) => (n, Ok(())),
                Ok(None) => (1, self.stmt(&ss[k], out)),
                Err(g) => {
                    let lets = ss[k..].iter().take_while(|s| moves_out(s)).count();
                    ((2 * lets + 1).min(ss.len() - k), Err(g))
                }
            };
            if let Err(g) = r {
                // The checker typed an unknown name `Err` and went on, so a
                // gap may come before the builder meets it.
                let mut named = Vec::new();
                for s in &ss[k..k + span] {
                    vyrn_frontend::ast::exprs_one(s, &mut |e, locals| {
                        let local = matches!(e, Expr::Var { name, .. } if locals.contains(name));
                        let typed = self.types.get(&(e as *const Expr as usize));
                        if !local && matches!(typed, None | Some(Type::Err)) {
                            named.extend(self.unknown_of(e));
                        }
                    });
                }
                self.body.refused.extend(named);
                if self.body.refused.is_empty() && self.body.mistyped.is_empty() {
                    return Err(g);
                }
                // The body is refused: the statements go, and a name one
                // binds is poisoned. The trap stands for them, so a `return`
                // among them is not read as falling through ([`returns`]).
                out.truncate(at);
                out.push(St::Trap);
                self.scope.truncate(scope);
                if let [Stmt::Let {
                    name,
                    line,
                    mutable,
                    ..
                }] = &ss[k..k + span]
                {
                    let n = self.name(name, Type::Err, false, *line);
                    self.body.names[n as usize].mutable = *mutable;
                    self.scope.push((name.clone(), n));
                }
            }
            k += span;
        }
        Ok(())
    }

    /// The store or removal at the head of `ss` when it is a move-out window
    /// ([`Builder::stmt_list`]), stated into its path; how many statements it
    /// spans, or `None` when `ss` does not start with one.
    fn nested_store(&mut self, ss: &'a [Stmt], out: &mut Vec<St>) -> Result<Option<usize>, Gap> {
        let Some((lets, last, place, ty)) = self.window(ss, out)? else {
            return Ok(None);
        };
        let store = &ss[lets];
        let sid = store as *const Stmt as usize;
        match store {
            Stmt::SetField {
                field, value, line, ..
            } => self.set_field((place, ty), last, field, value, sid, *line, out)?,
            Stmt::IndexSet {
                index, value, line, ..
            } => self.index_set((place, ty), last, index, value, sid, *line, out)?,
            _ => self.removal_at(place, ty, last, store, store.line(), out)?,
        }
        Ok(Some(2 * lets + 1))
    }

    /// The move-out window at the head of `ss`: how many temps it moves out,
    /// and the name, place and type of the last, which the next statement
    /// writes. `None`, with nothing stated, when `ss` does not start with one.
    fn window(
        &mut self,
        ss: &'a [Stmt],
        out: &mut Vec<St>,
    ) -> Result<Option<(usize, &'a String, Place, Type)>, Gap> {
        let lets = ss.iter().take_while(|s| moves_out(s)).count();
        if lets == 0 || ss.len() < 2 * lets + 1 {
            return Ok(None);
        }
        // Each temp reads a field or an element of the one before it, the
        // first of a named root, and each is put back where it was read,
        // innermost first.
        let mut parts = Vec::new();
        for (i, s) in ss[..lets].iter().enumerate() {
            let Stmt::Let { name, value, .. } = s else {
                return Ok(None);
            };
            let (parent, part) = match value {
                Expr::Field { expr, field, .. } => match &**expr {
                    Expr::Var { name: p, .. } => (p, Ok(field)),
                    _ => return Ok(None),
                },
                Expr::Call { name: at, args, .. } if at == "@at" && args.len() == 2 => {
                    match (&args[0], &args[1]) {
                        (Expr::Var { name: p, .. }, Expr::Var { .. }) => (p, Err(&args[..])),
                        _ => return Ok(None),
                    }
                }
                _ => return Ok(None),
            };
            let back = &ss[2 * lets - i];
            let put = match (back, &part) {
                (
                    Stmt::SetField {
                        name: p,
                        field,
                        value: Expr::Var { name: v, .. },
                        ..
                    },
                    Ok(f),
                ) => p == parent && field == *f && v == name,
                (
                    Stmt::IndexSet {
                        name: p,
                        index: Expr::Var { name: j, .. },
                        value: Expr::Var { name: v, .. },
                        ..
                    },
                    Err([_, Expr::Var { name: i2, .. }]),
                ) => p == parent && j == i2 && v == name,
                _ => false,
            };
            let chained = i == 0 || matches!(&ss[i - 1], Stmt::Let { name: n, .. } if n == parent);
            if !put || !chained {
                return Ok(None);
            }
            parts.push((parent, part));
        }
        let store = &ss[lets];
        let Stmt::Let { name: last, .. } = &ss[lets - 1] else {
            return Ok(None);
        };
        let into = match store {
            Stmt::SetField { name, .. } | Stmt::IndexSet { name, .. } => Some(name),
            Stmt::Expr(e) | Stmt::Let { value: e, .. } => removal(e),
            _ => None,
        };
        if into != Some(last) {
            return Ok(None);
        }
        let line = &store.line();
        // The path is a record's fields and an array's elements; a map entry
        // is a key read, stated apart. A projected container yields its
        // element's place from `atSet`, whose prologue runs once
        // where the window opens, so only the root may be one
        // ([`Builder::yielded`]).
        let (mut place, mut ty) = self.named_place(parts[0].0, *line)?;
        let mut t = ty.clone();
        let mut tys = Vec::new();
        for (i, (_, part)) in parts.iter().enumerate() {
            let next = match part {
                Ok(f) => self.field_ty(&t, f, *line),
                Err(_) if self.is_map(&t) => return Ok(None),
                Err(_) if self.projected(&t) && i > 0 => return Ok(None),
                Err(_) if self.projected(&t) => match &ss[i] {
                    Stmt::Let { value, .. } => self.ty_of(value),
                    _ => return Ok(None),
                },
                Err(_) => self.elem_ty(&t, *line),
            };
            let Ok(next) = next else {
                return Ok(None);
            };
            t = next.clone();
            tys.push(next);
        }
        for ((_, part), next) in parts.iter().zip(tys) {
            place = match part {
                Ok(f) => Place::Field(Box::new(place), f.to_string()),
                Err(_) if self.projected(&ty) => {
                    let Stmt::IndexSet {
                        name,
                        index,
                        value,
                        line,
                    } = &ss[2 * lets]
                    else {
                        return Ok(None);
                    };
                    match self.yielded(name, index, value, *line, out)? {
                        Some((p, _)) => p,
                        None => return Ok(None),
                    }
                }
                Err(args) => Place::Elem(Box::new(place), self.read_val(&args[1], out)?),
            };
            ty = next;
        }
        Ok(Some((lets, last, place, ty)))
    }

    /// The place `atSet` yields for the store `name[index] = value` into a
    /// projected container, with the prologue stated (`project::stored`).
    /// Also answers the value the place receives: `value`, or the temp the
    /// prologue binds for a value that reads the container
    /// (`c[h] = c[h] + 1`). `None`, with nothing stated, where the checker
    /// expanded no such store.
    fn yielded(
        &mut self,
        name: &str,
        index: &'a Expr,
        value: &'a Expr,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Option<(Place, &'a Expr)>, Gap> {
        let Some(blk) = vyrn_frontend::project::stored(name, index, value) else {
            return Ok(None);
        };
        let Some(k) = vyrn_frontend::project::store_node(blk)
            .and_then(|s| blk.stmts.iter().position(|t| std::ptr::eq(t, s)))
        else {
            return gap("an `atSet` expansion with no store", line);
        };
        let (into, part, stored) = match &blk.stmts[k] {
            Stmt::IndexSet {
                name, index, value, ..
            } => (name, Ok(index), value),
            Stmt::SetField {
                name, field, value, ..
            } => (name, Err(field), value),
            _ => return gap("an `atSet` expansion whose store is no place", line),
        };
        let group = blk.stmts[..k]
            .iter()
            .rposition(|s| !moves_out(s))
            .map_or(0, |j| j + 1);
        for s in &blk.stmts[..group] {
            self.stmt(s, out)?;
        }
        let base = match self.window(&blk.stmts[group..], out)? {
            Some((_, _, p, _)) => p,
            None => self.named_place(into, line)?.0,
        };
        let place = match part {
            Ok(index) => Place::Elem(Box::new(base), self.read_val(index, out)?),
            Err(field) => Place::Field(Box::new(base), field.clone()),
        };
        Ok(Some((place, if stored == value { value } else { stored })))
    }

    /// A removal whose receiver is a move-out window's temp
    /// ([`Builder::nested_store`]), stated with the window's place as its
    /// `modify` argument; the temp and its put-back are no rows.
    fn removal_at(
        &mut self,
        place: Place,
        ty: Type,
        temp: &str,
        store: &'a Stmt,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        let t = self.name(temp, ty, false, line);
        // The parser binds the temp `let mut` ([`moves_out`]).
        self.body.names[t as usize].mutable = true;
        self.scope.push((temp.to_string(), t));
        let mut rows = Vec::new();
        let lowered = self.stmt(store, &mut rows);
        if let Some(at) = self.scope.iter().rposition(|(_, n)| *n == t) {
            self.scope.remove(at);
        }
        lowered?;
        let mut placed = false;
        for r in &mut rows {
            if let St::Let(_, Rhs::Call { args, .. })
            | St::Do {
                rhs: Rhs::Call { args, .. },
                ..
            } = r
            {
                if let Some(a) = args
                    .first_mut()
                    .filter(|a| !placed && *a == &(Arg::Val(Val::Name(t)), Capability::Modify))
                {
                    a.0 = Arg::Place(place.clone());
                    placed = true;
                }
            }
        }
        let mut named = Vec::new();
        rows.iter().for_each(|s| names_in(s, &mut named));
        if !placed || named.contains(&t) {
            return gap(
                "a move-out window whose removal does not modify the temp alone",
                line,
            );
        }
        out.extend(rows);
        Ok(())
    }

    fn stmt(&mut self, s: &'a Stmt, out: &mut Vec<St>) -> Result<(), Gap> {
        self.stmt_rows(s, out)?;
        match self.owed.take() {
            Some((to, line)) => gap_d("a check of a validated record no binding took", &to, line),
            None => Ok(()),
        }
    }

    fn stmt_rows(&mut self, s: &'a Stmt, out: &mut Vec<St>) -> Result<(), Gap> {
        let sid = s as *const Stmt as usize;
        match s {
            Stmt::Let {
                name,
                value,
                line,
                ty: annotation,
                ..
            } => {
                if let Some(vty) = node_ty(value as *const Expr as usize) {
                    let decls = self.proto.types();
                    let refusal = match annotation {
                        Some(t) if !vyrn_frontend::types::coercible(&vty, t, decls) => {
                            Some(format!("`{name}` declared {t} but initializer is {vty}"))
                        }
                        _ if vyrn_frontend::types::resolve(&vty, decls) == Type::Unit => {
                            Some(format!("cannot bind `{name}` to a Unit value"))
                        }
                        _ => None,
                    };
                    self.body.mistyped.extend(refusal.map(|r| (*line, r)));
                }
                let ty = self.ty_of(value)?;
                let check = annotation
                    .as_ref()
                    .and_then(|t| self.checked(&ty, t, value));
                let copied = check.is_none() && self.copies(s, &ty);
                // Nothing may take module state: `let t = g`, `let t = consume g`
                // and `let t = consume g.f` bind a read of the place.
                let read = match value {
                    Expr::Consume { place, .. } if self.in_module_state(place) => &**place,
                    _ => value,
                };
                let global = self.in_module_state(read);
                if check.is_none()
                    && !copied
                    && (global || !matches!(read, Expr::Var { .. }))
                    && is_place_read(read)
                    && self.deferred_of(read).is_none()
                {
                    let place = self.place(read, out)?;
                    let n = self.name(name, ty.clone(), false, *line);
                    let rhs = Rhs::Read(place);
                    self.body.names[n as usize].not_owned =
                        self.report_reason(&rhs, &ty, false, false, self.lends(read));
                    out.push(St::Let(n, rhs));
                    self.release_receiver(read, out, true);
                    self.grows(n, name);
                    self.scope.push((name.clone(), n));
                    self.keyed_let(n, s);
                    return Ok(());
                }
                // A crossing into a validated type is its constructor:
                // `let a: Age = n` is `let a = Age(n)`.
                let (rhs, ty) = match check {
                    Some(to) => (self.check(&to, value, *line, out)?, Type::Named(to)),
                    None if copied => {
                        let outer = std::mem::take(&mut self.after);
                        let rhs = self.copy_of(value, out);
                        self.after_of_rhs = std::mem::replace(&mut self.after, outer);
                        (rhs?, ty)
                    }
                    None => (self.rhs(value, out)?, ty),
                };
                // A literal, or a nullary constructor: nothing allocated it.
                let static_value = match &rhs {
                    Rhs::Val(Val::Lit(l)) => !matches!(l, Lit::Opaque(_)),
                    _ if over_a_literal(&rhs) => true,
                    Rhs::Call {
                        kind: Callee::Ctor,
                        args,
                        ..
                    } if args.is_empty() => true,
                    Rhs::Val(Val::Name(m)) => matches!(
                        self.body.names[*m as usize].not_owned,
                        Some(NotOwned::Static)
                    ),
                    _ => false,
                };
                // A lending call binds a borrow whatever its type says; the
                // `Rhs` does not carry that. A copy lends nothing.
                let mutable = matches!(s, Stmt::Let { mutable: true, .. });
                let lends = !copied && self.lends(value);
                let owned = !lends && self.owned_binding(&rhs, &ty, static_value, mutable);
                let reason = self.report_reason(&rhs, &ty, static_value, mutable, lends);
                if owned {
                    self.loop_alias(&rhs, *line)?;
                }
                // Not owned is not borrowed: static data and a heapless value
                // are nobody's borrow.
                let borrow = !owned && (lends || matches!(&rhs, Rhs::Val(v) if self.borrows(v)));
                let n = self.name(name, ty, owned, *line);
                self.body.names[n as usize].borrow = borrow && self.body.names[n as usize].heap;
                self.body.names[n as usize].not_owned = reason;
                self.record_fields(n, value);
                // `let t = s` on a `read` parameter makes `t` a second name
                // for it, with its words and its must-use take exception.
                if let Rhs::Val(Val::Name(m)) = &rhs {
                    if self.body.names[n as usize].borrow {
                        self.body.names[n as usize].borrow_kind =
                            self.body.names[*m as usize].borrow_kind.clone();
                        self.body.names[n as usize].must_use_param =
                            self.body.names[*m as usize].must_use_param;
                    }
                }
                self.bind(n, rhs, out);
                // The unnamed receiver of the part read: released after the
                // read where the plan says this frame owns it.
                if reads_a_part(value) {
                    self.release_receiver(value, out, false);
                }
                self.grows(n, name);
                self.scope.push((name.clone(), n));
                self.keyed_let(n, s);
            }
            Stmt::Assign { name, value, line } => {
                if !self.known(name, *line, "assignment to unknown variable") {
                    return Ok(());
                }
                let to = match self.lookup(name) {
                    Some(n) => Some(self.body.names[n as usize].ty.clone()),
                    None => self.named_place(name, *line).ok().map(|(_, t)| t),
                };
                if let (Some(to), Some(vty)) = (&to, node_ty(value as *const Expr as usize)) {
                    if !vyrn_frontend::types::coercible(&vty, to, self.proto.types()) {
                        let refusal = format!("`{name}` is {to} but assigned {vty}");
                        self.body.mistyped.push((*line, refusal));
                    }
                }
                let n = self.lookup(name);
                // A crossing into a validated type is its constructor.
                let check = match &to {
                    Some(to) => self.checked(&self.ty_of(value)?, to, value),
                    None => None,
                };
                let grown = match (n, &check) {
                    (Some(n), None) if self.region == 0 && self.body.names[n as usize].grows => {
                        crate::append::self_append_spine(name, value).map(|parts| (n, parts))
                    }
                    // Module state grows through a read of it, which the row
                    // names as its receiver.
                    (None, None)
                        if self.region == 0
                            && crate::append::global_grows(name)
                            && vyrn_frontend::types::resolve(
                                &self.ty_of(value)?,
                                self.proto.types(),
                            ) == Type::Str =>
                    {
                        match crate::append::self_append_spine(name, value) {
                            Some(parts) => {
                                let mut root = value;
                                while let Expr::Binary { lhs, .. } = root {
                                    root = lhs;
                                }
                                let Val::Name(g) = self.global_read(root, name, *line, out)? else {
                                    return gap("a module-state read that names no value", *line);
                                };
                                self.body.names[g as usize].grows = true;
                                Some((g, parts))
                            }
                            None => None,
                        }
                    }
                    _ => None,
                };
                self.rebinding = grown.is_none();
                let v = match (check, grown) {
                    (_, Some((n, parts))) => self.str_append(n, &parts, *line, out),
                    (Some(to), None) => self.checked_temp(&to, value, *line, out).map(Val::Name),
                    _ => self.val(value, out),
                };
                self.rebinding = false;
                let v = v?;
                let ty = match n {
                    Some(n) => self.body.names[n as usize].ty.clone(),
                    None => self.ty_of(value)?,
                };
                // The rule for a store to a name or module state: the plan
                // says whether it releases the old value. A value that
                // mentions the place may hand the old buffer back
                // (`xs = xs.push(v)`), so the release stands down, unless the
                // plan proved every mention a read argument that cannot hand
                // it back (`store_is_fresh`), or the value is a String
                // concatenation, which builds a fresh buffer (`s = s + x`).
                // The hand-back is read off the statement, so it is the
                // core's answer; the rest of what a store displaces is the
                // kernel's.
                let mentions = vyrn_frontend::ast::mentions_place(value, name);
                let fresh_str = self.fresh_str(&ty, value);
                let handed_back = mentions && !fresh_str && !self.store_is_fresh(value, name);
                let key = self.store_key(sid);
                let releases = !handed_back && placed_store(key);
                // Module state owns what it holds and nothing may `consume`
                // it, so a store into one releases what it replaces whenever
                // that owns heap.
                let (place, owes) = match n {
                    None => (Place::Global(name.clone()), self.owns(&ty)),
                    Some(n) => {
                        // A rebind answers as a `let` does: `t = d.title` is a
                        // projection of `d`. A `mut` slot is released by its
                        // final value, so a slot ever assigned somebody
                        // else's place is not this frame's to release.
                        if self.borrows(&v) && self.body.names[n as usize].releases {
                            self.body.names[n as usize].releases = false;
                            self.body.names[n as usize].borrow = true;
                        }
                        (Place::Name(n), self.body.names[n as usize].releases)
                    }
                };
                // The hand-back comes before the place's obligation: a name
                // that owes no release still hands its buffer back, and the
                // word tells the two reasons apart.
                let old = if handed_back {
                    Old::Transferred
                } else if !owes {
                    Old::Nothing
                } else if releases {
                    Old::Released
                } else {
                    Old::Pending
                };
                out.push(St::Store {
                    place,
                    value: v,
                    old,
                    line: *line,
                    site: Site::Node(key),
                    releases,
                    holes: if releases {
                        store_holes(key)
                    } else {
                        Vec::new()
                    },
                });
                // [`Builder::str_append`] queues its operand temporaries so the
                // store stays next to its row.
                for t in std::mem::take(&mut self.after_of_rhs) {
                    out.push(St::Drop(t, Site::None, 0, None));
                }
            }
            Stmt::SetField {
                name,
                field,
                value,
                line,
            } => {
                if !self.known(name, *line, "assignment to field of unknown variable") {
                    return Ok(());
                }
                let base = self.named_place(name, *line)?;
                self.set_field(base, name, field, value, sid, *line, out)?;
            }
            Stmt::IndexSet {
                name,
                index,
                value,
                line,
            } => {
                if !self.known(name, *line, "index-assignment to unknown variable") {
                    return Ok(());
                }
                let base = self.named_place(name, *line)?;
                self.index_set(base, name, index, value, sid, *line, out)?;
            }
            Stmt::Return { value, line } => {
                let vty = match value {
                    Some(e) => node_ty(e as *const Expr as usize),
                    None => Some(Type::Unit),
                };
                if let (Some(vty), Some(ret)) = (vty, &self.ret) {
                    if !vyrn_frontend::types::coercible(&vty, ret, self.proto.types()) {
                        let refusal = format!("return type mismatch: expected {ret}, found {vty}");
                        self.body.mistyped.push((*line, refusal));
                    }
                }
                if let Some(e) = value {
                    if self.return_through(e, sid, *line, out)? {
                        return Ok(());
                    }
                }
                let v = match (value, self.ret.clone()) {
                    (Some(e), Some(r)) => Some(self.proven_val(e, Some(&r), *line, out)?),
                    (Some(e), None) => Some(self.val(e, out)?),
                    (None, _) => None,
                };
                self.return_exit(v, sid, *line, out)?;
            }
            Stmt::Break { line } => {
                if let Some(Some(u)) = self.walks.last().cloned() {
                    self.release_unreached(&u, out);
                }
                self.drops_at(Exit::Break, sid, out)?;
                out.push(St::Break {
                    site: sid,
                    line: *line,
                });
            }
            Stmt::Continue { line } => {
                self.drops_at(Exit::Continue, sid, out)?;
                out.push(St::Continue {
                    site: sid,
                    line: *line,
                });
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                line,
            } => {
                let c = self.condition(cond, "if", *line, out)?;
                let mut t = Vec::new();
                self.block(then_block, &mut t)?;
                self.edge_drops(sid, 0, &mut t)?;
                let mut e = Vec::new();
                if let Some(blk) = else_block {
                    self.block(blk, &mut e)?;
                }
                self.edge_drops(sid, 1, &mut e)?;
                out.push(St::If {
                    cond: c,
                    then: t,
                    els: e,
                    site: sid,
                });
            }
            Stmt::IfLet {
                pattern,
                scrutinee,
                then_block,
                else_block,
                line,
            } => {
                if self.optional_if_let(
                    pattern,
                    scrutinee,
                    then_block,
                    else_block.as_ref(),
                    sid,
                    *line,
                    out,
                )? {
                    return Ok(());
                }
                let sty = self.ty_of(scrutinee)?;
                let (sv, consuming) = self.scrutinee(scrutinee, sid, None, out)?;
                let owns = self.owns_boxes(scrutinee, consuming);
                let mut t = Vec::new();
                let mark = self.scope.len();
                let from = borrow_root(&sv, owns);
                let binds = self.bind_pattern(pattern, &sty, consuming, *line, from, &mut t)?;
                self.block(then_block, &mut t)?;
                let frees = self.arm_frees(sid, 0, &binds, &mut t);
                self.edge_drops(sid, 0, &mut t)?;
                self.scope.truncate(mark);
                let mut e = Vec::new();
                if let Some(blk) = else_block {
                    self.block(blk, &mut e)?;
                }
                self.edge_drops(sid, 1, &mut e)?;
                out.push(St::Switch {
                    on: sv,
                    arms: vec![
                        Arm {
                            frees: Some(frees),
                            binds,
                            body: t,
                            test: self.arm_test(pattern, &sty, *line)?,
                            site: sid,
                            index: 0,
                        },
                        Arm {
                            frees: Some(Vec::new()),
                            binds: Vec::new(),
                            body: e,
                            test: Test::Else,
                            site: sid,
                            index: 1,
                        },
                    ],
                    consuming,
                    carries: false,
                    owns,
                    site: sid,
                    line: *line,
                });
                self.drops_at(Exit::Scrutinee, sid, out)?;
            }
            Stmt::While { cond, body, line } => {
                let mut l = Vec::new();
                let c = self.condition(cond, "while", *line, &mut l)?;
                l.push(St::If {
                    cond: c,
                    then: Vec::new(),
                    els: vec![St::Break { site: 0, line: 0 }],
                    site: 0,
                });
                self.loop_marks.push(self.body.names.len());
                self.walks.push(None);
                let r = self.block(body, &mut l);
                self.walks.pop();
                self.loop_marks.pop();
                r?;
                self.hoist_headers(&mut l, *line, out);
                out.push(St::Loop { body: l, site: sid });
            }
            Stmt::ForIn {
                var,
                iter,
                body,
                line,
                consuming,
                col: _,
            } => {
                let ity = self.ty_of(iter)?;
                // A user container's element is what its `nth` projection
                // yields. A `for` walks no map.
                let elem = self
                    .elem_ty(&ity, *line)
                    .ok()
                    .filter(|_| !self.is_map(&ity));
                let Some(ety) = elem.or_else(|| self.projected_elem(&ity)) else {
                    let t = vyrn_frontend::types::resolve(&ity, self.proto.types());
                    if t != Type::Err {
                        let refusal = format!(
                            "`for` needs an Array, a String, or a type that declares \
                             `impl Iterate` (a `size` method and an `nth` projection, \
                             `fn nth(read self, ..) -> read T`), found {t}"
                        );
                        self.body.mistyped.push((*line, refusal));
                    }
                    return gap("a `for` over what no loop walks", *line);
                };
                if *consuming {
                    take_names_a_place(iter, *line, true)?;
                }
                if let Some(blk) = self.iterated(var, iter, body, &ity).filter(|_| !*consuming) {
                    return self.iterate(blk, iter, ity, body, out);
                }
                // `owner` is the name the element rule below asks about: the
                // named container where the loop borrows it.
                let mut owner = None;
                let it = match iter {
                    Expr::Var { name, .. } if self.lookup(name).is_some() => {
                        let n = self.lookup(name).unwrap();
                        let pulled = matches!(
                            vyrn_frontend::types::resolve(&ity, &self.proto.types()),
                            Type::Stream(_)
                        );
                        if *consuming {
                            let t = self.temp(ity.clone(), *line);
                            out.push(St::Let(t, Rhs::Val(Val::Name(n))));
                            self.keyed(t, sid);
                            t
                        } else if pulled {
                            // A stream is pulled to its end and closed by the
                            // loop through its own name (below).
                            n
                        } else {
                            // The loop reads the container through a borrow,
                            // so the kernel refuses a store over the name in
                            // the body (`ys = []` would free the buffer the
                            // loop still walks).
                            owner = Some(n);
                            let t = self.borrow_name(iter, ity.clone(), *line);
                            self.body.names[t as usize].walked = Some(Walk::For);
                            out.push(St::Let(t, Rhs::Read(Place::Name(n))));
                            t
                        }
                    }
                    _ if !*consuming && is_place_read(iter) => {
                        // `for p in e.path`: the loop walks a container
                        // somebody else owns.
                        let place = self.place(iter, out)?;
                        // No drain encloses the receiver temporary, so it
                        // stays held and the judgment sees it.
                        self.pending_receiver = None;
                        let t = self.borrow_name(iter, ity.clone(), *line);
                        self.body.names[t as usize].walked = Some(Walk::For);
                        out.push(St::Let(t, Rhs::Read(place)));
                        t
                    }
                    _ => {
                        match self.val(iter, out)? {
                            // The construct owns the temporary; the plan keys
                            // its release rows by the statement.
                            Val::Name(t) => {
                                self.keyed(t, sid);
                                t
                            }
                            // A String literal is static: its name owns nothing.
                            lit => {
                                let t = self.name("@lit", ity.clone(), false, *line);
                                out.push(St::Let(t, Rhs::Val(lit)));
                                t
                            }
                        }
                    }
                };
                // Whatever spelling brought the container here, the loop took
                // it, and a refusal about the take names the loop.
                if *consuming {
                    self.body.names[it as usize].for_consume = true;
                }
                let decls = vyrn_frontend::types::decl_map(self.program);
                let streaming =
                    matches!(vyrn_frontend::types::resolve(&ity, &decls), Type::Stream(_));
                if streaming {
                    self.stream_loops.push(it);
                }
                // `for x in xs` walks a named index: the length read once
                // before the loop, and a counter from zero that steps right
                // after the element is read, so a `continue` needs no step. A
                // stream is pulled and has neither.
                let counter = if streaming {
                    None
                } else {
                    let n = self.temp(Type::Int, *line);
                    let len = self.length_of(it, &ity, *line)?;
                    out.push(St::Let(n, len));
                    let i = self.temp(Type::Int, *line);
                    out.push(St::Let(i, Rhs::Val(Val::Lit(Lit::Int(0)))));
                    Some((n, i))
                };
                let mut l = Vec::new();
                // The `if .. else break` a `while` has at its top. Without it
                // the path after the `for` is dead to the judgment, and the
                // placer rewrote hole sets from the other join edge alone
                // (`std/vyx`'s `vyxMergeImports`).
                let (cond, index) = match counter {
                    Some((n, i)) => {
                        let c = self.temp(Type::Bool, *line);
                        l.push(St::Let(
                            c,
                            Rhs::Prim(
                                Op::Bin(BinOp::Lt),
                                vec![Val::Name(i), Val::Name(n)],
                                Some(Type::Bool),
                            ),
                        ));
                        (Val::Name(c), Val::Name(i))
                    }
                    // A stream is pulled: one call answers whether an element
                    // came, and its name stands for the element below.
                    None => {
                        let c = self.temp(Type::Bool, *line);
                        l.push(St::Let(
                            c,
                            Rhs::Call {
                                callee: "@pull".into(),
                                args: vec![(Arg::Val(Val::Name(it)), Capability::Modify)],
                                write_back: false,
                                kind: Callee::Reserved,
                                ret: Some(Type::Bool),
                                solved: Vec::new(),
                                targets: Vec::new(),
                            },
                        ));
                        (Val::Name(c), Val::Name(c))
                    }
                };
                l.push(St::If {
                    cond,
                    then: Vec::new(),
                    els: vec![St::Break { site: 0, line: 0 }],
                    site: 0,
                });
                // Each turn owns its element where the element type owns heap
                // and either the container is a stream (a pulled element has
                // no other owner), or the variable is handed on in the body
                // ([`last_owner`], `Cand::Elem`) out of an unnamed container
                // this frame owns. A named container outlives the loop, so
                // `for r in ns` only borrows; a lender's result is somebody
                // else's buffer.
                let ekey = vyrn_frontend::own::for_var_key(var);
                let ic = &self.body.names[owner.unwrap_or(it) as usize];
                let loops_alone = !ic.borrow && !ic.bound_by_let;
                let owned =
                    self.owns(&ety) && (streaming || loops_alone && self.seed.contains(&ekey));
                // Where every element left through the variable, the
                // container's release frees the buffer alone (field 0 of a
                // growable array's triple; other containers have no such
                // buffer). A wrong answer here frees somebody else's storage.
                let mut unreached = None;
                if owned && matches!(vyrn_frontend::types::resolve(&ity, &decls), Type::Array(_)) {
                    self.body.loop_buffers.push(sid);
                    unreached = counter.map(|(n, i)| Unreached {
                        it,
                        i,
                        n,
                        elem: ety.clone(),
                        line: *line,
                    });
                }
                // Before the variable: each turn binds its own element, so it
                // may leave a join arm; the container, below the mark, may not.
                self.loop_marks.push(self.body.names.len());
                let x = self.name(var, ety, owned, *line);
                self.body.cands.push((ekey, x, Cand::Elem));
                // The container outlives the loop, so a refusal names the
                // variable as a loop variable, as the checker does.
                if !*consuming && self.body.names[x as usize].borrow {
                    let of = vyrn_frontend::ast::place_path(iter)
                        .map(|(r, _)| r)
                        .unwrap_or_default();
                    self.body.names[x as usize].loop_var = Some(of);
                }
                // The variable has no `let` node; the plan keys it by its
                // spelling's buffer, which is one address per loop.
                self.keyed(x, vyrn_frontend::own::for_var_key(var));
                let mut head = vec![St::Let(
                    x,
                    Rhs::Read(Place::Elem(Box::new(Place::Name(it)), index)),
                )];
                if let Some((_, i)) = counter {
                    self.step(i, &mut head);
                }
                let mark = self.scope.len();
                self.scope.push((var.clone(), x));
                self.walks.push(unreached);
                let r = self.block_with(body, head, &mut l);
                self.walks.pop();
                self.loop_marks.pop();
                r?;
                self.scope.truncate(mark);
                out.push(St::Loop { body: l, site: sid });
                if streaming {
                    self.stream_loops.pop();
                    // Pulled to its end or left by a `break`, the stream is
                    // closed here by its last owner, the loop.
                    if self.stream_owed(it) && self.taken_by_loop(it, sid) {
                        out.push(St::Drop(it, Site::None, 0, None));
                    }
                } else if *consuming && self.taken_by_loop(it, sid) {
                    // The loop is the container's last owner and releases it,
                    // keyed by the loop so emitters read the judgment rather
                    // than the source's `consume`.
                    out.push(St::Drop(it, Site::Node(sid), 0, None));
                }
                self.drops_at(Exit::Scrutinee, sid, out)?;
            }
            Stmt::Drop { name, line } => {
                let Some(n) = self.lookup(name) else {
                    self.body.unbound_drops.push((name.clone(), *line));
                    return Ok(());
                };
                // An ordinary drop inside a `region` too: `free` refuses an
                // arena block by its class word.
                out.push(St::Drop(n, Site::Node(sid), *line, None));
            }
            // An unaudited build states neither the audit hook nor its operand;
            // `p + 8` alone costs four instructions per allocation.
            Stmt::Expr(Expr::Call { name, .. })
                if vyrn_frontend::loader::audit_hook(name)
                    && !vyrn_frontend::loader::audit_build() => {}
            Stmt::Expr(e) => {
                let ty = self.ty_of(e).unwrap_or(Type::Unit);
                let rhs = self.rhs(e, out)?;
                if self.owns(&ty) || self.owed.is_some() {
                    let owns = self.owns(&ty);
                    let t = self.temp(ty, e.line());
                    self.bind(t, rhs, out);
                    if owns && self.discards(e) {
                        out.push(St::Drop(t, Site::Node(sid), 0, None));
                    }
                } else {
                    // A bare value does nothing: a Unit `match` or `if` yields
                    // a join no arm writes.
                    if !matches!(rhs, Rhs::Val(_)) {
                        out.push(St::Do {
                            rhs,
                            line: e.line(),
                            site: sid,
                        });
                    }
                    for t in std::mem::take(&mut self.after_of_rhs) {
                        out.push(St::Drop(t, Site::None, 0, None));
                    }
                }
            }
            // The arena owns only what `direct.rs`'s `arena_route` routes into
            // it, and the closing brace is the runtime's, so the body is an
            // ordinary block here.
            Stmt::Region { body, .. } => {
                self.region += 1;
                let r = self.block(body, out);
                self.region -= 1;
                r?;
                if let Some(St::Block { region, .. }) = out.last_mut() {
                    *region = true;
                }
            }
        }
        Ok(())
    }

    /// Whether a statement-position call's unbound result is this frame's to
    /// release right after the call. The caller has checked that the type
    /// owns heap. Excluded: a lending call, a variant constructor, a `panic`,
    /// and an `@`-spelled desugar, which the reading site frees,
    /// except a removal (`@pop`, `@swapRemove`), whose result nothing reads.
    fn discards(&self, e: &Expr) -> bool {
        let Expr::Call { name, .. } = e else {
            return false;
        };
        !vyrn_frontend::ast::is_panic(name)
            && (!name.starts_with('@') || matches!(builtin_row(name), Some(Spec::Removes)))
            && !self.lends(e)
            && !self.constructs(name)
    }

    /// Whether `name` constructs a sum value: a user enum's variant, or one of
    /// the five the language declares (`declared::Declared`'s seed).
    fn constructs(&self, name: &str) -> bool {
        matches!(name, "Some" | "Ok" | "Err" | "Success" | "Failure") || self.is_variant(name)
    }

    /// Marks `n`, bound by a `let` of `name`, as a String accumulator where
    /// the whitelist admits the name.
    fn grows(&mut self, n: Name, name: &str) {
        let info = &mut self.body.names[n as usize];
        info.grows = self.appends.contains(name)
            && vyrn_frontend::types::resolve(&info.ty, self.proto.types()) == Type::Str;
    }

    /// `s = s + a + b` on an accumulator: one `@strAppend` row that reads `s`
    /// and each part in written order; the store after it puts the result
    /// back into `s`. The parts' temporaries stay queued until the caller has
    /// pushed the store.
    fn str_append(
        &mut self,
        s: Name,
        parts: &[&'a Expr],
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Val, Gap> {
        let outer = std::mem::take(&mut self.after);
        self.drain += 1;
        let mut args = vec![(Arg::Val(Val::Name(s)), Capability::Read)];
        let read = parts.iter().try_for_each(|p| {
            args.push((
                Arg::Val(self.read_arg(p, out, "@concat", 1)?),
                Capability::Read,
            ));
            Ok(())
        });
        self.drain -= 1;
        self.after_of_rhs = std::mem::replace(&mut self.after, outer);
        read?;
        let t = self.temp(Type::Str, line);
        out.push(St::Let(
            t,
            Rhs::Call {
                callee: "@strAppend".into(),
                args,
                write_back: false,
                kind: Callee::Reserved,
                ret: Some(Type::Str),
                solved: Vec::new(),
                targets: Vec::new(),
            },
        ));
        Ok(Val::Name(t))
    }

    /// Whether the store value is a String concatenation, which builds a fresh
    /// buffer, so `s = s + x` displaces the old one rather than handing it
    /// back.
    fn fresh_str(&self, ty: &Type, value: &Expr) -> bool {
        matches!(
            vyrn_frontend::types::resolve(ty, self.proto.types()),
            Type::Str
        ) && matches!(
            value,
            Expr::Binary {
                op: vyrn_frontend::ast::BinOp::Add,
                ..
            }
        )
    }

    /// The node a store's row is keyed by, which is the node its readers key
    /// it by: the statement itself, except for a user container's `c[h] = v`,
    /// whose store statements the `place at` rewrite builds.
    fn store_key(&self, sid: usize) -> usize {
        self.own.plan.key_of(sid)
    }

    fn old_for(&self, ty: &Type, releases: bool) -> Old {
        if !self.owns(ty) {
            Old::Nothing
        } else if releases {
            Old::Released
        } else {
            // The kernel answers over the root, the one place a sub-place's
            // ownership can be read from.
            Old::Pending
        }
    }

    /// Releases the payload binders the kernel found still held where this
    /// arm ends. The first build states none, so [`crate::kernel::placement`]
    /// reports every held binder, and the second build reads the rows back
    /// out of [`Placed`].
    fn arm_frees(&mut self, site: usize, arm: u32, binds: &[Name], out: &mut Vec<St>) -> Vec<Name> {
        let mut frees: Vec<Name> = Vec::new();
        let Some(rows) = placed_arm(site, arm) else {
            return frees;
        };
        for b in binds {
            let src = self.body.names[*b as usize].source.clone();
            if let Some((_, holes)) = rows.iter().find(|(n, _)| *n == src) {
                self.body.names[*b as usize].holes =
                    holes.iter().map(|h| format!(".{h}")).collect();
                out.push(St::Drop(*b, Site::None, 0, None));
                frees.push(*b);
            }
        }
        frees
    }

    /// Whether the stream `it` a `for` walks is this frame's to close: one it
    /// holds, or a parameter, which carries the obligation into the callee
    /// ([`NameInfo::must_use_param`]).
    fn stream_owed(&self, it: Name) -> bool {
        let info = &self.body.names[it as usize];
        info.releases || info.must_use_param
    }

    /// The rows a `return` or a `?` runs for every enclosing `for`, innermost
    /// first: the elements no turn reached, then the stream it walks, closed.
    fn leave_loops(&mut self, out: &mut Vec<St>) {
        for u in self.walks.clone().iter().rev().flatten() {
            self.release_unreached(u, out);
        }
        for it in self.stream_loops.iter().rev() {
            if self.stream_owed(*it) {
                out.push(St::Drop(*it, Site::None, 0, None));
            }
        }
    }

    /// Releases the elements of `u`'s container from its counter to its
    /// length, which no turn bound: the rows a `return`, a `?` or a `break`
    /// runs before its own. The element the turn bound is the body's.
    fn release_unreached(&mut self, u: &Unreached, out: &mut Vec<St>) {
        let c = self.temp(Type::Bool, u.line);
        let mut l = vec![
            St::Let(
                c,
                Rhs::Prim(
                    Op::Bin(BinOp::Lt),
                    vec![Val::Name(u.i), Val::Name(u.n)],
                    Some(Type::Bool),
                ),
            ),
            St::If {
                cond: Val::Name(c),
                then: Vec::new(),
                els: vec![St::Break { site: 0, line: 0 }],
                site: 0,
            },
        ];
        let e = self.temp(u.elem.clone(), u.line);
        l.push(St::Let(
            e,
            Rhs::Read(Place::Elem(Box::new(Place::Name(u.it)), Val::Name(u.i))),
        ));
        self.step(u.i, &mut l);
        l.push(St::Drop(e, Site::None, 0, None));
        out.push(St::Loop { body: l, site: 0 });
    }

    /// Rule N: the drops one edge of a join owes.
    fn edge_drops(&mut self, join: usize, edge: u32, out: &mut Vec<St>) -> Result<(), Gap> {
        let Some(ers) = placed_edges(join) else {
            return Ok(());
        };
        for (name, e, holes) in &ers {
            if *e != edge {
                continue;
            }
            // `d.line`: a sub-place the other edge took, released on this one
            // as a take into a temporary dropped at once, so the kernel sees
            // the hole.
            let mut parts = name.split('.');
            let root = parts.next().unwrap_or_default();
            let Some(n) = self.lookup(root) else {
                return gap_d("an edge release of a name out of scope", name, 0);
            };
            let mut place = Place::Name(n);
            let mut ty = self.body.names[n as usize].ty.clone();
            let mut sub = false;
            for f in parts {
                ty = self.field_ty(&ty, f, 0)?;
                place = Place::Field(Box::new(place), f.to_string());
                sub = true;
            }
            let at = Site::Edge(join, edge);
            if sub {
                let t = self.temp(ty, self.body.names[n as usize].line);
                // Spelled as the sub-place, so a reader gets the row's name.
                self.body.names[t as usize].source = name.clone();
                out.push(St::Let(t, Rhs::Take(place)));
                out.push(St::Drop(t, at, 0, None));
            } else {
                let holes =
                    (!holes.is_empty()).then(|| holes.iter().map(|h| format!(".{h}")).collect());
                out.push(St::Drop(n, at, 0, holes));
            }
        }
        Ok(())
    }

    /// A store into `field` of the place `base`, which the source names `name`.
    #[allow(clippy::too_many_arguments)]
    fn set_field(
        &mut self,
        base: (Place, Type),
        name: &str,
        field: &str,
        value: &'a Expr,
        sid: usize,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        self.field_store(&base.1, name, field, value, line)?;
        let (base, bty) = base;
        let fty = self.field_ty(&bty, field, line)?;
        let v = self.proven_val(value, Some(&fty), line, out)?;
        // A name store's hand-back rule, one dot down:
        // `s.dense = s.dense.push(i)` releases nothing.
        let handed_back =
            vyrn_frontend::ast::mentions_place(value, name) && !self.fresh_str(&fty, value);
        let key = self.store_key(sid);
        let releases = !handed_back && placed_store(key);
        out.push(St::Store {
            place: Place::Field(Box::new(base), field.to_string()),
            value: v,
            old: if handed_back {
                Old::Transferred
            } else {
                self.old_for(&fty, releases)
            },
            line,
            site: Site::Node(key),
            releases,
            holes: if releases {
                store_holes(key)
            } else {
                Vec::new()
            },
        });
        Ok(())
    }

    /// The rules of a store into `name.field`, whose root has type `bty`: a
    /// validated record is rebuilt, not mutated; the root has the field; the
    /// field takes the value. A root without the field is a refused gap.
    fn field_store(
        &mut self,
        bty: &Type,
        name: &str,
        field: &str,
        value: &Expr,
        line: usize,
    ) -> Result<(), Gap> {
        let decls = self.proto.types();
        let fields = vyrn_frontend::types::record_fields(bty, decls);
        let refusal = match bty {
            Type::Err => return Ok(()),
            Type::Named(n) if decls.get(n).is_some_and(|d| d.predicate.is_some()) => format!(
                "cannot mutate a field of `{n}` in place (its `where` invariant could be broken mid-update); rebuild it: `{name} = {n} {{ .. }}`"
            ),
            _ => match fields.as_ref().map(|fs| fs.iter().find(|f| f.name == field)) {
                None => format!("`{name}` is not a record, so it has no field `{field}`"),
                Some(None) => format!("record `{name}` has no field `{field}`"),
                Some(Some(f)) => {
                    let fty = &f.ty;
                    let Some(vty) = node_ty(value as *const Expr as usize) else {
                        return Ok(());
                    };
                    let validated = matches!(fty, Type::Named(n)
                        if decls.get(n).is_some_and(|d| d.predicate.is_some()));
                    let refusal = if validated {
                        (!vyrn_frontend::types::assignable(&vty, fty, decls)).then(|| {
                            format!(
                                "field `{field}` is {fty} (validated); assign an already-constructed `{fty}` value, e.g. `{fty}(..)`"
                            )
                        })
                    } else {
                        (!vyrn_frontend::types::coercible(&vty, fty, decls))
                            .then(|| format!("field `{field}` is {fty} but assigned {vty}"))
                    };
                    self.body.mistyped.extend(refusal.map(|r| (line, r)));
                    return Ok(());
                }
            },
        };
        self.body.mistyped.push((line, refusal));
        gap("a store into a field its root has not", line)
    }

    /// The rules of a store into `name[index]`, whose root has type `bty`: the
    /// root is a container, the index is its key type, and the element takes
    /// the value. A root that is no container is a refused gap.
    fn index_store(
        &mut self,
        bty: &Type,
        name: &str,
        index: &Expr,
        value: &Expr,
        line: usize,
    ) -> Result<(), Gap> {
        let decls = self.proto.types();
        let coercible = |a: &Type, b: &Type| vyrn_frontend::types::coercible(a, b, decls);
        let (ity, vty) = (
            node_ty(index as *const Expr as usize),
            node_ty(value as *const Expr as usize),
        );
        let refusal = match vyrn_frontend::types::resolve(bty, decls) {
            Type::Err => None,
            Type::Map(key, val) => {
                let k = ity.map(|t| vyrn_frontend::types::resolve(&t, decls));
                match (k, vty) {
                    // Both at their base, as a lookup takes its key.
                    (Some(k), _)
                        if k != Type::Err
                            && !coercible(&k, &vyrn_frontend::types::resolve(&key, decls)) =>
                    {
                        Some(format!(
                            "`{name}` is keyed by {key}, but the key here is {k}"
                        ))
                    }
                    (_, Some(v)) if !coercible(&v, &val) => Some(format!(
                        "`{name}` holds values of type {val} but the stored value is {v}"
                    )),
                    _ => None,
                }
            }
            shape => {
                let (key, elem) = match shape {
                    Type::Array(e) | Type::ArrayN(e, _) | Type::SmallArray(e, _) => (Type::Int, *e),
                    other => {
                        match vyrn_frontend::project::lookup_in(&self.program.impls, bty, "atSet") {
                            Some(f) => (
                                f.params
                                    .get(1)
                                    .map_or(Type::Int, |p| self.under_impl(&p.ty, bty)),
                                self.under_impl(&f.ret, bty),
                            ),
                            None => {
                                let refusal = format!(
                                "`{name}[i] = ..` needs an Array, a Map, or a type whose impl declares the `atSet` projection (`fn atSet(modify self, ..) -> modify T`), found {other}"
                            );
                                self.body.mistyped.push((line, refusal));
                                return gap("a store into an element of what has none", line);
                            }
                        }
                    }
                };
                let i = ity.filter(|i| {
                    !coercible(i, &key) && vyrn_frontend::types::resolve(i, decls) != Type::Err
                });
                match (i, vty) {
                    (Some(i), _) if key == Type::Int => {
                        Some(format!("array index must be an Int64, found {i}"))
                    }
                    (Some(i), _) => Some(format!("`{name}[..] = ..` is keyed by {key}, found {i}")),
                    (None, Some(v)) if !coercible(&v, &elem) => {
                        Some(format!("`{name}` holds {elem} but the stored value is {v}"))
                    }
                    _ => None,
                }
            }
        };
        self.body.mistyped.extend(refusal.map(|r| (line, r)));
        Ok(())
    }

    /// A store into the element or the entry of the place `base` at `index`,
    /// which the source names `name`.
    #[allow(clippy::too_many_arguments)]
    fn index_set(
        &mut self,
        base: (Place, Type),
        name: &str,
        index: &'a Expr,
        value: &'a Expr,
        sid: usize,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        self.index_store(&base.1, name, index, value, line)?;
        let (base, bty) = base;
        // A user container's element is the place its `atSet` yields,
        // after the projection's prologue.
        let yielded = if self.projected(&bty) {
            self.yielded(name, index, value, line, out)?
        } else {
            None
        };
        let (place, stored) = match yielded {
            Some(y) => y,
            None if self.is_map(&bty) => {
                let k = self.val(index, out)?;
                (Place::Key(Box::new(base), k), value)
            }
            None => {
                let i = self.read_val(index, out)?;
                (Place::Elem(Box::new(base), i), value)
            }
        };
        // A user container's element type is the value's.
        let ety = match self.elem_ty(&bty, line) {
            Ok(t) => t,
            Err(_) => self.ty_of(value)?,
        };
        // A crossing into a validated element is its constructor, proven or
        // not.
        let v = match self.checked(&self.ty_of(stored)?, &ety, stored) {
            Some(to) => Val::Name(self.checked_temp(&to, stored, line, out)?),
            None => self.val(stored, out)?,
        };
        let key = self.store_key(sid);
        let site = Site::Node(key);
        // The same hand-back, and the index counts too: `xs[i] = xs[j]` and
        // `xs[xs.length - 1] = v` read the buffer the store writes into.
        // `xs[i] = xs[j].copy()` hands nothing back.
        let handed_back = (vyrn_frontend::ast::mentions_place(value, name)
            && !self.store_is_fresh(value, name))
            || vyrn_frontend::ast::mentions_place(index, name);
        let releases = !handed_back && placed_store(key);
        out.push(St::Store {
            place,
            value: v,
            old: if handed_back {
                Old::Transferred
            } else {
                self.old_for(&ety, releases)
            },
            line,
            site,
            releases,
            holes: if releases {
                store_holes(key)
            } else {
                Vec::new()
            },
        });
        Ok(())
    }

    /// Whether a binding or module state answers `name`. Where none does,
    /// records the refusal: `words` is the sentence before the name.
    fn known(&mut self, name: &str, line: usize, words: &str) -> bool {
        if self.answers(name) {
            return true;
        }
        let refusal = format!("{words} `{name}`");
        self.body.refused.push((line, refusal));
        false
    }

    /// The condition of an `if` or a `while`, read. A condition that is not
    /// Bool is refused; one typed `Err` is refused where its name is.
    fn condition(
        &mut self,
        cond: &'a Expr,
        word: &str,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Val, Gap> {
        let t = self.ty_of(cond)?;
        let bool = vyrn_frontend::types::resolve(&t, self.proto.types()) == Type::Bool;
        if !bool && t != Type::Err {
            let refusal = format!("`{word}` condition must be Bool, found {t}");
            self.body.mistyped.push((line, refusal));
        }
        self.read_val(cond, out)
    }

    fn answers(&self, name: &str) -> bool {
        self.lookup(name).is_some()
            || !self.closed && self.program.globals.iter().any(|g| g.name == name)
    }

    /// The refusal of `e` where it names nothing: a variable no binding or
    /// module state answers, or `T?(..)` of an undeclared type.
    fn unknown_of(&self, e: &Expr) -> Option<(usize, String)> {
        match e {
            Expr::Var { name, line } if !self.answers(name) && !self.is_variant(name) => {
                Some((*line, format!("unknown variable `{name}`")))
            }
            Expr::TryConstruct { name, line, .. } if !self.proto.types().contains_key(name) => {
                Some((*line, format!("unknown type `{name}`")))
            }
            _ => None,
        }
    }

    /// The receiver rule of `pop` and `swapRemove`: a `mut` name they shrink
    /// is a growable array. A receiver that is no name, or not `mut`, is
    /// refused elsewhere (the checker's `mut_array_receiver`,
    /// [`crate::typed::stores`]).
    fn shrinks(&mut self, op: &str, recv: &Expr, line: usize) {
        let Expr::Var { name, .. } = recv else {
            return;
        };
        let mutable = match self.lookup(name) {
            Some(n) => self.body.names[n as usize].mutable,
            None => self
                .program
                .globals
                .iter()
                .any(|g| &g.name == name && g.mutable),
        };
        let Some((_, ty)) = self.named_place(name, line).ok().filter(|_| mutable) else {
            return;
        };
        let refusal = match vyrn_frontend::types::resolve(&ty, self.proto.types()) {
            Type::Array(_) | Type::SmallArray(..) | Type::Err => return,
            Type::ArrayN(..) => format!(
                "`{op}` is not available on a fixed-size array (it cannot shrink); use a growable `Array<T>`"
            ),
            other => format!("`{op}` needs an `Array<T>`, found {other}"),
        };
        self.body.mistyped.push((line, refusal));
    }

    fn unknown_at(&mut self, e: &Expr) {
        self.body.refused.extend(self.unknown_of(e));
    }

    /// A name as a place: a binding of this body, or module state with its
    /// declared type.
    fn named_place(&self, name: &str, line: usize) -> Result<(Place, Type), Gap> {
        if let Some(n) = self.lookup(name) {
            return Ok((Place::Name(n), self.body.names[n as usize].ty.clone()));
        }
        match self.program.globals.iter().find(|g| &g.name == name) {
            Some(g) => match g
                .ty
                .clone()
                .or_else(|| node_ty(&g.init as *const Expr as usize))
            {
                Some(t) => Ok((Place::Global(name.to_string()), t)),
                None => gap_d("a global the checker did not type", name, line),
            },
            None => gap("a place that is not a binding", line),
        }
    }

    /// Binds, once before the loop, the header of every heap container the
    /// loop `l` indexes and no row of it rebuilds ([`crate::kernel::writes`]),
    /// and points the loop's element and length reads at it. The header is a
    /// borrow the loop walks, so the kernel keeps its alias. A heapless
    /// container is a value, read in place each turn. For module state, a
    /// call that stores into it counts as a write.
    fn hoist_headers(&mut self, l: &mut [St], line: usize, out: &mut Vec<St>) {
        let mut read = Vec::new();
        l.iter_mut().for_each(|s| header_reads(s, None, &mut read));
        read.sort_unstable_by(|a, b| match (a, b) {
            (Root::N(x), Root::N(y)) => x.cmp(y),
            (Root::G(x), Root::G(y)) => x.cmp(y),
            (Root::N(_), Root::G(_)) => std::cmp::Ordering::Less,
            (Root::G(_), Root::N(_)) => std::cmp::Ordering::Greater,
        });
        read.dedup();
        let mut bound = Vec::new();
        l.iter().for_each(|s| names_bound(s, &mut bound));
        let decls = self.proto.types();
        for r in read {
            let (ty, path, from, heap) = match &r {
                Root::N(n) => {
                    let info = &self.body.names[*n as usize];
                    if bound.contains(n) {
                        continue;
                    }
                    let path = info.path.clone().unwrap_or_else(|| info.source.clone());
                    (info.ty.clone(), path, Place::Name(*n), info.heap)
                }
                Root::G(g) => match self.named_place(g, line) {
                    Ok((from @ Place::Global(_), ty)) => {
                        let heap = self.proto.owns_heap(&ty);
                        (ty, g.clone(), from, heap)
                    }
                    _ => continue,
                },
            };
            let indexed = matches!(
                vyrn_frontend::types::resolve(&ty, &decls),
                Type::Array(_) | Type::SmallArray(..) | Type::Str
            );
            if !indexed
                || !heap
                || crate::kernel::writes(l, r.clone(), &self.body.names, &self.body.name)
            {
                continue;
            }
            let h = self.name("@borrow", ty, false, line);
            self.body.names[h as usize].walked = Some(Walk::While);
            self.body.names[h as usize].path = Some(path);
            out.push(St::Let(h, Rhs::Read(from)));
            l.iter_mut()
                .for_each(|s| header_reads(s, Some((&r, h)), &mut Vec::new()));
        }
    }

    /// The length a `for` over `it` walks to: a header read for a built-in
    /// container, the `Iterate` impl's `size` for a user one.
    fn length_of(&self, it: Name, ity: &Type, line: usize) -> Result<Rhs, Gap> {
        let decls = self.proto.types();
        let field = |f: &str| Rhs::Read(Place::Field(Box::new(Place::Name(it)), f.to_string()));
        Ok(match vyrn_frontend::types::resolve(ity, &decls) {
            Type::Str => field("byteLength"),
            Type::Array(_) | Type::ArrayN(..) | Type::SmallArray(..) | Type::Map(..) => {
                field("length")
            }
            _ => match vyrn_frontend::types::iterate_impl(&self.program.impls, ity) {
                Some((size, _)) => {
                    let solved = self.impl_args(&size, ity);
                    Rhs::Call {
                        kind: if solved.is_some() {
                            Callee::Fn
                        } else {
                            Callee::Method
                        },
                        callee: size,
                        args: vec![(Arg::Val(Val::Name(it)), Capability::Read)],
                        write_back: false,
                        ret: Some(Type::Int),
                        solved: solved.unwrap_or_default(),
                        targets: Vec::new(),
                    }
                }
                None => return gap("a `for` over a container with no length", line),
            },
        })
    }

    /// `i = i + 1`, for the counter of a `for` over an index.
    fn step(&mut self, i: Name, out: &mut Vec<St>) {
        let line = self.body.names[i as usize].line;
        let t = self.temp(Type::Int, line);
        out.push(St::Let(
            t,
            Rhs::Prim(
                Op::Bin(BinOp::Add),
                vec![Val::Name(i), Val::Lit(Lit::Int(1))],
                Some(Type::Int),
            ),
        ));
        out.push(St::Store {
            place: Place::Name(i),
            value: Val::Name(t),
            old: Old::Nothing,
            line,
            site: Site::None,
            releases: false,
            holes: Vec::new(),
        });
    }

    fn projected_elem(&self, ity: &Type) -> Option<Type> {
        let key = vyrn_frontend::types::type_key(ity)?;
        let imp = self.program.impls.iter().find(|i| {
            vyrn_frontend::types::type_key(&i.ty).as_deref() == Some(key.as_str())
                && i.places.iter().any(|p| p.name == "nth")
        })?;
        let nth = imp.places.iter().find(|p| p.name == "nth")?;
        Some(self.under_impl(&nth.ret, ity))
    }

    /// `ty` as an impl's member declares it, under the type arguments of the
    /// receiver `recv`: `impl<T> .. for Slots<T>` against `Slots<Person>`
    /// makes T Person.
    fn under_impl(&self, ty: &Type, recv: &Type) -> Type {
        let key = vyrn_frontend::types::type_key(recv);
        let mut subst = HashMap::new();
        if let Some(imp) =
            (self.program.impls.iter()).find(|i| vyrn_frontend::types::type_key(&i.ty) == key)
        {
            vyrn_frontend::types::solve_param(&imp.ty, recv, &mut subst);
        }
        vyrn_frontend::types::substitute(ty, &subst)
    }

    /// Whether a projection answers for `ty`'s element place.
    fn projected(&self, ty: &Type) -> bool {
        vyrn_frontend::project::lookup_in(&self.program.impls, ty, "atSet").is_some()
    }

    /// The type arguments of a call to the impl function `f` on a receiver
    /// of type `recv`, in `f`'s order: its impl head's parameters solved
    /// against the receiver, as [`Builder::under_impl`] solves them. Empty
    /// for a function with none; `None` where the program declares no `f`
    /// or the receiver leaves a parameter unsolved.
    fn impl_args(&self, f: &str, recv: &Type) -> Option<Vec<(String, Type)>> {
        let g = self.program.functions.iter().find(|g| g.name == f)?;
        let key = vyrn_frontend::types::type_key(recv)?;
        let mut subst = HashMap::new();
        if let Some(imp) = self.program.impls.iter().find(|i| {
            vyrn_frontend::types::type_key(&i.ty).as_deref() == Some(key.as_str())
                && (i.methods.iter()).any(|m| {
                    vyrn_frontend::types::impl_method_name(&i.protocol, &key, &m.name) == f
                })
        }) {
            vyrn_frontend::types::solve_param(&imp.ty, recv, &mut subst);
        }
        (g.type_params.iter())
            .map(|p| Some((p.clone(), subst.get(p)?.clone())))
            .collect()
    }

    fn is_map(&self, ty: &Type) -> bool {
        let decls = self.proto.types();
        matches!(vyrn_frontend::types::resolve(ty, &decls), Type::Map(..))
    }

    fn field_ty(&self, ty: &Type, field: &str, line: usize) -> Result<Type, Gap> {
        let decls = self.proto.types();
        let rt = vyrn_frontend::types::resolve(ty, &decls);
        match rt {
            Type::Record(fields) => fields
                .iter()
                .find(|f| f.name == field)
                .map(|f| f.ty.clone())
                .ok_or(Gap {
                    what: "a field the record does not have",
                    detail: field.to_string(),
                    line,
                    rule: None,
                }),
            _ => gap("a field of a non-record", line),
        }
    }

    fn elem_ty(&self, ty: &Type, line: usize) -> Result<Type, Gap> {
        let decls = self.proto.types();
        match vyrn_frontend::types::resolve(ty, &decls) {
            Type::Array(e) | Type::ArrayN(e, _) | Type::SmallArray(e, _) | Type::Stream(e) => {
                Ok(*e)
            }
            // A `for` over a String yields each byte as an `Int64`, the
            // checker's type; `s[i]` is a `UInt8` and never reaches here.
            Type::Str => Ok(Type::Int),
            Type::Map(_, v) => Ok(*v),
            t => gap_d("an element of a non-container", &t.to_string(), line),
        }
    }

    /// Whether a construct owns the boxes its binders come out of
    /// ([`St::Switch`]'s `owns`): it took a named scrutinee, or it switches
    /// on a value the frame made. A declared release's receiver is excepted:
    /// its caller frees the payload boxes after the call.
    fn owns_boxes(&self, e: &'a Expr, consuming: bool) -> bool {
        let receiver = match e {
            Expr::Consume { place, .. } => match &**place {
                Expr::Var { name, .. } => Some(name),
                _ => None,
            },
            Expr::Var { name, .. } => Some(name),
            _ => None,
        };
        let released =
            receiver.is_some_and(|r| self.released.is_some() && self.lookup(r) == self.released);
        !released && (consuming || self.made_scrutinee(e))
    }

    /// Whether the frame made the value a construct switches on: anything but
    /// a name or a place. `m[k]` on a `Map` is a place: its `Option<V>`
    /// borrows the entry, so its binders borrow too (#463).
    fn made_scrutinee(&self, e: &'a Expr) -> bool {
        use vyrn_frontend::ast::place_path;
        use vyrn_frontend::project::element_path;
        place_path(e).is_none() && element_path(e).is_none()
    }

    /// The scrutinee of a `match`, `if let` or `?`: the value it switches on,
    /// and whether the construct consumed it. `lines` is the construct's first
    /// and last line ([`Builder::takes_scrutinee`]); `None` for an `if let`
    /// or a `?`, which read a named local.
    fn scrutinee(
        &mut self,
        e: &'a Expr,
        construct: usize,
        lines: Option<(usize, usize)>,
        out: &mut Vec<St>,
    ) -> Result<(Val, bool), Gap> {
        if !matches!(e, Expr::Var { .. }) && is_place_read(e) {
            // A field or an element: the construct borrows it, and its
            // binders borrow what they name.
            let v = self.read_val(e, out)?;
            return Ok((v, false));
        }
        match e {
            Expr::Var { name, .. } if self.lookup(name).is_some() => {
                let n = self.lookup(name).unwrap();
                if !self.takes_scrutinee(n, lines) {
                    return Ok((Val::Name(n), false));
                }
                // Recorded as a candidate; only a site [`last_owner`] seeded
                // takes it.
                self.body.cands.push((construct, n, Cand::Switch));
                if !self.seed.contains(&construct) {
                    return Ok((Val::Name(n), false));
                }
                let t = self.temp(self.body.names[n as usize].ty.clone(), e.line());
                out.push(St::Let(t, Rhs::Val(Val::Name(n))));
                self.keyed(t, construct);
                Ok((Val::Name(t), true))
            }
            Expr::Consume { place, line } => match &**place {
                Expr::Var { name, .. } => {
                    let Some(n) = self.lookup(name) else {
                        // `consume` of module state: the same read and refusal.
                        let v = self.global_read(place, name, *line, out)?;
                        if let Val::Name(t) = v {
                            self.keyed(t, construct);
                        }
                        return Ok((v, false));
                    };
                    let t = self.temp(self.body.names[n as usize].ty.clone(), *line);
                    out.push(St::Let(t, Rhs::Val(Val::Name(n))));
                    self.keyed(t, construct);
                    Ok((Val::Name(t), self.taken_by(t, construct)))
                }
                _ => {
                    let Val::Name(t) = self.take_prefix(place, *line, out)? else {
                        return gap("a `consume` of a literal", *line);
                    };
                    self.keyed(t, construct);
                    Ok((Val::Name(t), self.taken_by(t, construct)))
                }
            },
            _ => {
                let v = self.val(e, out)?;
                match v {
                    Val::Name(t) => {
                        self.keyed(t, construct);
                        Ok((Val::Name(t), self.taken_by(t, construct)))
                    }
                    Val::Lit(l) => Ok((Val::Lit(l), false)),
                }
            }
        }
    }

    /// Whether this construct is a candidate to take its named scrutinee:
    /// every owned, heap-owning named scrutinee of a `match` is, and
    /// [`last_owner`] decides which construct is its last owner. `lines` is
    /// `None` for an `if let` or a `?`, which cannot hand a payload out.
    ///
    /// Too wide an answer is a refusal, never a double free: the take is
    /// stated in the core, so the kernel refuses a later read.
    fn takes_scrutinee(&self, n: Name, lines: Option<(usize, usize)>) -> bool {
        let info = &self.body.names[n as usize];
        lines.is_some() && info.releases && info.heap && !info.borrow && !self.reading.contains(&n)
    }

    /// Whether the construct took the temporary `t` it owns: the payloads
    /// moved into the arms' binders and the boxes were freed there. Where it
    /// did not, the binders borrowed and the value is released whole. Records
    /// the candidate; only a site [`last_owner`] seeded takes.
    fn taken_by(&mut self, t: Name, construct: usize) -> bool {
        if !self.body.names[t as usize].releases {
            return false;
        }
        self.body.cands.push((construct, t, Cand::Switch));
        self.seed.contains(&construct)
    }

    /// [`Builder::taken_by`] at a `for`: whether the loop is the last owner of
    /// its container, so it releases it where it ends ([`Cand::Loop`]).
    fn taken_by_loop(&mut self, it: Name, sid: usize) -> bool {
        self.body.cands.push((sid, it, Cand::Loop));
        self.seed.contains(&sid)
    }

    /// Which tag a pattern tests for, in the scrutinee's variant list. `??`'s
    /// pair names a tag: 1 succeeds and 0 fails, for every sum.
    fn arm_test(&self, p: &Pattern, sty: &Type, line: usize) -> Result<Test, Gap> {
        let Pattern::Variant(v, _) = p else {
            return Ok(match p {
                Pattern::Other => Test::Else,
                _ => Test::Tag(u64::from(matches!(p, Pattern::Success(_)))),
            });
        };
        let decls = self.proto.types();
        let Type::Enum(variants) = vyrn_frontend::types::resolve(sty, &decls) else {
            return gap("a variant pattern on a non-enum", line);
        };
        match variants.iter().position(|x| x.name == *v) {
            Some(at) => Ok(Test::Tag(at as u64)),
            None => gap("a variant the enum does not have", line),
        }
    }

    /// Refuses a `match` whose arms do not take each variant of `sty` once:
    /// a tag twice, or, with no default arm, a variant no arm takes. A
    /// `match` with no arm also gets a `trap` ahead of the switch, so the
    /// kernel never joins a switch no edge leaves.
    fn covers(
        &mut self,
        arms: &[Arm],
        sty: &Type,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        let decls = self.proto.types();
        let Type::Enum(variants) = vyrn_frontend::types::resolve(sty, &decls) else {
            return gap("a variant pattern on a non-enum", line);
        };
        if arms.is_empty() {
            out.push(St::Trap);
        }
        let mut taken = vec![false; variants.len()];
        for a in arms {
            if let Test::Tag(t) = a.test {
                let Some(seen) = taken.get_mut(t as usize) else {
                    return gap("a variant the enum does not have", line);
                };
                if *seen {
                    let v = &variants[t as usize].name;
                    self.body
                        .refused
                        .push((line, format!("duplicate `{v}` arm")));
                    return Ok(());
                }
                *seen = true;
            }
        }
        if arms.iter().any(|a| a.test == Test::Else) {
            return Ok(());
        }
        if let Some((v, _)) = variants.iter().zip(&taken).find(|(_, t)| !**t) {
            let refusal = format!("`match` is missing variant `{}`", v.name);
            self.body.refused.push((line, refusal));
        }
        Ok(())
    }

    /// Binds a pattern's names: owned binders when the match consumed its
    /// scrutinee, borrowed places otherwise. `from` is the scrutinee's name
    /// where the construct did not consume it: a binder over a borrow carries
    /// the borrow's kind, so `match o { Some(v) => take(v) }` over a `read`
    /// parameter is refused.
    fn bind_pattern(
        &mut self,
        p: &Pattern,
        sty: &Type,
        consuming: bool,
        line: usize,
        from: Option<Name>,
        out: &mut Vec<St>,
    ) -> Result<Vec<Name>, Gap> {
        let decls = self.proto.types();
        let rt = vyrn_frontend::types::resolve(sty, &decls);
        // Each binder's key is the address of the name the reader wrote: one
        // per binder, the same on every build.
        let (payloads, variant): (Vec<(String, Type, usize)>, String) = match p {
            Pattern::Other => (Vec::new(), String::new()),
            // `??`'s pair names a tag: variant 1 succeeds, 0 fails.
            Pattern::Success(n) | Pattern::Failure(n) => match &rt {
                Type::Enum(vs) if vs.len() == 2 => {
                    let at = usize::from(matches!(p, Pattern::Success(_)));
                    let ps = vs[at]
                        .payload
                        .first()
                        .map(|t| {
                            vec![(
                                n.name.clone(),
                                t.clone(),
                                vyrn_frontend::own::binder_key(&n.name),
                            )]
                        })
                        .unwrap_or_default();
                    (ps, vs[at].name.clone())
                }
                _ => return gap("a `??` pattern on a scrutinee with no two tags", line),
            },
            Pattern::Variant(v, names) => match &rt {
                Type::Enum(variants) => {
                    let Some(var) = variants.iter().find(|x| x.name == *v) else {
                        return gap("a variant the enum does not have", line);
                    };
                    if var.payload.len() != names.len() {
                        return gap("a variant pattern with the wrong arity", line);
                    }
                    let ps = names
                        .iter()
                        .zip(var.payload.iter().cloned())
                        .map(|(n, t)| (n.name.clone(), t, vyrn_frontend::own::binder_key(&n.name)))
                        .collect();
                    (ps, var.name.clone())
                }
                _ => return gap("a variant pattern on a non-enum", line),
            },
        };
        let mut binds = Vec::new();
        for (i, (name, ty, key)) in payloads.into_iter().enumerate() {
            let owned = consuming && self.owns(&ty);
            let layout = matches!(
                vyrn_frontend::types::resolve(&ty, &decls),
                Type::Record(_)
                    | Type::Enum(_)
                    | Type::Array(_)
                    | Type::ArrayN(..)
                    | Type::SmallArray(..)
                    | Type::Map(..)
            );
            let n = self.name(&name, ty, owned, line);
            // The placer keys a binder's exit rows (a `return`, `break` or
            // `continue` inside the arm) by this address. Keyed owned or not:
            // the first build takes nothing, so the second build's rows need
            // a name to land on; [`Builder::drops_at`] skips a borrowed one.
            self.keyed(n, key);
            if !owned {
                if let Some(m) = from {
                    if let Some(k) = self.body.names[m as usize].borrow_kind.clone() {
                        self.body.names[n as usize].borrow_kind = Some(k);
                        self.body.names[n as usize].must_use_param =
                            self.body.names[m as usize].must_use_param;
                    }
                    // A scrutinee that is a place read has no kind to pass
                    // on, so the binder is `Place` (`return match d.tag {
                    // Word(s) => s, .. }`). An owned scrutinee's payloads are
                    // the frame's to give.
                    if self.body.names[m as usize].borrow
                        && self.body.names[n as usize].borrow_kind.is_none()
                    {
                        self.body.names[n as usize].borrow_kind = Some(BorrowKind::Place);
                    }
                    // A binder of a layout or a heap value reads the scrutinee
                    // for the arm's extent ([`Arm::reads`]), so the kernel
                    // refuses a write to it meanwhile.
                    if layout || self.body.names[n as usize].borrow {
                        // The walk skips a payload hole on its live tag, and
                        // only a declared `release` cannot skip one
                        // ([`vyrn_frontend::declared::skippable`]).
                        let at = format!("{variant}.{i}");
                        self.body.names[n as usize].payload = Some(
                            if vyrn_frontend::declared::skippable(
                                &self.own.proto,
                                sty,
                                std::slice::from_ref(&at),
                            ) {
                                Payload::Hole(format!(".{at}"))
                            } else {
                                Payload::Sealed(sty.to_string())
                            },
                        );
                        out.push(St::Let(n, Rhs::Read(Place::Name(m))));
                    }
                }
            }
            // `_` never enters the scope, but its payload is real and a
            // consumed scrutinee's arm still owes its release.
            if name != "_" {
                self.scope.push((name, n));
            }
            binds.push(n);
        }
        Ok(binds)
    }

    /// An expression in a read position: an operand, a condition, an index, a
    /// `read` argument. A heap place is borrowed, not moved. A String
    /// temporary (`@str`, `@concat`, a string `+`) is freed by the reading
    /// site, so its drop is queued after the consuming binding.
    fn read_val(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        self.read_at(e, out, None)
    }

    /// A read in an argument position, with the position it fills. An
    /// operator is a call (`a + b` is `@concat(a, b)`), so its operands come
    /// through here too.
    fn read_arg(
        &mut self,
        e: &'a Expr,
        out: &mut Vec<St>,
        callee: &str,
        ix: usize,
    ) -> Result<Val, Gap> {
        self.read_at(e, out, Some((callee, ix)))
    }

    fn read_at(
        &mut self,
        e: &'a Expr,
        out: &mut Vec<St>,
        at: Option<(&str, usize)>,
    ) -> Result<Val, Gap> {
        let v = self.read_val_inner(e, out)?;
        // The argument-drop key, here rather than in `call`, which sees
        // neither an operator nor a `lazy` field read.
        if let (Val::Name(t), Some((callee, ix))) = (&v, at) {
            let t = *t;
            if self.arg_released(e, t, callee, ix) {
                self.body.names[t as usize].arg_drop = Some(e as *const Expr as usize);
            }
        }
        Ok(v)
    }

    /// Whether a store whose value mentions the place it writes into hands
    /// nothing back, because every mention is a read the value cannot hand
    /// back. A function whose result is not spelled `read`/`modify` returns
    /// an owned value; the kernel refuses returning a borrow of a parameter.
    fn store_is_fresh(&self, value: &'a Expr, name: &str) -> bool {
        // A heapless value has nothing to hand back.
        if !self.ty_of(value).is_ok_and(|t| self.proto.owns_heap(&t)) {
            return false;
        }
        let mut ms = Vec::new();
        self.read_only_mentions(value, name, &mut ms)
    }

    /// Whether every mention of `root` in `e` is a read that cannot hand
    /// `root`'s own storage back.
    fn read_only_mentions(&self, e: &Expr, root: &str, out: &mut Vec<String>) -> bool {
        use vyrn_frontend::movecheck as mc;
        if !vyrn_frontend::ast::mentions_place(e, root) {
            return true;
        }
        // A heapless mention hands nothing back (`Frag { start: f.start }`).
        if self.ty_of(e).is_ok_and(|t| !self.proto.owns_heap(&t)) {
            return true;
        }
        match e {
            // A call that forwards none of its arguments builds its result
            // afresh (`xs[j].copy()`), whatever it reads.
            Expr::Call { name, .. } if !mc::call_may_forward(name) => true,
            Expr::Call { name, args, .. } => args.iter().all(|a| {
                let is_root_read = match a {
                    Expr::Var { name: v, .. } => v == root,
                    _ => vyrn_frontend::ast::place_path(a).is_some_and(|(r, _)| r == root),
                };
                if !is_root_read {
                    return self.read_only_mentions(a, root, out);
                }
                if !mc::call_may_forward(name) {
                    true
                } else if self.declares(name)
                    && !name.starts_with('@')
                    && prelude::signature(name).is_none()
                {
                    out.push(name.clone());
                    true
                } else {
                    false
                }
            }),
            Expr::Binary { lhs, rhs, .. } => {
                self.read_only_mentions(lhs, root, out) && self.read_only_mentions(rhs, root, out)
            }
            Expr::Unary { expr, .. } => self.read_only_mentions(expr, root, out),
            Expr::StructLit { fields, .. } => fields
                .iter()
                .all(|(_, v)| self.read_only_mentions(v, root, out)),
            Expr::ArrayLit { elems, .. } => {
                elems.iter().all(|v| self.read_only_mentions(v, root, out))
            }
            _ => false,
        }
    }

    /// Whether the program declares a callable of this name: a function, a
    /// method, a projection, or a seeded builtin.
    fn declares(&self, name: &str) -> bool {
        self.program.functions.iter().any(|f| f.name == name)
            || self
                .program
                .impls
                .iter()
                .any(|i| i.methods.iter().any(|m| m.name == name))
            || self.projection(name).is_some()
            || prelude::signature(name).is_some()
    }

    /// Whether the temporary this frame minted for an argument position is
    /// the caller's to release after the call. It must be an allocation
    /// ([`NameInfo::releases`], or a forced `lazy` field), and
    /// [`vyrn_frontend::movecheck::arg_verdict`] decides what the callee does
    /// with it.
    fn arg_released(&self, e: &'a Expr, t: Name, callee: &str, ix: usize) -> bool {
        use vyrn_frontend::movecheck as mc;
        // A named value is nobody's temporary: `f(s)` hands over what `s`
        // owns, and the binding keeps the row.
        if matches!(e, Expr::Var { .. } | Expr::Consume { .. }) {
            return false;
        }
        // A forced `lazy` field read is a call returning a fresh owned value.
        // This pass binds a borrow for it because the read names
        // a place, but the caller still frees the value.
        let info = &self.body.names[t as usize];
        let forced = self.forces_a_thunk(e);
        if !info.releases && !forced {
            return false;
        }
        // `blackBox` of a borrow built nothing to free; of an owned
        // temporary, it took it, and its result frees it.
        if let Expr::Call { name, args, .. } = e {
            if self.hands_back_a_borrow(name, args) {
                return false;
            }
        }
        let ty = if forced {
            match self.forced_ty(e) {
                Some(t) => t,
                None => return false,
            }
        } else {
            info.ty.clone()
        };
        let Some(kind) = self.proto.release_kind(&ty) else {
            return false;
        };
        // The producer as `arg_verdict` partitions it: a call's name, `None`
        // for the allocating operator, else a name no user function can have.
        let producer = match e {
            Expr::Call { name, .. } => Some(name.clone()),
            Expr::Binary { op: BinOp::Add, .. } => None,
            Expr::Match { .. } => Some("@match".to_string()),
            Expr::StructLit { .. } => Some("@record".to_string()),
            Expr::ArrayLit { .. } => Some("@heapify".to_string()),
            Expr::Field { .. } => Some("@lazy".to_string()),
            _ => Some("@build".to_string()),
        };
        // A view LENDS, unless the element it hands out is a heap-free copy.
        let decls = self.proto.types();
        let view_copies = mc::lends_result(callee)
            && matches!(
                vyrn_frontend::types::resolve(&ty, &decls),
                Type::Array(ref et)
                    | Type::ArrayN(ref et, _)
                    | Type::SmallArray(ref et, _)
                    | Type::Stream(ref et) if !self.proto.owns_heap(et)
            );
        let s = mc::ArgTemp {
            id: e as *const Expr as usize,
            callee: callee.to_string(),
            ix,
            line: e.line(),
            module: self.body.file.clone(),
            producer,
            kind,
            verdict: mc::ArgVerdict::Unknown,
            view_copies,
        };
        let constructs = matches!(callee, "Some" | "Ok" | "Err" | "Success" | "Failure")
            || self.is_variant(callee);
        let cap = vyrn_frontend::declared::arg_cap(&self.own.arg_caps, callee, ix);
        if mc::arg_verdict(&s, constructs, cap) == mc::ArgVerdict::Released {
            return true;
        }
        // A call through a fn value has no capability row; the answer is the
        // meet over the signature's closed target set
        // ([`vyrn_frontend::movecheck::Facts::fnval_clear`]), as in
        // `examples/rpc.vyrn`'s `cb(Done(..))`.
        self.fnval_released(callee)
    }

    /// Whether `callee` names a fn value whose signature the meet cleared.
    fn fnval_released(&self, callee: &str) -> bool {
        use vyrn_frontend::movecheck as mc;
        let Some(n) = self.lookup(callee) else {
            return false;
        };
        let decls = self.proto.types();
        let Type::Fn(ps, r) =
            vyrn_frontend::types::resolve(&self.body.names[n as usize].ty, &decls)
        else {
            return false;
        };
        self.own
            .fnval_clear
            .contains(&mc::fn_sig_key(&ps, &r, &decls))
    }

    /// The type a forced `lazy` field read yields, where it owns heap.
    fn forced_ty(&self, e: &Expr) -> Option<Type> {
        self.deferred_of(e).filter(|t| self.proto.owns_heap(t))
    }

    /// The `T` of a read of a `lazy T` field.
    fn deferred_of(&self, e: &Expr) -> Option<Type> {
        let Expr::Field {
            expr: base, field, ..
        } = e
        else {
            return None;
        };
        let decls = self.proto.types();
        let bt = self.ty_of(base).ok()?;
        let Type::Record(fields) = vyrn_frontend::types::resolve(&bt, &decls) else {
            return None;
        };
        let f = fields.iter().find(|f| &f.name == field)?;
        vyrn_frontend::types::deferred(&f.ty).cloned()
    }

    /// Forces a read of a `lazy T` field: borrows the stored
    /// nullary closure out of the field and calls through it. The result is
    /// fresh on every read; its release is keyed by the read (`arg_released`).
    fn force(&mut self, e: &'a Expr, inner: Type, out: &mut Vec<St>) -> Result<Rhs, Gap> {
        let place = self.place(e, out)?;
        let thunk = Type::Fn(Vec::new(), Box::new(inner.clone()));
        let n = self.name("@thunk", thunk, false, e.line());
        let callee = format!("@thunk{n}");
        self.body.names[n as usize].source = callee.clone();
        self.body.names[n as usize].path = reader_path(e);
        out.push(St::Let(n, Rhs::Read(place)));
        self.release_receiver(e, out, true);
        Ok(Rhs::Call {
            callee,
            args: Vec::new(),
            write_back: false,
            kind: Callee::Value(n),
            ret: Some(inner),
            solved: Vec::new(),
            targets: Vec::new(),
        })
    }

    fn forces_a_thunk(&self, e: &Expr) -> bool {
        self.forced_ty(e).is_some()
    }

    fn read_val_inner(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        let ty = self.ty_of(e).ok();
        let owns = ty.as_ref().is_some_and(|t| self.owns(t));
        match e {
            Expr::Field { .. } if owns && self.deferred_of(e).is_none() => {
                let place = self.place(e, out)?;
                let t = self.borrow_name(e, ty.unwrap(), e.line());
                out.push(St::Let(t, Rhs::Read(place)));
                self.release_receiver(e, out, true);
                Ok(Val::Name(t))
            }
            Expr::Call { name, args, .. } if owns && name == "@at" && args.len() == 2 => {
                let place = self.place(e, out)?;
                let t = self.borrow_name(e, ty.unwrap(), e.line());
                out.push(St::Let(t, Rhs::Read(place)));
                self.release_receiver(e, out, true);
                Ok(Val::Name(t))
            }
            Expr::Var { .. } | Expr::Consume { .. } => self.val(e, out),
            Expr::Lambda { .. } => self.lambda(e, out),
            _ if owns => {
                let v = self.val(e, out)?;
                if let Val::Name(t) = v {
                    if self.body.names[t as usize].releases && !self.after.contains(&t) {
                        self.after.push(t);
                    }
                }
                Ok(v)
            }
            _ => self.val(e, out),
        }
    }

    /// Frees the unnamed receiver of a field or element read after the read,
    /// where this frame owns it: a receiver is pending only where
    /// [`Builder::place`] minted an owned name for it. The hole is the field
    /// the read took; a scalar read leaves none.
    ///
    /// `borrowed` says the consumer borrows a heap value out of the receiver
    /// (`f(x).rhs.startsWith("{")`), so the receiver must outlive the
    /// consumer: its free is an argument-temporary drop keyed by the
    /// producing node, which the backends free after the consuming call or
    /// operator. Outside such a drain the receiver stays held and the
    /// judgment refuses it.
    fn release_receiver(&mut self, e: &'a Expr, out: &mut Vec<St>, borrowed: bool) {
        let Some((r, producer, malloc)) = self.pending_receiver.take() else {
            return;
        };
        let node = e as *const Expr as usize;
        let took = self.ty_of(e).is_ok_and(|t| self.owns(&t));
        if borrowed && took {
            if placed_producer(producer) {
                if !self.after.contains(&r) {
                    self.body.names[r as usize].arg_drop = Some(producer);
                    self.after.push(r);
                }
            } else if self.drain > 0 {
                self.body.names[r as usize].producer = Some(producer);
            }
            return;
        }
        let _ = node;
        // An element's receiver is `@at`'s argument, and its release is keyed
        // as an argument temporary's.
        if let (false, Expr::Call { name, args, .. }) = (took, e) {
            if self.arg_released(&args[0], r, name, 0) {
                self.body.names[r as usize].arg_drop = Some(producer);
            }
        }
        // A heap value taken out leaves a hole the release walks around. An
        // element cannot be skipped, so its receiver stays held and the
        // kernel reports it.
        let holes: Vec<String> = match (took, e) {
            (false, _) => Vec::new(),
            (true, Expr::Field { field, .. }) => vec![format!(".{field}")],
            (true, _) => return,
        };
        self.body.names[r as usize].holes = holes;
        self.body.names[r as usize].receiver_malloc = malloc;
        out.push(St::Drop(r, Site::None, 0, None));
    }

    /// Records [`NameInfo::fields`] on the name a record literal is bound to.
    /// Called at both binding sites: a reader's `let` and an inline
    /// literal's temporary.
    fn record_fields(&mut self, n: Name, value: &Expr) {
        if let Expr::StructLit {
            name: t, fields, ..
        } = value
        {
            self.body.names[n as usize].fields = fields
                .iter()
                .map(|(f, _)| format!("the field `{t}.{f}`"))
                .collect();
        }
    }

    /// An expression in a take position: a `let`, a `return`, a store, a part
    /// of a literal, a `consume` argument.
    fn val(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        // The rebind flag covers this expression only: in
        // `n = n + size(if c { names } else { .. })` the join still owns.
        let rebinding = std::mem::take(&mut self.rebinding);
        if let Some(l) = lit_of(e).or_else(|| self.schema(e)) {
            return Ok(Val::Lit(l));
        }
        match e {
            Expr::Var { name, line } => match self.lookup(name) {
                Some(n) => Ok(Val::Name(n)),
                // A nullary constructor (`None`, a fieldless variant) parses
                // as a bare name. Like a literal, it owns and borrows nothing.
                None if self.is_nullary(name) => {
                    let ty = self.ty_of(e)?;
                    self.nullary(name, ty, *line, out)
                }
                // A function's name stored as a value: the closure enum's
                // variant for it, which captures and owns nothing.
                None if self
                    .program
                    .functions
                    .iter()
                    .any(|f| &f.name == name && f.type_params.is_empty())
                    && self
                        .types
                        .get(&(e as *const Expr as usize))
                        .is_some_and(|t| {
                            matches!(
                                vyrn_frontend::types::resolve(t, self.proto.types()),
                                Type::Fn(..)
                            )
                        }) =>
                {
                    let ty = self.types[&(e as *const Expr as usize)].clone();
                    let f = self.program.functions.iter().find(|f| &f.name == name);
                    let decls = self.proto.types();
                    let refusal = stored_slot(None, f, &ty, &decls, *line);
                    self.body.mistyped.extend(refusal);
                    let t = self.name("@closure", ty, false, *line);
                    self.body.names[t as usize].borrow = false;
                    self.body.names[t as usize].not_owned = Some(NotOwned::Static);
                    let made = Ctor::Closure(Target::Fn(name.clone()));
                    out.push(St::Let(t, Rhs::Make(made, Vec::new())));
                    Ok(Val::Name(t))
                }
                // A function's name as a value (`sortWith(es, byCount)`), or
                // a type's as an argument (`fromJson(Bag, src)`): static, and
                // the checker types neither as an expression.
                None if self.program.functions.iter().any(|f| &f.name == name)
                    || self.program.contracts.iter().any(|c| &c.name == name)
                    || self.proto.types().contains_key(name) =>
                {
                    Ok(Val::Lit(Lit::Opaque(Opaque::Static)))
                }
                // Nothing may take module state, so a read of it
                // is a borrow in any position.
                None => self.global_read(e, name, *line, out),
            },
            Expr::Consume { place, line } => match &**place {
                Expr::Var { name, .. } => match self.lookup(name) {
                    Some(n) => Ok(Val::Name(n)),
                    // `consume <module state>`: a borrow whose take the
                    // kernel refuses.
                    None => self.global_read(place, name, *line, out),
                },
                _ => self.take_prefix(place, *line, out),
            },
            Expr::Lambda { .. } => self.lambda(e, out),
            _ => {
                let ty = self.ty_of(e)?;
                if is_place_read(e) && self.owns(&ty) && self.deferred_of(e).is_none() {
                    // `best = m.name`: a borrow (`movecheck::names_a_place`),
                    // so a take of it needs a `.copy()`.
                    let place = self.place(e, out)?;
                    let t = self.borrow_name(e, ty, e.line());
                    out.push(St::Let(t, Rhs::Read(place)));
                    self.release_receiver(e, out, true);
                    return Ok(Val::Name(t));
                }
                let rhs = self.rhs(e, out)?;
                // A `panic` leaves no value to name: every row that reads
                // this one follows the `trap`, and [`cut`] drops it.
                if let Rhs::Val(v @ Val::Lit(Lit::Opaque(Opaque::Trapped))) = rhs {
                    return Ok(v);
                }
                // An `if` or `match` expression whose arm yields a borrow
                // yields a borrow (`movecheck::names_a_place`).
                let borrows = matches!(&rhs, Rhs::Val(v) if self.borrows(v));
                let t = if self.lends(e) || borrows {
                    self.borrow_name(e, ty, e.line())
                } else {
                    // `size(if c { names } else { [..] })`: the temporary owns
                    // the result as a `let` would ([`Builder::loop_alias`]).
                    if !rebinding && self.owns(&ty) {
                        self.loop_alias(&rhs, e.line())?;
                    }
                    self.temp(ty, e.line())
                };
                self.record_fields(t, e);
                self.bind(t, rhs, out);
                if reads_a_part(e) {
                    self.release_receiver(e, out, false);
                }
                Ok(Val::Name(t))
            }
        }
    }

    /// The nullary constructor `name` of `ty`, bound to a temporary.
    fn nullary(
        &mut self,
        name: &str,
        ty: Type,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Val, Gap> {
        let rhs = self.call(name, &[], line, Some(ty.clone()), out)?;
        let t = self.name("@nullary", ty, false, line);
        self.body.names[t as usize].borrow = false;
        self.body.names[t as usize].not_owned = Some(NotOwned::Static);
        out.push(St::Let(t, rhs));
        Ok(Val::Name(t))
    }

    /// A lambda literal as a closure value. Captures are reads of
    /// the enclosing names, which the enclosing frame still owns; a stored
    /// closure snapshots them and owns its snapshot. A literal a
    /// call's target names is monomorphized away ([`Builder::targets_of`]).
    /// The body is its own frame ([`Builder::lambda_frame`]).
    fn lambda(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        let caps = self.captures(e);
        let ty = self.ty_of(e).unwrap_or(Type::Unit);
        let t = self.name("@lambda", ty.clone(), self.owns(&ty), e.line());
        // Taken from the cell, so a nested or sibling lambda asks its own
        // position ([`NameInfo::closure_reads`]).
        self.body.names[t as usize].closure_reads = self.closure_reads(e, &caps);
        let key = self.lambda_frame(e, &caps)?;
        out.push(St::Let(t, Rhs::Prim(Op::Closure(key), caps, Some(ty))));
        Ok(Val::Name(t))
    }

    /// Builds the lambda's own frame, judged like a function's, and answers
    /// its key. Captures are borrowed inputs and parameters are `read`.
    /// The plan keys its bindings' rows by the lambda's nodes
    /// under the enclosing function's name. An expression body is a `return`
    /// at no site, so a name still held there is refused, not placed.
    fn lambda_frame(&mut self, e: &'a Expr, caps: &[Val]) -> Result<String, Gap> {
        let Expr::Lambda {
            params,
            body,
            line,
            col,
        } = e
        else {
            return gap("a lambda frame of no lambda literal", e.line());
        };
        let decls = self.proto.types();
        let (ptys, ret): (Vec<Type>, Option<Type>) = match self.ty_of(e).ok() {
            // A `lazy T` field's initializer is a nullary closure.
            Some(t) if vyrn_frontend::types::deferred(&t).is_some() => {
                (Vec::new(), vyrn_frontend::types::deferred(&t).cloned())
            }
            Some(t) => match vyrn_frontend::types::resolve(&t, &decls) {
                Type::Fn(ptys, r) => {
                    // A body its slot does not take. [`fn_slot`] covers a
                    // call's parameter; this covers every other slot.
                    if let LambdaBody::Expr(x) = body {
                        let got = self.ty_of(x).ok().filter(|g| *g != Type::Err);
                        let refusal =
                            got.and_then(|g| stored_slot(Some(&g), None, &t, &decls, *line));
                        self.body.mistyped.extend(refusal);
                    }
                    (ptys, Some(*r))
                }
                _ => return gap("a lambda the checker did not type as a function", *line),
            },
            // An untyped literal, an argument of a monomorphized generic: each
            // parameter takes the type of its first use in the typed body.
            None => {
                let vars = mentions_in_lambda(body);
                let ptys = params
                    .iter()
                    .map(|p| {
                        vars.iter()
                            .find(|v| matches!(v, Expr::Var { name, .. } if *name == p.name))
                            .and_then(|v| self.types.get(&(*v as *const Expr as usize)))
                            .cloned()
                            .unwrap_or(Type::Unit)
                    })
                    .collect();
                (ptys, None)
            }
        };
        if ptys.len() != params.len() {
            return gap("a lambda with the wrong arity for its type", *line);
        }
        let file = self.body.file.clone();
        let export = self.body.export;
        let outer = std::mem::replace(
            &mut self.body,
            Body {
                name: String::new(),
                file,
                export,
                names: Vec::new(),
                params: Vec::new(),
                stmts: Vec::new(),
                lambdas: Vec::new(),
                cands: Vec::new(),
                loop_buffers: Vec::new(),
                unbound_drops: Vec::new(),
                refused: Vec::new(),
                mistyped: Vec::new(),
            },
        );
        self.body.name = lambda_spelling(&outer.name, *line, *col);
        let saved = (
            std::mem::take(&mut self.scope),
            std::mem::take(&mut self.by_binding),
            std::mem::take(&mut self.after),
            std::mem::take(&mut self.after_of_rhs),
            self.pending_receiver.take(),
            std::mem::replace(&mut self.drain, 0),
            std::mem::take(&mut self.stream_loops),
            std::mem::take(&mut self.walks),
        );
        let outer_ret = std::mem::replace(&mut self.ret, ret);
        for c in caps {
            let Val::Name(n) = c else {
                continue;
            };
            let info = &outer.names[*n as usize];
            let (source, ty) = (info.source.clone(), info.ty.clone());
            let m = self.name(&source, ty, false, *line);
            // A capture is the enclosing frame's; the kernel refuses a take.
            self.body.names[m as usize].borrow_kind = Some(BorrowKind::Capture);
            self.scope.push((source, m));
            self.body.params.push(m);
        }
        for (p, pt) in params.iter().zip(ptys) {
            let m = self.name(&p.name, pt, false, *line);
            self.scope.push((p.name.clone(), m));
            self.body.params.push(m);
        }
        let mut stmts = Vec::new();
        let r = match body {
            LambdaBody::Block(b) => self.block(b, &mut stmts),
            LambdaBody::Expr(x) => self.val(x, &mut stmts).map(|v| {
                stmts.push(St::Return {
                    value: Some(v),
                    site: 0,
                    is_try: false,
                    line: *line,
                })
            }),
        };
        cut(&mut stmts);
        self.body.stmts = stmts;
        if let (Some(owes), LambdaBody::Block(_)) = (self.ret.clone(), body) {
            falls_through(&mut self.body, &owes, *line, || "this lambda".to_string());
        }
        let frame = std::mem::replace(&mut self.body, outer);
        (
            self.scope,
            self.by_binding,
            self.after,
            self.after_of_rhs,
            self.pending_receiver,
            self.drain,
            self.stream_loops,
            self.walks,
        ) = saved;
        self.ret = outer_ret;
        r?;
        let key = frame.name.clone();
        self.body.lambdas.push(frame);
        Ok(key)
    }

    /// The names of this body a lambda mentions, as a place or as a callee
    /// (`n -> f(n) + 1` captures `f`), in [`lambda_captures`]'s order, each
    /// resolved by [`Builder::lookup`], not a shadowed one (#483). Not
    /// `ast::mentions_place`: it answers `true` for every name in a block
    /// body, and a capture is a read.
    fn captures(&self, e: &Expr) -> Vec<Val> {
        let Expr::Lambda { params, body, .. } = e else {
            return Vec::new();
        };
        let locals = params.iter().map(|p| p.name.clone()).collect();
        lambda_captures(body, locals, &|n| self.lookup(n).is_some())
            .iter()
            .filter_map(|n| self.lookup(n).map(Val::Name))
            .collect()
    }

    /// [`NameInfo::closure_reads`] for one lambda literal.
    fn closure_reads(&mut self, e: &Expr, caps: &[Val]) -> Option<Vec<Name>> {
        if self.call_keeps.take() == Some(false) {
            return None;
        }
        let Expr::Lambda { body, .. } = e else {
            return None;
        };
        // Mentions, not captures: a captured callee is no value the closure
        // holds.
        let vars = mentions_in_lambda(body);
        Some(
            caps.iter()
                .filter_map(|v| match v {
                    Val::Name(n) => Some(*n),
                    Val::Lit(_) => None,
                })
                .filter(|n| reads_place(&vars, &self.body.names[*n as usize].source))
                .collect(),
        )
    }

    /// A read of module state as a value: a borrow of the global, which no
    /// frame may take. `e` is the expression that has the type.
    fn global_read(
        &mut self,
        e: &'a Expr,
        name: &str,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Val, Gap> {
        self.unknown_at(e);
        let ty = self.ty_of(e)?;
        let t = if self.owns(&ty) {
            self.borrow_name(e, ty, line)
        } else {
            self.temp(ty, line)
        };
        out.push(St::Let(t, Rhs::Read(Place::Global(name.to_string()))));
        Ok(Val::Name(t))
    }

    /// The `consume p` prefix. Its refusals are about the keyword,
    /// which the kernel does not see, so they are stated here.
    fn take_prefix(&mut self, e: &'a Expr, line: usize, out: &mut Vec<St>) -> Result<Val, Gap> {
        take_names_a_place(e, line, false)?;
        self.consume_names_a_borrow(e, line)?;
        if self.in_module_state(e) {
            return self.read_val(e, out);
        }
        self.take_place_at(e, out, true)
    }

    /// Whether `e` is module state or a place inside it. Nothing may take module
    /// state, so `consume g.f` reads the place, as `consume g` does, and the
    /// kernel refuses the read where an owner receives it. A take would leave
    /// a hole that the audited teardown frees again (#469).
    fn in_module_state(&self, e: &Expr) -> bool {
        vyrn_frontend::ast::place_path(e).is_some_and(|(root, _)| {
            self.lookup(&root).is_none() && self.program.globals.iter().any(|g| g.name == root)
        })
    }

    /// Refuses a prefix `consume` of a borrow (a `read` or `modify` parameter,
    /// a capture), which would hand somebody else's buffer away. Stated at
    /// the keyword: the write-back of `s.dense.push(i)` reaches
    /// [`Builder::take_place`] with no `consume` and changes no owner. The
    /// refusal names the root, as `movecheck::check_take` does; a heapless
    /// root is no borrow.
    fn consume_names_a_borrow(&self, e: &'a Expr, line: usize) -> Result<(), Gap> {
        let Some((root, path)) = vyrn_frontend::ast::place_path(e) else {
            return Ok(());
        };
        let Some(n) = self.lookup(&root) else {
            return Ok(());
        };
        let info = &self.body.names[n as usize];
        match &info.borrow_kind {
            // The sentence names the root; the fixes name the path.
            Some(k) if info.borrow && !info.must_use_param => {
                let mut msg = format!("`{root}` may not be consumed — it is {}", k.what(&root));
                for f in k.fixes(&path) {
                    msg.push_str(&format!("\n  fix: {f}"));
                }
                refuse(msg, line)
            }
            _ => Ok(()),
        }
    }

    /// Answers a join arm's yield of a name bound outside the construct
    /// (`let rel = if p == "" { st } else { p + "/" + st }`) and outside the
    /// enclosing loop. The yield moves the name, and the kernel releases it
    /// on the edges that do not; inside a loop the move would repeat every
    /// turn, and the `let` that owns the result refuses
    /// ([`Builder::loop_alias`]). A rebind hands the name back and is not
    /// refused.
    ///
    /// `mark` is the name count before the arms were lowered, which tells an
    /// outside name from a payload binder minted inside the arm.
    fn alias_out(&self, v: &Val, mark: usize) -> Option<String> {
        let Val::Name(m) = v else { return None };
        let m = *m as usize;
        // Only an owning name can be freed twice. A loop variable is minted
        // above the loop mark, so each turn's element is its own.
        (m < mark
            && self.body.names[m].releases
            && self.loop_marks.last().is_some_and(|lm| m < *lm))
        .then(|| self.body.names[m].source.clone())
    }

    /// A move out of a sub-place: `consume x.f`, or the receiver a rebuilding
    /// builtin hands back (`s.dense.push(i)` is `s.dense = @push(s.dense, i)`).
    /// The value leaves into an owned name and the base keeps a hole.
    fn take_place(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        self.take_place_at(e, out, false)
    }

    /// [`Builder::take_place`], saying whether the hole stays: a `consume x.f`
    /// leaves one the base's release walks around; the write-back
    /// form's store fills it. This is where a binding's holes are stated.
    fn take_place_at(
        &mut self,
        e: &'a Expr,
        out: &mut Vec<St>,
        keeps_hole: bool,
    ) -> Result<Val, Gap> {
        let ty = self.ty_of(e)?;
        let place = self.place(e, out)?;
        self.pending_receiver = None;
        if keeps_hole {
            if let Some((n, path)) = crate::kernel::root_of(&place) {
                // A hole the walk cannot skip is not stated: a declared
                // `release` cannot be told to leave a field alone, so it would
                // free the field twice (`refusals/r22_drop_with_a_hole.vyrn`).
                let bty = self.body.names[n as usize].ty.clone();
                let rel = path.trim_start_matches('.').to_string();
                if !rel.is_empty()
                    && vyrn_frontend::declared::skippable(
                        &self.own.proto,
                        &bty,
                        std::slice::from_ref(&rel),
                    )
                {
                    let hs = &mut self.body.names[n as usize].holes;
                    if !hs.contains(&path) {
                        hs.push(path);
                        hs.sort();
                    }
                }
            }
        }
        let t = self.temp(ty, e.line());
        out.push(St::Let(t, Rhs::Take(place)));
        Ok(Val::Name(t))
    }

    /// The String `jsonSchema<T>()` renders from `T`'s declaration at compile
    /// time. `direct::Fn_::reflected` renders the same declaration.
    fn schema(&self, e: &Expr) -> Option<Lit> {
        let Expr::Call {
            name, type_args, ..
        } = e
        else {
            return None;
        };
        let [Type::Named(t) | Type::App(t, _)] = type_args.as_slice() else {
            return None;
        };
        let types = self.proto.types();
        let decl = types.get(t).filter(|_| name == "jsonSchema")?;
        Some(Lit::Str(vyrn_frontend::types::json_schema_string(
            decl, types,
        )))
    }

    /// The `impl Show` function a `print`, `@str` or `value` of one argument
    /// calls, where the program declares it.
    fn render_callee(&self, name: &str, args: &[Expr]) -> Option<String> {
        let [a] = args else { return None };
        if !matches!(name, "print" | "@str" | "value") {
            return None;
        }
        let t = self.ty_of(a).ok()?;
        let base = vyrn_frontend::types::resolve(&t, self.proto.types());
        vyrn_frontend::types::show_dispatch(&self.program.impls, &t, &base)
            .filter(|f| self.program.functions.iter().any(|d| &d.name == f))
    }

    /// `@copy` of the read `e`. The copy drains its operand as a call does: a
    /// temporary the read left (`pieces()` of `pieces()[0]`) is dropped once
    /// the copy is bound.
    fn copy_of(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Rhs, Gap> {
        self.drain += 1;
        let v = self.read_at(e, out, None);
        self.drain -= 1;
        Ok(Rhs::Call {
            callee: "@copy".to_string(),
            args: vec![(Arg::Val(v?), Capability::Read)],
            write_back: false,
            kind: Callee::Reserved,
            ret: Some(self.ty_of(e)?),
            solved: Vec::new(),
            targets: Vec::new(),
        })
    }

    /// Whether `name(args)` at `e` is a heap element of a temporary
    /// (`pieces()[0]`), stated as `@copy` of the read (#537): the temporary
    /// is released whole after the consumer, so the taker must own a copy. A
    /// type with `impl Copy` is read as any element is.
    fn copies_an_element(&self, name: &str, args: &[Expr], e: &Expr) -> bool {
        name == vyrn_frontend::project::AT
            && args.len() == 2
            && !is_place_read(&args[0])
            && self.ty_of(e).is_ok_and(|t| {
                self.owns(&t) && vyrn_frontend::types::copy_impl(&self.program.impls, &t).is_none()
            })
    }

    /// Whether `name(args)` at `e` is a read that owns no heap: an element of
    /// a builtin array, a String's byte, or a map's entry, whose `Option` the
    /// runtime's lookup builds. A receiver that is no place is bound to a
    /// temporary and released after the read, as a field's is.
    fn reads_an_element(&self, name: &str, args: &[Expr], e: &Expr) -> bool {
        name == vyrn_frontend::project::AT
            && args.len() == 2
            && self.ty_of(e).is_ok_and(|t| !self.owns(&t))
            && self.ty_of(&args[0]).is_ok_and(|t| {
                matches!(
                    vyrn_frontend::types::resolve(&t, self.proto.types()),
                    Type::Array(_)
                        | Type::ArrayN(..)
                        | Type::SmallArray(..)
                        | Type::Str
                        | Type::Map(..)
                )
            })
    }

    /// An expression as the right-hand side of a `let`. The temporaries its
    /// own reads queued are left in `after_of_rhs` for the binding that
    /// follows; an enclosing expression's are kept aside meanwhile, so a
    /// nested read cannot drop what an outer one is about to read.
    fn rhs(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Rhs, Gap> {
        let outer = std::mem::take(&mut self.after);
        let r = self.rhs_inner(e, out);
        let mine = std::mem::replace(&mut self.after, outer);
        self.after_of_rhs = mine;
        r
    }

    /// `a && b` as `if a { b } else { false }`, and `a || b` as
    /// `if a { true } else { b }`, storing into a `Bool` temporary on each
    /// edge. The checker refuses any operand but `Bool`.
    fn short_circuit(
        &mut self,
        op: BinOp,
        lhs: &'a Expr,
        rhs: &'a Expr,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Rhs, Gap> {
        let res = self.temp(Type::Bool, line);
        let store = |value| St::Store {
            place: Place::Name(res),
            value,
            old: Old::Nothing,
            line,
            site: Site::None,
            releases: false,
            holes: Vec::new(),
        };
        // The left operand runs where the expression does, and the emitter
        // drains its temporaries at the operator (`Fn_::binary`).
        self.drain += 1;
        let cond = self.read_val(lhs, out)?;
        let mark = self.after.len();
        let mut taken = Vec::new();
        let v = self.read_val(rhs, &mut taken)?;
        taken.push(store(v));
        // The right operand's temporaries are released on its edge, the only
        // path that evaluates it.
        for t in self.after.split_off(mark) {
            taken.push(St::Drop(t, Site::None, 0, None));
        }
        self.drain -= 1;
        let decided = vec![store(Val::Lit(Lit::Bool(op == BinOp::Or)))];
        let (then, els) = if op == BinOp::And {
            (taken, decided)
        } else {
            (decided, taken)
        };
        out.push(St::If {
            cond,
            then,
            els,
            site: 0,
        });
        Ok(Rhs::Val(Val::Name(res)))
    }

    fn rhs_inner(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Rhs, Gap> {
        match e {
            // Named again because this match is exhaustive on purpose: a new
            // `Expr` variant must fail to compile. `lit_of` says what each is.
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {
                match lit_of(e) {
                    Some(l) => Ok(Rhs::Val(Val::Lit(l))),
                    None => gap("a literal form `lit_of` does not answer", e.line()),
                }
            }
            // A `let` builds a nullary constructor in its own slot, as it
            // builds `Some(v)`; [`Builder::nullary`] names one elsewhere.
            Expr::Var { name, line } if self.lookup(name).is_none() && self.is_nullary(name) => {
                let ty = self.ty_of(e)?;
                self.call(name, &[], *line, Some(ty), out)
            }
            Expr::Var { .. } | Expr::Consume { .. } => Ok(Rhs::Val(self.val(e, out)?)),
            Expr::Unary { op, expr, .. } => Ok(Rhs::Prim(
                Op::Un(*op),
                vec![self.read_val(expr, out)?],
                self.produced(e),
            )),
            Expr::Binary { op, lhs, rhs, line } => {
                // `&&` and `||` are control flow: the right operand may not run.
                if matches!(op, BinOp::And | BinOp::Or) {
                    return self.short_circuit(*op, lhs, rhs, *line, out);
                }
                // An operator drains its operands' temporaries in both
                // compiled backends (`binary`, `gen_binary`).
                self.drain += 1;
                // A String `+` is `@concat`, and a comparison and a `=~` read
                // operands the same way, so an allocating operand is an
                // argument at `(@concat, side)`. A `+` concatenates where its
                // own type is `String`.
                let concat = matches!(op, BinOp::Add)
                    && self.ty_of(e).is_ok_and(|t| {
                        matches!(
                            vyrn_frontend::types::resolve(&t, self.proto.types()),
                            Type::Str
                        )
                    });
                let compares = matches!(
                    op,
                    BinOp::Eq | BinOp::NotEq | BinOp::Lt | BinOp::LtEq | BinOp::Gt | BinOp::GtEq
                );
                let a = if concat || compares || matches!(op, BinOp::Match) {
                    self.read_arg(lhs, out, "@concat", 0)?
                } else {
                    self.read_val(lhs, out)?
                };
                let b = if concat || compares {
                    self.read_arg(rhs, out, "@concat", 1)?
                } else {
                    self.read_val(rhs, out)?
                };
                self.drain -= 1;
                Ok(Rhs::Prim(Op::Bin(*op), vec![a, b], self.produced(e)))
            }
            Expr::Field { expr, field, .. } => {
                if let Some(inner) = self.deferred_of(e) {
                    return Ok(self.force(e, inner, out)?);
                }
                let fty = self.ty_of(e)?;
                let place = self.place(expr, out)?;
                if let Some((r, _, _)) = self.pending_receiver {
                    self.body.names[r as usize].receiver = Some(e as *const Expr as usize);
                }
                if self.owns(&fty) {
                    // `let sels = parse(q).sels`: the binding takes the field
                    // out of the unnamed receiver; the kernel sees the rest
                    // held.
                    return Ok(Rhs::Take(Place::Field(Box::new(place), field.clone())));
                }
                Ok(Rhs::Read(Place::Field(Box::new(place), field.clone())))
            }
            Expr::Call {
                dot: _,
                name,
                args,
                line,
                type_args: _,
            } if name == "panic" || name == "@panicAt" || name == "serveStream" => {
                let r = if name == "serveStream" {
                    // A compiled build has no accept loop.
                    let msg = Lit::Str(vyrn_frontend::trap::SERVE_STREAM.into());
                    Rhs::Call {
                        callee: name.clone(),
                        args: vec![(Arg::Val(Val::Lit(msg)), Capability::Read)],
                        write_back: false,
                        kind: Callee::Builtin,
                        ret: self.produced(e),
                        solved: Vec::new(),
                        targets: Vec::new(),
                    }
                } else {
                    self.call(name, args, *line, self.produced(e), out)?
                };
                out.push(St::Do {
                    rhs: r,
                    line: *line,
                    site: 0,
                });
                out.push(St::Trap);
                Ok(Rhs::Val(Val::Lit(Lit::Opaque(Opaque::Trapped))))
            }
            // `xs[i]` of a heapless element, a String's byte and a map entry
            // are reads, not calls.
            Expr::Call {
                dot: _,
                name,
                args,
                line,
                type_args,
            } => {
                if self.reads_an_element(name, args, e) {
                    return Ok(Rhs::Read(self.place(e, out)?));
                }
                if self.copies_an_element(name, args, e) {
                    return self.copy_of(e, out);
                }
                // A builtin whose argument names its callee is a call to that
                // function where the program declares it.
                if let Some((f, fwd)) =
                    vyrn_frontend::loader::routed_callee(name, type_args, args, |a| {
                        self.ty_of(a).ok()
                    })
                    .filter(|(f, _)| self.program.functions.iter().any(|d| &d.name == f))
                {
                    return self.call(&f, fwd, *line, self.produced(e), out);
                }
                // A render of a type the language does not render calls its
                // `impl Show`. `print` releases the String after; `value`
                // takes it.
                if let Some(f) = self.render_callee(name, args) {
                    let r = self.call(&f, args, *line, Some(Type::Str), out)?;
                    if name == "@str" {
                        return Ok(r);
                    }
                    let t = self.temp(Type::Str, *line);
                    out.push(St::Let(t, r));
                    let cap = if name == "value" {
                        Capability::Consume
                    } else {
                        self.after.push(t);
                        Capability::Read
                    };
                    return Ok(Rhs::Call {
                        callee: name.clone(),
                        args: vec![(Arg::Val(Val::Name(t)), cap)],
                        write_back: false,
                        kind: Callee::Reserved,
                        ret: self.produced(e),
                        solved: Vec::new(),
                        targets: Vec::new(),
                    });
                }
                if let Some(l) = self.schema(e) {
                    return Ok(Rhs::Val(Val::Lit(l)));
                }
                // `schemaOf<T>()` is the `Schema` literal it stands for, whose
                // nodes the checker typed (`project::schema_at`).
                if let Some(lit) = vyrn_frontend::project::schema_at(e) {
                    return self.rhs(lit, out);
                }
                let id = e as *const Expr as usize;
                // Every call, accepted ones too: the solve binds a caller's
                // own type parameter where a slot names it (#566).
                let solved = self.solved.get(&id).map_or(&[][..], Vec::as_slice);
                let bound = |v: &str| {
                    self.lookup(v).is_some() || self.program.globals.iter().any(|g| g.name == v)
                };
                let decls = self.proto.types();
                let at = fn_slot(
                    self.program,
                    name,
                    args,
                    *line,
                    solved,
                    &self.types,
                    &decls,
                    &bound,
                );
                self.body.mistyped.extend(at);
                let mut r = self.call(name, args, *line, self.produced(e), out)?;
                if let Rhs::Call {
                    kind: Callee::Fn,
                    solved,
                    targets,
                    ..
                } = &mut r
                {
                    if let Some(s) = self.solved.get(&(e as *const Expr as usize)) {
                        *solved = s.clone();
                    }
                    let subst: HashMap<String, Type> = solved.iter().cloned().collect();
                    for t in targets.iter_mut() {
                        if let Target::Lambda(_, _, slot) = t {
                            *slot = vyrn_frontend::types::substitute(slot, &subst);
                        }
                    }
                }
                Ok(r)
            }
            Expr::TryConstruct { name, args, .. } => {
                self.unknown_at(e);
                let mut vs = Vec::new();
                for a in args {
                    vs.push(self.val(a, out)?);
                }
                Ok(Rhs::Make(Ctor::Try(name.clone()), vs))
            }
            // A part crosses into its slot's type as a stored value does.
            Expr::ArrayLit { elems, line } => {
                let ety = self
                    .ty_of(e)
                    .ok()
                    .and_then(|t| self.elem_ty(&t, *line).ok());
                let mut vs = Vec::new();
                for a in elems {
                    vs.push(self.proven_val(a, ety.as_ref(), *line, out)?);
                }
                Ok(Rhs::Make(Ctor::Array, vs))
            }
            Expr::StructLit { name, fields, line } => {
                let ty = self.ty_of(e).ok();
                let mut vs = Vec::new();
                for (f, a) in fields {
                    let fty = ty.as_ref().and_then(|t| self.field_ty(t, f, *line).ok());
                    vs.push(self.proven_val(a, fty.as_ref(), *line, out)?);
                }
                let to = Type::Named(name.clone());
                if self
                    .proto
                    .types()
                    .get(name)
                    .is_some_and(|d| d.predicate.is_some())
                    && !self.proven(e, &to)
                {
                    self.owed = Some((name.clone(), *line));
                }
                Ok(Rhs::Make(
                    Ctor::Record(
                        name.clone(),
                        fields.iter().map(|(f, _)| f.clone()).collect(),
                    ),
                    vs,
                ))
            }
            Expr::MapLit { entries, line } => {
                let (kty, vty) = match self
                    .ty_of(e)
                    .map(|t| vyrn_frontend::types::resolve(&t, self.proto.types()))
                {
                    Ok(Type::Map(k, v)) => (Some(*k), Some(*v)),
                    _ => (None, None),
                };
                let mut vs = Vec::new();
                for (k, v) in entries {
                    vs.push(self.proven_val(k, kty.as_ref(), *line, out)?);
                    vs.push(self.proven_val(v, vty.as_ref(), *line, out)?);
                }
                Ok(Rhs::Make(Ctor::Map, vs))
            }
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                line,
            } => {
                let ty = self.ty_of(e)?;
                // The plan keys an if-expression's edge rows by the expression.
                let site = e as *const Expr as usize;
                let res = self.temp(ty, *line);
                let c = self.condition(cond, "if", *line, out)?;
                let mark = self.body.names.len();
                let mut t = Vec::new();
                let tv = self.val(then_branch, &mut t)?;
                let mut aliased = self.alias_out(&tv, mark);
                let then_v = tv.clone();
                t.push(St::Store {
                    place: Place::Name(res),
                    value: tv,
                    old: Old::Nothing,
                    line: *line,
                    site: Site::None,
                    releases: false,
                    holes: Vec::new(),
                });
                self.edge_drops(site, 0, &mut t)?;
                let mut f = Vec::new();
                match else_branch {
                    Some(eb) => {
                        let ev = self.val(eb, &mut f)?;
                        aliased = aliased.or(self.alias_out(&ev, mark));
                        let else_v = ev.clone();
                        f.push(St::Store {
                            place: Place::Name(res),
                            value: ev,
                            old: Old::Nothing,
                            line: *line,
                            site: Site::None,
                            releases: false,
                            holes: Vec::new(),
                        });
                        self.edge_drops(site, 1, &mut f)?;
                        self.join_borrows(res, &[then_v, else_v]);
                        if let Some(a) = aliased {
                            self.loop_aliased.insert(res, a);
                        }
                    }
                    None => return gap("an `if` expression without `else`", *line),
                }
                out.push(St::If {
                    cond: c,
                    then: t,
                    els: f,
                    site,
                });
                Ok(Rhs::Val(Val::Name(res)))
            }
            Expr::Match {
                scrutinee,
                arms,
                line,
                ..
            } => {
                let ty = self.ty_of(e)?;
                let sty = self.ty_of(scrutinee)?;
                let mid = e as *const Expr as usize;
                let res = self.temp(ty, *line);
                let (sv, consuming) =
                    self.scrutinee(scrutinee, mid, Some(arms_span(*line, arms)), out)?;
                let owns = self.owns_boxes(scrutinee, consuming);
                let outer = self.body.names.len();
                let mut core_arms = Vec::new();
                let mut yields = Vec::new();
                for (i, arm) in arms.iter().enumerate() {
                    let mut body = Vec::new();
                    let mark = self.scope.len();
                    let binds = self.bind_pattern(
                        &arm.pattern,
                        &sty,
                        consuming,
                        *line,
                        borrow_root(&sv, owns),
                        &mut body,
                    )?;
                    match &arm.body {
                        ArmBody::Expr(ae) => {
                            let v = self.val(ae, &mut body)?;
                            if let Some(a) = self.alias_out(&v, outer) {
                                self.loop_aliased.insert(res, a);
                            }
                            yields.push(v.clone());
                            body.push(St::Store {
                                place: Place::Name(res),
                                value: v,
                                old: Old::Nothing,
                                line: *line,
                                site: Site::None,
                                releases: false,
                                holes: Vec::new(),
                            });
                        }
                        ArmBody::Block(blk) => self.block(blk, &mut body)?,
                    }
                    let frees = self.arm_frees(mid, i as u32, &binds, &mut body);
                    self.edge_drops(mid, i as u32, &mut body)?;
                    self.scope.truncate(mark);
                    core_arms.push(Arm {
                        binds,
                        frees: Some(frees),
                        body,
                        test: self.arm_test(&arm.pattern, &sty, *line)?,
                        site: mid,
                        index: i as u32,
                    });
                }
                self.covers(&core_arms, &sty, *line, out)?;
                self.join_borrows(res, &yields);
                out.push(St::Switch {
                    on: sv,
                    arms: core_arms,
                    consuming,
                    carries: false,
                    owns,
                    site: mid,
                    line: *line,
                });
                self.drops_at(Exit::Scrutinee, mid, out)?;
                Ok(Rhs::Val(Val::Name(res)))
            }
            Expr::Try { expr, line } => {
                let ty = self.ty_of(e)?;
                let ity = self.ty_of(expr)?;
                let tid = e as *const Expr as usize;
                let res = self.temp(ty, *line);
                let (sv, consuming) = self.scrutinee(expr, tid, None, out)?;
                let owns = self.owns_boxes(expr, consuming);
                let decls = self.proto.types();
                // A declared `Fallible` enum asks its impl. `Option`
                // and `Result` resolve to variant lists too, so they are
                // excluded by name.
                let r = vyrn_frontend::types::resolve(&ity, &decls);
                if matches!(r, Type::Enum(_))
                    && vyrn_frontend::types::option_payload(&r).is_none()
                    && vyrn_frontend::types::result_payloads(&r).is_none()
                {
                    return self.fallible_try(ity, sv, owns, res, tid, out);
                }
                // Failure: the exit's drops, then the propagated value leaves.
                let mut fail = Vec::new();
                let mark = self.scope.len();
                let fb = self.bind_pattern(
                    &Pattern::Failure(Binder::synthetic("@err")),
                    &ity,
                    consuming,
                    *line,
                    borrow_root(&sv, owns),
                    &mut fail,
                )?;
                self.leave_loops(&mut fail);
                self.drops_at(Exit::Try, tid, &mut fail)?;
                // An `Option` fails with `None` of the frame's result; a
                // `Result` with its error binder taken into `Err`.
                let value = match (fb.first(), self.ret.clone()) {
                    (Some(n), Some(rt)) => {
                        let t = self.temp(rt.clone(), *line);
                        fail.push(St::Let(
                            t,
                            Rhs::Call {
                                callee: "Err".into(),
                                args: vec![(Arg::Val(Val::Name(*n)), Capability::Consume)],
                                write_back: false,
                                kind: Callee::Ctor,
                                ret: Some(rt),
                                solved: Vec::new(),
                                targets: Vec::new(),
                            },
                        ));
                        Some(Val::Name(t))
                    }
                    (Some(n), None) => Some(Val::Name(*n)),
                    (None, Some(rt)) => Some(self.nullary("None", rt, *line, &mut fail)?),
                    (None, None) => None,
                };
                fail.push(St::Return {
                    value,
                    site: tid,
                    is_try: true,
                    line: *line,
                });
                self.scope.truncate(mark);
                let mut ok = Vec::new();
                let mark = self.scope.len();
                let ob = self.bind_pattern(
                    &Pattern::Success(Binder::synthetic("@ok")),
                    &ity,
                    consuming,
                    *line,
                    borrow_root(&sv, owns),
                    &mut ok,
                )?;
                ok.push(St::Store {
                    place: Place::Name(res),
                    value: ob
                        .first()
                        .map(|n| Val::Name(*n))
                        .unwrap_or(Val::Lit(Lit::Opaque(Opaque::Unbound))),
                    old: Old::Nothing,
                    line: *line,
                    site: Site::None,
                    releases: false,
                    holes: Vec::new(),
                });
                let ok_frees = self.arm_frees(tid, 1, &ob, &mut ok);
                let fail_frees = self.arm_frees(tid, 0, &fb, &mut fail);
                self.scope.truncate(mark);
                out.push(St::Switch {
                    on: sv,
                    arms: vec![
                        Arm {
                            frees: Some(fail_frees),
                            binds: fb,
                            body: fail,
                            test: Test::Tag(0),
                            site: tid,
                            index: 0,
                        },
                        Arm {
                            frees: Some(ok_frees),
                            binds: ob,
                            body: ok,
                            test: Test::Tag(1),
                            site: tid,
                            index: 1,
                        },
                    ],
                    consuming,
                    carries: false,
                    owns,
                    site: tid,
                    line: *line,
                });
                Ok(Rhs::Val(Val::Name(res)))
            }
            Expr::Lambda { .. } => {
                let caps = self.captures(e);
                // Waits for the name `bind` gives ([`Builder::pending_closure`]).
                self.pending_closure = self.closure_reads(e, &caps);
                let key = self.lambda_frame(e, &caps)?;
                Ok(Rhs::Prim(Op::Closure(key), caps, self.produced(e)))
            }
        }
    }

    /// `?` on a declared `Fallible` type: the failing path returns
    /// the whole value; the succeeding path hands it to the impl's `success`,
    /// which reads it and answers a value of its own, then releases it. Each
    /// arm accounts for the value exactly once.
    fn fallible_try(
        &mut self,
        ity: Type,
        sv: Val,
        owns: bool,
        res: Name,
        tid: usize,
        out: &mut Vec<St>,
    ) -> Result<Rhs, Gap> {
        let line = self.body.names[res as usize].line;
        let Some(key) = vyrn_frontend::types::type_key(&ity) else {
            return gap("a `?` on a type with no impl key", line);
        };
        let method = |m: &str| {
            vyrn_frontend::types::impl_method_name(vyrn_frontend::types::FALLIBLE, &key, m)
        };
        let success = method("success");
        // The impl's `isSuccess` chooses the arm. Both impl calls are declared
        // functions under the dispatched name and read their argument.
        let held = self.temp(Type::Bool, line);
        out.push(St::Let(
            held,
            Rhs::Call {
                solved: self
                    .impl_args(&method("isSuccess"), &ity)
                    .unwrap_or_default(),
                callee: method("isSuccess"),
                args: vec![(Arg::Val(sv.clone()), Capability::Read)],
                write_back: false,
                kind: Callee::Fn,
                ret: Some(Type::Bool),
                targets: Vec::new(),
            },
        ));
        let failed = self.temp(Type::Bool, line);
        out.push(St::Let(
            failed,
            Rhs::Prim(Op::Un(UnOp::Not), vec![Val::Name(held)], Some(Type::Bool)),
        ));
        let mut fail = Vec::new();
        self.leave_loops(&mut fail);
        self.drops_at(Exit::Try, tid, &mut fail)?;
        fail.push(St::Return {
            value: Some(sv.clone()),
            site: tid,
            is_try: true,
            line,
        });
        let mut ok = Vec::new();
        let t = self.temp(self.body.names[res as usize].ty.clone(), line);
        ok.push(St::Let(
            t,
            Rhs::Call {
                solved: self.impl_args(&success, &ity).unwrap_or_default(),
                callee: success,
                args: vec![(Arg::Val(sv.clone()), Capability::Read)],
                write_back: false,
                kind: Callee::Fn,
                ret: Some(self.body.names[res as usize].ty.clone()),
                targets: Vec::new(),
            },
        ));
        if let Val::Name(n) = sv {
            if owns && self.body.names[n as usize].releases {
                ok.push(St::Drop(n, Site::None, 0, None));
            }
        }
        ok.push(St::Store {
            place: Place::Name(res),
            value: Val::Name(t),
            old: Old::Nothing,
            line,
            site: Site::None,
            releases: false,
            holes: Vec::new(),
        });
        out.push(St::Switch {
            on: sv,
            arms: vec![
                Arm {
                    frees: Some(Vec::new()),
                    binds: Vec::new(),
                    body: fail,
                    test: Test::Holds(failed),
                    site: tid,
                    index: 0,
                },
                Arm {
                    frees: Some(Vec::new()),
                    binds: Vec::new(),
                    body: ok,
                    test: Test::Else,
                    site: tid,
                    index: 1,
                },
            ],
            consuming: false,
            carries: false,
            owns,
            site: tid,
            line,
        });
        Ok(Rhs::Val(Val::Name(res)))
    }

    /// A place, for a read or a store. A field chain over a name, an element
    /// of one, or a temporary the expression produced (an unnamed receiver).
    fn place(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Place, Gap> {
        match e {
            Expr::Var { name, .. } => match self.lookup(name) {
                Some(n) => Ok(Place::Name(n)),
                None => {
                    self.unknown_at(e);
                    Ok(Place::Global(name.clone()))
                }
            },
            Expr::Field { expr, field, .. } => {
                let base = self.place(expr, out)?;
                Ok(Place::Field(Box::new(base), field.clone()))
            }
            Expr::Call {
                name, args, line, ..
            } if name == "@at" && args.len() == 2 => {
                if let Some(p) = self.inlined("at", &args[0], &args[1..], *line, out)? {
                    return Ok(p);
                }
                let bty = self.ty_of(&args[0])?;
                let base = self.place(&args[0], out)?;
                // The receiver is this read's; a field read in the index would
                // release it as its own.
                let receiver = self.pending_receiver.take();
                let i = self.read_val(&args[1], out)?;
                self.pending_receiver = receiver;
                if self.is_map(&bty) {
                    Ok(Place::Key(Box::new(base), i))
                } else {
                    Ok(Place::Elem(Box::new(base), i))
                }
            }
            _ => {
                let v = self.val(e, out)?;
                match v {
                    Val::Name(t) => {
                        if self.body.names[t as usize].releases {
                            // [`NameInfo::receiver_malloc`]: a callee's block.
                            let malloc = matches!(
                                e,
                                Expr::Call { name, .. } if !name.starts_with('@')
                            );
                            self.pending_receiver = Some((t, e as *const Expr as usize, malloc));
                        }
                        Ok(Place::Name(t))
                    }
                    // A literal receiver (`"abc".byteLength`): a temporary
                    // bound to the literal gives the chain a base.
                    Val::Lit(_) => {
                        let ty = self.ty_of(e)?;
                        let t = self.temp(ty, e.line());
                        out.push(St::Let(t, Rhs::Val(v)));
                        Ok(Place::Name(t))
                    }
                }
            }
        }
    }

    fn call(
        &mut self,
        name: &str,
        args: &'a [Expr],
        line: usize,
        ret: Option<Type>,
        out: &mut Vec<St>,
    ) -> Result<Rhs, Gap> {
        // `a[i]` asks the receiver's type for `at` before any builtin row, as
        // `Checker::call` dispatches it.
        if let (vyrn_frontend::project::AT, [recv, rest @ ..]) = (name, args) {
            if let Some(p) = self.inlined("at", recv, rest, line, out)? {
                return Ok(Rhs::Read(p));
            }
        }
        // `value(s)` of a String: the box owns its payload (#512), so a String
        // read out of a place is copied first.
        if let ("value", [arg]) = (name, args) {
            let string = self
                .ty_of(arg)
                .is_ok_and(|t| vyrn_frontend::types::resolve(&t, self.proto.types()) == Type::Str);
            if string {
                let v = if prelude::boxes_a_copy(arg, string) {
                    let copy = self.call("@copy", args, line, Some(Type::Str), out)?;
                    let c = self.temp(Type::Str, line);
                    self.bind(c, copy, out);
                    Val::Name(c)
                } else {
                    self.val(arg, out)?
                };
                return Ok(Rhs::Call {
                    callee: name.to_string(),
                    args: vec![(Arg::Val(v), Capability::Consume)],
                    write_back: false,
                    kind: Callee::Reserved,
                    ret,
                    solved: Vec::new(),
                    targets: Vec::new(),
                });
            }
        }
        let decls = self.proto.types();
        let method = self
            .program
            .impls
            .iter()
            .flat_map(|i| i.methods.iter())
            .find(|m| m.name == name);
        // A seeded row whose result is its receiver's own type hands the
        // buffer back through the result, so the receiver is taken by the
        // call.
        let rebuilds = prelude::rebuilds(name);
        // Who the callee is decides each argument position's capability.
        let mut kind = Callee::Reserved;
        // A binding of function type is asked first, as `Checker::call` asks
        // it: a `fn`-typed parameter `h` shadows a function `h` the program
        // declares, and `h(req)` is a call through the value.
        let bound = self.lookup(name).filter(|n| {
            matches!(
                vyrn_frontend::types::resolve(&self.body.names[*n as usize].ty, decls),
                Type::Fn(..)
            )
        });
        let mut caps: Vec<Capability> = if let Some(n) = bound {
            // A lambda captures by read and takes by read.
            kind = Callee::Value(n);
            vec![Capability::Read; args.len()]
        } else if let Some(f) = self.program.functions.iter().find(|f| f.name == name) {
            kind = Callee::Fn;
            f.params.iter().map(|p| p.capability).collect()
        } else if prelude::signature(name).is_some() {
            kind = Callee::Builtin;
            let mut caps: Vec<Capability> = (0..args.len())
                .map(|i| prelude::capability(name, i).unwrap_or(Capability::Read))
                .collect();
            if rebuilds && !caps.is_empty() {
                caps[0] = Capability::Consume;
            }
            caps
        } else if let Some(m) = method {
            kind = Callee::Method;
            m.params.iter().map(|p| p.capability).collect()
        } else if let Some(p) = self.projection(name) {
            kind = Callee::Projection;
            p.params.iter().map(|p| p.capability).collect()
        } else if let Some(sig) = self
            .program
            .protocols
            .iter()
            .flat_map(|p| &p.methods)
            .find(|m| m.name == name)
        {
            // A protocol member no impl answers, called on a bounded type
            // parameter in a generic read as written: its signature is what
            // a caller reads (`MethodSig::recv`).
            kind = Callee::Method;
            std::iter::once(sig.recv)
                .chain(sig.param_caps.iter().copied())
                .collect()
        } else if matches!(name, "Some" | "None" | "Ok" | "Err") || self.is_variant(name) {
            kind = Callee::Ctor;
            vec![Capability::Consume; args.len()]
        } else if vyrn_frontend::checker::RESERVED.contains(&name)
            || vyrn_frontend::ast::is_surface_builtin(name)
        {
            // A reserved name with no prelude row (`fromJson`, `value`, a
            // generation-time surface builtin, `ast::SURFACE_BUILTINS`): the
            // prelude's capability where it has one, `read` elsewhere.
            (0..args.len())
                .map(|i| prelude::capability(name, i).unwrap_or(Capability::Read))
                .collect()
        } else if decls.contains_key(name) {
            kind = match args {
                [v] if decls[name].predicate.is_some()
                    && self.proven(v, &Type::Named(name.to_string())) =>
                {
                    Callee::Proven
                }
                _ => Callee::Named,
            };
            vec![Capability::Consume; args.len()]
        } else if name.starts_with('@') {
            vec![Capability::Read; args.len()]
        } else if matches!(name, "print") {
            vec![Capability::Read; args.len()]
        } else if matches!(
            name,
            vyrn_frontend::checker::GEN_REFLECT
                | vyrn_frontend::checker::GEN_NEXT_INT
                | vyrn_frontend::checker::GEN_NEXT_STR
        ) {
            // The generation host's primitives, which exist only
            // under `checker::set_gen_host`. The host reads what it is
            // handed, so the guest keeps every argument it owns.
            vec![Capability::Read; args.len()]
        } else if let Some(g) = self.program.globals.iter().find(|g| {
            g.name == name
                && matches!(
                    vyrn_frontend::types::resolve(&g.ty.clone().unwrap_or(Type::Unit), decls),
                    Type::Fn(..)
                )
        }) {
            // Module state of function type: borrowed out of the
            // global and called through, as a forced `lazy` field is.
            let ty = g.ty.clone().unwrap_or(Type::Unit);
            let n = self.name(name, ty, false, line);
            out.push(St::Let(n, Rhs::Read(Place::Global(name.to_string()))));
            kind = Callee::Value(n);
            vec![Capability::Read; args.len()]
        } else {
            return gap_d("a call this slice cannot attribute", name, line);
        };
        if caps.len() < args.len() {
            return gap("a call with more arguments than parameters", line);
        }
        if let ("@pop" | "@swapRemove", Some(recv)) = (name, args.first()) {
            self.shrinks(&name[1..], recv, line);
        }
        if let (Callee::Projection, Some(recv)) = (kind, args.first()) {
            if let Some(p) = self.inlined(name, recv, &args[1..], line, out)? {
                return Ok(Rhs::Read(p));
            }
        }
        let mut vs = Vec::new();
        let mut temps_to_drop = Vec::new();
        // [`Rhs::Call`]'s `write_back`.
        let write_back = rebuilds
            && caps.first() == Some(&Capability::Consume)
            && matches!(args.first(), Some(Expr::Var { .. }));
        // A call drains its arguments' temporaries after it runs, unless its
        // result points into one of them: a lending call leaves them to the
        // call or operator above (both backends' `call` drain).
        let lends_here = self.lends_name(name) || self.hands_back_a_borrow(name, args);
        if vyrn_frontend::movecheck::hands_back(name)
            && !lends_here
            && !caps.is_empty()
            && args
                .first()
                .is_some_and(|a| self.ty_of(a).is_ok_and(|t| self.owns(&t)))
        {
            // The result is the argument: the temporary is taken and released
            // once, as the result.
            caps[0] = Capability::Consume;
        }
        // A stream is linear: a position it fills takes it, whatever
        // the position's word says.
        for (c, a) in caps.iter_mut().zip(args) {
            if self
                .ty_of(a)
                .is_ok_and(|t| matches!(vyrn_frontend::types::resolve(&t, decls), Type::Stream(_)))
            {
                *c = Capability::Consume;
            }
        }
        // `@list([..])` moves the literal's elements into the array it builds,
        // so it takes the literal; a read would release them twice.
        if name == "@list" {
            caps.iter_mut().for_each(|c| *c = Capability::Consume);
        }
        let drains = !lends_here;
        if drains {
            self.drain += 1;
        }
        let bound = match kind {
            Callee::Fn => self.targets_of(name, args),
            _ => Vec::new(),
        };
        let param_tys: Vec<Type> = match kind {
            Callee::Fn => self
                .program
                .functions
                .iter()
                .find(|f| f.name == name)
                .map(|f| f.params.iter().map(|p| p.ty.clone()).collect())
                .unwrap_or_default(),
            _ => Vec::new(),
        };
        let mut targets = Vec::new();
        // A lambda target's captures and a stored value follow the call's own
        // arguments, where [`specialize`] puts a forwarded target's.
        let mut forwarded = Vec::new();
        for (k, (a, cap)) in args.iter().zip(caps.iter()).enumerate() {
            let forwards = match bound.get(k) {
                Some(Some(t @ Target::Value(_))) => {
                    targets.push(t.clone());
                    true
                }
                Some(Some(t)) => {
                    if let Target::Lambda(..) = t {
                        let vals = self.captures(a);
                        self.lambda_frame(a, &vals)?;
                        forwarded.extend(vals.into_iter().map(|v| (Arg::Val(v), Capability::Read)));
                    }
                    targets.push(t.clone());
                    continue;
                }
                _ => false,
            };
            // Whether this position may keep a lambda literal written at it
            // (`declared::arg_cap`); an unanswered position may, the safe
            // direction. A lambda deeper in the argument gets `None` and
            // escapes: a literal retains what it is given.
            self.call_keeps = matches!(a, Expr::Lambda { .. }).then(|| {
                vyrn_frontend::declared::arg_cap(&self.own.arg_caps, name, k)
                    .is_none_or(|c| c == Capability::Consume)
            });
            let global = matches!(a, Expr::Var { name, .. }
                if self.lookup(name).is_none()
                    && self.program.globals.iter().any(|g| &g.name == name));
            // Module state handed to `modify` (`insert(cells, v)`) is passed
            // as the place: a borrow of it would be written through.
            if let (Capability::Modify, Expr::Var { name, .. }, true) = (cap, a, global) {
                vs.push((Arg::Place(Place::Global(name.clone())), *cap));
                continue;
            }
            let proven = param_tys.get(k).and_then(|to| self.proven_crossing(a, to));
            let v = if let Some(to) = proven {
                // A proven crossing is the constructor row, so no reader
                // checks it again.
                let t = self.checked_temp(&to, a, line, out)?;
                if self.arg_released(a, t, name, k) {
                    self.body.names[t as usize].arg_drop = Some(a as *const Expr as usize);
                }
                Val::Name(t)
            } else if *cap == Capability::Consume {
                // The write-back form on module state (`books.push(b)`), a
                // field or an element: the receiver leaves the place and the
                // store after the call fills it.
                if k == 0
                    && rebuilds
                    && (global || !matches!(a, Expr::Var { .. } | Expr::Consume { .. }))
                    && is_place_read(a)
                {
                    self.take_place(a, out)?
                } else {
                    self.val(a, out)?
                }
            } else {
                self.read_arg(a, out, name, k)?
            };
            self.call_keeps = None;
            if let Val::Name(t) = v {
                // Queue the drop `read_arg`'s key stands for. The key also
                // stands on a borrow (a forced `lazy` field); only a name
                // this frame releases is dropped.
                let info = &self.body.names[t as usize];
                if info.releases && info.arg_drop.is_some() && !self.after.contains(&t) {
                    temps_to_drop.push(t);
                }
            }
            match forwards {
                true => forwarded.push((Arg::Val(v), *cap)),
                false => vs.push((Arg::Val(v), *cap)),
            }
        }
        vs.extend(forwarded);
        if drains {
            self.drain -= 1;
        }
        self.after.extend(temps_to_drop);
        // `Int32(n)` and its siblings convert between scalars. The operand is
        // read above like any argument, so the keying and drains stay.
        if let (Some(to), [(Arg::Val(v), _)]) = (
            vyrn_frontend::types::numeric_conv_target(name),
            vs.as_slice(),
        ) {
            return Ok(Rhs::Prim(Op::Conv(to), vec![v.clone()], ret));
        }
        // `@concat(a, b)` is the String `+` the interpolation spine spells as
        // a call, so the row is the operator's, over the same arguments.
        if let ("@concat", [(Arg::Val(a), _), (Arg::Val(b), _)]) = (name, vs.as_slice()) {
            return Ok(Rhs::Prim(
                Op::Bin(BinOp::Add),
                vec![a.clone(), b.clone()],
                ret,
            ));
        }
        // A method is a call after dispatch, and so is `x.copy()` of a type
        // with `impl Copy`.
        let dispatched = match (kind, args.first()) {
            (Callee::Method, Some(r)) => self.dispatched(name, r),
            (Callee::Reserved, Some(r)) if name == "@copy" => self.copied(r),
            _ => None,
        };
        let (callee, kind, solved) = match dispatched {
            Some((f, solved)) => (f, Callee::Fn, solved),
            None => (name.to_string(), kind, Vec::new()),
        };
        Ok(Rhs::Call {
            callee,
            args: vs,
            write_back,
            kind,
            ret,
            solved,
            targets,
        })
    }

    /// The impl function the method `name` dispatches to on `recv`'s type,
    /// and its type arguments ([`Builder::impl_args`]): the one function the
    /// program declares under a name some protocol with that method mangles.
    fn dispatched(&self, name: &str, recv: &Expr) -> Option<(String, Vec<(String, Type)>)> {
        let rty = self.ty_of(recv).ok()?;
        let key = vyrn_frontend::types::type_key(&rty)?;
        let fs: std::collections::BTreeMap<String, Vec<(String, Type)>> = self
            .program
            .impls
            .iter()
            .filter(|i| i.methods.iter().any(|m| m.name == name))
            .map(|i| vyrn_frontend::types::impl_method_name(&i.protocol, &key, name))
            .filter_map(|f| Some((f.clone(), self.impl_args(&f, &rty)?)))
            .collect();
        let mut fs = fs.into_iter();
        match (fs.next(), fs.next()) {
            (Some(f), None) => Some(f),
            _ => None,
        }
    }

    /// The `impl Copy` function `x.copy()` calls on `recv`'s type, and its
    /// type arguments.
    fn copied(&self, recv: &Expr) -> Option<(String, Vec<(String, Type)>)> {
        let rty = self.ty_of(recv).ok()?;
        let f = vyrn_frontend::types::copy_impl(&self.program.impls, &rty)?;
        let solved = self.impl_args(&f, &rty)?;
        Some((f, solved))
    }

    /// Whether the program declares `f` as a non-generic function.
    fn concrete_fn(&self, f: &str) -> bool {
        self.program
            .functions
            .iter()
            .any(|g| g.name == f && g.type_params.is_empty())
    }

    /// Per argument of a call to `name`, the [`Target`] a `fn`-typed
    /// parameter is bound to: a declared function, a bound parameter of this
    /// body, a lambda literal with its captures, or a stored value
    /// forwarded as its one capture. A lambda at a `consume` position is a
    /// stored value. Empty where the callee takes no function. A parameter is
    /// `fn`-typed as written; one of an alias type takes a stored value.
    fn targets_of(&self, name: &str, args: &[Expr]) -> Vec<Option<Target>> {
        let Some(f) = self.program.functions.iter().find(|f| f.name == name) else {
            return Vec::new();
        };
        let mut out = Vec::new();
        for (p, a) in f.params.iter().zip(args) {
            if !matches!(p.ty, Type::Fn(..)) {
                out.push(None);
                continue;
            }
            let value = Target::Value(p.name.clone());
            let t = match a {
                // At a `consume` position the literal falls to the last arm:
                // a value the kernel judges ([`NameInfo::closure_reads`]).
                Expr::Lambda { line, col, .. } if p.capability != Capability::Consume => {
                    let caps = (self.captures(a).into_iter())
                        .filter_map(|c| match c {
                            Val::Name(n) => Some(n),
                            Val::Lit(_) => None,
                        })
                        .map(|n| {
                            let info = &self.body.names[n as usize];
                            (info.source.clone(), info.ty.clone())
                        })
                        .collect();
                    let key = lambda_spelling(&self.body.name, *line, *col);
                    Target::Lambda(key, caps, p.ty.clone())
                }
                Expr::Var { name: v, .. } => match self.lookup(v) {
                    Some(n)
                        if self.body.params.contains(&n)
                            && matches!(self.body.names[n as usize].ty, Type::Fn(..)) =>
                    {
                        Target::Param(n)
                    }
                    Some(_) => value,
                    None if self.concrete_fn(v) => Target::Fn(v.clone()),
                    None => return Vec::new(),
                },
                _ => value,
            };
            out.push(Some(t));
        }
        if out.iter().all(Option::is_none) {
            return Vec::new();
        }
        out
    }

    /// Whether a bare name no binding holds is a nullary constructor: `None`
    /// or a fieldless variant.
    fn is_nullary(&self, name: &str) -> bool {
        name == "None" || self.is_variant(name)
    }

    fn is_variant(&self, name: &str) -> bool {
        let decls = self.proto.types();
        decls.values().any(|d| {
            vyrn_frontend::types::declared_variants(&d.base)
                .is_some_and(|vs| vs.iter().any(|v| v.name == name))
        })
    }
}

vyrn_frontend::body_scope_descent!(BodyVisit, body_block, body_stmt, body_expr);

/// The binding a place expression names: `s.id[0]` names `s`.
fn place_base(name: &str) -> &str {
    &name[..name.find(['.', '[']).unwrap_or(name.len())]
}

/// Whether a lambda body's mentions read `base` or a place under it.
fn reads_place(vars: &[&Expr], base: &str) -> bool {
    vars.iter().any(|v| match v {
        Expr::Var { name, .. } => {
            name == base
                || (name.len() > base.len()
                    && name.starts_with(base)
                    && matches!(name.as_bytes()[base.len()], b'.' | b'['))
        }
        _ => false,
    })
}

/// The captured local variables of a lambda body, in first-seen
/// order: names read in the body that are not in `locals` (the lambda's
/// parameters and its own bindings) and that `is_local` answers for as an
/// enclosing local. This is the one statement of the capture order: the
/// closure row lists its captures in it, and the emitter lifts the signature
/// in it.
pub fn lambda_captures(
    body: &LambdaBody,
    locals: std::collections::HashSet<String>,
    is_local: &dyn Fn(&str) -> bool,
) -> Vec<String> {
    struct CapturesOf<'a> {
        out: Vec<String>,
        seen: std::collections::HashSet<String>,
        is_local: &'a dyn Fn(&str) -> bool,
    }

    impl CapturesOf<'_> {
        fn take(&mut self, n: &str, locals: &std::collections::HashSet<String>) {
            if locals.contains(n) || self.seen.contains(n) {
                return;
            }
            // The lifted function reaches module state and functions directly.
            if (self.is_local)(n) {
                self.seen.insert(n.to_string());
                self.out.push(n.to_string());
            }
        }
    }

    impl BodyVisit<'_> for CapturesOf<'_> {
        fn expr(&mut self, e: &Expr, locals: &std::collections::HashSet<String>) -> bool {
            match e {
                Expr::Var { name, .. } => self.take(place_base(name), locals),
                // A call captures a callee that names an enclosing local:
                // `|req, ps| run(req)` over a `fn`-typed `run` calls a value,
                // not a symbol. `is_local` is false for a top-level function.
                Expr::Call { name, .. } => self.take(name, locals),
                // A lambda body holds no lambda literal.
                Expr::Lambda { .. } => return false,
                _ => {}
            }
            true
        }
    }

    let mut v = CapturesOf {
        out: Vec::new(),
        seen: std::collections::HashSet::new(),
        is_local,
    };
    let mut locals = locals;
    match body {
        LambdaBody::Expr(e) => body_expr(e, &locals, &mut v),
        LambdaBody::Block(b) => body_block(b, &mut locals, &mut v),
    }
    v.out
}

/// Every `Var` node in a lambda's body, nested lambdas included, minus the
/// names the body itself binds: where an untyped parameter's type can be
/// read, and which captures the closure reads as values. A shadowed name is
/// not recorded: a capture is a read, and counting one would refuse a program
/// that reads nothing.
fn mentions_in_lambda(body: &LambdaBody) -> Vec<&Expr> {
    struct Mentions<'e>(Vec<&'e Expr>);

    impl<'e> BodyVisit<'e> for Mentions<'e> {
        fn expr(&mut self, e: &'e Expr, locals: &std::collections::HashSet<String>) -> bool {
            if let Expr::Var { name, .. } = e {
                if !locals.contains(place_base(name)) {
                    self.0.push(e);
                }
            }
            true
        }
    }

    let mut v = Mentions(Vec::new());
    let mut locals = std::collections::HashSet::new();
    match body {
        LambdaBody::Expr(e) => body_expr(e, &locals, &mut v),
        LambdaBody::Block(b) => body_block(b, &mut locals, &mut v),
    }
    v.0
}

thread_local! {
    static REFUSALS: std::cell::RefCell<Vec<crate::kernel::Refusal>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static FACTS: std::cell::RefCell<Option<Facts>> = const { std::cell::RefCell::new(None) };
    /// The core's bodies for the program last analysed on this thread, by the
    /// name each is emitted under: what an emitter walks in place of the
    /// source ([`Facts`] answers per node instead). `None` under a name two
    /// bodies share.
    static BODIES: std::cell::RefCell<HashMap<String, Option<Body>>> =
        std::cell::RefCell::new(HashMap::new());
    static PLACED: std::cell::RefCell<Placed> = std::cell::RefCell::new(Placed::default());
    /// What the checker decided about a program, under [`Key`]. Held as the
    /// checker's `Rc`, so serving it costs a refcount, not a copy of a map
    /// with a row per node.
    #[allow(clippy::type_complexity)]
    static DECIDED: std::cell::RefCell<
        Option<(Key, std::rc::Rc<vyrn_frontend::checker::Recorded>)>,
    > = const { std::cell::RefCell::new(None) };
}

/// Holds what the checker decided about `program` for the emitters to read by
/// node. Every route to an emitter calls this first, so one record serves the
/// lowering and both backends. Its types are as the checker wrote them: a
/// reader in a monomorphized body substitutes its own instantiation.
///
/// Where the held record is another program's, this makes a new one rather
/// than serve answers off colliding addresses; see [`Decided`].
#[must_use = "the record is held only while the guard is alive"]
pub fn decide(program: &Program) -> Decided {
    let key = key_of(program);
    if DECIDED.with(|d| d.borrow().as_ref().is_some_and(|(k, _)| *k == key)) {
        return Decided(None);
    }
    let made = vyrn_frontend::checker::recorded(program);
    let prev = DECIDED.with(|d| d.borrow_mut().replace((key, made)));
    Decided(Some(prev))
}

/// The guard [`decide`] returns: the record stays held while it lives.
///
/// The key is an address, and two programs built one after another can land
/// at the same address with the same shape (`vyrn-codegen`'s tests do), so a
/// record this made is put back as it was found on drop. A record it only
/// borrowed (the lowering's, for the same program) is left alone.
pub struct Decided(Option<Option<(Key, std::rc::Rc<vyrn_frontend::checker::Recorded>)>>);

impl Drop for Decided {
    fn drop(&mut self) {
        if let Some(prev) = self.0.take() {
            DECIDED.with(|d| *d.borrow_mut() = prev);
        }
    }
}

/// What a held record belongs to: the program, and the two contexts a check of
/// it depends on (`vyrn_frontend::checker::gen_host` and `test_host`).
type Key = (usize, bool, bool);

fn key_of(program: &Program) -> Key {
    (
        program as *const Program as usize,
        vyrn_frontend::checker::gen_host(),
        vyrn_frontend::checker::test_host(),
    )
}

/// Holds `made` as the record for `program`, unconditionally.
/// [`crate::lower_with`] calls it after its own check: a program extended
/// since the last lowering has the same address and different nodes.
pub fn set_decided(program: &Program, made: &std::rc::Rc<vyrn_frontend::checker::Recorded>) {
    let key = key_of(program);
    DECIDED.with(|d| *d.borrow_mut() = Some((key, made.clone())));
}

/// The checker's type for the expression at `node`.
///
/// `None` for a node the checker never typed: one of a program no lowering
/// ran over on this thread (a host that never linked this crate), or an
/// expression an emitter built. A reader then falls back to its own derivation.
pub fn node_ty(node: usize) -> Option<Type> {
    DECIDED.with(|d| {
        d.borrow()
            .as_ref()
            .and_then(|(_, r)| r.node_types.get(&node).cloned())
    })
}

/// The type arguments the checker solved at the call `node`, as it typed the
/// body: before an instance's substitution.
fn node_solved(node: usize) -> Option<Vec<(String, Type)>> {
    DECIDED.with(|d| {
        d.borrow()
            .as_ref()
            .and_then(|(_, r)| r.node_substs.get(&node).map(|(_, s)| s.clone()))
    })
}

/// The declaration the checker recorded at the call `node` it typed `Err`
/// for the typed judgment ([`vyrn_frontend::checker::Recorded::calls`]).
fn call_decl(node: usize) -> Option<vyrn_frontend::checker::CallDecl> {
    DECIDED.with(|d| {
        d.borrow()
            .as_ref()
            .and_then(|(_, r)| r.calls.get(&node).cloned())
    })
}

/// The checker's type for the `match` or `if` expression at `node`: the join
/// subset of [`node_ty`]. A merge holds one value, so an emitter must not
/// take the type one arm happened to produce (`["z"]` for `Array<String>`).
pub fn join_ty(node: usize) -> Option<Type> {
    DECIDED.with(|d| {
        d.borrow()
            .as_ref()
            .and_then(|(_, r)| r.joins.get(&node).cloned())
    })
}

/// The core's statements folded into side tables keyed by AST node, as the
/// plan keys them, so an emitter walking the AST can look them up. Built once
/// per compile; `compiler/vyrn-cli/tests/coretables.rs` counts every row over
/// the corpus.
#[derive(Default, Clone, Debug)]
pub struct Facts {
    /// The `Expr::Field` node of an unnamed receiver the core releases after
    /// the read, and the holes the release walks around.
    pub receivers: std::collections::HashMap<usize, Vec<String>>,
    /// `(match, arm) -> [(binder, holes, kind)]`: the payload binders the
    /// arm's body releases at its end. The kind is the binder type's release
    /// rule, which the interpreter needs; the compiled backends read the type.
    pub arms: std::collections::HashMap<(usize, u32), Vec<(String, Vec<String>, Option<DropKind>)>>,
    /// The store statements and whether each releases its old value: a
    /// `St::Store`'s `releases` at a [`Site::Node`]. An absent site has no
    /// answer here, and a reader falls back to the plan.
    pub stores: std::collections::HashMap<usize, bool>,
    /// The stores the core stands down at whatever the judgment says: the
    /// value hands the place back (`xs = xs.push(v)`), or the place owns no
    /// heap (`w = 2`). No emitter reads it (`stores` folds both in); the
    /// corpus test uses it to check that every plan row is either released
    /// or stood down.
    pub stood_down: std::collections::HashSet<usize>,
    /// The statement-position calls whose unbound owned result the core
    /// releases: a `St::Drop` at the statement's [`Site::Node`].
    pub discarded: std::collections::HashSet<usize>,
    /// The `for x in consume xs` loops that release their container where the
    /// loop ends: a `St::Drop` of a `for_consume` name at the loop's
    /// [`Site::Node`]. No row names this release, and it excludes an exit row
    /// for the container.
    pub loop_gives_back: std::collections::HashSet<usize>,
    /// The `for` statements whose container release frees the buffer alone
    /// ([`Body::loop_buffers`]), keyed by the loop's node. The release is a
    /// placed row at the loop, or [`Facts::loop_gives_back`].
    pub loop_buffer_only: std::collections::HashSet<usize>,
    /// The call-argument nodes whose temporary the caller releases after the
    /// call ([`NameInfo::arg_drop`]).
    pub arg_drops: std::collections::HashSet<usize>,
    /// Per join node, the `(name, edge, holes)` releases one edge owes
    /// because another edge took the name: a `St::Drop` at
    /// a [`Site::Edge`].
    pub edges: std::collections::HashMap<usize, Vec<EdgeRow>>,
    /// The receivers a callee allocated ([`NameInfo::receiver_malloc`]). An
    /// emitter still asks its own region depth.
    pub receiver_malloc: std::collections::HashSet<usize>,
    /// Per `match`, `if let` or `?` node: whether the construct took its
    /// scrutinee ([`St::Switch`]'s `consuming`). An absent site has no
    /// answer here.
    pub consuming: std::collections::HashMap<usize, bool>,
    /// The `match`, `if let` or `?` nodes that own the boxes their binders
    /// come out of ([`St::Switch`]'s `owns`, [`Builder::owns_boxes`]).
    pub owns_scrutinee: std::collections::HashSet<usize>,
}

/// One edge release: the name, the edge, and the holes the release walks
/// around, spelled relative to the name (`Elem.1`).
pub type EdgeRow = (String, u32, Vec<String>);

/// What the kernel decided over the core's first build
/// ([`crate::kernel::placement`]), which the second build writes down. The
/// first build states no release the kernel has not judged owed, so the
/// judgment reports every one.
#[derive(Default, Clone, Debug)]
pub(crate) struct Placed {
    /// `(switch site, arm) -> [(binder, holes)]`: the payload binders still
    /// held where their arm ends.
    arms: std::collections::HashMap<(usize, u32), Vec<(String, Vec<String>)>>,
    /// Per join node, the releases one edge owes because another edge took
    /// the name. A sub-place row is spelled `d.line`.
    edges: std::collections::HashMap<usize, Vec<EdgeRow>>,
    /// The store nodes the kernel found a held place at: the stores that
    /// release what they displace, with the holes each release walks around.
    stores: std::collections::HashMap<usize, Vec<String>>,
    /// The nodes that produced a borrowed receiver still held, whose free
    /// rides as an argument-temporary drop.
    producers: std::collections::HashSet<usize>,
}

/// Whether the kernel found this store's place still holding. Empty on the
/// first build, where every store says [`Old::Pending`].
fn placed_store(site: usize) -> bool {
    PLACED.with(|p| p.borrow().stores.contains_key(&site))
}

/// The holes the release at a placed store walks around.
fn store_holes(site: usize) -> Vec<String> {
    PLACED.with(|p| p.borrow().stores.get(&site).cloned().unwrap_or_default())
}

/// The binders the kernel found held at the end of one arm. Empty on the
/// first build.
fn placed_arm(site: usize, arm: u32) -> Option<Vec<(String, Vec<String>)>> {
    PLACED.with(|p| p.borrow().arms.get(&(site, arm)).cloned())
}

/// Whether the placer wrote an argument-temporary drop for the receiver this
/// node produced. Empty on the first build.
fn placed_producer(node: usize) -> bool {
    PLACED.with(|p| p.borrow().producers.contains(&node))
}

/// Rule N's rows for one join, as the kernel equalized its edges.
fn placed_edges(join: usize) -> Option<Vec<EdgeRow>> {
    PLACED.with(|p| p.borrow().edges.get(&join).cloned())
}

/// The core's answers for the program last analysed on this thread. `None`
/// in a host that never installed the placer; an emitter then reads the
/// plan.
pub fn facts() -> Option<Facts> {
    FACTS.with(|f| f.borrow().clone())
}

/// The name a lambda literal at `line` and `col` inside the body named
/// `outer` is built and emitted under, and so its key in [`body_of`].
pub fn lambda_spelling(outer: &str, line: usize, col: usize) -> String {
    format!("{outer}@lambda:{line}:{col}")
}

/// The line of a lambda key [`lambda_spelling`] spelled, which is how
/// a lambda source is named.
pub fn lambda_line(name: &str) -> Option<usize> {
    let (_, at) = name.rsplit_once("@lambda:")?;
    at.split(':').next()?.parse().ok()
}

/// The core's body for the function emitted under `name`: [`crate::spell`] of
/// the instance (`max<Int64>`, `main@lambda:26:13`, `test@1`, or the empty
/// name for module state). `None` for a [`Gap`] and for a name two bodies
/// share; a reader then walks the source.
pub fn body_of(name: &str) -> Option<Body> {
    BODIES.with(|b| b.borrow().get(name).cloned().flatten())
}

/// The instance of `body` whose `fn`-typed parameters are bound:
/// each parameter in `bound` leaves the parameter list, a call through it is
/// [`Callee::Fn`] to its target, and a call that passes it on names that
/// target. A lambda target's captures take the parameter's place; a call
/// through it passes them first, and a call passing it on takes them after
/// its own arguments (`direct::ho_args`). A bound parameter read once as a
/// value is made where it is read: its target's closure variant. `None`
/// where one is read twice, or a pass-through names no target.
pub fn specialize(body: &Body, bound: &[(Name, Target)]) -> Option<Body> {
    if bound.iter().any(|(_, t)| matches!(t, Target::Param(_))) {
        return None;
    }
    let mut out = body.clone();
    let values: Vec<Name> = (bound.iter())
        .filter_map(|(n, t)| match t {
            Target::Value(source) => {
                out.names[*n as usize].source = source.clone();
                Some(*n)
            }
            _ => None,
        })
        .collect();
    let mut caps: Vec<(Name, Vec<Name>)> = Vec::new();
    for (n, t) in bound {
        let Target::Lambda(_, cs, _) = t else {
            continue;
        };
        let names = (cs.iter())
            .map(|(source, ty)| {
                let mut info = out.names[*n as usize].clone();
                info.source = source.clone();
                info.ty = ty.clone();
                out.names.push(info);
                (out.names.len() - 1) as Name
            })
            .collect();
        caps.push((*n, names));
    }
    bind_targets(&mut out.stmts, bound, &caps);
    let mut reads = vec![0; out.names.len()];
    count_reads(&out.stmts, &mut reads);
    let gone = |n: &Name| !values.contains(n);
    for (n, t) in bound
        .iter()
        .filter(|(n, _)| gone(n) && reads[*n as usize] > 0)
    {
        let parts = (caps.iter().find(|(c, _)| c == n))
            .map(|(_, ns)| ns.iter().map(|c| Val::Name(*c)).collect())
            .unwrap_or_default();
        // A target with captures makes a value that owns its capture box, and
        // the lambda that captures it holds a copy, so it is released after.
        let release = !Vec::is_empty(&parts);
        if release {
            let info = &mut out.names[*n as usize];
            (info.releases, info.heap, info.borrow, info.borrow_kind) = (true, true, false, None);
        }
        let made = St::Let(*n, Rhs::Make(Ctor::Closure(t.clone()), parts));
        if reads[*n as usize] > 1
            || !make_before_read(&mut out.stmts, reads.len(), *n, made, release)
        {
            return None;
        }
    }
    out.params = (out.params.iter())
        .flat_map(|p| match caps.iter().find(|(n, _)| n == p) {
            Some((_, names)) => names.clone(),
            None if gone(p) && bound.iter().any(|(n, _)| n == p) => Vec::new(),
            None => vec![*p],
        })
        .collect();
    Some(out)
}

/// Puts `made`, the row that binds `n`, before the one row that reads `n`, at
/// that row's depth, and with `release` a release of `n` after it. `names` is
/// the body's name count. `false` where no row reads it, and where `release`
/// and the read is no lambda's capture, which copies what it reads.
fn make_before_read(ss: &mut Vec<St>, names: usize, n: Name, made: St, release: bool) -> bool {
    let mut reads = vec![0; names];
    for i in 0..ss.len() {
        let nested = match &mut ss[i] {
            St::If { then, els, .. } => vec![then, els],
            St::Loop { body, .. } | St::Block { body, .. } => vec![body],
            St::Switch { arms, .. } => arms.iter_mut().map(|a| &mut a.body).collect(),
            _ => Vec::new(),
        };
        let mut inner = 0;
        for b in &nested {
            count_reads(b, &mut reads);
            inner += std::mem::take(&mut reads[n as usize]);
        }
        if inner > 0 {
            return nested
                .into_iter()
                .find(|b| {
                    count_reads(b, &mut reads);
                    std::mem::take(&mut reads[n as usize]) > 0
                })
                .is_some_and(|b| make_before_read(b, names, n, made, release));
        }
        count_reads(std::slice::from_ref(&ss[i]), &mut reads);
        if reads[n as usize] > 0 {
            if release {
                if !matches!(ss[i], St::Let(_, Rhs::Prim(Op::Closure(_), ..))) {
                    return false;
                }
                ss.insert(i + 1, St::Drop(n, Site::None, 0, None));
            }
            ss.insert(i, made);
            return true;
        }
    }
    false
}

fn bind_targets(ss: &mut [St], bound: &[(Name, Target)], caps: &[(Name, Vec<Name>)]) {
    for s in ss {
        match s {
            St::Let(_, rhs) | St::Do { rhs, .. } => {
                let Rhs::Call {
                    callee,
                    args,
                    kind,
                    targets,
                    ..
                } = rhs
                else {
                    continue;
                };
                if let Some(v) = kind.value() {
                    match bound.iter().find(|(n, _)| *n == v) {
                        Some((_, Target::Fn(f))) => {
                            *kind = Callee::Fn;
                            *callee = f.clone();
                        }
                        Some((_, Target::Lambda(key, ..))) => {
                            *kind = Callee::Fn;
                            *callee = key.clone();
                            let names = caps.iter().find(|(n, _)| *n == v).map(|(_, ns)| ns);
                            let lead = names.into_iter().flatten();
                            let lead = lead.map(|c| (Arg::Val(Val::Name(*c)), Capability::Read));
                            args.splice(0..0, lead.collect::<Vec<_>>());
                        }
                        Some((_, Target::Value(source))) => *callee = source.clone(),
                        _ => {}
                    }
                }
                for t in targets.iter_mut() {
                    let Target::Param(p) = t else { continue };
                    let p = *p;
                    let Some((_, to)) = bound.iter().find(|(n, _)| *n == p) else {
                        continue;
                    };
                    *t = to.clone();
                    // A stored value forwards itself, the parameter that
                    // stays under its name.
                    if matches!(to, Target::Value(_)) {
                        args.push((Arg::Val(Val::Name(p)), Capability::Read));
                    }
                    let forwarded = caps.iter().find(|(n, _)| *n == p).map(|(_, ns)| ns);
                    let forwarded = forwarded.into_iter().flatten();
                    args.extend(forwarded.map(|c| (Arg::Val(Val::Name(*c)), Capability::Read)));
                }
            }
            St::If { then, els, .. } => {
                bind_targets(then, bound, caps);
                bind_targets(els, bound, caps);
            }
            St::Loop { body, .. } | St::Block { body, .. } => bind_targets(body, bound, caps),
            St::Switch { arms, .. } => {
                for a in arms {
                    bind_targets(&mut a.body, bound, caps);
                }
            }
            _ => {}
        }
    }
}

fn count_reads(ss: &[St], out: &mut [u32]) {
    fn hit(v: &Val, out: &mut [u32]) {
        if let Val::Name(n) = v {
            out[*n as usize] += 1;
        }
    }
    for s in ss {
        match s {
            St::Let(_, rhs) | St::Do { rhs, .. } => match rhs {
                Rhs::Val(v) => hit(v, out),
                Rhs::Prim(_, vs, _) | Rhs::Make(_, vs) => {
                    vs.iter().for_each(|v| hit(v, out));
                }
                Rhs::Call { args, kind, .. } => {
                    kind.value().iter().for_each(|f| hit(&Val::Name(*f), out));
                    args.iter().for_each(|(a, _)| {
                        if let Arg::Val(v) = a {
                            hit(v, out)
                        }
                    })
                }
                Rhs::Read(_) | Rhs::Take(_) => {}
            },
            St::Store { value, .. } => hit(value, out),
            St::Return { value: Some(v), .. } => hit(v, out),
            St::If { cond, .. } => hit(cond, out),
            St::Switch { on, arms, .. } => {
                hit(on, out);
                arms.iter()
                    .filter_map(|a| a.test.reads())
                    .for_each(|n| out[n as usize] += 1);
            }
            // A release reads the name, so the name holds a place until then.
            St::Drop(n, ..) => out[*n as usize] += 1,
            _ => {}
        }
        match s {
            St::If { then, els, .. } => {
                count_reads(then, out);
                count_reads(els, out);
            }
            St::Loop { body: b, .. } | St::Block { body: b, .. } => count_reads(b, out),
            St::Switch { arms, .. } => {
                for a in arms {
                    count_reads(&a.body, out);
                }
            }
            _ => {}
        }
    }
}

/// Every name a statement names, itself and everything under it: what it binds
/// and what it reads.
pub fn names_in(s: &St, out: &mut Vec<Name>) {
    match s {
        St::Let(n, rhs) => {
            out.push(*n);
            names_in_rhs(rhs, out);
        }
        St::Do { rhs, .. } => names_in_rhs(rhs, out),
        St::Store { place, value, .. } => {
            names_in_place(place, out);
            names_in_val(value, out);
        }
        St::Drop(n, ..) | St::Row { name: n, .. } => out.push(*n),
        St::If {
            cond, then, els, ..
        } => {
            names_in_val(cond, out);
            then.iter().for_each(|s| names_in(s, out));
            els.iter().for_each(|s| names_in(s, out));
        }
        St::Loop { body: b, .. } | St::Block { body: b, .. } => {
            b.iter().for_each(|s| names_in(s, out))
        }
        St::Switch { on, arms, .. } => {
            names_in_val(on, out);
            for a in arms {
                out.extend(a.test.reads());
                a.body.iter().for_each(|s| names_in(s, out));
            }
        }
        St::Return { value: Some(v), .. } => names_in_val(v, out),
        St::Return { .. } | St::Break { .. } | St::Continue { .. } | St::Trap => {}
    }
}

/// Each name a `let` of `ss` binds, at the row of `ss` its extent ends at.
///
/// A name holds its value from its `let` to the last row that names it,
/// itself or under it. A release names what it releases, so a name that owns
/// heap holds to its release. A name read out of a place may hold the
/// place's address, so the place's root holds as long as the name. A name
/// that `occurs` (from [`Body::occurrences`]) counts outside `ss` ends at no
/// row of `ss`.
pub fn extent_ends(ss: &[St], occurs: &[u32]) -> Vec<Vec<Name>> {
    let mut seen: HashMap<Name, (usize, u32)> = HashMap::new();
    let mut ns = Vec::new();
    for (i, s) in ss.iter().enumerate() {
        ns.clear();
        names_in(s, &mut ns);
        for &n in &ns {
            let e = seen.entry(n).or_insert((i, 0));
            *e = (i, e.1 + 1);
        }
    }
    let mut end: HashMap<Name, Option<usize>> = seen
        .iter()
        .map(|(&n, &(i, k))| (n, (k == occurs[n as usize]).then_some(i)))
        .collect();
    for s in ss.iter().rev() {
        if let St::Let(n, Rhs::Read(p)) = s {
            let held = end.get(n).copied().flatten();
            if let Some(Val::Name(r)) = root_name(p) {
                if let Some(e) = end.get_mut(&r) {
                    *e = e.zip(held).map(|(a, b)| a.max(b));
                }
            }
        }
    }
    let mut out = vec![Vec::new(); ss.len()];
    for s in ss {
        if let St::Let(n, _) = s {
            if let Some(i) = end.get(n).copied().flatten() {
                out[i].push(*n);
            }
        }
    }
    out
}

/// The names and the module state whose header a read in `s` walks: an
/// element read, or a length read, straight off one. With `rebase`, each
/// such read of the first reads the name instead. A store and a take keep
/// their place, so a store into an element writes the container and not its
/// header.
fn header_reads(s: &mut St, rebase: Option<(&Root, Name)>, out: &mut Vec<Root>) {
    fn place(p: &mut Place, rebase: Option<(&Root, Name)>, out: &mut Vec<Root>) {
        let header = match p {
            Place::Elem(b, _) => Some(b),
            Place::Field(b, f) if f == "length" || f == "byteLength" => Some(b),
            _ => None,
        };
        if let Some(b) = header {
            let r = match &**b {
                Place::Name(n) => Some(Root::N(*n)),
                Place::Global(g) => Some(Root::G(g.clone())),
                _ => None,
            };
            if let Some(r) = r {
                match rebase {
                    Some((from, to)) if *from == r => **b = Place::Name(to),
                    Some(_) => {}
                    None => out.push(r),
                }
                return;
            }
        }
        match p {
            Place::Field(b, _) | Place::Elem(b, _) | Place::Key(b, _) => place(b, rebase, out),
            Place::Name(_) | Place::Global(_) => {}
        }
    }
    let each = |ss: &mut Vec<St>, out: &mut Vec<Root>| {
        ss.iter_mut().for_each(|s| header_reads(s, rebase, out))
    };
    match s {
        St::Let(_, Rhs::Read(p))
        | St::Do {
            rhs: Rhs::Read(p), ..
        } => place(p, rebase, out),
        St::If { then, els, .. } => {
            each(then, out);
            each(els, out);
        }
        St::Loop { body, .. } | St::Block { body, .. } => each(body, out),
        St::Switch { arms, .. } => arms.iter_mut().for_each(|a| each(&mut a.body, out)),
        _ => {}
    }
}

/// Every name `s` binds, at any depth: a `let` and a switch arm's binders.
pub fn names_bound(s: &St, out: &mut Vec<Name>) {
    match s {
        St::Let(n, _) => out.push(*n),
        St::If { then, els, .. } => {
            then.iter().for_each(|s| names_bound(s, out));
            els.iter().for_each(|s| names_bound(s, out));
        }
        St::Loop { body: b, .. } | St::Block { body: b, .. } => {
            b.iter().for_each(|s| names_bound(s, out))
        }
        St::Switch { arms, .. } => {
            for a in arms {
                out.extend(&a.binds);
                a.body.iter().for_each(|s| names_bound(s, out));
            }
        }
        _ => {}
    }
}

/// Whether `s` moves a place out into a window's temp.
fn moves_out(s: &Stmt) -> bool {
    matches!(s, Stmt::Let { name, mutable: true, .. } if vyrn_frontend::ast::is_place_temp(name))
}

/// The receiver of a removal the parser brackets with a move-out window
/// (`parser::hoist_mutating_receiver`), where `e` is one.
fn removal(e: &Expr) -> Option<&String> {
    match e {
        Expr::Call { name, args, .. }
            if matches!(name.as_str(), "@pop" | "@swapRemove" | "@remove") =>
        {
            match args.first() {
                Some(Expr::Var { name, .. }) => Some(name),
                _ => None,
            }
        }
        _ => None,
    }
}

fn names_in_rhs(r: &Rhs, out: &mut Vec<Name>) {
    match r {
        Rhs::Val(v) => names_in_val(v, out),
        Rhs::Prim(_, vs, _) | Rhs::Make(_, vs) => vs.iter().for_each(|v| names_in_val(v, out)),
        Rhs::Call { args, kind, .. } => {
            out.extend(kind.value());
            args.iter().for_each(|(a, _)| match a {
                Arg::Val(v) => names_in_val(v, out),
                Arg::Place(p) => names_in_place(p, out),
            })
        }
        Rhs::Read(p) | Rhs::Take(p) => names_in_place(p, out),
    }
}

fn names_in_val(v: &Val, out: &mut Vec<Name>) {
    if let Val::Name(n) = v {
        out.push(*n);
    }
}

fn names_in_place(p: &Place, out: &mut Vec<Name>) {
    match p {
        Place::Name(n) => out.push(*n),
        Place::Global(_) => {}
        Place::Field(b, _) => names_in_place(b, out),
        Place::Elem(b, v) | Place::Key(b, v) => {
            names_in_place(b, out);
            names_in_val(v, out);
        }
    }
}

/// Every name a statement of `b` stores into whole (`x = v`), in nested
/// blocks and statement-position `match` arms too. A rebuild's write-back
/// (`xs.push(v)`, `xs = push(xs, v)`) is no such store: it hands the receiver
/// back, and the kernel refuses it on a borrow.
fn rebound(b: &Block, out: &mut std::collections::HashSet<String>) {
    for s in &b.stmts {
        match s {
            Stmt::Assign {
                value: Expr::Call { name: f, args, .. },
                name,
                ..
            } if vyrn_frontend::prelude::rebuilds(f)
                && matches!(args.first(), Some(Expr::Var { name: r, .. }) if r == name) => {}
            Stmt::Assign { name, .. } => {
                out.insert(name.clone());
            }
            Stmt::If {
                then_block,
                else_block,
                ..
            }
            | Stmt::IfLet {
                then_block,
                else_block,
                ..
            } => {
                rebound(then_block, out);
                if let Some(e) = else_block {
                    rebound(e, out);
                }
            }
            Stmt::While { body, .. } | Stmt::ForIn { body, .. } | Stmt::Region { body, .. } => {
                rebound(body, out)
            }
            Stmt::Expr(Expr::Match { arms, .. }) => {
                for a in arms {
                    if let ArmBody::Block(b) = &a.body {
                        rebound(b, out);
                    }
                }
            }
            _ => {}
        }
    }
}

/// The kernel spells a hole `.f.g`; every table spells it `f.g`, relative to
/// the binding.
fn plan_holes(holes: &[String]) -> Vec<String> {
    holes
        .iter()
        .map(|h| h.trim_start_matches('.').to_string())
        .collect()
}

/// Folds one frame's statements into the side table. Called after the placer
/// has added every row, so this is the core the emitters run.
fn fold_facts(body: &Body, proto: &Owned, stmts: &[St], out: &mut Facts) {
    for s in stmts {
        match s {
            St::If { then, els, .. } => {
                fold_facts(body, proto, then, out);
                fold_facts(body, proto, els, out);
            }
            St::Loop { body: b, .. } | St::Block { body: b, .. } => fold_facts(body, proto, b, out),
            St::Store {
                releases,
                site: Site::Node(at),
                old,
                ..
            } => {
                out.stores.insert(*at, *releases);
                if matches!(old, Old::Transferred | Old::Nothing) {
                    out.stood_down.insert(*at);
                }
            }
            St::Drop(_, _, line, _) if *line > 0 => {}
            St::Drop(n, at, _, holes) => match at {
                Site::Node(at) => {
                    if body.names[*n as usize].for_consume {
                        out.loop_gives_back.insert(*at);
                    } else {
                        out.discarded.insert(*at);
                    }
                }
                Site::Edge(join, edge) => {
                    let name = body.names[*n as usize].source.clone();
                    let holes = plan_holes(body.drop_holes(*n, holes));
                    let rows = out.edges.entry(*join).or_default();
                    // One row per name and edge: a generic instantiated twice
                    // folds the same join twice when the two share a node.
                    if !rows.iter().any(|(r, e, _)| *r == name && e == edge) {
                        rows.push((name, *edge, holes));
                    }
                }
                Site::None => {}
            },
            St::Switch {
                arms,
                consuming: took,
                owns,
                ..
            } => {
                if let Some(a) = arms.first() {
                    out.consuming.insert(a.site, *took);
                    if *owns {
                        out.owns_scrutinee.insert(a.site);
                    }
                }
                for a in arms {
                    if let Some(frees) = &a.frees {
                        let rows: Vec<(String, Vec<String>, Option<DropKind>)> = frees
                            .iter()
                            .map(|b| {
                                let info = &body.names[*b as usize];
                                (
                                    info.source.clone(),
                                    plan_holes(&info.holes),
                                    proto.release_kind(&info.ty),
                                )
                            })
                            .collect();
                        // An entry even when empty: "owes none" differs from
                        // "not stated".
                        out.arms.entry((a.site, a.index)).or_default().extend(rows);
                    }
                    fold_facts(body, proto, &a.body, out);
                }
            }
            _ => {}
        }
    }
}

/// Every frame's answers, added to the table.
fn fold_frame(body: &Body, proto: &Owned, out: &mut Facts) {
    // Filled at the same site as the fold, so a body the fold does not see is
    // one no emitter may walk either.
    BODIES.with(|b| {
        b.borrow_mut()
            .entry(body.name.clone())
            .and_modify(|had| *had = None)
            .or_insert_with(|| Some(body.clone()));
    });
    fold_facts(body, proto, &body.stmts, out);
    out.loop_buffer_only
        .extend(body.loop_buffers.iter().copied());
    let mut released = std::collections::HashSet::new();
    collect_drops(&body.stmts, &mut released);
    for (i, info) in body.names.iter().enumerate() {
        if !released.contains(&(i as Name)) {
            continue;
        }
        if let Some(node) = info.receiver {
            out.receivers.insert(node, plan_holes(&info.holes));
            if info.receiver_malloc {
                out.receiver_malloc.insert(node);
            }
        }
    }
    for info in body.names.iter() {
        // Whether or not this pass releases the temporary: a `lazy` field read
        // binds a borrow, and the caller still frees the value.
        if let Some(node) = info.arg_drop {
            out.arg_drops.insert(node);
        }
    }
}

fn collect_drops(stmts: &[St], out: &mut std::collections::HashSet<Name>) {
    for s in stmts {
        match s {
            St::Drop(n, _, _, _) => {
                out.insert(*n);
            }
            St::If { then, els, .. } => {
                collect_drops(then, out);
                collect_drops(els, out);
            }
            St::Loop { body: b, .. } | St::Block { body: b, .. } => collect_drops(b, out),
            St::Switch { arms, .. } => {
                for a in arms {
                    collect_drops(&a.body, out);
                }
            }
            _ => {}
        }
    }
}

/// Whether the kernel's hard refusals fail the command: true unless
/// `VYRN_NO_KERNEL=1`. The knob is for bisecting where a refusal came from;
/// nothing is built under it.
pub fn refuses() -> bool {
    !std::env::var("VYRN_NO_KERNEL").is_ok_and(|v| v == "1")
}

/// The hard refusals the placer met since the last call, on this thread: a
/// double free, a use after release, a join whose edges disagree, and a rule
/// the core states about a construct it does lower.
///
/// A gap ([`Gap`] with no `rule`) is not collected here: the core has no
/// opinion, and the command goes on with the plan. A refusal is an answer no
/// placement repairs, and only refusals fail a command.
pub fn take_refusals() -> Vec<crate::kernel::Refusal> {
    REFUSALS.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

type Typed = (
    Vec<vyrn_frontend::diagnostics::Diagnostic>,
    std::collections::HashSet<usize>,
);

thread_local! {
    /// What the typed judgment refused about the program last analysed on
    /// this thread, as `vyrn check` words it, and the statements it refused.
    static TYPED: std::cell::RefCell<Typed> = std::cell::RefCell::default();
}

/// Judges one built body with the typed judgment and answers whether it
/// refused. The judgment memo serves only the kernel's refusals, so a refused
/// body is built and judged again next time. `as_written` is false for an
/// instance of a generic function, whose types are the instance's.
fn typed(program: &Program, top: &Body, file: &Option<String>, as_written: bool) -> bool {
    let global_mutable = |g: &str| program.globals.iter().any(|d| d.name == g && d.mutable);
    let projected =
        |t: &Type| vyrn_frontend::project::lookup_in(&program.impls, t, "atSet").is_some();
    TYPED.with(|t| {
        let (out, seen) = &mut *t.borrow_mut();
        let mut found = crate::typed::stores(top, &global_mutable, &projected, seen);
        found.extend(crate::typed::loops(top, seen));
        // One sentence per line: a declaration's predicate is also the body
        // of its constructor.
        for u in crate::typed::refused(top, as_written) {
            let said = |d: &vyrn_frontend::diagnostics::Diagnostic| {
                (&d.file, d.line, &d.message) == (file, u.0, &u.1)
            };
            if !out.iter().any(said) && !found.contains(&u) {
                found.push(u);
            }
        }
        if as_written {
            found.extend(crate::typed::drops(top, program));
        }
        found.sort_by_key(|(line, _)| *line);
        let refused = !found.is_empty();
        out.extend(
            found
                .into_iter()
                .map(|(line, message)| diagnostic(line, message, file)),
        );
        refused
    })
}

fn diagnostic(
    line: usize,
    message: String,
    file: &Option<String>,
) -> vyrn_frontend::diagnostics::Diagnostic {
    let mut d = vyrn_frontend::diagnostics::Diagnostic::error(line, 0, "check", message);
    d.file = file.clone();
    d
}

/// The typed judgment's refusals, drained. Installed into `own`'s slot by
/// [`crate::install`].
pub fn typed_diagnostics() -> Vec<vyrn_frontend::diagnostics::Diagnostic> {
    TYPED.with(|t| std::mem::take(&mut *t.borrow_mut()).0)
}

/// The kernel's refusals as `movecheck`-stage diagnostics, deduplicated, for
/// the one list a file's refusals come out in
/// (`vyrn_frontend::movecheck::refusals`). Installed into `own::analyze`'s
/// slot by [`crate::install`]; the caller orders the list.
///
/// Several instances of one generic body reach the same rule, and a reader
/// is owed one sentence per mistake, so file, line and message are the
/// identity, with the count of that sentence within one body's run:
/// `out.push(s) out.push(s)` on one line is two mistakes. `file` is `None`
/// for the root module, which tells `vyrn fix` the edit is its to make.
pub fn refusal_diagnostics() -> Vec<vyrn_frontend::diagnostics::Diagnostic> {
    if !refuses() {
        let _ = take_refusals();
        return Vec::new();
    }
    let mut seen = std::collections::HashSet::new();
    let mut body = String::new();
    let mut nth: std::collections::HashMap<(Option<String>, usize, String), usize> =
        Default::default();
    take_refusals()
        .into_iter()
        .filter(|r| {
            if r.body != body {
                body = r.body.clone();
                nth.clear();
            }
            let key = (r.file.clone(), r.line, r.message.clone());
            let n = nth.entry(key.clone()).or_default();
            *n += 1;
            seen.insert((key, *n))
        })
        .map(|r| {
            let mut d =
                vyrn_frontend::diagnostics::Diagnostic::error(r.line, 0, "movecheck", r.message);
            d.file = r.file;
            d
        })
        .collect()
}

/// Reports a body the core did not build. A gap with a rule is the program's
/// refusal, in the checker's sentence. A gap without one is a defect in the
/// builder: the checker typed the body, so every judgment over the core
/// would otherwise pass over it in silence.
fn refuse_gap(g: Gap, file: &Option<String>, body: &str) {
    let Some(message) = g.rule else {
        let detail = if g.detail.is_empty() {
            String::new()
        } else {
            format!(" `{}`", g.detail)
        };
        let message = format!(
            "internal error: the core cannot state {}{detail}, so `{body}` is not judged",
            g.what
        );
        // The typed judgment's list prints whichever pass refused.
        TYPED.with(|t| t.borrow_mut().0.push(diagnostic(g.line, message, file)));
        return;
    };
    REFUSALS.with(|v| {
        v.borrow_mut().push(crate::kernel::Refusal {
            message,
            line: g.line,
            file: file.clone(),
            body: body.to_string(),
        })
    });
}

/// Places the releases the plan did not place. For every body the core can
/// build, the kernel walks it in placement mode: where an owned name is still
/// held at an exit (a block's end, a `return`, a `?`, a `break`, a
/// `continue`) and the plan placed no release, a row is added at that exit,
/// keyed as the plan keys its own, and the binding enters the plan's
/// droppable table. The core orders across a loop's back edge, which the
/// plan's fold cannot.
///
/// Installed into `own::analyze` by [`crate::install`], so every consumer of
/// the plan sees the same rows. A body the core cannot build, or the kernel
/// refuses for another reason (a double free, a use after release), is left
/// as the plan had it.
pub fn augment(program: &Program, own: &mut Ownership) {
    let _p = vyrn_frontend::prof::phase("placer");
    // A node is an address the allocator reuses: a row placed for the last
    // program must not fire on this one.
    PLACED.with(|p| *p.borrow_mut() = Placed::default());
    let lw = vyrn_frontend::prof::phase("placer: lower_with");
    let lowered = crate::lower_with(program, own);
    drop(lw);
    // `VYRN_KERNEL_TRACE=1` prints every release the placer found owed, and
    // whether it could place it.
    let trace = std::env::var("VYRN_KERNEL_TRACE").is_ok();
    let mut added: Vec<(String, Release)> = Vec::new();
    // Every body built, for the `Facts` fold below, and the functions this
    // pass wrote a row for. A row's node belongs to one function, so only
    // those need a rebuild.
    let mut built: Vec<Option<Body>> = Vec::with_capacity(lowered.instances.len());
    let mut touched: std::collections::HashSet<String> = Default::default();
    // The judgment memo, when the host armed one (`movecheck::Judgments`): a
    // body whose key is unchanged is served its refusals, neither built nor
    // judged. An armed host reads only refusals, not the facts or rows.
    let js = vyrn_frontend::prof::phase("placer: judgments");
    let memo = vyrn_frontend::movecheck::Judgments::open(program);
    drop(js);
    let _held = crate::append::Held::new(program);
    // Every body is built before any is placed: the kernel asks the effect
    // judgment, which joins every body, whether a callee writes module state.
    // A call into a served body is judged as pure.
    let mut made: Vec<Made> = Vec::with_capacity(lowered.instances.len());
    for inst in &lowered.instances {
        let key = memo
            .as_ref()
            .and_then(|m| m.key(inst.func.module.as_deref(), &inst.spelling()));
        if let Some(rs) = serve(memo.as_ref(), key.as_ref()) {
            made.push(Made::Served(rs));
            continue;
        }
        let bs = vyrn_frontend::prof::phase("placer: core::build");
        let top = build(program, inst, own);
        drop(bs);
        made.push(Made::Built(key, top));
    }
    // `test` and `bench` bodies are judged like any other.
    let os = vyrn_frontend::prof::phase("placer: build_outside");
    let mut made_outside: Vec<Made> = Vec::with_capacity(lowered.bodies.len());
    for ob in &lowered.bodies {
        // Keyed with the line too: the `test@<i>` index is global, so a test
        // added to an earlier module renumbers every later one.
        let key = memo
            .as_ref()
            .and_then(|m| m.key(ob.module.as_deref(), &format!("{}@{}", ob.name, ob.line)));
        if let Some(rs) = serve(memo.as_ref(), key.as_ref()) {
            made_outside.push(Made::Served(rs));
            continue;
        }
        let top = build_outside(
            program,
            own,
            &ob.name,
            ob.module.clone(),
            ob.block,
            &ob.facts,
        );
        made_outside.push(Made::Built(key, top));
    }
    drop(os);
    let ej = vyrn_frontend::prof::phase("placer: effects");
    let mut tops: Vec<(&str, &Body)> = Vec::new();
    for (inst, m) in lowered.instances.iter().zip(&made) {
        if let Made::Built(_, Ok(b)) = m {
            tops.push((inst.func.name.as_str(), b));
        }
    }
    for (ob, m) in lowered.bodies.iter().zip(&made_outside) {
        if let Made::Built(_, Ok(b)) = m {
            tops.push((ob.name.as_str(), b));
        }
    }
    crate::effects::judge_built(program, &lowered, own, &tops, |judged, refs, _| {
        crate::effects::set_state_callees(Some((judged, refs)));
    });
    drop(tops);
    drop(ej);
    // A hoist asked `kernel::writes` before the effect judgment was held. A
    // frame that hoisted a header and calls a function that stores module
    // state may get a different answer, so it is built again.
    let unjudged = |m: &Made| {
        matches!(m, Made::Built(_, Ok(b)) if b.frames().iter().any(|f| {
            f.names.iter().any(|i| i.walked == Some(Walk::While))
                && crate::effects::stores_state(&f.name)
        }))
    };
    for (inst, m) in lowered.instances.iter().zip(made.iter_mut()) {
        if let (true, Made::Built(_, top)) = (unjudged(m), &mut *m) {
            *top = build(program, inst, own);
        }
    }
    for (ob, m) in lowered.bodies.iter().zip(made_outside.iter_mut()) {
        if let (true, Made::Built(_, top)) = (unjudged(m), &mut *m) {
            *top = build_outside(
                program,
                own,
                &ob.name,
                ob.module.clone(),
                ob.block,
                &ob.facts,
            );
        }
    }
    for (inst, m) in lowered.instances.iter().zip(made) {
        let (key, made) = match m {
            Made::Served(rs) => {
                REFUSALS.with(|v| v.borrow_mut().extend(rs));
                built.push(None);
                continue;
            }
            Made::Built(key, made) => (key, made),
        };
        let refused_before = REFUSALS.with(|v| v.borrow().len());
        let top = match made {
            Ok(b) => Some(b),
            Err(g) => {
                refuse_gap(g, &inst.func.module, &inst.func.name);
                None
            }
        };
        if let Some(top) = &top {
            // `VYRN_KERNEL_TRACE=<fn>` prints that body's core, lambdas included.
            if std::env::var("VYRN_KERNEL_TRACE").is_ok_and(|v| v != "1" && top.name.contains(&v)) {
                eprintln!("{}", top.render());
            }
            // A lambda's rows are keyed by its own nodes under the enclosing
            // function's name, where the emitters read them.
            place_frames(top, &inst.func.name, own, &mut added, &mut touched, trace);
        }
        let refused = top
            .as_ref()
            .is_some_and(|t| typed(program, t, &inst.func.module, inst.subst.is_empty()));
        let key = key.filter(|_| !refused);
        remember(memo.as_ref(), key, refused_before);
        built.push(top);
    }
    let mut outside: Vec<Option<Body>> = Vec::with_capacity(lowered.bodies.len());
    for (ob, m) in lowered.bodies.iter().zip(made_outside) {
        let (mut key, made) = match m {
            Made::Served(rs) => {
                REFUSALS.with(|v| v.borrow_mut().extend(rs));
                outside.push(None);
                continue;
            }
            Made::Built(key, made) => (key, made),
        };
        let refused_before = REFUSALS.with(|v| v.borrow().len());
        match made {
            Ok(top) => {
                if std::env::var("VYRN_KERNEL_TRACE")
                    .is_ok_and(|v| v != "1" && ob.name.contains(&v))
                {
                    eprintln!("{}", top.render());
                }
                place_frames(&top, &ob.name, own, &mut added, &mut touched, trace);
                if typed(program, &top, &ob.module, true) {
                    key = None;
                }
                outside.push(Some(top));
            }
            Err(g) => {
                refuse_gap(g, &ob.module, &ob.name);
                outside.push(None);
            }
        }
        remember(memo.as_ref(), key, refused_before);
    }
    // Every generic function is built once more with its parameters as
    // written, the way the checker typed it, for the judgment alone. It
    // places no row and is never emitted.
    let written = Ownership {
        proto: own.proto.as_written(),
        ..own.clone()
    };
    for inst in crate::as_written(program, own) {
        match build(program, &inst, &written) {
            Ok(top) => {
                typed(program, &top, &inst.func.module, true);
                for body in top.frames() {
                    if let Err(rs) = crate::kernel::placement(body) {
                        REFUSALS.with(|v| v.borrow_mut().extend(rs));
                    }
                }
            }
            Err(g) => refuse_gap(g, &inst.func.module, &inst.func.name),
        }
    }
    // Each `impl` projection's body, for the judgment alone: a projection is
    // inlined at its site, and no instance builds its body.
    for p in &lowered.places {
        let inst = crate::Instance {
            func: p.func,
            type_args: Vec::new(),
            subst: Default::default(),
            facts: p.facts.clone(),
            releases: Vec::new(),
        };
        match build(program, &inst, own) {
            Ok(top) => {
                typed(program, &top, &p.func.module, true);
            }
            Err(g) => {
                refuse_gap(g, &p.func.module, &p.func.name);
            }
        }
    }
    // Each module-state initializer and each `where` predicate, for the
    // judgment alone. An initializer the checker did not type has no core;
    // the checker's refusal is its sentence.
    for g in &program.globals {
        if node_ty(&g.init as *const Expr as usize).is_none() {
            continue;
        }
        match build_root(
            program,
            own,
            &lowered.globals,
            g.module.clone(),
            None,
            &g.init,
        ) {
            Ok(top) => {
                typed(program, &top, &g.module, true);
            }
            Err(e) => {
                refuse_gap(e, &g.module, &g.name);
            }
        }
    }
    for d in &program.type_decls {
        let Some(p) = &d.predicate else { continue };
        let binds: Vec<(String, Type)> = match &d.base {
            Type::Record(fields) => fields
                .iter()
                .map(|f| (f.name.clone(), f.ty.clone()))
                .collect(),
            base => vec![("value".to_string(), base.clone())],
        };
        match build_root(
            program,
            own,
            &lowered.predicates,
            d.module.clone(),
            Some(&binds),
            p,
        ) {
            Ok(top) => {
                typed(program, &top, &d.module, true);
            }
            Err(e) => {
                refuse_gap(e, &d.module, &d.name);
            }
        }
    }
    // The lint re-checks the types, which fails two kinds of program by
    // design: a comptime program (its generator helpers use `lex`, `render`
    // and `Token`, which an ordinary check types `<type error>`), and one the
    // typed judgment refused (an unknown name typed `<type error>`, never
    // emitted).
    debug_assert!(
        vyrn_frontend::movecheck::in_comptime()
            || TYPED.with(|t| !t.borrow().0.is_empty())
            || crate::lint(&lowered).is_empty(),
        "the lowered form failed its own lint:
  {}",
        crate::lint(&lowered).join(
            "
  "
        )
    );
    // A placed release of a generic declared release is a call the lowering's
    // worklist follows ([`crate::dispatched`]) only once the row is in the
    // plan, so such a program is lowered again below.
    let by_name: HashMap<&str, &vyrn_frontend::ast::Function> = program
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect();
    let placed: Vec<Release> = added.iter().map(|(_, r)| r.clone()).collect();
    let mut dispatches = !crate::dispatched(&placed, &by_name).is_empty();
    for (f, row) in added {
        touched.insert(f.clone());
        own.releases.entry(f).or_default().push(row);
    }
    // A second build for the emitters, after every row the placer added: the
    // core above read the plan before this pass filled it. A host that armed
    // the memo runs no emitter, and served bodies would leave the facts
    // partial, so it stops here.
    if memo.is_some() {
        crate::effects::set_state_callees(None);
        return;
    }
    let _p2 = vyrn_frontend::prof::phase("placer: facts rebuild");
    let mut facts = Facts::default();
    BODIES.with(|b| b.borrow_mut().clear());
    if let Ok(top) = build_module_state(program, own, &lowered.globals) {
        for body in top.frames() {
            fold_frame(body, &own.proto, &mut facts);
        }
    }
    for (i, inst) in lowered.instances.iter().enumerate() {
        // Rebuilt only where the pass above wrote a row for this function; the
        // rest fold the body that pass already built.
        let fresh = if touched.contains(&inst.func.name) {
            let _p = vyrn_frontend::prof::phase("placer: facts: rebuilt");
            build(program, inst, own).ok()
        } else {
            None
        };
        let Some(top) = fresh.as_ref().or(built[i].as_ref()) else {
            continue;
        };
        for body in top.frames() {
            fold_frame(body, &own.proto, &mut facts);
        }
    }
    // The same for `test` and `bench` bodies, whose nodes an emitter looks up
    // too.
    for (i, ob) in lowered.bodies.iter().enumerate() {
        let fresh = if touched.contains(&ob.name) {
            build_outside(
                program,
                own,
                &ob.name,
                ob.module.clone(),
                ob.block,
                &ob.facts,
            )
            .ok()
        } else {
            None
        };
        let Some(top) = fresh.as_ref().or(outside[i].as_ref()) else {
            continue;
        };
        for body in top.frames() {
            fold_frame(body, &own.proto, &mut facts);
        }
    }
    // A worklist to a fixpoint. Each body is built, placed, and built again,
    // because the first build read the plan before the rows its own placement
    // adds; a row that reaches a generic declared release turns it again.
    // Measure: the instances not yet built, a finite set (the declared
    // releases times the types the program instantiates). A round turns only
    // after one that built at least one of them, since only a new body's
    // placement adds to `placed`.
    let mut had: std::collections::HashSet<String> =
        lowered.instances.iter().map(Instance::spelling).collect();
    while dispatches {
        let again = crate::lower_with(program, own);
        let mut placed: Vec<Release> = Vec::new();
        for inst in &again.instances {
            if !had.insert(inst.spelling()) {
                continue;
            }
            if let Ok(top) = build(program, inst, own) {
                let mut rows = Vec::new();
                place_frames(&top, &inst.func.name, own, &mut rows, &mut touched, trace);
                for (f, row) in rows {
                    placed.push(row.clone());
                    own.releases.entry(f).or_default().push(row);
                }
            }
            let Ok(top) = build(program, inst, own) else {
                continue;
            };
            for body in top.frames() {
                fold_frame(body, &own.proto, &mut facts);
            }
        }
        dispatches = !crate::dispatched(&placed, &by_name).is_empty();
    }
    FACTS.with(|f| *f.borrow_mut() = Some(facts));
    crate::effects::set_state_callees(None);
}

/// One body `augment` built, or served out of the memo.
enum Made {
    /// The refusals the memo recorded for it.
    Served(Vec<crate::kernel::Refusal>),
    Built(
        Option<vyrn_frontend::movecheck::JudgmentKey>,
        Result<Body, Gap>,
    ),
}

/// One body's refusals out of the judgment memo. `Some` means the body is
/// neither built nor judged.
fn serve(
    memo: Option<&vyrn_frontend::movecheck::Judgments>,
    key: Option<&vyrn_frontend::movecheck::JudgmentKey>,
) -> Option<Vec<crate::kernel::Refusal>> {
    let hit = memo?.get(key?)?;
    Some(
        hit.into_iter()
            .map(|(file, line, message, body)| crate::kernel::Refusal {
                message,
                line,
                file,
                body,
            })
            .collect(),
    )
}

/// Records one body's refusals, from `from` to the end of the list, for
/// every body with a key. Serving skips placement too, which a host that
/// armed the memo does not read ([`movecheck::reuse_judgments`]).
fn remember(
    memo: Option<&vyrn_frontend::movecheck::Judgments>,
    key: Option<vyrn_frontend::movecheck::JudgmentKey>,
    from: usize,
) {
    let (Some(memo), Some(key)) = (memo, key) else {
        return;
    };
    memo.put(
        key,
        REFUSALS.with(|v| {
            v.borrow()[from..]
                .iter()
                .map(|r| (r.file.clone(), r.line, r.message.clone(), r.body.clone()))
                .collect()
        }),
    );
}

/// The memory report for one frame, read by `vyrn why --memory` and the
/// editor's memory hints: one row per source `let`, in line order. Every word
/// comes off the core: the type table says how the type is released, the
/// `let` whose the value is ([`NameInfo::not_owned`]), and the kernel what
/// took it and where the release stands.
fn report(
    body: &Body,
    owner: &str,
    missing: &[crate::kernel::Missing],
    took: &[Option<crate::kernel::Took>],
    released: &[Option<Vec<String>>],
    own: &mut Ownership,
) {
    // The releases the kernel found owed, and the holes each walks around:
    // "reclaimed at block exit". A whole-value row counts whichever table
    // `place_frames` files it under (exit, edge, arm binder); a returned
    // `match` holds a value at one edge row per arm.
    let mut exits: HashMap<Name, Vec<String>> = HashMap::new();
    for m in missing {
        match m.kind {
            crate::kernel::MissingKind::Exit
            | crate::kernel::MissingKind::Edge { .. }
            | crate::kernel::MissingKind::ArmBinder { .. } => {
                exits.entry(m.name).or_insert_with(|| plan_holes(&m.holes));
            }
            // A sub-place row says nothing about the binding as a whole, and
            // a store row names a place that may be nobody's binding.
            crate::kernel::MissingKind::EdgePlace { .. } | crate::kernel::MissingKind::Store => {}
        }
    }
    // Taken out and put back at the end, so `own` (whose type table holds
    // every declaration) is not copied per frame, which is per keystroke.
    let mut rows = std::mem::take(own.memory.entry(owner.to_string()).or_default());
    for (i, info) in body.names.iter().enumerate() {
        if !info.bound_by_let {
            continue;
        }
        // The first instance of a generic function answers for the source
        // `let`.
        if rows
            .iter()
            .any(|r| r.name == info.source && r.line == info.line)
        {
            continue;
        }
        let name = info.source.clone();
        let line = info.line;
        let took = took.get(i).and_then(|t| t.as_ref());
        let leaked = |text: String, reason: &'static str, heap: bool| MemoryRow {
            name: name.clone(),
            line,
            text,
            last_use: None,
            moved_into: None,
            bucket: Bucket::Leaked { reason, heap },
        };
        let row = match (&info.not_owned, took) {
            (Some(NotOwned::NoRelease { heap: false }), _) => leaked(
                format!("NOT reclaimed — the type {} owns no heap", info.ty),
                "the type owns no heap",
                false,
            ),
            (Some(NotOwned::NoRelease { heap: true }), _) => leaked(
                format!("NOT reclaimed — nothing releases the type {} yet", info.ty),
                "the type has no release rule",
                true,
            ),
            (Some(NotOwned::Borrow(what)), _) => leaked(
                format!("NOT reclaimed — it is {what}"),
                "it names somebody else's value",
                true,
            ),
            (Some(NotOwned::Static), _) => MemoryRow {
                name,
                line,
                text: "static data — nothing reclaims it, and nothing needs to".to_string(),
                last_use: None,
                moved_into: None,
                bucket: Bucket::Static,
            },
            // A must-use value handed to a builtin is disposed of, not moved.
            (Some(NotOwned::MustUse(l)), Some(t)) if t.builtin => MemoryRow {
                name,
                line,
                text: discharged(l),
                last_use: None,
                moved_into: None,
                bucket: Bucket::Discharged,
            },
            (_, Some(t)) if t.how == crate::kernel::TookHow::Drop => MemoryRow {
                name,
                line,
                text: format!("reclaimed by `drop` at line {}", t.line),
                last_use: Some(t.line),
                moved_into: None,
                bucket: Bucket::Dropped,
            },
            // A `return` is named "the return": it has no taker to look at.
            (_, Some(t)) => {
                let into = match t.how {
                    crate::kernel::TookHow::Return => "the return".to_string(),
                    _ => t.by.clone(),
                };
                MemoryRow {
                    name,
                    line,
                    text: format!("moved at line {} into {into}", t.line),
                    last_use: Some(t.line),
                    moved_into: Some(into),
                    bucket: Bucket::Moved,
                }
            }
            // A program that reaches here discharges every must-use value,
            // and the discharging construct frees it.
            (Some(NotOwned::MustUse(l)), None) => MemoryRow {
                name,
                line,
                text: discharged(l),
                last_use: None,
                moved_into: None,
                bucket: Bucket::Discharged,
            },
            (None, None) => match (
                own.proto.release_kind(&info.ty),
                exits
                    .get(&(i as Name))
                    .cloned()
                    .or_else(|| released[i].as_deref().map(plan_holes)),
            ) {
                (Some(kind), Some(holes)) => MemoryRow {
                    name,
                    line,
                    text: reclaimed(&kind, &holes),
                    last_use: None,
                    moved_into: None,
                    bucket: Bucket::Reclaimed,
                },
                // Owned, and no release placed; the core states no reason.
                _ => leaked(
                    "NOT reclaimed — nothing in this frame releases it".to_string(),
                    "nothing releases it here",
                    info.heap,
                ),
            },
        };
        rows.push(row);
    }
    rows.sort_by_key(|r| r.line);
    own.memory.insert(owner.to_string(), rows);
}

/// The "reclaimed at block exit" sentence, with the places a `consume` took
/// out of the value, which the release walks around.
fn reclaimed(kind: &DropKind, holes: &[String]) -> String {
    if holes.is_empty() {
        return format!("reclaimed at block exit — {}", kind.words());
    }
    let places = holes
        .iter()
        .map(|p| format!("`{p}`"))
        .collect::<Vec<_>>()
        .join(", ");
    format!(
        "reclaimed at block exit — {}, except {places}, which a `consume` took",
        kind.words()
    )
}

/// The must-use sentence: what the program wrote to discharge the value, and
/// which lowering frees it.
fn discharged(l: &Linear) -> String {
    match l {
        Linear::Stream => "discharged, not leaked — a stream is consumed, forwarded or closed \
             on every path, and that lowering frees it"
            .to_string(),
        Linear::Declared(by) => format!(
            "discharged, not leaked — `{by}` declares `impl MustUse`, so it is handed on or \
             dropped on every path"
        ),
    }
}

/// Places what one built body owes, frame by frame. `owner` keys the plan's
/// tables: the function's name, or the synthetic `test@<i>` / `bench@<i>`.
/// A lambda frame is keyed by its enclosing body's name.
fn place_frames(
    top: &Body,
    owner: &str,
    own: &mut Ownership,
    added: &mut Vec<(String, Release)>,
    touched: &mut std::collections::HashSet<String>,
    trace: bool,
) {
    for body in top.frames() {
        let ks = vyrn_frontend::prof::phase("placer: kernel::placement");
        let placed = crate::kernel::placement(body);
        drop(ks);
        let crate::kernel::Placement {
            missing,
            took,
            released,
        } = match placed {
            Ok(m) => m,
            Err(rs) => {
                for r in rs {
                    if trace {
                        eprintln!("placer: refused: {}: {}", r.body, r.message);
                    }
                    // No placement repairs these. Every one the body earns is
                    // kept, so the driver can merge by binding and line.
                    REFUSALS.with(|v| v.borrow_mut().push(r));
                }
                continue;
            }
        };
        let rp = vyrn_frontend::prof::phase("placer: report");
        report(body, owner, &missing, &took, &released, own);
        drop(rp);
        for m in missing {
            // A store's row is keyed by the store alone: its place may be no
            // binding of this frame, so it is handled before the name is read.
            if m.kind == MissingKind::Store {
                let fresh = PLACED.with(|p| {
                    p.borrow_mut()
                        .stores
                        .insert(m.site, m.holes.clone())
                        .is_none()
                });
                if fresh {
                    if trace {
                        eprintln!("placer: {} store at {} releases", body.name, m.site);
                    }
                    touched.insert(owner.to_string());
                }
                continue;
            }
            let info = &body.names[m.name as usize];
            let kind = own.proto.release_kind(&info.ty);
            if trace {
                eprintln!(
                    "placer: {} `{}` (line {}) {:?} at {:?} site {} kind {:?} holes {:?}",
                    body.name, info.source, info.line, m.kind, m.exit, m.site, kind, m.holes
                );
            }
            // A receiver a consumer borrowed out of ([`NameInfo::producer`]):
            // an argument temporary keyed by the producing node.
            if let Some(producer) = info.producer {
                let fresh = PLACED.with(|p| p.borrow_mut().producers.insert(producer));
                if fresh {
                    touched.insert(owner.to_string());
                }
                continue;
            }
            if m.site == 0 {
                continue;
            }
            let Some(kind) = kind else {
                continue;
            };
            // An element hole (`.[]`) cannot be skipped: no row, and the
            // judgment refuses the name.
            if m.holes.iter().any(|h| h.contains("[]")) {
                continue;
            }
            let holes: Vec<String> = m
                .holes
                .iter()
                .map(|h| h.trim_start_matches('.').to_string())
                .collect();
            // A declared release takes the whole value: it cannot
            // be told a hole.
            if !holes.is_empty() && matches!(kind, DropKind::Release(..)) {
                continue;
            }
            match m.kind {
                // Rule N: one edge of a join still holds what another took.
                // Keyed by name, so a loop variable qualifies.
                MissingKind::Edge { edge } => {
                    PLACED.with(|p| {
                        let mut p = p.borrow_mut();
                        let rows = p.edges.entry(m.site).or_default();
                        if !rows.iter().any(|(n, e, _)| *n == info.source && *e == edge) {
                            rows.push((info.source.clone(), edge, holes));
                            touched.insert(owner.to_string());
                        }
                    });
                    continue;
                }
                // The sub-place one edge took, released on the other, spelled
                // `d.line`.
                MissingKind::EdgePlace { edge, path } => {
                    let name = format!("{}{}", info.source, path);
                    PLACED.with(|p| {
                        let mut p = p.borrow_mut();
                        let rows = p.edges.entry(m.site).or_default();
                        if !rows.iter().any(|(n, e, _)| *n == name && *e == edge) {
                            rows.push((name, edge, Vec::new()));
                            touched.insert(owner.to_string());
                        }
                    });
                    continue;
                }
                // The arm's unmoved payload binders, one entry each, with the
                // holes the arm left.
                MissingKind::ArmBinder { arm } => {
                    PLACED.with(|p| {
                        let mut p = p.borrow_mut();
                        let rows = p.arms.entry((m.site, arm)).or_default();
                        if !rows.iter().any(|(n, _)| *n == info.source) {
                            rows.push((info.source.clone(), holes));
                            touched.insert(owner.to_string());
                        }
                    });
                    continue;
                }
                MissingKind::Exit => {}
                MissingKind::Store => unreachable!("read above, keyed by the store"),
            }
            // A field read's unnamed receiver is stated on the name
            // (`NameInfo::receiver`, `NameInfo::holes`), not as a row.
            if info.receiver.is_some() {
                continue;
            }
            let Some(binding) = info.binding else {
                continue;
            };
            // A row the plan already placed here takes the kernel's hole set,
            // which is per path where the plan's is per binding.
            if let Some(r) = own.releases.get_mut(owner).and_then(|rows| {
                rows.iter_mut()
                    .find(|r| r.exit == m.exit && r.site == m.site && r.binding == binding)
            }) {
                if trace {
                    eprintln!(
                        "placer: rewrite {} `{}` {:?} -> {:?}",
                        owner, info.source, m.exit, holes
                    );
                }
                r.holes = Some(holes);
                touched.insert(owner.to_string());
                continue;
            }
            let dup = added.iter().any(|(f, r)| {
                *f == owner && r.exit == m.exit && r.site == m.site && r.binding == binding
            });
            if dup {
                continue;
            }
            added.push((
                owner.to_string(),
                Release {
                    site: m.site,
                    binding,
                    name: info.source.clone(),
                    kind: kind.clone(),
                    exit: m.exit,
                    line: info.line as u32,
                    // The kernel's set at this exit, even when empty: `None`
                    // would fall back to the binding's set, which is not per
                    // path (`regexredux`'s early `Err` returns walk the whole
                    // record).
                    holes: Some(holes),
                },
            ));
        }
    }
}
