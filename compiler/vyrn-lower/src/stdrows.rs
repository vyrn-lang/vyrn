//! Std's stored rows: what each given function of a std module
//! ([`Body::Given`]) leaves its importers, written by the module's own load
//! and read by a load that gives std bodies: `vyrn check`
//! ([`loader::std_given`]), and an emit host for what only gen fns reach.
//!
//! A row is what the judgment memo keeps of a body ([`Judgment`]): its frames
//! as the effect judgment walks them, the answer the kernel read, its fact
//! summary and the calls the worklist follows out of it, with what the
//! generation fence reads of it ([`Fenced`]). Each module adds the function
//! values its bodies store and pass, which join the program's
//! (`StoredFnEffects::every_source`).
//!
//! An entry is one module's rows, the rows of the modules its generator
//! imports wrote included. It sits beside the generator cache's entries,
//! keyed by the compiler's identity, the module's key and its text hash,
//! authenticated by the per-user secret, and records every file its load read
//! ([`Inputs`]). A foreign, damaged or stale entry reads as a miss, and the
//! miss writes it again. `VYRN_NO_GEN_CACHE` reads and writes none: each load
//! writes the entries it needs in memory, once per thread. A read indexes an
//! entry's rows by the length each row is written after, and decodes a row
//! only for a function the program links.
//!
//! A row names a declaration by its written name and module, so a program
//! that renames one apart reads it under the program's name. [`Given::read`]
//! is the one constructor of the table the placer and the worklist read, so
//! every given function of the program has a row there.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::ops::Range;
use std::sync::Arc;

use vyrn_frontend::ast::{Body, Function, Program, Type};
use vyrn_frontend::checker::{Fenced, Recorded, StoredLambda, StoredSource};
use vyrn_frontend::core::check::{Fact, Word};
use vyrn_frontend::diagnostics::Diagnostic;
use vyrn_frontend::effects::{Call, Effect, Effects, Mark, Walked};
use vyrn_frontend::gen::GenEngine;
use vyrn_frontend::loader::{self, Inputs, LoadOptions, ModuleResolver};
use vyrn_frontend::movecheck::{Judgment, JudgmentKey};
use vyrn_frontend::session::Session;

/// The format word that opens an entry.
const TAG: &str = "stdrows4";

/// The stored rows of a program's given functions, each decoded once from
/// its module's entry and named as the program names it.
#[derive(Default)]
pub struct Given {
    /// By linked name: the row of each function of the program whose module's
    /// entry holds one, the given functions and a std module's impl methods.
    rows: HashMap<String, Stored>,
    /// What the generation fence reads of each given function, by linked
    /// name; empty for a program with no written gen fn, whose fence reads
    /// no given body.
    pub(crate) fenced: BTreeMap<String, Fenced>,
    /// The function values the given bodies store and pass.
    pub(crate) sources: Vec<StoredSource>,
}

impl Given {
    /// The rows of every given function of `program`, read from their
    /// modules' entries and written where an entry is missing or stale.
    ///
    /// # Errors
    ///
    /// The refusals of a std module that does not check as the root of its
    /// own load, and an internal error where its entry holds no row for a
    /// given function.
    pub(crate) fn read(
        program: &Program,
        opts: &LoadOptions,
        resolver: &dyn ModuleResolver,
        engine: Option<&GenEngine>,
    ) -> Result<Given, Vec<Diagnostic>> {
        let mut out = Given::default();
        let given: Vec<&Function> = (program.functions.iter())
            .filter(|f| matches!(f.body, Body::Given(_)))
            .collect();
        if given.is_empty() {
            return Ok(out);
        }
        let _p = vyrn_frontend::prof::phase("std rows: read");
        // Each declaration's name in `program`, by its written name and module.
        let linked: HashMap<(&str, &str), &str> = (program.spellings.decls.iter())
            .map(|(l, (w, m))| ((w.as_str(), m.as_str()), l.as_str()))
            .collect();
        let mut entries: BTreeMap<&str, (Arc<Entry>, HashMap<String, String>)> = BTreeMap::new();
        for f in &given {
            let m = f.module.as_deref().expect("a given function is imported");
            let owner = loader::generated_importer(m).unwrap_or(m);
            if !entries.contains_key(owner) {
                let e = entry(owner, program, opts, resolver, engine)?;
                let names: HashMap<String, String> = (e.homes())
                    .filter_map(|(own, w, m)| {
                        let to = *linked.get(&(w, m)).unwrap_or(&w);
                        (to != own).then(|| (own.to_string(), to.to_string()))
                    })
                    .collect();
                for s in &e.sources {
                    let mut s = s.clone();
                    source_names(&mut s, &mut |n| rename(&names, n));
                    out.sources.push(s);
                }
                entries.insert(owner, (e, names));
            }
        }
        for f in &program.functions {
            let Some(m) = f.module.as_deref() else {
                continue;
            };
            let owner = loader::generated_importer(m).unwrap_or(m);
            let Some((e, names)) = entries.get(owner) else {
                continue;
            };
            match e.row(m, program.spellings.written(&f.name)) {
                Some(mut s) => {
                    each_name(&mut s, &mut |n| rename(names, n));
                    out.rows.insert(f.name.clone(), s);
                }
                None if matches!(f.body, Body::Given(_)) => return Err(vec![missing(&f.name, m)]),
                None => {}
            }
        }
        once_each(&mut out.sources);
        if program
            .functions
            .iter()
            .any(|f| f.is_gen && matches!(f.body, Body::Written(_)))
        {
            for f in &given {
                let s = &out.rows[&f.name];
                out.fenced.insert(f.name.clone(), s.fenced.clone());
            }
        }
        Ok(out)
    }

