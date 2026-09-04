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
//! ([`vyrn_frontend::own::Owned`]). It derives nothing about ownership itself:
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
    ArmBody, Block, Capability, Expr, Function, LambdaBody, Pattern, Program, Stmt, Type,
};
use vyrn_frontend::own::{DropKind, Exit, Fate, Leak, Owned, Ownership, Release};
use vyrn_frontend::prelude;

use crate::kernel::MissingKind;
use crate::{Instance, Node};

/// A name in a body: an index into [`Body::names`].
pub type Name = u32;

#[derive(Debug, Clone)]
pub struct NameInfo {
    /// The source spelling, or `@tN` for a temporary the naming pass minted.
    pub source: String,
    pub ty: Type,
    /// Whether the kernel tracks this name: it owns heap or carries a must-use
    /// obligation. A borrowed binding (a `read` parameter, a pattern binder of
    /// a non-consuming match, a `for` variable over a container it does not
    /// consume) is never owned, whatever its type.
    pub owned: bool,
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
}

/// The borrow a parameter's capability makes, or `None` for one that owns
/// what it is handed (`consume`).
fn param_borrow(cap: Capability, name: &str) -> Option<BorrowKind> {
    let cap = match cap {
        Capability::Read | Capability::Share => "read",
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
}

impl BorrowKind {
    /// What this borrow is, in words, for a refusal. `at` is the name the
    /// sentence is about: a second name for a parameter says so, which is
    /// how `movecheck::Borrow::what` words it.
    pub fn what(&self, at: &str) -> String {
        match self {
            BorrowKind::Param { cap, of } if at == of => format!("a `{cap}` parameter"),
            BorrowKind::Param { cap, of } => {
                format!("a second name for the `{cap}` parameter `{of}`")
            }
            BorrowKind::Capture => "a captured binding".to_string(),
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

#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Val {
    Name(Name),
    /// A literal: nothing to own. A string literal is static data
    /// ([`vyrn_frontend::own::Fate::Static`]).
    Lit,
}

#[derive(Debug, Clone)]
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
        /// `spawn callee(..)`: the call runs as a task. The effect judgment
        /// reads the marker (RFC-0125 §2.2, the fourth effect); the linear
        /// judgment and the emitters see an ordinary call.
        spawn: bool,
        /// Argument 0 is the receiver of a rebuilding builtin passed by name
        /// (`out.push(v)`): the call hands the buffer back through its result
        /// and the store after it puts it back, so the take changes no owner.
        /// That is `movecheck::sinks`'s write-back exception, which stays the
        /// checker's this slice (RFC-0125 §3 M3, the census).
        write_back: bool,
        /// The producer type: what the callee answers at this site, with the
        /// call's own type arguments already substituted. `None` for a call
        /// the checker did not type.
        ret: Option<Type>,
    },
    /// Arithmetic, comparison, interpolation, conversion: reads its operands.
    /// The second field is the producer type — the operator's own result,
    /// which is what `binop_type` decided and the destination did not.
    Prim(Vec<Val>, Option<Type>),
    /// A record, array, map or variant literal: takes its parts.
    Make(Vec<Val>),
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
    Drop(Name, Site),
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
    Loop(Vec<St>),
    /// A source block: its own scope, and the site the plan keys its
    /// fall-through release rows by.
    Block {
        site: usize,
        body: Vec<St>,
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
        line: usize,
    },
    /// An expression for its effect, on its line.
    Do(Rhs, usize),
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
    /// `None` where this pass does not state the answer: the `if let` and `?`
    /// desugars build arms of their own and consult no table, so a reader
    /// falls back to the plan for those sites. Closing that is the next
    /// step, and the emitters this slice flips do not read the table there.
    pub frees: Option<Vec<Name>>,
    pub body: Vec<St>,
    /// The `match` (or `if let`, or `?`) this arm belongs to, and which arm
    /// it is — the plan's key for an arm payload free and an edge release.
    pub site: usize,
    pub index: u32,
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
        if i.owned {
            format!("{}!", i.source)
        } else {
            i.source.clone()
        }
    }

    fn val(&self, v: &Val) -> String {
        match v {
            Val::Name(n) => self.spell(*n),
            Val::Lit => "lit".into(),
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
                callee,
                args,
                spawn,
                ..
            } => format!(
                "{}{callee}({})",
                if *spawn { "spawn " } else { "" },
                args.iter()
                    .map(|(v, c)| format!("{:?} {}", c, self.val(v)).to_lowercase())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Rhs::Prim(vs, _) => format!(
                "prim({})",
                vs.iter()
                    .map(|v| self.val(v))
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Rhs::Make(vs) => format!(
                "make({})",
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
                St::Drop(n, _) => out.push_str(&format!("{pad}drop {}\n", self.spell(*n))),
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
                St::Loop(b) => {
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
                St::Do(r, _) => out.push_str(&format!("{pad}do {}\n", self.rhs(r))),
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

/// The kind of an expression, for a gap's detail.
fn expr_kind(e: &Expr) -> &'static str {
    match e {
        Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => "literal",
        Expr::Var { .. } => "var",
        Expr::Unary { .. } => "unary",
        Expr::Binary { .. } => "binary",
        Expr::Field { .. } => "field",
        Expr::Call { name, .. } => {
            if name.starts_with('@') {
                "builtin call"
            } else {
                "call"
            }
        }
        Expr::TryConstruct { .. } => "try construct",
        Expr::ArrayLit { .. } => "array literal",
        Expr::StructLit { .. } => "record literal",
        Expr::MapLit { .. } => "map literal",
        Expr::Spawn { .. } => "spawn",
        Expr::IfExpr { .. } => "if expression",
        Expr::Match { .. } => "match",
        Expr::Try { .. } => "try",
        Expr::Lambda { .. } => "lambda",
        Expr::Consume { .. } => "consume",
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

/// Build the core of one instance.
pub fn build(program: &Program, inst: &Instance<'_>, own: &Ownership) -> Result<Body, Gap> {
    let mut types: HashMap<usize, Type> = HashMap::new();
    let mut produced: HashMap<usize, Type> = HashMap::new();
    for r in &inst.rows {
        if let Node::Expr(_) = r.node {
            if let Some(t) = r.ty.as_ref().or(r.has.as_ref()) {
                types.insert(r.node.id(), t.clone());
            }
            // The other half of the pair: what the node's own code produces.
            if let Some(t) = r.has.as_ref().or(r.ty.as_ref()) {
                produced.insert(r.node.id(), t.clone());
            }
        }
    }
    // The placed releases, by the exit they are at.
    let mut placed: HashMap<(Exit, usize), Vec<&Release>> = HashMap::new();
    for r in &inst.releases {
        placed.entry((r.exit, r.site)).or_default().push(r);
    }
    let mut b = Builder {
        program,
        own,
        proto: &own.proto,
        types,
        produced,
        placed,
        body: Body {
            name: inst.spelling(),
            file: inst.func.module.clone(),
            export: inst.func.is_export_extern,
            names: Vec::new(),
            params: Vec::new(),
            stmts: Vec::new(),
            lambdas: Vec::new(),
        },
        scope: Vec::new(),
        by_binding: HashMap::new(),
        temps: 0,
        func_name: inst.func.name.clone(),
        pending_receiver: None,
        drain: 0,
        after: Vec::new(),
        after_of_rhs: Vec::new(),
        stream_loops: Vec::new(),
    };
    let f: &Function = inst.func;
    // The instance's substitution, so a parameter's type here is the type the
    // instance has rather than the declaration's (RFC-0125 §3 M6, finding 14):
    // `map<Int64, Int64>`'s `f` is `fn(Int64) -> Int64`, which is the shape
    // RFC-0037 collected its stored sources under. Every other type in the
    // core comes from the instance's rows and is substituted already.
    let subst: HashMap<String, Type> = inst.subst.clone().into_iter().collect();
    // A declared release (`impl Owned for T { fn release(consume self) }`) IS
    // the release of `self`: its body frees the parts, and nothing releases
    // `self` again — so `self` is not a name the kernel owns there.
    let is_release = f.name.starts_with("Owned__") && f.name.ends_with("__release");
    for p in &f.params {
        let pty = vyrn_frontend::types::substitute(&p.ty, &subst);
        let owned = p.capability == Capability::Consume && b.owns(&pty) && !is_release;
        let n = b.name(&p.name, pty, owned, f.line);
        // RFC-0089 rule 2: a `read` or `modify` parameter may be observed and
        // passed on, never taken. The kernel refuses the take and needs the
        // capability to word it (RFC-0125 §3 M3, the census, rows 11 to 34).
        //
        // A must-use type is the exception RFC-0075 M1 states: "a stream
        // PARAMETER carries the obligation into the callee", whatever the
        // capability says, so the callee is the one that disposes of it and
        // `boxStream(s)` is not a take of the caller's value.
        if !b.proto.must_use(&b.body.names[n as usize].ty.clone()) {
            b.body.names[n as usize].borrow_kind = param_borrow(p.capability, &p.name);
        }
        b.scope.push((p.name.clone(), n));
        b.keyed(n, p as *const _ as usize);
        b.body.params.push(n);
    }
    let mut out = Vec::new();
    b.block(&f.body, &mut out)?;
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
    let mut types: HashMap<usize, Type> = HashMap::new();
    let mut produced: HashMap<usize, Type> = HashMap::new();
    for r in rows {
        if let Node::Expr(_) = r.node {
            if let Some(t) = r.ty.as_ref().or(r.has.as_ref()) {
                types.insert(r.node.id(), t.clone());
            }
            // The other half of the pair: what the node's own code produces.
            if let Some(t) = r.has.as_ref().or(r.ty.as_ref()) {
                produced.insert(r.node.id(), t.clone());
            }
        }
    }
    let mut b = Builder {
        program,
        own,
        proto: &own.proto,
        types,
        produced,
        placed: HashMap::new(),
        body: Body {
            name: String::new(),
            file: None,
            export: false,
            names: Vec::new(),
            params: Vec::new(),
            stmts: Vec::new(),
            lambdas: Vec::new(),
        },
        scope: Vec::new(),
        by_binding: HashMap::new(),
        temps: 0,
        func_name: String::new(),
        pending_receiver: None,
        drain: 0,
        after: Vec::new(),
        after_of_rhs: Vec::new(),
        stream_loops: Vec::new(),
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
    let mut types: HashMap<usize, Type> = HashMap::new();
    let mut produced: HashMap<usize, Type> = HashMap::new();
    for r in rows {
        if let Node::Expr(_) = r.node {
            if let Some(t) = r.ty.as_ref().or(r.has.as_ref()) {
                types.insert(r.node.id(), t.clone());
            }
            if let Some(t) = r.has.as_ref().or(r.ty.as_ref()) {
                produced.insert(r.node.id(), t.clone());
            }
        }
    }
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
        placed,
        body: Body {
            name: name.to_string(),
            file,
            export: false,
            names: Vec::new(),
            params: Vec::new(),
            stmts: Vec::new(),
            lambdas: Vec::new(),
        },
        scope: Vec::new(),
        by_binding: HashMap::new(),
        temps: 0,
        func_name: name.to_string(),
        pending_receiver: None,
        drain: 0,
        after: Vec::new(),
        after_of_rhs: Vec::new(),
        stream_loops: Vec::new(),
    };
    let mut out = Vec::new();
    b.block(block, &mut out)?;
    b.body.stmts = out;
    Ok(b.body)
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
    placed: HashMap<(Exit, usize), Vec<&'a Release>>,
    body: Body,
    scope: Vec<(String, Name)>,
    /// The plan keys a release by the node that owns the value: a `Stmt::Let`,
    /// a parameter, or the construct that owns a temporary.
    by_binding: HashMap<usize, Name>,
    temps: u32,
    /// The function's declared name — the key the plan's binding notes use.
    func_name: String,
    /// An unnamed receiver `place` minted for a field or element read, with
    /// the node that produced it, so the read can release it afterwards when
    /// the plan says the frame owns it (R1').
    pending_receiver: Option<(Name, usize)>,
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
}

impl<'a> Builder<'a> {
    fn owns(&self, ty: &Type) -> bool {
        self.proto.owns_heap(ty) || self.proto.must_use(ty) || self.proto.release_kind(ty).is_some()
    }

    fn name(&mut self, source: &str, ty: Type, owned: bool, line: usize) -> Name {
        let heap = self.proto.owns_heap(&ty);
        self.body.names.push(NameInfo {
            source: source.to_string(),
            ty,
            owned,
            heap,
            borrow: heap && !owned,
            borrow_kind: None,
            line,
            binding: None,
            receiver: None,
            producer: None,
            arg_drop: None,
            holes: Vec::new(),
        });
        (self.body.names.len() - 1) as Name
    }

    /// Record the plan's key for a name, and the name for the key.
    fn keyed(&mut self, n: Name, binding: usize) {
        self.body.names[n as usize].binding = Some(binding);
        self.body.names[n as usize].holes = self.plan_holes(binding);
        self.by_binding.insert(binding, n);
    }

    /// The plan's hole set for a binding, spelled for the kernel.
    fn plan_holes(&self, binding: usize) -> Vec<String> {
        self.own
            .holes
            .get(&self.func_name)
            .and_then(|m| m.get(&binding))
            .map(|hs| hs.iter().map(|h| format!(".{h}")).collect())
            .unwrap_or_default()
    }

    /// What the plan decided a named binding's fate is: whether THIS frame
    /// owns it. Static data, an alias of a place, a borrow, a capture, a value
    /// the arena holds and a value a callee took are not this frame's to
    /// release; a binding with no release rule for its type is, and the
    /// kernel will say so.
    fn fate_owned(&self, name: &str, line: usize, ty: &Type) -> Option<bool> {
        let Some(fate) = self.fate_of(name, line) else {
            return if self.owns(ty) { None } else { Some(false) };
        };
        Some(match fate {
            // The plan releases, moves or discharges it: it is this frame's.
            Fate::Reclaimed(..)
            | Fate::Moved { .. }
            | Fate::Dropped { .. }
            | Fate::Discharged(_) => true,
            // The plan could not type the initializer (`xs.toArray()` on a
            // SmallArray answers `unknown`), so its note says nothing about
            // ownership; the checker's type decides, as it does for a
            // binding with no note at all.
            Fate::Leaked(Leak::NoRelease { ty: noted, .. }) if noted == "unknown" => {
                return if self.owns(ty) { None } else { Some(false) };
            }
            Fate::Leaked(Leak::NoRelease { owns_heap, .. }) => *owns_heap,
            Fate::Static | Fate::Leaked(_) => false,
        })
    }

    /// The plan's note for a `let` binding, when it wrote one.
    fn fate_of(&self, name: &str, line: usize) -> Option<&Fate> {
        self.own
            .notes
            .get(&self.func_name)?
            .iter()
            .find(|n| n.name == name && n.line == line)
            .map(|n| &n.fate)
    }

    /// Whether a call's result points into one of its arguments, so the name
    /// bound to it is a borrow: a lending prelude row (`at`, `bytes`), the
    /// `value` box, or a projection an `impl` declares (RFC-0120).
    fn lends(&self, e: &Expr) -> bool {
        match e {
            Expr::Call { name, .. } => self.lends_name(name),
            _ => false,
        }
    }

    /// Whether a call by this name lends: `a[i]` and the seeded element row
    /// it dispatches to, a lending prelude row, the `value` box, a projection.
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

    /// Whether a value is a borrow: a name whose type owns heap and which
    /// the body does not own (RFC-0089 rule 2).
    fn borrows(&self, v: &Val) -> bool {
        match v {
            Val::Name(n) => self.body.names[*n as usize].borrow,
            Val::Lit => false,
        }
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
        out.push(St::Let(n, rhs));
        for t in std::mem::take(&mut self.after_of_rhs) {
            out.push(St::Drop(t, Site::None));
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
        let Some(rows) = self.placed.get(&(exit, site)) else {
            return Ok(());
        };
        for r in rows {
            match self.by_binding.get(&r.binding) {
                Some(n) => {
                    // The row's own set (a placer row, or round fifty-two's
                    // whole walk), else the binding's.
                    let holes = if r.full {
                        Vec::new()
                    } else if let Some(h) = &r.holes {
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
        for s in &blk.stmts {
            self.stmt(s, &mut body)?;
        }
        self.drops_at(Exit::Block, site, &mut body)?;
        self.scope.truncate(mark);
        out.push(St::Block { site, body });
        Ok(())
    }

    fn stmt(&mut self, s: &'a Stmt, out: &mut Vec<St>) -> Result<(), Gap> {
        let sid = s as *const Stmt as usize;
        match s {
            Stmt::Let {
                name, value, line, ..
            } => {
                let ty = self.ty_of(value)?;
                // A `consume` took a place the release walk cannot be told
                // to skip (a declared `release`, an enum path, a filled
                // hole), so the plan leaks the whole binding on purpose.
                // Not modelled: the kernel would have to know what the walk
                // can skip, which is the plan's question and RFC-0093's.
                if let Some(Fate::Leaked(Leak::Hole { .. })) = self.fate_of(name, *line) {
                    return gap_d(
                        "a binding whose hole the release walk cannot skip",
                        name,
                        *line,
                    );
                }
                if !matches!(value, Expr::Var { .. }) && is_place_read(value) {
                    let place = self.place(value, out)?;
                    let n = self.name(name, ty, false, *line);
                    out.push(St::Let(n, Rhs::Read(place)));
                    self.release_receiver(value, out, true);
                    self.scope.push((name.clone(), n));
                    self.keyed(n, sid);
                    return Ok(());
                }
                let rhs = self.rhs(value, out)?;
                let literal = matches!(
                    value,
                    Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_)
                );
                // A call whose result points into an argument — a lending
                // prelude row, a projection — binds a borrow whatever the
                // note says: the plan cannot type a projection it expands
                // at the site, and its note then says "unknown".
                let owned = !self.lends(value)
                    && self
                        .fate_owned(name, *line, &ty)
                        .unwrap_or_else(|| self.owns(&ty) && !literal);
                // Not owned is not the same as borrowed: static data (`let
                // s = ""`, a literal of literals), a value the plan leaks and
                // the source of a whole-value alias (`Leak::Aliased`: the
                // other name reclaims) are nobody's borrow. A lending call, a
                // borrowed value and a read of a place are (the plan's note
                // says which).
                let borrow = !owned
                    && (self.lends(value)
                        || matches!(&rhs, Rhs::Val(v) if self.borrows(v))
                        || matches!(
                            self.fate_of(name, *line),
                            Some(Fate::Leaked(Leak::Borrowed(_)))
                        ));
                let n = self.name(name, ty, owned, *line);
                self.body.names[n as usize].borrow = borrow && self.body.names[n as usize].heap;
                // `let t = s` on a `read` parameter: `t` is a second name for
                // it, and the checker says so in the refusal it gives at `t`.
                if let Rhs::Val(Val::Name(m)) = &rhs {
                    if self.body.names[n as usize].borrow {
                        self.body.names[n as usize].borrow_kind =
                            self.body.names[*m as usize].borrow_kind.clone();
                    }
                }
                self.bind(n, rhs, out);
                // The unnamed receiver of the field the binding took or read
                // (RFC-0114 R1′): released after the read where the plan says
                // this frame owns it, held — and seen by the kernel — where
                // it does not.
                if let Expr::Field { .. } = value {
                    self.release_receiver(value, out, false);
                }
                self.scope.push((name.clone(), n));
                self.keyed(n, sid);
            }
            Stmt::Assign { name, value, line } => {
                let v = self.val(value, out)?;
                let n = self.lookup(name);
                let ty = match n {
                    Some(n) => self.body.names[n as usize].ty.clone(),
                    None => self.ty_of(value)?,
                };
                // The emitters' rule for a store to a NAME, stated once: the
                // plan says whether the store releases the old value; a value
                // that MENTIONS the place may be handing the old buffer back
                // (`xs = xs.push(v)`) so the release stands down — unless the
                // plan proved every mention a read argument to a function
                // that cannot hand it back (`store_fresh_at`, exit-residue
                // round eighteen), or the value is a String concatenation,
                // which builds a fresh buffer whatever it reads (`s = s + x`).
                // Both compiled backends spell the last exception
                // `fresh_str`; it stands here so the one answer they read is
                // this one (RFC-0125 §3 M3, the emitter-reads-the-core
                // slice). Module state takes the same rule: it is a name to
                // both of them.
                let mentions = vyrn_frontend::movecheck::mentions_place(value, name);
                let fresh_str = matches!(
                    vyrn_frontend::types::resolve(
                        &ty,
                        &vyrn_frontend::types::decl_map(self.program),
                    ),
                    Type::Str
                ) && matches!(
                    value,
                    Expr::Binary {
                        op: vyrn_frontend::ast::BinOp::Add,
                        ..
                    }
                );
                let releases = self.own.plan.store_owned_at(sid)
                    && (fresh_str || !mentions || self.own.plan.store_fresh_at(sid));
                let Some(n) = n else {
                    out.push(St::Store {
                        place: Place::Global(name.clone()),
                        value: v,
                        old: if releases {
                            Old::Released
                        } else {
                            Old::Nothing
                        },
                        line: *line,
                        site: Site::Node(sid),
                        releases,
                    });
                    return Ok(());
                };
                let old = if !self.body.names[n as usize].owned {
                    Old::Nothing
                } else if releases {
                    Old::Released
                } else if mentions {
                    Old::Transferred
                } else {
                    Old::Unreleased
                };
                out.push(St::Store {
                    place: Place::Name(n),
                    value: v,
                    old,
                    line: *line,
                    site: Site::Node(sid),
                    releases,
                });
            }
            Stmt::SetField {
                name,
                field,
                value,
                line,
            } => {
                let v = self.val(value, out)?;
                let (base, bty) = self.named_place(name, *line)?;
                let fty = self.field_ty(&bty, field, *line)?;
                let old = self.old_for(&fty, sid);
                out.push(St::Store {
                    place: Place::Field(Box::new(base), field.clone()),
                    value: v,
                    old,
                    line: *line,
                    site: Site::Node(sid),
                    // A field store takes the plan's row as it stands: the
                    // value-alias guard is folded into the row itself
                    // (`fold_store_owned`), so there is nothing to add here.
                    releases: self.own.plan.store_owned_at(sid),
                });
            }
            Stmt::IndexSet {
                name,
                index,
                value,
                line,
            } => {
                let (base, bty) = self.named_place(name, *line)?;
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
                // checker walks it, so the plan's row stands on a statement
                // this pass never sees: no site, and a reader falls back to
                // the plan (RFC-0125 §3 M3, the emitter-reads-the-core
                // slice).
                let (ety, site) = match self.elem_ty(&bty, *line) {
                    Ok(t) => (t, Site::Node(sid)),
                    Err(_) => (self.ty_of(value)?, Site::None),
                };
                let old = self.old_for(&ety, sid);
                out.push(St::Store {
                    place,
                    value: v,
                    old,
                    line: *line,
                    site,
                    releases: self.own.plan.store_owned_at(sid),
                });
            }
            Stmt::Return { value, line } => {
                let v = match value {
                    Some(e) => Some(self.val(e, out)?),
                    None => None,
                };
                self.close_streams(out);
                self.drops_at(Exit::Return, sid, out)?;
                out.push(St::Return {
                    value: v,
                    site: sid,
                    is_try: false,
                    line: *line,
                });
            }
            Stmt::Break { .. } => {
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
                let sty = self.ty_of(scrutinee)?;
                let (sv, consuming) = self.scrutinee(scrutinee, sid, out)?;
                let mut t = Vec::new();
                let mark = self.scope.len();
                let binds = self.bind_pattern(pattern, &sty, consuming, *line, &mut t)?;
                self.block(then_block, &mut t)?;
                self.scope.truncate(mark);
                let mut e = Vec::new();
                if let Some(blk) = else_block {
                    self.block(blk, &mut e)?;
                }
                out.push(St::Switch {
                    on: sv,
                    arms: vec![
                        Arm {
                            frees: None,
                            binds,
                            body: t,
                            site: sid,
                            index: 0,
                        },
                        Arm {
                            frees: None,
                            binds: Vec::new(),
                            body: e,
                            site: sid,
                            index: 1,
                        },
                    ],
                    consuming,
                    line: *line,
                });
                self.drops_at(Exit::Scrutinee, sid, out)?;
            }
            Stmt::While { cond, body, .. } => {
                let mut l = Vec::new();
                let c = self.read_val(cond, &mut l)?;
                l.push(St::If {
                    cond: c,
                    then: Vec::new(),
                    els: vec![St::Break { site: 0 }],
                    site: 0,
                });
                self.block(body, &mut l)?;
                out.push(St::Loop(l));
            }
            Stmt::ForIn {
                var,
                iter,
                body,
                line,
                consuming,
            } => {
                let ity = self.ty_of(iter)?;
                let ety = match self.elem_ty(&ity, *line) {
                    Ok(t) => t,
                    // A user container: the element is what its `nth`
                    // projection yields (RFC-0091 M2), under this
                    // instantiation's type arguments.
                    Err(g) => self.projected_elem(&ity).ok_or(g)?,
                };
                // The container: a name the loop reads, or one it takes.
                let it = match iter {
                    Expr::Var { name, .. } if self.lookup(name).is_some() => {
                        let n = self.lookup(name).unwrap();
                        if *consuming {
                            let t = self.temp(ity.clone(), *line);
                            out.push(St::Let(t, Rhs::Val(Val::Name(n))));
                            self.keyed(t, sid);
                            t
                        } else {
                            n
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
                        let t = self.name("@borrow", ity.clone(), false, *line);
                        out.push(St::Let(t, Rhs::Read(place)));
                        t
                    }
                    _ => {
                        let v = self.val(iter, out)?;
                        let Val::Name(t) = v else {
                            return gap("a `for` over a literal", *line);
                        };
                        // The construct owns the temporary; the plan keys its
                        // release by the statement, and so does a row the
                        // placer adds for it (`for t in lex(src)` with a
                        // `return` inside the loop).
                        self.keyed(t, sid);
                        t
                    }
                };
                let decls = vyrn_frontend::types::decl_map(self.program);
                let streaming =
                    matches!(vyrn_frontend::types::resolve(&ity, &decls), Type::Stream(_));
                if streaming {
                    self.stream_loops.push(it);
                }
                let mut l = Vec::new();
                // Whose is each element? The plan's row for the loop says:
                // a `FreeArr` row frees the buffer only, because the body
                // took the elements out through the variable (round sixteen's
                // handover) — then each turn owns its element and must move or
                // release it. Any other row, or none, releases the container
                // deep, and the variable is an alias into it.
                let handed_over = self
                    .own
                    .droppable
                    .get(&self.func_name)
                    .and_then(|m| m.get(&sid))
                    .is_some_and(|k| matches!(k, DropKind::FreeArr));
                // The loop leaves when the container is walked: the same
                // `if .. else break` a `while` has at its top. Without it the
                // kernel sees a loop nothing leaves, and the path after the
                // `for` — the rest of its block, and its edge at every join
                // above — is dead to the judgment. The placer's rewrite of a
                // row's hole set then read a join's other edge alone, and
                // walked a field the dead edge had taken (`std/vyx`'s
                // `vyxMergeImports`, found by the cross-engine generator gate).
                l.push(St::If {
                    cond: Val::Lit,
                    then: Vec::new(),
                    els: vec![St::Break { site: 0 }],
                    site: 0,
                });
                let owned = handed_over && self.owns(&ety);
                let x = self.name(var, ety, owned, *line);
                // The variable has no `let` node; the plan keys it by its
                // spelling's buffer, which is one address per loop.
                self.keyed(x, vyrn_frontend::own::for_var_key(var));
                let head = vec![St::Let(
                    x,
                    Rhs::Read(Place::Elem(Box::new(Place::Name(it)), Val::Lit)),
                )];
                let mark = self.scope.len();
                self.scope.push((var.clone(), x));
                self.block_with(body, head, &mut l)?;
                self.scope.truncate(mark);
                out.push(St::Loop(l));
                if streaming {
                    self.stream_loops.pop();
                    // The loop pulled the stream to its end, or a `break`
                    // left early: either way the loop closes it here.
                    if self.body.names[it as usize].owned
                        && !self.placed.contains_key(&(Exit::Scrutinee, sid))
                    {
                        out.push(St::Drop(it, Site::None));
                    }
                } else if *consuming && !self.placed.contains_key(&(Exit::Scrutinee, sid)) {
                    // The loop took the container and the plan placed no row
                    // for it, so the loop gives it back here.
                    out.push(St::Drop(it, Site::None));
                }
                self.drops_at(Exit::Scrutinee, sid, out)?;
            }
            Stmt::Drop { name, line } => {
                let Some(n) = self.lookup(name) else {
                    return gap("a `drop` of module state", *line);
                };
                // A String bound inside a `region` is the arena's: both
                // compiling backends emit its release as nothing under
                // `region_depth`, and the plan notes it `Leak::Region`.
                let info = &self.body.names[n as usize];
                if !info.owned
                    && matches!(
                        self.fate_of(&info.source, info.line),
                        Some(Fate::Leaked(Leak::Region))
                    )
                {
                    return Ok(());
                }
                out.push(St::Drop(n, Site::None));
            }
            Stmt::Expr(e) => {
                let ty = self.ty_of(e).unwrap_or(Type::Unit);
                let rhs = self.rhs(e, out)?;
                if self.owns(&ty) {
                    let t = self.temp(ty, e.line());
                    self.bind(t, rhs, out);
                    if self.own.plan.discarded_result(sid) {
                        out.push(St::Drop(t, Site::Node(sid)));
                    }
                } else {
                    out.push(St::Do(rhs, e.line()));
                    for t in std::mem::take(&mut self.after_of_rhs) {
                        out.push(St::Drop(t, Site::None));
                    }
                }
            }
            // The arena owns what is allocated inside it: the plan notes such
            // a binding `Leak::Region` and it is not this frame's, so the
            // body is an ordinary block here and the closing brace is the
            // runtime's.
            Stmt::Region { body, .. } => self.block(body, out)?,
        }
        Ok(())
    }

    /// What a field or element store displaces. The plan decides whether the
    /// old value is released (RFC-0114 §26 steps 3-4: the place's ownedness is
    /// a plan row, keyed by the statement). Where it placed no release, this
    /// slice records `Nothing` rather than `Unreleased`: the kernel tracks
    /// whole names, and a sub-place the plan knows to be empty — a payload
    /// already taken out, an `Option` already `None` — is not a name it can
    /// see. Sub-place ownership is M3's judgment, not M2's.
    fn old_for(&self, ty: &Type, sid: usize) -> Old {
        if !self.owns(ty) {
            Old::Nothing
        } else if self.own.plan.store_owned_at(sid) {
            Old::Released
        } else {
            Old::Nothing
        }
    }

    /// The streams every enclosing `for` walks, closed on the way out of the
    /// function, innermost first.
    fn close_streams(&self, out: &mut Vec<St>) {
        for it in self.stream_loops.iter().rev() {
            if self.body.names[*it as usize].owned {
                out.push(St::Drop(*it, Site::None));
            }
        }
    }

    /// RFC-0114 Rule N: the drops one edge of a join owes.
    fn edge_drops(&mut self, join: usize, edge: u32, out: &mut Vec<St>) -> Result<(), Gap> {
        let Some(ers) = self.own.plan.edge_releases_at(join) else {
            return Ok(());
        };
        for (name, e) in ers {
            if *e != edge {
                continue;
            }
            // `d.line`: a sub-place the other edge took, released on this
            // one (RFC-0125 M3). It leaves as a take into a temporary that is
            // dropped at once, so the kernel sees the hole it leaves.
            let mut parts = name.split('.');
            let root = parts.next().unwrap_or_default();
            let Some(n) = self.lookup(root) else {
                return gap("an edge release of a name out of scope", 0);
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
                out.push(St::Drop(t, at));
            } else {
                out.push(St::Drop(n, at));
            }
        }
        Ok(())
    }

    /// A name as a place: a binding of this body, or module state with its
    /// declared type.
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
        let decls = vyrn_frontend::types::decl_map(self.program);
        matches!(vyrn_frontend::types::resolve(ty, &decls), Type::Map(..))
    }

    fn field_ty(&self, ty: &Type, field: &str, line: usize) -> Result<Type, Gap> {
        let decls = vyrn_frontend::types::decl_map(self.program);
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
        let decls = vyrn_frontend::types::decl_map(self.program);
        match vyrn_frontend::types::resolve(ty, &decls) {
            Type::Array(e) | Type::ArrayN(e, _) | Type::SmallArray(e, _) | Type::Stream(e) => {
                Ok(*e)
            }
            Type::Str => Ok(Type::IntN {
                bits: 8,
                signed: false,
            }),
            Type::Map(_, v) => Ok(*v),
            t => gap_d("an element of a non-container", &t.to_string(), line),
        }
    }

    /// The scrutinee of a `match`, `if let` or `?`: the value it switches on,
    /// and whether the construct consumed it.
    fn scrutinee(
        &mut self,
        e: &'a Expr,
        construct: usize,
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
                if !self.body.names[n as usize].owned {
                    return Ok((Val::Name(n), false));
                }
                if self.own.plan.match_consumes(construct) {
                    let t = self.temp(self.body.names[n as usize].ty.clone(), e.line());
                    out.push(St::Let(t, Rhs::Val(Val::Name(n))));
                    self.by_binding.insert(construct, t);
                    Ok((Val::Name(t), true))
                } else {
                    Ok((Val::Name(n), false))
                }
            }
            Expr::Consume { place, line } => match &**place {
                Expr::Var { name, .. } => {
                    let Some(n) = self.lookup(name) else {
                        // `for x in consume <module state>`: the same read,
                        // and the same refusal (RFC-0125 §3 M3, row 29).
                        let v = self.global_read(place, name, *line, out)?;
                        if let Val::Name(t) = v {
                            self.by_binding.insert(construct, t);
                        }
                        return Ok((v, false));
                    };
                    let t = self.temp(self.body.names[n as usize].ty.clone(), *line);
                    out.push(St::Let(t, Rhs::Val(Val::Name(n))));
                    self.by_binding.insert(construct, t);
                    Ok((Val::Name(t), self.taken_by(t, construct)))
                }
                _ => {
                    let Val::Name(t) = self.take_prefix(place, *line, out)? else {
                        return gap("a `consume` of a literal", *line);
                    };
                    self.by_binding.insert(construct, t);
                    Ok((Val::Name(t), self.taken_by(t, construct)))
                }
            },
            _ => {
                let v = self.val(e, out)?;
                match v {
                    Val::Name(t) => {
                        self.by_binding.insert(construct, t);
                        Ok((Val::Name(t), self.taken_by(t, construct)))
                    }
                    Val::Lit => Ok((Val::Lit, false)),
                }
            }
        }
    }

    /// Whether the construct took the temporary `t` it owns: the payloads
    /// moved into the arms' binders and the boxes were freed there, so the
    /// plan placed no release of the whole value after the construct. Where it
    /// did place one, the binders borrowed and the value is released whole.
    fn taken_by(&self, t: Name, construct: usize) -> bool {
        self.body.names[t as usize].owned
            && !self.placed.contains_key(&(Exit::Scrutinee, construct))
    }

    /// Bind a pattern's names. Owned binders when the match consumed its
    /// scrutinee; borrowed places otherwise.
    fn bind_pattern(
        &mut self,
        p: &Pattern,
        sty: &Type,
        consuming: bool,
        line: usize,
        out: &mut Vec<St>,
    ) -> Result<Vec<Name>, Gap> {
        let decls = vyrn_frontend::types::decl_map(self.program);
        let rt = vyrn_frontend::types::resolve(sty, &decls);
        let payloads: Vec<(String, Type)> = match p {
            Pattern::None | Pattern::Other => Vec::new(),
            Pattern::Some(n) | Pattern::Success(n) => match &rt {
                Type::Option(t) => vec![(n.clone(), (**t).clone())],
                Type::Result(t, _) => vec![(n.clone(), (**t).clone())],
                _ => return gap("a `Some` pattern on a non-option", line),
            },
            Pattern::Ok(n) => match &rt {
                Type::Result(t, _) => vec![(n.clone(), (**t).clone())],
                _ => return gap("an `Ok` pattern on a non-result", line),
            },
            Pattern::Err(n) | Pattern::Failure(n) => match &rt {
                Type::Result(_, e) => vec![(n.clone(), (**e).clone())],
                Type::Option(_) => Vec::new(),
                _ => return gap("an `Err` pattern on a non-result", line),
            },
            Pattern::Variant(v, names) => match &rt {
                Type::Enum(variants) => {
                    let Some(var) = variants.iter().find(|x| x.name == *v) else {
                        return gap("a variant the enum does not have", line);
                    };
                    if var.payload.len() != names.len() {
                        return gap("a variant pattern with the wrong arity", line);
                    }
                    names
                        .iter()
                        .cloned()
                        .zip(var.payload.iter().cloned())
                        .collect()
                }
                _ => return gap("a variant pattern on a non-enum", line),
            },
        };
        let mut binds = Vec::new();
        for (name, ty) in payloads {
            let owned = consuming && self.owns(&ty);
            let n = self.name(&name, ty, owned, line);
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
        let _ = out;
        Ok(binds)
    }

    /// An expression in a READ position: an operand, a condition, an index, a
    /// `read` argument. A place that owns heap is borrowed, not moved — the
    /// value the position sees is a name the kernel does not own. A String
    /// temporary (`@str`, `@concat`, a string `+`) is freed by the site that
    /// reads it (RFC-0096 M3), so the drop is queued for right after the
    /// binding that consumes it.
    fn read_val(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        let v = self.read_val_inner(e, out)?;
        // RFC-0114 M1's key, stated once for every read in an argument
        // position (RFC-0125 §3 M3, the emitter-reads-the-core slice). An
        // operator is a call to the plan (`a + b` is `@concat(a, b)`) and a
        // `lazy` field read is one too, so the key is taken here rather than
        // in `call`, which sees neither. Membership, not the query: the
        // query records a row as consumed, and §26's finish check is the
        // emitters' to discharge.
        if let Val::Name(t) = v {
            let node = e as *const Expr as usize;
            if self.own.plan.arg_drops.contains(&node) {
                self.body.names[t as usize].arg_drop = Some(node);
            }
        }
        Ok(v)
    }

    fn read_val_inner(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        let ty = self.ty_of(e).ok();
        let owns = ty.as_ref().is_some_and(|t| self.owns(t));
        match e {
            Expr::Field { .. } if owns => {
                let place = self.place(e, out)?;
                let t = self.name("@borrow", ty.unwrap(), false, e.line());
                out.push(St::Let(t, Rhs::Read(place)));
                self.release_receiver(e, out, true);
                Ok(Val::Name(t))
            }
            Expr::Call { name, args, .. } if owns && name == "@at" && args.len() == 2 => {
                let place = self.place(e, out)?;
                let t = self.name("@borrow", ty.unwrap(), false, e.line());
                out.push(St::Let(t, Rhs::Read(place)));
                self.release_receiver(e, out, true);
                Ok(Val::Name(t))
            }
            Expr::Var { .. } | Expr::Consume { .. } => self.val(e, out),
            Expr::Lambda { .. } => self.lambda(e, out),
            _ if owns => {
                let v = self.val(e, out)?;
                if let Val::Name(t) = v {
                    if self.body.names[t as usize].owned && !self.after.contains(&t) {
                        self.after.push(t);
                    }
                }
                Ok(v)
            }
            _ => self.val(e, out),
        }
    }

    /// RFC-0114 R1': the unnamed receiver of a field or element read, freed
    /// after the read when the plan says this frame owns it. Where the plan
    /// did not place the free, the receiver stays held and the kernel says so.
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
    fn release_receiver(&mut self, e: &Expr, out: &mut Vec<St>, borrowed: bool) {
        let Some((r, producer)) = self.pending_receiver.take() else {
            return;
        };
        let node = e as *const Expr as usize;
        let took = self.ty_of(e).is_ok_and(|t| self.owns(&t));
        if borrowed && took {
            if self.own.plan.arg_drop(producer) {
                if !self.after.contains(&r) {
                    self.body.names[r as usize].arg_drop = Some(producer);
                    self.after.push(r);
                }
            } else if self.drain > 0 {
                self.body.names[r as usize].producer = Some(producer);
            }
            return;
        }
        if !self.own.plan.receiver_free(node) {
            return;
        }
        // Both emitters run the row after a SCALAR field read only, and
        // after a heap field's take when the row carries the hole
        // (RFC-0125 M3): a row without one stands for nothing there,
        // and the receiver stays held for the kernel to see.
        let holes = self.own.plan.receiver_holes_at(node);
        if took && holes.is_empty() {
            return;
        }
        self.body.names[r as usize].holes = holes.iter().map(|h| format!(".{h}")).collect();
        out.push(St::Drop(r, Site::None));
    }

    /// An expression in a TAKE position: a `let`, a `return`, a store, a part
    /// of a literal, a `consume` argument. A name, or a literal.
    fn val(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        match e {
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {
                Ok(Val::Lit)
            }
            Expr::Var { name, line } => match self.lookup(name) {
                Some(n) => Ok(Val::Name(n)),
                // A function's name as a value (`sortWith(es, byCount)`), or
                // a type's as an argument (`fromJson(Bag, src)`): static, and
                // the checker types neither as an expression.
                // A nullary constructor (`None`, a fieldless variant) parses
                // as a bare name too, and is a literal: it owns nothing, and
                // it is not module state anything reads out of.
                None if self.program.functions.iter().any(|f| &f.name == name)
                    || self.program.contracts.iter().any(|c| &c.name == name)
                    || vyrn_frontend::types::decl_map(self.program).contains_key(name)
                    || name == "None"
                    || self.is_variant(name) =>
                {
                    Ok(Val::Lit)
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
                    let t = self.name("@borrow", ty, false, e.line());
                    out.push(St::Let(t, Rhs::Read(place)));
                    self.release_receiver(e, out, true);
                    return Ok(Val::Name(t));
                }
                let rhs = self.rhs(e, out)?;
                // An `if` or `match` expression whose arm yields a borrow
                // yields a borrow (`movecheck::names_a_place`).
                let borrows = matches!(&rhs, Rhs::Val(v) if self.borrows(v));
                let t = if self.lends(e) || borrows {
                    self.name("@borrow", ty, false, e.line())
                } else {
                    self.temp(ty, e.line())
                };
                self.bind(t, rhs, out);
                if let Expr::Field { .. } = e {
                    self.release_receiver(e, out, false);
                }
                Ok(Val::Name(t))
            }
        }
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
        out.push(St::Let(t, Rhs::Prim(caps.clone(), Some(ty))));
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
        let decls = vyrn_frontend::types::decl_map(self.program);
        let ptys: Vec<Type> = match self.ty_of(e).ok() {
            // A `lazy T` field's initializer is a nullary closure (RFC-0085).
            Some(t) if vyrn_frontend::types::deferred(&t).is_some() => Vec::new(),
            Some(t) => match vyrn_frontend::types::resolve(&t, &decls) {
                Type::Fn(ptys, _) => ptys,
                _ => return gap("a lambda the checker did not type as a function", *line),
            },
            // The checker did not type the literal: an argument of a generic
            // the instance monomorphized away. Its body is typed, so each
            // parameter has the type of its first use there.
            None => {
                let (mut vars, mut calls) = (Vec::new(), Vec::new());
                mentions_in_lambda(body, &mut vars, &mut calls);
                params
                    .iter()
                    .map(|p| {
                        vars.iter()
                            .find(|v| matches!(v, Expr::Var { name, .. } if name == p))
                            .and_then(|v| self.types.get(&(*v as *const Expr as usize)))
                            .cloned()
                            .unwrap_or(Type::Unit)
                    })
                    .collect()
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
            },
        );
        self.body.name = format!("{}@lambda:{line}", outer.name);
        let saved = (
            std::mem::take(&mut self.scope),
            std::mem::take(&mut self.by_binding),
            std::mem::take(&mut self.after),
            std::mem::take(&mut self.after_of_rhs),
            self.pending_receiver.take(),
            std::mem::replace(&mut self.drain, 0),
            std::mem::take(&mut self.stream_loops),
        );
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
            let m = self.name(p, pt, false, *line);
            self.scope.push((p.clone(), m));
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
        ) = saved;
        r?;
        self.body.lambdas.push(frame);
        Ok(())
    }

    /// The names of this body a lambda mentions, as a place or as a callee
    /// (`n -> f(n) + 1` captures the function value `f`). Over-approximate: a
    /// name the lambda shadows is counted, and a block-bodied lambda counts
    /// every name in scope (`mentions_place` answers true for one); either
    /// costs a read of a held name and nothing else.
    fn captures(&self, e: &Expr) -> Vec<Val> {
        let Expr::Lambda { params, body, .. } = e else {
            return Vec::new();
        };
        let (mut vars, mut calls) = (Vec::new(), Vec::new());
        mentions_in_lambda(body, &mut vars, &mut calls);
        let mut caps = Vec::new();
        for (name, n) in &self.scope {
            if params.contains(name) || caps.contains(&Val::Name(*n)) {
                continue;
            }
            if vyrn_frontend::movecheck::mentions_place(e, name) || calls.contains(&name.as_str()) {
                caps.push(Val::Name(*n));
            }
        }
        caps
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
            self.name("@borrow", ty, false, line)
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
        if vyrn_frontend::movecheck::place_path(e).is_none() {
            if let Some((_, path)) = vyrn_frontend::movecheck::element_path(e) {
                return refuse(
                    format!("`{path}` may not be taken — an element is not a place a take reaches"),
                    line,
                );
            }
            return refuse(
                "`consume` here has nothing to take — the value is already owned, so \
                 there is no place to leave a hole in"
                    .to_string(),
                line,
            );
        }
        self.take_place(e, out)
    }

    /// A move out of a sub-place: `consume x.f`, or the receiver a rebuilding
    /// builtin hands back (`s.dense.push(i)` is `s.dense = @push(s.dense, i)`).
    /// The value leaves into an owned name and the base keeps a hole.
    fn take_place(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        let ty = self.ty_of(e)?;
        let place = self.place(e, out)?;
        self.pending_receiver = None;
        let t = self.temp(ty, e.line());
        out.push(St::Let(t, Rhs::Take(place)));
        Ok(Val::Name(t))
    }

    /// An expression as the right-hand side of a `let`. The temporaries the
    /// expression's own reads queued for release are left in `self.after` for
    /// the binding that follows; the ones queued by an enclosing expression
    /// are kept aside meanwhile, so a nested read cannot drop what an outer
    /// expression is still about to read.
    fn rhs(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Rhs, Gap> {
        let outer = std::mem::take(&mut self.after);
        let r = self.rhs_inner(e, out);
        let mine = std::mem::replace(&mut self.after, outer);
        self.after_of_rhs = mine;
        r
    }

    fn rhs_inner(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Rhs, Gap> {
        match e {
            Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {
                Ok(Rhs::Val(Val::Lit))
            }
            Expr::Var { .. } | Expr::Consume { .. } => Ok(Rhs::Val(self.val(e, out)?)),
            Expr::Unary { expr, .. } => {
                Ok(Rhs::Prim(vec![self.read_val(expr, out)?], self.produced(e)))
            }
            Expr::Binary { lhs, rhs, .. } => {
                // An operator drains its operands' temporaries in both
                // compiled backends (`binary`, `gen_binary`).
                self.drain += 1;
                let a = self.read_val(lhs, out)?;
                let b = self.read_val(rhs, out)?;
                self.drain -= 1;
                Ok(Rhs::Prim(vec![a, b], self.produced(e)))
            }
            Expr::Field { expr, field, .. } => {
                let fty = self.ty_of(e)?;
                let place = self.place(expr, out)?;
                if let Some((r, _)) = self.pending_receiver {
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
            Expr::Call { name, args, line } if name == "panic" || name == "@panicAt" => {
                let r = self.call(name, args, *line, self.produced(e), out)?;
                out.push(St::Do(r, *line));
                out.push(St::Trap);
                Ok(Rhs::Val(Val::Lit))
            }
            Expr::Call { name, args, line } => self.call(name, args, *line, self.produced(e), out),
            Expr::TryConstruct { args, .. } | Expr::ArrayLit { elems: args, .. } => {
                let mut vs = Vec::new();
                for a in args {
                    vs.push(self.val(a, out)?);
                }
                Ok(Rhs::Make(vs))
            }
            Expr::StructLit { fields, .. } => {
                let mut vs = Vec::new();
                for (_, a) in fields {
                    vs.push(self.val(a, out)?);
                }
                Ok(Rhs::Make(vs))
            }
            Expr::MapLit { entries, .. } => {
                let mut vs = Vec::new();
                for (k, v) in entries {
                    vs.push(self.val(k, out)?);
                    vs.push(self.val(v, out)?);
                }
                Ok(Rhs::Make(vs))
            }
            Expr::Spawn { name, args, .. } => {
                let mut vs = Vec::new();
                for a in args {
                    vs.push((self.val(a, out)?, Capability::Consume));
                }
                Ok(Rhs::Call {
                    callee: name.clone(),
                    args: vs,
                    spawn: true,
                    write_back: false,
                    ret: self.produced(e),
                })
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
                let mut t = Vec::new();
                let tv = self.val(then_branch, &mut t)?;
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
                            self.body.names[res as usize].owned = false;
                            self.body.names[res as usize].borrow = true;
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
            } => {
                let ty = self.ty_of(e)?;
                let sty = self.ty_of(scrutinee)?;
                let mid = e as *const Expr as usize;
                let res = self.temp(ty, *line);
                let (sv, consuming) = self.scrutinee(scrutinee, mid, out)?;
                let mut core_arms = Vec::new();
                for (i, arm) in arms.iter().enumerate() {
                    let mut body = Vec::new();
                    let mark = self.scope.len();
                    let binds =
                        self.bind_pattern(&arm.pattern, &sty, consuming, *line, &mut body)?;
                    match &arm.body {
                        ArmBody::Expr(ae) => {
                            let v = self.val(ae, &mut body)?;
                            // An arm that yields a borrow makes the result
                            // one (`movecheck::names_a_place`).
                            if self.borrows(&v) {
                                self.body.names[res as usize].owned = false;
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
                    let mut frees: Vec<Name> = Vec::new();
                    if let Some(rows) = self.own.plan.arm_payload_free(mid, i as u32) {
                        for b in &binds {
                            let src = &self.body.names[*b as usize].source;
                            if let Some((_, _, holes)) = rows.iter().find(|(n, _, _)| n == src) {
                                self.body.names[*b as usize].holes =
                                    holes.iter().map(|h| format!(".{h}")).collect();
                                body.push(St::Drop(*b, Site::None));
                                frees.push(*b);
                            }
                        }
                    }
                    self.edge_drops(mid, i as u32, &mut body)?;
                    self.scope.truncate(mark);
                    core_arms.push(Arm {
                        binds,
                        frees: Some(frees),
                        body,
                        site: mid,
                        index: i as u32,
                    });
                }
                out.push(St::Switch {
                    on: sv,
                    arms: core_arms,
                    consuming,
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
                let (sv, consuming) = self.scrutinee(expr, tid, out)?;
                let decls = vyrn_frontend::types::decl_map(self.program);
                if let Type::Enum(_) = vyrn_frontend::types::resolve(&ity, &decls) {
                    return self.fallible_try(ity, sv, res, tid, out);
                }
                // Failure: the exit's drops, then the propagated value leaves.
                let mut fail = Vec::new();
                let mark = self.scope.len();
                let fb = self.bind_pattern(
                    &Pattern::Failure("@err".into()),
                    &ity,
                    consuming,
                    *line,
                    &mut fail,
                )?;
                self.close_streams(&mut fail);
                self.drops_at(Exit::Try, tid, &mut fail)?;
                fail.push(St::Return {
                    value: fb.first().map(|n| Val::Name(*n)),
                    site: tid,
                    is_try: true,
                    line: *line,
                });
                self.scope.truncate(mark);
                let mut ok = Vec::new();
                let mark = self.scope.len();
                let ob = self.bind_pattern(
                    &Pattern::Success("@ok".into()),
                    &ity,
                    consuming,
                    *line,
                    &mut ok,
                )?;
                ok.push(St::Store {
                    place: Place::Name(res),
                    value: ob.first().map(|n| Val::Name(*n)).unwrap_or(Val::Lit),
                    old: Old::Nothing,
                    line: *line,
                    site: Site::None,
                    releases: false,
                });
                self.scope.truncate(mark);
                out.push(St::Switch {
                    on: sv,
                    arms: vec![
                        Arm {
                            frees: None,
                            binds: fb,
                            body: fail,
                            site: tid,
                            index: 0,
                        },
                        Arm {
                            frees: None,
                            binds: ob,
                            body: ok,
                            site: tid,
                            index: 1,
                        },
                    ],
                    consuming,
                    line: *line,
                });
                Ok(Rhs::Val(Val::Name(res)))
            }
            Expr::Lambda { .. } => {
                let caps = self.captures(e);
                self.lambda_frame(e, &caps)?;
                Ok(Rhs::Prim(caps, self.produced(e)))
            }
        }
    }

    /// `?` on a declared `Fallible` type (RFC-0080 M3): the failing path
    /// returns the whole value, whichever failing variant it is; the succeeding
    /// path hands it to the impl's `success`, which owns it. The switch reads
    /// the value and each arm takes it, so neither arm leaves it held.
    fn fallible_try(
        &mut self,
        ity: Type,
        sv: Val,
        res: Name,
        tid: usize,
        out: &mut Vec<St>,
    ) -> Result<Rhs, Gap> {
        let line = self.body.names[res as usize].line;
        let Some(key) = vyrn_frontend::types::type_key(&ity) else {
            return gap("a `?` on a type with no impl key", line);
        };
        let success =
            vyrn_frontend::types::impl_method_name(vyrn_frontend::types::FALLIBLE, &key, "success");
        let mut fail = Vec::new();
        self.close_streams(&mut fail);
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
                args: vec![(sv.clone(), Capability::Consume)],
                spawn: false,
                write_back: false,
                // `success` answers the unwrapped value, which is what the
                // result name of the `?` holds.
                ret: Some(self.body.names[res as usize].ty.clone()),
            },
        ));
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
                    frees: None,
                    binds: Vec::new(),
                    body: fail,
                    site: tid,
                    index: 0,
                },
                Arm {
                    frees: None,
                    binds: Vec::new(),
                    body: ok,
                    site: tid,
                    index: 1,
                },
            ],
            consuming: false,
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
                let i = self.read_val(&args[1], out)?;
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
                        if self.body.names[t as usize].owned {
                            self.pending_receiver = Some((t, e as *const Expr as usize));
                        }
                        Ok(Place::Name(t))
                    }
                    // A literal receiver — `"abc".byteLength`, which the
                    // corpus writes only inside a `test` body. The place is a
                    // temporary the site owns, named here so the chain above
                    // has a base (RFC-0125 §3 M6, seventh slice).
                    Val::Lit => {
                        let ty = self.ty_of(e)?;
                        let t = self.temp(ty, e.line());
                        out.push(St::Let(t, Rhs::Val(Val::Lit)));
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
        let decls = vyrn_frontend::types::decl_map(self.program);
        let scalar = matches!(
            name,
            "Int64"
                | "Int32"
                | "Int16"
                | "Int8"
                | "UInt64"
                | "UInt32"
                | "UInt16"
                | "UInt8"
                | "Float64"
                | "Float32"
                | "F32x4"
                | "F64x2"
                | "I32x4"
                | "Mask32x4"
                | "Mask64x2"
                | "logger"
        );
        let method = self
            .program
            .impls
            .iter()
            .flat_map(|i| i.methods.iter())
            .find(|m| m.name == name);
        // A seeded row whose result is its receiver's own type hands the
        // buffer back through the result, so the receiver is taken by the
        // call (`movecheck::sinks`).
        let rebuilds = prelude::signature(name).is_some_and(|sig| {
            sig.params.first().is_some_and(|p| p.ty == sig.ret)
                && matches!(
                    sig.ret,
                    Type::Array(_) | Type::SmallArray(..) | Type::Map(..)
                )
        });
        let caps: Vec<Capability> =
            if let Some(f) = self.program.functions.iter().find(|f| f.name == name) {
                f.params.iter().map(|p| p.capability).collect()
            } else if prelude::signature(name).is_some() {
                let mut caps: Vec<Capability> = (0..args.len())
                    .map(|i| prelude::capability(name, i).unwrap_or(Capability::Read))
                    .collect();
                if rebuilds && !caps.is_empty() {
                    caps[0] = Capability::Consume;
                }
                caps
            } else if let Some(m) = method {
                m.params.iter().map(|p| p.capability).collect()
            } else if let Some(p) = self.projection(name) {
                p.params.iter().map(|p| p.capability).collect()
            } else if matches!(name, "Some" | "Ok" | "Err") || self.is_variant(name) {
                vec![Capability::Consume; args.len()]
            } else if vyrn_frontend::checker::RESERVED.contains(&name)
                || vyrn_frontend::ast::is_log_level(name)
                || vyrn_frontend::ast::is_surface_builtin(name)
            {
                // A reserved name with no prelude row (`fromJson`, `value`,
                // a generation-time surface builtin, a log level): its
                // capabilities are the prelude's answer where it has one, and
                // `read` elsewhere. The four surface builtins are one list
                // (`ast::SURFACE_BUILTINS`); naming two of them here left
                // `std/vyx`'s `vyxRegion` with no core (RFC-0125 §3 M6,
                // finding 12).
                (0..args.len())
                    .map(|i| prelude::capability(name, i).unwrap_or(Capability::Read))
                    .collect()
            } else if scalar {
                vec![Capability::Read; args.len()]
            } else if decls.contains_key(name) {
                vec![Capability::Consume; args.len()]
            } else if name.starts_with('@') {
                vec![Capability::Read; args.len()]
            } else if self.lookup(name).is_some() {
                // A call through a function value: the value's parameters are
                // `read` (RFC-0023) — a lambda captures by read and takes by read.
                vec![Capability::Read; args.len()]
            } else if matches!(name, "print") {
                vec![Capability::Read; args.len()]
            } else {
                return gap_d("a call this slice cannot attribute", name, line);
            };
        if caps.len() < args.len() {
            return gap("a call with more arguments than parameters", line);
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
        let drains = !self.lends_name(name);
        if drains {
            self.drain += 1;
        }
        for (k, (a, cap)) in args.iter().zip(caps.iter()).enumerate() {
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
                self.read_val(a, out)?
            };
            if let Val::Name(t) = v {
                let is_temp = !matches!(a, Expr::Var { .. } | Expr::Consume { .. });
                if is_temp
                    && *cap != Capability::Consume
                    && self.body.names[t as usize].owned
                    && self.own.plan.arg_drop(a as *const Expr as usize)
                {
                    // The key stands whether or not `read_val` already queued
                    // the temporary: the drop is the same drop, and a reader
                    // of the fold looks the row up by this node (RFC-0125 §3
                    // M3, the emitter-reads-the-core slice).
                    self.body.names[t as usize].arg_drop = Some(a as *const Expr as usize);
                    if !self.after.contains(&t) {
                        temps_to_drop.push(t);
                    }
                }
            }
            vs.push((v, *cap));
        }
        if drains {
            self.drain -= 1;
        }
        self.after.extend(temps_to_drop);
        Ok(Rhs::Call {
            callee: name.to_string(),
            args: vs,
            spawn: false,
            write_back,
            ret,
        })
    }

    fn is_variant(&self, name: &str) -> bool {
        let decls = vyrn_frontend::types::decl_map(self.program);
        decls
            .values()
            .any(|d| matches!(&d.base, Type::Enum(vs) if vs.iter().any(|v| v.name == name)))
    }
}

/// Every `Var` node and every callee name in a lambda's body, nested
/// lambdas included: what the frame captures, and where an untyped
/// parameter's type can be read.
fn mentions_in_lambda<'e>(
    body: &'e LambdaBody,
    vars: &mut Vec<&'e Expr>,
    calls: &mut Vec<&'e str>,
) {
    match body {
        LambdaBody::Expr(e) => mentions_in_expr(e, vars, calls),
        LambdaBody::Block(b) => mentions_in_block(b, vars, calls),
    }
}

fn mentions_in_block<'e>(b: &'e Block, vars: &mut Vec<&'e Expr>, calls: &mut Vec<&'e str>) {
    for s in &b.stmts {
        match s {
            Stmt::Let { value, .. } | Stmt::Assign { value, .. } | Stmt::SetField { value, .. } => {
                mentions_in_expr(value, vars, calls)
            }
            Stmt::IndexSet { index, value, .. } => {
                mentions_in_expr(index, vars, calls);
                mentions_in_expr(value, vars, calls);
            }
            Stmt::Return { value, .. } => {
                if let Some(v) = value {
                    mentions_in_expr(v, vars, calls);
                }
            }
            Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Drop { .. } => {}
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                mentions_in_expr(cond, vars, calls);
                mentions_in_block(then_block, vars, calls);
                if let Some(e) = else_block {
                    mentions_in_block(e, vars, calls);
                }
            }
            Stmt::IfLet {
                scrutinee,
                then_block,
                else_block,
                ..
            } => {
                mentions_in_expr(scrutinee, vars, calls);
                mentions_in_block(then_block, vars, calls);
                if let Some(e) = else_block {
                    mentions_in_block(e, vars, calls);
                }
            }
            Stmt::While { cond, body, .. } => {
                mentions_in_expr(cond, vars, calls);
                mentions_in_block(body, vars, calls);
            }
            Stmt::ForIn { iter, body, .. } => {
                mentions_in_expr(iter, vars, calls);
                mentions_in_block(body, vars, calls);
            }
            Stmt::Expr(e) => mentions_in_expr(e, vars, calls),
            Stmt::Region { body, .. } => mentions_in_block(body, vars, calls),
        }
    }
}

fn mentions_in_expr<'e>(e: &'e Expr, vars: &mut Vec<&'e Expr>, calls: &mut Vec<&'e str>) {
    match e {
        Expr::Int(_) | Expr::Byte(_) | Expr::Float(_) | Expr::Bool(_) | Expr::Str(_) => {}
        Expr::Var { .. } => vars.push(e),
        Expr::Unary { expr, .. }
        | Expr::Field { expr, .. }
        | Expr::Try { expr, .. }
        | Expr::Consume { place: expr, .. } => mentions_in_expr(expr, vars, calls),
        Expr::Binary { lhs, rhs, .. } => {
            mentions_in_expr(lhs, vars, calls);
            mentions_in_expr(rhs, vars, calls);
        }
        Expr::Call { name, args, .. } | Expr::Spawn { name, args, .. } => {
            calls.push(name.as_str());
            for a in args {
                mentions_in_expr(a, vars, calls);
            }
        }
        Expr::TryConstruct { args, .. } | Expr::ArrayLit { elems: args, .. } => {
            for a in args {
                mentions_in_expr(a, vars, calls);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, a) in fields {
                mentions_in_expr(a, vars, calls);
            }
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                mentions_in_expr(k, vars, calls);
                mentions_in_expr(v, vars, calls);
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            mentions_in_expr(cond, vars, calls);
            mentions_in_expr(then_branch, vars, calls);
            if let Some(b) = else_branch {
                mentions_in_expr(b, vars, calls);
            }
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            mentions_in_expr(scrutinee, vars, calls);
            for arm in arms {
                match &arm.body {
                    ArmBody::Expr(a) => mentions_in_expr(a, vars, calls),
                    ArmBody::Block(b) => mentions_in_block(b, vars, calls),
                }
            }
        }
        Expr::Lambda { body, .. } => mentions_in_lambda(body, vars, calls),
    }
}

thread_local! {
    static STRICT_REFUSALS: std::cell::RefCell<Vec<crate::kernel::Refusal>> =
        const { std::cell::RefCell::new(Vec::new()) };
    static FACTS: std::cell::RefCell<Option<Facts>> = const { std::cell::RefCell::new(None) };
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
/// `compiler/vyrn-cli/tests/coretables.rs` proves the two sources agree at
/// every site in the corpus. `VYRN_PLAN_ROWS=1` makes an emitter read the
/// plan again, which is the bisect for a difference this table would hide.
#[derive(Default, Clone, Debug)]
pub struct Facts {
    /// RFC-0114 R1′: the `Expr::Field` node of an unnamed receiver the core
    /// releases after the read, and the holes the release walks around.
    pub receivers: std::collections::HashMap<usize, Vec<String>>,
    /// Round forty's table: `(match, arm) -> [(binder, holes)]`, the payload
    /// binders the arm's own body releases at its end.
    pub arms: std::collections::HashMap<(usize, u32), Vec<(String, Vec<String>)>>,
    /// RFC-0114 M2 and exit-residue round eighteen: the store statements
    /// whose old value the store releases — a `St::Store`'s `releases` at a
    /// [`Site::Node`]. The plan's `store_owned` and
    /// `store_fresh` are two halves of one answer, and this is the answer:
    /// the core folds the mention guard and the `fresh_str` exception in
    /// where both compiled backends spell them. A site absent from the map
    /// is one this pass states no answer for, and a reader falls back to the
    /// plan there.
    pub stores: std::collections::HashMap<usize, bool>,
    /// Round twenty-eight: the statement-position calls whose owned result
    /// nothing binds and the core releases — a `St::Drop` at the
    /// statement's [`Site::Node`].
    pub discarded: std::collections::HashSet<usize>,
    /// RFC-0114 M1: the call-argument nodes whose temporary the caller
    /// releases after the call — [`NameInfo::arg_drop`], which the core sets
    /// wherever it lowers such an argument.
    pub arg_drops: std::collections::HashSet<usize>,
    /// RFC-0114 Rule N: per join node, the `(name, edge)` releases one edge
    /// owes because another edge took the name — a `St::Drop` at a
    /// [`Site::Edge`].
    pub edges: std::collections::HashMap<usize, Vec<(String, u32)>>,
    /// Round twenty-seven's question, answered by the rule rather than by
    /// the plan's table: per `match`, `if let` or `?` node, whether the
    /// construct TOOK its scrutinee, so the boxes its binders came out of
    /// are its own to give back ([`St::Switch`]'s `consuming`). A site
    /// absent from the map is one this pass states no answer for.
    pub consuming: std::collections::HashMap<usize, bool>,
}

/// The core's answers for the program last analysed on this thread. `None`
/// when the placer is not installed (`VYRN_NO_PLACER=1`), in which case an
/// emitter reads the plan as it always did.
pub fn facts() -> Option<Facts> {
    FACTS.with(|f| f.borrow().clone())
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
fn fold_facts(body: &Body, stmts: &[St], out: &mut Facts) {
    for s in stmts {
        match s {
            St::If { then, els, .. } => {
                fold_facts(body, then, out);
                fold_facts(body, els, out);
            }
            St::Loop(b) | St::Block { body: b, .. } => fold_facts(body, b, out),
            St::Store {
                releases,
                site: Site::Node(at),
                ..
            } => {
                out.stores.insert(*at, *releases);
            }
            St::Drop(n, at) => match at {
                Site::Node(at) => {
                    out.discarded.insert(*at);
                }
                Site::Edge(join, edge) => {
                    let name = body.names[*n as usize].source.clone();
                    let rows = out.edges.entry(*join).or_default();
                    // One row per name and edge: a generic instantiated twice
                    // folds the same join twice when the two share a node.
                    if !rows.contains(&(name.clone(), *edge)) {
                        rows.push((name, *edge));
                    }
                }
                Site::None => {}
            },
            St::Switch {
                arms,
                consuming: took,
                ..
            } => {
                if let Some(a) = arms.first() {
                    out.consuming.insert(a.site, *took);
                }
                for a in arms {
                    if let Some(frees) = &a.frees {
                        let rows: Vec<(String, Vec<String>)> = frees
                            .iter()
                            .map(|b| {
                                let info = &body.names[*b as usize];
                                (info.source.clone(), plan_holes(&info.holes))
                            })
                            .collect();
                        // An entry even when it is empty: "this arm states
                        // its releases and owes none" is not the same answer
                        // as "this pass did not state them".
                        out.arms.entry((a.site, a.index)).or_default().extend(rows);
                    }
                    fold_facts(body, &a.body, out);
                }
            }
            _ => {}
        }
    }
}

/// Every frame's answers, added to the table.
fn fold_frame(body: &Body, out: &mut Facts) {
    fold_facts(body, &body.stmts, out);
    let mut released = std::collections::HashSet::new();
    collect_drops(&body.stmts, &mut released);
    for (i, info) in body.names.iter().enumerate() {
        if !released.contains(&(i as Name)) {
            continue;
        }
        if let Some(node) = info.receiver {
            out.receivers.insert(node, plan_holes(&info.holes));
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
            St::Drop(n, _) => {
                out.insert(*n);
            }
            St::If { then, els, .. } => {
                collect_drops(then, out);
                collect_drops(els, out);
            }
            St::Loop(b) | St::Block { body: b, .. } => collect_drops(b, out),
            St::Switch { arms, .. } => {
                for a in arms {
                    collect_drops(&a.body, out);
                }
            }
            _ => {}
        }
    }
}

/// Whether `VYRN_KERNEL_STRICT=1` is set: a hard refusal by the kernel fails
/// `vyrn check` and `vyrn build`.
pub fn strict() -> bool {
    std::env::var("VYRN_KERNEL_STRICT").is_ok_and(|v| v == "1")
}

/// The hard refusals the placer met since the last call, on this thread: a
/// double free, a use after release, a join whose edges disagree — what no
/// placement repairs. Drained by the CLI in strict mode after an analysis.
pub fn take_strict_refusals() -> Vec<crate::kernel::Refusal> {
    STRICT_REFUSALS.with(|v| std::mem::take(&mut *v.borrow_mut()))
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
    let lowered = crate::lower_with(program, own);
    // `VYRN_KERNEL_TRACE=1` prints every release the placer found owed, and
    // whether it could place it.
    let trace = std::env::var("VYRN_KERNEL_TRACE").is_ok();
    let mut added: Vec<(String, Release, DropKind)> = Vec::new();
    for inst in &lowered.instances {
        let top = match build(program, inst, own) {
            Ok(b) => b,
            Err(g) => {
                // A rule the core states, rather than a construct it cannot
                // lower: reported like the kernel's own refusals (RFC-0125
                // §3 M3, the checker's deletion path).
                if let Some(message) = g.rule {
                    STRICT_REFUSALS.with(|v| {
                        v.borrow_mut().push(crate::kernel::Refusal {
                            message,
                            line: g.line,
                            file: inst.func.module.clone(),
                            body: inst.func.name.clone(),
                        })
                    });
                    continue;
                }
                if trace {
                    eprintln!(
                        "placer: {} not lowered: {} {}",
                        inst.spelling(),
                        g.what,
                        g.detail
                    );
                }
                continue;
            }
        };
        // `VYRN_KERNEL_TRACE=<fn>` prints that body's core, lambdas included.
        if std::env::var("VYRN_KERNEL_TRACE").is_ok_and(|v| v != "1" && top.name.contains(&v)) {
            eprintln!("{}", top.render());
        }
        // The body and every lambda frame under it: a lambda's rows are keyed
        // by its own nodes under the enclosing function's name, so a row
        // placed here lands where the emitters read (RFC-0125 M3, third slice).
        for body in top.frames() {
            let missing = match crate::kernel::placement(body) {
                Ok(m) => m,
                Err(r) => {
                    if trace {
                        eprintln!("placer: refused: {}: {}", r.body, r.message);
                    }
                    // A refusal no placement repairs: a double free, a use after
                    // release, a join whose edges disagree. Kept for the CLI's
                    // strict mode.
                    STRICT_REFUSALS.with(|v| v.borrow_mut().push(r));
                    continue;
                }
            };
            for m in missing {
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
                    if own.plan.arg_drops.insert(producer) {
                        own.plan.owners.insert(producer, inst.func.name.clone());
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
                        let rows = own.plan.edge_releases.entry(m.site).or_default();
                        if !rows.iter().any(|(n, e)| *n == info.source && *e == edge) {
                            rows.push((info.source.clone(), edge));
                            own.plan.owners.insert(m.site, inst.func.name.clone());
                        }
                        continue;
                    }
                    // The same table, one level down: the sub-place one edge took,
                    // released on the edge that did not. Spelled `d.line`, which
                    // every reader of the table resolves as a place.
                    MissingKind::EdgePlace { edge, path } => {
                        let name = format!("{}{}", info.source, path);
                        let rows = own.plan.edge_releases.entry(m.site).or_default();
                        if !rows.iter().any(|(n, e)| *n == name && *e == edge) {
                            rows.push((name, edge));
                            own.plan.owners.insert(m.site, inst.func.name.clone());
                        }
                        continue;
                    }
                    // Round forty's table: the arm's unmoved payload binders, one
                    // entry per binder so an emitter frees exactly those, each
                    // with the holes its arm left in it.
                    MissingKind::ArmBinder { arm } => {
                        let rows = own.plan.arm_frees.entry((m.site, arm)).or_default();
                        if !rows.iter().any(|(n, _, _)| *n == info.source) {
                            rows.push((info.source.clone(), kind.clone(), holes));
                        }
                        own.plan.owners.insert(m.site, inst.func.name.clone());
                        continue;
                    }
                    MissingKind::Exit => {}
                }
                // The unnamed receiver of a field read: R1′'s table, with the
                // field the read took as its hole. Freed right after the read,
                // whichever exit found it held.
                if let Some(node) = info.receiver {
                    own.plan.receiver_frees.insert(node);
                    if !holes.is_empty() {
                        own.plan.receiver_holes.insert(node, holes);
                    }
                    own.plan.owners.insert(node, inst.func.name.clone());
                    continue;
                }
                let Some(binding) = info.binding else {
                    continue;
                };
                // A row the plan already placed here whose hole set is not the
                // kernel's at this exit (the analysis's set is per binding, the
                // kernel's is per path): the row keeps its key and takes the
                // kernel's set.
                if let Some(r) = own.releases.get_mut(&inst.func.name).and_then(|rows| {
                    rows.iter_mut()
                        .find(|r| r.exit == m.exit && r.site == m.site && r.binding == binding)
                }) {
                    if trace {
                        eprintln!(
                            "placer: rewrite {} `{}` {:?} -> {:?}",
                            inst.func.name, info.source, m.exit, holes
                        );
                    }
                    r.full = false;
                    r.holes = Some(holes);
                    continue;
                }
                let dup = added.iter().any(|(f, r, _)| {
                    *f == inst.func.name
                        && r.exit == m.exit
                        && r.site == m.site
                        && r.binding == binding
                });
                if dup {
                    continue;
                }
                added.push((
                    inst.func.name.clone(),
                    Release {
                        site: m.site,
                        binding,
                        name: info.source.clone(),
                        kind: kind.clone(),
                        exit: m.exit,
                        line: info.line as u32,
                        full: false,
                        holes: if holes.is_empty() { None } else { Some(holes) },
                    },
                    kind,
                ));
            }
        }
    }
    for (f, row, kind) in added {
        own.droppable
            .entry(f.clone())
            .or_default()
            .entry(row.binding)
            .or_insert(kind);
        own.releases.entry(f).or_default().push(row);
    }
    // A SECOND build, after every row the placer added: the emitters read
    // the core's answers, and the core built above read the plan as it was
    // before this pass filled it (RFC-0125 §3 M3, the deletion-preparation
    // slice). The lowering is reused, so this costs the naming pass alone.
    // `VYRN_PLAN_ROWS=1` puts every emitter back on the plan, and then the
    // second build has no reader and is not run.
    if std::env::var("VYRN_PLAN_ROWS").is_ok() {
        return;
    }
    let mut facts = Facts::default();
    if let Ok(top) = build_module_state(program, own, &lowered.globals) {
        for body in top.frames() {
            fold_frame(body, &mut facts);
        }
    }
    for inst in &lowered.instances {
        if let Ok(top) = build(program, inst, own) {
            for body in top.frames() {
                fold_frame(body, &mut facts);
            }
        }
    }
    FACTS.with(|f| *f.borrow_mut() = Some(facts));
}
