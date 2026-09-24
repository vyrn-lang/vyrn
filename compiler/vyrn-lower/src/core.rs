//! The named core — RFC-0125 §2.1 (M2).
//!
//! Every intermediate value has a name, every access is a place, and every
//! release the ownership plan decided is an explicit [`St::Drop`]. A backend
//! reading this form would decide nothing; today nothing reads it but the
//! kernel (`kernel.rs`), which makes the linear judgment over it: every owned
//! name is consumed exactly once on every path.
//!
//! **What this slice does and does not do.** It builds the core for a function
//! instance from three things it does not derive: the checker's type for every
//! expression ([`crate::Row`]), the ownership plan's decisions
//! ([`vyrn_frontend::own::ReleasePlan`] and the placed [`Release`] rows), and
//! the declarations' answer to "does this type own heap"
//! ([`vyrn_frontend::declared::Owned`]). It derives nothing about ownership itself:
//! where the plan placed a release, a `Drop` stands; where it did not, nothing
//! stands, and the kernel says whether that is a leak. That is the point of M2:
//! the kernel re-checks the plan's decisions per program, so a decision the
//! plan missed is refused at compile time instead of found by the ratchet.
//!
//! A construct this pass does not lower returns a [`Gap`] naming it, and the
//! instance is reported as unlowered rather than accepted or refused. The
//! corpus test counts gaps by construct. The second slice lowered every
//! construct the corpus has (RFC-0125 §3 M2); what a gap still names is a
//! binding the plan leaks on purpose, a hole its release walk cannot skip.

use std::collections::HashMap;

use vyrn_frontend::ast::{
    ArmBody, BinOp, Binder, Block, Capability, Expr, Function, LambdaBody, MatchArm, Pattern,
    Program, Stmt, Type, UnOp,
};
use vyrn_frontend::declared::Owned;
use vyrn_frontend::own::{Bucket, DropKind, Exit, Linear, MemoryRow, Ownership, Release};
use vyrn_frontend::prelude;

use crate::kernel::{MissingKind, Root};
use crate::{Instance, Node};

/// A name in a body: an index into [`Body::names`].
pub type Name = u32;

#[derive(Debug, Clone)]
pub struct NameInfo {
    /// The source spelling, or `@tN` for a temporary the naming pass minted.
    pub source: String,
    pub ty: Type,
    /// Whether a held value of this name owes a RELEASE at an exit: its type
    /// owns heap, or it carries a must-use obligation. A borrowed binding (a
    /// `read` parameter, a pattern binder of a non-consuming match, a `for`
    /// variable over a container it does not consume) owes none, whatever its
    /// type.
    ///
    /// This is RFC-0114's question, and it is not RFC-0089 rule 1's. A value
    /// that owns no heap has no release and is still owned; the kernel asks
    /// the two apart (`kernel::Kernel::releases` and `kernel::Kernel::owned`).
    pub releases: bool,
    /// Whether the type owns heap.
    pub heap: bool,
    /// Whether the name is a borrow (RFC-0089 rule 2): its type owns heap,
    /// the body does not own it, and it is not static data — a `read` or
    /// `modify` parameter, a binding read out of a place somebody owns, a
    /// second name for one, a payload binder, a `for` variable over a
    /// container the loop does not own. The kernel keeps what a borrow
    /// bound to a place reads, and refuses a take of it.
    pub borrow: bool,
    /// What kind of borrow this name is, in the checker's words
    /// (`movecheck::Borrow::what`): the capability, and the parameter it
    /// comes from. A `read` or `modify` parameter and a second name for one
    /// carry a kind; a borrow bound by a read of a place carries none,
    /// because the kernel's alias table words that one from the place it
    /// reads. RFC-0125 §3 M3, the census: this is what the kernel needs to
    /// refuse a take of a parameter, which has no place to be an alias of.
    pub borrow_kind: Option<BorrowKind>,
    pub line: usize,
    /// The node the plan keys this binding by — a `Stmt::Let`, a parameter —
    /// when the name is one the plan can be told about. `None` for a
    /// temporary this pass minted.
    pub binding: Option<usize>,
    /// For the unnamed receiver of a field read (`parse(q).sels`): the
    /// `Expr::Field` node, which is the key of the plan's receiver-free row
    /// (RFC-0114 R1′). The placer frees such a receiver right after the read,
    /// minus the field the read took.
    pub receiver: Option<usize>,
    /// For the unnamed receiver of a heap field or element read the
    /// consumer BORROWS (`f(x).rhs.startsWith("{")`, `weekdayLetters()[1]`):
    /// the node that produced the receiver, which is the key of the
    /// argument-temporary drop the placer writes (RFC-0125 M3, third
    /// slice). Set only where the consumer is a call or an operator, the
    /// two sites each compiled backend drains such temporaries at.
    pub producer: Option<usize>,
    /// RFC-0114 M1: for a call-argument temporary the CALLER releases after
    /// the call, the argument expression's node — the key of the plan's
    /// `arg_drops` row (RFC-0125 §3 M3, the emitter-reads-the-core slice).
    /// The release itself is the `St::Drop` the binding after the call
    /// queues; this is the key a reader looks it up by.
    pub arg_drop: Option<usize>,
    /// The holes the plan's release walk skips for this binding (RFC-0093
    /// M2's table), spelled as the kernel spells them (`.f.g`). A `Drop` of
    /// the name walks around exactly these; a placed row may carry its own.
    pub holes: Vec<String>,
    /// For a payload binder read out of its scrutinee ([`Arm::reads`]): what a
    /// `consume` handed the binder leaves in the scrutinee.
    pub payload: Option<Payload>,
    /// RFC-0125 §3 M3, row 11b: for a receiver ([`NameInfo::receiver`]), was
    /// the block a CALLEE allocated? A callee's block is malloc-side
    /// whatever `region` is open at the call site, so the free stands there
    /// too; the `@`-spelled producers (`@concat`, `@str`, `@copy`) route
    /// through the arena lexically and stay region-gated. The region itself
    /// is the emitter's, because this pass lowers a `region` as an ordinary
    /// block.
    pub receiver_malloc: bool,
    /// RFC-0075 M1: a parameter of a must-use type carries the obligation
    /// into the callee, so a take of it is the callee's to make — `boxStream(s)`
    /// is not a take of the caller's value. That is a rule about OWNERSHIP,
    /// and the capability the parameter declares is still a fact about the
    /// NAME: `read self` is a `read` parameter whether or not `Self` declares
    /// `impl MustUse`. The two were one field until row 21 asked what an
    /// undeclared `self` IS (RFC-0125 §3 M3, the corpus slice), and this is
    /// the ownership half: it excepts the take, and [`NameInfo::borrow_kind`]
    /// keeps the words.
    pub must_use_param: bool,
    /// A String accumulator [`crate::append::append_candidates`] admits: its
    /// `let` gives it the ownership word, and `s = s + e` on it is one
    /// `@strAppend` row ([`Spec::Rebuilds`]). A read of a module-state
    /// accumulator ([`crate::append::global_append_candidates`]) is the
    /// receiver of that row for `g = g + e`.
    pub grows: bool,
    /// The path the READER wrote, for a temporary this pass minted to hold a
    /// read of a place: `p.name`, `xs[i]`, `d.title`. A refusal about the
    /// temporary is a refusal about that read, and `@borrow` is a name no
    /// program contains — the checker quotes the path and the kernel quoted
    /// the compiler's own spelling, which is 23 of the corpus's differing
    /// refusals (RFC-0125 §3 M3, the corpus slice). `None` for every name a
    /// program does spell, whose `source` is already the reader's.
    pub path: Option<String>,
    /// The temporary a `for x in consume xs` loop binds its container to.
    ///
    /// The core lowers that loop's take to `let @tN = xs`, which is the shape
    /// a move into any other binding has, so a refusal about `xs` named "a
    /// value" where the checker names the loop the reader wrote (RFC-0125 §3
    /// M3, row 07). The form is a fact about the statement and the core keeps
    /// no statement kinds, so it is a fact about the name the statement binds.
    pub for_consume: bool,
    /// For a name a RECORD literal binds: where each part of the literal goes,
    /// in the checker's words — "the field `R.s`", one per field in order.
    ///
    /// A literal takes its parts, and the checker names the field each part
    /// went into (`movecheck`'s `Expr::StructLit` arm). `Rhs::Make` is a list
    /// of values with no names on it, so the kernel could only say "a literal"
    /// (RFC-0125 §3 M3, row 07). Empty for an array, a map and a variant,
    /// whose parts the checker does not name either.
    pub fields: Vec<String>,
    /// For the variable of a `for` over a container the loop does NOT own: the
    /// container's root, which the way out names (RFC-0125 §3 M3, row 19).
    ///
    /// It is not a [`NameInfo::borrow_kind`], and the difference is the whole
    /// point. A kind refuses every take of the name; this one only says what
    /// the name IS when a take is refused for a reason the alias table already
    /// found. Making it a kind refused `std/vyx.vyrn`'s `for s in kids` and
    /// twenty-two programs of the corpus with it.
    pub loop_var: Option<String>,
    /// The loop that walks this borrow, if one does. A `modify` argument over
    /// the container ends it whatever the container holds ([`crate::kernel`]).
    pub walked: Option<Walk>,
    /// Whether the type is LINEAR — a `Stream`, a `Task`, a type that declares
    /// `impl MustUse` (RFC-0075). Such a value is disposed, not stored, and the
    /// builtin that disposes it (`close`, `@join`, `boxStream`) is the one
    /// builtin `movecheck::sinks` answers `false` for: the must-use walk owns
    /// it, and it words a use after it as a `consume` parameter's, not as a
    /// move into a sink. RFC-0125 §3 M3, row 07.
    pub linear: bool,
    /// Whether a `let` a reader WROTE bound this name. A `for` variable, a
    /// pattern binder and a temporary are keyed by a node too, and none of
    /// them is a binding the memory report is about.
    pub bound_by_let: bool,
    /// For a name a LAMBDA literal binds: the captures the closure reads as
    /// VALUES, where the closure may outlive the call it is written at
    /// (RFC-0037, RFC-0125 §3 M3, row 24). `None` where it may not, and for
    /// every name no lambda binds.
    ///
    /// Two facts about the literal, both about WHERE it is written, so the
    /// core states them and the kernel states the rule over them. A lambda
    /// written at an argument position whose parameter provably only borrows
    /// it — `map(xs, x -> ..)` — cannot outlive the call and captures freely;
    /// everywhere else the closure is a value under RFC-0037's
    /// defunctionalization, which is the default and the safe direction. And
    /// a capture the body only CALLS is not a value the closure holds:
    /// `applyAll`'s `n -> f(n) + 1` names a function, and the call reaches
    /// the same body whoever holds it, where a captured buffer is one block
    /// with one owner (`examples/capturefn.vyrn`, `std/stream.vyrn`).
    pub closure_reads: Option<Vec<Name>>,
    /// Why the value this name binds is NOT this frame's, where it is not —
    /// the core's own statement, minted where [`Builder::owned_binding`]
    /// decides it (RFC-0125 §3 M3, the report slice). `None` for a name the
    /// frame owns and for every temporary.
    ///
    /// It is the reason half of the same question `releases` and `borrow`
    /// answer as flags. Nothing in a judgment reads it: it is what the memory
    /// report says out loud, so the report and the ownership rule are one
    /// statement rather than two walks that could disagree.
    pub not_owned: Option<NotOwned>,
}

/// A loop that reads its container through a borrow ([`NameInfo::walked`]).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Walk {
    /// A `for` reads its container through the borrow from its head to its
    /// end.
    For,
    /// A `while` reads the header of a container it indexes and never
    /// rebuilds ([`Builder::hoist_headers`]). An element store moves no
    /// header, so it ends no such borrow.
    While,
}

/// Why a `let` binds a value the frame does not own — the report's reasons,
/// in the order [`Builder::owned_binding`] asks them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum NotOwned {
    /// The type releases nothing. `heap` says whether it owns heap anyway,
    /// which is the difference between "nothing to reclaim" and "no release
    /// rule yet" (RFC-0092 M0).
    NoRelease { heap: bool },
    /// The type carries a must-use obligation, and the construct that
    /// discharges it reclaims the value (RFC-0075 M1, RFC-0095 M1).
    MustUse(Linear),
    /// A literal, in the module's data segment. Nothing allocated it.
    Static,
    /// Somebody else owns the storage. The words say what the binding is, as
    /// `movecheck::Borrow::what` words them.
    Borrow(String),
    /// A join arm handed this binding's value on ([`Builder::alias_out`]):
    /// the frame holds one value under two names afterwards and stops
    /// answering for this one. Carries the join's line.
    Aliased(usize),
}

/// The path a reader wrote for a place read, spelled as the checker quotes it
/// (`ast::place_path`, `project::element_path`): `p.name`, `xs[i]`,
/// `d.title`. `None` where the expression names no place.
fn reader_path(e: &Expr) -> Option<String> {
    vyrn_frontend::ast::place_path(e)
        .or_else(|| vyrn_frontend::project::element_path(e))
        .map(|(_, p)| p)
}

/// The borrow a parameter's capability makes, or `None` for one that owns
/// what it is handed (`consume`).
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
/// place (RFC-0089 rule 2, `movecheck::Borrow`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BorrowKind {
    /// A `read` or `modify` parameter, or a second name for one: the
    /// capability, and the parameter's own spelling.
    Param { cap: &'static str, of: String },
    /// A name of the enclosing frame that a lambda frame reads (RFC-0037).
    /// The closure observes it; the frame that made it still owns it.
    Capture,
    /// The variable of a `for` over a container the loop does not own: the
    /// container still owns the element, and the loop only names it. `of` is
    /// the container's ROOT, which is what the way out names
    /// (`movecheck::Borrow::Element`). RFC-0125 §3 M3, row 19.
    LoopVar { of: String },
    /// A payload binder of a `match` that did not take its scrutinee, where
    /// the scrutinee is itself a read of a place: the place owns the payload
    /// and the binder only names it (`movecheck::Borrow::Projection`).
    ///
    /// The alias beside it says WHICH place, for the rules about writing
    /// around a live read. This says what the binder IS, which is the half a
    /// refusal quotes — and it is the reader's own name that gets quoted,
    /// where following the alias to its root quotes the place instead
    /// (RFC-0125 §3 M3, row 17).
    Place,
}

impl BorrowKind {
    /// What this borrow is, in words, for a refusal. `at` is the name the
    /// sentence is about: a second name for a parameter says so, which is
    /// how `movecheck::Borrow::what` words it.
    pub fn what(&self, at: &str) -> String {
        // A PATH under the parameter is the parameter: `h.meta[0]` is read out
        // of a `read` parameter and the reader is told so, where a name bound
        // to one (`let t = r.s`) is a second name for it. The two are
        // comparable at the root alone, which is `movecheck::Borrow::what`'s
        // own test (RFC-0125 §3 M3).
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

    /// The named ways out (RFC-0087 U2), in the order and the words
    /// `movecheck::Borrow::fixes` names them: take ownership if this function
    /// should have it, copy if both sides genuinely need a value. `path` is
    /// what was read out of the binding — a `consume` goes on the parameter,
    /// a `.copy()` on the path.
    ///
    /// A capture has no menu here: the frame that made it owns it, and the
    /// checker answers that shape at the capture rather than at the take.
    pub fn fixes(&self, path: &str) -> Vec<String> {
        match self {
            BorrowKind::Param { of, .. } => vec![
                format!("declare the parameter `{of}: consume ..` if this function should own it"),
                format!("`{path}.copy()` if both sides need a value"),
            ],
            BorrowKind::Capture => Vec::new(),
            // A loop variable has a second way out, and it comes first: let the
            // loop take the elements. It only works when the WHOLE element is
            // handed on — a stored FIELD of one is a partial move — so a path
            // under the variable is left with the copy alone
            // (`movecheck::Borrow::fixes`).
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

/// Where a statement stands, for a reader that looks a plan row up by node
/// (RFC-0125 §3 M3, the emitter-reads-the-core slice).
///
/// **A site is the address of the AST node the ownership plan keys its row
/// by, and nothing else.** It is not a source position and not an ordinal: a
/// reader asks the core for the row at a node it already holds, and gets an
/// answer or none. The census's lesson stated once — a fact stated at a
/// position is not stated, because a careful reader can see it and a lookup
/// cannot ask for it.
///
/// The statements that carry one: [`St::Store`] (its `Stmt::Assign`,
/// `Stmt::SetField` or `Stmt::IndexSet` node), a [`St::Drop`] of a discarded
/// result (its `Stmt::Expr` node), a [`St::Drop`] one edge of a join owes
/// ([`Site::Edge`]), [`St::Row`], [`St::If`], [`St::Block`], [`St::Break`],
/// [`St::Continue`], [`St::Return`] and [`Arm`]. An argument temporary's key
/// rides on the NAME instead ([`NameInfo::arg_drop`]), because the drop that
/// runs it belongs to the binding after the call. Every other statement
/// states [`Site::None`]: this pass has no key for it, and a reader falls
/// back to the plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum Site {
    /// This pass states no key for the statement.
    #[default]
    None,
    /// The node the plan keys the row by.
    Node(usize),
    /// RFC-0114 Rule N: the join whose edge owes this release, and which edge
    /// — 0/1 for an `if`'s then/else, the arm's source index for a `match`.
    Edge(usize, u32),
}

/// WHAT a literal is (RFC-0125 §3 M3, the operation slice).
///
/// The core stated WHO OWNS WHAT and WHERE CONTROL GOES, and never WHAT IS
/// COMPUTED: `let x = 5` and `let x = 7` were one statement, so an emitter
/// read the value off the source. §2.3's emitter "maps `prim` rows to wasm
/// instructions", and it cannot while the instruction's operand is missing
/// from the row.
///
/// The WIDTH is not here, and that is the granularity the census argued for.
/// An integer literal's type is its destination's (RFC-0058's sized
/// integers), which is the checker's answer at the node and
/// [`crate::Row::ty`]'s to hand out. A width in the row would be a second
/// statement of the same rule, which is the thing this RFC is about.
#[derive(Debug, Clone, PartialEq)]
pub enum Lit {
    Int(i64),
    /// A byte literal `'c'` (RFC-0057) — an integer literal whose value is
    /// the byte.
    Byte(u8),
    Float(f64),
    Bool(bool),
    /// A string literal, decoded. The row names the BYTES and not a data
    /// segment: where they land is the emitter's question, and the two
    /// compiled backends answer it differently.
    Str(String),
    /// Not a value a reader wrote, and nothing an emitter loads. The kind
    /// says which row wrote it, so a partial close of the family says which
    /// part.
    Opaque(Opaque),
}

/// What a row stands on where it names no value.
///
/// Each kind is one producer in this pass, and each one blocks on something
/// of its own: [`Opaque::Pull`] on a stream's pull, which is a call the row
/// does not state, [`Opaque::Trapped`] on nothing, because the statement after
/// it never runs, and [`Opaque::Unbound`] on a store with nothing to store.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Opaque {
    /// A function's name used as a value, or a type's name as an argument
    /// (`fromJson(Bag, src)`). The position decides what it stands for: a
    /// `fn`-typed argument (`unfold(100, naturals)`) is monomorphized at the
    /// call and no value stands there at all, and a stored fn value (`let
    /// sink: IntSink = double`) is the tag RFC-0037's defunctionalizer
    /// chose, which the emitter holds and this pass does not.
    Static,
    /// A `for` head over a stream: its exit test and the index its element is
    /// read at. A stream is pulled and not indexed, so both are the one call
    /// that answers the next element or none, and the row states no such call.
    Pull,
    /// The result of a call that traps (`panic`). Only a row after the
    /// `St::Trap` reads it, and no finished body holds one ([`cut`]).
    Trapped,
    /// The value a `?` stores where its ok arm binds nothing.
    Unbound,
}

/// The literal a literal expression IS, and `None` for every other
/// expression.
///
/// WHICH expression forms are literals is stated here and nowhere else. It
/// was stated twice before this row existed — [`Builder::val`] and
/// [`Builder::rhs_inner`] each named the same five `Expr` variants to answer
/// "nothing to own" — and the row would have made it three.
/// [`Builder::val`] asks this instead of naming them. [`Builder::rhs_inner`]
/// still names the five, because its match is exhaustive on purpose and a
/// form with no arm must fail to compile; it names them and asks here for the
/// answer (RFC-0125 §3 M3, the operation slice).
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

#[derive(Debug, Clone, PartialEq)]
pub enum Place {
    Name(Name),
    Global(String),
    Field(Box<Place>, String),
    Elem(Box<Place>, Val),
    /// A map's entry at a key. A store into it takes the key: the map keeps
    /// the key it is handed, or releases the surplus one when an equal key is
    /// already there (RFC-0028, `examples/mapkeyowned.vyrn`). A read of it
    /// borrows the key.
    Key(Box<Place>, Val),
}

/// A right-hand side: what produced the value a `let` binds or a store puts
/// away.
///
/// **The producer type.** Two variants below carry one, and it is the type the
/// value HAS when the node's code has run, before any coercion the destination
/// asks for — [`crate::Row::has`] falling back to [`crate::Row::ty`], which is
/// the pair a backend reads (RFC-0101 §2.1 item 2 [A16]). It is the CHECKER's
/// answer at that node and never this pass's guess. The other four variants
/// need none: a `Val`, a `Read` and a `Take` name a place or a name whose type
/// the core already carries, and a `Make` is a literal of its destination.
///
/// The typed judgment (`typed.rs`) is what reads it. Without it `a + b` and
/// `UInt8(n)` read alike, so a store into a sized integer had no producer to
/// ask and 94,691 of them were unjudged (RFC-0125 §3 M6, the third judgment's
/// second slice).
#[derive(Debug, Clone)]
pub enum Rhs {
    /// The value of a name: a move when the name is owned, a copy otherwise.
    Val(Val),
    /// A read of a place that yields a value the kernel does not own — a scalar
    /// field, an element of a heapless type, a borrowed payload.
    Read(Place),
    /// A move out of a sub-place (`consume x.f`, RFC-0093; the receiver a
    /// rebuilding builtin hands back). The value leaves into the bound name,
    /// and the base keeps a hole where it was: a later release of the base
    /// walks the rest, and a later read of the hole is refused.
    Take(Place),
    Call {
        callee: String,
        args: Vec<(Val, Capability)>,
        /// Argument 0 is the receiver of a rebuilding builtin passed by name
        /// (`out.push(v)`): the call hands the buffer back through its result
        /// and the store after it puts it back, so the take changes no owner.
        ///
        /// The KERNEL is the reader, and the only one: it lets a `modify`
        /// parameter be this take's subject and it words the refusal when the
        /// receiver is an alias ([`crate::kernel::Kernel::take_arg`]). An
        /// emitter needs no such field, because the call and the store are two
        /// rows and each is read where it stands.
        ///
        /// The rule under it — which builtin rebuilds its receiver — is
        /// [`vyrn_frontend::prelude::rebuilds`], the one statement both this
        /// pass and `movecheck::sinks` read. The exception itself is stated
        /// here (RFC-0125 §3 M3, the checker's deletion path).
        write_back: bool,
        /// WHO the name resolves to (RFC-0125 §3 M3, the callee slice).
        kind: Callee,
        /// The producer type: what the callee answers at this site, with the
        /// call's own type arguments already substituted. `None` for a call
        /// the checker did not type.
        ret: Option<Type>,
        /// The instance a call to a generic function names: its type
        /// arguments as the checker solved them at this site, by parameter
        /// name in the callee's order ([`crate::Row::solved`]). Empty for a
        /// callee with no type parameters and for a call the checker did not
        /// solve.
        solved: Vec<(String, Type)>,
        /// At a call to a function with `fn`-typed parameters (RFC-0023), the
        /// target each such parameter is bound to, in the callee's order. The
        /// argument states no value of its own. Empty elsewhere, and where an
        /// argument is a value no [`Target`] names, which stays in `args`.
        targets: Vec<Target>,
    },
    /// Arithmetic, comparison, interpolation, conversion: reads its operands.
    /// The first field is WHAT it computes; the last is the producer type —
    /// the operator's own result, which is what `binop_type` decided and the
    /// destination did not.
    Prim(Op, Vec<Val>, Option<Type>),
    /// A record, array, map or variant literal: takes its parts. The first
    /// field is WHAT it constructs.
    Make(Ctor, Vec<Val>),
}

/// What a `fn`-typed parameter of a specialization calls (RFC-0023): the
/// instance of a higher-order function is one per target, and a call through
/// the parameter is a direct call to it ([`specialize`]).
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Target {
    /// A function the program declares, called with no captures.
    Fn(String),
    /// The target this body's own `fn`-typed parameter is bound to: a
    /// pass-through, which [`specialize`] replaces by the instance's target.
    Param(Name),
}

/// WHO a [`Rhs::Call`]'s name resolves to (RFC-0125 §3 M3, the callee slice).
///
/// [`Builder::call`] answers this once, to say where each argument's
/// capability comes from. It threw the answer away into two bools —
/// `declared`, for the three cases whose parameters the author wrote, and
/// `ctor`, for the variant — and every later reader that needed a THIRD case
/// resolved the name again. The emitter's ladder is that reader: fourteen
/// rungs from a `std/mem` primitive to the function table, walked over the
/// source at every call site, and no row it could have read instead.
///
/// So the answer is the row. `declared` and `ctor` are the two questions the
/// kernel asks of it ([`Callee::declared`], [`Callee::ctor`]), and an emitter
/// asks which rung it is.
///
/// The cases are [`Builder::call`]'s own branches and nothing more. Two of
/// them fold three branches each, because the branches differ in the
/// capability they synthesize and not in who the callee is: a name with no
/// seeded row and a name beginning with `@` are both [`Callee::Reserved`],
/// and `print` is too.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Callee {
    /// A function this program declares. The emitter's function table
    /// answers for it, at the instance this call dispatches to.
    Fn,
    /// A method of an `impl` block (RFC-0002 §5), dispatched on its
    /// receiver's concrete type.
    Method,
    /// A projection (RFC-0120), dispatched through the places table.
    Projection,
    /// A seeded builtin: a row of [`prelude::signature`].
    Builtin,
    /// A variant of an enum, or `Some`, `Ok`, `Err`.
    Ctor,
    /// A declared type's constructor: `T(v)` for a record or a
    /// `where`-checked type (RFC-0079). Its arguments are taken.
    Named,
    /// A reserved name with no seeded row: `fromJson`, `value`, a log level,
    /// a generation-time surface builtin, a `@`-spelled operation, `print`.
    Reserved,
    /// A call through a function VALUE in scope (RFC-0023): a lambda takes
    /// its parameters by read.
    Value,
}

impl Callee {
    /// Whether the callee DECLARES its capabilities: a function, a method or
    /// a projection whose parameters the author wrote. A `consume` on such a
    /// parameter is RFC-0089 rule 1 — it takes ownership of whatever it is
    /// handed, heap or not.
    ///
    /// False for a builtin, a variant constructor and every other callee
    /// whose capabilities [`Builder::call`] synthesizes. Such a call STORES
    /// its argument, and storing a value that owns no heap copies it, so the
    /// caller keeps its own (`movecheck::sinks` asks `owns_heap` there and
    /// asks nothing at a declared parameter). RFC-0125 §3 M3, the
    /// two-questions slice.
    pub fn declared(self) -> bool {
        matches!(self, Callee::Fn | Callee::Method | Callee::Projection)
    }

    /// Whether the callee is a CONSTRUCTOR. A builtin and a variant are both
    /// undeclared and the two are different sentences: a builtin STORES its
    /// argument and a constructor PUTS it into the value it makes, which is
    /// how `movecheck` words the refusal (RFC-0125 §3 M3, row 19).
    pub fn ctor(self) -> bool {
        matches!(self, Callee::Ctor)
    }