    /// The rows `program` carries ([`Program::given`]); none for a program no
    /// load gave a body.
    pub(crate) fn of(program: &Program) -> Arc<Given> {
        let rows = program
            .given
            .0
            .clone()
            .and_then(|r| r.downcast::<Given>().ok());
        rows.unwrap_or_default()
    }

    /// `record`, the checker's record of the program, with the function values
    /// the given bodies store and pass.
    pub(crate) fn record(&self, mut record: Recorded) -> Recorded {
        record.stored.sources.extend(self.sources.iter().cloned());
        record
    }

    /// The row the placer serves the given function `func`, with the memo key
    /// it is served under. A written one with a row, an impl method, is served
    /// only to a host that emits nothing, because an emitter walks the body
    /// the placer states.
    pub(crate) fn serve(
        &self,
        program: &Program,
        func: &Function,
    ) -> Option<(JudgmentKey, Judgment)> {
        if func.body.written().is_some() && vyrn_frontend::movecheck::emitting() {
            return None;
        }
        let j = &self.rows.get(&func.name)?.judgment;
        let judgment = Judgment {
            verdict: j.verdict.clone(),
            frames: j.frames.clone(),
            state: j.state.clone(),
            summary: j.summary.clone(),
            calls: Vec::new(),
        };
        let m = func.module.clone()?;
        let hash = program.module_hashes.get(&m).cloned().unwrap_or_default();
        Some(((m, hash, func.name.clone()), judgment))
    }

    /// The calls the worklist follows out of the given function `f`.
    ///
    /// # Panics
    ///
    /// Where `f` has no row: [`Given::read`] gives every given function one,
    /// and only the placer's lowering walks a given body.
    pub(crate) fn calls(&self, f: &Function) -> Vec<(String, HashMap<String, Type>)> {
        let s = self.rows.get(&f.name);
        let s = s.unwrap_or_else(|| panic!("the given `{}` has no stored row", f.name));
        (s.judgment.calls.iter())
            .map(|(c, subst)| (c.clone(), subst.iter().cloned().collect()))
            .collect()
    }
}

/// Renames `n` to the name `names` gives it, if any.
fn rename(names: &HashMap<String, String>, n: &mut String) {
    if let Some(to) = names.get(n.as_str()) {
        n.clone_from(to);
    }
}

/// The refusal of a program whose given function `name` of module `m` has no
/// row: the module's own load wrote none for it.
fn missing(name: &str, m: &str) -> Diagnostic {
    let d = Diagnostic::error(
        0,
        0,
        "check",
        format!("internal error: `{m}` stores no row for `{name}`"),
    );
    Diagnostic {
        file: Some(m.to_string()),
        ..d
    }
}

thread_local! {
    /// The entries this thread read or wrote, by key: the cache key, or the
    /// module and its text hash where `VYRN_NO_GEN_CACHE` keys no file. Either
    /// names the module and its text, so an entry here is never stale.
    static ENTRIES: RefCell<HashMap<String, Arc<Entry>>> = RefCell::new(HashMap::new());
}

/// The entry of std module `owner` as `program` links it: kept, read or
/// written.
fn entry(
    owner: &str,
    program: &Program,
    opts: &LoadOptions,
    resolver: &dyn ModuleResolver,
    engine: Option<&GenEngine>,
) -> Result<Arc<Entry>, Vec<Diagnostic>> {
    let hash = program.module_hashes.get(owner).map_or("", String::as_str);
    // The cache key names the compiler, so only a load that reads or writes
    // the cache asks for its identity.
    let off = loader::gen_cache_off();
    let key = match off {
        true => format!("{owner}\0{hash}"),
        false => entry_key(owner, hash),
    };
    if let Some(e) = ENTRIES.with(|c| c.borrow().get(&key).cloned()) {
        return Ok(e);
    }
    let cached = (!off)
        .then(|| resolver.gen_cache_get(&key))
        .flatten()
        .and_then(|text| Entry::read(&key, text))
        .filter(|e| e.fresh(program, resolver));
    let e = match cached {
        Some(e) => e,
        None => {
            let body = Entry::written(owner, opts, resolver, engine)?.encode();
            if !off {
                let tag = loader::gen_cache_tag(&key, body.as_bytes());
                resolver.gen_cache_put(&key, &format!("{TAG} {tag}\n{body}"));
            }
            Entry::parse(body).ok_or_else(|| vec![missing("its own rows", owner)])?
        }
    };
    let e = Arc::new(e);
    ENTRIES.with(|c| c.borrow_mut().insert(key, e.clone()));
    Ok(e)
}

/// The cache key of module `m`'s entry at text hash `hash`.
fn entry_key(m: &str, hash: &str) -> String {
    let id = vyrn_frontend::gen::compiler_identity();
    let seed = format!("{TAG}\0{id}\0{m}\0{hash}");
    format!("rows-{}", vyrn_frontend::hash::sha256_hex(seed.as_bytes()))
}

