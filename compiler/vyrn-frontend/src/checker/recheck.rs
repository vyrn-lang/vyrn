//! Per-body reuse for a host that rechecks per function: the editor, which
//! arms [`super::record_reads`]. `vyrn check` opens no [`Session`].
//!
//! The data. Per source body (a function, an `impl` projection, a `test`, a
//! `bench`), keyed by its text, an [`Entry`] holds what one check of that
//! text read and what it gave: its diagnostics, its record rows, its root
//! bindings, its stored-function-value facts, its `derive` sites and its read
//! rows. A read is a declaration by kind and name, or a name missed, with the
//! fingerprint of what the lookup answered ([`Session::answer`]).
//!
//! The rule. A body is checked again when no entry holds its text, or when
//! one of its reads answers differently in this check: a changed signature,
//! type, module-state type or variant, or a miss that a new declaration turns
//! into a hit. Otherwise the entry is replayed in the body's place, so the
//! check gives the output a check of every body gives, byte for byte. A
//! body's text holds its position, so an edit that moves a line rechecks
//! every body after it in that module. Module state, type declarations,
//! contracts and protocols are checked on every check.
//!
//! What no read row records, [`world`] fingerprints, and an entry answers
//! only under the same fingerprint: the other modules' texts, which also
//! decide how the linker rewrites their bodies, the names of their protocols
//! and contracts, the root's protocols and contracts, every `impl` head, the
//! root's projection bodies, the host, the surface shadows and the `consume`
//! slots of stored function signatures. A rename apart of another module's
//! declaration changes a name its readers read, so their reads answer
//! differently. A body whose record rows name a node of another unit (an
//! inlined projection's expansion) or that read a protocol is checked on
//! every check.
//!
//! A body's typing reads the declarations and the module state, never what
//! an earlier body left on the checker: the typing workers take bodies in any
//! order and give one output (`tests/parallel.rs`). So a replayed body leaves
//! the next body's typing as it was. Function bodies are replayed or queued
//! for the workers on the loading thread ([`Checker::replayed`]), and the
//! merge stores each typed one ([`Checker::store`]); projection, test and
//! bench bodies take [`Checker::body`].
//!
//! A key or a fingerprint is 64 bits of SipHash; a collision between two
//! bodies' keys would replay the other body's result.

use std::cell::{OnceCell, RefCell};
use std::collections::{HashMap, HashSet};
use std::fmt::Write as _;
use std::hash::{Hash as _, Hasher as _};

use crate::ast::SourceBody;
use crate::ast::{Capability, DeclId, DeclKind, Function, Key, NamedBlock, Program, ScopeId};
use crate::diagnostics::Diagnostic;

use super::{Checker, Recorded, Typed};

/// How many sessions a world outlives unopened. A keystroke opens one per
/// program it checks: the root's, the synthesis's and each generator's.
const KEEP: u64 = 64;

/// How many opens of its world an entry outlives unread: one for a check
/// that reads a part of the program (the synthesis's), one for a body edited
/// away.
const STALE: u64 = 2;

thread_local! {
    static CACHE: RefCell<Cache> = RefCell::new(Cache::default());
}

/// The next [`Entry::serial`].
static SERIALS: std::sync::atomic::AtomicU64 = std::sync::atomic::AtomicU64::new(0);

#[derive(Default)]
struct Cache {
    /// Sessions closed on this thread.
    sessions: u64,
    /// Per [`world`], the session that last opened it and how many did.
    worlds: HashMap<u64, (u64, u64)>,
    entries: HashMap<u64, Entry>,
    /// `(checked, replayed)` bodies since [`tally`] last read it.
    tally: (u64, u64),
}

/// Returns how many bodies this thread's sessions checked and replayed since
/// the last call, and starts the count again.
pub fn tally() -> (u64, u64) {
    CACHE.with(|c| std::mem::take(&mut c.borrow_mut().tally))
}

