//! The effect lattice — RFC-0125 §3 M6, the table.
//!
//! The lattice is stated ONCE, as the table in RFC-0125 §3 M6. This file is
//! that table's DATA: [`ATOMS`] is its second column and [`Effect::gen`] is
//! its last one. `tests/effects.rs` reads the table out of the RFC and holds
//! both equal to it, which is the direction the RFC asks for — the RFC is the
//! statement, the code is the copy.
//!
//! The JUDGMENT that joins a body's atoms with its callees' sets is
//! `vyrn_lower::effects`, because it walks the named core. The table is here
//! instead, in the frontend, for two readers that cannot see `vyrn-lower`:
//! RFC-0021's generation fence (`checker::check_comptime_purity`, which runs
//! mid-check over the AST and must answer for a `gen fn` no lowering
//! instantiates) and [`crate::floor`]. `vyrn-lower` re-exports every name
//! here, so the judgment still spells the lattice `effects::`.

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

impl Effect {
    /// The `gen` column of the lattice table (RFC-0125 §3 M6): whether an atom of
    /// this effect may run at GENERATION time, inside RFC-0021's deterministic,
    /// cache-keyed sandbox.
    ///
    /// The column is a fact about the sandbox, not about a target. A generator is
    /// re-run only when its cache key changes, so an effect the key cannot name
    /// makes the same build behave two ways: `print` in a `gen fn` writes to the
    /// compiler's stdout and is silent on a cache hit (RFC-0125 §3 M6 finding 4),
    /// a clock read is a different answer every build, `args` is the compiler's
    /// command line and not the program's.
    ///
    /// `alloc` and `trap` are yes because the sandbox is an interpreter that
    /// allocates and can fail. `gen-only` is yes because it exists nowhere else.
    /// `fs-read` and `fs-list` are yes because they route through the loader's
    /// resolver and are recorded as cache inputs — with the one exception
    /// [`GEN_ATOM_OVERRIDES`] names.
    pub fn gen(self) -> bool {
        match self {
            Effect::Alloc | Effect::FsRead | Effect::FsList | Effect::Trap | Effect::GenOnly => {
                true
            }
            Effect::ReadInput
            | Effect::WriteOutput
            | Effect::FsWrite
            | Effect::Args
            | Effect::Clock
            | Effect::Random
            | Effect::Extern
            | Effect::Serve
            | Effect::Spawn => false,
        }
    }
}

/// The one cell of the `gen` column that differs from its row — RFC-0125 §3 M6
/// finding 5, decided and unchanged: the split is the ROUTE, not the effect.
/// `readFile` goes through the loader's resolver and is recorded as a cache
/// input; `readFileBytes` does not, so a generation that read bytes would be
/// cached on a key that does not name them. The cell becomes its row's when
/// `readFileBytes` takes the resolver route, and not before.
pub const GEN_ATOM_OVERRIDES: &[(&str, bool)] = &[("readFileBytes", false)];

/// Whether the atom `name` may run at generation time: its row's cell, unless
/// [`GEN_ATOM_OVERRIDES`] gives it its own. A name that is no atom is not the
/// lattice's business and is allowed here.
pub fn gen_allows(name: &str) -> bool {
    if let Some((_, cell)) = GEN_ATOM_OVERRIDES.iter().find(|(n, _)| *n == name) {
        return *cell;
    }
    atom(name).is_none_or(Effect::gen)
}

/// Why the fence refuses an atom of this effect, for the diagnostic's "it …"
/// clause. Named per ROW, so the reason a generator is refused is the row and
/// not the name it happened to spell.
///
/// The clock and entropy rows have their own words because RFC-0103 M2's
/// host-boundary rule says `hostNowMillis` and its two neighbours are not host
/// imports at all — the runtime shim implements them on every target — so
/// calling them "the extern" was wrong (RFC-0125 §3 M6 finding 13).
pub fn gen_refusal(name: &str) -> Option<String> {
    if gen_allows(name) {
        return None;
    }
    Some(match atom(name) {
        Some(Effect::Clock) => "reads the clock".to_string(),
        Some(Effect::Random) => "reads entropy".to_string(),
        _ => format!("calls `{name}`"),
    })
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

    /// The generation fence's whole list is the `gen` column now, so this pins
    /// the cells the fifth slice changed and the one it kept split.
    #[test]
    fn the_gen_column_answers_by_row_and_by_override() {
        // finding 4: one effect, one cell. `print` is refused with the rest.
        for n in ["print", "writeStdout", "trace", "error"] {
            assert_eq!(gen_refusal(n).as_deref(), Some(&*format!("calls `{n}`")));
        }
        // finding 13: the reason is the row, not "the extern".
        assert_eq!(
            gen_refusal("hostNowMillis").as_deref(),
            Some("reads the clock")
        );
        assert_eq!(
            gen_refusal("hostRandomSeed").as_deref(),
            Some("reads entropy")
        );
        // finding 5: the row is split by the route, and stays split.
        assert!(gen_allows("readFile"));
        assert!(!gen_allows("readFileBytes"));
        // A resolver-mediated listing, an allocation and a trap are the sandbox.
        for n in ["listDir", "runtime$malloc", "panic", "moduleInterface"] {
            assert!(gen_allows(n), "`{n}`");
        }
        // Not an atom, not the lattice's business.
        assert!(gen_allows("main"));
    }
}
