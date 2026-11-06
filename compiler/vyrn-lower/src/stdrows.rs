//! Std's stored rows: what each given function of a std module
//! ([`Body::Given`]) leaves its importers, written by the module's own load
//! and read where `vyrn check` takes std as its interface
//! ([`loader::std_given`]).
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
//! writes the entries it needs in memory, once per thread.
//!
//! A row names a declaration by its written name and module, so a program
//! that renames one apart reads it under the program's name. [`Given::read`]
//! is the one constructor of the table the placer and the worklist read, so
//! every given function of the program has a row there.

use std::cell::RefCell;
use std::collections::{BTreeMap, HashMap};
use std::sync::Arc;

use vyrn_frontend::ast::{Body, Function, Program, Type};
use vyrn_frontend::checker::{Fenced, StoredLambda, StoredSource};
use vyrn_frontend::core::check::{Fact, Word};
use vyrn_frontend::diagnostics::Diagnostic;
use vyrn_frontend::effects::{Call, Effect, Effects, Mark, Walked};
use vyrn_frontend::gen::GenEngine;
use vyrn_frontend::loader::{self, Inputs, LoadOptions, ModuleResolver, Seen};
use vyrn_frontend::movecheck::{Judgment, JudgmentKey};
use vyrn_frontend::session::Session;

/// The format word that opens an entry.
const TAG: &str = "stdrows2";

/// The stored rows of a program's given functions: each std module's entry
/// and how the program names what it names.
#[derive(Default)]
pub struct Given {
    /// By std module key: the module's entry, which holds the rows of the
    /// modules its generator imports wrote too, and each name the entry holds
    /// that the program links under another.
    modules: HashMap<String, (Arc<Entry>, HashMap<String, String>)>,
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
            .filter(|f| matches!(f.body, Body::Given))
            .collect();
        if given.is_empty() {
            return Ok(out);
        }
        let _p = vyrn_frontend::prof::phase("std rows: read");
        // Each declaration's name in `program`, by its written name and module.
        let linked: HashMap<(&str, &str), &str> = (program.spellings.decls.iter())
            .map(|(l, (w, m))| ((w.as_str(), m.as_str()), l.as_str()))
            .collect();
        let mut seen = Seen::default();
        let mut entries: BTreeMap<&str, Arc<Entry>> = BTreeMap::new();
        for f in &given {
            let m = f.module.as_deref().expect("a given function is imported");
            let owner = loader::generated_importer(m).unwrap_or(m);
            if !entries.contains_key(owner) {
                let e = entry(owner, program, opts, resolver, engine, &mut seen)?;
                entries.insert(owner, e);
            }
            if row(&entries[owner], program, f).is_none() {
                return Err(vec![missing(&f.name, m)]);
            }
        }
        for (owner, e) in entries {
            let names: HashMap<String, String> = (e.homes.iter())
                .filter_map(|(own, (w, m))| {
                    let to = *linked.get(&(w.as_str(), m.as_str())).unwrap_or(&w.as_str());
                    (to != own).then(|| (own.clone(), to.to_string()))
                })
                .collect();
            for s in &e.sources {
                let mut s = s.clone();
                source_names(&mut s, &mut |n| rename(&names, n));
                out.sources.push(s);
            }
            out.modules.insert(owner.to_string(), (e, names));
        }
        out.sources.sort_by_key(source_key);
        out.sources.dedup_by(|a, b| source_key(a) == source_key(b));
        if program
            .functions
            .iter()
            .any(|f| f.is_gen && matches!(f.body, Body::Written(_)))
        {
            for f in &given {
                let s = out.stored(program, f).expect("checked above");
                out.fenced.insert(f.name.clone(), s.fenced);
            }
        }
        Ok(out)
    }

    /// The row of the given function `f` of `program`, under the program's
    /// names; `None` for a written one.
    fn stored(&self, program: &Program, f: &Function) -> Option<Stored> {
        let m = f.module.as_deref()?;
        let (e, names) = self
            .modules
            .get(loader::generated_importer(m).unwrap_or(m))?;
        let mut s = row(e, program, f)?.clone();
        each_name(&mut s, &mut |n| rename(names, n));
        Some(s)
    }

    /// The row the placer serves the given function `func` of `program`, with
    /// the memo key it is served under; `None` for a written one.
    pub(crate) fn serve(
        &self,
        program: &Program,
        func: &Function,
    ) -> Option<(JudgmentKey, Judgment)> {
        let mut s = self.stored(program, func)?;
        s.judgment.calls.clear();
        let m = func.module.clone()?;
        let hash = program.module_hashes.get(&m).cloned().unwrap_or_default();
        Some(((m, hash, func.name.clone()), s.judgment))
    }

    /// The calls the worklist follows out of the given function `f` of
    /// `program`.
    ///
    /// # Panics
    ///
    /// Where `f` has no row: [`Given::read`] gives every given function one,
    /// and only the placer's lowering walks a given body.
    pub(crate) fn calls(
        &self,
        program: &Program,
        f: &Function,
    ) -> Vec<(String, HashMap<String, Type>)> {
        let s = self.stored(program, f);
        let s = s.unwrap_or_else(|| panic!("the given `{}` has no stored row", f.name));
        (s.judgment.calls.into_iter())
            .map(|(c, subst)| (c, subst.into_iter().collect()))
            .collect()
    }
}