/// What one check of a body's text read and gave.
struct Entry {
    /// The [`world`] the entry answers under.
    world: u64,
    /// How many sessions had opened `world` when the entry was last stored or
    /// replayed.
    seen: u64,
    /// The unit the body was numbered as when checked; its record rows are
    /// keyed in it.
    unit: u32,
    /// Unique per stored entry in the process ([`Recorded::entries`]).
    serial: u64,
    reads: Vec<Read>,
    /// What typing the body gave, its read rows in `reads`.
    typed: Typed,
}

/// A lookup a body made, and the fingerprint of its answer.
struct Read {
    looked: Looked,
    /// The hash of `looked`, which [`Session::answers`] is keyed by.
    id: u64,
    answer: u64,
}

#[derive(Clone, PartialEq, Eq, Hash)]
enum Looked {
    Decl(DeclKind, String),
    /// A miss, with the reading module ([`ScopeId`]).
    Miss(Option<String>, String),
}

/// The text a body is keyed by.
#[derive(Clone, Copy)]
pub(super) enum Text<'a> {
    Fn(&'a Function),
    Block(&'a NamedBlock),
}

impl Text<'_> {
    fn unit(self) -> u32 {
        match self {
            Text::Fn(f) => f.body.id.0.unit(),
            Text::Block(b) => b.body.id.0.unit(),
        }
    }
}

/// One recording check's view of the cache.
pub(super) struct Session<'a> {
    program: &'a Program,
    /// [`world`] of the program.
    world: u64,
    /// Each answer by [`Read::id`], fingerprinted once per check.
    answers: RefCell<HashMap<u64, Option<u64>>>,
    /// Each read's row in this check by [`Read::id`].
    keys: RefCell<HashMap<u64, Key>>,
    /// Variant names by [`DeclId::index`], filled on first use.
    variants: OnceCell<Vec<&'a str>>,
}

