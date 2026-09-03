//! The typed judgment — RFC-0125 §2.2, judgment 3 (M6, the third judgment's
//! second slice).
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
//! WHICH crossings are validated is [`vyrn_frontend::validate`]'s, and the
//! caller asks it: `judge` takes the question in the same shape the rule is
//! stated in — a type crossed FROM, a type crossed INTO — so the judgment and
//! the three engines cannot answer it differently. A crossing whose source
//! type the core does not carry is asked with `None`, which is the form the
//! interpreter asks in (it has a value, not a source type).
//!
//! # The one thing the core cannot name yet
//!
//! `Rhs::Prim` erases the operator: `a + b` and `UInt8(n)` are both "a
//! primitive over these operands", so the TYPE a primitive produces is not in
//! the core. Neither is the type a call answers — the caller resolves that
//! where it can (`ret`), and a builtin resolves to nothing. A store into a
//! SIZED INTEGER whose producer has no type is therefore counted as unjudged
//! rather than guessed at ([`Judged::unjudged`]): every integer store would
//! otherwise read as a narrowing, because a narrowing is exactly a store whose
//! producer is of another width. The count is the honest size of what §2.3's
//! constructor would close — a narrowing that is a call has a callee to name.

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
    Constructor,
    /// A name already of the type — nothing crossed, so nothing is owed.
    ByName,
    /// A literal. The checker proves a literal against its slot's type at
    /// compile time (RFC-0003's const validation), so no producer runs.
    Literal,
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
    /// Stores into a sized integer whose producer has no type: the core
    /// carries none for a primitive, and the caller resolves none for a
    /// builtin. Counted rather than guessed (the note above).
    pub unjudged: usize,
}

impl Judged {
    pub fn findings(&self) -> impl Iterator<Item = &Store> {
        self.stores.iter().filter(|s| s.how.is_finding())
    }
}

/// The judgment over `bodies`, each a frame of the core.
///
/// `validated` is the rule, asked as the rule is stated: the type crossed FROM
/// where the core carries one, the type crossed INTO, and the answer is the
/// name of the declaration whose producer must have run. `step` says what a
/// place holds and `ret` what a callee answers — two questions about the
/// program's declarations, which the judgment does not hold.
pub fn judge(
    bodies: &[&Body],
    validated: &mut dyn FnMut(Option<&Type>, &Type) -> Option<String>,
    step: &mut dyn FnMut(Option<&Type>, Step) -> Option<Type>,
    ret: &mut dyn FnMut(&str, &[Option<Type>]) -> Option<Type>,
) -> Judged {
    let mut out = Judged::default();
    for (i, b) in bodies.iter().enumerate() {
        let mut w = Walk {
            body: b,
            index: i,
            born: HashMap::new(),
            validated,
            step,
            ret,
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
    validated: &'b mut dyn FnMut(Option<&Type>, &Type) -> Option<String>,
    step: &'b mut dyn FnMut(Option<&Type>, Step) -> Option<Type>,
    ret: &'b mut dyn FnMut(&str, &[Option<Type>]) -> Option<Type>,
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
            | St::Drop(_)
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
        // A sized integer whose producer has no type cannot be judged: a
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
        let Some(name) = (self.validated)(from.as_ref(), &to) else {
            return;
        };
        let how = match rhs {
            _ if ctor => How::Constructor,
            Rhs::Val(Val::Lit) => How::Literal,
            // A name or a place already of the type is not a crossing —
            // `validate::required`'s one exemption, asked the same way.
            _ if from.as_ref().is_some_and(|f| *f == to) => How::ByName,
            Rhs::Make(_) => How::Finding("record-literal"),
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

    /// The type a right-hand side produces, where the core or the caller
    /// carries one. A primitive has none (the note at the top); a call has the
    /// one its declaration answers, and a builtin has none.
    fn rhs_ty(&mut self, rhs: &Rhs) -> Option<Type> {
        match rhs {
            Rhs::Val(Val::Name(n)) => Some(self.body.names[*n as usize].ty.clone()),
            Rhs::Read(p) | Rhs::Take(p) => self.place_ty(p),
            // A builtin's answer is often its receiver's own value — a copy of
            // a `Title` is a `Title` — so the caller is handed the argument
            // types with the name.
            Rhs::Call { callee, args, .. } => {
                let at: Vec<Option<Type>> = args
                    .iter()
                    .map(|(v, _)| match v {
                        Val::Name(n) => Some(self.body.names[*n as usize].ty.clone()),
                        Val::Lit => None,
                    })
                    .collect();
                (self.ret)(callee, &at)
            }
            _ => None,
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
