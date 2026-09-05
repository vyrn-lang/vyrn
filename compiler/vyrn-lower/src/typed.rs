//! The typed judgment — RFC-0125 §2.2, judgment 3 (M6, the third judgment's
//! third slice).
//!
//! Over a [`Body`] in the named core: **a name of a validated type is produced
//! only by that type's constructor.** The census of §3 M6 states it as the
//! fifth line of its design — "for every store into a place whose type is
//! validated, the value's producer is that type's `validate`, or a name already
//! of that type, or a literal the checker proved. It refuses anything else."
//! This file is that walk, beside the linear judgment (`kernel`) and the effect
//! judgment (`effects`), over the same form.
//!
//! It is a USE-DEF walk and nothing else. Every name of a body is bound once
//! (`St::Let`), so the producer of a name is a lookup rather than a dataflow:
//! the walk records what bound each name, and asks about the value of every
//! store into a place the caller calls validated.
//!
//! # What the judgment does NOT decide
//!
//! WHICH types carry a rule is [`vyrn_frontend::validate`]'s, and the caller
//! asks it, so the judgment and the three engines cannot answer it
//! differently. What the judgment decides is the other half: whether the
//! producer of THIS store satisfied that rule. The two are one sentence — a
//! name of a validated type is produced only by that type's constructor — cut
//! where the program's declarations stop and the body's own text starts.
//!
//! # The producer type
//!
//! Every right-hand side of the core names the type it produces (`core::Rhs`,
//! the third slice): `Rhs::Prim` carries the operator's own result and
//! `Rhs::Call` what the callee answers at that site, both the checker's answer
//! at the node. So `a + b` and `UInt8(n)` no longer read alike, and a store
//! into a SIZED INTEGER is judged by the lookup that answers every other
//! store: a producer at the destination's width and signedness crossed
//! nothing, the type's own conversion is its constructor, and a producer of
//! another width that is neither is a finding. Before the third slice 94,691
//! such stores were counted as unjudged, because a judgment that guessed would
//! have read every integer store as a narrowing.
//!
//! [`Judged::unjudged`] survives for the one thing the core still cannot
//! supply: a store whose producer is a read of a place the CALLER resolves no
//! type for — a generic parameter, a type the program does not declare. The
//! core names a producer there and the declarations do not say what it holds.
//! The corpus has none since the third slice.

use std::collections::HashMap;

use vyrn_frontend::ast::Type;

use crate::core::{Body, Name, Place, Rhs, St, Val};

/// A step from one type into the type a place holds, for the caller that
/// resolves a place's type. `Global` has no base.
#[derive(Debug, Clone, Copy)]
pub enum Step<'a> {
    Field(&'a str),
    Elem,
    Key,
    Global(&'a str),
}

/// What produced the value a store put into a validated place.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum How {
    /// The type's own constructor: `Age(n)`, which is where the predicate runs.
    /// A RECORD LITERAL of a validated record type is the same answer — it is
    /// that type's second producer by design (RFC-0003's cross-field `where` has
    /// no other spelling), and since RFC-0125 §3 M6's fourth slice all three
    /// engines run the generated constructor at it.
    Constructor,
    /// A name already of the type — nothing crossed, so nothing is owed.
    ByName,
    /// A literal. The checker proves a literal against its slot's type at
    /// compile time (RFC-0003's const validation), so no producer runs.
    Literal,
    /// A primitive over literals only, into a SIZED INTEGER: a constant the
    /// program wrote out. The checker ranges it against the destination where
    /// the two have a sign in common — `-200` into an `Int8` is refused — and
    /// where they do not, the census's `int-narrowing` row answers rather than
    /// refusing, which is what that row IS: `-1` into a `UInt8` is 255, the
    /// same fact as `UInt8(300)` being 44. Nothing crossed unchecked, so this
    /// is not a finding; it is named apart from [`How::Literal`] because the
    /// two are proved by different halves of the rule.
    Constant,
    /// Anything else, by kind. Each one is a raw value reaching a validated
    /// slot, which is what the judgment refuses.
    Finding(&'static str),
}

impl How {
    /// The kind's name, for a tally and for the RFC's record.
    pub fn kind(&self) -> &'static str {
        match self {
            How::Constructor => "by-constructor",
            How::ByName => "by-name",
            How::Literal => "by-literal",
            How::Constant => "by-constant",
            How::Finding(k) => k,
        }
    }

    pub fn is_finding(&self) -> bool {
        matches!(self, How::Finding(_))
    }
}