impl<'a> Session<'a> {
    /// The session of a check of `program` that records read rows.
    pub(super) fn open(
        program: &'a Program,
        caps_by_sig: &HashMap<String, Vec<Capability>>,
    ) -> Session<'a> {
        let world = world(program, caps_by_sig);
        CACHE.with(|c| {
            let c = &mut *c.borrow_mut();
            let (last, opens) = c.worlds.entry(world).or_default();
            *last = c.sessions;
            *opens += 1;
        });
        Session {
            program,
            world,
            answers: RefCell::default(),
            keys: RefCell::default(),
            variants: OnceCell::new(),
        }
    }

    /// The key of `body`: its kind and its text under this [`world`]. A body
    /// of a module with a content hash is keyed by that hash and its name
    /// and position; a body of the root, or one synthesis wrote, by its
    /// whole text.
    fn key(&self, body: SourceBody, text: Text<'_>) -> u64 {
        let mut h = Sip::default();
        let tag = match body {
            SourceBody::Fn(_) => 'f',
            SourceBody::Place(_) => 'p',
            SourceBody::Test(_) => 't',
            SourceBody::Bench(_) => 'b',
            SourceBody::Global(_) => 'g',
            SourceBody::TypeDecl(_) => 'd',
        };
        let hashed =
            |m: &Option<String>| m.as_ref().and_then(|m| self.program.module_hashes.get(m));
        let _ = write!(h, "{}|{tag}|", self.world);
        let _ = match text {
            Text::Fn(f) => match hashed(&f.module) {
                Some(mh) => write!(h, "{:?}|{mh}|{}|{}|{}", f.module, f.name, f.line, f.col),
                None => {
                    head(&mut h, f);
                    write!(h, "{:?}|{}|{}|{:?}", f.params, f.line, f.col, f.body)
                }
            },
            Text::Block(b) => match hashed(&b.module) {
                Some(mh) => write!(h, "{:?}|{mh}|{}|{}", b.module, b.name, b.line),
                None => write!(h, "{:?}|{:?}|{}|{:?}", b.module, b.name, b.line, b.body),
            },
        };
        h.0.finish()
    }

    /// The fingerprint of what looking up `looked`, whose hash is `id`,
    /// answers in `c`'s check, or `None` for an answer no fingerprint states.
    fn answer(&self, c: &Checker, id: u64, looked: &Looked) -> Option<u64> {
        if let Some(a) = self.answers.borrow().get(&id) {
            return *a;
        }
        let a = self.answer_now(c, looked);
        self.answers.borrow_mut().insert(id, a);
        a
    }

    fn answer_now(&self, c: &Checker, l: &Looked) -> Option<u64> {
        let mut h = Sip::default();
        let _ = match l {
            Looked::Decl(DeclKind::Fn, n) => {
                head(&mut h, &c.functions[c.fn_decls.get(n)?.index()]);
                Ok(())
            }
            Looked::Decl(DeclKind::Type, n) => {
                let (_, t) = c.types.get(n)?;
                let (name, module) = (&t.name, &t.module);
                let (params, base, predicate) = (&t.type_params, &t.base, &t.predicate);
                let exported = t.exported;
                write!(
                    h,
                    "{name}|{exported}|{module:?}|{params:?}|{base:?}|{predicate:?}"
                )
            }
            Looked::Decl(DeclKind::Global, n) => {
                let (d, b) = c.globals.borrow().get(n).cloned()?;
                let g = &self.program.globals[d.index()];
                let (name, mutable, ty, module) = (&g.name, g.mutable, &g.ty, &g.module);
                let bound = (&b.ty, b.mutable);
                write!(h, "{name}|{mutable}|{ty:?}|{module:?}|{bound:?}")
            }
            Looked::Decl(DeclKind::Variant, n) => {
                let v = c.variants.get(n)?;
                write!(h, "{}|{:?}", v.enum_name, v.payload)
            }
            Looked::Decl(DeclKind::Protocol, _) => return None,
            // A miss names no table, so a declaration of the name in any of
            // them moves it.
            Looked::Miss(_, n) => {
                let fs = c.fn_decls.contains_key(n);
                let ts = c.types.contains_key(n);
                let gs = c.globals.borrow().contains_key(n);
                let vs = c.variants.contains_key(n);
                write!(h, "{fs}{ts}{gs}{vs}")
            }
        };
        Some(h.0.finish())
    }

    /// The reads of `rows`, each once in the order read, or `None` when one
    /// has no answer a fingerprint states.
    fn reads(&self, c: &Checker, rows: &[(SourceBody, Key)]) -> Option<Vec<Read>> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for (_, k) in rows {
            let looked = match k {
                Key::Decl(d) => Looked::Decl(d.kind, self.name_of(*d)?.to_string()),
                Key::Miss(s, n) => Looked::Miss(s.module.clone(), n.clone()),
            };
            let mut h = std::hash::DefaultHasher::new();
            looked.hash(&mut h);
            let id = h.finish();
            if seen.insert(id) {
                let answer = self.answer(c, id, &looked)?;
                out.push(Read { looked, id, answer });
            }
        }
        Some(out)
    }

    fn name_of(&self, d: DeclId) -> Option<&'a str> {
        let p = self.program;
        Some(match d.kind {
            DeclKind::Fn => &p.functions[d.index()].name,
            DeclKind::Type => &p.type_decls[d.index()].name,
            DeclKind::Global => &p.globals[d.index()].name,
            DeclKind::Variant => self.variants.get_or_init(|| {
                (p.type_decls.iter())
                    .filter_map(|t| crate::types::declared_variants(&t.base))
                    .flatten()
                    .map(|v| v.name.as_str())
                    .collect()
            })[d.index()],
            DeclKind::Protocol => return None,
        })
    }

    /// The row `r` records in `c`'s check. The caller has seen `r` answer
    /// as it did when stored, so a declaration it names exists, and it is no
    /// protocol, which [`Session::answer`] does not fingerprint.
    fn key_of(&self, c: &Checker, r: &Read) -> Key {
        if let Some(k) = self.keys.borrow().get(&r.id) {
            return k.clone();
        }
        let k = match &r.looked {
            Looked::Decl(kind, n) => Key::Decl(match kind {
                DeclKind::Fn => c.fn_decls[n],
                DeclKind::Type => c.types[n].0,
                DeclKind::Global => c.globals.borrow()[n].0,
                DeclKind::Variant => c.variants[n].id,
                DeclKind::Protocol => unreachable!("a protocol read is never stored"),
            }),
            Looked::Miss(m, n) => Key::Miss(ScopeId { module: m.clone() }, n.clone()),
        };
        self.keys.borrow_mut().insert(r.id, k.clone());
        k
    }
}

