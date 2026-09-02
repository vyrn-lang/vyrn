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
    ArmBody, Block, Capability, Expr, Function, Pattern, Program, Stmt, Type,
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
    /// The holes the plan's release walk skips for this binding (RFC-0093
    /// M2's table), spelled as the kernel spells them (`.f.g`). A `Drop` of
    /// the name walks around exactly these; a placed row may carry its own.
    pub holes: Vec<String>,
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
    },
    /// Arithmetic, comparison, interpolation, conversion: reads its operands.
    Prim(Vec<Val>),
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
    },
    Drop(Name),
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
    },
    Switch {
        on: Val,
        arms: Vec<Arm>,
        /// The construct took the value: its payloads moved into the arms'
        /// binders, so nothing releases the scrutinee itself afterwards.
        consuming: bool,
    },
    Do(Rhs),
    /// A refusal or a `panic`: the path ends here and owes nothing.
    Trap,
}

#[derive(Debug, Clone)]
pub struct Arm {
    /// The payload binders. Owned when the match consumed its scrutinee.
    pub binds: Vec<Name>,
    pub body: Vec<St>,
    /// The `match` (or `if let`, or `?`) this arm belongs to, and which arm
    /// it is — the plan's key for an arm payload free and an edge release.
    pub site: usize,
    pub index: u32,
}

#[derive(Debug, Clone)]
pub struct Body {
    pub name: String,
    pub names: Vec<NameInfo>,
    pub params: Vec<Name>,
    pub stmts: Vec<St>,
}

impl Body {
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
            Rhs::Call { callee, args } => format!(
                "{callee}({})",
                args.iter()
                    .map(|(v, c)| format!("{:?} {}", c, self.val(v)).to_lowercase())
                    .collect::<Vec<_>>()
                    .join(", ")
            ),
            Rhs::Prim(vs) => format!(
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
                St::Store { place, value, old } => out.push_str(&format!(
                    "{pad}{} = {}  ({:?})\n",
                    self.place(place),
                    self.val(value),
                    old
                )),
                St::Drop(n) => out.push_str(&format!("{pad}drop {}\n", self.spell(*n))),
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
                St::Do(r) => out.push_str(&format!("{pad}do {}\n", self.rhs(r))),
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
    })
}

fn gap_d<T>(what: &'static str, detail: &str, line: usize) -> Result<T, Gap> {
    Err(Gap {
        what,
        detail: detail.to_string(),
        line,
    })
}