/// One given function's row as its module's own load names it.
#[derive(Clone)]
struct Stored {
    judgment: Judgment,
    fenced: Fenced,
}

/// Calls `f` with every name `s` holds: the judgment's frames, answers,
/// summary and calls, and the fence's calls. [`Entry::written`] records each
/// one's declaration, and [`Given::read`] renames each through it.
fn each_name(s: &mut Stored, f: &mut impl FnMut(&mut String)) {
    let j = &mut s.judgment;
    for w in &mut j.frames {
        frame_name(&mut w.name, f);
        let writes = std::mem::take(&mut w.writes);
        w.writes = (writes.into_iter())
            .map(|mut n| {
                f(&mut n);
                n
            })
            .collect();
        for c in &mut w.calls {
            f(&mut c.callee);
            if let Some(t) = &mut c.through {
                type_names(t, f);
            }
        }
    }
    for (c, gs) in j.state.iter_mut().flatten() {
        f(c);
        gs.iter_mut().for_each(&mut *f);
    }
    for fact in &mut j.summary {
        type_names(&mut fact.order, f);
    }
    for (c, subst) in &mut j.calls {
        f(c);
        subst.iter_mut().for_each(|(_, t)| type_names(t, f));
    }
    s.fenced.calls.iter_mut().for_each(f);
}

/// Calls `f` with every name a function value source holds.
fn source_names(s: &mut StoredSource, f: &mut impl FnMut(&mut String)) {
    type_names(&mut s.sig, f);
    s.named.iter_mut().for_each(&mut *f);
    if let Some(l) = &mut s.lambda {
        f(&mut l.defined_in);
    }
}

/// Calls `f` with the function a frame's name names: the whole name, or the
/// part before a lambda frame's `@lambda:`.
fn frame_name(name: &mut String, f: &mut impl FnMut(&mut String)) {
    match name.split_once("@lambda:") {
        Some((owner, at)) => {
            let (mut owner, at) = (owner.to_string(), at.to_string());
            f(&mut owner);
            *name = format!("{owner}@lambda:{at}");
        }
        None => f(name),
    }
}

/// Calls `f` with every type name in `t`.
fn type_names(t: &mut Type, f: &mut impl FnMut(&mut String)) {
    if let Type::Named(n) | Type::App(n, _) = t {
        f(n);
    }
    t.children_mut().for_each(|c| type_names(c, f));
}

/// What a module's own load leaves its importers, as [`Written::encode`]
/// writes it after the entry's tag line.
struct Written {
    /// The files the own load read.
    inputs: Inputs,
    /// The text hash of each generated module the own load linked, by key.
    generated: Vec<(String, String)>,
    /// By each name the rows and sources hold: the written name and module key
    /// of the declaration it names in the own load.
    homes: BTreeMap<String, (String, String)>,
    /// By module key and written name: the row of each given function.
    rows: BTreeMap<String, BTreeMap<String, Stored>>,
    /// The function values the module's bodies store and pass, each once.
    sources: Vec<StoredSource>,
}

/// One module's stored rows: the entry's text, which a load indexes once and
/// decodes a row of only where the program links its function.
struct Entry {
    inputs: Inputs,
    generated: Vec<(String, String)>,
    /// The entry's text after its tag line.
    text: String,
    /// Each name the entry holds, by index: its bytes in `text`.
    names: Vec<Range<usize>>,
    /// Each home ([`Written::homes`]): the name, the written name and the
    /// module key, as indices into `names`.
    homes: Vec<[usize; 3]>,
    /// Each row's module key and written name, as indices into `names`, and
    /// its bytes in `text`, in the order of module key then written name.
    rows: Vec<(usize, usize, Range<usize>)>,
    sources: Vec<StoredSource>,
}

/// Reads a text a line at a time, keeping the offset of the next line.
struct Cursor<'t> {
    text: &'t str,
    at: usize,
}

impl<'t> Cursor<'t> {
    fn line(&mut self) -> Option<&'t str> {
        let (line, _) = self.text.get(self.at..)?.split_once('\n')?;
        self.at += line.len() + 1;
        Some(line)
    }
}

impl Entry {
    /// Whether every file still holds what the own load read, and `program`
    /// links each generated module with the text the own load linked.
    fn fresh(&self, program: &Program, resolver: &dyn ModuleResolver) -> bool {
        let same = |(k, h): &(String, String)| program.module_hashes.get(k) == Some(h);
        self.generated.iter().all(same) && self.inputs.fresh(resolver, &program.seen)
    }

