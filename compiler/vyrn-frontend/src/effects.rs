//! The effect lattice as data.
//!
//! [`atoms`] is the table's second column and [`Effect::gen`] its last;
//! `tests/effects.rs` holds both equal to the table it reads. The judgment that joins a body's atoms with its callees' sets is
//! `vyrn_lower::effects`, which walks the named core. The table lives here for
//! two readers that cannot see `vyrn-lower`: the generation fence
//! (`checker::check_comptime_purity`, over the AST, for a `gen fn` no lowering
//! instantiates) and [`crate::floor`]. `vyrn-lower` re-exports every name.

/// One effect, in the table's order.
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
    /// A host function imported by name is called. No atom: the
    /// caller resolves an `extern fn` declaration to it.
    Extern,
    /// A stream is handed to the serving host.
    Serve,
    /// A module-state binding is read or written. No atom: the core
    /// spells it as a global place, and the body that names one carries it.
    ModuleState,
    /// The path may end in a trap.
    Trap,
    /// The compiler's own state is read; exists at generation time only.
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
        Effect::ModuleState,
        Effect::Trap,
        Effect::GenOnly,
    ];

    /// The name the table and every printout use.
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
            Effect::ModuleState => "module-state",
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

/// The atoms that are no builtin: `(callee name, effect)`. Every builtin's
/// atom is its row's [`crate::prelude::Builtin::effect`]; [`atoms`] is the
/// table's second column. A callee that is neither an atom nor a user function
/// is pure.
///
/// `extern` has no row: whoever builds the call graph resolves an `extern fn`
/// declaration and asks [`extern_effect`]. A function the program declares
/// under a host-boundary name is an ordinary function.
const RUNTIME_ATOMS: &[(&str, Effect)] = &[
    ("runtime$malloc", Effect::Alloc),
    ("mem$grow", Effect::Alloc),
    ("runtime$trap", Effect::Trap),
    ("mem$trap", Effect::Trap),
];

/// Every atom and its effect.
pub fn atoms() -> impl Iterator<Item = (&'static str, Effect)> {
    crate::prelude::builtins()
        .iter()
        .filter_map(|b| Some((b.name, b.effect?)))
        .chain(RUNTIME_ATOMS.iter().copied())
}

/// Returns the effect of a call to `f` if it is an `extern fn`: a
/// host-boundary extern ([`crate::trap::HOST_EXTERNS`]) reads the clock or
/// entropy, which the runtime implements on every target; any other is a
/// host import. `None` for a function with a body.
pub fn extern_effect(f: &crate::ast::Function) -> Option<Effect> {
    let host = crate::trap::HOST_EXTERNS
        .iter()
        .find(|(n, ..)| *n == f.name);
    f.is_extern
        .then(|| host.map_or(Effect::Extern, |(.., e)| *e))
}

/// Returns the effect of the atom `name`.
pub fn atom(name: &str) -> Option<Effect> {
    match crate::prelude::builtin(name) {
        Some(b) => b.effect,
        None => RUNTIME_ATOMS
            .iter()
            .find(|(n, _)| *n == name)
            .map(|(_, e)| *e),
    }
}

impl Effect {
    /// The `gen` column: whether an atom of this effect may run at generation
    /// time, in the deterministic, cache-keyed generator sandbox.
    ///
    /// A generator re-runs only when its cache key changes, so an effect the key
    /// cannot name makes one build behave two ways: `print` writes to the
    /// compiler's stdout and is silent on a cache hit, a clock read differs per
    /// build, `args` is the compiler's command line, and module state holds
    /// whatever generation order left there. `alloc` and `trap` are yes because
    /// the sandbox allocates and can fail, and `gen-only` exists nowhere else.
    /// `fs-read` and `fs-list` go through the loader's resolver and are recorded
    /// as cache inputs, except as [`GEN_ATOM_OVERRIDES`] says.
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
            | Effect::ModuleState => false,
        }
    }
}

/// The one `gen` cell that differs from its row. `readFile` goes through the
/// loader's resolver and is recorded as a cache input; `readFileBytes` does
/// not, so a generation that read bytes would be cached on a key that does not
/// name them. The override goes when `readFileBytes` takes the resolver route.
pub const GEN_ATOM_OVERRIDES: &[(&str, bool)] = &[("readFileBytes", false)];