/// One store the judgment looked at.
#[derive(Debug, Clone)]
pub struct Store {
    /// The body, by index into the slice handed to [`judge`].
    pub body: usize,
    /// The place, as the core spells it.
    pub place: String,
    /// The validated type the place holds.
    pub ty: String,
    /// What produced the value, as the core spells it: a callee's name, or the
    /// kind of right-hand side. A finding is read by this.
    pub producer: String,
    pub line: usize,
    pub how: How,
}

/// The judgment's answer.
#[derive(Debug, Default)]
pub struct Judged {
    /// Every store into a validated place, in body order.
    pub stores: Vec<Store>,
    /// Stores into a sized integer whose producer names no type: a read of a
    /// place the caller resolves none for. Counted rather than guessed (the
    /// note above), and zero over the corpus.
    pub unjudged: usize,
}

impl Judged {
    pub fn findings(&self) -> impl Iterator<Item = &Store> {
        self.stores.iter().filter(|s| s.how.is_finding())
    }
}

/// The judgment over `bodies`, each a frame of the core.
///
/// `validated` is the rule: a type a store lands in, and the name of the
/// declaration whose producer must have run for it — a named type with a
/// `where`, or a sized integer, whose rule is the census's two narrowing rows.
/// `step` says what a place holds. Both are questions about the program's
/// declarations, which the judgment does not hold. What a callee answers is
/// asked of nobody now: the core carries it (`Rhs::Call::ret`).
pub fn judge(
    bodies: &[&Body],
    validated: &mut dyn FnMut(&Type) -> Option<String>,
    step: &mut dyn FnMut(Option<&Type>, Step) -> Option<Type>,
) -> Judged {
    let mut out = Judged::default();
    for (i, b) in bodies.iter().enumerate() {
        let mut w = Walk {
            body: b,
            index: i,
            born: HashMap::new(),
            validated,
            step,
            out: &mut out,
        };
        w.stmts(&b.stmts);
    }
    out
}

struct Walk<'a, 'b> {
    body: &'a Body,
    index: usize,
    /// What bound each name — the use-def edge, and the whole state this
    /// judgment carries.
    born: HashMap<Name, &'a Rhs>,
    validated: &'b mut dyn FnMut(&Type) -> Option<String>,
    step: &'b mut dyn FnMut(Option<&Type>, Step) -> Option<Type>,
    out: &'b mut Judged,
}