    /// Loads module `m` as its own root and keeps the row of each function an
    /// importer takes as given, the module's own and those of each module its
    /// generator imports wrote.
    ///
    /// # Errors
    ///
    /// The own load's refusals, or an internal error where a file it read
    /// cannot be read back.
    fn written(
        m: &str,
        opts: &LoadOptions,
        resolver: &dyn ModuleResolver,
        engine: Option<&GenEngine>,
    ) -> Result<Written, Vec<Diagnostic>> {
        let _p = vyrn_frontend::prof::phase("std rows: write");
        let unread = || vec![missing("its inputs", m)];
        let text = resolver.read(m).map_err(|_| unread())?;
        let session = Session::rows_writer();
        let own = loader::rows_writer(opts, m, session.clone());
        let (loaded, _) = vyrn_frontend::movecheck::emitting_nothing(|| {
            crate::pipeline::load_warned(&text, m, &own, resolver, engine)
        });
        // The own load's root is `m`, which the importer's diagnostics name.
        let in_m = |ds: Vec<Diagnostic>| -> Vec<Diagnostic> {
            let file = |d: Diagnostic| match d.file {
                None => d.in_file(Some(m.to_string())),
                Some(_) => d,
            };
            ds.into_iter().map(file).collect()
        };
        let (program, world) = loaded.map_err(in_m)?;
        let mut judged = session.take_judgments();
        let root = program.spellings.root.as_str();
        // The module a function the entry holds is in: the root, or one its
        // generator import wrote.
        fn home<'a>(f: &'a vyrn_frontend::ast::Function, root: &'a str) -> Option<&'a str> {
            let k = f.module.as_deref().unwrap_or(root);
            (k == root || loader::generated_importer(k) == Some(root)).then_some(k)
        }
        let fenced = vyrn_frontend::checker::fenced(&program, &world.ownership.record);
        let mut rows = BTreeMap::new();
        let gives = loader::Gives::of(root, opts);
        for f in program
            .functions
            .iter()
            .filter(|f| loader::is_given(f, gives))
        {
            let Some(k) = home(f, root) else {
                continue;
            };
            let hash = program.module_hashes.get(k).map_or("", String::as_str);
            let key = (k.to_string(), hash.to_string(), f.name.clone());
            let (Some(judgment), Some(fenced)) = (judged.remove(&key), fenced.get(&f.name)) else {
                continue;
            };
            let written = program.spellings.written(&f.name).to_string();
            let stored = Stored {
                judgment,
                fenced: fenced.clone(),
            };
            rows.entry(k.to_string())
                .or_insert_with(BTreeMap::new)
                .insert(written, stored);
        }
        let stored = &world.ownership.record.stored;
        let mut sources: Vec<StoredSource> = stored.every_source().cloned().collect();
        once_each(&mut sources);
        // Every name the rows and sources hold that a declaration answers to.
        let decl = |n: &str| -> Option<(String, String)> {
            if let Some(d) = program.spellings.decls.get(n) {
                return Some(d.clone());
            }
            let module = (program.functions.iter().map(|f| (&f.name, &f.module)))
                .chain(program.type_decls.iter().map(|t| (&t.name, &t.module)))
                .chain(program.globals.iter().map(|g| (&g.name, &g.module)))
                .find(|(name, _)| *name == n)?
                .1;
            Some((n.to_string(), module.as_deref().unwrap_or(root).to_string()))
        };
        let mut homes = BTreeMap::new();
        let mut note = |n: &mut String| {
            if !homes.contains_key(n.as_str()) {
                if let Some(d) = decl(n) {
                    homes.insert(n.clone(), d);
                }
            }
        };
        for s in rows.values_mut().flat_map(|r| r.values_mut()) {
            each_name(s, &mut note);
        }
        for s in &mut sources {
            source_names(s, &mut note);
        }
        // A generated module has no file; the program that reads the entry
        // holds it again, under the same key.
        let (generated, files): (Vec<_>, Vec<_>) = (program.module_hashes.iter())
            .partition(|(k, _)| loader::generated_importer(k).is_some());
        let generated = (generated.into_iter())
            .map(|(k, h)| (k.clone(), h.clone()))
            .collect();
        let files = (files.into_iter().map(|(k, _)| k.clone())).chain([root.to_string()]);
        let inputs = Inputs::now(files, resolver).ok_or_else(unread)?;
        Ok(Written {
            inputs,
            generated,
            homes,
            rows,
            sources,
        })
    }

    /// Reads the entry `text` stored under `key`; `None` for a foreign,
    /// damaged or other-format entry.
    fn read(key: &str, mut text: String) -> Option<Entry> {
        let (head, body) = text.split_once('\n')?;
        let tag = head.strip_prefix(TAG)?.strip_prefix(' ')?;
        if loader::gen_cache_tag(key, body.as_bytes()) != tag {
            return None;
        }
        let head = head.len() + 1;
        text.replace_range(..head, "");
        Entry::parse(text)
    }

    /// Indexes the text [`Written::encode`] wrote and decodes all of it but the
    /// homes and the rows; `None` where it does not hold that shape.
    fn parse(text: String) -> Option<Entry> {
        let mut cur = Cursor { text: &text, at: 0 };
        let inputs = Inputs::read(&mut || cur.line())?;
        let mut d = Dec {
            text: &text,
            at: cur.at,
            names: &[],
        };
        let names: Vec<_> = d.list(Dec::bytes)?;
        let mut d = Dec { names: &names, ..d };
        let generated = d.list(|d| Some((d.s()?, d.s()?)))?;
        let homes = d.list(|d| Some([d.id()?, d.id()?, d.id()?]))?;
        let rows: Vec<_> = d.list(|d| Some((d.id()?, d.id()?, d.bytes()?)))?;
        let sources = d.list(Dec::source)?;
        let end = d.at;
        let key = |(m, n, _): &(usize, usize, Range<usize>)| {
            (&text[names[*m].clone()], &text[names[*n].clone()])
        };
        let sorted = rows.windows(2).all(|w| key(&w[0]) < key(&w[1]));
        (sorted && end == text.len()).then_some(Entry {
            inputs,
            generated,
            names,
            homes,
            rows,
            sources,
            text,
        })
    }

