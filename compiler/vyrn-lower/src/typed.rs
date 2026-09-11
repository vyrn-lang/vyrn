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
                        Val::Lit(_) => {
                            outside = Rhs::Val(Val::Lit(crate::core::Lit::Opaque));
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
            St::Loop { body, .. } | St::Block { body, .. } => self.stmts(body),
            St::Switch { arms, .. } => {
                for a in arms {
                    self.stmts(&a.body);
                }
            }
            St::Do { .. }
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
            && !matches!(rhs, Rhs::Val(Val::Lit(_)))
        {
            self.out.unjudged += 1;
            return;
        }
        let Some(name) = (self.validated)(&to) else {
            return;
        };
        let how = match rhs {
            _ if ctor => How::Constructor,
            Rhs::Val(Val::Lit(_)) => How::Literal,
            // A constant into a sized integer, and only there: a named type's
            // predicate still owes a producer whatever the operands are.
            Rhs::Prim(_, vs, _)
                if matches!(to, Type::IntN { .. })
                    && !vs.is_empty()
                    && vs.iter().all(|v| matches!(v, Val::Lit(_))) =>
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
            Rhs::Make(..) => How::Constructor,
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
                Rhs::Make(..) => "@make".into(),
                Rhs::Read(p) | Rhs::Take(p) => self.spell(p),
                Rhs::Val(Val::Name(n)) => self.body.names[*n as usize].source.clone(),
                Rhs::Val(Val::Lit(_)) => "@lit".into(),
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
            Rhs::Prim(_, _, ty) => ty.clone(),
            Rhs::Make(..) | Rhs::Val(Val::Lit(_)) => None,
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

/// The **must-use** obligation: a value of a linear type is acquired once and
/// disposed exactly once, and this is where that is proved (RFC-0086 M3).
///
/// It lived in `movecheck.rs` until RFC-0125 §3 M3's obligation slice, and it
/// was never an ownership rule: RFC-0125 §2.2 has three judgments, and a
/// TYPE's obligation belongs to the typed one. That is the whole move. The
/// walk, the wordings and the menus are unchanged, and the census rows 30 and
/// 31 hold them still.
///
/// It was `mod streams` before that, and the rename was the milestone. The
/// rules below never mentioned a stream's representation — they are about a
/// name, a block and the paths out of it — but three of them matched
/// `Type::Stream` directly, so the one compile-time reclamation proof in the
/// language served exactly one type. The matches are a lookup in
/// [`vyrn_frontend::declared::Owned`], the same table `impl Owned for T` adds a row
/// to, so a user's file handle, transaction or reply obligation joins the
/// mechanism with no compiler change.
///
/// What the lookup answers is *whether*. [`vyrn_frontend::own::Linear`] answers
/// *which row*, and the only thing that reads it is the wording of the fix
/// menu: a stream is closed, a declared type is dropped, and offering either
/// menu for the other names a disposal that reclaims nothing.
///
/// # Why it is an AST walk and not a walk over the core
///
/// The two questions the rule asks — is this binding disposed on every path
/// out of its block, and is it mentioned again after it was — are questions
/// about the paths a READER wrote. The core has no `if` and no block: it has
/// a switch over arms and a site per exit, and the sentence a reader is owed
/// names the line the `let` is on. So the judgment reads the same tree the
/// reader reads, and the file it lives in is what says which judgment it is.
pub mod obligation {
    use std::collections::HashMap;

    use vyrn_frontend::ast::*;
    use vyrn_frontend::ast::{paths, stmt_mentions, sub_blocks};
    use vyrn_frontend::declared::Declared;
    use vyrn_frontend::diagnostics::Diagnostic;
    use vyrn_frontend::own::Linear;

    /// One live must-use binding, as a diagnostic about it needs it: the type
    /// spelled the way the program spelled it, and which row obliged it.
    #[derive(Clone)]
    struct Owed {
        ty: String,
        row: Linear,
    }

    /// What a straight-line statement list does to one live binding.
    #[derive(Clone, Copy, Default)]
    struct Scan {
        /// Disposed on every path that FALLS OUT of the list.
        disposed: bool,
        /// Nothing falls out — every path leaves via `return`/`break`/`continue`,
        /// so `disposed` says nothing about what follows.
        diverges: bool,
        /// Some path abandons it: a `return` that does not move it out, or two
        /// branches that disagree about whether it was disposed (one of those two
        /// paths is wrong whatever comes next, so it is reported here rather than
        /// left to a later statement to make look fine).
        leaked: bool,
        /// Disposed, then mentioned again on the same path.
        doubled: bool,
    }

    /// The judgment, for the slot `vyrn_frontend::own::install_must_use` fills
    /// — a program, and every obligation it breaks.
    ///
    /// The declarations are rebuilt here rather than handed in: the walk is
    /// asked once per check, and the caller's own [`Declared`] is inside the
    /// move check's run and not its to lend.
    pub fn judge(program: &Program) -> Vec<Diagnostic> {
        check(program, &Declared::new(program))
    }

    pub fn check(program: &Program, decl: &Declared) -> Vec<Diagnostic> {
        // Functions whose return type carries the obligation, with the rendering
        // the diagnostic quotes.
        //
        // The seeded rows are read the same way as the declared ones, which is
        // RFC-0094 M1's whole change here: `fromArray`, `fromStep` and
        // `unboxStream` were a three-name `match` in `owed_let`, and they are now
        // three return types. Each is `Stream<T>` over a bound `T`, so [`owed`]
        // quotes the type CONSTRUCTOR — plainly `Stream` — which is what the
        // `match` said and what this pass can say without types.
        let producers: HashMap<&str, Owed> = program
            .functions
            .iter()
            .chain(vyrn_frontend::prelude::all())
            .filter_map(|f| {
                Some((
                    f.name.as_str(),
                    owed(&f.ret, f.type_params.as_slice(), decl)?,
                ))
            })
            .collect();
        let mut out = Vec::new();
        for f in &program.functions {
            // A must-use parameter carries the obligation into the callee: the
            // caller discharged its own by moving it, and `fn sink(s: Stream<T>) {}`
            // must not be the hole that lets it evaporate.
            let mut live: Vec<(String, Owed)> = Vec::new();
            for p in &f.params {
                // A **receiver** does not, and the obligation would be circular
                // if it did: `impl Owned for Txn { fn release(self) }` IS the
                // disposal, so a rule that made it discharge its own receiver
                // before reading it would leave the declared release unwritable.
                // `self` is a keyword, so a parameter carrying that name is an
                // impl receiver and nothing else.
                if p.name == "self" {
                    continue;
                }
                if let Some(o) = owed(&p.ty, &[], decl) {
                    let s = scan(&f.body.stmts, &p.name, false);
                    report(&mut out, &s, f.line, &p.name, &o, &f.module);
                    live.push((p.name.clone(), o));
                }
            }
            block(&f.body, &mut live, &producers, &f.module, decl, &mut out);
        }
        for t in &program.tests {
            block(
                &t.body,
                &mut Vec::new(),
                &producers,
                &t.module,
                decl,
                &mut out,
            );
        }
        for b in &program.benches {
            block(
                &b.body,
                &mut Vec::new(),
                &producers,
                &b.module,
                decl,
                &mut out,
            );
        }
        out
    }

    /// The obligation `ty` carries, with the spelling a diagnostic quotes it by,
    /// or `None` where it carries none.
    ///
    /// `binders` are the type parameters in scope where `ty` was written. A
    /// generic producer — every std/stream combinator is one — returns
    /// `Stream<U>`, and quoting that at `let m = map(feed(), double)` names a
    /// type parameter the program never wrote. This pass has no types, so it
    /// cannot say `Stream<Int64>` either; it quotes the type CONSTRUCTOR, which
    /// is what it already said for `fromArray` and is an under-specification
    /// rather than a wrong name. The test is on the rendered spelling because a
    /// signature's type parameter is not reliably a `Type::Param` before the
    /// checker runs.
    fn owed(ty: &Type, binders: &[String], decl: &Declared) -> Option<Owed> {
        let row = decl.linear_kind(ty)?;
        let r = ty.to_string();
        let mentions = |p: &String| {
            r.split(|c: char| !c.is_alphanumeric() && c != '_')
                .any(|w| w == p.as_str())
        };
        let ty = match binders.iter().any(mentions) {
            true => r.split('<').next().unwrap_or(&r).to_string(),
            false => r,
        };
        Some(Owed { ty, row })
    }

    fn report(
        out: &mut Vec<Diagnostic>,
        s: &Scan,
        line: usize,
        name: &str,
        o: &Owed,
        module: &Option<String>,
    ) {
        let ty = &o.ty;
        // `Array<Txn>` reads as "an" and `Stream<Int64>` reads as "a". The
        // container spellings arrived with RFC-0092 M4, and the sentence has said
        // "is a" since RFC-0075.
        let art = match ty.chars().next() {
            Some('A' | 'E' | 'I' | 'O' | 'U' | 'a' | 'e' | 'i' | 'o' | 'u') => "an",
            _ => "a",
        };
        let msg = if s.doubled {
            format!("`{name}` is {art} `{ty}` and is disposed more than once")
        } else if s.leaked || !(s.disposed || s.diverges) {
            format!("`{name}` is {art} `{ty}` and is never disposed")
        } else {
            return;
        };
        let mut d = Diagnostic::error(line, 0, "movecheck", msg);
        // The two menus differ because the two disposals do. A stream's release
        // is pushed by its own lowering, so `drop` on one reclaims nothing; a
        // declared type has no `close` and is not iterable unless it says so.
        d.note = Some(match &o.row {
            Linear::Stream => format!(
                "a stream must be consumed with `for … in`, forwarded by returning it, \
                 or released with `close({name})` — on every path"
            ),
            Linear::Declared(by) if by == ty => format!(
                "`{ty}` declares `impl MustUse`, so a value of it must be handed on by \
                 name — passed to a call, forwarded by returning it, or released with \
                 `drop {name}` — on every path"
            ),
            // The container case (RFC-0092 M4). Naming both types is the whole
            // point: the reader wrote `Array<Txn>` and the row is `Txn`'s, and a
            // note that named only one of them sends them to the wrong file.
            Linear::Declared(by) => format!(
                "`{by}` declares `impl MustUse` and a `{ty}` holds one, so the container \
                 must be handed on by name — passed to a call, forwarded by returning it, \
                 or released with `drop {name}`, which releases each element — on every path"
            ),
        });
        d.file = module.clone();
        out.push(d);
    }

    /// Check one block: every must-use binding declared in it must be disposed
    /// on every path out of the REST of that block. `live` is the enclosing
    /// scopes' obliged names, needed only so `let t = s` is recognised as a move.
    fn block(
        b: &Block,
        live: &mut Vec<(String, Owed)>,
        producers: &HashMap<&str, Owed>,
        module: &Option<String>,
        decl: &Declared,
        out: &mut Vec<Diagnostic>,
    ) {
        let base = live.len();
        for (i, st) in b.stmts.iter().enumerate() {
            if let Stmt::Let {
                name,
                ty,
                value,
                line,
                ..
            } = st
            {
                if let Some(o) = owed_let(ty.as_ref(), value, live, producers, decl) {
                    // `false`: a `break` in the rest of THIS block leaves the block
                    // that declared the value, so it abandons it. Inside a loop
                    // nested below, a `break` only leaves that loop and control
                    // comes back here still owning it — which is what the flag
                    // distinguishes.
                    let s = scan(&b.stmts[i + 1..], name, false);
                    report(out, &s, *line, name, &o, module);
                    live.push((name.clone(), o));
                }
            }
            for sub in sub_blocks(st) {
                block(sub, live, producers, module, decl, out);
            }
        }
        live.truncate(base);
    }

    /// The obligation this `let` binds, if it binds one.
    fn owed_let(
        ty: Option<&Type>,
        value: &Expr,
        live: &[(String, Owed)],
        producers: &HashMap<&str, Owed>,
        decl: &Declared,
    ) -> Option<Owed> {
        // The must-use row, not a `Stream` match: an alias of a must-use type
        // carries the obligation its base does.
        if let Some(o) = ty.and_then(|t| owed(t, &[], decl)) {
            return Some(o);
        }
        match value {
            // The builtin producers arrive here through `producers` like every
            // declared one — RFC-0094 M1 deleted the three-name `match` that
            // stood in front of this arm.
            Expr::Call { name, .. } => producers.get(name.as_str()).cloned(),
            // `let t = s` moves the value; `t` inherits both the obligation and
            // the rendering, and the mention of `s` discharges `s`'s.
            Expr::Var { name, .. } => live.iter().find(|(l, _)| l == name).map(|(_, o)| o.clone()),
            // An arm is a path here as much as it is in [`scan`] (RFC-0095 M3,
            // which recorded this one as open). A branch hands on whichever arm
            // ran, so the binding inherits the obligation ANY arm carries: the
            // union is what makes `let t2 = match c { A => t, B => u }` a task
            // `t2` answers for, where before it was a task nothing answered for.
            //
            // The first arm that carries one answers for the rendering as well.
            // The checker has already made the arms agree on the type, so a
            // second arm would quote the same spelling.
            Expr::Match { arms, .. } => arms.iter().find_map(|a| {
                // A block arm (RFC-0118) yields nothing a binding could owe.
                a.body
                    .as_expr()
                    .and_then(|e| owed_let(None, e, live, producers, decl))
            }),
            Expr::IfExpr {
                then_branch,
                else_branch,
                ..
            } => owed_let(None, then_branch, live, producers, decl).or_else(|| {
                else_branch
                    .as_ref()
                    .and_then(|e| owed_let(None, e, live, producers, decl))
            }),
            _ => None,
        }
    }

    /// `nested_loop` is whether this list is (transitively) the body of a loop
    /// *inside* the block that declared the stream. It is the whole difference
    /// between the two things `break` can mean: leaving the declaring block, which
    /// abandons the stream, and leaving a loop below it, after which control
    /// returns to the declaring block still owning it.
    fn scan(stmts: &[Stmt], name: &str, nested_loop: bool) -> Scan {
        let mut acc = Scan::default();
        for (i, st) in stmts.iter().enumerate() {
            // The one place a disposal is decided: any mention of the binding in a
            // statement's own expressions moves it (`close(s)`, `for x in s`,
            // `sink(s)`, `let t = s`). A second mention anywhere in the rest of the
            // list is then a double disposal on this path.
            // `.0` is "some path through this statement disposes it", `.1` is
            // "every path does". They differ only where a `match` or an
            // if-expression branches (RFC-0095 M3).
            let none = (false, false);
            let moved = match st {
                // A write back INTO the binding is not a disposal: whatever the
                // right-hand side did with the value, the binding holds one
                // again when the statement ends. RFC-0092 M4 is what made this
                // matter. `pool.push(t)` is parsed as `pool = @push(pool, t)`
                // (see `hoist_mutating_receiver` and the `@push` arm beside it),
                // so with a container carrying its element's obligation, every
                // mutation of the pool read as "handed on by name" and the
                // obligation evaporated at the one statement the milestone
                // exists to catch.
                Stmt::Assign { name: n, value, .. } if n == name => none,
                Stmt::Assign { value, .. }
                | Stmt::Let { value, .. }
                | Stmt::SetField { value, .. }
                | Stmt::Expr(value) => paths(value, name),
                Stmt::IndexSet { index, value, .. } => {
                    let (i, v) = (paths(index, name), paths(value, name));
                    (i.0 || v.0, i.1 || v.1)
                }
                Stmt::If { cond: e, .. }
                | Stmt::While { cond: e, .. }
                | Stmt::IfLet { scrutinee: e, .. } => paths(e, name),
                Stmt::ForIn { iter, .. } => paths(iter, name),
                Stmt::Drop { name: n, .. } => (n == name, n == name),
                Stmt::Return { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => none,
                Stmt::Region { .. } => none,
            };
            // One arm disposes it and another does not. Whatever follows, one of
            // the two paths is wrong — the same authoring mistake two disagreeing
            // `if` blocks make below, reported the same way and at the same
            // point, rather than left to a later statement to make look fine.
            if moved.0 && !moved.1 {
                acc.leaked = true;
                return acc;
            }
            if moved.1 {
                acc.disposed = true;
                // The probe walks the REACHABLE rest: a statement after a
                // diverging one is unreachable and [`MoveCheck::block`] never
                // checks it, so a mention there is not a second disposal.
                let mut doubled = false;
                for s in &stmts[i + 1..] {
                    if stmt_mentions(s, name) {
                        doubled = true;
                        break;
                    }
                    if diverges(std::slice::from_ref(s)) {
                        break;
                    }
                }
                acc.doubled = doubled;
                // The disposal settles `disposed`, but the caller's branch merge
                // still needs to know whether anything falls out of this list —
                // `if c { close(s) return 1 }` disposes AND diverges, and reading
                // it as a plain fall-through made the merge see two branches
                // disagreeing when only one of them continues.
                acc.diverges = diverges(&stmts[i + 1..]);
                return acc;
            }
            match st {
                Stmt::Return { value, .. } => {
                    // Forwarding by returning it is a disposal; returning anything
                    // else leaves the function still owning it. `paths` and not
                    // `mentions`, for the reason it exists: `return match p {
                    // Some(n) => t, None => 0 }` forwards the task on one path
                    // and abandons it on the other (RFC-0095 M3).
                    acc.diverges = true;
                    acc.leaked |= !value.as_ref().is_some_and(|e| paths(e, name).1);
                    return acc;
                }
                // Inside a loop below the declaring block, `break`/`continue` land
                // back in the declaring block still owning the stream — nothing to
                // report. At the declaring block's own level they leave it, so an
                // undisposed stream is abandoned exactly as by a bare `return`.
                Stmt::Break { .. } | Stmt::Continue { .. } => {
                    acc.diverges = true;
                    acc.leaked |= !nested_loop;
                    return acc;
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
                    let t = scan(&then_block.stmts, name, nested_loop);
                    let e = match else_block {
                        Some(b) => scan(&b.stmts, name, nested_loop),
                        None => Scan::default(),
                    };
                    acc.leaked |= t.leaked || e.leaked;
                    acc.doubled |= t.doubled || e.doubled;
                    match (t.diverges, e.diverges) {
                        (true, true) => {
                            acc.diverges = true;
                            return acc;
                        }
                        (true, false) => {
                            if e.disposed {
                                acc.disposed = true;
                                return acc;
                            }
                        }
                        (false, true) => {
                            if t.disposed {
                                acc.disposed = true;
                                return acc;
                            }
                        }
                        (false, false) => {
                            if t.disposed && e.disposed {
                                acc.disposed = true;
                                return acc;
                            }
                            // The branches DISAGREE. Whatever follows, one of the
                            // two paths is wrong: if nothing disposes later the
                            // disposing branch is the only correct one, and if
                            // something does, it double-frees on that branch. Both
                            // are the same authoring mistake, so it is reported
                            // once, here, rather than turned into a puzzle by a
                            // later statement that makes the merge look clean.
                            acc.leaked |= t.disposed != e.disposed;
                        }
                    }
                }
                // A loop body may run zero times, so a disposal inside it never
                // discharges the obligation on the fall-through — and disposing on
                // one iteration would dispose again on the next, which is the same
                // shape `check_loop_reuse` already rejects for `consume`.
                Stmt::While { body, .. } | Stmt::ForIn { body, .. } => {
                    let b = scan(&body.stmts, name, true);
                    acc.leaked |= b.leaked || b.disposed;
                    acc.doubled |= b.doubled;
                }
                Stmt::Region { body, .. } => {
                    let b = scan(&body.stmts, name, nested_loop);
                    acc.leaked |= b.leaked;
                    acc.doubled |= b.doubled;
                    if b.disposed || b.diverges {
                        acc.disposed = b.disposed;
                        acc.diverges = b.diverges;
                        return acc;
                    }
                }
                _ => {}
            }
        }
        acc
    }

    /// Whether every path out of `stmts` leaves via `return`/`break`/`continue`
    /// (or `panic`, which diverges for the same reason it does above).
    ///
    /// The same question `MoveCheck::block` answers as its return value; asked
    /// again here because [`scan`] stops at the disposal and so never reaches the
    /// `return` that follows it.
    fn diverges(stmts: &[Stmt]) -> bool {
        stmts.iter().any(|s| match s {
            Stmt::Return { .. } | Stmt::Break { .. } | Stmt::Continue { .. } => true,
            Stmt::Expr(Expr::Call { name, .. }) => vyrn_frontend::ast::is_panic(name),
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
                diverges(&then_block.stmts)
                    && else_block.as_ref().is_some_and(|b| diverges(&b.stmts))
            }
            Stmt::Region { body, .. } => diverges(&body.stmts),
            _ => false,
        })
    }
}