/// Returns whether the atom `name` may run at generation time: its row's cell
/// unless [`GEN_ATOM_OVERRIDES`] overrides it. A name that is no atom is
/// allowed.
pub fn gen_allows(name: &str) -> bool {
    if let Some((_, cell)) = GEN_ATOM_OVERRIDES.iter().find(|(n, _)| *n == name) {
        return *cell;
    }
    atom(name).is_none_or(Effect::gen)
}

/// Returns why the fence refuses `name`, for the diagnostic's "it ..." clause;
/// the reason follows the row, not the spelling.
pub fn gen_refusal(name: &str) -> Option<String> {
    (!gen_allows(name)).then(|| refusal(atom(name), name))
}

/// [`gen_refusal`] for a call to the declared `extern fn` `f`; `None` for a
/// function with a body. Every extern is refused.
pub fn extern_gen_refusal(f: &crate::ast::Function) -> Option<String> {
    Some(refusal(Some(extern_effect(f)?), &f.name))
}

/// The "it ..." clause for a call to `name` of effect `e`. The clock and
/// entropy have their own words: a host-boundary extern is no host import.
fn refusal(e: Option<Effect>, name: &str) -> String {
    match e {
        Some(Effect::Clock) => "reads the clock".to_string(),
        Some(Effect::Random) => "reads entropy".to_string(),
        Some(Effect::Extern) => format!("calls the extern `{name}`"),
        // Name the word the source wrote: `@info` is the sugar's internal spelling of
        // `log.info(..)`, and no source can lex it.
        _ => format!("calls `{}`", crate::prelude::method_surface(name)),
    }
}

/// What the effect judgment reads of one frame, before any callee is
/// resolved. `vyrn_lower::effects` walks a frame into one and joins them; the
/// editor's judgment memo keeps a served body's, since no build of it runs.
#[derive(Debug, Clone, PartialEq)]
pub struct Walked {
    /// The frame's name, which keys a lambda frame by its line.
    pub name: String,
    /// The atoms of the frame's own statements, calls left out.
    pub own: Effects,
    /// The globals the frame's own statements store into.
    pub writes: std::collections::BTreeSet<String>,
    pub calls: Vec<Call>,
    /// The frames of the lambdas it builds, as offsets forward in
    /// `Body::frames` order.
    pub lambdas: Vec<usize>,
}

/// One call a frame makes.
#[derive(Debug, Clone, PartialEq)]
pub struct Call {
    pub callee: String,
    /// The type of the function value the call runs through; `None` for a
    /// call by name.
    pub through: Option<crate::ast::Type>,
    pub line: usize,
    /// Whether the call binds an owned result, which is the frame's own
    /// allocation when the callee is no user body.
    pub born: bool,
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn every_atom_names_one_effect_once() {
        for (n, _) in RUNTIME_ATOMS {
            assert!(crate::prelude::builtin(n).is_none(), "`{n}` is a builtin");
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
    }

    /// The fence reads the `gen` column, by row and by override.
    #[test]
    fn the_gen_column_answers_by_row_and_by_override() {
        // One effect, one cell: `print` is refused with the rest.
        for n in ["print", "writeStdout"] {
            assert_eq!(gen_refusal(n).as_deref(), Some(&*format!("calls `{n}`")));
        }
        // A log level is keyed by its call-site spelling, and the reason names the
        // word the program wrote.
        for n in ["trace", "error"] {
            assert_eq!(
                gen_refusal(&format!("@{n}")).as_deref(),
                Some(&*format!("calls `{n}`"))
            );
        }
        // A host-boundary spelling is no atom: its declaration decides.
        assert!(gen_allows("hostRandomSeed"));
        // The route splits the fs-read row.
        assert!(gen_allows("readFile"));
        assert!(!gen_allows("readFileBytes"));
        // A resolver-mediated listing, an allocation and a trap are allowed.
        for n in ["listDir", "runtime$malloc", "panic", "moduleInterface"] {
            assert!(gen_allows(n), "`{n}`");
        }
        // Not an atom: allowed.
        assert!(gen_allows("main"));
    }
}