/// Build the core of one instance.
pub fn build(program: &Program, inst: &Instance<'_>, own: &Ownership) -> Result<Body, Gap> {
    let mut types: HashMap<usize, Type> = HashMap::new();
    for r in &inst.rows {
        if let Node::Expr(_) = r.node {
            if let Some(t) = r.ty.as_ref().or(r.has.as_ref()) {
                types.insert(r.node.id(), t.clone());
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
        placed,
        body: Body {
            name: inst.spelling(),
            names: Vec::new(),
            params: Vec::new(),
            stmts: Vec::new(),
        },
        scope: Vec::new(),
        by_binding: HashMap::new(),
        temps: 0,
        func_name: inst.func.name.clone(),
        pending_receiver: None,
        after: Vec::new(),
        after_of_rhs: Vec::new(),
        stream_loops: Vec::new(),
    };
    let f: &Function = inst.func;
    // A declared release (`impl Owned for T { fn release(consume self) }`) IS
    // the release of `self`: its body frees the parts, and nothing releases
    // `self` again — so `self` is not a name the kernel owns there.
    let is_release = f.name.starts_with("Owned__") && f.name.ends_with("__release");
    for p in &f.params {
        let owned = p.capability == Capability::Consume && b.owns(&p.ty) && !is_release;
        let n = b.name(&p.name, p.ty.clone(), owned, f.line);
        b.scope.push((p.name.clone(), n));
        b.keyed(n, p as *const _ as usize);
        b.body.params.push(n);
    }
    let mut out = Vec::new();
    b.block(&f.body, &mut out)?;
    b.body.stmts = out;
    Ok(b.body)
}

struct Builder<'a> {
    program: &'a Program,
    own: &'a Ownership,
    proto: &'a Owned,
    types: HashMap<usize, Type>,
    placed: HashMap<(Exit, usize), Vec<&'a Release>>,
    body: Body,
    scope: Vec<(String, Name)>,
    /// The plan keys a release by the node that owns the value: a `Stmt::Let`,
    /// a parameter, or the construct that owns a temporary.
    by_binding: HashMap<usize, Name>,
    temps: u32,
    /// The function's declared name — the key the plan's binding notes use.
    func_name: String,
    /// An unnamed receiver `place` minted for a field read, so the read can
    /// release it afterwards when the plan says the frame owns it (R1').
    pending_receiver: Option<Name>,
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
        self.body.names.push(NameInfo {
            source: source.to_string(),
            ty,
            owned,
            line,
            binding: None,
            receiver: None,
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
            Expr::Call { name, .. } => {
                prelude::lends(name) || name == "value" || self.projection(name).is_some()
            }
            _ => false,
        }
    }

    fn projection(&self, name: &str) -> Option<&'a Function> {
        self.program
            .impls
            .iter()
            .flat_map(|i| i.places.iter())
            .find(|p| p.name == name)
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
            out.push(St::Drop(t));
        }
    }

    fn lookup(&self, name: &str) -> Option<Name> {
        self.scope
            .iter()
            .rev()
            .find(|(n, _)| n == name)
            .map(|(_, i)| *i)
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
                    self.release_receiver(value, out);
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
                let n = self.name(name, ty, owned, *line);
                self.bind(n, rhs, out);
                // The unnamed receiver of the field the binding took or read
                // (RFC-0114 R1′): released after the read where the plan says
                // this frame owns it, held — and seen by the kernel — where
                // it does not.
                if let Expr::Field { .. } = value {
                    self.release_receiver(value, out);
                }
                self.scope.push((name.clone(), n));
                self.keyed(n, sid);
            }
            Stmt::Assign { name, value, line } => {
                let v = self.val(value, out)?;
                let Some(n) = self.lookup(name) else {
                    let _ = line;
                    let ty = self.ty_of(value)?;
                    let old = self.old_for(&ty, sid);
                    out.push(St::Store {
                        place: Place::Global(name.clone()),
                        value: v,
                        old,
                    });
                    return Ok(());
                };
                // The emitters' rule, stated once: the plan says whether the
                // store releases the old value; a value that MENTIONS the
                // place may be handing the old buffer back (`xs = xs.push(v)`)
                // so the release stands down — unless the plan proved every
                // mention a read argument to a function that cannot hand it
                // back (`store_fresh_at`, exit-residue round eighteen).
                let mentions = vyrn_frontend::movecheck::mentions_place(value, name);
                let old = if !self.body.names[n as usize].owned {
                    Old::Nothing
                } else if self.own.plan.store_owned_at(sid)
                    && (!mentions || self.own.plan.store_fresh_at(sid))
                {
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
                // (RFC-0091 M2), and the element's type is the value's.
                let ety = match self.elem_ty(&bty, *line) {
                    Ok(t) => t,
                    Err(_) => self.ty_of(value)?,
                };
                let old = self.old_for(&ety, sid);
                out.push(St::Store {
                    place,
                    value: v,
                    old,
                });
            }
            Stmt::Return { value, .. } => {
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
                            binds,
                            body: t,
                            site: sid,
                            index: 0,
                        },
                        Arm {
                            binds: Vec::new(),
                            body: e,
                            site: sid,
                            index: 1,
                        },
                    ],
                    consuming,
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
                            self.by_binding.insert(sid, t);
                            t
                        } else {
                            n
                        }
                    }
                    _ if !*consuming && is_place_read(iter) => {
                        // `for p in e.path`: the loop walks a container
                        // somebody else owns.
                        let place = self.place(iter, out)?;
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
                        // release by the statement.
                        self.by_binding.insert(sid, t);
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
                let owned = handed_over && self.owns(&ety);
                let x = self.name(var, ety, owned, *line);
                // The variable has no `let` node; the plan keys it by its
                // spelling in the statement, which is one address per loop.
                self.keyed(x, var as *const String as usize);
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
                        out.push(St::Drop(it));
                    }
                } else if *consuming && !self.placed.contains_key(&(Exit::Scrutinee, sid)) {
                    // The loop took the container and the plan placed no row
                    // for it, so the loop gives it back here.
                    out.push(St::Drop(it));
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
                out.push(St::Drop(n));
            }
            Stmt::Expr(e) => {
                let ty = self.ty_of(e).unwrap_or(Type::Unit);
                let rhs = self.rhs(e, out)?;
                if self.owns(&ty) {
                    let t = self.temp(ty, e.line());
                    self.bind(t, rhs, out);
                    if self.own.plan.discarded_result(sid) {
                        out.push(St::Drop(t));
                    }
                } else {
                    out.push(St::Do(rhs));
                    for t in std::mem::take(&mut self.after_of_rhs) {
                        out.push(St::Drop(t));
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
                out.push(St::Drop(*it));
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
            if sub {
                let t = self.temp(ty, self.body.names[n as usize].line);
                out.push(St::Let(t, Rhs::Take(place)));
                out.push(St::Drop(t));
            } else {
                out.push(St::Drop(n));
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
                        return gap("a `consume` of module state", *line);
                    };
                    let t = self.temp(self.body.names[n as usize].ty.clone(), *line);
                    out.push(St::Let(t, Rhs::Val(Val::Name(n))));
                    self.by_binding.insert(construct, t);
                    Ok((Val::Name(t), self.taken_by(t, construct)))
                }
                _ => {
                    let Val::Name(t) = self.take_place(place, out)? else {
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
            if name == "_" {
                continue;
            }
            let owned = consuming && self.owns(&ty);
            let n = self.name(&name, ty, owned, line);
            self.scope.push((name, n));
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
        let ty = self.ty_of(e).ok();
        let owns = ty.as_ref().is_some_and(|t| self.owns(t));
        match e {
            Expr::Field { .. } if owns => {
                let place = self.place(e, out)?;
                let t = self.name("@borrow", ty.unwrap(), false, e.line());
                out.push(St::Let(t, Rhs::Read(place)));
                self.release_receiver(e, out);
                Ok(Val::Name(t))
            }
            Expr::Call { name, args, .. } if owns && name == "@at" && args.len() == 2 => {
                let place = self.place(e, out)?;
                let t = self.name("@borrow", ty.unwrap(), false, e.line());
                out.push(St::Let(t, Rhs::Read(place)));
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

    /// RFC-0114 R1': the unnamed receiver of a field read, freed after the
    /// read when the plan says this frame owns it. Where the plan did not
    /// place the free, the receiver stays held and the kernel says so.
    fn release_receiver(&mut self, e: &Expr, out: &mut Vec<St>) {
        if let Some(r) = self.pending_receiver.take() {
            let node = e as *const Expr as usize;
            if !self.own.plan.receiver_free(node) {
                return;
            }
            // Both emitters run the row after a SCALAR field read only, and
            // after a heap field's take when the row carries the hole
            // (RFC-0125 M3): a row without one stands for nothing there,
            // and the receiver stays held for the kernel to see.
            let holes = self.own.plan.receiver_holes_at(node);
            let took = self.ty_of(e).is_ok_and(|t| self.owns(&t));
            if took && holes.is_empty() {
                return;
            }
            self.body.names[r as usize].holes = holes.iter().map(|h| format!(".{h}")).collect();
            out.push(St::Drop(r));
        }
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
                None if self.program.functions.iter().any(|f| &f.name == name)
                    || self.program.contracts.iter().any(|c| &c.name == name)
                    || vyrn_frontend::types::decl_map(self.program).contains_key(name) =>
                {
                    Ok(Val::Lit)
                }
                None => {
                    // Module state lives for the whole module and nothing
                    // may take it (RFC-0013): `movecheck` refuses passing it
                    // to a `consume` parameter or returning it, and `own`
                    // notes `let x = g` as a borrow. So a read of it in any
                    // position is a borrow, and the name it yields is one
                    // the kernel does not own.
                    let ty = self.ty_of(e)?;
                    let t = if self.owns(&ty) {
                        self.name("@borrow", ty, false, *line)
                    } else {
                        self.temp(ty, *line)
                    };
                    out.push(St::Let(t, Rhs::Read(Place::Global(name.clone()))));
                    Ok(Val::Name(t))
                }
            },
            Expr::Consume { place, line } => match &**place {
                Expr::Var { name, .. } => match self.lookup(name) {
                    Some(n) => Ok(Val::Name(n)),
                    None => gap("a `consume` of module state", *line),
                },
                _ => self.take_place(place, out),
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
                    self.release_receiver(e, out);
                    return Ok(Val::Name(t));
                }
                let rhs = self.rhs(e, out)?;
                let t = if self.lends(e) {
                    self.name("@borrow", ty, false, e.line())
                } else {
                    self.temp(ty, e.line())
                };
                self.bind(t, rhs, out);
                if let Expr::Field { .. } = e {
                    self.release_receiver(e, out);
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
    /// value is this frame's. The lambda's own body is a separate frame and
    /// is not judged here.
    fn lambda(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        let caps = self.captures(e);
        let ty = self.ty_of(e).unwrap_or(Type::Unit);
        let t = self.name("@lambda", ty, false, e.line());
        out.push(St::Let(t, Rhs::Prim(caps)));
        Ok(Val::Name(t))
    }

    /// The names of this body a lambda mentions. Over-approximate: a name the
    /// lambda shadows is counted, and a block-bodied lambda counts every name
    /// in scope (`mentions_place` answers true for one); either costs a read
    /// of a held name and nothing else.
    fn captures(&self, e: &Expr) -> Vec<Val> {
        let Expr::Lambda { params, .. } = e else {
            return Vec::new();
        };
        let mut caps = Vec::new();
        for (name, n) in &self.scope {
            if params.contains(name) || caps.contains(&Val::Name(*n)) {
                continue;
            }
            if vyrn_frontend::movecheck::mentions_place(e, name) {
                caps.push(Val::Name(*n));
            }
        }
        caps
    }

    /// A move out of a sub-place: `consume x.f`, or the receiver a rebuilding
    /// builtin hands back (`s.dense.push(i)` is `s.dense = @push(s.dense, i)`).
    /// The value leaves into an owned name and the base keeps a hole.
    fn take_place(&mut self, e: &'a Expr, out: &mut Vec<St>) -> Result<Val, Gap> {
        let ty = self.ty_of(e)?;
        let place = self.place(e, out)?;
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
            Expr::Unary { expr, .. } => Ok(Rhs::Prim(vec![self.read_val(expr, out)?])),
            Expr::Binary { lhs, rhs, .. } => {
                let a = self.read_val(lhs, out)?;
                let b = self.read_val(rhs, out)?;
                Ok(Rhs::Prim(vec![a, b]))
            }
            Expr::Field { expr, field, .. } => {
                let fty = self.ty_of(e)?;
                let place = self.place(expr, out)?;
                if let Some(r) = self.pending_receiver {
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
                let r = self.call(name, args, *line, out)?;
                out.push(St::Do(r));
                out.push(St::Trap);
                Ok(Rhs::Val(Val::Lit))
            }
            Expr::Call { name, args, line } => self.call(name, args, *line, out),
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
                })
            }
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                line,
            } => {
                let ty = self.ty_of(e)?;
                let res = self.temp(ty, *line);
                let c = self.read_val(cond, out)?;
                let mut t = Vec::new();
                let tv = self.val(then_branch, &mut t)?;
                t.push(St::Store {
                    place: Place::Name(res),
                    value: tv,
                    old: Old::Nothing,
                });
                let mut f = Vec::new();
                match else_branch {
                    Some(eb) => {
                        let ev = self.val(eb, &mut f)?;
                        f.push(St::Store {
                            place: Place::Name(res),
                            value: ev,
                            old: Old::Nothing,
                        });
                    }
                    None => return gap("an `if` expression without `else`", *line),
                }
                out.push(St::If {
                    cond: c,
                    then: t,
                    els: f,
                    site: 0,
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
                            body.push(St::Store {
                                place: Place::Name(res),
                                value: v,
                                old: Old::Nothing,
                            });
                        }
                        ArmBody::Block(blk) => self.block(blk, &mut body)?,
                    }
                    if let Some(rows) = self.own.plan.arm_payload_free(mid, i as u32) {
                        for b in &binds {
                            let src = &self.body.names[*b as usize].source;
                            if let Some((_, _, holes)) = rows.iter().find(|(n, _, _)| n == src) {
                                self.body.names[*b as usize].holes =
                                    holes.iter().map(|h| format!(".{h}")).collect();
                                body.push(St::Drop(*b));
                            }
                        }
                    }
                    self.edge_drops(mid, i as u32, &mut body)?;
                    self.scope.truncate(mark);
                    core_arms.push(Arm {
                        binds,
                        body,
                        site: mid,
                        index: i as u32,
                    });
                }
                out.push(St::Switch {
                    on: sv,
                    arms: core_arms,
                    consuming,
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
                });
                self.scope.truncate(mark);
                out.push(St::Switch {
                    on: sv,
                    arms: vec![
                        Arm {
                            binds: fb,
                            body: fail,
                            site: tid,
                            index: 0,
                        },
                        Arm {
                            binds: ob,
                            body: ok,
                            site: tid,
                            index: 1,
                        },
                    ],
                    consuming,
                });
                Ok(Rhs::Val(Val::Name(res)))
            }
            Expr::Lambda { .. } => Ok(Rhs::Prim(self.captures(e))),
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
        });
        let mut ok = Vec::new();
        let t = self.temp(self.body.names[res as usize].ty.clone(), line);
        ok.push(St::Let(
            t,
            Rhs::Call {
                callee: success,
                args: vec![(sv.clone(), Capability::Consume)],
            },
        ));
        ok.push(St::Store {
            place: Place::Name(res),
            value: Val::Name(t),
            old: Old::Nothing,
        });
        out.push(St::Switch {
            on: sv,
            arms: vec![
                Arm {
                    binds: Vec::new(),
                    body: fail,
                    site: tid,
                    index: 0,
                },
                Arm {
                    binds: Vec::new(),
                    body: ok,
                    site: tid,
                    index: 1,
                },
            ],
            consuming: false,
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
                            self.pending_receiver = Some(t);
                        }
                        Ok(Place::Name(t))
                    }
                    Val::Lit => gap("a place that is a literal", e.line()),
                }
            }
        }
    }

    fn call(
        &mut self,
        name: &str,
        args: &'a [Expr],
        line: usize,
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
                || matches!(name, "render" | "lex")
            {
                // A reserved name with no prelude row (`fromJson`, `value`,
                // `lex`, `render`, a log level): its capabilities are the
                // prelude's answer where it has one, and `read` elsewhere.
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
        for (k, (a, cap)) in args.iter().zip(caps.iter()).enumerate() {
            let v = if *cap == Capability::Consume {
                if k == 0
                    && rebuilds
                    && !matches!(a, Expr::Var { .. } | Expr::Consume { .. })
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
                    && !self.after.contains(&t)
                {
                    temps_to_drop.push(t);
                }
            }
            vs.push((v, *cap));
        }
        self.after.extend(temps_to_drop);
        Ok(Rhs::Call {
            callee: name.to_string(),
            args: vs,
        })
    }

    fn is_variant(&self, name: &str) -> bool {
        let decls = vyrn_frontend::types::decl_map(self.program);
        decls
            .values()
            .any(|d| matches!(&d.base, Type::Enum(vs) if vs.iter().any(|v| v.name == name)))
    }
}