    /// The name at index `i`, which [`Entry::parse`] checked is one.
    fn name(&self, i: usize) -> &str {
        &self.text[self.names[i].clone()]
    }

    /// Each home: a name the rows hold, and the written name and module key
    /// of the declaration it names in the own load.
    fn homes(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        (self.homes.iter()).map(|[n, w, m]| (self.name(*n), self.name(*w), self.name(*m)))
    }

    /// The row of the function written `name` in module `m`, decoded; `None`
    /// where the entry holds none.
    fn row(&self, m: &str, name: &str) -> Option<Stored> {
        let i = (self.rows)
            .binary_search_by(|(rm, rn, _)| (self.name(*rm), self.name(*rn)).cmp(&(m, name)))
            .ok()?;
        let r = &self.rows[i].2;
        let mut d = Dec {
            text: &self.text[..r.end],
            at: r.start,
            names: &self.names,
        };
        let s = d.row()?;
        (d.at == r.end).then_some(s)
    }
}

/// Keeps the first of each run of equal sources, in their order.
fn once_each(sources: &mut Vec<StoredSource>) {
    let keep: Vec<bool> = {
        let mut seen = std::collections::HashSet::new();
        sources.iter().map(|s| seen.insert(s)).collect()
    };
    let mut keep = keep.into_iter();
    sources.retain(|_| keep.next().unwrap_or(false));
}

/// The types an entry codes as their tag alone, the tag being the index.
const LEAVES: [Type; 13] = [
    Type::Int,
    Type::Float,
    Type::Float32,
    Type::F32x4,
    Type::I32x4,
    Type::F64x2,
    Type::Mask32x4,
    Type::Mask64x2,
    Type::Bool,
    Type::Str,
    Type::Unit,
    Type::Logger,
    Type::Never,
];

/// The tags of the other types an entry holds, after [`LEAVES`].
const INT_N: usize = LEAVES.len();
const CONST_INT: usize = INT_N + 1;
const NAMED: usize = INT_N + 2;
const PARAM: usize = INT_N + 3;
const APP: usize = INT_N + 4;
const UNION: usize = INT_N + 5;
const FN: usize = INT_N + 6;
const RECORD: usize = INT_N + 7;
const ENUM: usize = INT_N + 8;
const ARRAY_N: usize = INT_N + 9;
const SMALL: usize = INT_N + 10;
const ARRAY: usize = INT_N + 11;
const STREAM: usize = INT_N + 12;
const LAZY: usize = INT_N + 13;
const ELEM: usize = INT_N + 14;
const MAP: usize = INT_N + 15;

/// The code of a call's mark and whether it binds an owned result.
fn mark_code(c: &Call) -> usize {
    let mark = match c.mark {
        Mark::None => 0,
        Mark::Await => 1,
        Mark::Branch => 2,
        Mark::Fork(n) => 3 + n,
    };
    mark * 2 + usize::from(c.born)
}

/// The mark and the owned result [`mark_code`] coded as `code`.
fn mark_of(code: usize) -> (Mark, bool) {
    let mark = match code / 2 {
        0 => Mark::None,
        1 => Mark::Await,
        2 => Mark::Branch,
        n => Mark::Fork(n - 3),
    };
    (mark, code % 2 == 1)
}

impl Written {
    /// The entry's text after its tag line: [`Inputs::render`]'s lines, then
    /// the coded part ([`Enc`]): the name table, the generated modules, the
    /// homes, each row after its length, and each source.
    fn encode(&self) -> String {
        let mut e = Enc::default();
        e.list(self.generated.iter(), |e, (k, h)| {
            e.s(k);
            e.s(h);
        });
        e.list(self.homes.iter(), |e, (n, (w, m))| {
            e.s(n);
            e.s(w);
            e.s(m);
        });
        let rows = (self.rows.iter()).flat_map(|(m, r)| r.iter().map(move |(n, s)| (m, n, s)));
        e.u(rows.clone().count());
        for (module, name, s) in rows {
            e.s(module);
            e.s(name);
            let outer = std::mem::take(&mut e.out);
            e.row(s);
            let body = std::mem::replace(&mut e.out, outer);
            e.u(body.len());
            e.out.push_str(&body);
        }
        e.list(self.sources.iter(), Enc::source);
        let mut out = String::new();
        self.inputs.render(&mut out);
        let mut table = Enc::default();
        table.list(e.names.iter(), |t, n| {
            t.u(n.len());
            t.out.push_str(n);
        });
        out.push_str(&table.out);
        out.push_str(&e.out);
        out
    }
}

/// Writes an entry's coded part. Each count, index and tag is a varint of
/// ASCII bytes, six bits each and 0x40 where another follows, so the entry
/// stays the UTF-8 text the cache stores; each name is an index into one
/// table, in first-use order.
#[derive(Default)]
struct Enc<'a> {
    out: String,
    ids: HashMap<&'a str, usize>,
    names: Vec<&'a str>,
}

impl<'a> Enc<'a> {
    fn u(&mut self, mut v: usize) {
        while v >= 0x40 {
            self.out.push(char::from(0x40 | (v & 0x3f) as u8));
            v >>= 6;
        }
        self.out.push(char::from(v as u8));
    }

