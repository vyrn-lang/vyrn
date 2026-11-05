//! Std's stored rows: the judgment of each non-generic function of a std
//! module, written by the module's own load and served by `vyrn check`'s
//! placer in place of building and judging the function's body.
//!
//! A row is what the judgment memo keeps of a body ([`Judgment`]): its frames
//! as the effect judgment walks them and the answer the kernel read. The placer
//! serves it as it serves a memo entry (`core::augment`): the frames join the
//! effect judgment and the waiting rules, and a body whose answer moved is
//! built and judged again.
//!
//! An entry is one module's rows. It sits beside the generator cache's entries,
//! keyed by the compiler's identity, the module's key and its text hash,
//! authenticated by the per-user secret, and records every file its load read
//! ([`Inputs`]). A foreign, damaged or stale entry reads as a miss, and the miss
//! writes it again. `VYRN_NO_GEN_CACHE` reads and writes none.
//!
//! [`Given::read`] is the one constructor of the table the placer reads, so
//! every row in it has passed every check there.

use std::collections::{BTreeMap, HashMap};

use vyrn_frontend::ast::{Function, Program, Type};
use vyrn_frontend::checker::Recorded;
use vyrn_frontend::effects::{Call, Effect, Effects, Mark, Walked};
use vyrn_frontend::gen::GenEngine;
use vyrn_frontend::loader::{self, Inputs, LoadOptions, ModuleResolver, Seen};
use vyrn_frontend::movecheck::{Judgment, JudgmentKey};
use vyrn_frontend::session::Session;

/// The format word that opens an entry.
const TAG: &str = "stdrows1";

/// The rows the placer serves, by module key and function name, each with
/// its module's hash. Empty for every host but `vyrn check`.
#[derive(Default)]
pub struct Given(HashMap<String, HashMap<String, (String, Judgment)>>);

impl Given {
    /// The rows of every std module `program` links, read from their entries
    /// and written where an entry is missing or stale, keeping only the rows
    /// of functions whose stored judgment still holds in `program`:
    ///
    /// - the function is std's, non-generic, neither `gen` nor `extern`, and its
    ///   module is not one the manifest's `proved` names;
    /// - every name its frames read means in `program` what it meant in the
    ///   module's own load, renamed or not;
    /// - every protocol method call the check dispatched in it goes to an impl a
    ///   std module declares.
    pub(crate) fn read(
        program: &Program,
        record: &Recorded,
        opts: &LoadOptions,
        resolver: &dyn ModuleResolver,
        engine: Option<&GenEngine>,
    ) -> Given {
        let mut out = Given::default();
        if loader::gen_cache_off() {
            return out;
        }
        let _p = vyrn_frontend::prof::phase("std rows: read");
        let std_impl_methods = std_impl_methods(program, opts);
        let foreign_dispatch: std::collections::HashSet<&str> = (record.stored.dispatched.iter())
            .filter(|(_, to)| !std_impl_methods.contains(to.as_str()))
            .map(|(from, _)| from.as_str())
            .collect();
        let mut seen = Seen::default();
        let mut by_module: BTreeMap<&str, Vec<&Function>> = BTreeMap::new();
        for f in &program.functions {
            let Some(m) = f.module.as_deref() else {
                continue;
            };
            if eligible(f) && loader::in_std(m, opts) && !program.proved.contains(&f.module) {
                by_module.entry(m).or_default().push(f);
            }
        }
        for (m, fns) in by_module {
            let Some(hash) = program.module_hashes.get(m) else {
                continue;
            };
            let key = entry_key(m, hash);
            let entry = (resolver.gen_cache_get(&key))
                .and_then(|text| Entry::read(&key, &text))
                .filter(|e| e.fresh(program, resolver, &mut seen))
                .or_else(|| {
                    let e = Entry::written(m, opts, resolver, engine)?;
                    resolver.gen_cache_put(&key, &e.render(&key));
                    Some(e)
                });
            let Some(mut entry) = entry else { continue };
            for f in fns {
                let Some(j) = entry.rows.remove(&f.name) else {
                    continue;
                };
                if foreign_dispatch.contains(f.name.as_str())
                    || !names(&j).all(|n| program.spellings.decls.get(n) == entry.renamed.get(n))
                {
                    continue;
                }
                let rows = out.0.entry(m.to_string()).or_default();
                rows.insert(f.name.clone(), (hash.clone(), j));
            }
        }
        out
    }