    /// Whether the callee STORES what it is handed: neither declared nor a
    /// constructor, which is the third of the same three sentences.
    pub fn stores(self) -> bool {
        !self.declared() && !self.ctor()
    }
}

/// The operation a [`Rhs::Prim`] row performs (RFC-0125 §3 M3, the operation
/// slice).
///
/// §2.3 says the emitter "maps `prim` rows to wasm instructions". A row that
/// carries operands alone maps to nothing: `a + b` and `a - b` were the same
/// row, and no emitter could tell them apart.
///
/// It names the SOURCE's operator and not an opcode, and that is the
/// granularity the census argued for. One operator is many instructions —
/// `i32.add`, `i64.add`, `f64.add`, `i32x4.add` — and what chooses among them
/// is the OPERAND's type, which the checker states at the operand's own node.
/// An opcode in the row would restate the checker there, and RFC-0083's lane
/// counts would arrive as a second table.
///
/// Two of these are not arithmetic and a reader has to know: `&&` and `||`
/// SHORT-CIRCUIT, so an emitter runs the right operand under a branch. This
/// pass reads both operands into the row because a read of either owns
/// nothing and the linear judgment is the same either way, and the operator
/// is what tells a later reader that the second read may not happen.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Op {
    Un(UnOp),
    Bin(BinOp),
    /// A conversion between two scalars: `Int32(n)`, `UInt8(n)`, `Float64(n)`
    /// and their siblings (RFC-0125 §3 M7). The payload is the TARGET, and
    /// the source is the operand's own type, which the checker put on the
    /// name the row reads — the same split every other operator here makes,
    /// for the same reason.
    ///
    /// Which names convert is [`vyrn_frontend::types::numeric_conv_target`],
    /// the one table the checker types the node by, so no pass decides it
    /// twice. What a conversion DOES between a given pair — widen by the
    /// source's signedness, wrap into the target's, saturate across the
    /// int/float line — stays the coercion plan's ([`vyrn_codegen::Rung`]),
    /// which is where both compiled backends already read it.
    Conv(Type),
    /// RFC-0023: a lambda literal. Its operands are the captures the closure
    /// snapshots and its result is the closure value, which is why it is a
    /// prim and not a [`Ctor`] — the parts are READ, where a constructor's
    /// are taken.
    Closure,
}

/// What a [`Rhs::Make`] row constructs (RFC-0125 §3 M3, the operation slice).
///
/// `Make(vs)` was a list of values with no name on it, so a record literal,
/// an array literal, a map literal and a `where`-checked constructor were one
/// row and an emitter read the constructor off the source. `layout.rs` needs
/// the TYPE to place the value and the FIELD each part fills to place its
/// parts, and the row carried neither.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Ctor {
    /// A record literal `T { f: .. }`: the named type, and the field each
    /// part goes into — one name per value, in the order the reader WROTE
    /// them, which is the order the values are in.
    ///
    /// The DECLARATION's order is the layout's, and it is not restated here:
    /// an emitter joins the two by name, which is what it does off the source
    /// today. A row that carried the declaration's order would go stale the
    /// day a field moves.
    Record(String, Vec<String>),
    /// An array literal `[a, b]`: its elements, in order.
    Array,
    /// A map literal `{k: v}`: key then value, one pair per entry, in the
    /// order they were written (RFC-0028 keeps insertion order).
    Map,
    /// `T?(v)` (RFC-0079): the constructor of a `where`-checked type, which
    /// answers an `Option<T>` rather than a `T`.
    Try(String),
    /// A function value made for storage (RFC-0037): the variant of the
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
    /// The stored value was built FROM the place's own value — `xs = xs.push(v)`
    /// hands the buffer back — so the name keeps holding without a release.
    Transferred,
    /// The FIRST build's word at every other store: the place may hold a
    /// value, and the row that releases it is still to be written (RFC-0125
    /// §3 M3, the store slice).
    ///
    /// A store answer is an INPUT to the judgment as well as an output of it,
    /// which is what stopped the slice before this one. This word is what
    /// makes the input harmless: `old` decides a REFUSAL and never a state —
    /// a place written to holds what was written to it, whatever it held
    /// before — so a first build that refuses nothing at a store leaves the
    /// judgment at every later statement exactly where a first build reading
    /// a filled table left it. The kernel then reports the store it finds a
    /// held place at ([`crate::kernel::MissingKind::Store`]), the placer
    /// writes the row, and the second build states [`Old::Released`] or
    /// [`Old::Nothing`] there.
    Pending,
}

#[derive(Debug, Clone)]
pub enum St {
    Let(Name, Rhs),
    Store {
        place: Place,
        value: Val,
        old: Old,
        /// The source line, for a refusal's wording (RFC-0125 M3, third
        /// slice): the kernel names the line a value was moved on and the
        /// line it is used again on, as the checker does.
        line: usize,
        /// The statement the plan keys its store decision by (RFC-0125 §3 M3,
        /// the emitter-reads-the-core slice). [`Site::None`] for a store this
        /// pass made up — a global's initializer, a desugar's temporary — and
        /// for a user container's `c[i] = v`, whose store statements RFC-0091
        /// M2's `place at` rewrite BUILDS: the plan's row stands on one of
        /// those and this pass walks the source, so a reader must fall back
        /// to the plan there.
        site: Site,
        /// Whether the store releases the value it displaces — the plan's
        /// decision, with the mention guard and round eighteen's exceptions
        /// folded in for a name store, which is how both compiled backends
        /// read it.
        ///
        /// A different question from `old`, and both are needed. `old` is
        /// what the KERNEL sees at the place: a place holding nothing that
        /// owns heap displaces nothing, whatever the plan decided about the
        /// statement, and a sub-place's ownership is not a judgment this
        /// pass makes. `releases` is what an EMITTER emits.
        releases: bool,
    },
    /// A release. `site` is the node the plan keys the row by where the plan
    /// has one: the `Stmt::Expr` of a discarded result, or the join and edge
    /// of a Rule N release. [`Site::None`] elsewhere — a `drop` statement, a
    /// scope's own release, an argument temporary, a payload binder, which
    /// [`NameInfo::arg_drop`], [`NameInfo::receiver`] and [`Arm::frees`] name
    /// instead.
    ///
    /// The last field is the line of the `drop` a reader WROTE, and 0 for
    /// every release this pass places. A refusal at a source `drop` names
    /// that line and calls the taker `drop`; a refusal at a placed release
    /// names neither, because a reader has no such statement to change
    /// (RFC-0125 §3 M3, rows 06, 20 and 21).
    ///
    /// The holes are the parts that left the value on this row's path, which
    /// the release walks around ([`Body::drop_holes`]). `None` is the name's
    /// own set ([`NameInfo::holes`]); an edge release carries the set the
    /// kernel found on its edge, because one name is holed on one arm and
    /// whole on the next (`vyxProcessElem` in `std/vyx.vyrn`).
    Drop(Name, Site, usize, Option<Vec<String>>),
    /// A release row the plan placed at an exit, keyed as the plan keys it,
    /// walking the name around `holes`. The kernel checks the set against
    /// the holes its state has there: a row that skips a place still held
    /// is a leak the placer repairs by rewriting the row's set; a row that
    /// walks a place a take left is a double free.
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
    /// A loop, and the source statement it came from: a `while`, whose exit
    /// this pass desugars into a two-way branch at the head, or a `for`. `0`
    /// for a loop this pass made up.
    Loop {
        body: Vec<St>,
        site: usize,
    },
    /// A source block: its own scope, and the site the plan keys its
    /// fall-through release rows by.
    Block {
        site: usize,
        body: Vec<St>,
        /// `region { .. }` (RFC-0004 §4) rather than a plain block: an arena
        /// scope, whose values the exit frees together.
        ///
        /// The block is the same block either way — one scope, one site, the
        /// same release rows — and this pass judges it the same, which is why
        /// the region was a plain `St::Block` until the driver slice
        /// (RFC-0125 §3 M3). What it is NOT the same for is the emission: an
        /// emitter takes a mark on the way in and hands it back on the way
        /// out, and a walk over the statements that could not tell the two
        /// apart emitted neither. The DEPTH stays the emitter's — it is a
        /// counter over the code it is writing, not a fact about the block.
        region: bool,
    },
    /// `site` is the statement's node, or 0 for a break this pass made up.
    Break {
        site: usize,
    },
    Continue {
        site: usize,
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
        /// Every arm carries the enclosing exit: it ends with the `return`
        /// this `match` was the operand of ([`Builder::return_through`]), and
        /// the switch has no join at all. What is still held there is one
        /// ARM's, so the kernel keys the row by the arm rather than by the
        /// exit — the two tables the core reads back per arm are the binders'
        /// and RFC-0114 Rule N's edges (RFC-0125 §3 M3, row 17).
        carries: bool,
        /// The scrutinee is a value the frame MADE, not a place it reads: a
        /// `consume`, a call's result, a literal, a named value the construct
        /// took, or a `Map` lookup, which builds its `Option<V>` rather than
        /// naming an entry (RFC-0028). So the boxes the arms' binders come
        /// out of are the construct's own to give back.
        ///
        /// A different question from `consuming`, and both are needed.
        /// `consuming` is about a NAME: the construct is the last owner of a
        /// binding the reader wrote, so nothing releases it afterwards. This
        /// one is about the BOXES: nobody else frees them. It holds where the
        /// construct took a name or switches on a value nobody else holds,
        /// and never on a declared release's receiver, whose caller frees
        /// the boxes ([`Builder::owns_boxes`]). An emitter frees the boxes
        /// where this is true, and reads nothing else for it.
        ///
        /// Stated here rather than read off the source, which is where each
        /// compiled backend read it until RFC-0125 §3 M3's box slice.
        owns: bool,
        /// The node the plan keys this switch by, which is the one every
        /// [`Arm`] of it carries: an `if let` statement's own node, or the
        /// `match` or `?` EXPRESSION's where the construct is one. `0` for a
        /// switch this pass made up.
        site: usize,
        line: usize,
    },
    /// An expression for its effect, on its line, and the statement it came
    /// from. `0` for a row this pass made up — the `panic` call it states
    /// before a [`St::Trap`].
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
    /// The binders this arm releases at its end, in the order it releases
    /// them — round forty's table, stated by the core (RFC-0125 §3 M3, the
    /// deletion-preparation slice). Each is a `St::Drop` at the end of
    /// `body`, and the holes are the binder's own (`NameInfo::holes`). Named
    /// rather than left to the reader's eye, because the edge drops of a
    /// join follow them and a position is not a key.
    ///
    /// `None` where this pass does not state the answer. Every switch this
    /// pass builds states one — a `match`, an `if let` and a `?` alike — so a
    /// reader needs no second table.
    pub frees: Option<Vec<Name>>,
    pub body: Vec<St>,
    /// Why control reaches this arm — RFC-0125 M7, the tag family.
    pub test: Test,
    /// The `match` (or `if let`, or `?`) this arm belongs to, and which arm
    /// it is — the plan's key for an arm payload free and an edge release.
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
/// `consume` parameter takes it — RFC-0125 M7.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Payload {
    /// A hole the scrutinee's release walks around, spelled `.Variant.i`.
    Hole(String),
    /// Nothing may leave: the scrutinee's type, spelled here, declares a
    /// `release` that reads every payload.
    Sealed(String),
}

/// How one [`Arm`] of a [`St::Switch`] is chosen.
///
/// The arms are tried in order, so a [`Test::Else`] runs when no arm before it
/// did. The index is the SOURCE's arm order and the tag is the variant's
/// position in the scrutinee's list, and the two are different numbers: `match
/// o { None => a, Some(n) => b }` has arm 0 at tag 0 only because the reader
/// wrote it that way.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Test {
    /// The scrutinee's tag is this one.
    Tag(u64),
    /// No arm before it was chosen: `_` (RFC-0121's refutable `let`), and the
    /// `else` block of an `if let`.
    Else,
    /// The `Bool` this name holds is true: a predicate the PROGRAM states,
    /// which the builder calls before the switch. `?` on a declared
    /// `Fallible` (RFC-0080 M3) asks the impl's `isSuccess`.
    Holds(Name),
}

impl Test {
    /// The name the test reads, where it reads one.
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
    /// `export extern fn`: the caller is JS, and it releases every String the
    /// call hands back (RFC-0012 M2, RFC-0089 M3b). So a return of a borrow
    /// gets its own sentence, and `.copy()` is the only way out that exists
    /// (RFC-0125 §3 M3, the census, row 17). A lambda frame carries the flag
    /// of the body that holds it, which is how `movecheck::refuse_return`
    /// reads it — `cur_fn` is the enclosing function either way.
    pub export: bool,
    pub names: Vec<NameInfo>,
    pub params: Vec<Name>,
    pub stmts: Vec<St>,
    /// The bodies of the lambdas this body holds, each a frame of its own
    /// (RFC-0125 M3, third slice): its parameters and its captures are
    /// borrowed inputs, its own bindings are ordinary, and the plan keys its
    /// rows by its own nodes under the enclosing function's name.
    pub lambdas: Vec<Body>,
    /// RFC-0125 §3 M3, the third derivation slice: every construct that may
    /// take the value it was handed, with the name that value has here and
    /// the shape of the construct. Whether it DOES take it is decided over
    /// this body by [`last_owner`], and the second build acts on what that
    /// decided.
    pub(crate) cands: Vec<(usize, Name, Cand)>,
    /// RFC-0125 §3 M3: the `for` statements whose container release walks the
    /// BUFFER alone, keyed by the loop's own node.
    ///
    /// It is the second half of [`Cand::Elem`]. Where every element left
    /// through the loop variable, each turn owns its element and the deep
    /// walk would free values somebody else now owns; what the loop still
    /// owns is the growable array's buffer, which is field 0 of the triple.
    /// The first half says whose an element is; this says how the container
    /// goes back, and an emitter reads it at the loop rather than reading a
    /// plan row's KIND.
    pub(crate) loop_buffers: Vec<usize>,
    /// `(exit, loop)`: the `return`, `?` or `break` node that releases the
    /// elements no turn of that `for` reached, innermost loop first
    /// ([`Facts::unreached`]).
    pub(crate) unreached: Vec<(usize, usize)>,
}

/// What a candidate construct is, which is what [`last_owner`] has to ask of
/// it — RFC-0125 §3 M3, the take-rule slice.
#[derive(Clone, Copy, PartialEq, Eq, Debug)]
pub(crate) enum Cand {
    /// A `match`, an `if let` or a `?`: the core switches on the value, and
    /// the construct is its last owner where nothing reads the name after
    /// the switch.
    Switch,
    /// A `for`: the core loops over the value, and the construct is its last
    /// owner where the name's last read is INSIDE the loop.
    Loop,
    /// A `for`'s VARIABLE: every element left the container through it, so
    /// each turn owns its element and the container's release frees the
    /// buffer alone. The name is handed on somewhere in the body — into a
    /// `consume` position, a literal, a store, a map key or a `return` —
    /// rather than only read.
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

    /// Which rows each SOURCE statement produced, by the node the row names —
    /// RFC-0125 §3 M3, the interleave slice.
    ///
    /// A driver that picks its walk per FUNCTION can only delete an AST arm
    /// when every body of the corpus goes through the core. The unit this
    /// answers for is the statement: a reader hands one source statement to
    /// the walk that carries it and the other statement to the other walk, and
    /// an arm goes when no occurrence of its form reaches it any more.
    ///
    /// The correspondence is already stated. A `let`'s row carries the node
    /// the plan keys the binding by ([`NameInfo::binding`]), and `St::Store`,
    /// `St::If`, `St::Return`, `St::Break`, `St::Continue`, `St::Loop`,
    /// `St::Do` and `St::Switch` each carry the statement's own node — the
    /// last three since the site slice, which is what took a `while`, a `for`,
    /// an expression statement and an `if let` off the floor. What this adds is the run BEFORE that row: the
    /// temporaries the statement computes first, which are the `let`s of
    /// minted names it reads, walked back until a row that is not one. A
    /// statement whose row names no node is not in the map, and the reader
    /// falls back — which is what a `for`, a `match` and an expression
    /// statement each do today.
    pub fn rows_by_statement(&self) -> HashMap<usize, Vec<St>> {
        let mut out = HashMap::new();
        let mut twice = Vec::new();
        self.rows_in(&self.stmts, &mut out, &mut twice);
        // One statement, two runs: a `return` the pass copied into every arm
        // of a `match` it was the operand of ([`Builder::return_through`]).
        // Which run is that statement's is not a question this map answers, so
        // it answers neither.
        for at in twice {
            out.remove(&at);
        }
        out
    }

    fn rows_in(&self, ss: &[St], out: &mut HashMap<usize, Vec<St>>, twice: &mut Vec<usize>) {
        for (i, s) in ss.iter().enumerate() {
            match s {
                St::If { then, els, .. } => {
                    self.rows_in(then, out, twice);
                    self.rows_in(els, out, twice);
                }
                St::Loop { body: b, .. } | St::Block { body: b, .. } => self.rows_in(b, out, twice),
                St::Switch { arms, .. } => {
                    for a in arms {
                        self.rows_in(&a.body, out, twice);
                    }
                }
                _ => {}
            }
            let Some(node) = self.node_of(s) else {
                continue;
            };
            let mut need: Vec<Name> = Vec::new();
            names_in(s, &mut need);
            // A temporary's release names no node, so it is the run's that
            // binds the temporary: before the statement, where the builder
            // frees an argument before the row that reads the result, and
            // after it. Without it a temporary that owns heap is never freed
            // where the rows emit the statement.
            let temp = |n: &Name, run: &[St]| {
                !self.names[*n as usize].bound_by_let
                    && run.iter().any(|r| matches!(r, St::Let(m, _) if m == n))
            };
            let mut start = i;
            while start > 0 {
                match &ss[start - 1] {
                    // The releases an exit runs are the exit's own rows: the
                    // row names the node the plan keys the exit by, which is
                    // this statement's (RFC-0125 M7).
                    St::Row { site, .. } if *site == node => {}
                    // The only loop this pass makes up: the elements an
                    // exit leaves unreached ([`Builder::release_unreached`]).
                    St::Loop { site: 0, .. } => {}
                    St::Let(n, rhs)
                        if !self.names[*n as usize].bound_by_let && need.contains(n) =>
                    {
                        names_in_rhs(rhs, &mut need);
                    }
                    St::Drop(n, ..) if !self.names[*n as usize].bound_by_let => {}
                    _ => break,
                }
                start -= 1;
            }
            while let Some(d) = (start..i)
                .rev()
                .find(|&d| matches!(&ss[d], St::Drop(n, ..) if !temp(n, &ss[start..=i])))
            {
                start = d + 1;
            }
            let mut end = i;
            while matches!(ss.get(end + 1), Some(St::Drop(n, ..)) if temp(n, &ss[start..=i])) {
                end += 1;
            }
            if out.insert(node, ss[start..=end].to_vec()).is_some() {
                twice.push(node);
            }
        }
    }

    /// The holes a release row of `n` walks around: the row's own set, else
    /// the name's.
    pub fn drop_holes<'b>(&'b self, n: Name, row: &'b Option<Vec<String>>) -> &'b [String] {
        row.as_deref().unwrap_or(&self.names[n as usize].holes)
    }

    /// How many times each of this body's names is READ, which is what an
    /// emitter has to know before it can leave a value on an operand stack
    /// rather than in a local (RFC-0125 §3 M3, the driver slice).
    pub fn reads(&self) -> Vec<u32> {
        let mut out = vec![0u32; self.names.len()];
        count_reads(&self.stmts, &mut out);
        out
    }

    /// How many times a row names each of this body's names, itself or under
    /// it, a binding and a release included. [`extent_ends`] compares a list
    /// against it to know that no row outside the list names a name.
    pub fn occurrences(&self) -> Vec<u32> {
        let mut ns = Vec::new();
        self.stmts.iter().for_each(|s| names_in(s, &mut ns));
        let mut out = vec![0u32; self.names.len()];
        for n in ns {
            out[n as usize] += 1;
        }
        out
    }