    /// A count, then each item.
    fn list<T>(
        &mut self,
        items: impl ExactSizeIterator<Item = T>,
        mut f: impl FnMut(&mut Self, T),
    ) {
        self.u(items.len());
        items.for_each(|t| f(self, t));
    }

    fn s(&mut self, s: &'a str) {
        let next = self.names.len();
        let id = *self.ids.entry(s).or_insert(next);
        if id == next {
            self.names.push(s);
        }
        self.u(id);
    }

    fn opt(&mut self, s: Option<&'a str>) {
        self.list(s.into_iter(), Enc::s);
    }

    fn tys(&mut self, ts: &'a [Type]) {
        self.list(ts.iter(), Enc::ty);
    }

    /// # Panics
    ///
    /// On a type no checked signature or row holds: a type-level operator the
    /// checker resolves (`Omit`, `Pick`, `Merge`, `Partial`), an order, or an
    /// error type.
    fn ty(&mut self, t: &'a Type) {
        if let Some(i) = LEAVES.iter().position(|l| l == t) {
            return self.u(i);
        }
        match t {
            Type::IntN { bits, signed } => {
                self.u(INT_N);
                self.u(usize::from(*bits) * 2 + usize::from(*signed));
            }
            Type::ConstInt(n) => {
                self.u(CONST_INT);
                self.u(*n as usize);
            }
            Type::Named(n) => {
                self.u(NAMED);
                self.s(n);
            }
            Type::Param(n) => {
                self.u(PARAM);
                self.s(n);
            }
            Type::App(n, ts) => {
                self.u(APP);
                self.s(n);
                self.tys(ts);
            }
            Type::Union(ts) => {
                self.u(UNION);
                self.tys(ts);
            }
            Type::Fn(ps, r) => {
                self.u(FN);
                self.tys(ps);
                self.ty(r);
            }
            Type::Record(fs) => {
                self.u(RECORD);
                self.list(fs.iter(), |e, f| {
                    e.s(&f.name);
                    e.ty(&f.ty);
                });
            }
            Type::Enum(vs) => {
                self.u(ENUM);
                self.list(vs.iter(), |e, v| {
                    e.s(&v.name);
                    e.tys(&v.payload);
                });
            }
            Type::ArrayN(b, n) | Type::SmallArray(b, n) => {
                self.u(if matches!(t, Type::ArrayN(..)) {
                    ARRAY_N
                } else {
                    SMALL
                });
                self.u(*n);
                self.ty(b);
            }
            Type::Array(b) | Type::Stream(b) | Type::Lazy(b) | Type::Elem(b) => {
                self.u(match t {
                    Type::Array(_) => ARRAY,
                    Type::Stream(_) => STREAM,
                    Type::Lazy(_) => LAZY,
                    _ => ELEM,
                });
                self.ty(b);
            }
            Type::Map(k, v) => {
                self.u(MAP);
                self.ty(k);
                self.ty(v);
            }
            other => panic!("a stored row holds the type `{other:?}`"),
        }
    }

    fn row(&mut self, s: &'a Stored) {
        let j = &s.judgment;
        self.list(j.frames.iter().zip(&j.state), |e, (f, state)| {
            e.s(&f.name);
            e.u(f.own.iter().fold(0, |m, a| m | 1 << a as usize));
            e.list(f.writes.iter(), |e, w| e.s(w));
            e.list(f.lambdas.iter(), |e, l| e.u(*l));
            e.list(f.calls.iter(), |e, c| {
                e.s(&c.callee);
                e.u(c.line);
                e.u(mark_code(c));
                e.list(c.through.iter(), Enc::ty);
            });
            e.list(state.iter(), |e, (c, gs)| {
                e.s(c);
                e.list(gs.iter(), |e, g| e.s(g));
            });
        });
        self.list(j.summary.iter(), |e, fact| {
            e.u(usize::from(matches!(fact.word, Word::Strict)));
            e.ty(&fact.order);
        });
        self.list(j.calls.iter(), |e, (c, subst)| {
            e.s(c);
            e.list(subst.iter(), |e, (p, t)| {
                e.s(p);
                e.ty(t);
            });
        });
        self.opt(s.fenced.impure.as_deref());
        self.list(s.fenced.calls.iter(), |e, c| e.s(c));
    }

    fn source(&mut self, s: &'a StoredSource) {
        self.ty(&s.sig);
        self.opt(s.named.as_deref());
        self.list(s.lambda.iter(), |e, l| {
            e.s(&l.defined_in);
            e.u(l.line);
            e.u(l.col);
        });
    }
}

/// Reads what [`Enc`] wrote, from `at` in `text`. Every read is bounded by
/// `text`, so a truncated or damaged entry reads as `None`.
struct Dec<'t> {
    text: &'t str,
    at: usize,
    names: &'t [Range<usize>],
}

impl<'t> Dec<'t> {
    fn u(&mut self) -> Option<usize> {
        let (mut v, mut shift) = (0usize, 0u32);
        loop {
            let b = *self.text.as_bytes().get(self.at)?;
            self.at += 1;
            if b >= 0x80 || shift >= usize::BITS {
                return None;
            }
            v |= usize::from(b & 0x3f) << shift;
            if b & 0x40 == 0 {
                return Some(v);
            }
            shift += 6;
        }
    }

    /// A count ([`Dec::n`]), then that many items.
    fn list<T, C: FromIterator<T>>(
        &mut self,
        mut f: impl FnMut(&mut Self) -> Option<T>,
    ) -> Option<C> {
        (0..self.n()?).map(|_| f(self)).collect()
    }

