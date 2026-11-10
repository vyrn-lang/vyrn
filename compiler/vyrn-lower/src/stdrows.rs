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
//! entry's rows by the line count each row's header ends with, and decodes a
//! row only for a function the program links.
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
const TAG: &str = "stdrows3";

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
        out.sources.sort_by_key(source_key);
        out.sources.dedup_by(|a, b| source_key(a) == source_key(b));
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
            let body = Entry::written(owner, opts, resolver, engine)?;
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

/// One module's stored rows: the entry's text, which a load indexes once and
/// decodes a row of only where the program links its function.
struct Entry {
    /// The files the module's own load read.
    inputs: Inputs,
    /// The text hash of each generated module the own load linked, by key.
    generated: Vec<(String, String)>,
    /// The entry's text after its tag line.
    text: String,
    /// The homes' lines in `text`, `name<TAB>written<TAB>module` each: the
    /// declaration each name the rows hold names in the own load.
    homes: Range<usize>,
    /// Each row's header line and its other lines in `text`, in the order of
    /// their module key and written name ([`Entry::key`]).
    rows: Vec<(Range<usize>, Range<usize>)>,
    /// The function values the module's bodies store and pass.
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

    /// Loads module `m` as its own root and writes the row of each function
    /// an importer takes as given, the module's own and those of each module
    /// its generator imports wrote, as the entry's text after its tag line:
    /// [`Inputs::render`], the generated modules, the homes, each row after a
    /// header that counts its lines, and each source.
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
    ) -> Result<String, Vec<Diagnostic>> {
        let _p = vyrn_frontend::prof::phase("std rows: write");
        let unread = || vec![missing("its inputs", m)];
        let text = resolver.read(m).map_err(|_| unread())?;
        let session = Session::rows_writer();
        let own = loader::rows_writer(opts, m, session.clone());
        let (loaded, _) = vyrn_frontend::movecheck::emitting_nothing(|| {
            crate::pipeline::load_warned(&text, m, &own, resolver, engine)
        });
        let (program, world) = loaded?;
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
        sources.sort_by_key(source_key);
        sources.dedup_by(|a, b| source_key(a) == source_key(b));
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
        let files = (files.into_iter().map(|(k, _)| k.clone())).chain([root.to_string()]);
        let inputs = Inputs::now(files, resolver).ok_or_else(unread)?;
        let mut out = String::new();
        inputs.render(&mut out);
        out.push_str(&format!("{}\n", generated.len()));
        for (k, h) in &generated {
            out.push_str(&format!("{k}\t{h}\n"));
        }
        out.push_str(&format!("{}\n", homes.len()));
        for (n, (w, m)) in &homes {
            out.push_str(&format!("{n}\t{w}\t{m}\n"));
        }
        let rows = (rows.iter()).flat_map(|(m, r)| r.iter().map(move |(n, s)| (n, m, s)));
        out.push_str(&format!("{}\n", rows.clone().count()));
        for (name, module, s) in rows {
            let j = &s.judgment;
            let impure = s.fenced.impure.as_deref().unwrap_or("-");
            let mut body = String::new();
            for (f, state) in j.frames.iter().zip(&j.state) {
                write_frame(&mut body, f);
                let state: Vec<String> = (state.iter())
                    .map(|(c, gs)| format!("{c}={}", gs.join(",")))
                    .collect();
                body.push_str(&state.join("\t"));
                body.push('\n');
            }
            for fact in &j.summary {
                body.push_str(fact.word.spelled());
                body.push('\t');
                write_type(&mut body, &fact.order);
                body.push('\n');
            }
            for (c, subst) in &j.calls {
                body.push_str(c);
                for (p, t) in subst {
                    body.push_str(&format!("\t{p}\t"));
                    write_type(&mut body, t);
                }
                body.push('\n');
            }
            body.push_str(&s.fenced.calls.join("\t"));
            body.push('\n');
            let lines = body.matches('\n').count();
            let (frames, summary, calls) = (j.frames.len(), j.summary.len(), j.calls.len());
            out.push_str(&format!(
                "{name}\t{module}\t{frames}\t{summary}\t{calls}\t{impure}\t{lines}\n"
            ));
            out.push_str(&body);
        }
        out.push_str(&format!("{}\n", sources.len()));
        for s in &sources {
            write_type(&mut out, &s.sig);
            let named = s.named.as_deref().unwrap_or("-");
            match &s.lambda {
                Some(l) => out.push_str(&format!(
                    "\t{named}\t{}\t{}\t{}\n",
                    l.defined_in, l.line, l.col
                )),
                None => out.push_str(&format!("\t{named}\n")),
            }
        }
        Ok(out)
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

    /// Indexes the text [`Entry::written`] wrote and decodes all of it but the
    /// homes and the rows; `None` where it does not hold that shape.
    fn parse(text: String) -> Option<Entry> {
        let mut cur = Cursor { text: &text, at: 0 };
        let count = |l: Option<&str>| l?.parse::<usize>().ok();
        let inputs = Inputs::read(&mut || cur.line())?;
        let generated = (0..count(cur.line())?)
            .map(|_| {
                let (k, h) = cur.line()?.split_once('\t')?;
                Some((k.to_string(), h.to_string()))
            })
            .collect::<Option<_>>()?;
        let n = count(cur.line())?;
        let start = cur.at;
        for _ in 0..n {
            cur.line()?.split('\t').nth(2)?;
        }
        let homes = start..cur.at;
        let mut rows = Vec::new();
        for _ in 0..count(cur.line())? {
            let at = cur.at;
            let header = cur.line()?;
            let lines: usize = header.rsplit('\t').next()?.parse().ok()?;
            let from = cur.at;
            for _ in 0..lines {
                cur.line()?;
            }
            rows.push((at..at + header.len(), from..cur.at));
        }
        let mut line = || cur.line();
        let sources = (0..count(line())?)
            .map(|_| {
                let mut f = line()?.split('\t');
                let sig = whole_type(f.next()?)?;
                let named = Some(f.next()?).filter(|n| *n != "-").map(String::from);
                let lambda = match f.next() {
                    Some(d) => Some(StoredLambda {
                        defined_in: d.to_string(),
                        line: f.next()?.parse().ok()?,
                        col: f.next()?.parse().ok()?,
                    }),
                    None => None,
                };
                Some(StoredSource { sig, named, lambda })
            })
            .collect::<Option<_>>()?;
        let end = cur.at;
        let key = |r| Entry::key(&text, r);
        let sorted = rows.windows(2).all(|w| key(&w[0]) < key(&w[1]));
        (sorted && end == text.len()).then_some(Entry {
            inputs,
            generated,
            homes,
            rows,
            sources,
            text,
        })
    }

    /// The module key and written name the header of row `r` names.
    fn key<'t>(text: &'t str, r: &(Range<usize>, Range<usize>)) -> (&'t str, &'t str) {
        let mut f = text[r.0.clone()].split('\t');
        let name = f.next().unwrap_or("");
        (f.next().unwrap_or(""), name)
    }

    /// Each home: a name the rows hold, and the written name and module key
    /// of the declaration it names in the own load.
    fn homes(&self) -> impl Iterator<Item = (&str, &str, &str)> {
        (self.text[self.homes.clone()].lines()).filter_map(|l| {
            let mut f = l.split('\t');
            Some((f.next()?, f.next()?, f.next()?))
        })
    }

    /// The row of the function written `name` in module `m`, decoded; `None`
    /// where the entry holds none.
    fn row(&self, m: &str, name: &str) -> Option<Stored> {
        let i = (self.rows)
            .binary_search_by(|r| Entry::key(&self.text, r).cmp(&(m, name)))
            .ok()?;
        let (header, body) = &self.rows[i];
        let mut h = self.text[header.clone()].split('\t').skip(2);
        let mut lines = self.text[body.clone()].split_terminator('\n');
        let mut line = || lines.next();
        let count = |l: Option<&str>| l?.parse::<usize>().ok();
        let (nf, ns, nc) = (count(h.next())?, count(h.next())?, count(h.next())?);
        let impure = Some(h.next()?).filter(|i| *i != "-").map(String::from);
        let (mut frames, mut state) = (Vec::new(), Vec::new());
        for _ in 0..nf {
            frames.push(read_frame(&mut line)?);
            let answer = (line()?.split('\t').filter(|s| !s.is_empty()))
                .map(|e| {
                    let (c, gs) = e.split_once('=')?;
                    let gs = gs.split(',').filter(|g| !g.is_empty()).map(String::from);
                    Some((c.to_string(), gs.collect()))
                })
                .collect::<Option<Vec<_>>>()?;
            state.push(answer);
        }
        let summary = (0..ns)
            .map(|_| {
                let (w, t) = line()?.split_once('\t')?;
                let word = [Word::Sorted, Word::Strict]
                    .into_iter()
                    .find(|x| x.spelled() == w)?;
                Some(Fact {
                    word,
                    order: whole_type(t)?,
                })
            })
            .collect::<Option<_>>()?;
        let calls = (0..nc)
            .map(|_| {
                let mut f = line()?.split('\t');
                let callee = f.next()?.to_string();
                let mut subst = Vec::new();
                while let Some(p) = f.next() {
                    subst.push((p.to_string(), whole_type(f.next()?)?));
                }
                Some((callee, subst))
            })
            .collect::<Option<_>>()?;
        let fence = line()?
            .split('\t')
            .filter(|s| !s.is_empty())
            .map(String::from);
        let judgment = Judgment {
            verdict: Vec::new(),
            frames,
            state,
            summary,
            calls,
        };
        let fenced = Fenced {
            impure,
            calls: fence.collect(),
        };
        Some(Stored { judgment, fenced })
    }
}

