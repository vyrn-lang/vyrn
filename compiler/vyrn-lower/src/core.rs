//! The named core — RFC-0125 §2.1, first slice (M2).
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
//! A construct this slice does not lower yet returns a [`Gap`] naming it, and
//! the instance is reported as unlowered rather than accepted or refused. The
//! corpus test counts gaps by construct; the list is the work left.

use std::collections::HashMap;

use vyrn_frontend::ast::{
    ArmBody, Block, Capability, Expr, Function, Pattern, Program, Stmt, Type,
};
use vyrn_frontend::own::{DropKind, Exit, Fate, Leak, Owned, Ownership, Release};
use vyrn_frontend::prelude;

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
}

#[derive(Debug, Clone)]
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
}

#[derive(Debug, Clone)]
pub enum Rhs {
    /// The value of a name: a move when the name is owned, a copy otherwise.
    Val(Val),
    /// A read of a place that yields a value the kernel does not own — a scalar
    /// field, an element of a heapless type, a borrowed payload.
    Read(Place),
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
    If(Val, Vec<St>, Vec<St>),
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
        }
    }

    fn rhs(&self, r: &Rhs) -> String {
        match r {
            Rhs::Val(v) => self.val(v),
            Rhs::Read(p) => format!("read {}", self.place(p)),
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
                St::If(c, t, e) => {
                    out.push_str(&format!("{pad}if {}\n", self.val(c)));
                    self.render_stmts(t, depth + 1, out);
                    out.push_str(&format!("{pad}else\n"));
                    self.render_stmts(e, depth + 1, out);
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
    // A binding with a hole (RFC-0093) releases part of itself; the kernel of
    // this slice tracks whole names only.
    if own
        .holes
        .get(&inst.func.name)
        .is_some_and(|h| h.values().any(|v| !v.is_empty()))
    {
        return gap("a binding with a `consume` hole", inst.func.line);
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
        });
        (self.body.names.len() - 1) as Name
    }

    /// Record the plan's key for a name, and the name for the key.
    fn keyed(&mut self, n: Name, binding: usize) {
        self.body.names[n as usize].binding = Some(binding);
        self.by_binding.insert(binding, n);
    }

    /// What the plan decided a named binding's fate is: whether THIS frame
    /// owns it. Static data, an alias of a place, a borrow, a capture, a value
    /// the arena holds and a value a callee took are not this frame's to
    /// release; a binding with no release rule for its type is, and the
    /// kernel will say so.
    fn fate_owned(&self, name: &str, line: usize, ty: &Type) -> Option<bool> {
        let Some(notes) = self.own.notes.get(&self.func_name) else {
            return if self.owns(ty) { None } else { Some(false) };
        };
        let Some(n) = notes.iter().find(|n| n.name == name && n.line == line) else {
            return if self.owns(ty) { None } else { Some(false) };
        };
        Some(match &n.fate {
            // The plan releases, moves or discharges it: it is this frame's.
            Fate::Reclaimed(..)
            | Fate::Moved { .. }
            | Fate::Dropped { .. }
            | Fate::Discharged(_) => true,
            Fate::Leaked(Leak::NoRelease { owns_heap, .. }) => *owns_heap,
            Fate::Static | Fate::Leaked(_) => false,
        })
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
            None => gap("an expression the checker did not type", e.line()),
        }
    }

    /// The releases the plan placed at one exit, as drops, in the plan's order.
    fn drops_at(&self, exit: Exit, site: usize, out: &mut Vec<St>) -> Result<(), Gap> {
        let Some(rows) = self.placed.get(&(exit, site)) else {
            return Ok(());
        };
        for r in rows {
            match self.by_binding.get(&r.binding) {
                Some(n) => out.push(St::Drop(*n)),
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
        let mark = self.scope.len();
        let site = blk as *const Block as usize;
        let mut body = Vec::new();
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
                let owned = self
                    .fate_owned(name, *line, &ty)
                    .unwrap_or_else(|| self.owns(&ty) && !literal);
                let n = self.name(name, ty, owned, *line);
                self.bind(n, rhs, out);
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
                let Some(n) = self.lookup(name) else {
                    return gap("a field store into module state", *line);
                };
                let fty = self.field_ty(&self.body.names[n as usize].ty.clone(), field, *line)?;
                let old = self.old_for(&fty, sid);
                out.push(St::Store {
                    place: Place::Field(Box::new(Place::Name(n)), field.clone()),
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
                let i = self.read_val(index, out)?;
                let v = self.val(value, out)?;
                let Some(n) = self.lookup(name) else {
                    return gap("an element store into module state", *line);
                };
                let ety = self.elem_ty(&self.body.names[n as usize].ty.clone(), *line)?;
                let old = self.old_for(&ety, sid);
                out.push(St::Store {
                    place: Place::Elem(Box::new(Place::Name(n)), i),
                    value: v,
                    old,
                });
            }
            Stmt::Return { value, .. } => {
                let v = match value {
                    Some(e) => Some(self.val(e, out)?),
                    None => None,
                };
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
                out.push(St::If(c, t, e));
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
                        Arm { binds, body: t },
                        Arm {
                            binds: Vec::new(),
                            body: e,
                        },
                    ],
                    consuming,
                });
                self.drops_at(Exit::Scrutinee, sid, out)?;
            }
            Stmt::While { cond, body, .. } => {
                let mut l = Vec::new();
                let c = self.read_val(cond, &mut l)?;
                l.push(St::If(c, Vec::new(), vec![St::Break { site: 0 }]));
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
                let ety = self.elem_ty(&ity, *line)?;
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
                l.push(St::Let(
                    x,
                    Rhs::Read(Place::Elem(Box::new(Place::Name(it)), Val::Lit)),
                ));
                let mark = self.scope.len();
                self.scope.push((var.clone(), x));
                self.block(body, &mut l)?;
                self.scope.truncate(mark);
                out.push(St::Loop(l));
                if *consuming && !self.placed.contains_key(&(Exit::Scrutinee, sid)) {
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
            Stmt::Region { line, .. } => return gap("a `region`", *line),
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

    /// RFC-0114 Rule N: the drops one edge of a join owes.
    fn edge_drops(&self, join: usize, edge: u32, out: &mut Vec<St>) -> Result<(), Gap> {
        let Some(ers) = self.own.plan.edge_releases_at(join) else {
            return Ok(());
        };
        for (name, e) in ers {
            if *e != edge {
                continue;
            }
            match self.lookup(name) {
                Some(n) => out.push(St::Drop(n)),
                None => return gap("an edge release of a name out of scope", 0),
            }
        }
        Ok(())
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
            _ => gap("an element of a non-container", line),
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
                _ => gap("a `consume` of a place that is not a name", *line),
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
            Expr::Var { name, line } if owns && self.lookup(name).is_none() => {
                let t = self.name("@borrow", ty.unwrap(), false, *line);
                out.push(St::Let(t, Rhs::Read(Place::Global(name.clone()))));
                Ok(Val::Name(t))
            }
            Expr::Var { .. } | Expr::Consume { .. } => self.val(e, out),
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
            if self.own.plan.receiver_free(e as *const Expr as usize) {
                out.push(St::Drop(r));
            }
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
                None => {
                    let ty = self.ty_of(e)?;
                    if self.owns(&ty) {
                        return gap("a read of module state that owns heap", *line);
                    }
                    let t = self.temp(ty, *line);
                    out.push(St::Let(t, Rhs::Read(Place::Global(name.clone()))));
                    Ok(Val::Name(t))
                }
            },
            Expr::Consume { place, line } => match &**place {
                Expr::Var { name, .. } => match self.lookup(name) {
                    Some(n) => Ok(Val::Name(n)),
                    None => gap("a `consume` of module state", *line),
                },
                _ => gap("a `consume` of a place that is not a name", *line),
            },
            _ => {
                let ty = self.ty_of(e)?;
                if is_place_read(e) && self.owns(&ty) {
                    return gap("a move out of a field or an element", e.line());
                }
                let rhs = self.rhs(e, out)?;
                let t = self.temp(ty, e.line());
                self.bind(t, rhs, out);
                if let Expr::Field { .. } = e {
                    self.release_receiver(e, out);
                }
                Ok(Val::Name(t))
            }
        }
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
            Expr::Field { expr, field, line } => {
                let fty = self.ty_of(e)?;
                let place = self.place(expr, out)?;
                if self.owns(&fty) {
                    return gap("a read of a field that owns heap", *line);
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
                out.push(St::If(c, t, f));
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
                    if self.own.plan.arm_payload_free(mid, i as u32).is_some() {
                        for b in &binds {
                            body.push(St::Drop(*b));
                        }
                    }
                    self.edge_drops(mid, i as u32, &mut body)?;
                    self.scope.truncate(mark);
                    core_arms.push(Arm { binds, body });
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
                        },
                        Arm {
                            binds: ob,
                            body: ok,
                        },
                    ],
                    consuming,
                });
                Ok(Rhs::Val(Val::Name(res)))
            }
            Expr::Lambda { line, .. } => gap("a lambda", *line),
        }
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
            Expr::Call { name, args, line } if name == "@at" && args.len() == 2 => {
                let decls = vyrn_frontend::types::decl_map(self.program);
                let bty = self.ty_of(&args[0])?;
                if matches!(vyrn_frontend::types::resolve(&bty, &decls), Type::Map(..)) {
                    return gap("a map lookup as a place", *line);
                }
                let base = self.place(&args[0], out)?;
                let i = self.read_val(&args[1], out)?;
                Ok(Place::Elem(Box::new(base), i))
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
        let caps: Vec<Capability> =
            if let Some(f) = self.program.functions.iter().find(|f| f.name == name) {
                f.params.iter().map(|p| p.capability).collect()
            } else if let Some(sig) = prelude::signature(name) {
                let mut caps: Vec<Capability> = (0..args.len())
                    .map(|i| prelude::capability(name, i).unwrap_or(Capability::Read))
                    .collect();
                let rebuilds = sig.params.first().is_some_and(|p| p.ty == sig.ret)
                    && matches!(
                        sig.ret,
                        Type::Array(_) | Type::SmallArray(..) | Type::Map(..)
                    );
                if rebuilds && !caps.is_empty() {
                    caps[0] = Capability::Consume;
                }
                caps
            } else if let Some(m) = method {
                m.params.iter().map(|p| p.capability).collect()
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
            } else if matches!(name, "Some" | "Ok" | "Err") || self.is_variant(name) {
                vec![Capability::Consume; args.len()]
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
        for (a, cap) in args.iter().zip(caps.iter()) {
            let v = if *cap == Capability::Consume {
                self.val(a, out)?
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
    let mut added: Vec<(String, Release, DropKind)> = Vec::new();
    for inst in &lowered.instances {
        let Ok(body) = build(program, inst, own) else {
            continue;
        };
        let Ok(missing) = crate::kernel::placement(&body) else {
            continue;
        };
        for m in missing {
            if m.site == 0 {
                continue;
            }
            let info = &body.names[m.name as usize];
            let Some(binding) = info.binding else {
                continue;
            };
            let Some(kind) = own.proto.release_kind(&info.ty) else {
                continue;
            };
            let dup = own.releases.get(&inst.func.name).is_some_and(|rows| {
                rows.iter()
                    .any(|r| r.exit == m.exit && r.site == m.site && r.binding == binding)
            }) || added.iter().any(|(f, r, _)| {
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