    /// Takes the row the placer serves `func` at `type_args`, with the memo
    /// key it is served under. A generic's instance has none.
    pub(crate) fn serve(
        &mut self,
        func: &Function,
        type_args: &[Type],
    ) -> Option<(JudgmentKey, Judgment)> {
        if !type_args.is_empty() {
            return None;
        }
        let m = func.module.as_deref()?;
        let (hash, j) = self.0.get_mut(m)?.remove(&func.name)?;
        Some(((m.to_string(), hash, func.name.clone()), j))
    }
}

/// Whether a function can have a row: one body, judged once for every
/// program, and not run at generation.
fn eligible(f: &Function) -> bool {
    f.type_params.is_empty() && !f.is_gen && !f.is_extern && !f.name.starts_with(loader::MEM_PREFIX)
}

/// The flattened name of every method of an impl a std module declares.
fn std_impl_methods(program: &Program, opts: &LoadOptions) -> std::collections::HashSet<String> {
    let mut out = std::collections::HashSet::new();
    for im in &program.impls {
        let Some(key) = vyrn_frontend::types::type_key(&im.ty) else {
            continue;
        };
        for f in im.methods.iter().chain(&im.places) {
            if f.module.as_deref().is_some_and(|m| loader::in_std(m, opts)) {
                out.insert(vyrn_frontend::types::impl_method_name(
                    &im.protocol,
                    &key,
                    &f.name,
                ));
            }
        }
    }
    out
}

/// The cache key of module `m`'s entry at text hash `hash`.
fn entry_key(m: &str, hash: &str) -> String {
    let id = vyrn_frontend::gen::compiler_identity();
    let seed = format!("{TAG}\0{id}\0{m}\0{hash}");
    format!("rows-{}", vyrn_frontend::hash::sha256_hex(seed.as_bytes()))
}

/// Every name a row's frames and answer read, which [`Given::read`] holds to
/// one meaning across the two loads.
fn names(j: &Judgment) -> impl Iterator<Item = &str> {
    let frames = j.frames.iter().flat_map(|f| {
        let calls = f.calls.iter().flat_map(|c| {
            let mut heads = Vec::new();
            c.through.iter().for_each(|t| type_heads(t, &mut heads));
            std::iter::once(c.callee.as_str()).chain(heads)
        });
        std::iter::once(f.name.as_str())
            .chain(f.writes.iter().map(String::as_str))
            .chain(calls)
    });
    let state = (j.state.iter().flatten())
        .flat_map(|(c, gs)| std::iter::once(c.as_str()).chain(gs.iter().map(String::as_str)));
    frames.chain(state)
}

/// Every type name in `t`, outermost first.
fn type_heads<'t>(t: &'t Type, out: &mut Vec<&'t str>) {
    if let Type::Named(n) | Type::App(n, _) = t {
        out.push(n);
    }
    t.children().for_each(|c| type_heads(c, out));
}

/// One module's stored rows.
struct Entry {
    /// The files the module's own load read.
    inputs: Inputs,
    /// The text hash of each generated module the own load linked, by key.
    generated: Vec<(String, String)>,
    /// What each name a row reads that the own load renamed was written as,
    /// and in which module ([`vyrn_frontend::ast::Spellings::decls`]).
    renamed: HashMap<String, (String, String)>,
    /// The judgment of each function the entry holds, by its linked name.
    rows: BTreeMap<String, Judgment>,
}

impl Entry {
    /// Whether every file still holds what the own load read, and `program`
    /// links each generated module with the text the own load linked.
    fn fresh(&self, program: &Program, resolver: &dyn ModuleResolver, seen: &mut Seen) -> bool {
        let same = |(k, h): &(String, String)| program.module_hashes.get(k) == Some(h);
        self.generated.iter().all(same) && self.inputs.fresh(resolver, seen)
    }