    /// The source statement a row names, where it names one. `0` is this
    /// pass's own word for "a row I made up", so it is no statement's node.
    fn node_of(&self, s: &St) -> Option<usize> {
        match s {
            St::Let(n, _) if self.names[*n as usize].bound_by_let => {
                self.names[*n as usize].binding
            }
            St::Store {
                site: Site::Node(at),
                ..
            } => Some(*at),
            // A `?` states its exit as a `return` whose site is the EXPRESSION,
            // so it names no statement of the source.
            St::Return {
                site,
                is_try: false,
                ..
            } => (*site != 0).then_some(*site),
            St::If { site, .. }
            | St::Break { site }
            | St::Continue { site }
            | St::Loop { site, .. }
            | St::Do { site, .. }
            | St::Switch { site, .. } => (*site != 0).then_some(*site),
            // A `region` names its block, which is the node its statement's
            // reader asks by: the site is the block's scope, and no other
            // statement's run is keyed there.
            St::Block {
                site, region: true, ..
            } => Some(*site),
            _ => None,
        }
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
                    .map(|(v, c)| format!("{:?} {}", c, self.val(v)).to_lowercase())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Rhs::Prim(op, vs, _) => format!(
                "prim {}({})",
                match op {
                    Op::Un(o) => format!("{o:?}").to_lowercase(),
                    Op::Bin(o) => format!("{o:?}").to_lowercase(),
                    Op::Conv(t) => format!("conv {t}"),
                    Op::Closure => "closure".into(),
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
                    place, value, old, ..
                } => out.push_str(&format!(
                    "{pad}{} = {}  ({:?})\n",
                    self.place(place),
                    self.val(value),
                    old
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

/// A construct this slice does not lower. The instance is neither accepted nor
/// refused; the corpus test counts these by `what`.
#[derive(Debug, Clone)]
pub struct Gap {
    pub what: &'static str,
    /// The particular: a callee's name, a binding's name. Empty when the
    /// construct alone says it.
    pub detail: String,
    pub line: usize,
    /// RFC-0125 §3 M3, the checker's deletion path: this is not a construct
    /// the slice cannot lower. It is a rule the PROGRAM breaks, in the
    /// checker's own sentence, and the placer turns it into a refusal the
    /// same way it turns the kernel's own. A rule about the KEYWORD belongs
    /// here rather than in the kernel, because the kernel has no keywords —
    /// `consume make()` and `make()` denote the same value.
    pub rule: Option<String>,
}

/// A field read or an element read: a place, not a value the reader owns.
fn is_place_read(e: &Expr) -> bool {
    match e {
        Expr::Field { expr, .. } => {
            matches!(&**expr, Expr::Var { .. } | Expr::Field { .. }) || is_place_read(expr)
        }
        Expr::Call { name, args, .. } => {
            name == "@at" && args.len() == 2 && is_place_read(&args[0])
        }
        Expr::Var { .. } => true,
        _ => false,
    }
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

/// The two refusals a `consume` gets when what follows it names no place.
/// `by_loop` picks the form's wording: `for x in consume xs` says the loop
/// already owns a container, and a prefix `consume` says the value is already
/// owned. Both are the checker's own sentences (`movecheck::TakeForm`), and
/// both are stated from the SYNTAX, so no heapless counterexample to either
/// exists (RFC-0125 §3 M3, rows 08 and 09).
fn take_names_a_place(e: &Expr, line: usize, by_loop: bool) -> Result<(), Gap> {
    if vyrn_frontend::ast::place_path(e).is_some() {
        return Ok(());
    }
    if let Some((root, path)) = vyrn_frontend::project::element_path(e) {
        // A container element is the one place that CAN hold a hole at run
        // time, and `swapRemove` already spells it (RFC-0011).
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

/// Which candidate constructs are their value's LAST owner — RFC-0125
/// §3 M3, the third derivation slice and the take-rule slice, and the rule
/// `own.rs`'s `consuming_matches` used to state off `movecheck`'s event
/// stream.
///
/// The question is an order over the core the first build made, and it is
/// the same question for every shape a candidate has. A construct takes the
/// value it was handed where nothing reads that name after it — a payload
/// binder is a name of its own, so an arm reading the payload is not a read
/// of the scrutinee — and where the binding and the construct stand under the
/// same loops, so one value is not taken twice.
///
/// A release row is NOT a read (RFC-0125 §3 M3, the walk's deletion). The
/// take is decided over the core's own statements, and the release rows are
/// derived FROM the take: the placer runs the kernel over the body this
/// decision made, and a value the construct took is gone at the scrutinee's
/// exit, so no row is placed there. The other order — the row deciding the
/// take — made the answer depend on a table this pass writes, so the facts
/// rebuild read rows the placer had just added and seeded a different take
/// than the rows were placed for.
///
/// A [`Cand::Switch`] asks it of the switch the core emitted: the name's
/// last read is the switch's own. A [`Cand::Loop`] asks it of the loop: the
/// name's last read is DEEPER than the frame it was bound in, so it is
/// inside the loop and nothing after the loop reads it.
///
/// Every screen the fold applied is here in the core's own terms: its order
/// window is an order, its loop test is a nesting depth, and its "no read of
/// the scrutinee's own NAME" is the difference between two names.
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
    /// Whether the name was HANDED ON rather than only read: the element
    /// question a [`Cand::Elem`] asks.
    handed: Vec<bool>,
    switches: Vec<(usize, Name, usize, usize)>,
    order: usize,
    depth: usize,
}

impl Reads {
    /// A value in a position that TRANSFERS it: a declared `consume`
    /// argument, a part of a literal, a stored value, a map key a store
    /// writes at, a returned value.
    fn hand(&mut self, v: &Val) {
        if let Val::Name(n) = v {
            self.handed[*n as usize] = true;
        }
        self.val(v);
    }

    /// The place of a STORE: the key it writes at is handed to the container.
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
            // A take out of a sub-place hands that part on, so the name is
            // not whole any more and what is left of it is this turn's:
            // `out.push(consume p.value)` inside a `for p in ..`
            // (`std/tw.vyrn`'s `twSafelist`, `std/rpc.vyrn`'s
            // `rpcApplyConfig`).
            Rhs::Take(p) => {
                self.place(p);
                if let Some(Val::Name(n)) = root_name(p) {
                    self.handed[n as usize] = true;
                }
            }
            Rhs::Call { args, .. } => {
                for (v, c) in args {
                    if *c == vyrn_frontend::ast::Capability::Consume {
                        self.hand(v);
                    } else {
                        self.val(v);
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

/// A `match`'s own source lines: its head, and the last line an arm's value
/// starts on. Read by [`Builder::takes_scrutinee`], which asks whether the
/// note on a named scrutinee was written inside this construct.
///
/// A BLOCK arm (RFC-0118) yields nothing, so it hands no payload out as a
/// value and adds no line here.
fn arms_span(line: usize, arms: &[MatchArm]) -> (usize, usize) {
    let last = arms.iter().fold(line, |m, a| match &a.body {
        ArmBody::Expr(e) => m.max(e.line()),
        ArmBody::Block(_) => m,
    });
    (line, last)
}

/// The kind of an expression, for a gap's detail.
///
/// The word for a form is [`Node::kind`]'s. What a gap wants on top of a dump
/// axis is two distinctions: the five literals answer as one, and a builtin
/// call is named apart from a user's.
fn expr_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => "literal",
        Expr::Call { name, .. } if name.starts_with('@') => "builtin call",
        _ => Node::Expr(e).kind(),
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

/// A rule the program breaks, stated by the core (RFC-0125 §3 M3, the
/// checker's deletion path). The lowering stops here, as it does at a gap,
/// and the placer reports `message` at `line` the way it reports the
/// kernel's own refusals.
fn refuse<T>(message: String, line: usize) -> Result<T, Gap> {
    Err(Gap {
        what: "a rule the program breaks",
        detail: String::new(),
        line,
        rule: Some(message),
    })
}

/// What a builtin's specification row states about its operands and its
/// result (RFC-0125 §2.1). A builtin with such a row is a `call`, not a gap.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Spec {
    /// Each operand at the stated type, in order, and a result at the stated
    /// type. The emitter writes one instruction between them.
    Typed(Vec<Type>, Type),
    /// One operand, at whatever type the row put on the name it reads, and a
    /// result of that same type.
    OwnType,
    /// One operand, at whatever type the row put on the name it reads, and a
    /// result at the stated type. The checker types the operand over a union
    /// (`print`, `@str`), and the emitter chooses the rendering by the
    /// operand's own type, as it chooses an instruction for [`Op::Conv`].
    Renders(Type),
    /// A message at `String`, and for `@panicAt` the site as a string
    /// literal. The call writes the line and returns to nobody; the
    /// [`St::Trap`] the builder states after it is what ends the path.
    /// `serveStream`'s message is the frontend's sentence, stated as a
    /// literal in place of the stream, which a compiled build never pulls.
    Traps,
    /// A receiver first, rebuilt in place by the runtime. An array's
    /// receiver takes at most one operand at whatever type the row put on
    /// its name: the runtime writes the new triple into the receiver's own
    /// storage, so the result is the receiver, and the store the builder
    /// states after the call puts back what is already there.
    /// `@strAppend`'s receiver is a String accumulator ([`NameInfo::grows`])
    /// and its operands are Strings: the runtime grows the buffer when the
    /// accumulator's ownership word says this path allocated it and copies
    /// it otherwise, and the store after the call puts the new address
    /// into the name.
    Rebuilds,
    /// A SIMD operation (RFC-0083): the lane constructors, a lane read or
    /// write, a mask reduction, and a load or store of consecutive array
    /// elements. The vector operand's own type chooses the instruction, and a
    /// lane index is a literal the row carries, which the checker proved
    /// constant and in range.
    Lanes,
    /// A host import a compiled generator calls while it runs (RFC-0076 M7,
    /// RFC-0054): `@codeText`, `raw`, `rawAt` and `@codeSplice` hand the host
    /// a piece and get back a `Code` handle, and `render` hands it a handle
    /// and gets back a String. `@codeSplice`'s tag is its operand's own type.
    /// Outside a generator no body reaches one.
    Host,
    /// The call is a call to the named function, which the program links:
    /// an entry a generator host's engine synthesizes, or a `std` function a
    /// builtin routes to (`loader::RT_MODULES`). The emitter reads it as a
    /// declared callee, and where the program does not define the function it
    /// reads no such call.
    Routes(&'static str),
    /// An array receiver, and for `@swapRemove` an index at `Int64`. The call
    /// shrinks the receiver in its own storage and hands back what it
    /// removed: `@pop` an `Option` of the last element, `@swapRemove` the
    /// element at the index.
    Removes,
    /// Operands at whatever type the row put on their names, and a result at
    /// the stated type, whose parameters the operands' types solve, that the
    /// call builds in storage of its own. The caller lands it as it lands any
    /// aggregate result.
    Builds(Type),
}

/// Every builtin the row specifies, by name.
///
/// The emitter answers each of these from the row and nowhere else
/// (`vyrn_codegen::direct::Fn_::core_call`), so a name added here needs an
/// emission there; the match over [`Spec`] makes a new KIND a compile error,
/// and the codegen test `builtin_rows_all_emit` refuses a [`Spec::Typed`] row
/// with no instruction.
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
            // The IEEE-754 bit views (RFC-0078 M4a): the same 64 bits read at
            // the other type, which is why neither is a conversion.
            one("floatBits", &Type::Float, &u64_),
            one("floatFromBits", &u64_, &Type::Float),
            // RFC-0083: one lane value broadcast to every lane.
            one("@f32x4Splat", &Type::Float32, &f4),
            one("@i32x4Splat", &i32_, &Type::I32x4),
            one("@f64x2Splat", &Type::Float, &d2),
            // The lane-wise arithmetic of both widths. `min` and `max` take
            // two vectors; the roundings and the root take one.
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
            // `x.copy()` (RFC-0089 M1b) hands back the receiver's value with
            // heap of its own, so its result is the receiver's type and the
            // row states it by naming the operand.
            ("@copy", Spec::OwnType),
            ("print", Spec::Renders(Type::Unit)),
            ("@str", Spec::Renders(Type::Str)),
            ("panic", Spec::Traps),
            (vyrn_frontend::ast::PANIC_AT, Spec::Traps),
            ("serveStream", Spec::Traps),
            ("@push", Spec::Rebuilds),
            ("@reserve", Spec::Rebuilds),
            ("@clear", Spec::Rebuilds),
            ("@append", Spec::Rebuilds),
            ("@copyFrom", Spec::Rebuilds),
            ("@strAppend", Spec::Rebuilds),
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
            (
                "moduleInterface",
                Spec::Routes(vyrn_frontend::checker::GEN_ENTRY_MODULE_INTERFACE),
            ),
            ("lex", Spec::Routes(vyrn_frontend::checker::GEN_ENTRY_LEX)),
            ("@pop", Spec::Removes),
            ("@swapRemove", Spec::Removes),
            ("bytes", Spec::Builds(Type::Array(Box::new(u8_.clone())))),
            (
                "stringFromBytes",
                Spec::Builds(Type::result(Type::Str, Type::Str)),
            ),
            // RFC-0014 and RFC-0044's I/O: the runtime writes the whole result
            // into the caller's slot, and a failure is its `Err` message.
            ("args", Spec::Builds(Type::Array(Box::new(Type::Str)))),
            ("readLine", Spec::Builds(Type::option(Type::Str))),
            ("readFile", Spec::Builds(Type::result(Type::Str, Type::Str))),
            (
                "readFileBytes",
                Spec::Builds(Type::result(Type::Array(Box::new(u8_)), Type::Str)),
            ),
            (
                "fsyncFile",
                Spec::Builds(Type::result(Type::Bool, Type::Str)),
            ),
            (
                "writeFile",
                Spec::Builds(Type::result(Type::Bool, Type::Str)),
            ),
            (
                "renameFile",
                Spec::Builds(Type::result(Type::Bool, Type::Str)),
            ),
            (
                "writeFileBytes",
                Spec::Builds(Type::result(Type::Bool, Type::Str)),
            ),
            ("parse", Spec::Builds(Type::option(Type::Int))),
            (
                "listDir",
                Spec::Builds(Type::result(Type::Array(Box::new(Type::Str)), Type::Str)),
            ),
            (
                "listDirKinds",
                Spec::Builds(Type::result(Type::Array(Box::new(Type::Str)), Type::Str)),
            ),
            // A snapshot of a map's keys, at the prelude's own parameter.
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
                .map(|(builtin, f)| (*builtin, Spec::Routes(*f))),
        )
        .collect()
    })
}

/// The row [`builtin_rows`] holds for `name`, or `None` where the name is not
/// a builtin the row specifies.
pub fn builtin_row(name: &str) -> Option<&'static Spec> {
    builtin_rows()
        .iter()
        .find(|(n, _)| *n == name)
        .map(|(_, s)| s)
}

/// Every gap of `body`: the shapes among its rows that no emitter reads from
/// the core, in source order, each named once.
///
/// An empty answer means the rows carry the body end to end. A tag names the
/// family one form track closes: `Call:<who>:<name>` for a callee the
/// emitter's function table does not answer, `Read:<kind>` and
/// `Take:<kind>` for a place, `Opaque:<what>` for a row
/// that names no value, and `Lambda`.
/// `tests/coredrive.rs` ranks the tags into its classes, and
/// `VYRN_GAP_TALLY` tables them over the gate list.
pub fn gaps(body: &Body) -> Vec<String> {
    let mut out = Vec::new();
    gaps_of(&body.stmts, &mut out);
    let mut seen = std::collections::HashSet::new();
    out.retain(|t| seen.insert(t.clone()));
    out
}

/// Ends every list of `ss` at its `trap`, because nothing after one runs.
/// An `if` or a `switch` whose every arm ends at one is a `trap` too.
///
/// The builders extend a list after a `panic` in value position: the join's
/// store, the call its argument feeds, an arm's releases. A builder cannot
/// cut its own list, because its caller extends the list after it returns.
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

fn gaps_of(ss: &[St], out: &mut Vec<String>) {
    for s in ss {
        match s {
            St::Let(_, r) | St::Do { rhs: r, .. } => gaps_rhs(r, out),
            St::Store { place, value, .. } => {
                gaps_place(place, out);
                gaps_val(value, out);
            }
            // The emitter reads a release off the row it stands on, whether
            // the plan placed it at an exit or the core states it as a
            // statement (RFC-0125 M7), so neither is a gap.
            St::Drop(..) | St::Row { .. } => {}
            St::If {
                cond, then, els, ..
            } => {
                gaps_val(cond, out);
                gaps_of(then, out);
                gaps_of(els, out);
            }
            // Since the loop slice the exit is the row's: the pass makes up the
            // two-way branch and the `break` at the head of the loop it
            // desugared, and a walk emits wasm's conditional branch for it.
            St::Loop { body: b, .. } | St::Block { body: b, .. } => gaps_of(b, out),
            St::Break { .. } | St::Continue { .. } | St::Trap => {}
            St::Return { value, .. } => {
                if let Some(v) = value {
                    gaps_val(v, out);
                }
            }
            // Since RFC-0125 M7 the arm says what reaches it, so the emitter
            // chooses the arm off the row (`direct::Fn_::core_switch`).
            St::Switch { on, arms, .. } => {
                gaps_val(on, out);
                for a in arms {
                    gaps_of(&a.body, out);
                }
            }
        }
    }
}

fn gaps_rhs(r: &Rhs, out: &mut Vec<String>) {
    let vals = |vs: &[Val], out: &mut Vec<String>| {
        for v in vs {
            gaps_val(v, out);
        }
    };
    match r {
        Rhs::Val(v) => gaps_val(v, out),
        // Since RFC-0125 M7 every place is read off the row: a name, a
        // global, a field and an element at the address the reader computes
        // (`direct::Fn_::core_read`), and a key through the runtime's lookup
        // (`direct::Fn_::map_at`). A take of a key has no reader.
        Rhs::Read(p) => gaps_place(p, out),
        Rhs::Take(p) => {
            if matches!(p, Place::Key(..)) {
                out.push("Take:Key".into());
            }
            gaps_place(p, out);
        }
        // Since the callee slice the row says WHO: a function this program
        // declares is one the emitter's own table answers for, and only the
        // other eight kinds are still waiting on a row.
        Rhs::Call {
            callee, args, kind, ..
        } => {
            // Since RFC-0125 M7 a variant constructor is read off the row too
            // (`direct::Fn_::core_make`): it is a layout MADE, with a tag in
            // front of its payload, and not the `call` the table answers.
            // Since RFC-0125 M7's builtin family a builtin the row SPECIFIES
            // is read off the row too (`direct::Fn_::core_call`): the row
            // names the operand types and the result, and the emitter's table
            // names the instruction.
            //
            // A handed-back receiver is no gap of its own (RFC-0125 M7): the
            // builder states `out.push(v)` as the call and the store that puts
            // the result back, two rows an emitter reads, for a name, a field,
            // an element and a global alike. What such a body waits on is its
            // CALLEE, which is `@push` and its siblings, and the tag says so.
            if !matches!(kind, Callee::Fn | Callee::Ctor | Callee::Named)
                && builtin_row(callee).is_none()
            {
                out.push(format!("Call:{kind:?}:{callee}"));
            }
            for (v, _) in args {
                gaps_val(v, out);
            }
        }
        Rhs::Prim(Op::Closure, vs, _) => {
            out.push("Lambda".into());
            vals(vs, out);
        }
        Rhs::Prim(_, vs, _) => vals(vs, out),
        // Since RFC-0125 M7 a record literal, an array literal and a map
        // literal are read off the row (`direct::Fn_::core_make`), so none is
        // a gap. What refuses a part the emitter cannot place is the emitter's
        // own screen, the way a `Callee::Fn` whose parameter crosses by
        // address is not a gap either. A checked construction `T?(v)` is read
        // there too.
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
    /// The lines already appended. A body is built twice where a scrutinee is
    /// seeded, and again by every host that compiles it, and the histogram
    /// counts bodies.
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
            // The field is space-separated and a gap names a construct in
            // words, so the words are joined.
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
                // A body the rows carry end to end is a line too, with no gap
                // in either field: the denominator of every table the tally
                // answers is the tally's own.
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

/// Build the core of one instance.
///
/// Twice where the body has a `match` over a NAMED scrutinee whose note says
/// this construct gave the value away: the first build records the candidate
/// and takes nothing, [`last_owner`] reads the core it made, and the second
/// build takes the scrutinees it named. A body with no candidate is built
/// once (RFC-0125 §3 M3, the third derivation slice).
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

/// What the rows of a body say per expression node: the type it must end up
/// as, the type its own code produces, and at a generic call the type
/// arguments the checker solved ([`crate::Row::solved`]).
type RowFacts = (
    HashMap<usize, Type>,
    HashMap<usize, Type>,
    HashMap<usize, Vec<(String, Type)>>,
);

fn row_facts(rows: &[crate::Row<'_>]) -> RowFacts {
    let (mut types, mut produced, mut solved) = (HashMap::new(), HashMap::new(), HashMap::new());
    for r in rows {
        if let Node::Expr(_) = r.node {
            if let Some(t) = r.ty.as_ref().or(r.has.as_ref()) {
                types.insert(r.node.id(), t.clone());
            }
            if let Some(t) = r.has.as_ref().or(r.ty.as_ref()) {
                produced.insert(r.node.id(), t.clone());
            }
            if !r.solved.is_empty() {
                solved.insert(r.node.id(), r.solved.clone());
            }
        }
    }
    (types, produced, solved)
}

fn build_seeded(
    program: &Program,
    inst: &Instance<'_>,
    own: &Ownership,
    seed: &std::collections::HashSet<usize>,
) -> Result<Body, Gap> {
    let (types, produced, solved) = row_facts(&inst.rows);
    // The placed releases, by the exit they are at — the PLAN's own rows and
    // not the instance's copy of them. The copy is made where the lowering
    // names the instance, which is before [`augment`] places the rows the plan
    // was missing, so the second build read a plan that was one pass old and
    // stated none of them. What the copy carries and the plan does not is the
    // substituted type a `Deep` walks, and no row below reads a kind.
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
            unreached: Vec::new(),
        },
        scope: Vec::new(),
        by_binding: HashMap::new(),
        temps: 0,
        pending_receiver: None,
        drain: 0,
        after: Vec::new(),
        after_of_rhs: Vec::new(),
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
        region: 0,
        ret: None,
        released: None,
    };
    let f: &Function = inst.func;
    // The instance's substitution, so a parameter's type here is the type the
    // instance has rather than the declaration's (RFC-0125 §3 M6, finding 14):
    // `map<Int64, Int64>`'s `f` is `fn(Int64) -> Int64`, which is the shape
    // RFC-0037 collected its stored sources under. Every other type in the
    // core comes from the instance's rows and is substituted already.
    let subst: HashMap<String, Type> = inst.subst.clone().into_iter().collect();
    b.ret = Some(vyrn_frontend::types::substitute(&f.ret, &subst));
    // A declared release (`impl Owned for T { fn release(consume self) }`) IS
    // the release of `self`: its body frees the parts, and nothing releases
    // `self` again — so `self` is not a name the kernel owns there.
    let is_release = b.proto.is_release_fn(&f.name);
    for p in &f.params {
        let pty = vyrn_frontend::types::substitute(&p.ty, &subst);
        let owned = p.capability == Capability::Consume && b.owns(&pty) && !is_release;
        let n = b.name(&p.name, pty, owned, f.line);
        if is_release {
            b.released = Some(n);
        }
        // RFC-0089 rule 2: a `read` or `modify` parameter may be observed and
        // passed on, never taken. The kernel refuses the take and needs the
        // capability to word it (RFC-0125 §3 M3, the census, rows 11 to 34).
        //
        // A must-use type is the exception RFC-0075 M1 states: "a stream
        // PARAMETER carries the obligation into the callee", whatever the
        // capability says, so the callee is the one that disposes of it and
        // `boxStream(s)` is not a take of the caller's value. The exception is
        // about the TAKE and it is recorded as such: the capability is still
        // the parameter's, and a refusal about a second name for it says so
        // (RFC-0125 §3 M3, row 21 over `examples/mustuse_abandoned.vyrn`).
        b.body.names[n as usize].must_use_param =
            b.proto.must_use(&b.body.names[n as usize].ty.clone());
        b.body.names[n as usize].borrow_kind = param_borrow(p.capability, &p.name);
        b.scope.push((p.name.clone(), n));
        b.keyed(n, p as *const _ as usize);
        b.body.params.push(n);
    }
    b.appends = crate::append::append_candidates(&f.body);
    let mut out = Vec::new();
    b.block(&f.body, &mut out)?;
    cut(&mut out);
    b.body.stmts = out;
    Ok(b.body)
}

/// The module-state initializer (RFC-0013) as a body of its own.
///
/// It is a body and no function of the program: every `let` at module scope
/// is a store into the global it names, run once at `_start`, and the place
/// held nothing before it. Its name is the empty one, which is the name the
/// checker records a lambda written in it under (RFC-0037's
/// `StoredLambda::defined_in`), so a call through a value of that lambda's
/// type is judged over its frame rather than nowhere (RFC-0125 §3 M6,
/// finding 14).
///
/// A `test` (RFC-0015) or `bench` (RFC-0055) body is a body too; it is
/// [`build_outside`]'s, because it is a BLOCK and this one is a list of
/// stores.
pub fn build_module_state<'a>(
    program: &'a Program,
    own: &'a Ownership,
    rows: &[crate::Row<'a>],
) -> Result<Body, Gap> {
    let seed = std::collections::HashSet::new();
    let seed = &seed;
    let (types, produced, solved) = row_facts(rows);
    let mut b = Builder {
        program,
        own,
        proto: &own.proto,
        types,
        produced,
        solved,
        placed: HashMap::new(),
        body: Body {
            name: String::new(),
            file: None,
            export: false,
            names: Vec::new(),
            params: Vec::new(),
            stmts: Vec::new(),
            lambdas: Vec::new(),
            cands: Vec::new(),
            loop_buffers: Vec::new(),
            unreached: Vec::new(),
        },
        scope: Vec::new(),
        by_binding: HashMap::new(),
        temps: 0,
        pending_receiver: None,
        drain: 0,
        after: Vec::new(),
        after_of_rhs: Vec::new(),
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
        region: 0,
        ret: None,
        released: None,
    };
    let mut out = Vec::new();
    for g in &program.globals {
        let v = b.val(&g.init, &mut out)?;
        out.push(St::Store {
            place: Place::Global(g.name.clone()),
            value: v,
            old: Old::Nothing,
            line: g.line,
            site: Site::None,
            releases: false,
        });
    }
    cut(&mut out);
    b.body.stmts = out;
    Ok(b.body)
}

/// A body that is no function of the program and no module-state
/// initializer: a `test` (RFC-0015) or a `bench` (RFC-0055).
///
/// It is a block with no parameters, checked under the synthetic
/// `test@<i>` / `bench@<i>` name that `own`'s release plan is keyed by. The
/// checker used to check a CLONE of the block, so nothing typed the nodes
/// `own` and the lowering walk and the core was one gap per expression; the
/// checker checks the real nodes now (RFC-0125 §3 M6, seventh slice), and
/// the lambdas the body holds get a frame like any other function's.
pub fn build_outside<'a>(
    program: &'a Program,
    own: &'a Ownership,
    name: &str,
    file: Option<String>,
    block: &Block,
    rows: &[crate::Row<'a>],
) -> Result<Body, Gap> {
    let none = std::collections::HashSet::new();
    let first = build_outside_seeded(program, own, name, file.clone(), block, rows, &none)?;
    let seed = last_owner(&first);
    if seed.is_empty() {
        return Ok(first);
    }
    build_outside_seeded(program, own, name, file, block, rows, &seed)
}

#[allow(clippy::too_many_arguments)]
fn build_outside_seeded<'a>(
    program: &'a Program,
    own: &'a Ownership,
    name: &str,
    file: Option<String>,
    block: &Block,
    rows: &[crate::Row<'a>],
    seed: &std::collections::HashSet<usize>,
) -> Result<Body, Gap> {
    let (types, produced, solved) = row_facts(rows);
    // The plan's rows for this body. No substitution: the body has no type
    // parameters, so `own`'s answer is already the concrete one.
    let no_steps: Vec<Release> = Vec::new();
    let steps = own.releases.get(name).unwrap_or(&no_steps);
    let mut placed: HashMap<(Exit, usize), Vec<&Release>> = HashMap::new();
    for r in steps {
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
            name: name.to_string(),
            file,
            export: false,
            names: Vec::new(),
            params: Vec::new(),
            stmts: Vec::new(),
            lambdas: Vec::new(),
            cands: Vec::new(),
            loop_buffers: Vec::new(),
            unreached: Vec::new(),
        },
        scope: Vec::new(),
        by_binding: HashMap::new(),
        temps: 0,
        pending_receiver: None,
        drain: 0,
        after: Vec::new(),
        after_of_rhs: Vec::new(),
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
        region: 0,
        ret: None,
        released: None,
    };
    b.appends = crate::append::append_candidates(block);
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
    site: usize,
}

struct Builder<'a> {
    program: &'a Program,
    own: &'a Ownership,
    proto: &'a Owned,
    types: HashMap<usize, Type>,
    /// The producer type of every expression the checker typed: what the node
    /// HAS, before the destination's coercion (see [`Rhs`]). `types` above is
    /// the other half of the same pair — what the value must end up as.
    produced: HashMap<usize, Type>,
    /// The type arguments the checker solved at each generic call node.
    solved: HashMap<usize, Vec<(String, Type)>>,
    placed: HashMap<(Exit, usize), Vec<&'a Release>>,
    body: Body,
    scope: Vec<(String, Name)>,
    /// The plan keys a release by the node that owns the value: a `Stmt::Let`,
    /// a parameter, or the construct that owns a temporary.
    by_binding: HashMap<usize, Name>,
    temps: u32,
    /// An unnamed receiver `place` minted for a field or element read, with
    /// the node that produced it, so the read can release it afterwards when
    /// the plan says the frame owns it (R1').
    pending_receiver: Option<(Name, usize, bool)>,
    /// How many calls that do not lend, and operators, enclose the expression
    /// being built. Each is a site the compiled backends drain argument
    /// temporaries at, so a receiver borrowed under one can be freed there.
    drain: u32,
    /// Temporaries the expression being built has read and must release once
    /// it is bound — see `read_val`, `call`, `rhs` and `bind`.
    after: Vec<Name>,
    /// What `rhs` left for the binding that follows it.
    after_of_rhs: Vec<Name>,
    /// The streams the enclosing `for` loops walk, innermost last. A `return`
    /// or a `?` inside such a loop closes every one of them on its way out
    /// (the direct backend's cursor stack), and the loop's end closes its own.
    stream_loops: Vec<Name>,
    /// One entry per loop enclosing the statement being built, innermost
    /// last: the `for` whose elements no turn reached yet, or `None`. A
    /// `return`, a `?` and a `break` release them ([`Builder::release_unreached`]).
    walks: Vec<Option<Unreached>>,
    /// The constructs this build may take their named scrutinee at — what
    /// [`last_owner`] decided over the build before it. Empty on the first
    /// build, which is where the candidates come from.
    seed: &'a std::collections::HashSet<usize>,
    /// One entry per LOOP enclosing the statement being built: the name count
    /// when the loop's body was opened ([`Builder::alias_out`]). A name below
    /// the innermost entry is bound outside that loop, and the loop's back
    /// edge is what makes handing it out of a join arm a double free.
    loop_marks: Vec<usize>,
    /// A join's result, and the name an arm handed out of it from OUTSIDE the
    /// enclosing loop ([`Builder::alias_out`]). Nothing is wrong until
    /// something OWNS the result and releases it once per turn, which is what
    /// [`Builder::loop_alias`] refuses at the two doors that do.
    loop_aliased: HashMap<Name, String>,
    /// Whether the value being lowered is a REBIND's. A store into a name
    /// hands the value on — the slot is released by its final value — so the
    /// temporary the value passes through owns nothing, and the loop's back
    /// edge repeats no release. `std/html.vyrn`'s `attrKey` is the shape.
    rebinding: bool,
    /// Whether the ARGUMENT position being lowered may keep what it is
    /// handed: `Some(false)` for a position that provably only borrows,
    /// `Some(true)` for one that may store it, `None` outside an argument.
    ///
    /// It answers one question about one form — a lambda literal written at
    /// a call argument — and the two lambda arms are its only readers
    /// (RFC-0125 §3 M3, row 24).
    call_keeps: Option<bool>,
    /// [`NameInfo::closure_reads`] for the lambda [`Builder::rhs`] has just
    /// built, waiting for the name [`Builder::bind`] gives it. The arm that
    /// builds a lambda as a whole right-hand side has no name yet.
    pending_closure: Option<Vec<Name>>,
    /// The receivers of the projections being inlined here, innermost last.
    /// A projection declares `read self`, so no construct of its body is its
    /// receiver's last owner however the substituted name reads
    /// ([`Builder::takes_scrutinee`]).
    reading: Vec<Name>,
    /// The body's String accumulators ([`crate::append::append_candidates`]).
    appends: std::collections::HashSet<String>,
    /// How many `region`s enclose the statement being lowered. An arena
    /// buffer cannot grow, so an append inside one is the `concat` call.
    region: u32,
    /// The frame's declared result, which a `?` on an `Option` fails with
    /// `None` of. `None` for module state, an outside block and a lambda the
    /// checker did not type.
    ret: Option<Type>,
    /// The receiver of a declared release, which the frame does not own
    /// ([`Builder::owns_boxes`]). `None` in every other body.
    released: Option<Name>,
}

impl<'a> Builder<'a> {
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

    /// The same, for a `let` a reader wrote — the bindings the memory report
    /// is about.
    fn keyed_let(&mut self, n: Name, binding: usize) {
        self.keyed(n, binding);
        self.body.names[n as usize].bound_by_let = true;
    }

    /// Record the plan's key for a name, and the name for the key.
    fn keyed(&mut self, n: Name, binding: usize) {
        self.body.names[n as usize].binding = Some(binding);
        self.by_binding.insert(binding, n);
    }

    /// A join whose arm handed out a name bound outside the enclosing loop,
    /// bound HERE by something that owns the result and releases it on every
    /// turn ([`Builder::alias_out`]). The two doors are a `let` and the
    /// unnamed temporary an argument position binds, and they get one
    /// sentence: the name that goes out is the reader's, and it is the name
    /// the second turn frees again.
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

    /// Whether a `let` binds a value THIS frame owns — RFC-0125 §3 M3, the
    /// named-binding slice, and the rule `own.rs`'s `Fate` used to state.
    ///
    /// It is the argument slice's reading one binding form over: the core
    /// lowered the initializer, so it has already said what produced the
    /// value, and a `Rhs` is the whole answer. The type owns heap or carries
    /// an obligation, and then three things are not this frame's:
    ///
    ///   - a static value — a literal, or a nullary constructor — which
    ///     lives in the data segment. It answers only for a binding nothing
    ///     can reassign: a `mut` slot is released by its FINAL value in all
    ///     three engines, and `let mut acc: String = ""` is the opening line
    ///     of every accumulator in this language;
    ///   - a read of a place, or a second name for a borrow. The place's
    ///     owner still owns it.
    ///
    /// A rebind states the same rule at the store rather than here
    /// (`Stmt::Assign`): `t = d.title` makes `t` a projection of `d`,
    /// exactly as `let t = d.title` does.
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

    /// Why a `let` binds a value this frame does not own, in the order the
    /// REPORT needs the questions — RFC-0125 §3 M3, the report slice, and the
    /// rule `own.rs`'s `Fate` used to state a second time.
    ///
    /// [`Builder::owned_binding`] beside it asks the same facts of the same
    /// `Rhs`, the same type table and the same region depth; it asks them in
    /// the order a JUDGMENT needs, which is ownership first. A reader needs
    /// the type first: what does it release? Nothing, and there is nothing
    /// more to say. Something, and then: does anybody else own this storage?
    /// So the two orders differ and the facts do not, and neither walks the
    /// tree a second time.
    fn report_reason(
        &self,
        rhs: &Rhs,
        ty: &Type,
        static_value: bool,
        mutable: bool,
        lends: bool,
    ) -> Option<NotOwned> {
        // What the type releases. A must-use type reaches a `let` BECAUSE it
        // is discharged on every path — that is a compile error otherwise —
        // so "nothing reclaims it" is the wrong sentence about one.
        if self.proto.release_kind(ty).is_none() {
            return Some(match self.proto.linear_kind(ty) {
                Some(l) => NotOwned::MustUse(l),
                None => NotOwned::NoRelease {
                    heap: self.proto.owns_heap(ty),
                },
            });
        }
        // A static value lives in the data segment. It answers only for a
        // binding nothing can reassign: a `mut` slot is released by its FINAL
        // value.
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
    /// bound to it is a borrow: a lending prelude row (`at`, `bytes`), the
    /// `value` box, or a projection an `impl` declares (RFC-0120).
    fn lends(&self, e: &Expr) -> bool {
        match e {
            Expr::Call { name, args, .. } => {
                self.lends_name(name) || self.hands_back_a_borrow(name, args)
            }
            _ => false,
        }
    }

    /// Whether a call whose result IS its argument (`blackBox`, read off the
    /// signature by `movecheck::hands_back`) hands back a borrow: it does
    /// when the argument is a place read or a call that lends. An owned
    /// temporary handed to it is TAKEN instead (`call` marks the position
    /// `consume`), and the result owns what the argument owned. Either way
    /// one release stands for the value: owning the result of `blackBox(s)`
    /// freed `s` twice, and borrowing the result of `blackBox(mk(16))` freed
    /// the array never.
    fn hands_back_a_borrow(&self, name: &str, args: &[Expr]) -> bool {
        vyrn_frontend::movecheck::hands_back(name)
            && args
                .first()
                .is_some_and(|a| is_place_read(a) || self.lends(a))
    }

    /// Whether a call by this name lends: `a[i]` and the seeded element row
    /// it dispatches to, a lending prelude row, the `value` box, a projection.
    /// A call that hands its argument back is [`Self::lends`]'s question,
    /// because the answer depends on the argument.
    fn lends_name(&self, name: &str) -> bool {
        name == vyrn_frontend::project::AT
            || name == vyrn_frontend::project::ELEM
            || prelude::lends(name)
            || name == "value"
            || self.projection(name).is_some()
    }

    fn projection(&self, name: &str) -> Option<&'a Function> {
        self.program
            .impls
            .iter()
            .flat_map(|i| i.places.iter())
            .find(|p| p.name == name)
    }

    /// A projection's body, stated as rows AT the access site — RFC-0091 M2,
    /// RFC-0120, and RFC-0125 §2.3.
    ///
    /// A projection is never flattened into `Program::functions` and is
    /// inlined where it is called, so a row keyed to its own parameters can
    /// stand for no site: at a site the parameters are the caller's
    /// expressions. So the site is where the rows are written. The tree is
    /// [`vyrn_frontend::project::site`]'s, the same one the checker typed
    /// (`Checker::record_desugar`) and every backend walks, so each row lands
    /// on the node an emitter asks about.
    ///
    /// `None` leaves the site as it was, and says which of four:
    ///
    /// - No compile scope is open. Outside one every caller expands for
    ///   itself and leaks the tree, which the LSP pays per keystroke — the
    ///   condition `movecheck`'s facts walk puts on the store form, for the
    ///   same reason.
    /// - No projection answers for this receiver's type: a builtin
    ///   container, whose expansion is the identity, or a type this walk
    ///   cannot name.
    /// - The OPTIONAL kind (RFC-0122). Its body splits into four parts at a
    ///   miss test, its consumer is an `if let`, and
    ///   [`vyrn_frontend::project::optional_inline`] mints a different tree.
    ///   [`Builder::optional_if_let`] states that split at the `if let`.
    /// - The receiver has no recorded type, so the tree cannot be keyed.
    fn inlined(
        &mut self,
        method: &str,
        recv: &'a Expr,
        args: &'a [Expr],
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Option<Rhs>, Gap> {
        if !vyrn_frontend::project::memo_open() {
            return Ok(None);
        }
        let Ok(rty) = self.ty_of(recv) else {
            return Ok(None);
        };
        // By the RECEIVER's type, not by the name alone: two `impl`s may
        // declare a projection of one name, and only one of them answers
        // here. The lookup is the same one `project::site` repeats below,
        // and the kind it reads decides whether this site is inlinable at
        // all.
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
        // The yielded place is a BORROW of the receiver, which is what the
        // declared `-> read T` says. Reading it through `Builder::place`
        // rather than as an ordinary right-hand side is the difference
        // between a borrow and a take: `Builder::rhs`'s field arm answers
        // `Rhs::Take` where the field owns heap, and that takes the yield OUT
        // of the receiver. A yield that is no place is `check_places`'s
        // refusal ("a place and not a value"), so a shape that reaches the
        // gap here is a defect in that rule rather than a value to lower.
        if !is_place_read(&p.place) {
            return gap_d("a projection whose yield is not a place", method, line);
        }
        let place = self.place(&p.place, out)?;
        Ok(Some(Rhs::Read(place)))
    }

    /// `if let Some(x) = s.tryAt(h)` (RFC-0122), stated at the site — RFC-0125
    /// §3 M3, the optional-projection slice.
    ///
    /// The OPTIONAL kind is [`Builder::inlined`]'s fourth `None`: its body
    /// splits into four parts at a miss test, and
    /// [`vyrn_frontend::project::optional_inline`] mints a tree of its own.
    /// There is no `Option` on either path, so the site is no scrutinee and no
    /// switch: it is the two-way branch the emitters emit, on the miss test,
    /// with the source's `else` block on the TRUE edge. Stating it as a
    /// [`St::Switch`] on a call result left every statement of the four parts
    /// with no row at all, which is what `tryplace.vyrn`'s three `break`
    /// occurrences on the AST arm were.
    ///
    /// Answers whether this site is one. The conditions are
    /// [`Builder::inlined`]'s, and the reasons with them.
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
        // The TRUE edge is the miss, which is the source's `else` — edge 1 of
        // the join, as the plan numbers it.
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
        // The binder names the yielded place, which is a BORROW of the
        // receiver: the declared `-> read Option<T>` says so, and nothing on
        // either path is copied or released. Read through
        // [`Builder::place`] for [`Builder::inlined`]'s reason — a right-hand
        // side would take the yield out of the receiver where the field owns
        // heap.
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

    /// Whether a value is a borrow: a name whose type owns heap and which
    /// the body does not own (RFC-0089 rule 2).
    fn borrows(&self, v: &Val) -> bool {
        match v {
            Val::Name(n) => self.body.names[*n as usize].borrow,
            Val::Lit(_) => false,
        }
    }

    /// The validated type a value of `from` crosses into at a destination
    /// of `to`, where the core states the crossing as `to`'s constructor.
    ///
    /// WHICH crossings check is `validate::required`'s, which the arm asks
    /// too. A value that owns heap is not stated here: the constructor takes
    /// its argument, and a plain binding moves or borrows it, so the
    /// kernel's verdict on a borrowed operand would change.
    fn checked(&self, from: &Type, to: &Type) -> Option<String> {
        let decls = self.proto.types();
        vyrn_frontend::validate::required(from, to, &decls)
            .filter(|_| !self.owns(from))
            .map(|d| d.name.clone())
    }

    /// The constructor of the validated type `to` over `value`: the row a
    /// checked crossing is. A literal is its own producer, because the
    /// checker proves it against `to` (RFC-0003).
    fn check(
        &mut self,
        to: &str,
        value: &'a Expr,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Rhs, Gap> {
        if let Some(l) = lit_of(value) {
            return Ok(Rhs::Val(Val::Lit(l)));
        }
        let ty = Type::Named(to.to_string());
        self.call(to, std::slice::from_ref(value), line, Some(ty), out)
    }

    fn temp(&mut self, ty: Type, line: usize) -> Name {
        self.temps += 1;
        let owned = self.owns(&ty);
        let src = format!("@t{}", self.temps);
        self.name(&src, ty, owned, line)
    }

    /// Bind `n` to `rhs`, then release the temporaries `rhs`'s own reads
    /// queued — argument temporaries the plan said the caller frees, and
    /// String temporaries the reading site frees (RFC-0096 M3). After, because
    /// the result is named first and the temporaries were its operands.
    fn bind(&mut self, n: Name, rhs: Rhs, out: &mut Vec<St>) {
        if matches!(rhs, Rhs::Prim(Op::Closure, ..)) {
            self.body.names[n as usize].closure_reads = self.pending_closure.take();
        }
        out.push(St::Let(n, rhs));
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

    /// The producer type of a node — what its own code answers, before the
    /// destination's coercion (see [`Rhs`]). `None` where the checker typed no
    /// row for the node, which is a gap for `ty_of` and merely an unanswered
    /// question here: the judgment counts a store it cannot ask about rather
    /// than guessing at it.
    fn produced(&self, e: &Expr) -> Option<Type> {
        self.produced
            .get(&(e as *const Expr as usize))
            .cloned()
            // A projection the checker expanded at the site (RFC-0122) has no
            // row of its own, and its declared result under the receiver's
            // type arguments is what it produces — the same answer `ty_of`
            // reads there. `ty_of` refuses everything else, so this fills the
            // one class and guesses at none.
            .or_else(|| self.ty_of(e).ok())
    }

    fn ty_of(&self, e: &Expr) -> Result<Type, Gap> {
        match self.types.get(&(e as *const Expr as usize)) {
            Some(t) => Ok(t.clone()),
            // A call to a projection the checker expanded at the site
            // (`people.tryAt(h)`, RFC-0122): its declared result, under the
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

    /// The same, with one binding left held: the place a `return` hands a
    /// read of ([`Builder::return_exit`]).
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
                // A row for a name this pass does not own is a row the plan
                // states and the core does not — RFC-0125 §3 M3, the
                // named-binding slice. The plan reads a payload binder of a
                // NON-consuming construct as reclaimed (`let Tagged(tag, n)
                // = local`); the core says the scrutinee is this frame's and
                // the binder names its payload, so the release is stated
                // once, at the value that owns it.
                //
                // A consuming loop's container is the exception: the row is
                // the loop's TAKE, and the kernel refuses it there when the
                // container is a `read` parameter's field or module state
                // (rows 10, 11, 29). A borrow with no row is a take nobody
                // judged.
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

    /// The exit a `return` IS: the streams closed, the exit's releases, and
    /// the return of the value.
    ///
    /// The releases skip the binding the returned value reads out of. `return
    /// d.s` hands the caller a read of `d`, so a release of `d` here frees the
    /// buffer that leaves — and the core stated it in that order, which made
    /// the kernel word the refusal as a write around a live alias where the
    /// checker words it as a return. There is one rule about a return and the
    /// kernel states it at the return, so the release that would speak first
    /// is not stated at all: the frame cannot give back what it hands out
    /// (RFC-0125 §3 M3, row 17).
    fn return_exit(
        &mut self,
        v: Option<Val>,
        sid: usize,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<(), Gap> {
        self.leave_loops(sid, out);
        self.drops_at_but(Exit::Return, sid, self.reads_out_of(&v), out)?;
        out.push(St::Return {
            value: v,
            site: sid,
            is_try: false,
            line,
        });
        Ok(())
    }

    /// The binding of this frame a returned value reads out of, if it is a
    /// read of a place at all.
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

    /// RFC-0125 §3 M3, row 17: a `return` of an `if` or of a `match` carries
    /// the exit INTO the arms.
    ///
    /// `return match t { Word(s) => s, .. }` lowered to one store per arm into
    /// a minted result and a `return` of that result, so an arm's value
    /// reached the kernel as a STORE. The kernel then said "`q` may not be
    /// stored into a store" — once per arm, and about a store no reader wrote
    /// — where the checker says "`q` may not be returned from an exported
    /// function" once, about the binding the reader did write. Each arm's
    /// value IS the return, so the core says so: the arm ends with the return,
    /// [`crate::kernel::MissingKind::Exit`] and the export's own rule apply
    /// there, and a reader gets one sentence.
    ///
    /// `Ok(false)` for every other shape, which keeps its own lowering. A
    /// block arm (RFC-0118) is a statement and carries its exits already, so a
    /// `match` with one is left whole; an `if let` is a statement too, and its
    /// blocks hold `return` statements of their own.
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
                ..
            } => {
                let site = e as *const Expr as usize;
                let c = self.read_val(cond, out)?;
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
                    // The scrutinee's own release, on the path that leaves:
                    // every arm returns, so the statement after the switch is
                    // reached by nothing.
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

    /// The statements of a list, where a store into a nested place is one
    /// store into the place's path (RFC-0125 M7).
    ///
    /// The parser writes `b[i].vx = v` as RFC-0082's move-out: `let mut b[] =
    /// b[b[]idx]`, the store into `b[]`, and `b[b[]idx] = b[]`, one temp per
    /// level and one store back per temp ([`vyrn_frontend::parser::store_stmts`]).
    /// The rows state the store alone, into `b[i].vx`, as the arm stores the
    /// field in place: the kernel judges it as the root's store into that
    /// part, and the part's old value is the store's to release.
    fn stmt_list(&mut self, ss: &'a [Stmt], out: &mut Vec<St>) -> Result<(), Gap> {
        let mut k = 0;
        while k < ss.len() {
            k += match self.nested_store(&ss[k..], out)? {
                Some(n) => n,
                None => {
                    self.stmt(&ss[k], out)?;
                    1
                }
            };
        }
        Ok(())
    }

    /// The store at the head of `ss` when it is a move-out window
    /// ([`Builder::stmt_list`]), stated into its path; how many statements it
    /// spans, or `None` when `ss` does not start with one.
    fn nested_store(&mut self, ss: &'a [Stmt], out: &mut Vec<St>) -> Result<Option<usize>, Gap> {
        let lets = ss
            .iter()
            .take_while(|s| {
                matches!(s, Stmt::Let { name, mutable: true, .. }
                    if vyrn_frontend::ast::is_place_temp(name))
            })
            .count();
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
                        (Expr::Var { name: p, .. }, idx @ Expr::Var { .. }) => (p, Err(idx)),
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
                    Err(Expr::Var { name: i2, .. }),
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
        let (Stmt::SetField { name, line, .. } | Stmt::IndexSet { name, line, .. }) = store else {
            return Ok(None);
        };
        let Stmt::Let { name: last, .. } = &ss[lets - 1] else {
            return Ok(None);
        };
        if name != last {
            return Ok(None);
        }
        // The path is a record's fields and an array's elements. A map's
        // entry is a key read and a user container's element is its `place
        // at`, which the rows state apart.
        let (mut place, mut ty) = self.named_place(parts[0].0, *line)?;
        let mut t = ty.clone();
        for (_, part) in &parts {
            let next = match part {
                Ok(f) => self.field_ty(&t, f, *line),
                Err(_) if self.is_map(&t) => return Ok(None),
                Err(_) => self.elem_ty(&t, *line),
            };
            let Ok(next) = next else {
                return Ok(None);
            };
            t = next;
        }
        for (_, part) in &parts {
            (place, ty) = match part {
                Ok(f) => (
                    Place::Field(Box::new(place), f.to_string()),
                    self.field_ty(&ty, f, *line)?,
                ),
                Err(idx) => (
                    Place::Elem(Box::new(place), self.read_val(idx, out)?),
                    self.elem_ty(&ty, *line)?,
                ),
            };
        }
        let sid = store as *const Stmt as usize;
        match store {
            Stmt::SetField { field, value, .. } => {
                self.set_field((place, ty), name, field, value, sid, *line, out)?
            }
            Stmt::IndexSet { index, value, .. } => {
                self.index_set((place, ty), name, index, value, sid, *line, out)?
            }
            _ => return Ok(None),
        }
        Ok(Some(2 * lets + 1))
    }

    fn stmt(&mut self, s: &'a Stmt, out: &mut Vec<St>) -> Result<(), Gap> {
        let sid = s as *const Stmt as usize;
        match s {
            Stmt::Let {
                name,
                value,
                line,
                ty: annotation,
                ..
            } => {
                let ty = self.ty_of(value)?;
                let check = annotation.as_ref().and_then(|t| self.checked(&ty, t));
                if check.is_none() && !matches!(value, Expr::Var { .. }) && is_place_read(value) {
                    let place = self.place(value, out)?;
                    let n = self.name(name, ty.clone(), false, *line);
                    let rhs = Rhs::Read(place);
                    self.body.names[n as usize].not_owned =
                        self.report_reason(&rhs, &ty, false, false, self.lends(value));
                    out.push(St::Let(n, rhs));
                    self.release_receiver(value, out, true);
                    self.grows(n, name);
                    self.scope.push((name.clone(), n));
                    self.keyed_let(n, sid);
                    return Ok(());
                }
                // A value crossing into a validated type is that type's
                // constructor (§2.2): `let a: Age = n` is `let a = Age(n)`,
                // and the name has the type the reader wrote.
                let (rhs, ty) = match check {
                    Some(to) => (self.check(&to, value, *line, out)?, Type::Named(to)),
                    None => (self.rhs(value, out)?, ty),
                };
                // A literal, or a nullary constructor, which is static in
                // the same sense: [`Builder::val`] makes the variant and
                // nothing allocated it.
                let static_value = match &rhs {
                    Rhs::Val(Val::Lit(l)) => !matches!(l, Lit::Opaque(_)),
                    Rhs::Val(Val::Name(m)) => matches!(
                        self.body.names[*m as usize].not_owned,
                        Some(NotOwned::Static)
                    ),
                    _ => false,
                };
                // A call whose result points into an argument — a lending
                // prelude row, a projection — binds a borrow whatever its
                // type says, and that screen is the one thing about the
                // value the `Rhs` does not carry.
                let mutable = matches!(s, Stmt::Let { mutable: true, .. });
                let lends = self.lends(value);
                let owned = !lends && self.owned_binding(&rhs, &ty, static_value, mutable);
                let reason = self.report_reason(&rhs, &ty, static_value, mutable, lends);
                // A join inside a loop, one of whose arms handed out a name
                // bound outside it, and a binding that owns the result: the
                // release the back edge repeats ([`Builder::loop_alias`]).
                if owned {
                    self.loop_alias(&rhs, *line)?;
                }
                // Not owned is not the same as borrowed: static data (`let s
                // = ""`, a literal of literals) and a value whose type owns
                // no heap are nobody's borrow. A lending call and a second
                // name for a borrow are.
                let borrow =
                    !owned && (self.lends(value) || matches!(&rhs, Rhs::Val(v) if self.borrows(v)));
                let n = self.name(name, ty, owned, *line);
                self.body.names[n as usize].borrow = borrow && self.body.names[n as usize].heap;
                self.body.names[n as usize].not_owned = reason;
                self.record_fields(n, value);
                // `let t = s` on a `read` parameter: `t` is a second name for
                // it, and the checker says so in the refusal it gives at `t`.
                if let Rhs::Val(Val::Name(m)) = &rhs {
                    if self.body.names[n as usize].borrow {
                        self.body.names[n as usize].borrow_kind =
                            self.body.names[*m as usize].borrow_kind.clone();
                        // The take exception travels with the words: a second
                        // name for a must-use parameter is the callee's to
                        // hand on, as the parameter is.
                        self.body.names[n as usize].must_use_param =
                            self.body.names[*m as usize].must_use_param;
                    }
                }
                self.bind(n, rhs, out);
                // The unnamed receiver of the part the binding took or read
                // (RFC-0114 R1′): released after the read where the plan says
                // this frame owns it, held — and seen by the kernel — where
                // it does not.
                if reads_a_part(value) {
                    self.release_receiver(value, out, false);
                }
                self.grows(n, name);
                self.scope.push((name.clone(), n));
                self.keyed_let(n, sid);
            }
            Stmt::Assign { name, value, line } => {
                let n = self.lookup(name);
                let check = match n {
                    Some(n) => {
                        let to = self.body.names[n as usize].ty.clone();
                        self.checked(&self.ty_of(value)?, &to)
                    }
                    None => None,
                };
                let grown = match (n, &check) {
                    (Some(n), None) if self.region == 0 && self.body.names[n as usize].grows => {
                        crate::append::self_append_spine(name, value).map(|parts| (n, parts))
                    }
                    // Module state grows through a read of it, which the row
                    // names as its receiver.
                    (None, _)
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
                    (Some(to), None) if lit_of(value).is_none() => {
                        self.check(&to, value, *line, out).map(|rhs| {
                            let t = self.temp(Type::Named(to), *line);
                            self.bind(t, rhs, out);
                            Val::Name(t)
                        })
                    }
                    _ => self.val(value, out),
                };
                self.rebinding = false;
                let v = v?;
                let ty = match n {
                    Some(n) => self.body.names[n as usize].ty.clone(),
                    None => self.ty_of(value)?,
                };
                // The emitters' rule for a store to a NAME, stated once: the
                // plan says whether the store releases the old value; a value
                // that MENTIONS the place may be handing the old buffer back
                // (`xs = xs.push(v)`) so the release stands down — unless the
                // plan proved every mention a read argument to a function
                // that cannot hand it back (`store_is_fresh`, exit-residue
                // round eighteen), or the value is a String concatenation,
                // which builds a fresh buffer whatever it reads (`s = s + x`).
                // Both compiled backends spell the last exception
                // `fresh_str`; it stands here so the one answer they read is
                // this one (RFC-0125 §3 M3, the emitter-reads-the-core
                // slice). Module state takes the same rule: it is a name to
                // both of them.
                let mentions = vyrn_frontend::ast::mentions_place(value, name);
                let fresh_str = self.fresh_str(&ty, value);
                // The hand-back is the CORE's answer and not the kernel's: it
                // is read off the statement, not off the path. Everything
                // else a store displaces is the kernel's, and the first build
                // leaves it open (RFC-0125 §3 M3, the store slice).
                let handed_back = mentions && !fresh_str && !self.store_is_fresh(value, name);
                let key = self.store_key(sid);
                let releases = !handed_back && placed_store(key);
                // Module state owns what it holds for the whole module and
                // nothing may `consume` it, so a store into one releases what
                // it replaces whenever that owns heap.
                let (place, owes) = match n {
                    None => (Place::Global(name.clone()), self.owns(&ty)),
                    Some(n) => {
                        // A rebind carries the same ownership answer a `let`
                        // does, which is the other half of the same sentence:
                        // `let t = d.title` is a projection of `d` and so is
                        // `t = d.title`. RFC-0092's two-spellings-two-verdicts
                        // defect, stated once — a `mut` slot is released by
                        // its FINAL value in all three engines, so a slot ever
                        // assigned somebody else's place is not this frame's
                        // to release.
                        if self.borrows(&v) && self.body.names[n as usize].releases {
                            self.body.names[n as usize].releases = false;
                            self.body.names[n as usize].borrow = true;
                        }
                        (Place::Name(n), self.body.names[n as usize].releases)
                    }
                };
                // The hand-back is read off the STATEMENT, so it is stated
                // before the place's own obligation is: a name that owes no
                // release still hands its buffer back, and the census reads
                // the word to tell the two reasons for `false` apart.
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
                });
                // An append's operand temporaries, which [`Builder::str_append`]
                // left queued so the store stays next to its row.
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
                let base = self.named_place(name, *line)?;
                self.set_field(base, name, field, value, sid, *line, out)?;
            }
            Stmt::IndexSet {
                name,
                index,
                value,
                line,
            } => {
                let base = self.named_place(name, *line)?;
                self.index_set(base, name, index, value, sid, *line, out)?;
            }
            Stmt::Return { value, line } => {
                if let Some(e) = value {
                    if self.return_through(e, sid, *line, out)? {
                        return Ok(());
                    }
                }
                let v = match value {
                    Some(e) => Some(self.val(e, out)?),
                    None => None,
                };
                self.return_exit(v, sid, *line, out)?;
            }
            Stmt::Break { .. } => {
                if let Some(Some(u)) = self.walks.last().cloned() {
                    self.release_unreached(&u, sid, out);
                }
                self.drops_at(Exit::Break, sid, out)?;
                out.push(St::Break { site: sid });
            }
            Stmt::Continue { .. } => {
                self.drops_at(Exit::Continue, sid, out)?;
                out.push(St::Continue { site: sid });
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                let c = self.read_val(cond, out)?;
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
                let c = self.read_val(cond, &mut l)?;
                l.push(St::If {
                    cond: c,
                    then: Vec::new(),
                    els: vec![St::Break { site: 0 }],
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
                let ety = match self.elem_ty(&ity, *line) {
                    Ok(t) => t,
                    // A user container: the element is what its `nth`
                    // projection yields (RFC-0091 M2), under this
                    // instantiation's type arguments.
                    Err(g) => self.projected_elem(&ity).ok_or(g)?,
                };
                // The LOOP form of the take (RFC-0125 §3 M3, row 09): the
                // same rule as the prefix form, at the other spelling.
                if *consuming {
                    take_names_a_place(iter, *line, true)?;
                }
                // The container: a name the loop reads, or one it takes.
                // `owner` is the name whose ownership the element sentence
                // below asks about, which for a borrowed name is the name.
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
                            // The loop reads the container from its head to
                            // its end, so it reads it through a borrow, as it
                            // reads a field below: a store over the name inside
                            // the body then ends the borrow the next turn
                            // reads, and the kernel refuses it. Without the
                            // borrow `ys = []` freed the buffer the loop was
                            // still walking.
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
                        // A temporary the container was read out of has no
                        // consumer a drain encloses: it stays held, and the
                        // judgment says so.
                        self.pending_receiver = None;
                        let t = self.borrow_name(iter, ity.clone(), *line);
                        self.body.names[t as usize].walked = Some(Walk::For);
                        out.push(St::Let(t, Rhs::Read(place)));
                        t
                    }
                    _ => {
                        match self.val(iter, out)? {
                            // The construct owns the temporary; the plan keys
                            // its release by the statement, and so does a row
                            // the placer adds for it (`for t in lex(src)` with
                            // a `return` inside the loop).
                            Val::Name(t) => {
                                self.keyed(t, sid);
                                t
                            }
                            // A String literal is static: the loop reads it
                            // through a name that owns nothing.
                            lit => {
                                let t = self.name("@lit", ity.clone(), false, *line);
                                out.push(St::Let(t, Rhs::Val(lit)));
                                t
                            }
                        }
                    }
                };
                // The loop is what took the container, whichever spelling
                // brought it here: a bare name binds a temporary above, and
                // module state or a field reaches `val` and binds one there.
                // A refusal about the take names the loop the reader wrote,
                // at the `let` and at the release alike (RFC-0125 §3 M3, row
                // 07 and rows 10, 11, 29).
                if *consuming {
                    self.body.names[it as usize].for_consume = true;
                }
                let decls = vyrn_frontend::types::decl_map(self.program);
                let streaming =
                    matches!(vyrn_frontend::types::resolve(&ity, &decls), Type::Stream(_));
                if streaming {
                    self.stream_loops.push(it);
                }
                // `for x in xs` walks an index the core names (§2.1): the
                // length read once, before the loop, and a counter from zero
                // that steps right after the element is read, so a `continue`
                // needs no step of its own. A stream is pulled and has
                // neither.
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
                // The loop leaves when the container is walked: the same
                // `if .. else break` a `while` has at its top. Without it the
                // kernel sees a loop nothing leaves, and the path after the
                // `for` — the rest of its block, and its edge at every join
                // above — is dead to the judgment. The placer's rewrite of a
                // row's hole set then read a join's other edge alone, and
                // walked a field the dead edge had taken (`std/vyx`'s
                // `vyxMergeImports`, found by the cross-engine generator gate).
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
                    None => (
                        Val::Lit(Lit::Opaque(Opaque::Pull)),
                        Val::Lit(Lit::Opaque(Opaque::Pull)),
                    ),
                };
                l.push(St::If {
                    cond,
                    then: Vec::new(),
                    els: vec![St::Break { site: 0 }],
                    site: 0,
                });
                // Whose is each element? The loop VARIABLE is the last owner
                // of every element that left the container, and the core
                // answers that over its own first build ([`last_owner`],
                // `Cand::Elem`): the variable is handed on somewhere in the
                // body rather than only read. Then each turn owns its
                // element and must move or release it, and the container's
                // release frees the buffer alone. Read only where the
                // element type owns heap; nothing else is a question.
                let ekey = vyrn_frontend::own::for_var_key(var);
                // …out of a container this frame owns: a lender's result is
                // somebody else's buffer, so its elements are somebody
                // else's too, and a `for x in xs` over a `read` parameter
                // may not hand `x` on (`x` is the loop variable, and a
                // return is owned).
                // …out of a container the loop is the only owner of: a
                // value with no name of its own that this frame owns. A
                // lender's result is somebody else's buffer and its
                // elements are somebody else's too; a container the reader
                // NAMED outlives the loop, so `for r in ns` only borrows
                // its elements and `take(r)` is refused (the structural
                // census, rows 01, 02, 03, 27 and 34).
                let ic = &self.body.names[owner.unwrap_or(it) as usize];
                let loops_alone = !ic.borrow && !ic.bound_by_let;
                let owned = self.owns(&ety) && loops_alone && self.seed.contains(&ekey);
                // The other half of the same sentence: if every element left
                // through the variable, the container's release at the loop
                // walks the BUFFER alone. A growable array only — a fixed or
                // small array is a value with no heap buffer, and a map or a
                // stream has machinery of its own — so field 0 of the triple
                // is what is left to give back. Round fourteen is what a
                // wrong answer costs: a blanket buffer-only free took
                // somebody else's storage.
                let mut unreached = None;
                if owned && matches!(vyrn_frontend::types::resolve(&ity, &decls), Type::Array(_)) {
                    self.body.loop_buffers.push(sid);
                    unreached = counter.map(|(n, i)| Unreached {
                        it,
                        i,
                        n,
                        elem: ety.clone(),
                        line: *line,
                        site: sid,
                    });
                }
                // In front of the variable: each turn binds its own element,
                // so handing THAT out of a join arm frees once per turn. The
                // container is below the mark and handing it out is refused.
                self.loop_marks.push(self.body.names.len());
                let x = self.name(var, ety, owned, *line);
                self.body.cands.push((ekey, x, Cand::Elem));
                // What the variable IS, for a refusal about it: the container
                // outlives the loop, so the loop only names the element. The
                // kernel read that off the alias table and said the element was
                // read out of a place; the checker says what the reader wrote
                // (RFC-0125 §3 M3, row 19).
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
                    // The loop pulled the stream to its end, or a `break`
                    // left early: either way the loop closes it here, where
                    // the loop is the stream's last owner.
                    if self.body.names[it as usize].releases && self.taken_by_loop(it, sid) {
                        out.push(St::Drop(it, Site::None, 0, None));
                    }
                } else if *consuming && self.taken_by_loop(it, sid) {
                    // The loop took the container and is its last owner, so
                    // the loop gives it back here. Keyed by the LOOP, so the
                    // emitters read the judgment rather than the word
                    // `consume` in the source (RFC-0125 §3 M3, the event
                    // stream's slice).
                    out.push(St::Drop(it, Site::Node(sid), 0, None));
                }
                self.drops_at(Exit::Scrutinee, sid, out)?;
            }
            Stmt::Drop { name, line } => {
                let Some(n) = self.lookup(name) else {
                    return gap("a `drop` of module state", *line);
                };
                // `drop s` inside a `region` used to lower to nothing,
                // because this pass read the binding as the arena's and the
                // arena would give the block back at the brace. The arena
                // answers for its own blocks now — `free` refuses one by its
                // class word — so a `drop` inside a region is an ordinary
                // drop.
                out.push(St::Drop(n, Site::None, *line, None));
            }
            // RFC-0114 section 25: an unaudited build emits no audit hook, so the
            // row states neither the call nor its operand. A row for `p + 8`
            // alone is four instructions on every allocation.
            Stmt::Expr(Expr::Call { name, .. })
                if vyrn_frontend::loader::audit_hook(name)
                    && !vyrn_frontend::loader::audit_build() => {}
            Stmt::Expr(e) => {
                let ty = self.ty_of(e).unwrap_or(Type::Unit);
                let rhs = self.rhs(e, out)?;
                if self.owns(&ty) {
                    let t = self.temp(ty, e.line());
                    self.bind(t, rhs, out);
                    if self.discards(e) {
                        out.push(St::Drop(t, Site::Node(sid), 0, None));
                    }
                } else {
                    // A value on its own does nothing: a `match` or an `if`
                    // statement yields its join, which no arm writes when the
                    // type is Unit.
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
            // The arena owns what IT was handed, which is what
            // `direct.rs`'s `arena_route` routes into it; every other block
            // the body mints is the frame's, and the closing brace is the
            // runtime's. So the body is an ordinary block here and this pass
            // asks nothing about the depth.
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

    /// Round twenty-eight's rule, stated by the core rather than read off the
    /// plan (RFC-0125 §3 M3, the derivation slice): a statement-position CALL
    /// whose owned heap result nothing binds is this frame's to release right
    /// after the call.
    ///
    /// The caller has already asked whether the type owns heap, which is the
    /// first half of the rule. The screens here are the second, and each is a
    /// value that is not this frame's: a lending call hands back a place
    /// inside its argument, a variant constructor builds a value that
    /// outlives the call in what it built, a `panic` returns to nobody, and
    /// an `@`-spelled desugar is freed by the site that reads it (RFC-0096
    /// M3). A removal (`@pop`, `@swapRemove`) hands back what it took out,
    /// and a statement that discards it is no site that reads it. `own.rs` decided the same rule over a declared-types reading of
    /// the program; the core asks the checker's own type, which is why this
    /// is a second opinion and not a filter.
    fn discards(&self, e: &Expr) -> bool {
        let Expr::Call { name, .. } = e else {
            return false;
        };
        !vyrn_frontend::ast::is_panic(name)
            && (!name.starts_with('@') || matches!(builtin_row(name), Some(Spec::Removes)))
            && !self.lends(e)
            && !self.constructs(name)
    }

    /// Whether `name` constructs a sum value out of its arguments: a user
    /// enum's variant, or one of the five the language declares itself
    /// (`declared::Declared`'s own seed).
    fn constructs(&self, name: &str) -> bool {
        matches!(name, "Some" | "Ok" | "Err" | "Success" | "Failure") || self.is_variant(name)
    }

    /// What a field or element store displaces. The plan decides whether the
    /// old value is released (RFC-0114 §26 steps 3-4: the place's ownedness is
    /// a plan row, keyed by the statement). Where it placed no release, this
    /// slice records `Nothing` rather than `Unreleased`: the kernel tracks
    /// whole names, and a sub-place the plan knows to be empty — a payload
    /// already taken out, an `Option` already `None` — is not a name it can
    /// see. Sub-place ownership is M3's judgment, not M2's.
    /// Marks `n`, bound by a `let` of `name`, as a String accumulator where
    /// the whitelist admits the name.
    fn grows(&mut self, n: Name, name: &str) {
        let info = &mut self.body.names[n as usize];
        info.grows = self.appends.contains(name)
            && vyrn_frontend::types::resolve(&info.ty, self.proto.types()) == Type::Str;
    }

    /// `s = s + a + b` on an accumulator: one `@strAppend` row that reads
    /// `s` and each part in written order, which the store after it puts
    /// back into `s`. The parts are the operands the `+` chain would read,
    /// at the same argument keys, and their temporaries stay queued, as
    /// [`Builder::rhs`] queues them, until the caller has pushed the store.
    fn str_append(
        &mut self,
        s: Name,
        parts: &[&'a Expr],
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Val, Gap> {
        let outer = std::mem::take(&mut self.after);
        self.drain += 1;
        let mut args = vec![(Val::Name(s), Capability::Read)];
        let read = parts.iter().try_for_each(|p| {
            args.push((self.read_arg(p, out, "@concat", 1)?, Capability::Read));
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

    /// A String concatenation builds a fresh buffer whatever it reads, so
    /// `s = s + x` displaces the old one and does not hand it back. Both
    /// compiled backends spell this exception `fresh_str`; it stands here so
    /// the one answer they read is this one.
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

    /// The node a store's row is keyed by, which is the node its READERS key
    /// it by (RFC-0125 §3 M3, the store slice).
    ///
    /// Identity for every store written as one. RFC-0091 M2's `place at`
    /// rewrite builds the store statements a user container's `c[h] = v`
    /// becomes, and both compiled backends ask about the source statement
    /// through the same mapping, so the answer is filed under the rewrite's
    /// node and found there.
    fn store_key(&self, sid: usize) -> usize {
        self.own.plan.key_of(sid)
    }

    fn old_for(&self, ty: &Type, releases: bool) -> Old {
        if !self.owns(ty) {
            Old::Nothing
        } else if releases {
            Old::Released
        } else {
            // The first build leaves it open and the kernel answers over the
            // ROOT, which is the one place a sub-place's ownership can be
            // read from (RFC-0125 §3 M3, the store slice). `Nothing` still
            // stands where the type owns nothing: that is the core's answer,
            // not a decision it defers.
            Old::Pending
        }
    }

    /// Round forty's releases, derived: the payload binders the kernel found
    /// still held where this arm ends, released there.
    ///
    /// RFC-0125 §3 M3, the derivation slice. The FIRST build states none, so
    /// [`crate::kernel::placement`] reports every binder an arm still holds —
    /// which is the whole of the rule — and the second build reads the rows
    /// back out of [`Placed`]. Nothing here asks `own.rs`, and the answer is
    /// the same one the kernel refuses a leak by.
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

    /// The rows a `return` or a `?` runs for every enclosing `for`, innermost
    /// first: the elements no turn reached, then the stream it walks, closed.
    fn leave_loops(&mut self, exit: usize, out: &mut Vec<St>) {
        for u in self.walks.clone().iter().rev().flatten() {
            self.release_unreached(u, exit, out);
        }
        for it in self.stream_loops.iter().rev() {
            if self.body.names[*it as usize].releases {
                out.push(St::Drop(*it, Site::None, 0, None));
            }
        }
    }

    /// Releases the elements of `u`'s container from its counter to its
    /// length, which no turn bound: the rows a `return`, a `?` or a `break`
    /// runs before its own. The element the turn bound is the body's.
    fn release_unreached(&mut self, u: &Unreached, exit: usize, out: &mut Vec<St>) {
        self.body.unreached.push((exit, u.site));
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
                els: vec![St::Break { site: 0 }],
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

    /// RFC-0114 Rule N: the drops one edge of a join owes.
    fn edge_drops(&mut self, join: usize, edge: u32, out: &mut Vec<St>) -> Result<(), Gap> {
        let Some(ers) = placed_edges(join) else {
            return Ok(());
        };
        for (name, e, holes) in &ers {
            if *e != edge {
                continue;
            }
            // `d.line`: a sub-place the other edge took, released on this
            // one (RFC-0125 M3). It leaves as a take into a temporary that is
            // dropped at once, so the kernel sees the hole it leaves.
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
                // The temporary is spelled as the sub-place it took, so a
                // reader of the fold gets back the row's own name.
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

    /// A name as a place: a binding of this body, or module state with its
    /// declared type.
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
        let v = self.val(value, out)?;
        let (base, bty) = base;
        let fty = self.field_ty(&bty, field, line)?;
        // `s.dense.push(i)` IS `s.dense = s.dense.push(i)`: the
        // receiver comes back through the result, so the store hands
        // the buffer back and releases nothing — the same rule a
        // store to a name takes, one dot down, with the same
        // exception for a String concatenation, which builds a fresh
        // buffer whatever it reads (RFC-0125 §3 M3, the store slice).
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
        });
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
        let (base, bty) = base;
        let place = if self.is_map(&bty) {
            let k = self.val(index, out)?;
            Place::Key(Box::new(base), k)
        } else {
            let i = self.read_val(index, out)?;
            Place::Elem(Box::new(base), i)
        };
        let v = self.val(value, out)?;
        // A user container's `place at` yields the element's place
        // (RFC-0091 M2), and the element's type is the value's. Such
        // a store is REWRITTEN into a block of its own before the
        // checker walks it, so the node a reader keys it by is the
        // rewrite's and not this statement's — which is what
        // `key_of` says, and why every store above keys by it too
        // (RFC-0125 §3 M3, the store slice). This pass judges the
        // SOURCE statement and files the answer where the emitters
        // look.
        let ety = match self.elem_ty(&bty, line) {
            Ok(t) => t,
            Err(_) => self.ty_of(value)?,
        };
        let key = self.store_key(sid);
        let site = Site::Node(key);
        // The same hand-back, and the INDEX counts as well: `xs[i] =
        // xs[j]` and `xs[xs.length - 1] = v` both read the buffer the
        // store writes into, and neither displaces anything the
        // container did not keep.
        let handed_back = vyrn_frontend::ast::mentions_place(value, name)
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
        });
        Ok(())
    }

    fn named_place(&self, name: &str, line: usize) -> Result<(Place, Type), Gap> {
        if let Some(n) = self.lookup(name) {
            return Ok((Place::Name(n), self.body.names[n as usize].ty.clone()));
        }
        match self.program.globals.iter().find(|g| &g.name == name) {
            Some(g) => match g.ty.clone().or_else(|| self.init_ty(&g.init)) {
                Some(t) => Ok((Place::Global(name.to_string()), t)),
                None => gap_d("a global without a declared type", name, line),
            },
            None => gap("a place that is not a binding", line),
        }
    }

    /// The type of a global's initializer, from its shape: the checker's rows
    /// are per instance and a global is instantiated nowhere.
    fn init_ty(&self, init: &Expr) -> Option<Type> {
        match init {
            Expr::StructLit { name, .. } => Some(Type::Named(name.clone())),
            Expr::Str(_) => Some(Type::Str),
            Expr::Bool(_) => Some(Type::Bool),
            Expr::Call { name, .. } => self
                .program
                .functions
                .iter()
                .find(|f| &f.name == name)
                .map(|f| f.ret.clone()),
            _ => None,
        }
    }

    /// Bind the header of every container the loop `l` indexes and no row of
    /// it rebuilds ([`crate::kernel::writes`]), once, before the loop, and
    /// point the loop's element and length reads at it (RFC-0125 M7, the
    /// hoisted header). The header is a borrow the loop walks, as a `for`'s
    /// container is, so the kernel keeps its alias and an emitter takes the
    /// header apart once. A container that owns no heap is a value and not a
    /// borrow, so it is read in place each turn. Module state takes the same
    /// rule, and a call that stores into it is a write by the effect judgment.
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

    /// The length a `for` over `it` walks to: a header read for a container
    /// the language lays out, and the `Iterate` impl's `size` for a user one
    /// (RFC-0091 M3).
    fn length_of(&self, it: Name, ity: &Type, line: usize) -> Result<Rhs, Gap> {
        let decls = self.proto.types();
        let field = |f: &str| Rhs::Read(Place::Field(Box::new(Place::Name(it)), f.to_string()));
        Ok(match vyrn_frontend::types::resolve(ity, &decls) {
            Type::Str => field("byteLength"),
            Type::Array(_) | Type::ArrayN(..) | Type::SmallArray(..) | Type::Map(..) => {
                field("length")
            }
            _ => match vyrn_frontend::types::iterate_impl(&self.program.impls, ity) {
                Some((size, _)) => Rhs::Call {
                    kind: if self.concrete_fn(&size) {
                        Callee::Fn
                    } else {
                        Callee::Method
                    },
                    callee: size,
                    args: vec![(Val::Name(it), Capability::Read)],
                    write_back: false,
                    ret: Some(Type::Int),
                    solved: Vec::new(),
                    targets: Vec::new(),
                },
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
        let imp = self
            .program
            .impls
            .iter()
            .find(|i| vyrn_frontend::types::type_key(&i.ty) == key);
        let mut subst = HashMap::new();
        if let (Some(imp), Type::App(_, args)) = (imp, recv) {
            if let Type::App(_, params) = &imp.ty {
                for (p, a) in params.iter().zip(args) {
                    if let Type::Param(n) = p {
                        subst.insert(n.clone(), a.clone());
                    }
                }
            }
        }
        vyrn_frontend::types::substitute(ty, &subst)
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

    /// The scrutinee of a `match`, `if let` or `?`: the value it switches on,
    /// and whether the construct consumed it. `lines` is the construct's own
    /// first and last source line, which is how a give written inside it is
    /// told from a move after it ([`Builder::takes_scrutinee`]). `None` where
    /// the construct has no arm that can hand a payload out of a NAMED
    /// scrutinee — an `if let`, a `?` — and a named local is read there.
    /// Whether the construct MADE the value it switches on — [`St::Switch`]'s
    /// `owns`, and the half of that row `consuming` does not answer.
    ///
    /// A name is not made here: what a named scrutinee is worth is
    /// [`Builder::takes_scrutinee`]'s question, and the `||` at each call site
    /// puts the two halves together. Nor is a place: a field, an element, a
    /// projection's `read` result. Everything else is a call's result, a
    /// literal, a `consume` or a constructor, and the frame made all four.
    ///
    /// ONE place is made rather than named: `m[k]` on a `Map` BUILDS its
    /// `Option<V>` rather than naming an entry (RFC-0028), so the value is
    /// the construct's like any temporary's. The lowering still reads it as a
    /// place, and that is a different question — what the arms may hold, not
    /// what the frame owns.
    /// Whether a construct owns the boxes its binders come out of
    /// ([`St::Switch`]'s `owns`): it took a named scrutinee, or it switches
    /// on a value the frame made.
    ///
    /// A declared release's receiver is the exception, taken or not: the
    /// caller of a declared release frees the payload boxes after the call
    /// (RFC-0096), so a switch on the receiver, or on the temporary bound to
    /// it, hands the parts to its binders and owns no box.
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

    fn made_scrutinee(&self, e: &'a Expr) -> bool {
        use vyrn_frontend::ast::place_path;
        use vyrn_frontend::project::element_path;
        if place_path(e).is_none() && element_path(e).is_none() {
            return true;
        }
        let Expr::Call { name, args, .. } = e else {
            return false;
        };
        name == vyrn_frontend::project::AT
            && args.len() == 2
            && self.ty_of(&args[0]).is_ok_and(|t| {
                matches!(
                    vyrn_frontend::types::resolve(&t, self.proto.types()),
                    Type::Map(..)
                )
            })
    }

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
                // The note says this construct gave the value away. Whether
                // it may TAKE it is the second question, and the first build
                // answers it over the core it just made ([`last_owner`]): the
                // candidate is recorded here, and only a seeded site acts.
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
                        // `for x in consume <module state>`: the same read,
                        // and the same refusal (RFC-0125 §3 M3, row 29).
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

    /// Whether this construct is a CANDIDATE to take its named scrutinee —
    /// RFC-0125 §3 M3, the named-binding slice.
    ///
    /// Round twenty-seven's question used to be asked of the plan's note, and
    /// the note answered it the wrong way round: `let o = tag(7)` in `let s =
    /// match o { Some(v) => v, .. }` is an ALIAS out of the join, because the
    /// construct hands the payload out and `s` reclaims it. Read as "never
    /// owned", that made this pass bind `o` as a borrow, so the rule that
    /// asks whether the construct TOOK it rested on the decision it feeds.
    ///
    /// The binding owns its value where it is bound
    /// ([`Builder::owned_binding`]), so the question left here is only which
    /// construct is its LAST owner, and [`last_owner`] answers that over the
    /// core the first build made. Every owned named scrutinee is a candidate;
    /// nothing else has to be said.
    ///
    /// `lines` is `None` where the construct has no arm that can hand a
    /// payload out of a named scrutinee — an `if let`, a `?`.
    ///
    /// An answer too WIDE is a refusal and never a double free: the take is
    /// stated in the core, so the kernel refuses a later read rather than
    /// freeing behind it.
    fn takes_scrutinee(&self, n: Name, lines: Option<(usize, usize)>) -> bool {
        let info = &self.body.names[n as usize];
        lines.is_some() && info.releases && info.heap && !info.borrow && !self.reading.contains(&n)
    }

    /// Whether the construct took the temporary `t` it owns: the payloads
    /// moved into the arms' binders and the boxes were freed there. Where it
    /// did not, the binders borrowed and the value is released whole.
    ///
    /// A `consume` expression and a computed scrutinee are CANDIDATES like a
    /// named one (RFC-0125 §3 M3, the take-rule slice). The value is the
    /// construct's own temporary, so nothing screens it but ownership; which
    /// construct is its LAST owner is [`last_owner`]'s answer over the first
    /// build, and only a seeded site acts.
    ///
    /// This used to read the plan's SILENCE — no release placed at the
    /// scrutinee's exit — which is one answer stated twice: the plan's row
    /// IS a read of the name in the core, so the order the core already
    /// holds says the same thing without the plan.
    fn taken_by(&mut self, t: Name, construct: usize) -> bool {
        if !self.body.names[t as usize].releases {
            return false;
        }
        self.body.cands.push((construct, t, Cand::Switch));
        self.seed.contains(&construct)
    }

    /// The same question at a `for`: whether the loop is the last owner of
    /// the container it was handed, so the loop releases it where it ends.
    /// [`Cand::Loop`], answered by [`last_owner`] over the first build.
    fn taken_by_loop(&mut self, it: Name, sid: usize) -> bool {
        self.body.cands.push((sid, it, Cand::Loop));
        self.seed.contains(&sid)
    }

    /// Which tag a pattern tests for, in the scrutinee's own variant list —
    /// RFC-0125 M7, the tag family.
    ///
    /// `??`'s pair (RFC-0079) names a TAG rather than a variant: tag 1
    /// succeeds and tag 0 fails, for every sum since RFC-0126 §8.11's M4b.
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

    /// Bind a pattern's names. Owned binders when the match consumed its
    /// scrutinee; borrowed places otherwise.
    /// `from` is the scrutinee's name where the construct did not consume it:
    /// a binder over a borrow is a second name for that borrow, and it
    /// carries the same kind, so a take of it is refused in the same words
    /// (RFC-0125 §3 M3, row 13). Without it `match o { Some(v) => take(v) }`
    /// over a `read` parameter handed the caller's buffer away.
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
        // The binder's own key, beside its spelling: the address of the name
        // the reader wrote, which is one address per binder and the same one
        // on every build (RFC-0125 §3 M3, the walk's deletion —
        // [`Builder::bind_pattern`] below says what it is for).
        let (payloads, variant): (Vec<(String, Type, usize)>, String) = match p {
            Pattern::Other => (Vec::new(), String::new()),
            // `??`'s pair (RFC-0079) names a TAG rather than a variant, and the
            // SCRUTINEE says which one: variant 1 succeeds, variant 0 fails. Since
            // RFC-0126 §8.11's M4b that is one list for every sum, so the two
            // built-in spellings have no arm of their own here.
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
            // A binder the arm OWNS is a binding of the frame like any other,
            // and the frame's exit rule is stated for it once: the kernel
            // finds it still held at a `return`, a `break` or a `continue`
            // inside the arm, the placer keys the row by this address, and
            // [`Builder::drops_at`] emits the release there. The arm's own end
            // is the other half, and `binders_end` states it (RFC-0125 §3 M3,
            // the walk's deletion). Without the key `place_frames` skipped the
            // row and three programs of the corpus held a payload at a
            // `return` with no release placed for it.
            //
            // Keyed whether the arm owns the payload or not: the key is read
            // on EVERY build, and the first build of a pair takes nothing, so
            // a key given only to an owned binder would leave the row the
            // second build's placement wrote with no name to land on.
            // [`Builder::drops_at`] skips a row for a name this frame does not
            // own, which is what a borrowed binder is.
            self.keyed(n, key);
            if !owned {
                if let Some(m) = from {
                    if let Some(k) = self.body.names[m as usize].borrow_kind.clone() {
                        self.body.names[n as usize].borrow_kind = Some(k);
                        self.body.names[n as usize].must_use_param =
                            self.body.names[m as usize].must_use_param;
                    }
                    // And where the scrutinee is itself a BORROW, the binder
                    // reads it: that is the other half of the same sentence.
                    // A `read` parameter's kind travels above; a read of a
                    // place — module state, a field, an element — has no kind
                    // to travel, and the read is what says the payload is not
                    // the binder's (RFC-0125 §3 M3, row 17: `return match
                    // d.tag { Word(s) => s, .. }`).
                    //
                    // A scrutinee this frame OWNS is not one: its payloads are
                    // the frame's to give, and `std/vyx.vyrn`'s
                    // `vyxProcessElem` hands one to a `consume` parameter on
                    // the arm that does not return the value whole.
                    if self.body.names[m as usize].borrow
                        && self.body.names[n as usize].borrow_kind.is_none()
                    {
                        self.body.names[n as usize].borrow_kind = Some(BorrowKind::Place);
                    }
                    // A binder of a layout, or of a value that owns heap, is
                    // a read out of the scrutinee for the arm's extent,
                    // whoever owns the scrutinee ([`Arm::reads`]): a write to
                    // it while the binder lives is the kernel's refusal.
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
            // `_` names nothing a body can read, so it never enters the
            // scope — but the payload is real, and a consumed scrutinee's arm
            // still owes its release. The plan's arm table names `_`
            // (`revcomp.vyrn`'s `Err(_) => ""`), and the core said nothing
            // about it until this slice (RFC-0125 §3 M3, the
            // deletion-preparation slice).
            if name != "_" {
                self.scope.push((name, n));
            }
            binds.push(n);
        }
        Ok(binds)
    }

    /// An expression in a READ position: an operand, a condition, an index, a
    /// `read` argument. A place that owns heap is borrowed, not moved — the
    /// value the position sees is a name the kernel does not own. A String
    /// temporary (`@str`, `@concat`, a string `+`) is freed by the site that
    /// reads it (RFC-0096 M3), so the drop is queued for right after the
    /// binding that consumes it.
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
        // RFC-0114 M1's key, stated once for every read in an argument
        // position (RFC-0125 §3 M3, the argument slice). The key is taken
        // here rather than in `call`, which sees neither an operator nor a
        // `lazy` field read.
        if let (Val::Name(t), Some((callee, ix))) = (&v, at) {
            let t = *t;
            if self.arg_released(e, t, callee, ix) {
                self.body.names[t as usize].arg_drop = Some(e as *const Expr as usize);
            }
        }
        Ok(v)
    }

    /// Round eighteen's rule, stated by the core (RFC-0125 §3 M3): a store
    /// whose value mentions the place it writes into may be handing the old
    /// buffer back, UNLESS every mention is a read the value cannot hand
    /// back. The shape is read off the statement.
    ///
    /// It asked a call-graph closure as well until RFC-0125 §3 M3's escape
    /// slice — the set of functions whose result may HOLD a borrowed
    /// parameter's storage. A declared result answers that: a function whose
    /// result is not spelled `read`/`modify` returns an owned value, and
    /// returning a borrow of a parameter is the kernel's refusal (row 17). So
    /// the closure was a guess at a declaration, and both of the two members
    /// it ever fired on returned a value every field of which is a copy.
    fn store_is_fresh(&self, value: &'a Expr, name: &str) -> bool {
        // The recording gate: a value whose type owns no heap has nothing to
        // hand back and no row is written for it.
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
        // A mention whose TYPE owns no heap hands nothing back however it is
        // read — `f = Frag { start: f.start, .. }` reads one scalar.
        if self.ty_of(e).is_ok_and(|t| !self.proto.owns_heap(&t)) {
            return true;
        }
        match e {
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

    /// Whether the program declares a callable of this name — a function, a
    /// method, or a projection: `Declared::is_function`'s reading.
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
    /// the CALLER's to release after the call — RFC-0125 §3 M3, the argument
    /// slice.
    ///
    /// `movecheck` recognises an allocating argument by its SHAPE, because it
    /// has no lowering: a call, a String `+`, an array or struct literal the
    /// boundary heapifies, a match whose every arm builds, a forced `lazy`
    /// field. Sixteen shapes, each read off the source. This pass LOWERED the
    /// argument, so it has already answered the same question: a name it
    /// minted whose type owns heap, which no place was read into and no
    /// lending producer handed back, IS the census's shape A and shape B.
    /// [`NameInfo::releases`] is that answer, and it is the whole recording
    /// rule.
    ///
    /// What the CALLEE does with the value is a second question and the same
    /// one both passes ask: [`vyrn_frontend::movecheck::arg_verdict`], the
    /// rule stated once. The four fields it reads that a walk derived are
    /// stated here from the body instead — the producer, the element
    /// producers, whether a view hands out a copy, and the release kind.
    fn arg_released(&self, e: &'a Expr, t: Name, callee: &str, ix: usize) -> bool {
        use vyrn_frontend::movecheck as mc;
        // A named value is nobody's temporary: `f(s)` hands over what `s`
        // owns, and the binding keeps the row.
        if matches!(e, Expr::Var { .. } | Expr::Consume { .. }) {
            return false;
        }
        // A forced `lazy` field read IS a call — nothing is cached, and every
        // read is a fresh owned value (RFC-0085 M4a). This pass binds a
        // BORROW for it, because the read names a place; the value behind the
        // place is still the caller's to free, and the key says so whether or
        // not this pass releases the temporary itself.
        let info = &self.body.names[t as usize];
        let forced = self.forces_a_thunk(e);
        if !info.releases && !forced {
            return false;
        }
        // A producer whose result IS its argument (`blackBox`) built nothing
        // this frame may free when it hands back a borrow. When it was handed
        // an owned temporary it took it (`call` marks the position
        // `consume`), and the result is the one name that frees it.
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
        // How the value came to be, in the spelling `arg_verdict` partitions
        // on: a call answers its own name, the one allocating OPERATOR
        // answers nothing, and every other build answers a name no user
        // function can have.
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
        // Round forty-six: a call THROUGH A FN VALUE names no function, so no
        // capability row answered above and the verdict fell to `Unknown`.
        // The answer is the meet over the signature's closed target set, which
        // the pass that reads every body states by key
        // ([`vyrn_frontend::movecheck::Facts::fnval_clear`]). `usersById(req,
        // onGetUser)` in `examples/rpc.vyrn` is the shape: the generated
        // dispatcher calls `cb(Done(..))`, and the built reply is the caller's
        // to free.
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

    /// The type a forced `lazy` field read yields, or `None` where the read is
    /// an ordinary field of a record — see [`Builder::arg_released`].
    fn forced_ty(&self, e: &Expr) -> Option<Type> {
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
        let inner = vyrn_frontend::types::deferred(&f.ty)?;
        self.proto.owns_heap(inner).then(|| inner.clone())
    }

    fn forces_a_thunk(&self, e: &Expr) -> bool {
        self.forced_ty(e).is_some()
    }

    fn read_val_inner(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        let ty = self.ty_of(e).ok();
        let owns = ty.as_ref().is_some_and(|t| self.owns(t));
        match e {
            Expr::Field { .. } if owns => {
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

    /// RFC-0114 R1': the unnamed receiver of a field or element read, freed
    /// after the read because THIS FRAME owns it (RFC-0125 §3 M3, the
    /// derivation slice).
    ///
    /// The plan is not asked. A receiver is pending only where the value the
    /// place was read out of is an owned name this frame minted
    /// ([`Builder::place`]'s fallback), which is R1′'s whole question: a
    /// lending producer binds a borrow and mints no receiver at all, and a
    /// producer whose type owns no heap mints an unowned one. The hole is
    /// the field the read TOOK, and it is read off the source rather than
    /// off the plan's row — a scalar read takes nothing and leaves none.
    ///
    /// `borrowed` says the read yields a heap value its consumer borrows
    /// (`f(x).rhs.startsWith("{")`, `weekdayLetters()[1]`) rather than a
    /// scalar, or a field a binding took. Such a receiver must outlive the
    /// consumer, so its free is an argument-temporary drop keyed by the node
    /// that PRODUCED the receiver (RFC-0125 M3, third slice): each compiled
    /// backend tees that node's value and frees it after the call or
    /// operator that consumed the read. The core drops it after the
    /// consumer's binding, the same point. The placer writes the row only
    /// where such a drain encloses the read; elsewhere the receiver stays
    /// held and the judgment refuses it.
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
        // An element's receiver is `@at`'s argument, and the AST walk frees
        // it by the key an argument temporary has.
        if let (false, Expr::Call { name, args, .. }) = (took, e) {
            if self.arg_released(&args[0], r, name, 0) {
                self.body.names[r as usize].arg_drop = Some(producer);
            }
        }
        // The read that took a heap value out of the receiver leaves a hole
        // where it was, and the release walks around it. A take the walk
        // cannot be told to skip — an ELEMENT, which the plan spells `[]` —
        // stands for nothing, so the receiver stays held and the kernel says
        // so, exactly as a row without a hole did.
        let holes: Vec<String> = match (took, e) {
            (false, _) => Vec::new(),
            (true, Expr::Field { field, .. }) => vec![format!(".{field}")],
            (true, _) => return,
        };
        self.body.names[r as usize].holes = holes;
        self.body.names[r as usize].receiver_malloc = malloc;
        out.push(St::Drop(r, Site::None, 0, None));
    }

    /// Where each part of a record literal goes, on the name the literal is
    /// bound to: "the field `R.s`", one per field in order.
    ///
    /// A literal takes its parts and the checker names the field each part
    /// went into. `Rhs::Make` is a list of values with no names on it, so the
    /// fact rides on the binding — and a literal is bound at two doors, a
    /// reader's `let` and the temporary an inline one gets. It was written at
    /// the first alone, so `return R { s: x }` was told the part went into
    /// "the literal" (RFC-0125 §3 M3, row 07). Empty for an array, a map and
    /// a variant, whose parts the checker does not name either.
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

    /// An expression in a TAKE position: a `let`, a `return`, a store, a part
    /// of a literal, a `consume` argument. A name, or a literal.
    fn val(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        // The rebind's flag answers for THIS expression and no expression
        // inside it: `n = n + size(if c { names } else { .. })` stores an
        // Int64 and the join still binds an owning temporary.
        let rebinding = std::mem::take(&mut self.rebinding);
        if let Some(l) = lit_of(e).or_else(|| self.schema(e)) {
            return Ok(Val::Lit(l));
        }
        match e {
            Expr::Var { name, line } => match self.lookup(name) {
                Some(n) => Ok(Val::Name(n)),
                // A nullary constructor (`None`, a fieldless variant) parses
                // as a bare name, and the row makes it: the same variant
                // `Some(x)` makes, with one part less. The temporary owns
                // nothing and borrows nothing, for the reason a literal does
                // — the payload that would allocate is the variant this is
                // not — so the plan places no release at it and the value is
                // nobody else's.
                None if name == "None" || self.is_variant(name) => {
                    let ty = self.ty_of(e)?;
                    self.nullary(name, ty, *line, out)
                }
                // A function's name stored as a value: the closure enum's
                // variant for it, which captures nothing and so owns nothing.
                // The type is the one the value ends up as, where the row has
                // it.
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
                // Module state lives for the whole module and nothing
                // may take it (RFC-0013): `movecheck` refuses passing it
                // to a `consume` parameter or returning it, and `own`
                // notes `let x = g` as a borrow. So a read of it in any
                // position is a borrow, and the name it yields is one
                // the kernel does not own.
                None => self.global_read(e, name, *line, out),
            },
            Expr::Consume { place, line } => match &**place {
                Expr::Var { name, .. } => match self.lookup(name) {
                    Some(n) => Ok(Val::Name(n)),
                    // `consume <module state>`: a read of the global, which
                    // is a borrow the kernel then refuses the take of
                    // (RFC-0013, and RFC-0125 §3 M3, the census, row 10). It
                    // used to be a gap, so the whole body went unjudged.
                    None => self.global_read(place, name, *line, out),
                },
                _ => self.take_prefix(place, *line, out),
            },
            Expr::Lambda { .. } => self.lambda(e, out),
            _ => {
                let ty = self.ty_of(e)?;
                if is_place_read(e) && self.owns(&ty) {
                    // `best = m.name`, `if c { parts[0] } else { "" }`: the
                    // name this reaches is a borrow (`movecheck::names_a_place`
                    // says so at the `let` and at the store), and every take
                    // that would own it — a `return`, a literal part, a
                    // `consume` argument — is refused there without a `.copy()`.
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
                    // The other door: `size(if c { names } else { [..] })`
                    // binds no name a reader wrote, and the temporary owns
                    // and releases the result just the same.
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

    /// A lambda literal (RFC-0023). Its captures are reads of the enclosing
    /// names — a capture is by read, and a stored closure snapshots what it
    /// captured (RFC-0037), so the enclosing frame still owns its value. In an
    /// argument position the literal is monomorphized away and owns nothing;
    /// as a `let`'s initializer the plan's note says whether the closure
    /// value is this frame's. The lambda's own body is a separate frame,
    /// built by [`Builder::lambda_frame`].
    fn lambda(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        let caps = self.captures(e);
        let ty = self.ty_of(e).unwrap_or(Type::Unit);
        let t = self.name("@lambda", ty.clone(), false, e.line());
        // Where the literal is written ([`NameInfo::closure_reads`]). The cell
        // is taken, so a lambda in the BODY of this one — and a sibling lambda
        // in a later argument of the same call — asks the position it is
        // written at rather than this one's.
        self.body.names[t as usize].closure_reads = self.closure_reads(e, &caps);
        out.push(St::Let(t, Rhs::Prim(Op::Closure, caps.clone(), Some(ty))));
        self.lambda_frame(e, &caps)?;
        Ok(Val::Name(t))
    }

    /// The lambda's own frame (RFC-0125 M3, third slice), judged like a
    /// function's. Its captures are the enclosing names spelled again as
    /// borrowed inputs — the closure reads what it captured and the enclosing
    /// frame keeps owning it — and its parameters are `read` (RFC-0023). Its
    /// own bindings are ordinary: the plan keys their rows by the lambda's
    /// nodes under the enclosing function's name, which is where both
    /// compiled backends now read them (`direct.rs`'s shell carries the
    /// owner's name; `lib.rs` keeps `placed` across the lift). An expression
    /// body is a `return` of its value at no site: nothing an engine runs
    /// stands there, so a name still held at it is refused, not placed.
    fn lambda_frame(&mut self, e: &'a Expr, caps: &[Val]) -> Result<(), Gap> {
        let Expr::Lambda { params, body, line } = e else {
            return Ok(());
        };
        let decls = self.proto.types();
        let (ptys, ret): (Vec<Type>, Option<Type>) = match self.ty_of(e).ok() {
            // A `lazy T` field's initializer is a nullary closure (RFC-0085).
            Some(t) if vyrn_frontend::types::deferred(&t).is_some() => {
                (Vec::new(), vyrn_frontend::types::deferred(&t).cloned())
            }
            Some(t) => match vyrn_frontend::types::resolve(&t, &decls) {
                Type::Fn(ptys, r) => (ptys, Some(*r)),
                _ => return gap("a lambda the checker did not type as a function", *line),
            },
            // The checker did not type the literal: an argument of a generic
            // the instance monomorphized away. Its body is typed, so each
            // parameter has the type of its first use there.
            None => {
                let (mut vars, mut calls) = (Vec::new(), Vec::new());
                mentions_in_lambda(body, &mut vars, &mut calls);
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
                unreached: Vec::new(),
            },
        );
        self.body.name = lambda_spelling(&outer.name, *line);
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
            // RFC-0037: the closure's result is its caller's, and a capture
            // is the enclosing frame's. The kernel refuses a take of one
            // (RFC-0125 §3 M3, the census, row 28).
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
        self.body.lambdas.push(frame);
        Ok(())
    }

    /// The names of this body a lambda mentions, as a place or as a callee
    /// (`n -> f(n) + 1` captures the function value `f`).
    ///
    /// The mention set is [`mentions_in_lambda`]'s, which reads the body and
    /// honours the body's own bindings. `ast::mentions_place` cannot answer
    /// this question: it answers `true` for every name once the lambda has a
    /// block body, so a frame captured every name in scope, and a capture is a
    /// READ — every block-bodied lambda written after a `consume` was refused
    /// as a use of the consumed name (round two's F2-051).
    fn captures(&self, e: &Expr) -> Vec<Val> {
        let Expr::Lambda { params, body, .. } = e else {
            return Vec::new();
        };
        let (mut vars, mut calls) = (Vec::new(), Vec::new());
        mentions_in_lambda(body, &mut vars, &mut calls);
        let mut caps = Vec::new();
        for (name, n) in &self.scope {
            if params.iter().any(|p| p.name == *name) || caps.contains(&Val::Name(*n)) {
                continue;
            }
            if reads_place(&vars, name) || calls.contains(&name.as_str()) {
                caps.push(Val::Name(*n));
            }
        }
        caps
    }

    /// [`NameInfo::closure_reads`] for one lambda literal: the captures its
    /// body reads as VALUES, where the literal is written somewhere the
    /// closure value may outlive the call, and `None` where it may not.
    fn closure_reads(&mut self, e: &Expr, caps: &[Val]) -> Option<Vec<Name>> {
        if self.call_keeps.take() == Some(false) {
            return None;
        }
        let Expr::Lambda { body, .. } = e else {
            return None;
        };
        // The names the body MENTIONS, which is not the names it captures: a
        // callee is a name the core captures — the closure has to reach the
        // body — and no value the closure holds.
        let (mut vars, mut calls) = (Vec::new(), Vec::new());
        mentions_in_lambda(body, &mut vars, &mut calls);
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

    /// A read of module state as a value: a borrow of the global, because
    /// RFC-0013 gives it no owner a frame can take from. `e` is the
    /// expression whose type this is.
    fn global_read(
        &mut self,
        e: &'a Expr,
        name: &str,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Val, Gap> {
        let ty = self.ty_of(e)?;
        let t = if self.owns(&ty) {
            self.borrow_name(e, ty, line)
        } else {
            self.temp(ty, line)
        };
        out.push(St::Let(t, Rhs::Read(Place::Global(name.to_string()))));
        Ok(Val::Name(t))
    }

    /// The `consume p` prefix (RFC-0093). The rule is stated here, where the
    /// desugar is written, because it is about the KEYWORD rather than about
    /// ownership: `consume make()` and `make()` denote the same value, and
    /// the kernel has no keywords (RFC-0125 §3 M3, the census, rows 08 and
    /// 09). Two refusals, in the checker's own words. An element is not a
    /// place a take reaches, because nothing walks around an element hole. A
    /// value that names no place at all is already owned, so there is no
    /// place to leave a hole in.
    fn take_prefix(&mut self, e: &'a Expr, line: usize, out: &mut Vec<St>) -> Result<Val, Gap> {
        take_names_a_place(e, line, false)?;
        self.consume_names_a_borrow(e, line)?;
        self.take_place_at(e, out, true)
    }

    /// RFC-0125 §3 M3, row 11: a prefix `consume` of a BORROW hands somebody
    /// else's buffer away. The caller owns a `read` or `modify` parameter
    /// (RFC-0089 rule 2) and the frame that made a capture owns it
    /// (RFC-0037), so neither may be emptied here. Stated where the keyword
    /// is, as rows 08 and 09 above are: the write-back of RFC-0082's place
    /// desugar (`s.dense.push(i)`) reaches [`Builder::take_place`] with no
    /// `consume` in the program, and it changes no owner.
    ///
    /// The refusal names the ROOT, as `movecheck::check_take` names it, and
    /// the borrow flag carries the heap gate the checker asks for: a record
    /// of `Int64`s has no buffer to hand away.
    fn consume_names_a_borrow(&self, e: &'a Expr, line: usize) -> Result<(), Gap> {
        let Some((root, path)) = vyrn_frontend::ast::place_path(e) else {
            return Ok(());
        };
        let Some(n) = self.lookup(&root) else {
            return Ok(());
        };
        let info = &self.body.names[n as usize];
        match &info.borrow_kind {
            // The sentence names the ROOT and the menu names the PATH: a
            // `consume` goes on the parameter, a `.copy()` on what was read
            // out of it.
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

    /// A join arm that yields a name bound OUTSIDE the construct hands its
    /// value on without giving it up — `let rel = if p == "" { st } else { p
    /// + "/" + st }` is `st` on one edge and a fresh buffer on the other,
    /// and the frame holds one value under two names afterwards.
    ///
    /// This is the analysis's `Gone::Aliased`, stated where the alias is
    /// made rather than read off a note. The name that goes OUT stops being
    /// this frame's: one edge handed its value on and the other did not, so
    /// a release after the join frees a buffer the joined name still holds
    /// on one path. It is not a borrow either — nothing else owns it, and
    /// the frame simply stops answering for it, which is the leak RFC-0089
    /// takes over a double free. The RESULT keeps its own answer: on the
    /// other edge it holds a buffer of its own.
    ///
    /// `mark` is the name count before the arms were lowered, which is how a
    /// name from outside is told from a payload binder: `match o { Some(v)
    /// => v }` yields a binder minted inside the arm, and the scrutinee is
    /// this frame's to take (`takes_scrutinee`).
    ///
    /// The handover is stated ONCE, and it says the result is released where
    /// the name it was handed would have been. A LOOP breaks that: a `let`
    /// inside the body is released on every turn, while a name bound outside
    /// the loop is handed out again on the next one. The first turn frees the
    /// buffer, the second reads it and frees it again — the double free
    /// `VYRN_LEAK_CHECK=1` reports as exit 134. So the name the handover
    /// stood down is reported here, and the `let` that OWNS the result
    /// refuses ([`Builder::loop_aliased`]). A rebind (`found = match a { ..
    /// => found }`) binds nothing and releases once, and stays lowered as it
    /// was.
    fn alias_out(&mut self, v: &Val, mark: usize, line: usize) -> Option<String> {
        let Val::Name(m) = v else { return None };
        let m = *m as usize;
        if m >= mark {
            return None;
        }
        // Only a name that still OWNS can be freed twice. A borrow, a literal
        // and a name a previous arm already stood down all release nothing,
        // and a loop VARIABLE is minted above the mark, so each turn's element
        // is its own.
        let repeated =
            self.body.names[m].releases && self.loop_marks.last().is_some_and(|lm| m < *lm);
        self.body.names[m].releases = false;
        self.body.names[m].not_owned = Some(NotOwned::Aliased(line));
        repeated.then(|| self.body.names[m].source.clone())
    }

    /// A move out of a sub-place: `consume x.f`, or the receiver a rebuilding
    /// builtin hands back (`s.dense.push(i)` is `s.dense = @push(s.dense, i)`).
    /// The value leaves into an owned name and the base keeps a hole.
    fn take_place(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        self.take_place_at(e, out, false)
    }

    /// The same, saying whether the hole STAYS: a `consume x.f` empties the
    /// place and nothing fills it, so the base's release walks around it
    /// from here on (RFC-0093 M2). The write-back form fills the hole with
    /// the store that follows the call, so its base keeps none.
    ///
    /// This is where the core states a binding's holes. It used to read them
    /// off `own::Ownership::holes` at [`Builder::keyed`] — the plan's answer
    /// to the question the core's own `take` already asks, and the input
    /// half of the circle RFC-0125 §3 M3 names.
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
                // A hole the walk cannot be told to skip is not stated: the
                // release would then hand back a place the take gave away,
                // or — where the type declares its own `release` — free it
                // twice, because a function cannot be told to leave one
                // field alone (`refusals/r22_drop_with_a_hole.vyrn`). The
                // rule about the TYPE is `Owned`'s, and it is asked here.
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

    /// An expression as the right-hand side of a `let`. The temporaries the
    /// expression's own reads queued for release are left in `self.after` for
    /// the binding that follows; the ones queued by an enclosing expression
    /// are kept aside meanwhile, so a nested read cannot drop what an outer
    /// expression is still about to read.
    /// The String `jsonSchema<T>()` renders from `T`'s declaration at compile
    /// time, and `None` for every other expression. The arm's rewrite
    /// (`direct::Fn_::reflected`) renders the same declaration.
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

    /// The `impl Show` function a `print` or `@str` of one argument calls,
    /// where the program declares it.
    fn render_callee(&self, name: &str, args: &[Expr]) -> Option<String> {
        let [a] = args else { return None };
        if !matches!(name, "print" | "@str") {
            return None;
        }
        let t = self.ty_of(a).ok()?;
        let base = vyrn_frontend::types::resolve(&t, self.proto.types());
        vyrn_frontend::types::show_dispatch(&self.program.impls, &t, &base)
            .filter(|f| self.program.functions.iter().any(|d| &d.name == f))
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

    fn rhs(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Rhs, Gap> {
        let outer = std::mem::take(&mut self.after);
        let r = self.rhs_inner(e, out);
        let mine = std::mem::replace(&mut self.after, outer);
        self.after_of_rhs = mine;
        r
    }

    /// `a && b` as `if a { b } else { false }`, and `a || b` as
    /// `if a { true } else { b }` (RFC-0125 M7). The result is a `Bool`
    /// temporary each edge stores into, which is the shape an `if` expression
    /// takes. The type is not read off the node because the checker refuses
    /// any operand but `Bool`.
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
        };
        // The left operand runs where the expression does, and the emitter
        // drains its temporaries at the operator (`Fn_::binary`).
        self.drain += 1;
        let cond = self.read_val(lhs, out)?;
        let mark = self.after.len();
        let mut taken = Vec::new();
        let v = self.read_val(rhs, &mut taken)?;
        taken.push(store(v));
        // A temporary the right operand read is released on the edge that
        // made it: no other path evaluates it.
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
            // The five forms are named again here, and only here, because the
            // match below is EXHAUSTIVE on purpose: a new `Expr` variant must
            // fail to compile rather than fall into a catch-all. WHAT each one
            // is stays `lit_of`'s answer alone.
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {
                match lit_of(e) {
                    Some(l) => Ok(Rhs::Val(Val::Lit(l))),
                    None => gap("a literal form `lit_of` does not answer", e.line()),
                }
            }
            Expr::Var { .. } | Expr::Consume { .. } => Ok(Rhs::Val(self.val(e, out)?)),
            Expr::Unary { op, expr, .. } => Ok(Rhs::Prim(
                Op::Un(*op),
                vec![self.read_val(expr, out)?],
                self.produced(e),
            )),
            Expr::Binary { op, lhs, rhs, line } => {
                // `&&` and `||` are control flow. A prim row names both
                // operands and states no branch, so the row could not say that
                // the right one does not run when the left decides.
                if matches!(op, BinOp::And | BinOp::Or) {
                    return self.short_circuit(*op, lhs, rhs, *line, out);
                }
                // An operator drains its operands' temporaries in both
                // compiled backends (`binary`, `gen_binary`).
                self.drain += 1;
                // A String `+` is `@concat` written as an operator, and a
                // String comparison and a `=~` read their operands the same
                // way, so an allocating operand is an argument at
                // `(@concat, side)` (RFC-0125 §3 M3, the argument slice).
                // The `+` that concatenates is the one whose own type is
                // `String`, which this pass reads off the node.
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
                let fty = self.ty_of(e)?;
                let place = self.place(expr, out)?;
                if let Some((r, _, _)) = self.pending_receiver {
                    self.body.names[r as usize].receiver = Some(e as *const Expr as usize);
                }
                if self.owns(&fty) {
                    // `let sels = parse(q).sels`: the receiver is a temporary
                    // nobody names, so the binding takes the field out of it
                    // (`movecheck`: "the binding takes ownership of the
                    // extracted buffer"). The rest of the temporary is what
                    // the kernel then sees held.
                    return Ok(Rhs::Take(Place::Field(Box::new(place), field.clone())));
                }
                Ok(Rhs::Read(Place::Field(Box::new(place), field.clone())))
            }
            Expr::Call {
                name,
                args,
                line,
                type_args: _,
            } if name == "panic" || name == "@panicAt" || name == "serveStream" => {
                let r = if name == "serveStream" {
                    // RFC-0074 M3a: a compiled build has no accept loop.
                    let msg = Lit::Str(vyrn_frontend::trap::SERVE_STREAM.into());
                    Rhs::Call {
                        callee: name.clone(),
                        args: vec![(Val::Lit(msg), Capability::Read)],
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
            // `xs[i]` of a heapless element and a String's byte are section
            // 2.1's element read, one load at an address, and a map's entry is
            // a key read through the runtime's lookup; none is a call. The
            // seeded `place at` row yields `@slot(self, i)` and nothing else.
            // A user container's projection is another read.
            Expr::Call {
                name,
                args,
                line,
                type_args,
            } => {
                if self.reads_an_element(name, args, e) {
                    return Ok(Rhs::Read(self.place(e, out)?));
                }
                // A builtin whose argument names its callee is a call to that
                // function where the program declares it (RFC-0125 M7).
                // Elsewhere the builtin stays, with the effect its row states.
                if let Some((f, fwd)) =
                    vyrn_frontend::loader::routed_callee(name, type_args, args, |a| {
                        self.ty_of(a).ok()
                    })
                    .filter(|(f, _)| self.program.functions.iter().any(|d| &d.name == f))
                {
                    return self.call(&f, fwd, *line, self.produced(e), out);
                }
                // A render of a type the language does not render is a call
                // to its `impl Show` (RFC-0125 M7). `print` prints the String
                // the call hands back, and releases it after.
                if let Some(f) = self.render_callee(name, args) {
                    let r = self.call(&f, args, *line, Some(Type::Str), out)?;
                    if name == "@str" {
                        return Ok(r);
                    }
                    let t = self.temp(Type::Str, *line);
                    out.push(St::Let(t, r));
                    self.after.push(t);
                    return Ok(Rhs::Call {
                        callee: name.clone(),
                        args: vec![(Val::Name(t), Capability::Read)],
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
                let mut r = self.call(name, args, *line, self.produced(e), out)?;
                if let Rhs::Call {
                    kind: Callee::Fn,
                    solved,
                    ..
                } = &mut r
                {
                    *solved = self
                        .solved
                        .get(&(e as *const Expr as usize))
                        .cloned()
                        .unwrap_or_default();
                }
                Ok(r)
            }
            Expr::TryConstruct { name, args, .. } => {
                let mut vs = Vec::new();
                for a in args {
                    vs.push(self.val(a, out)?);
                }
                Ok(Rhs::Make(Ctor::Try(name.clone()), vs))
            }
            Expr::ArrayLit { elems, .. } => {
                let mut vs = Vec::new();
                for a in elems {
                    vs.push(self.val(a, out)?);
                }
                Ok(Rhs::Make(Ctor::Array, vs))
            }
            Expr::StructLit { name, fields, .. } => {
                let mut vs = Vec::new();
                for (_, a) in fields {
                    vs.push(self.val(a, out)?);
                }
                Ok(Rhs::Make(
                    Ctor::Record(
                        name.clone(),
                        fields.iter().map(|(f, _)| f.clone()).collect(),
                    ),
                    vs,
                ))
            }
            Expr::MapLit { entries, .. } => {
                let mut vs = Vec::new();
                for (k, v) in entries {
                    vs.push(self.val(k, out)?);
                    vs.push(self.val(v, out)?);
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
                // The plan keys an if-expression's edge rows by the expression
                // (RFC-0114 Rule N at an if-expression join), and every engine
                // runs them there.
                let site = e as *const Expr as usize;
                let res = self.temp(ty, *line);
                let c = self.read_val(cond, out)?;
                let mark = self.body.names.len();
                let mut t = Vec::new();
                let tv = self.val(then_branch, &mut t)?;
                let mut aliased = self.alias_out(&tv, mark, *line);
                let then_borrows = self.borrows(&tv);
                t.push(St::Store {
                    place: Place::Name(res),
                    value: tv,
                    old: Old::Nothing,
                    line: *line,
                    site: Site::None,
                    releases: false,
                });
                self.edge_drops(site, 0, &mut t)?;
                let mut f = Vec::new();
                match else_branch {
                    Some(eb) => {
                        let ev = self.val(eb, &mut f)?;
                        aliased = aliased.or(self.alias_out(&ev, mark, *line));
                        let else_borrows = self.borrows(&ev);
                        f.push(St::Store {
                            place: Place::Name(res),
                            value: ev,
                            old: Old::Nothing,
                            line: *line,
                            site: Site::None,
                            releases: false,
                        });
                        self.edge_drops(site, 1, &mut f)?;
                        // `if c { parts[0] } else { "Bool" }`: an arm that
                        // yields a borrow makes the result one
                        // (`movecheck::names_a_place`, one arm is enough).
                        if then_borrows || else_borrows {
                            self.body.names[res as usize].releases = false;
                            self.body.names[res as usize].borrow = true;
                        }
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
                            if let Some(a) = self.alias_out(&v, outer, *line) {
                                self.loop_aliased.insert(res, a);
                            }
                            // An arm that yields a borrow makes the result
                            // one (`movecheck::names_a_place`).
                            if self.borrows(&v) {
                                self.body.names[res as usize].releases = false;
                                self.body.names[res as usize].borrow = true;
                            }
                            body.push(St::Store {
                                place: Place::Name(res),
                                value: v,
                                old: Old::Nothing,
                                line: *line,
                                site: Site::None,
                                releases: false,
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
                // A DECLARED `Fallible` enum (RFC-0080 M3) asks its impl; the two
                // built-in sums have tags, and since RFC-0126 §8.11's M4b they
                // resolve to variant lists too — so the test is which list it is,
                // not whether there is one.
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
                self.leave_loops(tid, &mut fail);
                self.drops_at(Exit::Try, tid, &mut fail)?;
                // An `Option` fails with no binder, and the value it returns
                // is `None` of the frame's result. A `Result` fails with its
                // error binder, taken into `Err` of the frame's result.
                let value = match (fb.first(), self.ret.clone()) {
                    (Some(n), Some(rt)) => {
                        let t = self.temp(rt.clone(), *line);
                        fail.push(St::Let(
                            t,
                            Rhs::Call {
                                callee: "Err".into(),
                                args: vec![(Val::Name(*n), Capability::Consume)],
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
                // The name this closure binds is `bind`'s to give, so the
                // fact waits for it ([`Builder::pending_closure`]).
                self.pending_closure = self.closure_reads(e, &caps);
                self.lambda_frame(e, &caps)?;
                Ok(Rhs::Prim(Op::Closure, caps, self.produced(e)))
            }
        }
    }

    /// `?` on a declared `Fallible` type (RFC-0080 M3): the failing path
    /// returns the whole value, whichever failing variant it is; the succeeding
    /// path hands it to the impl's `success`, which READS it — the protocol
    /// declares a bare `self` — and answers a value of its own. So the copy the
    /// `?` made is still this frame's on that path, and this frame gives it
    /// back after the call. The failing path returns it instead, so each arm
    /// accounts for it exactly once.
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
        // The impl's `isSuccess` chooses the arm, and the failing arm is the
        // one its answer does not hold for. Both impl calls are functions
        // this program declares under the dispatched name, which reads the
        // value it is handed.
        let held = self.temp(Type::Bool, line);
        out.push(St::Let(
            held,
            Rhs::Call {
                callee: method("isSuccess"),
                args: vec![(sv.clone(), Capability::Read)],
                write_back: false,
                kind: Callee::Fn,
                ret: Some(Type::Bool),
                solved: Vec::new(),
                targets: Vec::new(),
            },
        ));
        let failed = self.temp(Type::Bool, line);
        out.push(St::Let(
            failed,
            Rhs::Prim(Op::Un(UnOp::Not), vec![Val::Name(held)], Some(Type::Bool)),
        ));
        let mut fail = Vec::new();
        self.leave_loops(tid, &mut fail);
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
                callee: success,
                args: vec![(sv.clone(), Capability::Read)],
                write_back: false,
                kind: Callee::Fn,
                // `success` answers the unwrapped value, which is what the
                // result name of the `?` holds.
                ret: Some(self.body.names[res as usize].ty.clone()),
                solved: Vec::new(),
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
            Expr::Var { name, line } => match self.lookup(name) {
                Some(n) => Ok(Place::Name(n)),
                None => {
                    if self.program.globals.iter().any(|g| &g.name == name) {
                        Ok(Place::Global(name.clone()))
                    } else {
                        gap("a place that is not a binding", *line)
                    }
                }
            },
            Expr::Field { expr, field, .. } => {
                let base = self.place(expr, out)?;
                Ok(Place::Field(Box::new(base), field.clone()))
            }
            Expr::Call { name, args, .. } if name == "@at" && args.len() == 2 => {
                let bty = self.ty_of(&args[0])?;
                let base = self.place(&args[0], out)?;
                // The receiver is this read's, and a field read in the index
                // would release it as its own.
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
                            // Row 11b's rule, stated where the producer is
                            // known: a callee's own allocation.
                            let malloc = matches!(
                                e,
                                Expr::Call { name, .. } if !name.starts_with('@')
                            );
                            self.pending_receiver = Some((t, e as *const Expr as usize, malloc));
                        }
                        Ok(Place::Name(t))
                    }
                    // A literal receiver — `"abc".byteLength`, which the
                    // corpus writes only inside a `test` body. The place is a
                    // temporary the site owns, bound to the literal itself so
                    // the chain above has a base that names a value
                    // (RFC-0125 §3 M6, seventh slice).
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
        // The capability of each argument position, by who the callee is.
        let decls = self.proto.types();
        let method = self
            .program
            .impls
            .iter()
            .flat_map(|i| i.methods.iter())
            .find(|m| m.name == name);
        // A seeded row whose result is its receiver's own type hands the
        // buffer back through the result, so the receiver is taken by the
        // call (`movecheck::sinks`).
        let rebuilds = prelude::rebuilds(name);
        // WHO the name resolves to, which is the one question the capability
        // of each argument position turns on. The answer is the row's
        // ([`Callee`]) rather than the two bools it used to be flattened into,
        // because a later reader that needs a third case has no other way to
        // ask (RFC-0125 §3 M3, the callee slice).
        let mut kind = Callee::Reserved;
        // A binding of function type is asked first, as `Checker::call` asks
        // it: a `fn`-typed parameter `h` shadows a function `h` the program
        // declares, and `h(req)` is a call through the value.
        let bound = self.lookup(name).is_some_and(|n| {
            matches!(
                vyrn_frontend::types::resolve(&self.body.names[n as usize].ty, decls),
                Type::Fn(..)
            )
        });
        let mut caps: Vec<Capability> = if bound {
            // A lambda captures by read and takes by read (RFC-0023).
            kind = Callee::Value;
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
        } else if matches!(name, "Some" | "None" | "Ok" | "Err") || self.is_variant(name) {
            kind = Callee::Ctor;
            vec![Capability::Consume; args.len()]
        } else if vyrn_frontend::checker::RESERVED.contains(&name)
            || vyrn_frontend::ast::is_surface_builtin(name)
        {
            // A reserved name with no prelude row (`fromJson`, `value`, a
            // generation-time surface builtin): its capabilities are the
            // prelude's answer where it has one, and `read` elsewhere. The
            // four surface builtins are one list
            // (`ast::SURFACE_BUILTINS`); naming two of them here left
            // `std/vyx`'s `vyxRegion` with no core (RFC-0125 §3 M6,
            // finding 12). The log levels stood here too, and their rows
            // took them to the branch above.
            (0..args.len())
                .map(|i| prelude::capability(name, i).unwrap_or(Capability::Read))
                .collect()
        } else if decls.contains_key(name) {
            kind = Callee::Named;
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
            // The generation host's three primitives (RFC-0076 M3b). They
            // exist only under `checker::set_gen_host`, so a program cannot
            // name them and no declaration does either; the emitter lowers
            // each in place. The host READS what it is handed — `reflect`
            // takes the String by address and stashes atoms of its own —
            // so the guest keeps every argument it owns.
            vec![Capability::Read; args.len()]
        } else {
            return gap_d("a call this slice cannot attribute", name, line);
        };
        if caps.len() < args.len() {
            return gap("a call with more arguments than parameters", line);
        }
        if let (Callee::Projection, Some(recv)) = (kind, args.first()) {
            if let Some(r) = self.inlined(name, recv, &args[1..], line, out)? {
                return Ok(r);
            }
        }
        let mut vs = Vec::new();
        let mut temps_to_drop = Vec::new();
        // The write-back exception: a rebuilding builtin's receiver passed by
        // name is handed back through the result and stored back after the
        // call, so the take it looks like changes no owner.
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
            // The result is the argument, so the argument's owner is the
            // result's: the temporary is taken and released once, as the
            // result.
            caps[0] = Capability::Consume;
        }
        // A stream is linear (RFC-0081): the callee disposes what it is
        // handed, so a position a stream fills takes it, whatever the
        // position's word says. Every builtin that takes one says `consume`.
        for (c, a) in caps.iter_mut().zip(args) {
            if self
                .ty_of(a)
                .is_ok_and(|t| matches!(vyrn_frontend::types::resolve(&t, decls), Type::Stream(_)))
            {
                *c = Capability::Consume;
            }
        }
        let drains = !lends_here;
        if drains {
            self.drain += 1;
        }
        let bound = match kind {
            Callee::Fn => self.targets_of(name, args),
            _ => Vec::new(),
        };
        let mut targets = Vec::new();
        for (k, (a, cap)) in args.iter().zip(caps.iter()).enumerate() {
            if let Some(Some(t)) = bound.get(k) {
                targets.push(t.clone());
                continue;
            }
            // Whether THIS position may keep what it is handed, for a lambda
            // literal written AT it ([`NameInfo::closure_reads`]). The
            // capability is read where the rule about a position is stated
            // once — `declared::arg_cap`, the declaration's word or the
            // seeded row's — and a position neither answers may keep it: the
            // safe direction is refusing an author once, never dangling a
            // capture. A lambda DEEPER inside the argument answers `None` and
            // escapes: an array, a map and a record literal retain what they
            // are given, whatever the call around them would have done
            // (`movecheck`'s four literal arms).
            self.call_keeps = matches!(a, Expr::Lambda { .. }).then(|| {
                vyrn_frontend::declared::arg_cap(&self.own.arg_caps, name, k)
                    .is_none_or(|c| c == Capability::Consume)
            });
            let v = if *cap == Capability::Consume {
                // Module state as the receiver (`books.push(b)`): a read of
                // it is a borrow nothing may take, so the write-back form
                // takes the place and the store after the call fills it, as
                // for a field or an element.
                let global = matches!(a, Expr::Var { name, .. }
                    if self.lookup(name).is_none()
                        && self.program.globals.iter().any(|g| &g.name == name));
                if k == 0
                    && rebuilds
                    && (global || !matches!(a, Expr::Var { .. } | Expr::Consume { .. }))
                    && is_place_read(a)
                {
                    // The write-back form on a field or an element: the
                    // receiver leaves and the store after the call fills the
                    // hole with what the call handed back.
                    self.take_place(a, out)?
                } else {
                    self.val(a, out)?
                }
            } else {
                self.read_arg(a, out, name, k)?
            };
            self.call_keeps = None;
            if let Val::Name(t) = v {
                // The drop the key stands for: `read_arg` set it, and a
                // temporary the read did not already queue is queued here
                // (RFC-0125 §3 M3, the argument slice). The key stands on a
                // BORROW too — a forced `lazy` field read is a fresh value
                // behind a place — and only a name this frame releases is
                // dropped.
                let info = &self.body.names[t as usize];
                if info.releases && info.arg_drop.is_some() && !self.after.contains(&t) {
                    temps_to_drop.push(t);
                }
            }
            vs.push((v, *cap));
        }
        if drains {
            self.drain -= 1;
        }
        self.after.extend(temps_to_drop);
        // `Int32(n)` and its nine siblings CONVERT between two scalars. Both
        // emitters already write the conversion and neither could read this
        // one, because the row said only that a reserved name was called —
        // 13,388 bodies of the gap tally waited on that and on nothing else
        // (RFC-0125 §3 M7). The operand is read above, where every argument
        // of every call is read, so the row states the operation over it and
        // the argument keying and the drains do not move.
        if let (1, Some(to)) = (vs.len(), vyrn_frontend::types::numeric_conv_target(name)) {
            return Ok(Rhs::Prim(Op::Conv(to), vec![vs[0].0.clone()], ret));
        }
        // `@concat(a, b)` is the String `+` the interpolation spine spells as
        // a call, so the row is the operator's, over the same arguments.
        if let ("@concat", [(a, _), (b, _)]) = (name, vs.as_slice()) {
            return Ok(Rhs::Prim(
                Op::Bin(BinOp::Add),
                vec![a.clone(), b.clone()],
                ret,
            ));
        }
        // A method is a call after dispatch (section 2.1), as the `Fallible`
        // switch states it.
        let (callee, kind) = match args.first().and_then(|r| self.dispatched(name, r)) {
            Some(f) if kind == Callee::Method => (f, Callee::Fn),
            _ => (name.to_string(), kind),
        };
        Ok(Rhs::Call {
            callee,
            args: vs,
            write_back,
            kind,
            ret,
            solved: Vec::new(),
            targets,
        })
    }

    /// The impl function the method `name` dispatches to on `recv`'s type:
    /// the one function the program declares under a name some protocol with
    /// that method mangles, and no generic function. A generic impl waits on
    /// `Cx::sigs`, which holds no instance of one.
    fn dispatched(&self, name: &str, recv: &Expr) -> Option<String> {
        let key = vyrn_frontend::types::type_key(&self.ty_of(recv).ok()?)?;
        let fs: std::collections::BTreeSet<String> = self
            .program
            .impls
            .iter()
            .filter(|i| i.methods.iter().any(|m| m.name == name))
            .map(|i| vyrn_frontend::types::impl_method_name(&i.protocol, &key, name))
            .filter(|f| self.concrete_fn(f))
            .collect();
        let mut fs = fs.into_iter();
        match (fs.next(), fs.next()) {
            (Some(f), None) => Some(f),
            _ => None,
        }
    }

    /// Whether the program declares `f` as a function that is no generic one.
    fn concrete_fn(&self, f: &str) -> bool {
        self.program
            .functions
            .iter()
            .any(|g| g.name == f && g.type_params.is_empty())
    }

    /// Per argument of a call to `name`, the [`Target`] a `fn`-typed
    /// parameter is bound to: a function the program declares, or a
    /// parameter of this body that is itself bound. Empty where the callee
    /// takes no function, and where any function argument is a value no
    /// target names (a lambda, a stored value), so the call keeps every
    /// argument as a value.
    ///
    /// A parameter is `fn`-typed as written: one of an alias type takes the
    /// stored value (RFC-0037), which is a value like any other.
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
            let Expr::Var { name: v, .. } = a else {
                return Vec::new();
            };
            let t = match self.lookup(v) {
                Some(n)
                    if self.body.params.contains(&n)
                        && matches!(self.body.names[n as usize].ty, Type::Fn(..)) =>
                {
                    Target::Param(n)
                }
                Some(_) => return Vec::new(),
                None if self.concrete_fn(v) => Target::Fn(v.clone()),
                None => return Vec::new(),
            };
            out.push(Some(t));
        }
        if out.iter().all(Option::is_none) {
            return Vec::new();
        }
        out
    }

    fn is_variant(&self, name: &str) -> bool {
        let decls = self.proto.types();
        decls.values().any(|d| {
            vyrn_frontend::types::declared_variants(&d.base)
                .is_some_and(|vs| vs.iter().any(|v| v.name == name))
        })
    }
}

// The descent over a body is `ast::body_scope_descent!`'s, where the AST is
// declared (RFC-0125 §3 M6).
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

/// Every `Var` node and every callee name in a lambda's body, nested
/// lambdas included, minus the names the body itself binds: what the frame
/// captures, and where an untyped parameter's type can be read.
///
/// The descent is `ast::body_scope_descent!`'s; what is this reader's own is
/// the two names it records. A name the body shadows is NOT recorded: a
/// capture is a read, so counting a shadow refuses a program that reads
/// nothing (round two's F2-051).
fn mentions_in_lambda<'e>(
    body: &'e LambdaBody,
    vars: &mut Vec<&'e Expr>,
    calls: &mut Vec<&'e str>,
) {
    struct Mentions<'e, 'o> {
        vars: &'o mut Vec<&'e Expr>,
        calls: &'o mut Vec<&'e str>,
    }

    impl<'e> BodyVisit<'e> for Mentions<'e, '_> {
        fn expr(&mut self, e: &'e Expr, locals: &std::collections::HashSet<String>) -> bool {
            match e {
                Expr::Var { name, .. } if !locals.contains(place_base(name)) => self.vars.push(e),
                Expr::Call { name, .. } if !locals.contains(name.as_str()) => {
                    self.calls.push(name.as_str())
                }
                _ => {}
            }
            true
        }
    }

    let mut v = Mentions { vars, calls };
    let mut locals = std::collections::HashSet::new();
    match body {
        LambdaBody::Expr(e) => body_expr(e, &locals, &mut v),
        LambdaBody::Block(b) => body_block(b, &mut locals, &mut v),
    }
}

thread_local! {
    static REFUSALS: std::cell::RefCell<Vec<crate::kernel::Refusal>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static FACTS: std::cell::RefCell<Option<Facts>> = const { std::cell::RefCell::new(None) };
    /// The core's own BODIES for the program last analysed on this thread, by
    /// the name each one is emitted under — RFC-0125 §3 M3, the driver slice.
    ///
    /// [`Facts`] is a side table keyed by AST node: a question an emitter asks
    /// about a node it already holds. This is the other channel, and §2.3 is
    /// about this one — "the emitter reads the core and writes wasm". A body
    /// here is the STATEMENT an emitter walks in place of the source, so what
    /// it carries is the whole of [`Body`] and not an answer per node. `None`
    /// under a name two different bodies share: two lambdas on one line.
    static BODIES: std::cell::RefCell<HashMap<String, Option<Body>>> =
        std::cell::RefCell::new(HashMap::new());
    static PLACED: std::cell::RefCell<Placed> = std::cell::RefCell::new(Placed::default());
    /// What the checker decided about a program, under `(its address, whether
    /// it was checked as a generator host, whether as a test host)` — the key
    /// [`vyrn_frontend::checker::recorded`] uses, for its reasons. Held as the
    /// `Rc` the checker made, so serving it costs a refcount rather than a copy
    /// of a map with a row per node.
    #[allow(clippy::type_complexity)]
    static DECIDED: std::cell::RefCell<
        Option<(Key, std::rc::Rc<vyrn_frontend::checker::Recorded>)>,
    > = const { std::cell::RefCell::new(None) };
}

/// What the checker decided about `program`, held for the emitters to read by
/// node — RFC-0125 §3 M5.
///
/// Every route that reaches an emitter calls this first, and the lowering
/// calls it in place of asking the checker itself, so one record serves the
/// lowering and both backends. The types in it are the ones the checker
/// WROTE, with a generic body's parameters still spelled as parameters; a
/// reader inside a monomorphized body substitutes its own instantiation in,
/// exactly as it does for every other type it is handed.
///
/// The held record is the last program's, so an emitter that walks a DIFFERENT
/// program makes its own here rather than reading somebody else's answers off
/// colliding addresses. That is what this is for: `direct::compile` is reached
/// from `vyrn build` with the lowering's record already in place and from a
/// generator host, a probe or a test with another program's, and only the key
/// can tell those apart — see [`Decided`] for the half of that the key alone
/// cannot do.
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

/// What [`decide`] gives back: the right to read the record, for as long as
/// the emitter holds it.
///
/// A guard and not a `set`, because the key is an ADDRESS and a `Program` is
/// a local. Two programs built one after another by the same code land at the
/// same address with the same shape — `vyrn-codegen`'s own tests do it in a
/// loop — and a record left behind by the first is served to the second as if
/// it were about the same nodes. So a record this made is put back the way it
/// was found. A record it only BORROWED (the lowering's, for this same
/// program) is left alone: the lowering's own reader outlives the emit.
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

/// Hold `made` as the record for `program`.
///
/// [`crate::lower_with`] calls this with the record its own check just made,
/// unconditionally: a program the caller EXTENDED since the last lowering is
/// the same address with different nodes in it, and only the caller that
/// checked it again knows that. [`decide`] is the other direction — an
/// emitter reached with no lowering behind it — and it stands down when the
/// record held is already this program's.
pub fn set_decided(program: &Program, made: &std::rc::Rc<vyrn_frontend::checker::Recorded>) {
    let key = key_of(program);
    DECIDED.with(|d| *d.borrow_mut() = Some((key, made.clone())));
}

/// The checker's type for the expression at `node`.
///
/// The two compiled backends each derived this themselves — the textual one
/// from the operands at every kind of node, the direct one in `Fn_::peek`'s
/// twenty arms — which is a second statement of the rule the checker states
/// when it types the node. The two can disagree, and did: an arm can only
/// report the type it happens to have PRODUCED, so `Array<String>` in one
/// arm and `["z"]` in the other are the same type in two shapes, and an
/// emitter that reads one of them feeds its merge the other.
///
/// `None` for a node the checker never typed. Two things are that: a node of
/// a program no lowering ran over on this thread (`VYRN_NO_PLACER=1`, or a
/// host that never linked this crate), and an expression an EMITTER built at
/// an emit site, which the checker never saw and cannot have an opinion
/// about. A reader stands down to what it did before in both cases.
pub fn node_ty(node: usize) -> Option<Type> {
    DECIDED.with(|d| {
        d.borrow()
            .as_ref()
            .and_then(|(_, r)| r.node_types.get(&node).cloned())
    })
}

/// The checker's type for the `match` or `if` expression at `node`.
///
/// The join subset of [`node_ty`], separated by the checker because a join is
/// where the disagreement above became a miscompile: a merge holds ONE value
/// and the arms are two producers of it.
pub fn join_ty(node: usize) -> Option<Type> {
    DECIDED.with(|d| {
        d.borrow()
            .as_ref()
            .and_then(|(_, r)| r.joins.get(&node).cloned())
    })
}

/// RFC-0125 §3 M3, the deletion-preparation slice: what an emitter reads off
/// the core in place of a per-node table in `own.rs`, keyed exactly as the
/// plan keys the table it replaces.
///
/// The core states each of these as a statement — a `St::Drop` of a
/// receiver's name, a `St::Drop` of an arm's payload binder — with the
/// binding's hole set on the name. A statement is not a lookup, and the
/// emitters walk the AST, so the walk over the core is folded into this
/// side table once per compile and every emitter reads it by node.
///
/// `compiler/vyrn-cli/tests/coretables.rs` counts every row of it over the
/// corpus. There is no second source to compare against any more: `own.rs`
/// states none of these tables (RFC-0125 §3 M3, the
/// emitter-reads-the-core-alone slice).
#[derive(Default, Clone, Debug)]
pub struct Facts {
    /// RFC-0114 R1′: the `Expr::Field` node of an unnamed receiver the core
    /// releases after the read, and the holes the release walks around.
    pub receivers: std::collections::HashMap<usize, Vec<String>>,
    /// Round forty's table: `(match, arm) -> [(binder, holes, kind)]`, the
    /// payload binders the arm's own body releases at its end. The kind is
    /// the binder type's release rule ([`vyrn_frontend::declared::Owned`]), which
    /// the interpreter needs and the two compiled backends read off the type
    /// themselves.
    pub arms: std::collections::HashMap<(usize, u32), Vec<(String, Vec<String>, Option<DropKind>)>>,
    /// RFC-0114 M2 and exit-residue round eighteen: the store statements
    /// whose old value the store releases — a `St::Store`'s `releases` at a
    /// [`Site::Node`]. The plan's `store_owned` and
    /// `store_fresh` are two halves of one answer, and this is the answer:
    /// the core folds the mention guard and the `fresh_str` exception in
    /// where both compiled backends spell them. A site absent from the map
    /// is one this pass states no answer for, and a reader falls back to the
    /// plan there.
    pub stores: std::collections::HashMap<usize, bool>,
    /// The stores of that map the core STANDS DOWN at, whatever the judgment
    /// would say — the two reasons a store releases nothing that the
    /// statement itself carries (RFC-0125 §3 M3, the store slice):
    ///
    ///   - the value hands the place back (`xs = xs.push(v)`,
    ///     `s.dense.push(i)`), so the buffer never leaves;
    ///   - the place owns no heap (`w = 2`, `data = grown`), so there is
    ///     nothing to release whatever holds it.
    ///
    /// Not read by an emitter: `stores` already has both folded in. It is
    /// here so the corpus test can pin the equality with the plan's own table
    /// as a RULE and not as a number — a plan row the core neither releases
    /// nor stands down is a release that stopped being stated.
    pub stood_down: std::collections::HashSet<usize>,
    /// Round twenty-eight: the statement-position calls whose owned result
    /// nothing binds and the core releases — a `St::Drop` at the
    /// statement's [`Site::Node`].
    pub discarded: std::collections::HashSet<usize>,
    /// The `for x in consume xs` loops that give their container back where
    /// the loop ends — a `St::Drop` of a `for_consume` name at the LOOP's
    /// [`Site::Node`].
    ///
    /// A release the core states and no row names: the take is what the
    /// kernel judges at the loop, and it is where a consuming loop over a
    /// `read` parameter's field or over module state is refused (the
    /// structural census, rows 10, 11 and 29). So the loop's own release and
    /// an exit row for the container are exclusive by construction, and this
    /// is the half a row cannot carry. An emitter asked the SOURCE for it
    /// until RFC-0125 §3 M3's event-stream slice, which is a second reading
    /// of `consume` beside the one the kernel already made.
    pub loop_gives_back: std::collections::HashSet<usize>,
    /// The `for` statements whose container release walks the BUFFER alone —
    /// [`Body::loop_buffers`], keyed by the loop's own node.
    ///
    /// The release itself is a placed row at the loop (or, for a consuming
    /// loop, [`Facts::loop_gives_back`]); this says what it walks. An
    /// emitter read the KIND of the plan's own row for it until RFC-0125 §3
    /// M3's container slice, which is the last thing that table was asked.
    pub loop_buffer_only: std::collections::HashSet<usize>,
    /// Per `return`, `?` or `break` node inside such a `for`: the loops, by
    /// node and innermost first, whose elements from the counter to the end
    /// the exit releases before its own rows. The core states them as rows
    /// ([`Builder::release_unreached`]); the AST walk reads this.
    pub unreached: std::collections::HashMap<usize, Vec<usize>>,
    /// RFC-0114 M1: the call-argument nodes whose temporary the caller
    /// releases after the call — [`NameInfo::arg_drop`], which the core sets
    /// wherever it lowers such an argument.
    pub arg_drops: std::collections::HashSet<usize>,
    /// RFC-0114 Rule N: per join node, the `(name, edge, holes)` releases
    /// one edge owes because another edge took the name — a `St::Drop` at a
    /// [`Site::Edge`], with the holes it walks around in the plan's spelling.
    pub edges: std::collections::HashMap<usize, Vec<EdgeRow>>,
    /// RFC-0125 §3 M3, row 11b: of those receivers, the ones a CALLEE
    /// allocated. Such a block is malloc-side whatever `region` is open at
    /// the call site, so the free stands inside one; an emitter still asks
    /// its own region depth, because this pass lowers a `region` as an
    /// ordinary block.
    pub receiver_malloc: std::collections::HashSet<usize>,
    /// Round twenty-seven's question, answered by the rule rather than by
    /// the plan's table: per `match`, `if let` or `?` node, whether the
    /// construct TOOK its scrutinee, so the boxes its binders came out of
    /// are its own to give back ([`St::Switch`]'s `consuming`). A site
    /// absent from the map is one this pass states no answer for.
    pub consuming: std::collections::HashMap<usize, bool>,
    /// Per `match`, `if let` or `?` node: the construct switches on a value
    /// the frame MADE, so the boxes its binders come out of are its own to
    /// give back ([`St::Switch`]'s `owns`).
    ///
    /// The union of two questions and not one of them: the construct took a
    /// named scrutinee, OR the scrutinee names no place the frame keeps,
    /// except on a declared release's receiver ([`Builder::owns_boxes`]).
    /// Each compiled backend asked the second half of the SOURCE — a
    /// `consume`, an expression with no place path, a `Map` lookup — beside
    /// the first half off the table above. This row is the one statement of
    /// both (RFC-0125 §3 M3, the box slice).
    pub owns_scrutinee: std::collections::HashSet<usize>,
}

/// One edge release: the name, the edge, and the holes the release walks
/// around, spelled relative to the name (`Elem.1`, RFC-0093 M2).
pub type EdgeRow = (String, u32, Vec<String>);

/// What the kernel decided over the core's own first build, keyed the way the
/// emitters key it — RFC-0125 §3 M3, the derivation slice.
///
/// The first build states no release the kernel has not judged owed, so the
/// judgment reports every one of them ([`crate::kernel::placement`]), and the
/// second build writes them down. A table in `own.rs` computed the same rows
/// a second time from `movecheck`'s facts; this is the one statement of the
/// rule, and the corpus test diffs it against what that table said.
#[derive(Default, Clone, Debug)]
pub(crate) struct Placed {
    /// Round forty's table, derived: `(switch site, arm) -> [(binder, holes)]`,
    /// the payload binders the kernel found still held where their arm ends.
    arms: std::collections::HashMap<(usize, u32), Vec<(String, Vec<String>)>>,
    /// RFC-0114 Rule N, derived: per join node, the `(name, edge)` releases
    /// one edge owes because another edge took the name. A sub-place row is
    /// spelled `d.line`, which every reader resolves as a place.
    edges: std::collections::HashMap<usize, Vec<EdgeRow>>,
    /// The store table, derived: the store statements the kernel found a HELD
    /// place at, which are the stores that owe the release of what they
    /// displace. Keyed by the store's own node, which is how both compiled
    /// backends key it.
    stores: std::collections::HashSet<usize>,
    /// The nodes that PRODUCED a borrowed receiver the kernel found still
    /// held — the receiver frees that ride as an argument-temporary drop
    /// (RFC-0125 M3, third slice). The channel went through the plan's own
    /// `arg_drops` set until the last table left, and it was never that
    /// table's answer: the placer wrote the row and the second build read
    /// it straight back.
    producers: std::collections::HashSet<usize>,
}

/// Whether the kernel found this store's place still holding — read by the
/// second build, empty on the first, where every store says [`Old::Pending`].
fn placed_store(site: usize) -> bool {
    PLACED.with(|p| p.borrow().stores.contains(&site))
}

/// The binders the kernel found held at the end of one arm, or `None` where
/// it found none — read by the second build, empty on the first.
fn placed_arm(site: usize, arm: u32) -> Option<Vec<(String, Vec<String>)>> {
    PLACED.with(|p| p.borrow().arms.get(&(site, arm)).cloned())
}

/// Whether the placer wrote an argument-temporary drop for the receiver this
/// node produced — read by the second build, empty on the first.
fn placed_producer(node: usize) -> bool {
    PLACED.with(|p| p.borrow().producers.contains(&node))
}

/// Rule N's rows for one join, as the kernel equalized its edges.
fn placed_edges(join: usize) -> Option<Vec<EdgeRow>> {
    PLACED.with(|p| p.borrow().edges.get(&join).cloned())
}

/// The core's answers for the program last analysed on this thread. `None`
/// when the placer is not installed (`VYRN_NO_PLACER=1`), in which case an
/// emitter reads the plan as it always did.
pub fn facts() -> Option<Facts> {
    FACTS.with(|f| f.borrow().clone())
}

/// The name a lambda literal on `line` inside the body named `outer` is built
/// and emitted under, and so its key in [`body_of`].
pub fn lambda_spelling(outer: &str, line: usize) -> String {
    format!("{outer}@lambda:{line}")
}

/// The core's own body for the function emitted under `name`, or `None` where
/// this pass built none — RFC-0125 §3 M3, the driver slice.
///
/// The key is [`crate::spell`] of the instance, which is the name the emitters
/// lower a function under: `max<Int64>` for a specialization, `main@lambda:26` for a
/// lifted lambda, `test@1` for a `test` block, and the empty name for module
/// state. A body this pass could not build (a [`Gap`]) is absent, and so is
/// one whose name another body shares; a reader walks the source instead —
/// the same standing down every reader of [`facts`] makes.
pub fn body_of(name: &str) -> Option<Body> {
    BODIES.with(|b| b.borrow().get(name).cloned().flatten())
}

/// The instance of `body` whose `fn`-typed parameters are bound (RFC-0023):
/// each parameter in `bound` leaves the parameter list, a call through it is
/// [`Callee::Fn`] to its target, and a call that passes it on names that
/// target. `None` where a bound parameter is read any other way (stored,
/// captured, handed to a position no target names), or where another name
/// shares its spelling, because a call through a value names its callee by
/// spelling.
pub fn specialize(body: &Body, bound: &[(Name, Target)]) -> Option<Body> {
    let spelled = |n: Name| &body.names[n as usize].source;
    let shared = bound.iter().any(|(n, _)| {
        (body.names.iter().enumerate()).any(|(m, i)| m != *n as usize && i.source == *spelled(*n))
    });
    if shared || bound.iter().any(|(_, t)| matches!(t, Target::Param(_))) {
        return None;
    }
    let by_spelling: Vec<(&str, &Target)> = (bound.iter())
        .map(|(n, t)| (spelled(*n).as_str(), t))
        .collect();
    let mut out = body.clone();
    bind_targets(&mut out.stmts, &by_spelling, bound);
    let mut reads = vec![0; out.names.len()];
    count_reads(&out.stmts, &mut reads);
    if bound.iter().any(|(n, _)| reads[*n as usize] > 0) {
        return None;
    }
    out.params.retain(|p| bound.iter().all(|(n, _)| n != p));
    Some(out)
}

fn bind_targets(ss: &mut [St], by_spelling: &[(&str, &Target)], bound: &[(Name, Target)]) {
    for s in ss {
        match s {
            St::Let(_, rhs) | St::Do { rhs, .. } => {
                let Rhs::Call {
                    callee,
                    kind,
                    targets,
                    ..
                } = rhs
                else {
                    continue;
                };
                if *kind == Callee::Value {
                    if let Some((_, Target::Fn(f))) = by_spelling.iter().find(|(c, _)| c == callee)
                    {
                        *kind = Callee::Fn;
                        *callee = f.clone();
                    }
                }
                for t in targets.iter_mut() {
                    if let Target::Param(p) = t {
                        if let Some((_, to)) = bound.iter().find(|(n, _)| n == p) {
                            *t = to.clone();
                        }
                    }
                }
            }
            St::If { then, els, .. } => {
                bind_targets(then, by_spelling, bound);
                bind_targets(els, by_spelling, bound);
            }
            St::Loop { body, .. } | St::Block { body, .. } => {
                bind_targets(body, by_spelling, bound)
            }
            St::Switch { arms, .. } => {
                for a in arms {
                    bind_targets(&mut a.body, by_spelling, bound);
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
                Rhs::Call { args, .. } => args.iter().for_each(|(v, _)| hit(v, out)),
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
///
/// One walk for two readers — [`Body::rows_by_statement`]'s backward walk over
/// the temporaries a statement computes, and the emitter's own screen over the
/// types a run names (RFC-0125 §3 M3, the interleave slice).
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

/// Each name a `let` of `ss` binds, at the row of `ss` its extent ends at
/// (RFC-0125 M7, the slot's extent).
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

fn names_in_rhs(r: &Rhs, out: &mut Vec<Name>) {
    match r {
        Rhs::Val(v) => names_in_val(v, out),
        Rhs::Prim(_, vs, _) | Rhs::Make(_, vs) => vs.iter().for_each(|v| names_in_val(v, out)),
        Rhs::Call { args, .. } => args.iter().for_each(|(v, _)| names_in_val(v, out)),
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

/// The kernel spells a hole `.f.g`; every table spells it `f.g`, relative to
/// the binding (RFC-0093 M2).
fn plan_holes(holes: &[String]) -> Vec<String> {
    holes
        .iter()
        .map(|h| h.trim_start_matches('.').to_string())
        .collect()
}

/// Fold one frame's statements into the side table. Called after the placer
/// has added every row, so the core here is the core the emitters will run.
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
                        // An entry even when it is empty: "this arm states
                        // its releases and owes none" is not the same answer
                        // as "this pass did not state them".
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
    // The body itself, for the reader that walks it rather than asking it
    // questions by node (RFC-0125 §3 M3, the driver slice). The fold and the
    // walk are the same set of frames, so they are filled at one site: a body
    // the fold does not see is one no emitter may read either.
    BODIES.with(|b| {
        b.borrow_mut()
            .entry(body.name.clone())
            .and_modify(|had| *had = None)
            .or_insert_with(|| Some(body.clone()));
    });
    fold_facts(body, proto, &body.stmts, out);
    out.loop_buffer_only
        .extend(body.loop_buffers.iter().copied());
    for (exit, walk) in &body.unreached {
        let loops = out.unreached.entry(*exit).or_default();
        if !loops.contains(walk) {
            loops.push(*walk);
        }
    }
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
        // The key stands whether or not this pass releases the temporary
        // itself: a `lazy` field read binds a borrow here, and the row still
        // says the caller frees the value after the call. What the row
        // answers is "does an argument-temporary drop stand at this node",
        // and that is what an emitter asks.
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

/// Whether the kernel's hard refusals fail the command. They do, and
/// `VYRN_NO_KERNEL=1` turns them off (RFC-0125 §3 M3, the default slice).
///
/// The knob is a bisect, not a mode: it says whether a refusal came from the
/// kernel or from somewhere else, the way `VYRN_NO_MOVECHECK=1` and
/// `VYRN_NO_PLACER=1` say it for the two passes beside it. Nothing is built
/// under it.
pub fn refuses() -> bool {
    !std::env::var("VYRN_NO_KERNEL").is_ok_and(|v| v == "1")
}

/// The hard refusals the placer met since the last call, on this thread: a
/// double free, a use after release, a join whose edges disagree, and a rule
/// the core states about a construct it does lower.
///
/// A REFUSAL is not a GAP. A gap ([`Gap`] with no `rule`) is a construct this
/// slice cannot lower, so the core has no opinion about the program and the
/// command goes on with the plan the analysis left. A refusal is an answer:
/// the program breaks a rule, and no placement repairs it. Only refusals are
/// collected here, and only refusals fail a command.
pub fn take_refusals() -> Vec<crate::kernel::Refusal> {
    REFUSALS.with(|v| std::mem::take(&mut *v.borrow_mut()))
}

/// The same refusals as `movecheck`-stage diagnostics, deduplicated, for the
/// one list a file's refusals come out in
/// (`vyrn_frontend::movecheck::refusals`, RFC-0125 §3 M3, the accumulation
/// slice). Installed into `own::analyze`'s slot by [`crate::install`].
///
/// A refusal reaches the same rule through more than one instance of the same
/// generic body, and a reader is owed one sentence per program mistake, so the
/// file, the line and the message are the identity. `file` is `None` for the
/// root module, which is what tells `vyrn fix` an edit is its to make. Ordering
/// is the caller's: it orders the two passes' lists together.
///
/// **What the identity may not collapse is one body's own repetition.**
/// `out.push(s) out.push(s)` on one line is two mistakes, and the checker
/// prints two sentences. A body is judged once and its refusals arrive
/// together, so the count of an identical sentence WITHIN one body's run is
/// part of the identity, and only the second instance of the same generic body
/// repeats it (RFC-0125 §3 M3).
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

/// RFC-0125 M3, first slice: the releases the plan did not place, placed.
///
/// For every function instance the core can be built for, the kernel walks
/// the body in placement mode: wherever an owned name is still held at an
/// exit — the fall-through end of its block, a `return`, a `?`, a `break`, a
/// `continue` — and the plan placed no release there, a release row is added
/// at that exit, keyed exactly as the plan keys its own (the exit's node and
/// the binding's node), and the binding is entered in the plan's droppable
/// table so every engine registers a slot for it. The engines then consume
/// the row through the one path RFC-0101 M4 gave them; nothing here reaches
/// an emitter.
///
/// What this closes is the class the plan's own fold names — "the in-loop
/// exits keep their leak until the fold can order across a back edge" — and
/// its fall-through twin: the named core orders across the back edge, and a
/// name the kernel finds held is held on every turn the exit runs.
///
/// Installed into `own::analyze` by [`crate::install`], so every consumer of
/// the plan sees the same rows. A body the core cannot build, or the kernel
/// refuses for a reason other than a missing release (a double free, a use
/// after release), is left exactly as the plan had it.
pub fn augment(program: &Program, own: &mut Ownership) {
    let _p = vyrn_frontend::prof::phase("placer");
    // A node is an address, and the allocator hands the same one out again:
    // a row this pass placed for the LAST program must not fire on this one.
    PLACED.with(|p| *p.borrow_mut() = Placed::default());
    let lw = vyrn_frontend::prof::phase("placer: lower_with");
    let lowered = crate::lower_with(program, own);
    drop(lw);
    // `VYRN_KERNEL_TRACE=1` prints every release the placer found owed, and
    // whether it could place it.
    let trace = std::env::var("VYRN_KERNEL_TRACE").is_ok();
    let mut added: Vec<(String, Release)> = Vec::new();
    // Every core body this pass builds, kept for the `Facts` fold below, and
    // the functions this pass wrote a row for (RFC-0125 §3 M3, the repetition
    // slice). A row is keyed by a NODE and a node belongs to one function, so
    // a body whose function is not named here is a body the fold would rebuild
    // to the same thing — the second build's whole purpose is the rows this
    // one added, and it added none there.
    let mut built: Vec<Option<Body>> = Vec::with_capacity(lowered.instances.len());
    let mut touched: std::collections::HashSet<String> = Default::default();
    // The judgment memo, when the host armed one — RFC-0125 §3 M3, the memo
    // slice. A body whose key is unchanged is served its own refusals and is
    // neither built nor judged. The key and the cache are the driver's
    // (`movecheck::Judgments`), and so is the rule about what a served body
    // leaves behind: an armed host reads refusals, and neither the facts nor
    // the rows below have a reader in it.
    let js = vyrn_frontend::prof::phase("placer: judgments");
    let memo = vyrn_frontend::movecheck::Judgments::open(program);
    drop(js);
    let _held = crate::append::Held::new(program);
    // Every body is built before any is placed, because the kernel asks the
    // effect judgment whether a callee writes module state (RFC-0125 M7), and
    // the judgment joins every body. A body the memo serves is not built, and
    // a call into it is judged as pure.
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
    // A `test` (RFC-0015) or `bench` (RFC-0055) body is a body, and the
    // kernel judges it like any other (RFC-0125 §3 M3, the reach slice). The
    // core lowers FUNCTION instances, so these two were the last bodies it
    // did not reach: the judgment said nothing about them, and one program
    // of `movecheck`'s own suite was accepted for that reason alone.
    let os = vyrn_frontend::prof::phase("placer: build_outside");
    let mut made_outside: Vec<Made> = Vec::with_capacity(lowered.bodies.len());
    for ob in &lowered.bodies {
        // The same key, spelled with the LINE beside the synthetic name: a
        // `test@<i>` index is global, and a test added to an earlier module
        // renumbers every later one, so the name alone would name a different
        // body of the same unchanged module.
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
            &ob.rows,
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
    // A hoist asked `kernel::writes` before the judgment was held, when no
    // call stored into module state. Where a frame hoisted a header and calls
    // a function that does, the answer may differ, so the body is built
    // again; every other body builds the same.
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
                &ob.rows,
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
                // A rule the core states, rather than a construct it cannot
                // lower: reported like the kernel's own refusals (RFC-0125
                // §3 M3, the checker's deletion path).
                if let Some(message) = g.rule {
                    REFUSALS.with(|v| {
                        v.borrow_mut().push(crate::kernel::Refusal {
                            message,
                            line: g.line,
                            file: inst.func.module.clone(),
                            body: inst.func.name.clone(),
                        })
                    });
                } else if trace {
                    eprintln!(
                        "placer: {} not lowered: {} {}",
                        inst.spelling(),
                        g.what,
                        g.detail
                    );
                }
                None
            }
        };
        if let Some(top) = &top {
            // `VYRN_KERNEL_TRACE=<fn>` prints that body's core, lambdas included.
            if std::env::var("VYRN_KERNEL_TRACE").is_ok_and(|v| v != "1" && top.name.contains(&v)) {
                eprintln!("{}", top.render());
            }
            // The body and every lambda frame under it: a lambda's rows are
            // keyed by its own nodes under the enclosing function's name, so a
            // row placed here lands where the emitters read (RFC-0125 M3,
            // third slice).
            place_frames(top, &inst.func.name, own, &mut added, &mut touched, trace);
        }
        remember(memo.as_ref(), key, refused_before);
        built.push(top);
    }
    let mut outside: Vec<Option<Body>> = Vec::with_capacity(lowered.bodies.len());
    for (ob, m) in lowered.bodies.iter().zip(made_outside) {
        let (key, made) = match m {
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
                outside.push(Some(top));
            }
            Err(g) => {
                if let Some(message) = g.rule {
                    REFUSALS.with(|v| {
                        v.borrow_mut().push(crate::kernel::Refusal {
                            message,
                            line: g.line,
                            file: ob.module.clone(),
                            body: ob.name.clone(),
                        })
                    });
                } else if trace {
                    eprintln!("placer: {} not lowered: {} {}", ob.name, g.what, g.detail);
                }
                outside.push(None);
            }
        }
        remember(memo.as_ref(), key, refused_before);
    }
    for (f, row) in added {
        touched.insert(f.clone());
        own.releases.entry(f).or_default().push(row);
    }
    // A SECOND build, after every row the placer added: the emitters read
    // the core's answers, and the core built above read the plan as it was
    // before this pass filled it (RFC-0125 §3 M3, the deletion-preparation
    // slice). The lowering is reused, so this costs the naming pass alone.
    // An armed memo has no reader for them: a served body contributes no
    // frame, so these facts would be a partial answer, and the reader of the
    // facts is the emitter — which a host that armed the memo does not run
    // (RFC-0125 §3 M3, the memo slice).
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
    // The same for the bodies that are no function of the program: an emitter
    // reads the core's answer by node, and a `test` body's nodes must be in
    // it or the emitter falls back to a plan row this pass just placed.
    for (i, ob) in lowered.bodies.iter().enumerate() {
        let fresh = if touched.contains(&ob.name) {
            build_outside(
                program,
                own,
                &ob.name,
                ob.module.clone(),
                ob.block,
                &ob.rows,
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

/// One body's refusals out of the judgment memo — RFC-0125 §3 M3, the memo
/// slice. `Some` when it has them, and then the body is neither built nor
/// judged.
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

/// Record what one body earned: every refusal from `from` to the end of the
/// list.
///
/// Every body with a key, whether the placer wrote a row for it or not. Serving
/// a body skips its placement as well as its judgment, and the rows it skips
/// have no reader in a host that armed the memo — the rule is
/// [`movecheck::reuse_judgments`]'s and is stated there.
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

/// The memory report for one frame — RFC-0125 §3 M3, the report slice.
///
/// `vyrn why --memory` and the editor's memory hints read this. Every word
/// comes off the core: the type table decided how the type is released, the
/// `let` decided whose the value is ([`NameInfo::not_owned`]), and the kernel
/// decided what took it and where the release stands. `own.rs` kept a second
/// walk of the tree that answered the same question off the checker's notes,
/// and this deletes it.
///
/// One row per source `let`, in line order — a temporary the lowering minted
/// names nothing a reader wrote, and is in no report.
fn report(
    body: &Body,
    owner: &str,
    missing: &[crate::kernel::Missing],
    took: &[Option<crate::kernel::Took>],
    released: &[Option<Vec<String>>],
    own: &mut Ownership,
) {
    // The releases the kernel found owed, and the holes each walks around:
    // this is "reclaimed at block exit", stated once.
    //
    // A row is a release whatever TABLE the placer files it under, and the
    // table is a question about where the emitter reads it, not about whether
    // the value comes back. `place_frames` files a whole-value row under four
    // keys — the exit, a join's edge, an arm's binder, and a store — and a
    // reader that counted the first alone called the other three a leak. `let
    // arg = mk(); return match fromJson(R, arg.j) { .. }` holds `arg` at both
    // arms of a returned `match`, so the kernel files one edge row per arm
    // and the report said "NOT reclaimed — nothing in this frame releases it"
    // about a value the audit sees freed (RFC-0125 §3 M3, the returned
    // match).
    let mut exits: HashMap<Name, Vec<String>> = HashMap::new();
    for m in missing {
        match m.kind {
            // The whole value, released on this path.
            crate::kernel::MissingKind::Exit
            | crate::kernel::MissingKind::Edge { .. }
            | crate::kernel::MissingKind::ArmBinder { .. } => {
                exits.entry(m.name).or_insert_with(|| plan_holes(&m.holes));
            }
            // A SUB-PLACE one edge took, released on the edge that did not:
            // it says nothing about the binding as a whole. A store row names
            // a place, which may be nobody's binding.
            crate::kernel::MissingKind::EdgePlace { .. } | crate::kernel::MissingKind::Store => {}
        }
    }
    // The rows are built against a borrowed `own` and put in at the end: the
    // type table is a map of every declaration in the program, and a copy of
    // it per frame is a copy per keystroke.
    let mut rows = std::mem::take(own.memory.entry(owner.to_string()).or_default());
    for (i, info) in body.names.iter().enumerate() {
        // The report is about the `let`s a reader wrote. A parameter, a `for`
        // variable, a pattern binder and a temporary this pass minted are all
        // keyed by a node too, and none of them is one.
        if !info.bound_by_let {
            continue;
        }
        // A generic function is lowered once per instantiation and the report
        // is about the source `let`, so the first instance answers for it.
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
            // What the type releases, asked first, because a reader told that
            // the type reclaims nothing needs no second sentence.
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
            (Some(NotOwned::Aliased(at)), _) => leaked(
                format!("NOT reclaimed — another binding aliases it at line {at}"),
                "aliased by another binding",
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
            // A must-use value handed to a BUILTIN is disposed of, not moved:
            // `movecheck::sinks` answers false at a linear parameter, so the
            // checker records no move there either, and the construct that
            // discharges the obligation is what frees it.
            (Some(NotOwned::MustUse(l)), Some(t)) if t.builtin => MemoryRow {
                name,
                line,
                text: discharged(l),
                last_use: None,
                moved_into: None,
                bucket: Bucket::Discharged,
            },
            // A `drop` the reader wrote reclaims it, so the automatic path
            // must not.
            (_, Some(t)) if t.how == crate::kernel::TookHow::Drop => MemoryRow {
                name,
                line,
                text: format!("reclaimed by `drop` at line {}", t.line),
                last_use: Some(t.line),
                moved_into: None,
                bucket: Bucket::Dropped,
            },
            // It left: whoever holds it now reclaims it, so this block must
            // not. A `return` gets the report's own words, because "a
            // `return`" names no taker a reader can go and look at.
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
            // A must-use value nothing took: the construct that DISCHARGES it
            // reclaims it, and a program that reaches here has been proved to
            // discharge every one of them (RFC-0075 M1, RFC-0095 M1).
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
                // The frame owns it and the kernel placed no release for it.
                // The sentence says what is known rather than guessing a
                // reason the core does not state.
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

/// "reclaimed at block exit — …", with the places a `consume` took out of the
/// value (RFC-0093 M2), which the release walks around.
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

/// The must-use sentence, one tense over from the menu `movecheck` prints:
/// there the reader is told what to write, here what the program already
/// wrote, and which lowering does the freeing.
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

/// Place what one built body owes, frame by frame (RFC-0125 §3 M3).
///
/// `owner` is the name the plan's tables are keyed by: a function's own name
/// for an instance, and the synthetic `test@<i>` / `bench@<i>` for a body
/// that is no function of the program (RFC-0015, RFC-0055). A lambda frame
/// under either is keyed by its enclosing body's name, so a row placed here
/// lands where the emitters read.
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
                    // A refusal no placement repairs: a double free, a use
                    // after release, a join whose edges disagree. A refusal,
                    // not a gap: the CLI fails the command with it. Every one
                    // the body earns, so the driver can merge by the binding
                    // and the line (RFC-0125 §3 M3).
                    REFUSALS.with(|v| v.borrow_mut().push(r));
                }
                continue;
            }
        };
        let rp = vyrn_frontend::prof::phase("placer: report");
        report(body, owner, &missing, &took, &released, own);
        drop(rp);
        for m in missing {
            // A store's row is keyed by the STORE and by nothing else: the
            // place it writes into may be module state or a sub-place, which
            // is no binding of this frame, so this row is read before the
            // name is (RFC-0125 §3 M3, the store slice).
            if m.kind == MissingKind::Store {
                let fresh = PLACED.with(|p| p.borrow_mut().stores.insert(m.site));
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
            // The receiver a call or an operator borrowed a heap field or
            // element out of (`f(x).rhs.startsWith("{")`): an argument
            // temporary of that consumer, keyed by the node that produced
            // the receiver, which both backends tee and free after the
            // consumer's drain (RFC-0125 M3, third slice).
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
            // The kernel spells a hole `.f.g`; the plan's tables spell it
            // `f.g`, relative to the binding (RFC-0093 M2). An element hole
            // (`.[]`) is one the walk cannot skip: no row, and the judgment
            // refuses the name.
            if m.holes.iter().any(|h| h.contains("[]")) {
                continue;
            }
            let holes: Vec<String> = m
                .holes
                .iter()
                .map(|h| h.trim_start_matches('.').to_string())
                .collect();
            // A declared release takes the whole value (RFC-0086): it cannot
            // be told a hole.
            if !holes.is_empty() && matches!(kind, DropKind::Release(..)) {
                continue;
            }
            match m.kind {
                // Rule N's table: one edge of a join still holds what another
                // took. Consumed by name, so a loop variable qualifies.
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
                // The same table, one level down: the sub-place one edge took,
                // released on the edge that did not. Spelled `d.line`, which
                // every reader of the table resolves as a place.
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
                // Round forty's table: the arm's unmoved payload binders, one
                // entry per binder so an emitter frees exactly those, each
                // with the holes its arm left in it.
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
            // The unnamed receiver of a field read: R1′'s row, with the
            // field the read took as its hole. The core states it on the
            // name itself (`NameInfo::receiver`, `NameInfo::holes`) and this
            // pass places nothing for it — `own.rs` has no receiver table
            // left to write into (RFC-0125 §3 M3, the
            // emitter-reads-the-core-alone slice).
            if info.receiver.is_some() {
                continue;
            }
            let Some(binding) = info.binding else {
                continue;
            };
            // A row the plan already placed here whose hole set is not the
            // kernel's at this exit (the analysis's set is per binding, the
            // kernel's is per path): the row keeps its key and takes the
            // kernel's set.
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
                    // The kernel's set at THIS exit, empty included: a row
                    // with no set falls back to the binding's own, which is
                    // per binding and not per path. `regexredux`'s `compile`
                    // abandons a `Builder` at three early `Err` returns that
                    // precede every take of its arrays, so the release there
                    // walks the whole record — the answer round fifty-two's
                    // `full` flag reconstructed from walk order, stated here
                    // by the pass that judged the path (RFC-0125 §3 M3, the
                    // walk's deletion).
                    holes: Some(holes),
                },
            ));
        }
    }
}