impl<'a> Walk<'a, '_> {
    fn stmts(&mut self, stmts: &'a [St]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &'a St) {
        match s {
            St::Let(n, rhs) => {
                self.born.insert(*n, rhs);
                let info = &self.body.names[*n as usize];
                self.judge_store(info.ty.clone(), info.source.clone(), info.line, rhs);
            }
            St::Store {
                place, value, line, ..
            } => {
                if let Some(ty) = self.place_ty(place) {
                    // The value's producer: the `let` that bound the name in
                    // this frame, or the name itself when it was bound outside
                    // one — a parameter, a capture, an arm binder — where its
                    // own type is what answers.
                    let outside;
                    let rhs: &Rhs = match value {
                        Val::Name(n) => match self.born.get(n).copied() {
                            Some(r) => r,
                            None => {
                                outside = Rhs::Val(Val::Name(*n));
                                &outside
                            }
                        },
                        Val::Lit => {
                            outside = Rhs::Val(Val::Lit);
                            &outside
                        }
                    };
                    let place = self.spell(place);
                    self.judge_store(ty, place, *line, rhs);
                }
            }
            St::If { then, els, .. } => {
                self.stmts(then);
                self.stmts(els);
            }
            St::Loop(body) | St::Block { body, .. } => self.stmts(body),
            St::Switch { arms, .. } => {
                for a in arms {
                    self.stmts(&a.body);
                }
            }
            St::Do(..)
            | St::Drop(..)
            | St::Row { .. }
            | St::Break { .. }
            | St::Continue { .. }
            | St::Return { .. }
            | St::Trap => {}
        }
    }

    /// The one judgment. `to` is the place's type, `rhs` what the store was
    /// given.
    fn judge_store(&mut self, to: Type, place: String, line: usize, rhs: &Rhs) {
        let from = self.rhs_ty(rhs);
        // A producer NAMED after the type is that type's constructor, whatever
        // the core knows about what it answers.
        let ctor = matches!(rhs, Rhs::Call { callee, .. } if last(callee) == spelling(&to));
        // A sized integer whose producer names no type cannot be judged: a
        // narrowing IS a store whose producer is of another width, so guessing
        // would read every integer store as one (the note at the top).
        if from.is_none()
            && matches!(to, Type::IntN { .. })
            && !ctor
            && !matches!(rhs, Rhs::Val(Val::Lit))
        {
            self.out.unjudged += 1;
            return;
        }
        let Some(name) = (self.validated)(&to) else {
            return;
        };
        let how = match rhs {
            _ if ctor => How::Constructor,
            Rhs::Val(Val::Lit) => How::Literal,
            // A constant into a sized integer, and only there: a named type's
            // predicate still owes a producer whatever the operands are.
            Rhs::Prim(vs, _)
                if matches!(to, Type::IntN { .. })
                    && !vs.is_empty()
                    && vs.iter().all(|v| matches!(v, Val::Lit)) =>
            {
                How::Constant
            }
            // A name or a place already of the type is not a crossing —
            // `validate::required`'s one exemption, asked the same way. For a
            // sized integer the exemption is `validate::narrows` read the
            // other way round: a producer at the destination's width and
            // signedness re-reads no bits, and `Int` and `Int64` are one width
            // written two ways.
            _ if from
                .as_ref()
                .is_some_and(|f| *f == to || same_width(f, &to)) =>
            {
                How::ByName
            }
            // A record literal of a validated record type: the type's other
            // producer, and the one the `where-record` row exists for.
            Rhs::Make(_) => How::Constructor,
            Rhs::Call { .. } => How::Finding("other-call"),
            Rhs::Prim(..) => How::Finding("primitive"),
            Rhs::Read(_) | Rhs::Take(_) => How::Finding("read-of-place"),
            Rhs::Val(Val::Name(_)) => How::Finding("other-name"),
        };
        self.out.stores.push(Store {
            body: self.index,
            place,
            ty: name,
            producer: match rhs {
                Rhs::Call { callee, .. } => callee.clone(),
                Rhs::Prim(..) => "@prim".into(),
                Rhs::Make(_) => "@make".into(),
                Rhs::Read(p) | Rhs::Take(p) => self.spell(p),
                Rhs::Val(Val::Name(n)) => self.body.names[*n as usize].source.clone(),
                Rhs::Val(Val::Lit) => "@lit".into(),
            },
            line,
            how,
        });
    }

    /// The type a right-hand side produces. A primitive and a call carry the
    /// checker's answer at their own node; a name and a place are typed by the
    /// core and the declarations. A literal has none, and it is the one
    /// producer that needs none.
    fn rhs_ty(&mut self, rhs: &Rhs) -> Option<Type> {
        match rhs {
            Rhs::Val(Val::Name(n)) => Some(self.body.names[*n as usize].ty.clone()),
            Rhs::Read(p) | Rhs::Take(p) => self.place_ty(p),
            Rhs::Call { ret, .. } => ret.clone(),
            Rhs::Prim(_, ty) => ty.clone(),
            Rhs::Make(_) | Rhs::Val(Val::Lit) => None,
        }
    }

    /// The type a place holds, through the caller's declarations.
    fn place_ty(&mut self, p: &Place) -> Option<Type> {
        match p {
            Place::Name(n) => Some(self.body.names[*n as usize].ty.clone()),
            Place::Global(g) => (self.step)(None, Step::Global(g)),
            Place::Field(base, f) => {
                let b = self.place_ty(base);
                (self.step)(b.as_ref(), Step::Field(f))
            }
            Place::Elem(base, _) => {
                let b = self.place_ty(base);
                (self.step)(b.as_ref(), Step::Elem)
            }
            Place::Key(base, _) => {
                let b = self.place_ty(base);
                (self.step)(b.as_ref(), Step::Key)
            }
        }
    }

    /// A place as the core spells it, for a finding a reader has to find in
    /// the source.
    fn spell(&self, p: &Place) -> String {
        match p {
            Place::Name(n) => self.body.names[*n as usize].source.clone(),
            Place::Global(g) => g.clone(),
            Place::Field(b, f) => format!("{}.{f}", self.spell(b)),
            Place::Elem(b, _) => format!("{}[]", self.spell(b)),
            Place::Key(b, _) => format!("{}{{}}", self.spell(b)),
        }
    }
}

/// Whether two types are integers of one width and signedness — the crossing
/// `validate::narrows` says re-reads no bits (RFC-0125 §3 M6).
fn same_width(from: &Type, to: &Type) -> bool {
    vyrn_frontend::validate::width(from).is_some()
        && vyrn_frontend::validate::width(to).is_some()
        && !vyrn_frontend::validate::narrows(from, to)
}

/// How a type names its own producer: a named type by its name, and anything
/// else by its spelling, which is what a conversion is called (`UInt8`).
fn spelling(t: &Type) -> String {
    match t {
        Type::Named(n) => n.clone(),
        other => other.to_string(),
    }
}

/// The last segment of a callee's spelling: `mod.Age` and `Age` name one
/// declaration.
fn last(callee: &str) -> &str {
    callee
        .rsplit(['.', ':', '/'])
        .next()
        .unwrap_or(callee)
        .trim_start_matches('@')
}