/// The order [`Entry::written`] sorts and deduplicates sources by.
fn source_key(s: &StoredSource) -> (String, Option<String>, Option<(String, usize, usize)>) {
    let mut sig = String::new();
    write_type(&mut sig, &s.sig);
    let lambda = s
        .lambda
        .as_ref()
        .map(|l| (l.defined_in.clone(), l.line, l.col));
    (sig, s.named.clone(), lambda)
}

/// One frame: `name<TAB>effects<TAB>writes<TAB>lambdas<TAB>calls`, then one
/// `callee<TAB>line<TAB>born<TAB>mark<TAB>type` line per call.
fn write_frame(out: &mut String, f: &Walked) {
    let effects: Vec<&str> = f.own.iter().map(Effect::name).collect();
    let writes: Vec<&str> = f.writes.iter().map(String::as_str).collect();
    let lambdas: Vec<String> = f.lambdas.iter().map(usize::to_string).collect();
    out.push_str(&format!(
        "{}\t{}\t{}\t{}\t{}\n",
        f.name,
        effects.join(","),
        writes.join(","),
        lambdas.join(","),
        f.calls.len()
    ));
    for c in &f.calls {
        let mark = match c.mark {
            Mark::None => "-".to_string(),
            Mark::Await => "a".to_string(),
            Mark::Fork(n) => format!("f{n}"),
            Mark::Branch => "b".to_string(),
        };
        let born = c.born as u8;
        out.push_str(&format!("{}\t{}\t{born}\t{mark}\t", c.callee, c.line));
        match &c.through {
            Some(t) => write_type(out, t),
            None => out.push('-'),
        }
        out.push('\n');
    }
}