thread_local! {
    static STRICT_REFUSALS: std::cell::RefCell<Vec<String>> = const { std::cell::RefCell::new(Vec::new()) };
}

/// Whether `VYRN_KERNEL_STRICT=1` is set: a hard refusal by the kernel fails
/// `vyrn check` and `vyrn build`.
pub fn strict() -> bool {
    std::env::var("VYRN_KERNEL_STRICT").is_ok_and(|v| v == "1")
}

/// The hard refusals the placer met since the last call, on this thread: a
/// double free, a use after release, a join whose edges disagree — what no
/// placement repairs. Drained by the CLI in strict mode after an analysis.
pub fn take_strict_refusals() -> Vec<String> {
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
        let body = match build(program, inst, own) {
            Ok(b) => b,
            Err(g) => {
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
        // `VYRN_KERNEL_TRACE=<fn>` prints that body's core.
        if std::env::var("VYRN_KERNEL_TRACE").is_ok_and(|v| v != "1" && body.name.contains(&v)) {
            eprintln!("{}", body.render());
        }
        let missing = match crate::kernel::placement(&body) {
            Ok(m) => m,
            Err(r) => {
                if trace {
                    eprintln!("placer: refused: {}", r.message);
                }
                // A refusal no placement repairs: a double free, a use after
                // release, a join whose edges disagree. Kept for the CLI's
                // strict mode.
                STRICT_REFUSALS.with(|v| v.borrow_mut().push(r.message.clone()));
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
                r.full = false;
                r.holes = Some(holes);
                continue;
            }
            let dup = added.iter().any(|(f, r, _)| {
                *f == inst.func.name && r.exit == m.exit && r.site == m.site && r.binding == binding
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
    for (f, row, kind) in added {
        own.droppable
            .entry(f.clone())
            .or_default()
            .entry(row.binding)
            .or_insert(kind);
        own.releases.entry(f).or_default().push(row);
    }
}
