//! The effect judgment — RFC-0125 §2.2, judgment 2 (M6, first slice).
//!
//! Over a [`Body`] in the named core: **a body's effect set is the join of
//! its own atoms and its callees' sets**, taken to a fixpoint so recursion
//! is handled. An atom is a builtin or a host import the runtime declares;
//! the lattice is stated ONCE, as the table in RFC-0125 §3 M6, and
//! [`ATOMS`] here is that table's second column. `tests/effects.rs` reads
//! the table out of the RFC and refuses to run if the two differ, which is
//! the direction the RFC asks for: the code derives from the table.
//!
//! The judgment knows nothing about the surface language. It sees a call by
//! its callee's name, the spawn marker on a call (`Rhs::Call::spawn`), an
//! owned name born of a primitive or a literal (an allocation), and a trap.
//! It does not see inside a lambda body (the core lowers a lambda as a read
//! of its captures, and judges the body nowhere — RFC-0125 §3 M6 finding 2).
//!
//! One inclusion check lives here: the spawn-isolation rule of RFC-0004 §Q4,
//! stated as RFC-0125 §2.2 states it — a spawned callee's set within
//! [`Effects::SPAWN_ALLOWS`]. The other two, a body within its target's set
//! and within its context's, still wait on the floor and the fence;
//! `tests/effects.rs` compares the set with both passes, per function.

use std::collections::HashMap;

use crate::core::{Body, Rhs, St};

/// One effect. The order is the table's order in RFC-0125 §3 M6.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash)]
pub enum Effect {
    /// Heap is allocated: an owned name born of a primitive or a literal, or
    /// the allocator itself.
    Alloc,
    /// Standard input is read.
    ReadInput,
    /// Standard output or error is written.
    WriteOutput,
    /// A file is read.
    FsRead,
    /// A file is written, renamed or synced.
    FsWrite,
    /// A directory is listed.
    FsList,
    /// The command line is read.
    Args,
    /// The clock is read.
    Clock,
    /// Entropy is read.
    Random,
    /// A host function imported by name (RFC-0012) is called. Not an atom by
    /// name: the harness resolves an `extern fn` declaration to it.
    Extern,
    /// A stream is handed to the serving host (RFC-0074 M3a).
    Serve,
    /// A task is started (RFC-0004 §Q4): a call the core marks `spawn`.
    Spawn,
    /// The path may end in a trap.
    Trap,
    /// The compiler's own state is read; exists at generation time only
    /// (RFC-0021, RFC-0054).
    GenOnly,
}

impl Effect {
    pub const ALL: [Effect; 14] = [
        Effect::Alloc,
        Effect::ReadInput,
        Effect::WriteOutput,
        Effect::FsRead,
        Effect::FsWrite,
        Effect::FsList,
        Effect::Args,
        Effect::Clock,
        Effect::Random,
        Effect::Extern,
        Effect::Serve,
        Effect::Spawn,
        Effect::Trap,
        Effect::GenOnly,
    ];

    /// The name the RFC's table and every printout use.
    pub fn name(self) -> &'static str {
        match self {
            Effect::Alloc => "alloc",
            Effect::ReadInput => "read-input",
            Effect::WriteOutput => "write-output",
            Effect::FsRead => "fs-read",
            Effect::FsWrite => "fs-write",
            Effect::FsList => "fs-list",
            Effect::Args => "args",
            Effect::Clock => "clock",
            Effect::Random => "random",
            Effect::Extern => "extern",
            Effect::Serve => "serve",
            Effect::Spawn => "spawn",
            Effect::Trap => "trap",
            Effect::GenOnly => "gen-only",
        }
    }

    /// The inverse of [`Effect::name`].
    pub fn parse(s: &str) -> Option<Effect> {
        Effect::ALL.into_iter().find(|e| e.name() == s)
    }
}

/// A set of effects. `PURE` is the bottom of the lattice; join is union.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub struct Effects(u16);

impl Effects {
    pub const PURE: Effects = Effects(0);

    /// What a spawned callee may do (RFC-0004 §Q4: isolated means no I/O and
    /// no shared state; RFC-0125 §3 M6 finding 1): allocate and trap. A
    /// task's result is heap it owns, and a trap in a task is the task's.
    pub const SPAWN_ALLOWS: Effects =
        Effects((1 << Effect::Alloc as u16) | (1 << Effect::Trap as u16));

    pub fn of(e: Effect) -> Effects {
        Effects(1 << e as u16)
    }

    pub fn with(self, e: Effect) -> Effects {
        Effects(self.0 | Effects::of(e).0)
    }

    pub fn join(self, o: Effects) -> Effects {
        Effects(self.0 | o.0)
    }

    /// The effects in `self` that are not in `o`.
    pub fn minus(self, o: Effects) -> Effects {
        Effects(self.0 & !o.0)
    }

    pub fn has(self, e: Effect) -> bool {
        self.0 & Effects::of(e).0 != 0
    }

    pub fn is_pure(self) -> bool {
        self.0 == 0
    }