/// Reads one frame [`write_frame`] wrote.
fn read_frame<'t>(line: &mut impl FnMut() -> Option<&'t str>) -> Option<Walked> {
    let list = |s: &'t str| s.split(',').filter(|x| !x.is_empty());
    let mut f = line()?.split('\t');
    let name = f.next()?.to_string();
    let own =
        (list(f.next()?).map(Effect::parse)).try_fold(Effects::PURE, |e, a| Some(e.with(a?)))?;
    let writes = list(f.next()?).map(String::from).collect();
    let lambdas = list(f.next()?)
        .map(|l| l.parse().ok())
        .collect::<Option<_>>()?;
    let mut calls = Vec::new();
    for _ in 0..f.next()?.parse::<usize>().ok()? {
        let mut c = line()?.split('\t');
        let callee = c.next()?.to_string();
        let at = c.next()?.parse().ok()?;
        let born = c.next()? == "1";
        let mark = match c.next()? {
            "-" => Mark::None,
            "a" => Mark::Await,
            "b" => Mark::Branch,
            f => Mark::Fork(f.strip_prefix('f')?.parse().ok()?),
        };
        let through = match c.next()? {
            "-" => None,
            t => Some(whole_type(t)?),
        };
        calls.push(Call {
            callee,
            through,
            line: at,
            born,
            mark,
        });
    }
    Some(Walked {
        name,
        own,
        writes,
        calls,
        lambdas,
    })
}