    /// Loads module `m` as its own root and keeps the judgment of each of its
    /// functions that earned no refusal and holds no fact summary. `None` where
    /// a file it read cannot be read back.
    fn written(
        m: &str,
        opts: &LoadOptions,
        resolver: &dyn ModuleResolver,
        engine: Option<&GenEngine>,
    ) -> Option<Entry> {
        let _p = vyrn_frontend::prof::phase("std rows: write");
        let text = resolver.read(m).ok()?;
        let session = Session::rows_writer();
        let own = LoadOptions {
            std_root: opts.std_root.clone(),
            session: Some(session.clone()),
            ..LoadOptions::default()
        };
        let (loaded, _) = crate::pipeline::load_warned(&text, m, &own, resolver, engine);
        // A module its own load refuses holds no row, and its entry says so,
        // so a warm check does not load it again.
        let Ok((program, _)) = loaded else {
            return Some(Entry {
                inputs: Inputs::now([m.to_string()], resolver)?,
                generated: Vec::new(),
                renamed: HashMap::new(),
                rows: BTreeMap::new(),
            });
        };
        let mut judged = session.take_judgments();
        let root = program.spellings.root.as_str();
        let mut rows = BTreeMap::new();
        for f in (program.functions.iter()).filter(|f| f.module.is_none() && eligible(f)) {
            let key = (root.to_string(), String::new(), f.name.clone());
            let Some(j) = judged.remove(&key) else {
                continue;
            };
            if j.verdict.is_empty() && j.summary.is_empty() && j.frames.iter().all(encodable) {
                rows.insert(f.name.clone(), j);
            }
        }
        let renamed = (rows.values().flat_map(names))
            .filter_map(|n| Some((n.to_string(), program.spellings.decls.get(n)?.clone())))
            .collect();
        // A generated module has no file; the program that reads the entry
        // holds it again, under the same key.
        let (generated, files): (Vec<_>, Vec<_>) = (program.module_hashes.iter())
            .partition(|(k, _)| loader::generated_importer(k).is_some());
        let files = (files.into_iter().map(|(k, _)| k.clone())).chain([root.to_string()]);
        Some(Entry {
            inputs: Inputs::now(files, resolver)?,
            generated: generated
                .into_iter()
                .map(|(k, h)| (k.clone(), h.clone()))
                .collect(),
            renamed,
            rows,
        })
    }