    pub fn iter(self) -> impl Iterator<Item = Effect> {
        Effect::ALL.into_iter().filter(move |e| self.has(*e))
    }
}

impl std::fmt::Display for Effects {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        if self.is_pure() {
            return f.write_str("pure");
        }
        let names: Vec<&str> = self.iter().map(Effect::name).collect();
        f.write_str(&names.join(", "))
    }
}

/// The atoms: `(callee name, effect)`. The second column of the lattice
/// table in RFC-0125 §3 M6, and nothing else — `tests/effects.rs` holds the
/// two equal. A callee not here and not a user function is pure.
///
/// `extern` has no row: an `extern fn` is a user declaration and is resolved
/// by whoever builds the call graph ([`Callee::Atom`]). The three
/// host-boundary externs of RFC-0043 ARE here, because the runtime declares
/// them on every target and they are a clock and a seed, not an import.
pub const ATOMS: &[(&str, Effect)] = &[
    ("runtime$malloc", Effect::Alloc),
    ("mem$grow", Effect::Alloc),
    ("readLine", Effect::ReadInput),
    ("print", Effect::WriteOutput),
    ("writeStdout", Effect::WriteOutput),
    ("trace", Effect::WriteOutput),
    ("debug", Effect::WriteOutput),
    ("info", Effect::WriteOutput),
    ("warn", Effect::WriteOutput),
    ("error", Effect::WriteOutput),
    ("readFile", Effect::FsRead),
    ("readFileBytes", Effect::FsRead),
    ("writeFile", Effect::FsWrite),
    ("writeFileBytes", Effect::FsWrite),
    ("renameFile", Effect::FsWrite),
    ("fsyncFile", Effect::FsWrite),
    ("listDir", Effect::FsList),
    ("listDirKinds", Effect::FsList),
    ("args", Effect::Args),
    ("hostNowMillis", Effect::Clock),
    ("hostMonotonicNanos", Effect::Clock),
    ("hostRandomSeed", Effect::Random),
    ("serveStream", Effect::Serve),
    ("panic", Effect::Trap),
    ("@panicAt", Effect::Trap),
    ("assert", Effect::Trap),
    ("assertEq", Effect::Trap),
    ("runtime$trap", Effect::Trap),
    ("mem$trap", Effect::Trap),
    ("moduleInterface", Effect::GenOnly),
    ("contractOf", Effect::GenOnly),
    ("lex", Effect::GenOnly),
    ("render", Effect::GenOnly),
    ("raw", Effect::GenOnly),
    ("rawAt", Effect::GenOnly),
    ("@codeText", Effect::GenOnly),
    ("@codeSplice", Effect::GenOnly),
];

/// The atom `name` is, if it is one.
pub fn atom(name: &str) -> Option<Effect> {
    ATOMS.iter().find(|(n, _)| *n == name).map(|(_, e)| *e)
}

/// What a callee's name resolves to, as the caller of [`judge`] sees the
/// program. The judgment itself resolves nothing: it does not know which
/// names are functions, which are `extern`, which are builtins.
#[derive(Debug, Clone)]
pub enum Callee {
    /// A builtin or host import with a known effect.
    Atom(Effects),
    /// User bodies, by index into the slice handed to [`judge`] — several
    /// when a name has several instances or several impls.
    Bodies(Vec<usize>),
    /// A builtin with no effect.
    Pure,
    /// A name the caller cannot attribute (a call through a function value).
    /// Judged as pure and reported, so the tally counts it.
    Unknown,
}

/// One `spawn` the judgment saw.
#[derive(Debug, Clone)]
pub struct Spawned {
    /// The body the spawn is in, by index.
    pub body: usize,
    pub callee: String,
    pub line: usize,
    /// The spawned callee's set, after the fixpoint.
    pub effects: Effects,
}

impl Spawned {
    /// What the spawn-isolation rule refuses: the callee's effects outside
    /// [`Effects::SPAWN_ALLOWS`]. Pure when the spawn is allowed.
    pub fn outside(&self) -> Effects {
        self.effects.minus(Effects::SPAWN_ALLOWS)
    }
}

/// The judgment's answer for a set of bodies.
#[derive(Debug, Default)]
pub struct Judged {
    /// Per body, in the order given.
    pub effects: Vec<Effects>,
    /// `(body index, callee name)` for every call nobody could attribute.
    pub unknown: Vec<(usize, String)>,
    /// Every spawn site, with its callee's set.
    pub spawns: Vec<Spawned>,
}