/// Writes `t` as space-separated prefix tokens, a list after its length.
///
/// # Panics
///
/// On a type no checked signature or row holds: a type-level operator the
/// checker resolves (`Omit`, `Pick`, `Merge`, `Partial`), an order, or an
/// error type.
fn write_type(out: &mut String, t: &Type) {
    fn tok(out: &mut String, s: &str) {
        if !out.is_empty() && !out.ends_with(['\t', '\n']) {
            out.push(' ');
        }
        out.push_str(s);
    }
    match t {
        Type::Int => tok(out, "i64"),
        Type::Float => tok(out, "f64"),
        Type::Float32 => tok(out, "f32"),
        Type::F32x4 => tok(out, "f32x4"),
        Type::I32x4 => tok(out, "i32x4"),
        Type::F64x2 => tok(out, "f64x2"),
        Type::Mask32x4 => tok(out, "m32x4"),
        Type::Mask64x2 => tok(out, "m64x2"),
        Type::Bool => tok(out, "bool"),
        Type::Str => tok(out, "str"),
        Type::Unit => tok(out, "unit"),
        Type::Logger => tok(out, "logger"),
        Type::Never => tok(out, "never"),
        Type::IntN { bits, signed } => {
            tok(out, &format!("n{bits}{}", if *signed { "s" } else { "u" }))
        }
        Type::ConstInt(n) => tok(out, &format!("c{n}")),
        Type::Named(n) => {
            tok(out, "named");
            tok(out, n);
        }
        Type::Param(n) => {
            tok(out, "param");
            tok(out, n);
        }
        Type::App(n, ts) => {
            tok(out, "app");
            tok(out, n);
            tok(out, &ts.len().to_string());
            ts.iter().for_each(|t| write_type(out, t));
        }
        Type::Union(ts) => {
            tok(out, "union");
            tok(out, &ts.len().to_string());
            ts.iter().for_each(|t| write_type(out, t));
        }
        Type::Fn(ps, r) => {
            tok(out, "fn");
            tok(out, &ps.len().to_string());
            ps.iter().for_each(|t| write_type(out, t));
            write_type(out, r);
        }
        Type::Record(fs) => {
            tok(out, "record");
            tok(out, &fs.len().to_string());
            for f in fs {
                tok(out, &f.name);
                write_type(out, &f.ty);
            }
        }
        Type::Enum(vs) => {
            tok(out, "enum");
            tok(out, &vs.len().to_string());
            for v in vs {
                tok(out, &v.name);
                tok(out, &v.payload.len().to_string());
                v.payload.iter().for_each(|t| write_type(out, t));
            }
        }
        Type::ArrayN(b, n) | Type::SmallArray(b, n) => {
            tok(
                out,
                if matches!(t, Type::ArrayN(..)) {
                    "arrayn"
                } else {
                    "small"
                },
            );
            tok(out, &n.to_string());
            write_type(out, b);
        }
        Type::Array(b) | Type::Stream(b) | Type::Lazy(b) | Type::Elem(b) => {
            tok(
                out,
                match t {
                    Type::Array(_) => "array",
                    Type::Stream(_) => "stream",
                    Type::Lazy(_) => "lazy",
                    _ => "elem",
                },
            );
            write_type(out, b);
        }
        Type::Map(k, v) => {
            tok(out, "map");
            write_type(out, k);
            write_type(out, v);
        }
        other => panic!("a stored row holds the type `{other:?}`"),
    }
}