    /// The entry as text: the tag line, then [`Inputs::render`], the renamed
    /// names, and each row.
    fn render(&self, key: &str) -> String {
        let mut body = String::new();
        self.inputs.render(&mut body);
        body.push_str(&format!("{}\n", self.generated.len()));
        for (k, h) in &self.generated {
            body.push_str(&format!("{k}\t{h}\n"));
        }
        body.push_str(&format!("{}\n", self.renamed.len()));
        let renamed: BTreeMap<_, _> = self.renamed.iter().collect();
        for (n, (w, m)) in renamed {
            body.push_str(&format!("{n}\t{w}\t{m}\n"));
        }
        body.push_str(&format!("{}\n", self.rows.len()));
        for (name, j) in &self.rows {
            body.push_str(&format!("{name}\t{}\n", j.frames.len()));
            for (f, state) in j.frames.iter().zip(&j.state) {
                write_frame(&mut body, f);
                let state: Vec<String> = (state.iter())
                    .map(|(c, gs)| format!("{c}={}", gs.join(",")))
                    .collect();
                body.push_str(&state.join("\t"));
                body.push('\n');
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
        let inputs = Inputs::read(&mut line)?;
        let generated = (0..line()?.parse::<usize>().ok()?)
            .map(|_| {
                let (k, h) = line()?.split_once('\t')?;
                Some((k.to_string(), h.to_string()))
            })
            .collect::<Option<_>>()?;
        let mut renamed = HashMap::new();
        for _ in 0..line()?.parse::<usize>().ok()? {
            let mut f = line()?.split('\t');
            let (n, w, m) = (f.next()?, f.next()?, f.next()?);
            renamed.insert(n.to_string(), (w.to_string(), m.to_string()));
        }
        let mut rows = BTreeMap::new();
        for _ in 0..line()?.parse::<usize>().ok()? {
            let (name, n) = line()?.split_once('\t')?;
            let (mut frames, mut state) = (Vec::new(), Vec::new());
            for _ in 0..n.parse::<usize>().ok()? {
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
            let j = Judgment {
                verdict: Vec::new(),
                frames,
                state,
                summary: Vec::new(),
            };
            rows.insert(name.to_string(), j);
        }
        Some(Entry {
            inputs,
            generated,
            renamed,
            rows,
        })
    }
}

/// Whether [`write_type`] can state every type the frame's calls run through.
fn encodable(f: &Walked) -> bool {
    f.calls.iter().all(|c| {
        c.through
            .as_ref()
            .is_none_or(|t| write_type(&mut String::new(), t))
    })
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
        let mut ty = String::new();
        match &c.through {
            Some(t) => {
                write_type(&mut ty, t);
            }
            None => ty.push('-'),
        }
        let born = c.born as u8;
        out.push_str(&format!(
            "{}\t{}\t{born}\t{mark}\t{}\n",
            c.callee,
            c.line,
            ty.trim_end()
        ));
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
            t => {
                let mut tokens = t.split(' ');
                let ty = read_type(&mut tokens)?;
                Some(tokens.next().is_none().then_some(ty)?)
            }
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

/// Writes `t` as prefix tokens, each followed by a space, a list after its
/// length. Returns false for a type outside the forms a function value's
/// signature takes in std, which leaves the function without a row.
fn write_type(out: &mut String, t: &Type) -> bool {
    fn tok(out: &mut String, s: &str) {
        out.push_str(s);
        out.push(' ');
    }
    fn list(out: &mut String, ts: &[Type]) -> bool {
        tok(out, &ts.len().to_string());
        ts.iter().all(|t| write_type(out, t))
    }
    match t {
        Type::Int => tok(out, "i64"),
        Type::Float => tok(out, "f64"),
        Type::Bool => tok(out, "bool"),
        Type::Str => tok(out, "str"),
        Type::Unit => tok(out, "unit"),
        Type::IntN { bits, signed } => {
            tok(out, &format!("n{bits}{}", if *signed { "s" } else { "u" }))
        }
        Type::Named(n) | Type::Param(n) => {
            tok(
                out,
                if matches!(t, Type::Named(_)) {
                    "named"
                } else {
                    "param"
                },
            );
            tok(out, n);
        }
        Type::App(n, ts) => {
            tok(out, "app");
            tok(out, n);
            return list(out, ts);
        }
        Type::Fn(ps, r) => {
            tok(out, "fn");
            return list(out, ps) && write_type(out, r);
        }
        Type::Record(fs) => {
            tok(out, "record");
            tok(out, &fs.len().to_string());
            return fs.iter().all(|f| {
                tok(out, &f.name);
                write_type(out, &f.ty)
            });
        }
        Type::Enum(vs) => {
            tok(out, "enum");
            tok(out, &vs.len().to_string());
            return vs.iter().all(|v| {
                tok(out, &v.name);
                list(out, &v.payload)
            });
        }
        Type::Array(b) => {
            tok(out, "array");
            return write_type(out, b);
        }
        Type::Map(k, v) => {
            tok(out, "map");
            return write_type(out, k) && write_type(out, v);
        }
        _ => return false,
    }
    true
}

/// Reads one type [`write_type`] wrote from `tokens`.
fn read_type<'t>(tokens: &mut dyn Iterator<Item = &'t str>) -> Option<Type> {
    fn count<'t>(tokens: &mut dyn Iterator<Item = &'t str>) -> Option<usize> {
        tokens.next()?.parse().ok()
    }
    fn list<'t>(tokens: &mut dyn Iterator<Item = &'t str>) -> Option<Vec<Type>> {
        (0..count(tokens)?).map(|_| read_type(tokens)).collect()
    }
    let word = tokens.next()?;
    Some(match word {
        "i64" => Type::Int,
        "f64" => Type::Float,
        "bool" => Type::Bool,
        "str" => Type::Str,
        "unit" => Type::Unit,
        "named" => Type::Named(tokens.next()?.to_string()),
        "param" => Type::Param(tokens.next()?.to_string()),
        "app" => Type::App(tokens.next()?.to_string(), list(tokens)?),
        "fn" => Type::Fn(list(tokens)?, Box::new(read_type(tokens)?)),
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
        "array" => Type::Array(Box::new(read_type(tokens)?)),
        "map" => Type::Map(Box::new(read_type(tokens)?), Box::new(read_type(tokens)?)),
        _ => {
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
    })
}