/// The effect set of every body in `bodies`, each the join of its own atoms
/// and its callees', to a fixpoint. `resolve` says what a callee name is.
pub fn judge(bodies: &[&Body], resolve: &mut dyn FnMut(&str) -> Callee) -> Judged {
    let mut own: Vec<Effects> = Vec::with_capacity(bodies.len());
    let mut edges: Vec<Vec<usize>> = Vec::with_capacity(bodies.len());
    let mut unknown = Vec::new();
    let mut spawns: Vec<(usize, String, usize, Vec<usize>)> = Vec::new();
    // One resolution per distinct name, not one per call site.
    let mut memo: HashMap<String, Callee> = HashMap::new();
    for (i, b) in bodies.iter().enumerate() {
        let mut w = Walk {
            body: b,
            own: Effects::PURE,
            edges: Vec::new(),
            unknown: Vec::new(),
            spawns: Vec::new(),
            resolve,
            memo: &mut memo,
        };
        w.stmts(&b.stmts);
        own.push(w.own);
        let mut e = w.edges;
        e.sort_unstable();
        e.dedup();
        edges.push(e);
        unknown.extend(w.unknown.into_iter().map(|n| (i, n)));
        spawns.extend(w.spawns.into_iter().map(|(c, l, idx)| (i, c, l, idx)));
    }
    // The fixpoint. Monotone over a finite lattice, so it ends; the corpus
    // needs a handful of rounds.
    let mut effects = own.clone();
    loop {
        let mut changed = false;
        for i in 0..bodies.len() {
            let mut e = effects[i];
            for &j in &edges[i] {
                e = e.join(effects[j]);
            }
            if e != effects[i] {
                effects[i] = e;
                changed = true;
            }
        }
        if !changed {
            break;
        }
    }
    let spawns = spawns
        .into_iter()
        .map(|(body, callee, line, idx)| Spawned {
            body,
            callee,
            line,
            effects: idx
                .iter()
                .map(|j| effects[*j])
                .fold(Effects::PURE, Effects::join),
        })
        .collect();
    Judged {
        effects,
        unknown,
        spawns,
    }
}

struct Walk<'a> {
    body: &'a Body,
    own: Effects,
    edges: Vec<usize>,
    unknown: Vec<String>,
    /// `(callee, line, callee bodies)` per spawn.
    spawns: Vec<(String, usize, Vec<usize>)>,
    resolve: &'a mut dyn FnMut(&str) -> Callee,
    memo: &'a mut HashMap<String, Callee>,
}

impl Walk<'_> {
    fn stmts(&mut self, stmts: &[St]) {
        for s in stmts {
            self.stmt(s);
        }
    }

    fn stmt(&mut self, s: &St) {
        match s {
            St::Let(n, rhs) => {
                let atom_call = self.rhs(rhs, self.body.names[*n as usize].line);
                // An owned name born of a primitive, a literal or a builtin
                // is an allocation. Born of a user call, the callee's own
                // set says whether it allocated or handed a parameter back.
                let born = match rhs {
                    Rhs::Prim(_) | Rhs::Make(_) => true,
                    Rhs::Call { .. } => atom_call,
                    Rhs::Val(_) | Rhs::Read(_) | Rhs::Take(_) => false,
                };
                if born && self.body.names[*n as usize].owned {
                    self.own = self.own.with(Effect::Alloc);
                }
            }
            St::Do(rhs, line) => {
                self.rhs(rhs, *line);
            }
            St::Trap => self.own = self.own.with(Effect::Trap),
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
            St::Store { .. }
            | St::Drop(_)
            | St::Row { .. }
            | St::Break { .. }
            | St::Continue { .. }
            | St::Return { .. } => {}
        }
    }

    /// What `callee` is, by name.
    fn callee(&mut self, callee: &str) -> Callee {
        match self.memo.get(callee) {
            Some(c) => c.clone(),
            None => {
                let c = (self.resolve)(callee);
                self.memo.insert(callee.to_string(), c.clone());
                c
            }
        }
    }

    /// Whether the right-hand side was a call that is not a user body — the
    /// caller's own allocation when the result is owned.
    fn rhs(&mut self, r: &Rhs, line: usize) -> bool {
        let Rhs::Call { callee, spawn, .. } = r else {
            return false;
        };
        if *spawn {
            self.own = self.own.with(Effect::Spawn);
        }
        let c = self.callee(callee);
        let (atom, idx) = match c {
            Callee::Atom(e) => {
                self.own = self.own.join(e);
                (true, Vec::new())
            }
            Callee::Bodies(idx) => {
                self.edges.extend(idx.iter().copied());
                (false, idx)
            }
            Callee::Pure => (true, Vec::new()),
            Callee::Unknown => {
                self.unknown.push(callee.clone());
                (true, Vec::new())
            }
        };
        if *spawn {
            self.spawns.push((callee.clone(), line, idx));
        }
        atom
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_atom_names_one_effect_once() {
        for (i, (n, _)) in ATOMS.iter().enumerate() {
            assert!(
                ATOMS[..i].iter().all(|(m, _)| m != n),
                "`{n}` is in ATOMS twice"
            );
        }
        for e in Effect::ALL {
            assert_eq!(Effect::parse(e.name()), Some(e));
        }
    }

    #[test]
    fn the_set_prints_in_table_order() {
        let s = Effects::of(Effect::Trap).with(Effect::Alloc);
        assert_eq!(s.to_string(), "alloc, trap");
        assert_eq!(Effects::PURE.to_string(), "pure");
        assert_eq!(
            s.minus(Effects::of(Effect::Alloc)),
            Effects::of(Effect::Trap)
        );
        assert_eq!(Effects::SPAWN_ALLOWS.to_string(), "alloc, trap");
    }
}
