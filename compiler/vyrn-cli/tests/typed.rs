//! The typed-by-construction judgment (`vyrn_lower::typed`) over the
//! corpus: every example and every entry point of an example project with a
//! `vyrn.json`. For every store into a validated place it asks what produced
//! the value. The type's constructor, a name already of the type, a proven
//! literal and a constant into a sized integer are the rule; any other producer
//! is a finding. Which crossings are validated is `vyrn_frontend::validate`'s
//! question, not this file's.
//!
//! `VYRN_TYPED_DUMP=<file>:<fn>` prints one body's judged stores. `<file>` is a
//! corpus file name, a substring of one, or a path to any `.vyrn` file.

mod common;

use std::collections::{BTreeMap, BTreeSet};
use std::path::{Path, PathBuf};

use judgment::Step;
use vyrn_frontend::ast::{Program, Type, TypeDecl};
use vyrn_frontend::loader::DiskResolver;

fn repo_root() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d.pop();
    d
}

fn slash(p: &Path) -> String {
    p.to_string_lossy().replace('\\', "/")
}

fn manifest(dir: &Path) -> Option<vyrn_frontend::manifest::Manifest> {
    vyrn_frontend::manifest::find(dir).ok().flatten()
}

fn load(path: &Path, project: Option<&Path>) -> Result<Program, String> {
    let src = std::fs::read_to_string(path).map_err(|e| e.to_string())?;
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(slash(&repo_root().join("std"))),
        artifacts: project.and_then(manifest).and_then(|m| m.artifacts),
        expansions: vyrn_frontend::project::Expansions::shared(),
        ..Default::default()
    };
    vyrn_lower::load(
        &src,
        &slash(path),
        &opts,
        &DiskResolver,
        Some(&*vyrn_genwasm::engine()),
    )
    .map_err(|d| d.first().map(|d| d.render()).unwrap_or_default())
}

/// Every root to judge: the corpus `tests/effects.rs` judges.
fn corpus() -> Vec<(PathBuf, Option<PathBuf>)> {
    let ex = repo_root().join("examples");
    let mut files: Vec<PathBuf> = std::fs::read_dir(&ex)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.extension().is_some_and(|x| x == "vyrn"))
        .collect();
    files.sort();
    let mut out: Vec<(PathBuf, Option<PathBuf>)> = files.into_iter().map(|p| (p, None)).collect();
    let mut dirs: Vec<PathBuf> = std::fs::read_dir(&ex)
        .unwrap()
        .filter_map(|e| e.ok().map(|e| e.path()))
        .filter(|p| p.join("vyrn.json").is_file())
        .collect();
    dirs.sort();
    for dir in dirs {
        let Some(m) = manifest(&dir) else { continue };
        let mut entries: BTreeSet<String> = BTreeSet::new();
        if let Some(a) = &m.audience {
            entries.extend(a.entries.iter().map(|(p, _, _)| p.clone()));
        }
        if let Some(a) = &m.artifacts {
            entries.extend(a.list.iter().map(|a| a.entry.clone()));
        }
        for e in entries {
            out.push((PathBuf::from(e), Some(dir.clone())));
        }
    }
    assert!(!out.is_empty(), "no examples found");
    out
}

fn decls(p: &Program) -> BTreeMap<String, TypeDecl> {
    p.type_decls
        .iter()
        .map(|d| (d.name.clone(), d.clone()))
        .collect()
}

/// The type one place step reaches from `base`. `None` for a generic parameter
/// or a type this program does not declare.
fn step_ty(
    base: Option<&Type>,
    s: Step,
    types: &BTreeMap<String, TypeDecl>,
    globals: &BTreeMap<String, Type>,
) -> Option<Type> {
    let resolve = |t: &Type| -> Type {
        match t {
            Type::Named(n) => types.get(n).map(|d| d.base.clone()).unwrap_or(t.clone()),
            other => other.clone(),
        }
    };
    match s {
        Step::Global(g) => globals.get(g).cloned(),
        Step::Field(f) => match resolve(base?) {
            Type::Record(fields) => fields.iter().find(|x| x.name == f).map(|x| x.ty.clone()),
            _ => None,
        },
        Step::Elem => match resolve(base?) {
            Type::Array(t) | Type::ArrayN(t, _) | Type::SmallArray(t, _) => Some(*t),
            // A String indexes as bytes.
            Type::Str => Some(Type::IntN {
                bits: 8,
                signed: false,
            }),
            _ => None,
        },
        Step::Key => match resolve(base?) {
            Type::Map(_, v) => Some(*v),
            _ => None,
        },
    }
}