impl Drop for Session<'_> {
    /// Drops every world no session opened for [`KEEP`] sessions, and every
    /// entry its world's sessions did not read for [`STALE`] opens.
    fn drop(&mut self) {
        CACHE.with(|c| {
            let c = &mut *c.borrow_mut();
            c.sessions += 1;
            let now = c.sessions;
            c.worlds.retain(|_, (last, _)| *last + KEEP >= now);
            let worlds = &c.worlds;
            (c.entries).retain(|_, e| worlds.get(&e.world).is_some_and(|w| w.1 <= e.seen + STALE));
        });
    }
}

impl Checker<'_> {
    /// The result of `body` that an entry holds, when this checker has a
    /// [`Session`] and every read of the entry answers as it did, with its
    /// record rows keyed in the body's unit and its read rows in this check's
    /// ids. `None` means the caller types the body.
    pub(super) fn replayed(&self, body: SourceBody, text: Text<'_>) -> Option<Typed> {
        let s = self.recheck.as_ref()?;
        let key = s.key(body, text);
        CACHE.with(|cache| {
            let cache = &mut *cache.borrow_mut();
            let e = (cache.entries.get_mut(&key)).filter(|e| {
                (e.reads.iter()).all(|r| s.answer(self, r.id, &r.looked) == Some(r.answer))
            })?;
            e.seen = cache.worlds[&s.world].1;
            cache.tally.1 += 1;
            let unit = text.unit();
            let record = (e.typed.record.as_ref()).map(|r| match e.unit == unit {
                true => r.clone(),
                false => in_unit(r, unit),
            });
            let reads = e.reads.iter().map(|r| (body, s.key_of(self, r))).collect();
            let record = record.map(|r| Recorded {
                entries: vec![(body, e.serial)],
                ..r
            });
            Some(Typed {
                record,
                reads,
                ..e.typed.clone()
            })
        })
    }

    /// Stores `t`, what typing `body` gave, as an entry, unless a read or a
    /// record row cannot follow the body to another check, and names the
    /// entry in `t`'s record.
    pub(super) fn store(&self, body: SourceBody, text: Text<'_>, t: &mut Typed) {
        let Some(s) = &self.recheck else {
            return;
        };
        CACHE.with(|c| c.borrow_mut().tally.0 += 1);
        let unit = text.unit();
        let rows = t.record.iter().flat_map(|r| {
            (r.node_types.keys())
                .chain(r.joins.keys())
                .chain(r.node_substs.keys())
                .chain(r.calls.keys())
        });
        if !rows.map(|k| k.unit()).all(|u| u == unit) {
            return;
        }
        let Some(reads) = s.reads(self, &t.reads) else {
            return;
        };
        let typed = Typed {
            reads: Vec::new(),
            ..t.clone()
        };
        CACHE.with(|c| {
            let c = &mut *c.borrow_mut();
            let seen = c.worlds[&s.world].1;
            let (world, key) = (s.world, s.key(body, text));
            let serial = SERIALS.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
            let e = Entry {
                world,
                seen,
                unit,
                serial,
                reads,
                typed,
            };
            c.entries.insert(key, e);
            if let Some(r) = &mut t.record {
                r.entries.push((body, serial));
            }
        });
    }

    /// Checks one body on this thread: `check` appends its diagnostics to
    /// `out` and writes the rest on the checker. A body [`Checker::replayed`]
    /// answers is not checked.
    pub(super) fn body(
        &self,
        body: SourceBody,
        text: Text<'_>,
        out: &mut Vec<Diagnostic>,
        check: impl FnOnce(&mut Vec<Diagnostic>),
    ) {
        if self.recheck.is_none() {
            self.reading(body);
            return check(out);
        }
        if let Some(t) = self.replayed(body, text) {
            return out.extend(self.absorb(t));
        }
        let aside = self.taken(Vec::new());
        self.reading(body);
        let mut diags = Vec::new();
        check(&mut diags);
        let mut t = self.taken(diags);
        self.put(aside);
        self.store(body, text, &mut t);
        out.extend(self.absorb(t));
    }
}