    /// A count of items each at least one byte long, so no more than the
    /// bytes left.
    fn n(&mut self) -> Option<usize> {
        let n = self.u()?;
        (n <= self.text.len() - self.at).then_some(n)
    }

    /// A length, then that many bytes of whole characters: their range.
    fn bytes(&mut self) -> Option<Range<usize>> {
        let n = self.u()?;
        let r = self.at..self.at.checked_add(n)?;
        self.text.get(r.clone())?;
        self.at = r.end;
        Some(r)
    }

    fn id(&mut self) -> Option<usize> {
        let i = self.u()?;
        (i < self.names.len()).then_some(i)
    }

    fn s(&mut self) -> Option<String> {
        let r = self.names.get(self.u()?)?;
        Some(self.text.get(r.clone())?.to_string())
    }

    /// What [`Enc`] wrote as a list of none or one.
    fn opt<T>(&mut self, f: impl FnOnce(&mut Self) -> Option<T>) -> Option<Option<T>> {
        match self.u()? {
            0 => Some(None),
            1 => f(self).map(Some),
            _ => None,
        }
    }

    fn tys(&mut self) -> Option<Vec<Type>> {
        self.list(Dec::ty)
    }

    fn boxed(&mut self) -> Option<Box<Type>> {
        self.ty().map(Box::new)
    }

    fn ty(&mut self) -> Option<Type> {
        let tag = self.u()?;
        if let Some(t) = LEAVES.get(tag) {
            return Some(t.clone());
        }
        Some(match tag {
            INT_N => {
                let v = self.u()?;
                Type::IntN {
                    bits: u8::try_from(v / 2).ok()?,
                    signed: v % 2 == 1,
                }
            }
            CONST_INT => Type::ConstInt(self.u()? as u64),
            NAMED => Type::Named(self.s()?),
            PARAM => Type::Param(self.s()?),
            APP => Type::App(self.s()?, self.tys()?),
            UNION => Type::Union(self.tys()?),
            FN => Type::Fn(self.tys()?, self.boxed()?),
            RECORD => Type::Record(self.list(|d| {
                let name = d.s()?;
                Some(vyrn_frontend::ast::Field { name, ty: d.ty()? })
            })?),
            ENUM => Type::Enum(self.list(|d| {
                let name = d.s()?;
                let payload = d.tys()?;
                Some(vyrn_frontend::ast::EnumVariant { name, payload })
            })?),
            ARRAY_N => {
                let n = self.u()?;
                Type::ArrayN(self.boxed()?, n)
            }
            SMALL => {
                let n = self.u()?;
                Type::SmallArray(self.boxed()?, n)
            }
            ARRAY => Type::Array(self.boxed()?),
            STREAM => Type::Stream(self.boxed()?),
            LAZY => Type::Lazy(self.boxed()?),
            ELEM => Type::Elem(self.boxed()?),
            MAP => Type::Map(self.boxed()?, self.boxed()?),
            _ => return None,
        })
    }

    fn call(&mut self) -> Option<Call> {
        let callee = self.s()?;
        let line = self.u()?;
        let (mark, born) = mark_of(self.u()?);
        Some(Call {
            callee,
            through: self.opt(Dec::ty)?,
            line,
            born,
            mark,
        })
    }

    fn frame(&mut self) -> Option<(Walked, Vec<(String, Vec<String>)>)> {
        let name = self.s()?;
        let mask = self.u()?;
        let own = (Effect::ALL.into_iter())
            .filter(|e| mask >> *e as usize & 1 == 1)
            .fold(Effects::PURE, Effects::with);
        let walked = Walked {
            name,
            own,
            writes: self.list(Dec::s)?,
            lambdas: self.list(Dec::u)?,
            calls: self.list(Dec::call)?,
        };
        let answer = self.list(|d| Some((d.s()?, d.list(Dec::s)?)))?;
        Some((walked, answer))
    }

    fn row(&mut self) -> Option<Stored> {
        let n = self.n()?;
        let (mut frames, mut state) = (Vec::with_capacity(n), Vec::with_capacity(n));
        for _ in 0..n {
            let (walked, answer) = self.frame()?;
            frames.push(walked);
            state.push(answer);
        }
        let summary = self.list(|d| {
            let word = [Word::Sorted, Word::Strict].into_iter().nth(d.u()?)?;
            Some(Fact {
                word,
                order: d.ty()?,
            })
        })?;
        let calls = self.list(|d| Some((d.s()?, d.list(|d| Some((d.s()?, d.ty()?)))?)))?;
        let judgment = Judgment {
            verdict: Vec::new(),
            frames,
            state,
            summary,
            calls,
        };
        let fenced = Fenced {
            impure: self.opt(Dec::s)?,
            calls: self.list(Dec::s)?,
        };
        Some(Stored { judgment, fenced })
    }