/// Every body of `program` the core builds, the module-state initializer
/// included. The caller holds the projection memo open.
fn bodies_of(program: &Program) -> Vec<vyrn_frontend::core::Body> {
    let lowered = vyrn_lower::lower(program);
    let world = vyrn_lower::analyze(program);
    let own = &world.ownership;
    let mut bodies = Vec::new();
    for inst in &lowered.instances {
        if let Ok(b) = vyrn_lower::core::build(program, inst, &own) {
            bodies.push(b);
        }
    }
    if !program.globals.is_empty() {
        if let Ok(b) = vyrn_lower::core::build_module_state(
            program,
            &own,
            &Default::default(),
            &lowered.globals,
        ) {
            bodies.push(b);
        }
    }
    bodies
}

/// Judges `refs`, with `program`'s declarations answering which types are
/// validated and what each place step holds. A call's type is the core's
/// (`Rhs::Call::ret`).
fn judge(program: &Program, refs: &[&vyrn_frontend::core::Body]) -> judgment::Judged {
    let types = decls(program);
    let globals: BTreeMap<String, Type> = program
        .globals
        .iter()
        .filter_map(|g| g.ty.clone().map(|t| (g.name.clone(), t)))
        .collect();
    let map = types.iter().map(|(k, v)| (k.clone(), v.clone())).collect();
    judgment::judge(refs, &mut |to| validated(to, &map), &mut |base, s| {
        step_ty(base, s, &types, &globals)
    })
}

/// Holds even where the declarations type no step to the place (an
/// unannotated global).
#[test]
fn a_read_of_a_validated_place_produces_its_type() {
    let dir = std::env::temp_dir().join("vyrn-typed-read");
    std::fs::create_dir_all(&dir).unwrap();
    let path = dir.join("read.vyrn");
    std::fs::write(
        &path,
        "type Age = Int64 where value >= 18\n\
         type Roster = { ages: Array<Age> }\n\
         let mut shared = Roster { ages: [] }\n\
         fn main() -> Int64 {\n\
         shared.ages.push(Age(41))\n\
         let mut a = Age(20)\n\
         a = 30\n\
         print(shared.ages[0])\n\
         print(a)\n\
         return 0\n\
         }\n",
    )
    .unwrap();
    let program = load(&path, None).unwrap();
    let bodies = bodies_of(&program);
    let refs: Vec<&vyrn_frontend::core::Body> = bodies.iter().flat_map(|b| b.frames()).collect();
    let judged = judge(&program, &refs);
    let kind_of = |producer: &str| {
        judged
            .stores
            .iter()
            .filter(|s| refs[s.body].name == "main" && s.producer == producer)
            .map(|s| s.how.kind())
            .collect::<Vec<_>>()
    };
    assert_eq!(kind_of("shared.ages[]"), ["by-name"]);
    // Three proven constants, each a `Callee::Proven` row; `a = 30` also
    // stores the row's temporary into `a`, the fourth store.
    assert_eq!(kind_of("@lit"), ["by-literal"; 4]);
}

#[test]
fn an_uncalled_generic_calls_a_protocol_member_no_impl_answers() {
    let dir = common::scratch("typed-uncalled");
    let path = dir.join("uncalled.vyrn");
    std::fs::write(
        &path,
        "protocol Weigh {
         fn weigh(self) -> Int64
         }
         fn weighOne<T: Weigh>(x: T) -> Int64 {
         return x.weigh()
         }
         fn main() -> Int64 {
         return 0
         }
",
    )
    .unwrap();
    let out = common::vyrn().arg("check").arg(&path).output().unwrap();
    assert!(
        out.status.success(),
        "{}",
        String::from_utf8_lossy(&out.stderr)
    );
}

#[test]
#[ignore = "walks the whole corpus; run explicitly: cargo test -p vyrn-cli --test typed -- --ignored"]
fn the_typed_judgment_over_the_corpus() {
    std::thread::Builder::new()
        .stack_size(vyrn_frontend::trap::DEEP_STACK_BYTES)
        .spawn(run_corpus)
        .unwrap()
        .join()
        .unwrap();
}