/// `r` with every row keyed in `unit`: the same nodes of a body numbered as
/// another unit.
fn in_unit(r: &Recorded, unit: u32) -> Recorded {
    let at = |k: &crate::ast::NodeId| k.in_unit(unit);
    Recorded {
        node_types: (r.node_types.iter())
            .map(|(k, t)| (at(k), t.clone()))
            .collect(),
        joins: (r.joins.iter()).map(|(k, t)| (at(k), t.clone())).collect(),
        node_substs: (r.node_substs.iter())
            .map(|(k, s)| (at(k), s.clone()))
            .collect(),
        calls: (r.calls.iter()).map(|(k, d)| (at(k), d.clone())).collect(),
        stored: r.stored.clone(),
        reads: r.reads.clone(),
        entries: Vec::new(),
    }
}

/// The fingerprint of what a body reads that no read row records.
fn world(p: &Program, caps_by_sig: &HashMap<String, Vec<Capability>>) -> u64 {
    let mut h = Sip::default();
    let mut shadows: Vec<_> = p.surface_shadows.iter().collect();
    shadows.sort();
    let mut consume: Vec<_> = (caps_by_sig.iter())
        .filter(|(_, cs)| cs.contains(&Capability::Consume))
        .collect();
    consume.sort_by(|a, b| a.0.cmp(b.0));
    let (hashes, host) = (&p.module_hashes, p.host);
    let _ = write!(h, "{hashes:?}|{host:?}|{shadows:?}|{consume:?}|");
    for x in &p.protocols {
        let _ = match x.module {
            Some(_) => write!(h, "{}|", x.name),
            None => write!(h, "{x:?}|"),
        };
    }
    for x in &p.contracts {
        let _ = match x.module {
            Some(_) => write!(h, "{}|", x.name),
            None => write!(h, "{x:?}|"),
        };
    }
    for i in &p.impls {
        let mut bounds: Vec<_> = i.type_bounds.iter().collect();
        bounds.sort();
        let (protocol, params, ty, assoc) = (&i.protocol, &i.type_params, &i.ty, &i.assoc);
        let _ = write!(h, "{protocol}|{params:?}|{bounds:?}|{ty:?}|{assoc:?}|");
        for f in &i.methods {
            head(&mut h, f);
        }
        for f in i.places.iter().filter(|f| f.module.is_none()) {
            head(&mut h, f);
            let _ = write!(h, "{:?}|{:?}", f.params, f.body);
        }
    }
    h.0.finish()
}

/// Writes what a reader of `f` reads: all of it but its body, its doc and
/// its positions.
fn head(h: &mut Sip, f: &Function) {
    let mut bounds: Vec<_> = f.type_bounds.iter().collect();
    bounds.sort();
    let params: Vec<_> = (f.params.iter())
        .map(|p| (&p.name, p.capability, &p.ty))
        .collect();
    let flags = [
        f.exported,
        f.is_extern,
        f.is_export_extern,
        f.is_gen,
        f.is_mut,
    ];
    let (module, name, tps, ret) = (&f.module, &f.name, &f.type_params, &f.ret);
    let _ = write!(
        h,
        "{module:?}|{name}|{tps:?}|{bounds:?}|{params:?}|{ret:?}|{flags:?}|"
    );
}

/// A hasher that text is written into, so no fingerprint allocates its text.
#[derive(Default)]
struct Sip(std::hash::DefaultHasher);

impl std::fmt::Write for Sip {
    fn write_str(&mut self, s: &str) -> std::fmt::Result {
        self.0.write(s.as_bytes());
        Ok(())
    }
}