/// The row entry `e` holds of the given function `f` of `program`.
fn row<'e>(e: &'e Entry, program: &Program, f: &Function) -> Option<&'e Stored> {
    let m = f.module.as_deref()?;
    e.rows.get(m)?.get(program.spellings.written(&f.name))
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
    /// The entries this thread read or wrote, by key. A key names the
    /// compiler, the module and its text, so an entry here is never stale.
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
    seen: &mut Seen,
) -> Result<Arc<Entry>, Vec<Diagnostic>> {
    let hash = program.module_hashes.get(owner).map_or("", String::as_str);
    let key = entry_key(owner, hash);
    if let Some(e) = ENTRIES.with(|c| c.borrow().get(&key).cloned()) {
        return Ok(e);
    }
    let cached = (!loader::gen_cache_off())
        .then(|| resolver.gen_cache_get(&key))
        .flatten()
        .and_then(|text| Entry::read(&key, &text))
        .filter(|e| e.fresh(program, resolver, seen));
    let e = match cached {
        Some(e) => e,
        None => {
            let e = Entry::written(owner, opts, resolver, engine)?;
            if !loader::gen_cache_off() {
                resolver.gen_cache_put(&key, &e.render(&key));
            }
            e
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

/// One module's stored rows.
struct Entry {
    /// The files the module's own load read.
    inputs: Inputs,
    /// The text hash of each generated module the own load linked, by key.
    generated: Vec<(String, String)>,
    /// The declaration each name the rows hold names in the own load: its
    /// written name and module key.
    homes: BTreeMap<String, (String, String)>,
    /// Each given function's row, by module key and written name.
    rows: BTreeMap<String, BTreeMap<String, Stored>>,
    /// The function values the module's bodies store and pass.
    sources: Vec<StoredSource>,
}

impl Entry {
    /// Whether every file still holds what the own load read, and `program`
    /// links each generated module with the text the own load linked.
    fn fresh(&self, program: &Program, resolver: &dyn ModuleResolver, seen: &mut Seen) -> bool {
        let same = |(k, h): &(String, String)| program.module_hashes.get(k) == Some(h);
        self.generated.iter().all(same) && self.inputs.fresh(resolver, seen)
    }

    /// Loads module `m` as its own root and keeps the row of each function an
    /// importer takes as given: the module's own, and those of each module
    /// its generator imports wrote.
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
    ) -> Result<Entry, Vec<Diagnostic>> {
        let _p = vyrn_frontend::prof::phase("std rows: write");
        let unread = || vec![missing("its inputs", m)];
        let text = resolver.read(m).map_err(|_| unread())?;
        let session = Session::rows_writer();
        let own = loader::rows_writer(opts, m, session.clone());
        let (loaded, _) = crate::pipeline::load_warned(&text, m, &own, resolver, engine);
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
        for f in program
            .functions
            .iter()
            .filter(|f| loader::is_given(f, root, opts))
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
        Ok(Entry {
            inputs: Inputs::now(files, resolver).ok_or_else(unread)?,
            generated: (generated.into_iter())
                .map(|(k, h)| (k.clone(), h.clone()))
                .collect(),
            homes,
            rows,
            sources,
        })
    }

    /// The entry as text: the tag line, then [`Inputs::render`], the
    /// generated modules, the homes, each row and each source.
    fn render(&self, key: &str) -> String {
        let mut body = String::new();
        self.inputs.render(&mut body);
        body.push_str(&format!("{}\n", self.generated.len()));
        for (k, h) in &self.generated {
            body.push_str(&format!("{k}\t{h}\n"));
        }
        body.push_str(&format!("{}\n", self.homes.len()));
        for (n, (w, m)) in &self.homes {
            body.push_str(&format!("{n}\t{w}\t{m}\n"));
        }
        let rows = (self.rows.iter()).flat_map(|(m, r)| r.iter().map(move |(n, s)| (n, m, s)));
        body.push_str(&format!("{}\n", rows.clone().count()));
        for (name, module, s) in rows {
            let j = &s.judgment;
            let impure = s.fenced.impure.as_deref().unwrap_or("-");
            body.push_str(&format!(
                "{name}\t{module}\t{}\t{}\t{}\t{impure}\n",
                j.frames.len(),
                j.summary.len(),
                j.calls.len(),
            ));
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
        }
        body.push_str(&format!("{}\n", self.sources.len()));
        for s in &self.sources {
            write_type(&mut body, &s.sig);
            let named = s.named.as_deref().unwrap_or("-");
            match &s.lambda {
                Some(l) => body.push_str(&format!(
                    "\t{named}\t{}\t{}\t{}\n",
                    l.defined_in, l.line, l.col
                )),
                None => body.push_str(&format!("\t{named}\n")),
            }
        }
        let tag = loader::gen_cache_tag(key, body.as_bytes());
        format!("{TAG} {tag}\n{body}")
    }

    /// Reads what [`Entry::render`] wrote under `key`; `None` for a foreign,
    /// damaged or other-format entry.
    fn read(key: &str, text: &str) -> Option<Entry> {
        let (head, body) = text.split_once('\n')?;
        let tag = head.strip_prefix(TAG)?.strip_prefix(' ')?;
        if loader::gen_cache_tag(key, body.as_bytes()) != tag {
            return None;
        }
        let mut lines = body.split('\n');
        let mut line = || lines.next();
        let count = |l: Option<&str>| l?.parse::<usize>().ok();
        let inputs = Inputs::read(&mut line)?;
        let generated = (0..count(line())?)
            .map(|_| {
                let (k, h) = line()?.split_once('\t')?;
                Some((k.to_string(), h.to_string()))
            })
            .collect::<Option<_>>()?;
        let mut homes = BTreeMap::new();
        for _ in 0..count(line())? {
            let mut f = line()?.split('\t');
            let (n, w, m) = (f.next()?, f.next()?, f.next()?);
            homes.insert(n.to_string(), (w.to_string(), m.to_string()));
        }
        let mut rows = BTreeMap::new();
        for _ in 0..count(line())? {
            let mut h = line()?.split('\t');
            let (name, module) = (h.next()?.to_string(), h.next()?.to_string());
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
            let at = rows.entry(module).or_insert_with(BTreeMap::new);
            at.insert(name, Stored { judgment, fenced });
        }
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
        Some(Entry {
            inputs,
            generated,
            homes,
            rows,
            sources,
        })
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