fn run_corpus() {
    // Generation is the driver's engine; without it an example that imports
    // through a generator fails to load and the gate silently measures a
    // smaller corpus.
    let dump = std::env::var("VYRN_TYPED_DUMP").ok();
    // The LAST colon: a Windows path carries one after its drive letter.
    let dump_target = dump.as_deref().and_then(|d| d.rsplit_once(':'));
    let mut roots = corpus();
    if let Some((file, _)) = dump_target {
        let p = PathBuf::from(file);
        if p.is_file() && !roots.iter().any(|(r, _)| r == &p) {
            let dir = p.parent().map(Path::to_path_buf);
            roots = vec![(p, dir.filter(|d| d.join("vyrn.json").is_file()))];
        } else {
            roots.retain(|(r, _)| slash(r).contains(file));
        }
    }

    let mut by_kind: BTreeMap<&'static str, usize> = BTreeMap::new();
    let mut by_type: BTreeMap<String, usize> = BTreeMap::new();
    let mut findings: Vec<String> = Vec::new();
    let mut unjudged = 0usize;
    let mut programs = 0usize;
    let mut judged = 0usize;
    for (path, project) in &roots {
        let Ok(program) = load(path, project.as_deref()) else {
            continue;
        };
        programs += 1;
        let file = path.file_name().unwrap().to_string_lossy().to_string();
        let bodies = bodies_of(&program);
        let refs: Vec<&vyrn_frontend::core::Body> =
            bodies.iter().flat_map(|b| b.frames()).collect();
        let judgement = judge(&program, &refs);
        unjudged += judgement.unjudged;
        judged += judgement.stores.len();
        for s in &judgement.stores {
            *by_kind.entry(s.how.kind()).or_default() += 1;
            *by_type.entry(s.ty.clone()).or_default() += 1;
            if s.how.is_finding() {
                let root = format!("{}/", slash(&repo_root()));
                findings.push(format!(
                    "{}:{} {} — `{}` into `{}`: `{}` = {}",
                    refs[s.body]
                        .file
                        .as_deref()
                        .map(|f| f.trim_start_matches(&root).to_string())
                        .unwrap_or_else(|| file.clone()),
                    s.line,
                    refs[s.body].name,
                    s.how.kind(),
                    s.ty,
                    s.place,
                    s.producer
                ));
            }
        }
        if let Some((_, want)) = dump_target {
            for (i, b) in refs.iter().enumerate() {
                if b.name != *want {
                    continue;
                }
                eprintln!("{file} {}:", b.name);
                for s in judgement.stores.iter().filter(|s| s.body == i) {
                    eprintln!(
                        "  {:5} {:16} {} : {} = {}",
                        s.line,
                        s.how.kind(),
                        s.place,
                        s.ty,
                        s.producer
                    );
                }
            }
        }
    }

    eprintln!(
        "typed by construction over the corpus: {programs} programs, {judged} stores into a \
         validated place judged, {unjudged} unjudged (a store whose producer is a read of a \
         place these declarations resolve no type for)"
    );
    for (k, n) in &by_kind {
        eprintln!("  {n:5}  {k}");
    }
    for (t, n) in &by_type {
        eprintln!("  type {n:5}  {t}");
    }
    findings.sort();
    for f in findings.iter().take(60) {
        eprintln!("  finding: {f}");
    }
    // Zero is a fact about this corpus, not a refusal in the language: a raw
    // value entering a validated slot stays legal and every engine runs the
    // constructor at it.
    const RATCHET: usize = 0;
    assert_eq!(
        findings.len(),
        RATCHET,
        "a store into a validated place with a raw producer; the ratchet is          {RATCHET} and this one is worth reading before it is raised: {}",
        findings[0]
    );
    assert!(judged > 0, "the judgment judged nothing");
}