/// Reads a type [`write_type`] wrote as the whole of `text`.
fn whole_type(text: &str) -> Option<Type> {
    let mut tokens = text.split(' ');
    let t = read_type(&mut tokens)?;
    tokens.next().is_none().then_some(t)
}

/// Reads one type [`write_type`] wrote from `tokens`.
fn read_type<'t>(tokens: &mut dyn Iterator<Item = &'t str>) -> Option<Type> {
    fn count<'t>(tokens: &mut dyn Iterator<Item = &'t str>) -> Option<usize> {
        tokens.next()?.parse().ok()
    }
    fn list<'t>(tokens: &mut dyn Iterator<Item = &'t str>) -> Option<Vec<Type>> {
        (0..count(tokens)?).map(|_| read_type(tokens)).collect()
    }
    let boxed = |tokens: &mut dyn Iterator<Item = &'t str>| read_type(tokens).map(Box::new);
    let word = tokens.next()?;
    Some(match word {
        "i64" => Type::Int,
        "f64" => Type::Float,
        "f32" => Type::Float32,
        "f32x4" => Type::F32x4,
        "i32x4" => Type::I32x4,
        "f64x2" => Type::F64x2,
        "m32x4" => Type::Mask32x4,
        "m64x2" => Type::Mask64x2,
        "bool" => Type::Bool,
        "str" => Type::Str,
        "unit" => Type::Unit,
        "logger" => Type::Logger,
        "never" => Type::Never,
        "named" => Type::Named(tokens.next()?.to_string()),
        "param" => Type::Param(tokens.next()?.to_string()),
        "app" => Type::App(tokens.next()?.to_string(), list(tokens)?),
        "union" => Type::Union(list(tokens)?),
        "fn" => Type::Fn(list(tokens)?, boxed(tokens)?),
        "record" => {
            let fields = (0..count(tokens)?).map(|_| {
                let name = tokens.next()?.to_string();
                let ty = read_type(tokens)?;
                Some(vyrn_frontend::ast::Field { name, ty })
            });
            Type::Record(fields.collect::<Option<_>>()?)
        }
        "enum" => {
            let variants = (0..count(tokens)?).map(|_| {
                let name = tokens.next()?.to_string();
                let payload = list(tokens)?;
                Some(vyrn_frontend::ast::EnumVariant { name, payload })
            });
            Type::Enum(variants.collect::<Option<_>>()?)
        }
        "arrayn" => {
            let n = count(tokens)?;
            Type::ArrayN(boxed(tokens)?, n)
        }
        "small" => {
            let n = count(tokens)?;
            Type::SmallArray(boxed(tokens)?, n)
        }
        "array" => Type::Array(boxed(tokens)?),
        "stream" => Type::Stream(boxed(tokens)?),
        "lazy" => Type::Lazy(boxed(tokens)?),
        "elem" => Type::Elem(boxed(tokens)?),
        "map" => Type::Map(boxed(tokens)?, boxed(tokens)?),
        _ => match word.strip_prefix('c') {
            Some(n) => Type::ConstInt(n.parse().ok()?),
            None => {
                let body = word.strip_prefix('n')?;
                let (bits, sign) = body.split_at(body.len().checked_sub(1)?);
                let signed = match sign {
                    "s" => true,
                    "u" => false,
                    _ => return None,
                };
                Type::IntN {
                    bits: bits.parse().ok()?,
                    signed,
                }
            }
        },
    })
}