    fn source(&mut self) -> Option<StoredSource> {
        let sig = self.ty()?;
        let named = self.opt(Dec::s)?;
        let lambda = self.opt(|d| {
            Some(StoredLambda {
                defined_in: d.s()?,
                line: d.u()?,
                col: d.u()?,
            })
        })?;
        Some(StoredSource { sig, named, lambda })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    /// The key of std module `name`: the std directory beside this crate,
    /// slash-separated as module keys are, and the file.
    fn key(name: &str) -> String {
        let dir = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
        let dir = std::fs::canonicalize(dir).expect("std");
        let dir = dir.to_string_lossy().replace('\\', "/");
        format!("{}/{name}", dir.strip_prefix("//?/").unwrap_or(&dir))
    }

    /// What [`Entry::written`] keeps of std module `m`, loaded without a
    /// generator engine.
    fn own(m: &str) -> Result<Written, Vec<Diagnostic>> {
        let std = key("");
        let opts = LoadOptions {
            std_root: Some(std.trim_end_matches('/').to_string()),
            ..LoadOptions::default()
        };
        // The own load recurses as deep as the driver's, which runs it on a
        // deep stack.
        std::thread::scope(|scope| {
            (std::thread::Builder::new().stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES))
                .spawn_scoped(scope, || {
                    Entry::written(m, &opts, &loader::DiskResolver, None)
                })
                .expect("spawn the own load")
                .join()
                .expect("the own load returns")
        })
    }

    /// The callee of each call of each frame of the row of `name` in std
    /// module `m`.
    fn row_calls(m: &str, name: &str) -> Vec<String> {
        let k = key(m);
        let w = own(&k).expect("the module writes its rows");
        let frames = &w.rows[&k][name].judgment.frames;
        (frames.iter().flat_map(|f| &f.calls))
            .map(|c| c.callee.clone())
            .collect()
    }

    /// The own load expands a projection as an importer's does: `std/stream`'s
    /// `cells[h]` reads through `std/slots`' `at`, which checks the handle is
    /// alive.
    #[test]
    fn a_stored_row_judges_a_projection_as_the_importer_runs_it() {
        let calls = row_calls("stream.vyrn", "cursorGet");
        assert!(
            calls.iter().any(|c| c == "alive"),
            "cursorGet calls {calls:?}"
        );
    }

    /// The own load names its sites as an importer's does: the runtime's
    /// `panic` is its fixed trap, never stamped with a site.
    #[test]
    fn a_stored_row_names_its_sites_as_the_importer_does() {
        let calls = row_calls("runtime.vyrn", "intStr");
        assert!(calls.iter().any(|c| c == "panic"), "intStr calls {calls:?}");
        assert!(
            !calls.iter().any(|c| c.contains("panicAt")),
            "intStr calls {calls:?}"
        );
    }

    #[test]
    fn every_std_module_s_rows_decode_to_the_rows_written() {
        let dir = key("");
        let mut keys: Vec<String> = std::fs::read_dir(&dir)
            .expect("read std")
            .flatten()
            .map(|e| e.file_name().to_string_lossy().to_string())
            .filter(|n| n.ends_with(".vyrn") && !n.ends_with(".editor.vyrn"))
            .map(|n| format!("{dir}{n}"))
            .collect();
        keys.sort();
        let mut decoded = 0;
        for m in keys {
            // A module whose load runs a generator needs an engine.
            let Ok(w) = own(&m) else {
                continue;
            };
            decoded += 1;
            let e = Entry::parse(w.encode()).unwrap_or_else(|| panic!("{m} does not parse"));
            assert_eq!(e.generated, w.generated, "{m}");
            let homes: Vec<_> = (w.homes.iter())
                .map(|(n, (wr, md))| (n.as_str(), wr.as_str(), md.as_str()))
                .collect();
            assert_eq!(e.homes().collect::<Vec<_>>(), homes, "{m}");
            assert_eq!(e.sources, w.sources, "{m}");
            let rows = (w.rows.iter()).flat_map(|(k, r)| r.iter().map(move |(n, s)| (k, n, s)));
            assert_eq!(e.rows.len(), rows.clone().count(), "{m}");
            for (k, n, s) in rows {
                let back = e.row(k, n).unwrap_or_else(|| panic!("{m}: no row {n}"));
                assert_eq!(
                    format!("{:?} {:?}", back.judgment, back.fenced),
                    format!("{:?} {:?}", s.judgment, s.fenced),
                    "{m}: row {n}"
                );
            }
        }
        assert!(decoded >= 40, "{decoded} std modules wrote rows");
    }

    #[test]
    fn a_truncated_or_flipped_entry_reads_as_a_miss() {
        let m = key("task.vyrn");
        let body = own(&m).expect("std/task writes its rows").encode();
        let k = entry_key(&m, "test");
        let tag = loader::gen_cache_tag(&k, body.as_bytes());
        let text = format!("{TAG} {tag}\n{body}");
        assert!(Entry::read(&k, text.clone()).is_some());
        for cut in [text.len() - 1, text.len() / 2, tag.len()] {
            assert!(
                Entry::read(&k, text[..cut].to_string()).is_none(),
                "cut {cut}"
            );
        }
        let mut flipped = text.clone().into_bytes();
        flipped[text.len() - body.len() / 3] ^= 1;
        let flipped = String::from_utf8(flipped).expect("a flipped bit 0 keeps UTF-8");
        assert!(Entry::read(&k, flipped).is_none());
        // An entry no tag guards decodes each prefix without a panic.
        for cut in 0..body.len() {
            let prefix = body.get(..cut).and_then(|b| Entry::parse(b.to_string()));
            assert!(prefix.is_none(), "a prefix of {cut} bytes parsed");
        }
    }
}