/// Names the rule `to` carries: a named type's `where` through `validate::of`,
/// or a sized integer's narrowing rows. Whether a store crossed it is the
/// judgment's.
fn validated(to: &Type, types: &std::collections::HashMap<String, TypeDecl>) -> Option<String> {
    match to {
        // `of`, not `required`: `required` exempts a store of a name already
        // of the type, which must still be judged and land as `by-name`.
        Type::Named(n) => vyrn_frontend::validate::of(types.get(n)).map(|d| d.name.clone()),
        // Likewise every sized-integer store is judged; `validate::narrows`
        // tells a same-width producer apart.
        Type::IntN { .. } => Some(to.to_string()),
        _ => None,
    }
}

/// The census instrument: for every store into a validated place, what
/// produced the value. A name of a validated type is produced only by that
/// type's constructor, a name already of the type, or a literal the checker
/// proved. Every name is bound once (`St::Let`), so it is a use-def walk:
/// [`judge`](judgment::judge) records what bound each name and judges the
/// producer of every store into a place the caller calls validated. A sized
/// integer is judged by width: a producer of the same width and signedness
/// crosses nothing.
mod judgment {
    use std::collections::HashMap;

    use vyrn_frontend::ast::Type;
    use vyrn_frontend::core::{rows, Arg, Body, Callee, Name, Place, Rhs, St, Val};

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
        /// The type's own constructor, `Age(n)`, where the predicate runs. A
        /// record literal of a validated record type is the same answer: every
        /// engine runs the generated constructor at it (a cross-field
        /// `where`).
        Constructor,
        /// A name already of the type.
        ByName,
        /// A literal, or another crossing the checker proved at compile time
        /// ([`Callee::Proven`]).
        Literal,
        /// A primitive over literals only, into a sized integer. The checker
        /// ranges it where the two share a sign (`-200` into an `Int8` is
        /// refused); otherwise it wraps, as `-1` into a `UInt8` is 255.
        Constant,
        /// A raw value reaching a validated slot, by kind: what the judgment
        /// refuses.
        Finding(&'static str),
    }

    impl How {
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
        /// The name of the declaration whose producer must have run.
        pub ty: String,
        /// A callee's name, a place or a name as the core spells it, or `@lit`,
        /// `@prim` or `@make`.
        pub producer: String,
        pub line: usize,
        pub how: How,
    }

    /// The judgment's answer.
    #[derive(Debug, Default)]
    pub struct Judged {
        /// Every store into a validated place, in body order.
        pub stores: Vec<Store>,
        /// Stores into a sized integer whose producer has no type the caller
        /// resolves, such as a read of a generic parameter's place. Counted, not
        /// guessed; zero over the corpus.
        pub unjudged: usize,
    }

    /// Judges every store in `bodies`, each a frame of the core.
    ///
    /// `validated` maps a destination type to the name of the declaration whose
    /// producer must have run for it (a named type with a `where`, or a sized
    /// integer), or `None` where no rule applies. `step` says what a place holds.
    /// Both answer from the program's declarations, which the judgment does not
    /// hold.
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
            rows(&b.stmts).for_each(|(s, _)| w.stmt(s));
        }
        out
    }

    struct Walk<'a, 'b> {
        body: &'a Body,
        index: usize,
        /// What bound each name: the use-def edge.
        born: HashMap<Name, &'a Rhs>,
        validated: &'b mut dyn FnMut(&Type) -> Option<String>,
        step: &'b mut dyn FnMut(Option<&Type>, Step) -> Option<Type>,
        out: &'b mut Judged,
    }

    impl<'a> Walk<'a, '_> {
        fn stmt(&mut self, s: &'a St) {
            match s {
                St::Let(n, rhs) => {
                    self.born.insert(*n, rhs);
                    let info = &self.body.names[n.index()];
                    let ty = info.ty.clone();
                    self.judge_store(ty.clone(), info.source.clone(), info.line, rhs, Some(ty));
                }
                St::Store {
                    place, value, line, ..
                } => {
                    if let Some(ty) = self.place_ty(place) {
                        // The producer is the `let` that bound the name in this
                        // frame, or the name itself when a parameter, capture or
                        // arm binder bound it.
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
                                outside = Rhs::Val(value.clone());
                                &outside
                            }
                        };
                        let place = self.spell(place);
                        let named = match value {
                            Val::Name(n) => Some(self.body.names[n.index()].ty.clone()),
                            Val::Lit(_) => None,
                        };
                        self.judge_store(ty, place, *line, rhs, named);
                    }
                }
                St::If { .. }
                | St::Loop { .. }
                | St::Block { .. }
                | St::Switch { .. }
                | St::Do { .. }
                | St::Drop(..)
                | St::Row { .. }
                | St::Break { .. }
                | St::Continue { .. }
                | St::Return { .. }
                | St::Trap
                | St::Check(_) => {}
            }
        }

        /// `to` is the place's type, `rhs` what the store was given, and `named`
        /// the type of the name `rhs` was bound to.
        ///
        /// A read converts nothing, so where the declarations cannot resolve the
        /// place it reads, `named` answers. A place of a validated type holds only
        /// what this judgment let in, so a read of it is a producer of that type.
        fn judge_store(
            &mut self,
            to: Type,
            place: String,
            line: usize,
            rhs: &Rhs,
            named: Option<Type>,
        ) {
            let from = match rhs {
                Rhs::Read(_) | Rhs::Take(_) => self.rhs_ty(rhs).or(named),
                _ => self.rhs_ty(rhs),
            };
            let ctor = matches!(rhs, Rhs::Call { callee, .. } if last(callee) == spelling(&to));
            // A narrowing is a producer of another width, so a guess would read
            // every untyped integer store as one.
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
                Rhs::Call {
                    kind: Callee::Proven,
                    ..
                } => How::Literal,
                _ if ctor => How::Constructor,
                Rhs::Val(Val::Lit(_)) => How::Literal,
                // Only into a sized integer: a named type's predicate owes a
                // producer whatever the operands are.
                Rhs::Prim(_, vs, _)
                    if matches!(to, Type::IntN { .. })
                        && !vs.is_empty()
                        && vs.iter().all(|v| matches!(v, Val::Lit(_))) =>
                {
                    How::Constant
                }
                // A producer already of the type, or of the same integer width and
                // signedness (`Int` and `Int64`), crosses nothing
                // (`validate::required`, `validate::narrows`).
                _ if from
                    .as_ref()
                    .is_some_and(|f| *f == to || same_width(f, &to)) =>
                {
                    How::ByName
                }
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
                    Rhs::Call {
                        kind: Callee::Proven,
                        args,
                        ..
                    } if matches!(args.as_slice(), [(Arg::Val(Val::Lit(_)), _)]) => "@lit".into(),
                    Rhs::Call { callee, .. } => callee.clone(),
                    Rhs::Prim(..) => "@prim".into(),
                    Rhs::Make(..) => "@make".into(),
                    Rhs::Read(p) | Rhs::Take(p) => self.spell(p),
                    Rhs::Val(Val::Name(n)) => self.body.names[n.index()].source.clone(),
                    Rhs::Val(Val::Lit(_)) => "@lit".into(),
                },
                line,
                how,
            });
        }

        /// The type a right-hand side produces, where the core or the
        /// declarations name one. A literal and a record literal have none.
        fn rhs_ty(&mut self, rhs: &Rhs) -> Option<Type> {
            match rhs {
                Rhs::Val(Val::Name(n)) => Some(self.body.names[n.index()].ty.clone()),
                Rhs::Read(p) | Rhs::Take(p) => self.place_ty(p),
                Rhs::Call { ret, .. } => ret.clone(),
                Rhs::Prim(_, _, ty) => ty.clone(),
                Rhs::Make(..) | Rhs::Val(Val::Lit(_)) => None,
            }
        }

        fn place_ty(&mut self, p: &Place) -> Option<Type> {
            match p {
                Place::Name(n) => Some(self.body.names[n.index()].ty.clone()),
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

        fn spell(&self, p: &Place) -> String {
            match p {
                Place::Name(n) => self.body.names[n.index()].source.clone(),
                Place::Global(g) => g.clone(),
                Place::Field(b, f) => format!("{}.{f}", self.spell(b)),
                Place::Elem(b, _) => format!("{}[]", self.spell(b)),
                Place::Key(b, _) => format!("{}{{}}", self.spell(b)),
            }
        }
    }

    fn same_width(from: &Type, to: &Type) -> bool {
        vyrn_frontend::validate::width(from).is_some()
            && vyrn_frontend::validate::width(to).is_some()
            && !vyrn_frontend::validate::narrows(from, to)
    }

    /// The name of a type's own producer: a named type's name, else its spelling,
    /// which names its conversion (`UInt8`).
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
}
