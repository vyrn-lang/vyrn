//! A tree-walking interpreter for the v0.1 subset.
//!
//! This exists so Vyrn programs actually *run* today, with no LLVM. It is also
//! the executable reference semantics that the codegen backends must match.
//!
//! Control flow uses [`Ctrl`] in the error channel: a real error, or a
//! `?`-propagated early return of the whole function. This lets the `?` operator
//! (RFC-0005) short-circuit out of the middle of an expression.

use std::cell::RefCell;
use std::collections::HashMap;

/// The most Vyrn calls that may be in flight at once, in EVERY engine (audit
/// A5.3, RFC-0016 addendum).
///
/// A recursion limit is the language's, not the interpreter's. Without one the
/// three engines disagreed about the same program: at depth 30,000 the native
/// binary printed the answer while the reference semantics aborted with a Rust
/// runtime message, exit 127, no `file:line`. Counting the calls — here, and in
/// each backend's function prologue — is what makes the outcome the same
/// everywhere, and makes it a Vyrn diagnostic rather than a death.
///
/// 1,000 is what every engine reaches in every BUILD PROFILE, which is the part
/// the first number (10,000) got wrong. The interpreter spends ~8.5 KB of Rust
/// stack per Vyrn call in a release build and ~190 KB in a debug build — the
/// unoptimized `expr`/`stmt` frames keep every local of a large match alive — so
/// 10,000 fitted in release and died in debug, where CI runs the tests. A limit
/// only one profile honors is not a limit.
///
/// Measured on the debug build against [`INTERP_STACK_BYTES`], with this counter
/// lifted: depth 2,600 runs and 2,800 overflows, so 1,000 keeps 2.6x margin in
/// the profile that has the least. The native binary and `wasmtime` run past
/// 20,000 frames of an ordinary function in either profile. 1,000 is also where
/// CPython settles, and it is past what a recursive descent over real data
/// reaches: `.vyx` markup, a GraphQL selection set and a JSON document all nest
/// in the tens. Data nested deeper than that is data no engine should try — it
/// stops with the same diagnostic everywhere, which is the whole contract.
///
/// An `extern` is NOT counted: it is the host's frame, and no backend gives it a
/// Vyrn prologue. Neither is a lambda body, which has no name to call itself by
/// (RFC-0037) and so cannot recurse without passing through a named function.
pub const CALL_DEPTH_LIMIT: u32 = 1_000;

/// The most bytes one call frame may claim on the wasm backend's shadow stack.
///
/// A backend's stack is finite, and until this number existed nothing compared a
/// frame against it. The wasm backend's whole stack was one 64 KB page, so a
/// function with a 256-byte frame ran out of stack at depth 256 while
/// [`CALL_DEPTH_LIMIT`] said 1,000 and the other two engines reached it — the
/// program died there with `out of bounds memory access` at a wild address, and
/// stopped with the shared diagnostic everywhere else.
///
/// Bounding the frame is what makes the depth one number again.
/// `vyrn_codegen::wasm::STACK_BYTES` holds [`CALL_DEPTH_LIMIT`] of these, so at
/// every depth the counter admits the stack pointer is still above 0: the
/// counter is what stops the program, on every engine, with the same words. A
/// frame past this is refused when it is built, naming the function and its
/// line, because the backend that lays a frame out is the one that knows its
/// size.
///
/// 8 KB is 1.5x the largest frame the corpus builds (5,552 bytes, `createForm`
/// in `examples/shelf/boot.vyrn`), and the stack it implies costs 8,257,536
/// bytes of linear memory — 126 wasm pages a module reserves and touches only as
/// deep as it recurses.
pub const FRAME_LIMIT: u32 = 8 * 1024;

/// The most elements one array literal may have.
///
/// Half of [`FRAME_LIMIT`], over the eight bytes of an `Int64`: a literal is
/// built in a frame slot, and the widest element type an ordinary literal has is
/// what turns one bound into the other. HALF, because the slot is not all a
/// literal costs — the array it becomes needs its own slot in the same frame, so
/// a bound of a whole frame would let the checker admit a literal the backend
/// then refuses. Wider elements — a literal of records — are caught by the frame
/// bound itself, which knows the real stride.
///
/// The checker holds this rather than either backend, because the other half of
/// the defect is one no frame can express: the textual backend lowers a literal
/// to one `insertvalue` per element over an aggregate of the full width, so
/// 100,000 elements ran clang for 2 m 53 s and died `LLVM ERROR: out of memory`,
/// after `vyrn check` had said `ok` in 0.1 s. Refusing in the checker is what
/// makes `check` predict the build, and makes all three engines refuse the same
/// literal.
///
/// The corpus's largest literal has 24 elements, so this is 21x anything written
/// so far. A table longer than it belongs in a data segment rather than in
/// instructions, which is a lowering neither backend has yet.
pub const ARRAY_LIT_LIMIT: usize = FRAME_LIMIT as usize / 16;

/// How many `region` scopes may be open at once, in EVERY engine.
///
/// The two backends each keep a fixed stack of region records, so the number is
/// the length of an array in one and a reserved block in the other, and it is in
/// the trap's wording as well. It was written eight times across three engines
/// before this constant, three of those inside string literals; the backends'
/// comparisons had already drifted apart in signedness. One number, read by
/// everything that has an opinion about it.
pub const REGION_MAX: u32 = 64;

/// The Rust stack every thread that runs the interpreter reserves.
///
/// Reserving is cheap — the pages are virtual until a frame touches them — and
/// what is touched is [`CALL_DEPTH_LIMIT`] frames deep at worst: ~8.5 MB in a
/// release build, ~190 MB in a debug one. Both sit well inside this, which is
/// what gives the limit above room to be the same number in either profile.
pub const INTERP_STACK_BYTES: usize = 512 * 1024 * 1024;

/// A fast, non-cryptographic hasher for the interpreter's own maps.
///
/// Scope frames are `name -> Slot`, and a variable reference hashes its name on
/// every read — measured at ~150-235 ns per reference, which dominates every
/// interpreted program. Rust's default hasher is SipHash, chosen to resist
/// hash-flooding from untrusted keys; these keys are identifiers from the
/// program being run, so that protection buys nothing and costs the hot path.
/// FxHash (the algorithm rustc uses on its own interned strings) is a multiply
/// and a rotate per 8 bytes.
#[derive(Default, Clone, Copy)]
pub struct FxHasher {
    hash: u64,
}

impl FxHasher {
    const SEED: u64 = 0x51_7c_c1_b7_27_22_0a_95;

    #[inline]
    fn add(&mut self, word: u64) {
        self.hash = (self.hash.rotate_left(5) ^ word).wrapping_mul(Self::SEED);
    }
}

impl std::hash::Hasher for FxHasher {
    #[inline]
    fn write(&mut self, bytes: &[u8]) {
        let mut chunks = bytes.chunks_exact(8);
        for c in &mut chunks {
            self.add(u64::from_ne_bytes(c.try_into().unwrap()));
        }
        let rest = chunks.remainder();
        if !rest.is_empty() {
            let mut buf = [0u8; 8];
            buf[..rest.len()].copy_from_slice(rest);
            self.add(u64::from_ne_bytes(buf));
        }
    }
    #[inline]
    fn write_u8(&mut self, i: u8) {
        self.add(i as u64);
    }
    #[inline]
    fn write_usize(&mut self, i: usize) {
        self.add(i as u64);
    }
    #[inline]
    fn finish(&self) -> u64 {
        self.hash
    }
}

#[derive(Default, Clone, Copy)]
pub struct FxBuild;

impl std::hash::BuildHasher for FxBuild {
    type Hasher = FxHasher;
    #[inline]
    fn build_hasher(&self) -> FxHasher {
        FxHasher::default()
    }
}

/// The interpreter's scope frame: identifiers to slots, hashed cheaply.
type Frame = HashMap<String, Slot, FxBuild>;
use std::io::Write as _;

use crate::ast::*;

/// Name the exit an unwinding release walk is paying for — RFC-0101 M4's second
/// phase, and the one thing the release trace asks of this engine that it does
/// not ask of the compiled ones.
///
/// Both backends emit an early exit's walk AT the site, with the node in hand.
/// Here the walk happens as `Flow::Break` or `Ctrl::Return` propagates outward
/// through `Interp::block`, and neither signal carries a node — so the statement
/// that raised it leaves the site behind and each unwinding frame reads it.
/// Recording only: off unless a gate asked, and nothing the engine does depends
/// on it.
fn leaving<T>(exit: crate::own::trace::Exit, at: &T) {
    crate::own::trace::leaving(exit, at as *const T as usize);
}

/// An injected fixed value for a host-boundary extern (RFC-0043): the decimal
/// `Int64` in env var `key`, or `None` when unset/empty/unparsable. The parity
/// harness sets `VYRN_FIXED_TIME`/`VYRN_FIXED_SEED` so time/random examples are
/// byte-identical across the three backends (the native/wasi shims read the same
/// env via `strtoll`).
fn fixed_env_i64(key: &str) -> Option<i64> {
    match std::env::var(key) {
        Ok(s) if !s.is_empty() => s.trim().parse::<i64>().ok(),
        _ => None,
    }
}

// ---------------------------------------------------------------------------
// The host boundary, in the two shapes it has.
//
// Everywhere with an operating system underneath — native, `wasm32-wasip1` —
// output is a file descriptor, input is stdin, and the clock is `std::time`.
// `wasm32-unknown-unknown` (the browser playground, `compiler/vyrn-play`) has
// none of the three: writes to stdout go nowhere, stdin is always empty, and
// `std::time::SystemTime::now` PANICS. So each one is named once here and
// switched once, and every call site below is spelled the same in both.
//
// Nothing about a native build changes: the `cfg` arms below expand to the
// `println!`, `read_until` and `SystemTime` calls that were written inline.
// ---------------------------------------------------------------------------

/// One line of program output. `print`'s only sink.
macro_rules! vyrn_out {
    ($($arg:tt)*) => {{
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        crate::playhost::out_line(format_args!($($arg)*));
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        println!($($arg)*);
    }};
}

/// One line of log output, kept off stdout (RFC-0008).
macro_rules! vyrn_err {
    ($($arg:tt)*) => {{
        #[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
        crate::playhost::err_line(format_args!($($arg)*));
        #[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
        eprintln!($($arg)*);
    }};
}

/// The next raw line of stdin: bytes up to and including `\n`, or empty at EOF.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn host_read_line() -> Vec<u8> {
    use std::io::BufRead;
    // Locking the global stdin per call still streams: the buffer lives in the
    // shared handle, not the guard.
    let mut buf: Vec<u8> = Vec::new();
    let n = std::io::stdin()
        .lock()
        .read_until(b'\n', &mut buf)
        .unwrap_or(0);
    if n == 0 {
        buf.clear();
    }
    buf
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn host_read_line() -> Vec<u8> {
    crate::playhost::read_line()
}

/// Milliseconds since the Unix epoch, from the host clock.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn host_epoch_millis() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_millis() as i64)
        .unwrap_or(0)
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn host_epoch_millis() -> i64 {
    crate::playhost::now_ms()
}

/// Nanoseconds since the Unix epoch, from the host clock.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn host_epoch_nanos() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map(|d| d.as_nanos() as i64)
        .unwrap_or(0)
}

#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn host_epoch_nanos() -> i64 {
    crate::playhost::now_ms().saturating_mul(1_000_000)
}

/// Whether an `std::fs::rename` failure is a cross-device (`EXDEV`) rename —
/// surfaced as a distinct `IoError` per RFC-0044 rather than the generic write
/// error. `EXDEV` is 18 on Unix/wasi; Windows reports `ERROR_NOT_SAME_DEVICE`
/// (17). `writeAtomic` keeps its temp beside the target, so it never hits this;
/// a bare `renameFile` across mounts can.
fn is_cross_device(e: &std::io::Error) -> bool {
    match e.raw_os_error() {
        Some(code) if cfg!(windows) => code == 17,
        Some(code) => code == 18,
        None => false,
    }
}

/// A runtime value.
#[derive(Debug, Clone, PartialEq)]
pub enum Val {
    Int(i64),
    /// A sized integer (`Int8`/`Int16`/`Int32`). `v` is the logical value,
    /// sign-extended into `i64`; arithmetic wraps back to `bits`.
    IntN {
        v: i64,
        bits: u8,
        signed: bool,
    },
    /// A 64-bit float (`Float64`).
    Float(f64),
    /// A 32-bit float (`Float32`). Stored as `f32` so arithmetic rounds to single
    /// precision at each step, matching the native backend's `float` ops.
    Float32(f32),
    /// Four `Float32` lanes as one value (RFC-0083). Emulated lane-by-lane, and
    /// that is EXACT rather than approximate: each lane is an independent
    /// IEEE-754 single-precision operation, so nothing reassociates and the loop
    /// below produces the same bits a hardware `f32x4.add` does.
    F32x4([f32; 4]),
    /// Four `Int32` lanes as one value (RFC-0083 M3). `i32` and not the `i64`
    /// [`Val::IntN`] carries, because a vector lane has exactly one width and
    /// wrapping it back after every operation would be re-deriving what the
    /// narrower type already guarantees: `i32::wrapping_add` IS `i32x4.add`.
    I32x4([i32; 4]),
    /// Two `Float64` lanes as one value (RFC-0083 M4). Lane-by-lane again, and
    /// exact for the same reason: two independent double-precision operations
    /// reassociate with nothing.
    F64x2([f64; 2]),
    /// Four `Bool` lanes — a lane-wise comparison's result (RFC-0083 M2). Four
    /// `bool`s rather than an `[i32; 4]` of all-ones/all-zeros, because the type
    /// has no other inhabitants: the backends' bit patterns are their own
    /// business and there is nothing here to normalize.
    Mask32x4([bool; 4]),
    /// Two `Bool` lanes — an `F64x2` comparison's result (RFC-0083 M4). A second
    /// variant rather than a widened first one, for the reason the type is a
    /// second type: a mask is characterised by its lane count and lane width.
    Mask64x2([bool; 2]),
    Bool(bool),
    /// Copy-on-write, like [`Val::Array`]. Generators pass multi-KB source
    /// buffers by value all day; cloning one per reference and per call is the
    /// largest remaining copy in the interpreter. `Rc<String>` rather than
    /// `Rc<str>` so `Rc::make_mut` can still append in place — the accumulator
    /// fast path depends on growing a string without reallocating it.
    Str(std::rc::Rc<String>),
    Unit,
    /// An optional (RFC-0005): `Some(v)` or `None`.
    Option(Option<Box<Val>>),
    /// A result (RFC-0005): `(is_ok, payload)` — `Ok(v)` is `(true, v)`.
    Result(bool, Box<Val>),
    /// A structural record (RFC-0002): field name -> value, plus the declared
    /// type name it last crossed a boundary as (RFC-0084 M1).
    ///
    /// The name is what makes `impl Bump for Box` dispatchable here: the two
    /// compiled backends key on `type_key(static receiver type)`, and without a
    /// name the interpreter had nothing to key on. It is **optional** because a
    /// record that has not yet crossed a typed boundary genuinely has no name —
    /// a literal's type comes from its context, not from itself — and that is
    /// not an error, only a value no protocol call can be made on.
    ///
    /// Assigned by [`Interp::coerce`], so the key is DERIVED from the static
    /// type rather than guessed from the shape: a literal coerced into a
    /// differently-named type of the same shape dispatches as the type it was
    /// coerced to, which is what the checker told native/wasm. A literal starts
    /// out under its own name, which every boundary is then free to overwrite.
    Record(HashMap<String, Val>, Option<std::rc::Rc<str>>),
    /// A user-enum value (RFC-0002 §4): variant name + payload values.
    Enum(String, Vec<Val>),
    /// A growable array (`Vec`). Used linearly; `push` returns a new value.
    /// Copy-on-write, and — just as importantly — an IDENTITY.
    ///
    /// Cloning is O(1), so passing an array to a function no longer copies it.
    /// The pointer also gives builtins a cheap, exact key for per-buffer caches:
    /// `lineAt`/`colAt` memoize a line-start table on it, which hashing the
    /// contents per call could not do (that cost more than the scan it replaced).
    /// A cache that stores the `Rc` keeps the allocation alive, so an address
    /// cannot be recycled under a live cache entry.
    Array(std::rc::Rc<Vec<Val>>),
    /// A growable, insertion-ordered dictionary (RFC-0028). See [`MapVal`].
    Map(MapVal),
    /// An opaque `Code` fragment (RFC-0054): a sequence of rendered text pieces,
    /// some carrying an origin span. Produced only inside a generation context by
    /// `vyrn"…"` quotes / `raw` / `rawAt`, concatenated by `+`, and consumed by
    /// `render`. Never reaches a backend (gen-only).
    Code(Vec<CodePiece>),
    /// A function value (RFC-0023) — an internal, non-observable value produced
    /// when a lambda literal or a named function is passed to a `fn`-typed
    /// parameter. The checker guarantees it is never stored, returned, printed, or
    /// compared, so it never escapes into user-visible output; it exists only so
    /// the callee can invoke its `fn`-typed parameter. Native/wasm monomorphize it
    /// away entirely — this variant is the interpreter's dynamic stand-in, kept
    /// semantically identical by materializing captures at the outer call site.
    Fn(Box<FnVal>),
    /// A `Stream<T>` (RFC-0075 M2b). Until M2b this was a [`Val::Array`] under a
    /// different static type; it is now a producer, which is the whole milestone
    /// — an endless one exists, and `take(n)` over it reads n+1 elements rather
    /// than all of them.
    Stream(Box<StreamVal>),
}

/// A `Map<String, V>` (RFC-0028): the `(key, value)` pairs in first-insertion
/// order, plus a hash INDEX over them.
///
/// The `Vec` is the value — an update rewrites the pair in place, a remove shifts
/// later pairs down, a fresh insert appends — and it is what makes
/// iteration/encoding order deterministic and parity-stable. The `HashMap` beside
/// it holds each key's POSITION in that `Vec`, so a lookup stops being the linear
/// scan that made every map operation O(keys) and every program with distinct
/// keys quadratic in them (RFC-0104's k-nucleotide row, the same defect the two
/// compiled backends carried). It never decides an order; only the `pairs` do,
/// which is the whole reason the index is a second structure rather than a
/// different first one.
///
/// The index costs a second copy of each key. That is the oracle's trade and not
/// the compiled backends' — theirs index by position into a bucket array — and it
/// buys the same order of growth in the interpreter that generators run in.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct MapVal {
    pairs: Vec<(String, Val)>,
    idx: std::collections::HashMap<String, usize>,
}

impl MapVal {
    /// The value stored under `k`, or `None`.
    pub fn get(&self, k: &str) -> Option<&Val> {
        self.idx.get(k).map(|i| &self.pairs[*i].1)
    }

    /// Whether `k` has an entry — `m.has(k)`.
    pub fn contains(&self, k: &str) -> bool {
        self.idx.contains_key(k)
    }

    /// `m[k] = v`: overwrite in place on a hit (the entry keeps its position, so
    /// the order does not move), append on a miss.
    pub fn insert(&mut self, k: String, v: Val) {
        match self.idx.get(&k) {
            Some(i) => self.pairs[*i].1 = v,
            None => {
                self.idx.insert(k.clone(), self.pairs.len());
                self.pairs.push((k, v));
            }
        }
    }

    /// `m.remove(k)`: drop the entry, shifting the survivors down so
    /// first-insertion order holds for them — which is why a remove-then-insert
    /// moves a key to the end. Every survivor after the hole moved, so their
    /// positions are renumbered; the shift is already O(len), so this is free.
    pub fn remove(&mut self, k: &str) -> bool {
        let Some(i) = self.idx.remove(k) else {
            return false;
        };
        self.pairs.remove(i);
        for at in self.idx.values_mut() {
            if *at > i {
                *at -= 1;
            }
        }
        true
    }
}

impl std::ops::Deref for MapVal {
    type Target = [(String, Val)];
    fn deref(&self) -> &Self::Target {
        &self.pairs
    }
}

impl FromIterator<(String, Val)> for MapVal {
    /// Built by insertion, so a repeated key in the source updates in place and
    /// keeps its slot — the one policy every builder of a map shares.
    fn from_iter<T: IntoIterator<Item = (String, Val)>>(it: T) -> Self {
        let mut m = MapVal::default();
        for (k, v) in it {
            m.insert(k, v);
        }
        m
    }
}

/// The two shapes a [`Val::Stream`] can take (RFC-0075 M2b) — the interpreter's
/// spelling of the tagged header the two compiled backends lay out in six words.
#[derive(Debug, Clone, PartialEq)]
pub enum StreamVal {
    /// `fromArray(xs)`: a buffer that already exists, plus how far the consumer
    /// has read it.
    Buf(std::rc::Rc<Vec<Val>>, usize),
    /// `fromStep(slot, gen, f)`: the two words of a cursor its maker minted, the
    /// step, and whether the step has already answered `None` (after which it is
    /// never called again — a stream that ended stays ended).
    ///
    /// The two words used to index the interpreter's own cell slab (RFC-0004
    /// Path B). Since RFC-0090 M3 they index a `Slots` that lives in
    /// `std/stream`, so nothing here reads them — they are carried and handed
    /// back to the step, which is the only thing that knows what they mean.
    Step {
        slot: i64,
        gen: i64,
        step: Box<FnVal>,
        done: bool,
    },
}

/// One piece of a [`Val::Code`] fragment (RFC-0054). Either plain rendered text
/// (from a quote skeleton, a splice, or `raw`) or an origin-carrying region (from
/// `rawAt`) that `render` wraps in `//@origin path:line:col` … `//@origin end`.
#[derive(Debug, Clone, PartialEq)]
pub enum CodePiece {
    /// Verbatim rendered source text (no origin attribution).
    Text(String),
    /// A region derived from input at `path:line:col` — `render` brackets it with
    /// the RFC-0033 origin directives so a diagnostic inside it maps back.
    Origin {
        path: String,
        line: i64,
        col: i64,
        text: String,
    },
}

/// Render a `Code` piece list to final source text (RFC-0054 `render`). Origin
/// pieces are bracketed by `//@origin` directives, each on its own line (the
/// directive governs the lines that follow — RFC-0033), so a check/parse error
/// inside the region maps back to its recorded `path:line:col`.
/// `x.copy()` (RFC-0089 M1b): a value that shares no heap with `v`.
///
/// A `Ref` clones its `{slot, gen}` and therefore keeps pointing at the same
/// cell — RFC-0089 §5 keeps aliasing explicit, and copying a handle is not a
/// reason to duplicate what it names. A `Stream` and a `Fn` never reach here;
/// the checker refuses the first and the second owns nothing.
fn deep_copy(v: &Val) -> Val {
    match v {
        Val::Str(s) => Val::Str(std::rc::Rc::new(String::clone(s))),
        Val::Array(xs) => Val::Array(std::rc::Rc::new(xs.iter().map(deep_copy).collect())),
        Val::Map(kv) => Val::Map(kv.iter().map(|(k, x)| (k.clone(), deep_copy(x))).collect()),
        Val::Record(fs, name) => Val::Record(
            fs.iter().map(|(k, x)| (k.clone(), deep_copy(x))).collect(),
            name.clone(),
        ),
        Val::Option(o) => Val::Option(o.as_ref().map(|b| Box::new(deep_copy(b)))),
        Val::Result(ok, b) => Val::Result(*ok, Box::new(deep_copy(b))),
        Val::Enum(n, ps) => Val::Enum(n.clone(), ps.iter().map(deep_copy).collect()),
        other => other.clone(),
    }
}

pub fn render_code(pieces: &[CodePiece]) -> String {
    let mut out = String::new();
    let ensure_nl = |out: &mut String| {
        if !out.is_empty() && !out.ends_with('\n') {
            out.push('\n');
        }
    };
    for p in pieces {
        match p {
            CodePiece::Text(t) => out.push_str(t),
            CodePiece::Origin {
                path,
                line,
                col,
                text,
            } => {
                ensure_nl(&mut out);
                out.push_str(&format!("//@origin {path}:{line}:{col}\n"));
                out.push_str(text);
                ensure_nl(&mut out);
                out.push_str("//@origin end\n");
            }
        }
    }
    out
}

/// [`code_splice`] as a trap message rather than a `Ctrl`.
///
/// Public, and a free function, for the same reason [`gen_scoped_path`] is: the
/// wasm generation engine (RFC-0076) lowers `Code` to a handle into a host-side
/// arena and applies the splice rule host-side. It must be THIS rule — the
/// escaping, the identifier validation and the shortest-roundtrip float
/// formatting are not things a second implementation would reproduce, they are
/// things it would eventually disagree with.
pub fn gen_code_splice(val: &Val, ctx: i64) -> Result<Vec<CodePiece>, String> {
    code_splice(val, ctx).map_err(|c| match c {
        Ctrl::Err(m) => m,
        Ctrl::Return(_) => "internal: `?` propagated out of a code splice".to_string(),
    })
}

/// Shortest-roundtrip float digits as PLAIN decimal text. Rust's `Display`
/// formatting never uses an exponent (`Debug` switches to scientific notation
/// for magnitudes ≥ 1e16 or < 1e-4, which the Vyrn lexer cannot read), and an
/// integral value gets `.0` appended so the text lexes as a float literal
/// (`digits '.' digits`) rather than an integer.
fn splice_float(digits: String) -> String {
    if digits.contains('.') {
        digits
    } else {
        format!("{digits}.0")
    }
}

/// Apply the RFC-0054 splice rule for a value in a hole of grammatical context
/// `ctx` (`0` expression, `1` identifier fragment, `2` standalone identifier /
/// type), yielding the code pieces to splice. A `String` is DATA, never code:
/// in expression position it becomes an escaped string *literal*; in an
/// identifier position it is a validated bare-identifier fragment (there is
/// deliberately no way to splice a `String` as code).
fn code_splice(val: &Val, ctx: i64) -> Result<Vec<CodePiece>, Ctrl> {
    // A `Code` value splices verbatim in every context (already-validated code).
    if let Val::Code(pieces) = val {
        return Ok(pieces.clone());
    }
    let text = |s: String| Ok(vec![CodePiece::Text(s)]);
    match ctx {
        // Expression position.
        0 => match val {
            Val::Str(s) => text(escape_string_literal(s)),
            Val::Int(n) => text(n.to_string()),
            Val::IntN { v, signed, .. } => text(if *signed {
                v.to_string()
            } else {
                (*v as u64).to_string()
            }),
            // `NaN`/`inf` do not lex as Vyrn numbers (there is no literal for
            // either), so a computed non-finite value fails here, at the
            // boundary — not downstream as a module that cannot parse.
            Val::Float(f) if f.is_finite() => text(splice_float(format!("{f}"))),
            Val::Float32(f) if f.is_finite() => text(splice_float(format!("{f}"))),
            Val::Float(f) => {
                Err(format!("cannot splice non-finite float {f} into a code quote").into())
            }
            Val::Float32(f) => {
                Err(format!("cannot splice non-finite float {f} into a code quote").into())
            }
            Val::Bool(b) => text(b.to_string()),
            other => Err(format!(
                "cannot splice {} into a code quote (expected String, number, Bool, or Code)",
                val_kind(other)
            )
            .into()),
        },
        // Identifier fragment: `[A-Za-z0-9_]+`, non-empty (it merges with adjacent
        // word characters, so a leading digit is fine).
        1 => match val {
            Val::Str(s) => {
                if !s.is_empty() && s.chars().all(|c| c.is_ascii_alphanumeric() || c == '_') {
                    text(s.to_string())
                } else {
                    Err(format!(
                        "cannot splice {s:?} as an identifier fragment: not `[A-Za-z0-9_]+`"
                    )
                    .into())
                }
            }
            other => Err(format!(
                "cannot splice {} in identifier position (only String or Code)",
                val_kind(other)
            )
            .into()),
        },
        // Standalone identifier / type position: a valid, non-keyword identifier.
        _ => match val {
            Val::Str(s) => {
                if is_bare_identifier(s) {
                    text(s.to_string())
                } else {
                    Err(format!(
                        "cannot splice {s:?} as an identifier: not a valid non-keyword identifier"
                    )
                    .into())
                }
            }
            other => Err(format!(
                "cannot splice {} in identifier position (only String or Code)",
                val_kind(other)
            )
            .into()),
        },
    }
}

/// A short kind name for a `Val`, for splice diagnostics.
fn val_kind(v: &Val) -> &'static str {
    match v {
        Val::Int(_) | Val::IntN { .. } => "a number",
        Val::Float(_) | Val::Float32(_) => "a number",
        Val::Bool(_) => "a Bool",
        Val::Str(_) => "a String",
        Val::Code(_) => "Code",
        _ => "a value",
    }
}

/// The trap for calling an `extern` (RFC-0012) on a target that provides no
/// host for it. Parity compares these bytes byte-for-byte
/// (`vyrn-cli/tests/parity.rs`), so there is one definition: the interpreter
/// raises it, and the native trap stub `vyrn_codegen::toolchain` writes prints
/// it. Neither backend spells it a second time.
pub fn extern_unavailable(name: &str) -> String {
    format!("extern `{name}` is not available on this target")
}

/// Escape a `String` value into a Vyrn source string literal, quotes included —
/// the mechanism by which a spliced String is data, never code (RFC-0054). Uses
/// the same escapes the lexer decodes (`\n \t \r \" \\` and `\{` so an emitted
/// literal cannot itself open an interpolation).
fn escape_string_literal(s: &str) -> String {
    let mut out = String::with_capacity(s.len() + 2);
    out.push('"');
    for c in s.chars() {
        match c {
            '"' => out.push_str("\\\""),
            '\\' => out.push_str("\\\\"),
            '\n' => out.push_str("\\n"),
            '\t' => out.push_str("\\t"),
            '\r' => out.push_str("\\r"),
            // A literal `{` that follows a `\` in the OUTPUT would open a hole; the
            // backslash is already doubled above, so a lone `{` is safe. Emit it
            // verbatim (braces are ordinary characters in Vyrn strings).
            other => out.push(other),
        }
    }
    out.push('"');
    out
}

/// Whether `s` is a single, non-keyword Vyrn identifier — decided by the real
/// lexer, so `fn`/`if`/… (keywords) and `a b`/`1x`/… (non-identifiers) are all
/// rejected in one place (RFC-0054 identifier splice).
fn is_bare_identifier(s: &str) -> bool {
    match crate::lexer::lex(s) {
        Ok(toks) => {
            matches!(toks.first().map(|t| &t.tok), Some(crate::lexer::Tok::Ident(n)) if n == s)
                && toks.len() == 2 // the ident plus EOF
        }
        Err(_) => false,
    }
}

/// Run the compiler's real lexer over `source` non-fatally and return an
/// `Array<Token>` (RFC-0054 `lex`). Unlexable bytes never trap: on a lex error
/// the remainder is emitted as a single `error`-kind token (generators scan
/// work-in-progress text). Every other token carries the canonical token-name
/// string plus its 1-based line/col.
fn lex_tokens(source: &str) -> Vec<Val> {
    lexed(source)
        .into_iter()
        .map(|(kind, text, line, col)| {
            let mut r = HashMap::new();
            r.insert("kind".to_string(), Val::Str(std::rc::Rc::new(kind)));
            r.insert("text".to_string(), Val::Str(std::rc::Rc::new(text)));
            r.insert("line".to_string(), Val::Int(line));
            r.insert("col".to_string(), Val::Int(col));
            Val::Record(r, None)
        })
        .collect()
}

/// The same token list as an `Array<Token>` record *literal* — what the wasm
/// generation engine (RFC-0076 M3b) encodes for the guest.
///
/// Both this and [`lex_tokens`] read one [`lexed`], so the two engines cannot
/// disagree about which tokens `lex` yields, only about how the value is built.
pub fn gen_lex_tokens_lit(source: &str) -> Expr {
    Expr::ArrayLit {
        elems: lexed(source)
            .into_iter()
            .map(|(kind, text, line, col)| Expr::StructLit {
                name: "Token".to_string(),
                fields: vec![
                    ("kind".to_string(), Expr::Str(kind)),
                    ("text".to_string(), Expr::Str(text)),
                    ("line".to_string(), Expr::Int(line)),
                    ("col".to_string(), Expr::Int(col)),
                ],
                line: 0,
            })
            .collect(),
        line: 0,
    }
}

/// The compiler's real lexer over `source`, as `(kind, text, line, col)` rows —
/// the one place the `lex` builtin's token list is decided.
fn lexed(source: &str) -> Vec<(String, String, i64, i64)> {
    match crate::lexer::lex(source) {
        Ok(toks) => toks
            .iter()
            .filter(|t| !matches!(t.tok, crate::lexer::Tok::Eof))
            .map(|t| {
                let (kind, text) = crate::lexer::token_name_and_text(&t.tok);
                (kind, text, t.line as i64, t.col as i64)
            })
            .collect(),
        Err(d) => {
            // Non-fatal: attribute the unlexable input as one `error` token at the
            // diagnostic's position.
            vec![("error".to_string(), d.message, d.line as i64, d.col as i64)]
        }
    }
}

/// The two shapes a [`Val::Fn`] can take (RFC-0023).
#[derive(Debug, Clone, PartialEq)]
pub enum FnVal {
    /// A named top-level function passed by name (`twice(xs, double)`).
    Named(String),
    /// A lambda literal with its captured environment snapshot. Captures are read
    /// values fixed at the moment the lambda expression is evaluated (the outer
    /// call site) — a binding reassigned afterward is not visible, matching the
    /// monomorphized backends (which pass captures at the same point).
    Lambda {
        params: Vec<String>,
        body: LambdaBody,
        captures: Vec<(String, Val)>,
        /// The lambda's parameter types and return type, taken from the `fn(..)`
        /// signature of the parameter it was passed to (so arguments coerce and
        /// the result validates exactly as a named callee would).
        param_tys: Vec<Type>,
        ret: Type,
    },
    /// A thunk stored in a `lazy T` field (RFC-0085 M4a): the nullary closure
    /// above, tagged so that reading the field FORCES it.
    ///
    /// The tag rides on the VALUE rather than being looked up from the field's
    /// declared type, and that is the whole reason this variant exists. The two
    /// compiled backends read `Field.ty` at the access site and see the marker
    /// for free; the interpreter is walking a `Val` that has no type attached,
    /// and a lookup through the record's stamped name would answer `None` for
    /// every value that reached the read without crossing a naming boundary —
    /// which is a wrong answer on exactly one engine, the failure mode parity
    /// exists to catch. `coerce` sets it once, at the same boundary it stamps a
    /// record's name.
    Thunk(Box<FnVal>),
}

/// A control signal carried in the error channel.
#[derive(Debug)]
pub enum Ctrl {
    /// A genuine runtime error.
    Err(String),
    /// A `?`-propagated early return of the enclosing function.
    Return(Val),
}

impl From<String> for Ctrl {
    fn from(s: String) -> Self {
        Ctrl::Err(s)
    }
}
impl From<&str> for Ctrl {
    fn from(s: &str) -> Self {
        Ctrl::Err(s.to_string())
    }
}

/// Reserve room the way the compiled backends allocate: a refusal is a Vyrn
/// trap, in the words the other two engines already print.
///
/// `Vec` and `String` ABORT the process when the allocator says no — that is
/// Rust's contract, not this language's, and it is what made `s = s + s` print
/// `memory allocation of 68719476736 bytes failed` and exit 127 where the same
/// program under the direct backend printed `error: out of memory` and exited 1.
/// `try_reserve` is the whole mechanism: after a successful reserve the
/// `push_str`/`push` that follows cannot allocate again.
///
/// Applied at the sites where the AMOUNT IS A VALUE THE PROGRAM COMPUTED —
/// string concatenation, the concat accumulator, and `push` — and deliberately
/// nowhere else. The interpreter allocates on nearly every node; a fallible
/// reserve for a two-field record would be ceremony around a size no program can
/// drive, and it would still not make the interpreter allocation-safe, because
/// `Val::clone` is `Clone` and cannot fail. What is guaranteed here is the
/// growth a program names, and RFC-0081 records that boundary rather than
/// implying a stronger one.
fn reserve_str(s: &mut String, more: usize) -> Result<(), Ctrl> {
    s.try_reserve(more)
        .map_err(|_| Ctrl::Err(crate::trap::OUT_OF_MEMORY.into()))
}

fn reserve_vec<T>(v: &mut Vec<T>, more: usize) -> Result<(), Ctrl> {
    v.try_reserve(more)
        .map_err(|_| Ctrl::Err(crate::trap::OUT_OF_MEMORY.into()))
}

/// `a + b` for strings, allocated once and fallibly.
///
/// `format!("{a}{b}")` was two infallible allocations in a row (the `String`,
/// then its growth); this is one reserve of the exact answer.
fn concat_str(a: &str, b: &str) -> Result<Val, Ctrl> {
    let mut s = String::new();
    reserve_str(&mut s, a.len() + b.len())?;
    s.push_str(a);
    s.push_str(b);
    Ok(Val::Str(std::rc::Rc::new(s)))
}

/// Statement/block control flow (distinct from the `Ctrl` error channel).
enum Flow {
    Normal,
    Return(Val),
    /// `break` — exit the innermost loop (RFC-0060). Propagates up through
    /// nested blocks/regions (running their drops) until a loop catches it.
    Break,
    /// `continue` — skip to the innermost loop's next iteration (RFC-0060).
    Continue,
}

/// Render a scalar value with the canonical `toString`/`print` formatting:
/// signed `IntN` by logical value, unsigned as `u64`, `Float` to 6 decimals, a
/// `Bool` as `true`/`false`, a `String` verbatim. Shared by `x.toString()`
/// (`@str`) and `assertEq`'s failure message so all three render identically
/// (parity-identical by construction). Non-scalars (never reached from
/// `assertEq`, whose operands the checker restricts to equatable scalars) fall
/// back to the debug form.
fn scalar_to_string(v: &Val) -> String {
    match v {
        Val::Int(n) => n.to_string(),
        Val::IntN {
            v, signed: true, ..
        } => v.to_string(),
        Val::IntN {
            v, signed: false, ..
        } => (*v as u64).to_string(),
        Val::Float(f) => format!("{f:.6}"),
        Val::Float32(f) => format!("{:.6}", *f as f64),
        Val::Bool(b) => if *b { "true" } else { "false" }.to_string(),
        Val::Str(s) => (**s).clone(),
        other => format!("{other:?}"),
    }
}

/// Wrap `v` into a `bits`-wide two's-complement integer, matching the native
/// backend's `iN` arithmetic. Signed values are sign-extended back into `i64`;
/// unsigned are zero-extended. `bits >= 64` is the identity.
fn wrap_intn(v: i64, bits: u8, signed: bool) -> i64 {
    if bits >= 64 {
        return v;
    }
    let mask = (1i64 << bits) - 1;
    let m = v & mask;
    if signed && (m & (1i64 << (bits - 1))) != 0 {
        m | !mask // set the high bits (sign extension)
    } else {
        m
    }
}

/// IEEE-754-2019 `minimum` — NaN in either operand propagates, and `-0.0` orders
/// strictly below `+0.0` (RFC-0083 M2).
///
/// This is wasm's `f32x4.min` and LLVM's `llvm.minimum`, and it is deliberately
/// NOT `f32::min`, which is `minNum` and returns the non-NaN operand. That
/// difference is visible: `min(NaN, 1.0)` prints `NaN` under one rule and
/// `1.000000` under the other, where a NaN PAYLOAD difference prints the same
/// either way. The three engines agree because all three were pointed at this
/// rule, not because they were left to their defaults.
fn fminimum(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        return f32::NAN;
    }
    // `-0.0 == 0.0` in IEEE, so `<` cannot separate them and the sign bit has to
    // be asked directly. This is the whole of the difference between `minimum`
    // and a naive `if a < b`.
    if a == b {
        return if a.is_sign_negative() { a } else { b };
    }
    if a < b {
        a
    } else {
        b
    }
}

/// IEEE-754-2019 `maximum` — the mirror of [`fminimum`], `+0.0` above `-0.0`.
fn fmaximum(a: f32, b: f32) -> f32 {
    if a.is_nan() || b.is_nan() {
        return f32::NAN;
    }
    if a == b {
        return if a.is_sign_negative() { b } else { a };
    }
    if a > b {
        a
    } else {
        b
    }
}

/// IEEE-754-2019 `minimum` / `maximum` at the wide lane (RFC-0083 M4).
///
/// Written out a second time rather than computed through the `f32` pair: the
/// rule is the same, but routing `f64` operands through a narrower function
/// would round them, and the whole claim of a lane-wise operation is that it is
/// the scalar one exactly.
fn fminimum64(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a == b {
        return if a.is_sign_negative() { a } else { b };
    }
    if a < b {
        a
    } else {
        b
    }
}

fn fmaximum64(a: f64, b: f64) -> f64 {
    if a.is_nan() || b.is_nan() {
        return f64::NAN;
    }
    if a == b {
        return if a.is_sign_negative() { b } else { a };
    }
    if a > b {
        a
    } else {
        b
    }
}

/// One `I32x4` lane out of a `Val` (RFC-0083 M3).
///
/// Through [`convert_val`] rather than off the `Val` directly, for the reason a
/// float lane goes through it: an integer LITERAL evaluates to a `Val::Int` (i64)
/// while the checker has already typed it `Int32`, and the backends truncate it
/// with a `trunc i64 to i32`. Doing anything else here would make `I32x4(1, 2,
/// 3, 4)` a different value in the interpreter than in the other two engines.
fn i32_lane(v: Val) -> Result<i32, Ctrl> {
    match convert_val(
        v,
        &Type::IntN {
            bits: 32,
            signed: true,
        },
    ) {
        Val::IntN { v, .. } => Ok(v as i32),
        other => Err(format!("I32x4 lane: {other:?}").into()),
    }
}

/// The bounds trap a vector load/store of `span` elements at `i` reports.
///
/// The wording is the scalar one, and the index is the FIRST lane of
/// `i..i+span-1` that is actually out of range — reporting `i` itself would name
/// an in-range element whenever only the tail overruns, which is the common
/// case. `span` is the lane count and not a constant since M4: two `Float64`
/// lanes read two elements, and a four-element message would name one that was
/// never touched.
fn vec_oob(i: i64, span: i64) -> String {
    let k = if i < 0 { i } else { i + span - 1 };
    crate::trap::array_index(k)
}

/// Convert a numeric value to `target` (Int / sized IntN / Float / Float32),
/// matching the native casts (sext/trunc via `wrap_intn`, si/uitofp, fpto si/ui,
/// fp trunc/ext). Float→int truncates toward zero; out-of-range float→int is
/// unspecified (as in C/LLVM).
fn convert_val(v: Val, target: &Type) -> Val {
    match target {
        Type::Int => match v {
            Val::IntN { v, .. } => Val::Int(v),
            Val::Float(f) => Val::Int(f as i64),
            Val::Float32(f) => Val::Int(f as i64),
            other => other,
        },
        Type::IntN { bits, signed } => {
            let n = match v {
                Val::Int(n) => n,
                Val::IntN { v, .. } => v,
                // Truncate toward zero; an unsigned target reads the float as
                // `u64` (native `fptoui`), signed as `i64` (`fptosi`).
                Val::Float(f) if !*signed => f as u64 as i64,
                Val::Float(f) => f as i64,
                Val::Float32(f) if !*signed => f as u64 as i64,
                Val::Float32(f) => f as i64,
                other => return other,
            };
            Val::IntN {
                v: wrap_intn(n, *bits, *signed),
                bits: *bits,
                signed: *signed,
            }
        }
        Type::Float => match v {
            Val::Int(n) => Val::Float(n as f64),
            // An unsigned source reads its bits as `u64` before converting
            // (native uses `uitofp`); signed sign-extends via `as f64`.
            Val::IntN {
                v, signed: false, ..
            } => Val::Float(v as u64 as f64),
            Val::IntN {
                v, signed: true, ..
            } => Val::Float(v as f64),
            Val::Float32(f) => Val::Float(f as f64), // fpext
            other => other,
        },
        // Float32 rounds every source to single precision (`as f32`).
        Type::Float32 => match v {
            Val::Int(n) => Val::Float32(n as f32),
            Val::IntN {
                v, signed: false, ..
            } => Val::Float32(v as u64 as f32),
            Val::IntN {
                v, signed: true, ..
            } => Val::Float32(v as f32),
            Val::Float(f) => Val::Float32(f as f32), // fptrunc
            other => other,
        },
        _ => v,
    }
}

// (The hex / base64 / percent codecs lived here — 159 lines of Rust duplicating
// the textual emitter's hand-written IR. RFC-0078 M4c routed the six builtins to
// `std/codecs`, so the interpreter holds no definition of them at all.)

/// Parse a base-10 integer with strict, backend-matched semantics: an optional
/// leading `-`, then one or more ASCII digits, and nothing else (no whitespace,
/// no `+`). Returns `None` on any deviation. Overflow *wraps* (it is not
/// rejected) so the result matches the native backend bit-for-bit.
fn parse_int(s: &str) -> Option<i64> {
    let b = s.as_bytes();
    if b.is_empty() {
        return None;
    }
    let (neg, start) = if b[0] == b'-' { (true, 1) } else { (false, 0) };
    if start == b.len() {
        return None; // just "-"
    }
    let mut n: i64 = 0;
    for &c in &b[start..] {
        if !c.is_ascii_digit() {
            return None;
        }
        n = n.wrapping_mul(10).wrapping_add((c - b'0') as i64);
    }
    Some(if neg { n.wrapping_neg() } else { n })
}

/// Run the program's `main` and return its integer result.
///
/// The tree-walking interpreter recurses once per Vyrn call, so a deeply
/// recursive program can exhaust the OS main-thread stack (only ~1 MB on
/// Windows). Run the interpreter on a dedicated thread with a large stack so
/// recursion depth is bounded by the program, not the platform default.
pub fn run(program: &Program) -> Result<i64, String> {
    run_with_args(program, &[])
}

/// Like [`run`], but supplies the program's command-line arguments (RFC-0014
/// `args()`). These are the arguments *after* the program name (argv[1..]); the
/// native/wasm backends read the same slice from their C `main`'s `argv`.
pub fn run_with_args(program: &Program, args: &[String]) -> Result<i64, String> {
    on_deep_stack(|| run_inner(program, args))
}

/// Run `f` with room for [`CALL_DEPTH_LIMIT`] interpreter frames beneath it.
///
/// A dedicated thread is how a hosted platform gets that room, because the OS
/// main-thread stack is only ~1 MB on Windows.
#[cfg(not(all(target_arch = "wasm32", target_os = "unknown")))]
fn on_deep_stack(f: impl FnOnce() -> Result<i64, String> + Send) -> Result<i64, String> {
    // RFC-0101 M4's release trace is per thread, and the program runs on this
    // one. Nothing is carried unless a caller asked for the trace, which is the
    // corpus gate and nothing else.
    let tracing = crate::own::trace::on();
    let carried = std::sync::Mutex::new(Vec::new());
    let out = std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(INTERP_STACK_BYTES)
            .spawn_scoped(s, || {
                if tracing {
                    crate::own::trace::start();
                }
                let r = f();
                if tracing {
                    *carried.lock().unwrap() = crate::own::trace::take();
                }
                r
            })
            .expect("failed to spawn interpreter thread")
            .join()
            .unwrap_or_else(|_| Err("interpreter thread panicked (likely stack overflow)".into()))
    });
    if tracing {
        crate::own::trace::adopt(carried.into_inner().unwrap_or_default());
    }
    out
}

/// `wasm32-unknown-unknown` has one thread and cannot make another, so the room
/// is reserved at LINK time instead: `compiler/vyrn-play` passes
/// `-z stack-size` for the whole module and measures what depth that buys.
/// [`CALL_DEPTH_LIMIT`] is the same number here as everywhere else, which is what
/// makes "too deep" the same diagnostic in a browser as in a terminal.
#[cfg(all(target_arch = "wasm32", target_os = "unknown"))]
fn on_deep_stack(f: impl FnOnce() -> Result<i64, String>) -> Result<i64, String> {
    f()
}

fn run_inner(program: &Program, prog_args: &[String]) -> Result<i64, String> {
    let interp = new_interp(program, prog_args)?;
    if let Err(Ctrl::Err(s)) = interp.init_globals(program) {
        return Err(s);
    }
    match interp.call("main", &[]) {
        Ok(Val::Int(n)) => Ok(n),
        Ok(other) => Err(format!("main returned {other:?}, expected Int64")),
        Err(Ctrl::Err(s)) => Err(s),
        Err(Ctrl::Return(_)) => Err("internal: `?` propagated past main".into()),
    }
}

/// Run the ROOT module's `test` blocks (RFC-0015) under the interpreter, in
/// declaration order. Only tests with no `module` tag (the root's) run; an
/// imported module's tests are skipped (they still type-check). `filter`, when
/// present, keeps only tests whose name contains it (`vyrn test --name`).
///
/// `on_result` is invoked once per run test, AFTER its body finishes — so any
/// `print` output the body produced has already streamed to stdout, and the
/// caller's per-test result line prints after it (the RFC's "print passes
/// through" ordering). A body that traps (a failed `assert`, or any runtime
/// trap) yields `Err(message)`; the runner treats every `Err` as that test
/// FAILING and continues to the next. Returns `(passed, failed)`, or a harness
/// error string if program setup (module-state initialization) itself fails.
pub fn run_tests<F>(
    program: &Program,
    filter: Option<&str>,
    on_result: F,
) -> Result<(usize, usize), String>
where
    F: FnMut(&str, &Result<(), String>) + Send,
{
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(INTERP_STACK_BYTES)
            .spawn_scoped(s, || run_tests_inner(program, filter, on_result))
            .expect("failed to spawn interpreter thread")
            .join()
            .unwrap_or_else(|_| Err("interpreter thread panicked (likely stack overflow)".into()))
    })
}

fn run_tests_inner<F>(
    program: &Program,
    filter: Option<&str>,
    mut on_result: F,
) -> Result<(usize, usize), String>
where
    F: FnMut(&str, &Result<(), String>),
{
    let interp = new_interp(program, &[])?;
    if let Err(Ctrl::Err(s)) = interp.init_globals(program) {
        return Err(s);
    }
    let mut passed = 0usize;
    let mut failed = 0usize;
    for t in &program.tests {
        // Root-only: an imported module's tests are not run here (RFC-0015).
        if t.module.is_some() {
            continue;
        }
        if let Some(sub) = filter {
            if !t.name.contains(sub) {
                continue;
            }
        }
        let mut scope: Vec<Frame> = vec![Frame::default()];
        // Any `Ctrl::Err` is a FAILED test (including a failed `assert`); a bare
        // `?`-propagated `Ctrl::Return` (a test may use `?`) simply ends it.
        let result: Result<(), String> = match interp.block(&t.body, &mut scope) {
            Ok(_) => Ok(()),
            Err(Ctrl::Return(_)) => Ok(()),
            Err(Ctrl::Err(s)) => Err(s),
        };
        if result.is_ok() {
            passed += 1;
        } else {
            failed += 1;
        }
        on_result(&t.name, &result);
    }
    Ok((passed, failed))
}

/// Run the ROOT module's `bench` blocks (RFC-0055) **once each** under the
/// interpreter, in declaration order — the `vyrn bench --check` face. This is the
/// deterministic, byte-pinnable path (no timing): each body runs a single time to
/// prove it executes without trapping. Root-only and `filter`-aware exactly like
/// [`run_tests`]. `on_result` reports `Ok(())` (ran clean) or `Err(message)` (a
/// trap — a failed `assert`, an out-of-bounds index, etc.); the runner continues
/// to the next bench and the caller exits nonzero if any failed. Returns
/// `(ok, failed)`, or a harness error string if module-state init fails.
pub fn run_benches<F>(
    program: &Program,
    filter: Option<&str>,
    on_result: F,
) -> Result<(usize, usize), String>
where
    F: FnMut(&str, &Result<(), String>) + Send,
{
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(INTERP_STACK_BYTES)
            .spawn_scoped(s, || run_benches_inner(program, filter, on_result))
            .expect("failed to spawn interpreter thread")
            .join()
            .unwrap_or_else(|_| Err("interpreter thread panicked (likely stack overflow)".into()))
    })
}

fn run_benches_inner<F>(
    program: &Program,
    filter: Option<&str>,
    mut on_result: F,
) -> Result<(usize, usize), String>
where
    F: FnMut(&str, &Result<(), String>),
{
    let interp = new_interp(program, &[])?;
    if let Err(Ctrl::Err(s)) = interp.init_globals(program) {
        return Err(s);
    }
    let mut ok = 0usize;
    let mut failed = 0usize;
    for b in &program.benches {
        // Root-only: an imported module's benches are not run here (RFC-0055).
        if b.module.is_some() {
            continue;
        }
        if let Some(sub) = filter {
            if !b.name.contains(sub) {
                continue;
            }
        }
        let mut scope: Vec<Frame> = vec![Frame::default()];
        let result: Result<(), String> = match interp.block(&b.body, &mut scope) {
            Ok(_) => Ok(()),
            Err(Ctrl::Return(_)) => Ok(()),
            Err(Ctrl::Err(s)) => Err(s),
        };
        if result.is_ok() {
            ok += 1;
        } else {
            failed += 1;
        }
        on_result(&b.name, &result);
    }
    Ok((ok, failed))
}

/// One entry of a program's mounted router, as the router itself received it.
///
/// `method` is the first word of the value's own `derived` line, so a stream
/// reads `SSE` and a socket `WS` rather than the `GET` they both answer: that is
/// what the value says it is, and RFC-0074's whole claim is that a stream is a
/// different protocol and not a flag on a response.
#[derive(Debug, Clone, PartialEq)]
pub struct MountedRoute {
    pub method: String,
    pub path: String,
    /// The procedure the route answers with, or `"-"` when the value carries no
    /// name for it — a `Live`/`Socket` is built from a handler passed by value
    /// and nothing at runtime knows what that handler was called.
    pub procedure: String,
    /// A `surface(..)`: a whole subsystem behind a prefix, whose members are
    /// enumerated by the `//@route` channel instead of by this one.
    pub prefix: bool,
}

/// Every `Route`/`Live`/`Socket` the program hands to `std/http`'s `mount`
/// (RFC-0074's deferred "`vyrn routes` cannot see an explicit route").
///
/// A hand-written projection's paths are not derived from anything, so no
/// generator can emit a directive for them — but they are not unreachable
/// either: they exist as data the moment `mount` is called, and `Route`,
/// `Live` and `Socket` each carry the `derived` line their constructor and
/// combinators wrote. So this reads them from the values themselves.
///
/// It reads the arguments of the `mount(..)` CALL rather than calling exports
/// named `routes`/`feeds`/`sockets`, which is the difference between a fact and
/// a convention: an `Array<Route>` a module exports but nobody mounts is not on
/// the wire, and the composition root is the only place that knows which lists
/// were actually passed. The command therefore does not re-derive anything —
/// it evaluates the same expressions `mount` is handed and reads the same
/// fields `mount` routes on, which is the one-producer property the directive
/// channel has, obtained a different way.
///
/// Two honest limits. The arguments are evaluated ONCE, here, after module-state
/// init — a route list computed from mutable module state could differ on a
/// later request. And this runs the program's own startup, so a program that
/// traps or loops on init has no table; the caller keeps the directive rows and
/// says so rather than failing, which is why `vyrn routes` never prints less
/// than it did before this existed.
pub fn mounted_routes(program: &Program) -> Result<Vec<MountedRoute>, String> {
    let mut calls: Vec<&[Expr]> = Vec::new();
    for f in &program.functions {
        mount_calls_block(&f.body, &mut calls);
    }
    if calls.is_empty() {
        return Ok(Vec::new());
    }
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(INTERP_STACK_BYTES)
            .spawn_scoped(s, || mounted_routes_inner(program, &calls))
            .expect("failed to spawn interpreter thread")
            .join()
            .unwrap_or_else(|_| Err("interpreter thread panicked (likely stack overflow)".into()))
    })
}

fn mounted_routes_inner(program: &Program, calls: &[&[Expr]]) -> Result<Vec<MountedRoute>, String> {
    let interp = new_interp(program, &[])?;
    if let Err(Ctrl::Err(s)) = interp.init_globals(program) {
        return Err(s);
    }
    let mut out = Vec::new();
    for args in calls {
        // Argument 0 is the request; the route lists are everything after it.
        for a in args.iter().skip(1) {
            let mut scope: Vec<Frame> = vec![Frame::default()];
            match interp.expr(a, &mut scope) {
                Ok(v) => collect_mounted(&v, &mut out),
                Err(Ctrl::Err(s)) => return Err(s),
                Err(Ctrl::Return(_)) => {}
            }
        }
    }
    Ok(out)
}

/// Read one mounted value, recursing through the group arrays `mount` takes.
fn collect_mounted(v: &Val, out: &mut Vec<MountedRoute>) {
    match v {
        Val::Array(xs) => {
            for x in xs.iter() {
                collect_mounted(x, out);
            }
        }
        Val::Record(fields, Some(name)) if matches!(&**name, "Route" | "Live" | "Socket") => {
            let Some(Val::Str(derived)) = fields.get("derived") else {
                return;
            };
            // `method path [procedure] [policy..]`, written by the constructor
            // and appended to by each combinator. A `Route`'s third word is the
            // procedure the generator seeded it with; a `Live`/`Socket` has none.
            let mut words = derived.split_whitespace();
            let (Some(method), Some(path)) = (words.next(), words.next()) else {
                return;
            };
            let procedure = match &**name {
                "Route" => words.next().unwrap_or("-"),
                _ => "-",
            };
            out.push(MountedRoute {
                method: method.to_string(),
                path: path.to_string(),
                procedure: procedure.to_string(),
                prefix: matches!(fields.get("prefix"), Some(Val::Bool(true))),
            });
        }
        _ => {}
    }
}

/// Every `mount(..)` argument list in a block. A plain walk: a missed nesting is
/// a route silently absent from the table, which is the bug this closes.
fn mount_calls_block<'a>(b: &'a Block, out: &mut Vec<&'a [Expr]>) {
    for s in &b.stmts {
        match s {
            Stmt::Let { value, .. }
            | Stmt::Assign { value, .. }
            | Stmt::SetField { value, .. }
            | Stmt::Expr(value) => mount_calls_expr(value, out),
            Stmt::IndexSet { index, value, .. } => {
                mount_calls_expr(index, out);
                mount_calls_expr(value, out);
            }
            Stmt::Return { value, .. } => {
                if let Some(e) = value {
                    mount_calls_expr(e, out);
                }
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                mount_calls_expr(cond, out);
                mount_calls_block(then_block, out);
                if let Some(e) = else_block {
                    mount_calls_block(e, out);
                }
            }
            Stmt::IfLet {
                scrutinee,
                then_block,
                else_block,
                ..
            } => {
                mount_calls_expr(scrutinee, out);
                mount_calls_block(then_block, out);
                if let Some(e) = else_block {
                    mount_calls_block(e, out);
                }
            }
            Stmt::While { cond, body, .. } => {
                mount_calls_expr(cond, out);
                mount_calls_block(body, out);
            }
            Stmt::ForIn { iter, body, .. } => {
                mount_calls_expr(iter, out);
                mount_calls_block(body, out);
            }
            Stmt::Region { body, .. } => mount_calls_block(body, out),
            Stmt::Break { .. } | Stmt::Continue { .. } | Stmt::Drop { .. } => {}
        }
    }
}

fn mount_calls_expr<'a>(e: &'a Expr, out: &mut Vec<&'a [Expr]>) {
    match e {
        Expr::Call { name, args, .. } => {
            // Top-level names are unique across a linked program, so `mount` is
            // `std/http`'s or the program has none.
            if name == "mount" {
                out.push(args);
            }
            for a in args {
                mount_calls_expr(a, out);
            }
        }
        Expr::TryConstruct { args, .. } | Expr::Spawn { args, .. } => {
            for a in args {
                mount_calls_expr(a, out);
            }
        }
        Expr::Unary { expr, .. } | Expr::Try { expr, .. } | Expr::Field { expr, .. } => {
            mount_calls_expr(expr, out)
        }
        Expr::Consume { place, .. } => mount_calls_expr(place, out),
        Expr::Binary { lhs, rhs, .. } => {
            mount_calls_expr(lhs, out);
            mount_calls_expr(rhs, out);
        }
        Expr::Match {
            scrutinee, arms, ..
        } => {
            mount_calls_expr(scrutinee, out);
            for a in arms {
                mount_calls_expr(&a.body, out);
            }
        }
        Expr::IfExpr {
            cond,
            then_branch,
            else_branch,
            ..
        } => {
            mount_calls_expr(cond, out);
            mount_calls_expr(then_branch, out);
            if let Some(b) = else_branch {
                mount_calls_expr(b, out);
            }
        }
        Expr::StructLit { fields, .. } => {
            for (_, v) in fields {
                mount_calls_expr(v, out);
            }
        }
        Expr::ArrayLit { elems, .. } => {
            for x in elems {
                mount_calls_expr(x, out);
            }
        }
        Expr::MapLit { entries, .. } => {
            for (k, v) in entries {
                mount_calls_expr(k, out);
                mount_calls_expr(v, out);
            }
        }
        Expr::Lambda { body, .. } => match body {
            LambdaBody::Expr(x) => mount_calls_expr(x, out),
            LambdaBody::Block(b) => mount_calls_block(b, out),
        },
        Expr::Int(_)
        | Expr::Byte(_)
        | Expr::Float(_)
        | Expr::Bool(_)
        | Expr::Str(_)
        | Expr::Var { .. } => {}
    }
}

/// One HTTP request handed to a served `handle` (RFC-0016). The host (`vyrn
/// serve`) fills these from the wire; the interpreter turns each into a
/// `Request` record before calling `handle`.
pub struct ServeRequest {
    pub method: String,
    pub path: String,
    /// The request's header block, in wire order, with names ALREADY LOWERCASED
    /// (RFC-0072 M4). Case folding happens here, at the edge, so the `Map` the
    /// program sees has one spelling per header and an exact lookup is correct.
    pub headers: Vec<(String, String)>,
    pub body: String,
}

/// The fields a served `handle` returned — the interpreter reads them back out
/// of the `Response` record and hands them to the host to write on the wire.
pub struct ServeResponse {
    pub status: i64,
    pub content_type: String,
    pub body: String,
    /// The `Vary` header to write, or `""` for none (RFC-0072 M4).
    pub vary: String,
    /// Every other response header, in the order the program inserted them
    /// (RFC-0074 M2). Written verbatim; an empty map writes nothing.
    pub headers: Vec<(String, String)>,
}

/// What the host asks the interpreter for (RFC-0074 M3a). Until M3a there was
/// one question and it needed no name; a streaming answer adds two more, because
/// the stream it opens is pulled AFTER the call that opened it returned.
pub enum ServeCall {
    /// One request off the wire.
    Handle(ServeRequest),
    /// The next frame of the stream the last [`ServeAnswer::Live`] opened.
    Next,
    /// Release that stream. The host sends this the first time a write fails —
    /// which is how it learns the client is gone — and when the stream ends.
    Close,
}

/// What the interpreter answers. `Buffered` is the only shape that existed
/// before M3a and it is unchanged: a response that exists all at once, with the
/// `Vary` header and the conditional-request machinery that only make sense for
/// one. A streaming answer is a SECOND shape rather than a flag on the first.
pub enum ServeAnswer {
    /// A complete response. The answer to a `Handle` that opened no stream.
    Buffered(ServeResponse),
    /// A stream's header block: status, content type and headers, plus a `body`
    /// the host writes once as the stream's prologue (SSE's `retry:` line).
    /// Frames follow, one `Next` at a time.
    Live(ServeResponse),
    /// The answer to `Next`: one frame, or `None` when the producer ended.
    Frame(Option<String>),
    /// The answer to `Close`.
    Released,
}

/// Run a served program (RFC-0016) under the interpreter: build ONE interpreter,
/// initialize module state, run `main` once (the setup hook — optional; a
/// nonzero return aborts the serve), then hand the caller a handler closure it
/// can call once per request. The single interpreter instance lives for the
/// whole `run_loop`, so module state (`let mut`) persists across requests — the
/// host-owns-the-loop model. A trap inside `handle` surfaces as `Err(message)`
/// and does NOT poison the interpreter: the global frame is untouched by a
/// request's local unwinding, so the next request runs cleanly (exactly as a
/// failing `test` body leaves the next test's state intact in [`run_tests`]).
///
/// `run_loop` receives a `&mut dyn FnMut(ServeRequest) -> Result<ServeResponse,
/// String>` and owns the accept loop (the HTTP host lives in the CLI, keeping
/// this crate std-only and network-free). It runs on the big-stack interpreter
/// thread like `run`/`run_tests`, so deep `handle` recursion cannot overflow.
pub fn serve<F>(program: &Program, run_loop: F) -> Result<(), String>
where
    F: FnOnce(&mut dyn FnMut(ServeCall) -> Result<ServeAnswer, String>) -> Result<(), String>
        + Send,
{
    std::thread::scope(|s| {
        std::thread::Builder::new()
            .stack_size(INTERP_STACK_BYTES)
            .spawn_scoped(s, || serve_inner(program, run_loop))
            .expect("failed to spawn interpreter thread")
            .join()
            .unwrap_or_else(|_| Err("interpreter thread panicked (likely stack overflow)".into()))
    })
}

fn serve_inner<F>(program: &Program, run_loop: F) -> Result<(), String>
where
    F: FnOnce(&mut dyn FnMut(ServeCall) -> Result<ServeAnswer, String>) -> Result<(), String>,
{
    let interp = new_interp(program, &[])?;
    if let Err(Ctrl::Err(s)) = interp.init_globals(program) {
        return Err(s);
    }
    // `main` is optional in a served file (RFC-0016). When present it runs once,
    // before the first request (mirroring `_start`); a nonzero return aborts.
    if interp.funcs.contains_key("main") {
        match interp.call("main", &[]) {
            Ok(Val::Int(0)) => {}
            Ok(Val::Int(n)) => return Err(format!("main returned {n}, aborting serve")),
            Ok(other) => return Err(format!("main returned {other:?}, expected Int64")),
            Err(Ctrl::Err(s)) => return Err(s),
            Err(Ctrl::Return(_)) => return Err("internal: `?` propagated past main".into()),
        }
    }
    let mut handler = |call: ServeCall| serve_call(&interp, call);
    run_loop(&mut handler)
}

/// Route one host question (RFC-0074 M3a). `Handle` is [`handle_request`]
/// unchanged; `Next` and `Close` reach the stream that call parked.
fn serve_call(interp: &Interp<'_>, call: ServeCall) -> Result<ServeAnswer, String> {
    match call {
        ServeCall::Handle(req) => handle_request(interp, req),
        // The stream is taken OUT of its cell for the duration of the step: the
        // step is ordinary Vyrn and may reach anything, including `serveStream`
        // itself, and a producer running under a borrow of the cell it lives in
        // is a panic waiting for the program that does it.
        ServeCall::Next => {
            let taken = interp.live.borrow_mut().take();
            let Some(mut s) = taken else {
                return Err("internal: no stream is open".into());
            };
            let got = interp.stream_next(&mut s);
            match got {
                Ok(v) => {
                    // A step that called `serveStream` parked a SECOND stream
                    // while `live` was empty (the "already opened" trap could
                    // not fire). The newest producer wins; the displaced one
                    // goes through the same release every other path uses
                    // instead of being dropped untracked.
                    if interp.live.borrow().is_some() {
                        let _ = interp.release_stream(&s);
                    } else {
                        *interp.live.borrow_mut() = Some(s);
                    }
                    match v {
                        None => Ok(ServeAnswer::Frame(None)),
                        Some(Val::Str(f)) => Ok(ServeAnswer::Frame(Some((*f).clone()))),
                        Some(other) => Err(format!(
                            "a served stream yielded {other:?}, expected a String"
                        )),
                    }
                }
                // A trapping producer releases before the trap surfaces, exactly
                // as `for … in` does on the same path.
                Err(e) => {
                    let _ = interp.release_stream(&s);
                    Err(match e {
                        Ctrl::Err(m) => m,
                        Ctrl::Return(_) => "internal: `?` propagated past a stream step".into(),
                    })
                }
            }
        }
        ServeCall::Close => {
            let taken = interp.live.borrow_mut().take();
            if let Some(s) = taken {
                if let Err(Ctrl::Err(m)) = interp.release_stream(&s) {
                    return Err(m);
                }
            }
            Ok(ServeAnswer::Released)
        }
    }
}

/// Marshal one host request into a `Request` record, call `handle` on this
/// interpreter, and read the `Response` record back out — the shared body of
/// [`serve`] (one interpreter) and [`serve_pool`] (one per worker, RFC-0025).
fn handle_request(interp: &Interp<'_>, req: ServeRequest) -> Result<ServeAnswer, String> {
    let headers = Val::Map(
        req.headers
            .into_iter()
            .map(|(k, v)| (k, Val::Str(std::rc::Rc::new(v))))
            .collect(),
    );
    let request = Val::Record(
        HashMap::from([
            ("method".to_string(), Val::Str(std::rc::Rc::new(req.method))),
            ("path".to_string(), Val::Str(std::rc::Rc::new(req.path))),
            ("headers".to_string(), headers),
            ("body".to_string(), Val::Str(std::rc::Rc::new(req.body))),
        ]),
        None,
    );
    match interp.call("handle", &[request]) {
        Ok(Val::Record(map, _)) => {
            let status = match map.get("status") {
                Some(Val::Int(n)) => *n,
                Some(Val::IntN { v, .. }) => *v,
                _ => return Err("handle returned a Response without an Int64 `status`".into()),
            };
            let content_type = match map.get("contentType") {
                Some(Val::Str(s)) => (**s).clone(),
                _ => return Err("handle returned a Response without a String `contentType`".into()),
            };
            let body = match map.get("body") {
                Some(Val::Str(s)) => (**s).clone(),
                _ => return Err("handle returned a Response without a String `body`".into()),
            };
            let vary = match map.get("vary") {
                Some(Val::Str(s)) => (**s).clone(),
                _ => return Err("handle returned a Response without a String `vary`".into()),
            };
            let headers = match map.get("headers") {
                Some(Val::Map(m)) => m
                    .iter()
                    .map(|(k, v)| match v {
                        Val::Str(s) => Ok((k.clone(), (**s).clone())),
                        _ => Err("handle returned a Response whose `headers` holds a non-String"),
                    })
                    .collect::<Result<Vec<_>, _>>()?,
                _ => {
                    return Err(
                        "handle returned a Response without a Map<String, String> `headers`".into(),
                    )
                }
            };
            let resp = ServeResponse {
                status,
                content_type,
                body,
                vary,
                headers,
            };
            // The discriminator is the stream, not a field of the response
            // (RFC-0074 M3a): `handle` answered with a `Response` either way, and
            // what makes this a second shape is that a producer is still open
            // behind it. The response is then a header block and a prologue.
            if interp.live.borrow().is_some() {
                return Ok(ServeAnswer::Live(resp));
            }
            Ok(ServeAnswer::Buffered(resp))
        }
        Ok(other) => Err(format!(
            "handle returned {other:?}, expected a Response record"
        )),
        // A trap after `serveStream` leaves a producer nobody will pull: release
        // it here, since the host never hears about a stream it was not told to
        // write.
        Err(e) => {
            let taken = interp.live.borrow_mut().take();
            if let Some(s) = taken {
                let _ = interp.release_stream(&s);
            }
            Err(match e {
                Ctrl::Err(s) => s,
                Ctrl::Return(_) => "internal: `?` propagated past handle".into(),
            })
        }
    }
}

/// Run a served program with a POOL of `workers` interpreter-owning threads
/// (RFC-0025, `vyrn serve --workers N`). Soundness is the CALLER'S gate: the
/// CLI refuses `--workers` when `handle` transitively touches module state
/// ([`crate::checker::module_state_use`]), so nothing a worker can observe is
/// shared between workers.
///
/// The landed decisions, precisely:
/// - `main` (and module-state initialization) runs ONCE, on a setup
///   interpreter, before any worker starts — its output appears exactly once,
///   like the sequential loop. A missing `main` is fine; a nonzero return or
///   a setup trap aborts the serve before any thread spawns.
/// - EACH worker then builds a fully independent `Interp` and runs
///   `init_globals` again on its own copy: an interpreter needs a well-formed
///   global frame to exist, but the gated `handle` can never read or write
///   one — the per-worker copies are unobservable by construction. (Any
///   `print` inside an initializer would repeat per worker; initializers that
///   print AND a module-state-free `handle` is a shape that cannot observe
///   its own globals, so this stays a documented non-goal, not a soundness
///   hole.)
/// - `worker(i, handler)` runs on worker `i`'s big-stack thread with that
///   worker's private handler; the CLI loops it over an spmc channel of
///   connections.
/// - `accept()` runs on the calling thread once every worker thread has been
///   spawned — it owns the listener (this crate stays network-free). When it
///   returns, its channel sender drops, the workers' `recv` fails, and the
///   scope joins them.
pub fn serve_pool<W, A>(
    program: &Program,
    workers: usize,
    worker: W,
    accept: A,
) -> Result<(), String>
where
    W: Fn(usize, &mut dyn FnMut(ServeCall) -> Result<ServeAnswer, String>) + Send + Sync,
    A: FnOnce() -> Result<(), String> + Send,
{
    std::thread::scope(|s| {
        // Setup: module state + `main`, once, before any worker exists.
        let setup: Result<(), String> = std::thread::Builder::new()
            .stack_size(INTERP_STACK_BYTES)
            .spawn_scoped(s, || -> Result<(), String> {
                let interp = new_interp(program, &[])?;
                if let Err(Ctrl::Err(e)) = interp.init_globals(program) {
                    return Err(e);
                }
                if interp.funcs.contains_key("main") {
                    match interp.call("main", &[]) {
                        Ok(Val::Int(0)) => {}
                        Ok(Val::Int(n)) => {
                            return Err(format!("main returned {n}, aborting serve"))
                        }
                        Ok(other) => {
                            return Err(format!("main returned {other:?}, expected Int64"))
                        }
                        Err(Ctrl::Err(e)) => return Err(e),
                        Err(Ctrl::Return(_)) => {
                            return Err("internal: `?` propagated past main".into())
                        }
                    }
                }
                Ok(())
            })
            .expect("failed to spawn setup interpreter thread")
            .join()
            .unwrap_or_else(|_| Err("interpreter thread panicked (likely stack overflow)".into()));
        setup?;

        let worker = &worker;
        for i in 0..workers {
            std::thread::Builder::new()
                .stack_size(INTERP_STACK_BYTES)
                .spawn_scoped(s, move || {
                    let interp = match new_interp(program, &[]) {
                        Ok(it) => it,
                        Err(e) => {
                            eprintln!("error: worker {i}: {e}");
                            return;
                        }
                    };
                    if let Err(Ctrl::Err(e)) = interp.init_globals(program) {
                        eprintln!("error: worker {i}: {e}");
                        return;
                    }
                    let mut handler = |call: ServeCall| serve_call(&interp, call);
                    worker(i, &mut handler);
                })
                .expect("failed to spawn worker interpreter thread");
        }
        accept()
    })
}

/// One thing a generation read: the resolved key (a file, or a directory with a
/// trailing `/`) and what it held — `None` when the read or the listing FAILED.
///
/// A failure is an observation too. A generator that finds no `examples/`
/// directory emits "0 examples", and that answer stops being right the moment
/// the directory appears. Recording only successes made the entry unable to
/// notice, and the build stayed green with the wrong output.
pub type GenRead = (String, Option<Vec<u8>>);

/// The result of running a generator (RFC-0021): the synthesized module source
/// plus the inputs the generation read, which the loader folds into the
/// content-addressed cache entry.
pub struct GenOutput {
    pub source: String,
    pub reads: Vec<GenRead>,
}

/// Everything a generation run needs from the loader (RFC-0021). Bundled so the
/// [`generate`] signature stays legible.
pub struct GenInputs<'a> {
    pub resolver: &'a dyn crate::loader::ModuleResolver,
    /// The load options (std root + manifest aliases) — needed so `moduleInterface`
    /// can link the reflected module's imports to follow the type closure (RFC-0031).
    pub opts: &'a crate::loader::LoadOptions,
    /// The importing module's directory — the base for relative-path resolution.
    pub importer_dir: String,
    /// Resolved path prefixes the generator may read under (its constant path
    /// args). Empty ⇒ no filesystem access is permitted.
    pub allowed: Vec<String>,
    /// RFC-0107 M2: those constant path arguments that name a manifest
    /// DEPENDENCY, paired with the module key the import map resolves them to.
    /// A mediated read spelling one of them reaches the pinned bytes instead of a
    /// path that exists on no disk.
    ///
    /// This does not widen the sandbox. Every pair comes from the generator's own
    /// constant arguments and its resolved key is one of `allowed`, so the
    /// input-root rule decides exactly as before — an alias is a second SPELLING
    /// of a declared root, not a new root.
    pub aliased: Vec<(String, String)>,
    /// Step budget and output-size cap (guardrails).
    pub fuel: u64,
    pub max_output: usize,
    /// A fingerprint of the generator's own module closure — its keys and the
    /// content hashes the loader hashes them by anyway — or `None` when the
    /// closure contains something no resolver can re-read (a generated module),
    /// so no honest fingerprint exists.
    ///
    /// The interpreter ignores it. It is here for an engine that CACHES a
    /// compiled artifact (RFC-0076 M5): keying on this instead of on the whole
    /// program's `Debug` output turns a 1.1–1.9 ms hash of 4,536 lines into a
    /// string compare, and the loader already computed every part of it.
    pub sources_fingerprint: Option<String>,
}

/// Resolve a mediated path argument against the importer's directory, then
/// enforce that it stays under one of the generator's declared input roots (its
/// constant path args). Returns the resolved resolver key, or the scoping trap
/// message.
///
/// `aliased` is the import-map step (RFC-0107 M2): an argument that names a
/// manifest dependency resolves to the key the LOADER resolved it to, the same
/// key a module specifier of that name would reach. Path arithmetic cannot do
/// this — a dependency's target is a lock-pinned remote key or a path rooted at
/// the MANIFEST's directory, neither of which is `importer_dir/arg` — which is
/// why a `gen fn` could not read a pinned collection file before.
///
/// The input-root rule below still decides, on the resolved key: the loader
/// derives `aliased` from the generator's own constant arguments and puts each
/// resolved key into `allowed`, so an alias adds a SPELLING of a declared root
/// and never a root.
///
/// Public, and a free function, because the wasm generation engine (RFC-0076)
/// mediates its host imports with exactly this rule. Two implementations of a
/// sandbox boundary is one too many.
pub fn gen_scoped_path(
    importer_dir: &str,
    allowed: &[String],
    aliased: &[(String, String)],
    arg: &str,
) -> Result<String, String> {
    let resolved = match aliased.iter().find(|(spelling, _)| spelling == arg) {
        Some((_, key)) => key.clone(),
        None => {
            let joined = if importer_dir.is_empty() {
                arg.to_string()
            } else {
                format!("{importer_dir}/{arg}")
            };
            crate::loader::normalize(&joined)
        }
    };
    let ok = allowed
        .iter()
        .any(|root| resolved == *root || resolved.starts_with(&format!("{root}/")));
    if !ok {
        return Err(format!(
            "generator read `{arg}` escapes its declared inputs ({}) — a generator may only \
             read under its constant path arguments",
            allowed.join(", ")
        ));
    }
    Ok(resolved)
}

/// `moduleInterface(path)` (RFC-0021): read the referenced module through the
/// resolver, link it to follow its reachable type closure (RFC-0031), and build
/// the `ModuleInterface` record literal for its exported surface. Every module
/// the link touched is appended to `reads`, which is what makes editing a
/// closure type's DEFINING file miss the generator cache even though its path
/// was never a generator argument.
///
/// Public, and a free function, for the same reason [`gen_scoped_path`] is: the
/// wasm generation engine (RFC-0076 M3b) serves its `moduleInterface` import
/// from here. Reflection is compiler machinery — a second implementation would
/// be a second answer, and a second set of recorded reads is a stale cache hit.
pub fn gen_module_interface_lit(
    resolver: &dyn crate::loader::ModuleResolver,
    opts: &crate::loader::LoadOptions,
    importer_dir: &str,
    allowed: &[String],
    aliased: &[(String, String)],
    reads: &mut Vec<GenRead>,
    path: &str,
) -> Result<Expr, String> {
    // Resolve like a module specifier (`.vyrn` appended), scoped like readFile.
    // A manifest dependency is left ALONE: the import map answers for the whole
    // spelling, and its target already carries the extension.
    let spec = if path.ends_with(".vyrn")
        || path.ends_with(".json")
        || aliased.iter().any(|(spelling, _)| spelling == path)
    {
        path.to_string()
    } else {
        format!("{path}.vyrn")
    };
    let resolved = gen_scoped_path(importer_dir, allowed, aliased, &spec)?;
    let source = resolver.read(&resolved).map_err(|e| {
        format!(
            "moduleInterface {}: {e}",
            crate::trap::io_at("readerr", &path)
        )
    })?;
    reads.push((resolved.clone(), Some(source.clone().into_bytes())));

    // Follow the reflected module's imports to build the reachable type closure
    // (RFC-0031): link it into one program so a type declared in an imported
    // module is still visible to the closure walk. Every module the link reads is
    // recorded through a proxy resolver, so a closure type's defining FILE joins
    // the generator's cache inputs — editing `types.vyrn` must miss the cache
    // even though its path was never a generator argument.
    let rec = crate::loader::RecordingResolver::new(resolver);
    let program = crate::loader::load(&source, &resolved, opts, &rec).map_err(|diags| {
        let d = diags.first();
        let where_ = d
            .and_then(|d| d.file.clone())
            .map(|f| format!(" ({f})"))
            .unwrap_or_default();
        let msg = d
            .map(|d| d.message.clone())
            .unwrap_or_else(|| "load failed".to_string());
        format!("moduleInterface `{path}`{where_}: {msg}")
    })?;
    // Every module the link read, kept as text for the origin index below — the
    // AST has a declaration's line but not its name column, so the columns come
    // back out of the lexer (RFC-0073 M1). These are the same reads, so a module
    // reflected is a module indexed, by construction.
    let mut origin_src: Vec<(Option<String>, String, String)> =
        vec![(None, resolved.clone(), source.clone())];
    for (p, s) in rec.into_reads() {
        // The root module was already recorded above; skip the duplicate.
        if p != resolved {
            origin_src.push((Some(p.clone()), p.clone(), s.clone()));
            reads.push((p, Some(s.into_bytes())));
        }
    }

    // Import specifier per declaring module, so a generator that must SHARE a
    // closure type's identity (rpcServer/rpcInProcess) can import it from the
    // module that declares it (RFC-0031). The reflected module's own types
    // (`module == None`) keep the generator's own argument spelling; a foreign
    // type gets a specifier relative to the real importing file's directory.
    let mut specifiers: HashMap<Option<String>, String> = HashMap::new();
    specifiers.insert(None, path.to_string());
    for t in &program.type_decls {
        if let Some(key) = &t.module {
            specifiers.entry(Some(key.clone())).or_insert_with(|| {
                crate::loader::import_specifier(importer_dir, key, opts.std_root.as_deref())
            });
        }
    }
    let origins = crate::schema_reflect::Origins::new(
        origin_src
            .iter()
            .map(|(k, f, s)| (k.clone(), f.as_str(), s.as_str())),
    );
    Ok(crate::schema_reflect::module_interface_lit(
        &program,
        &specifiers,
        &origins,
    ))
}

/// An alternative engine for running a generator (RFC-0076).
///
/// The frontend defines this seam and nothing more: compiling a generator to
/// wasm needs codegen and clang, which only the driver has, so the driver
/// installs an engine and the frontend stays free of external dependencies.
///
/// Returning `None` means "not served" — not an error. Every generator the wasm
/// path cannot yet handle falls through to the interpreter, which stays the
/// reference the alternative is checked against.
pub type GenEngine = dyn Fn(
        &Program,
        &str,
        &[crate::consteval::ConstVal],
        &GenInputs<'_>,
    ) -> Option<Result<GenOutput, String>>
    + Send
    + Sync;

static GEN_ENGINE: std::sync::OnceLock<Box<GenEngine>> = std::sync::OnceLock::new();

/// Install the alternative generation engine. Called once, by the driver, before
/// any load. A second call is ignored rather than racing.
pub fn set_gen_engine(engine: Box<GenEngine>) {
    let _ = GEN_ENGINE.set(engine);
}

/// Run `fn_name` in `program` as a **generation target** (RFC-0021): under the
/// capability-mediated sandbox in `inputs`, with `args` (compile-time constants)
/// as its arguments. Returns the returned `String` (the synthesized module
/// source) plus the recorded input reads, or a trap message.
///
/// Runs on the big-stack interpreter thread like [`run`]. The generator is
/// ordinary Vyrn code — the ONLY differences from a normal call are the mediated
/// `readFile`/`listDir`/`moduleInterface` and the step/size guardrails.
pub fn generate(
    program: &Program,
    fn_name: &str,
    args: &[crate::consteval::ConstVal],
    inputs: GenInputs<'_>,
) -> Result<GenOutput, String> {
    // RFC-0076: an installed engine gets first refusal; `None` falls through.
    if let Some(engine) = GEN_ENGINE.get() {
        if let Some(out) = engine(program, fn_name, args, &inputs) {
            return out;
        }
    }
    generate_interpreted(program, fn_name, args, inputs)
}

/// The tree-walking generation path — the reference implementation, and the
/// fallback whenever an installed engine declines.
pub fn generate_interpreted(
    program: &Program,
    fn_name: &str,
    args: &[crate::consteval::ConstVal],
    inputs: GenInputs<'_>,
) -> Result<GenOutput, String> {
    // Runs on the caller's stack (the resolver holds a `RefCell` and is not
    // `Sync`, so it can't cross to a scoped thread). Deep recursion is bounded
    // by the step budget in `inputs.fuel`, so a runaway generator fails with the
    // budget trap long before it could exhaust the stack.
    use crate::consteval::ConstVal;
    let mut interp = new_interp(program, &[])?;
    interp.gen = Some(GenCtx {
        resolver: inputs.resolver,
        opts: inputs.opts,
        importer_dir: inputs.importer_dir,
        allowed: inputs.allowed,
        aliased: inputs.aliased,
        reads: RefCell::new(Vec::new()),
        fuel: std::cell::Cell::new(inputs.fuel),
    });
    if let Err(Ctrl::Err(s)) = interp.init_globals(program) {
        return Err(s);
    }
    let vals: Vec<Val> = args
        .iter()
        .map(|c| match c {
            ConstVal::Int(n) => Val::Int(*n),
            ConstVal::Bool(b) => Val::Bool(*b),
            ConstVal::Float(f) => Val::Float(*f),
            ConstVal::Str(s) => Val::Str(std::rc::Rc::new(s.clone())),
        })
        .collect();
    let source = match interp.call(fn_name, &vals) {
        Ok(Val::Str(s)) => (*s).clone(),
        // A `gen fn` may return `Code` directly (RFC-0054); render it here.
        Ok(Val::Code(pieces)) => render_code(&pieces),
        Ok(other) => {
            return Err(format!(
                "generator `{fn_name}` returned {other:?}, expected a String of module source"
            ))
        }
        Err(Ctrl::Err(s)) => return Err(s),
        Err(Ctrl::Return(_)) => return Err("internal: `?` propagated past a generator".into()),
    };
    if source.len() > inputs.max_output {
        return Err(format!(
            "generator `{fn_name}` produced {} bytes of source, over the {}-byte cap",
            source.len(),
            inputs.max_output
        ));
    }
    let reads = interp.gen.as_ref().unwrap().reads.borrow().clone();
    // `VYRN_GEN_STEPS=1` — what this generator actually spent of its budget. The
    // wasm engine meters in wasm instructions instead (RFC-0076 M5), and the
    // multiplier between the two units is only defensible if both sides can be
    // measured on the same run; this is the interpreted half of that pair.
    if std::env::var("VYRN_GEN_STEPS").is_ok() {
        eprintln!(
            "gen steps {fn_name}: {}",
            inputs.fuel - interp.gen.as_ref().unwrap().fuel.get()
        );
    }
    Ok(GenOutput { source, reads })
}

/// Build a fresh interpreter over `program` (shared setup for `run` and
/// `run_tests`): the ownership plan, function/type/variant indexes, and the log
/// sink. Does NOT initialize module state — call [`Interp::init_globals`].
fn new_interp<'a>(program: &'a Program, prog_args: &[String]) -> Result<Interp<'a>, String> {
    // The same ownership analysis the native backend uses to reclaim heap
    // values at block exit. The interpreter executes the identical plan, so the
    // three engines release the same bindings at the same points.
    // Identities are `Stmt` node addresses — unique program-wide, so the
    // per-function maps flatten into one.
    let ownership = crate::own::analyze(program);
    let droppable: HashMap<usize, crate::own::DropKind> =
        ownership.droppable.into_values().flatten().collect();
    let funcs: HashMap<&str, &Function> = program
        .functions
        .iter()
        .map(|f| (f.name.as_str(), f))
        .collect();
    let types: HashMap<&str, &TypeDecl> = program
        .type_decls
        .iter()
        .map(|t| (t.name.as_str(), t))
        .collect();
    // Owned copy for `crate::types::resolve` / `crate::codec` (JSON codec,
    // RFC-0018), which need `&HashMap<String, TypeDecl>`.
    let type_map: HashMap<String, TypeDecl> = program
        .type_decls
        .iter()
        .map(|t| (t.name.clone(), t.clone()))
        .collect();
    // Module contracts (RFC-0071), for the `contractOf(Name)` reflection.
    let contracts: HashMap<&str, &ContractDecl> = program
        .contracts
        .iter()
        .map(|c| (c.name.as_str(), c))
        .collect();
    // Enum variant names, so constructor uses (Var/Call) can be recognized.
    let mut variants: std::collections::HashSet<&str> = std::collections::HashSet::new();
    for t in &program.type_decls {
        if let Type::Enum(vs) = &t.base {
            for v in vs {
                variants.insert(v.name.as_str());
            }
        }
    }
    // Open the log file up front if the program directs logs to one.
    let log_file = match &program.log_sink {
        LogSink::File(path) => {
            let f = std::fs::File::create(path)
                .map_err(|e| format!("cannot open log file `{path}`: {e}"))?;
            RefCell::new(Some(f))
        }
        _ => RefCell::new(None),
    };
    let interp = Interp {
        funcs,
        // RFC-0092 M4: whether the release walk can find anything at all. A
        // program with no `impl Owned` in it declares no reclamation this engine
        // has to run, so the walk over what a binding holds is never made.
        has_owned: program
            .impls
            .iter()
            .any(|i| i.protocol == crate::types::OWNED),
        impls: &program.impls,
        types,
        contracts,
        type_map,
        variants,
        droppable,
        boxes: RefCell::new(HashMap::new()),
        next_box: std::cell::Cell::new(1),
        log_level: program.log_level,
        log_sink: program.log_sink.clone(),
        log_file,
        protocol_methods: program
            .protocols
            .iter()
            .flat_map(|p| p.methods.iter().map(|m| (m.name.clone(), p.name.clone())))
            .collect(),
        variant_enum: program
            .type_decls
            .iter()
            .filter_map(|d| match &d.base {
                Type::Enum(vs) => Some(vs.iter().map(|v| (v.name.clone(), d.name.clone()))),
                _ => None,
            })
            .flatten()
            .collect(),
        region_depth: std::cell::Cell::new(0),
        call_depth: std::cell::Cell::new(0),
        globals: RefCell::new(Frame::default()),
        args: prog_args.to_vec(),
        mono_counter: std::cell::Cell::new(0),
        live: RefCell::new(None),
        gen: None,
    };
    Ok(interp)
}

struct Interp<'a> {
    funcs: HashMap<&'a str, &'a Function>,
    /// Every `impl` block, for `place` projection lookup (RFC-0091 M2). A
    /// projection is not a function, so `funcs` cannot answer for it.
    impls: &'a [crate::ast::ImplBlock],
    /// Whether the program declares any `impl Owned` at all — the gate on the
    /// RFC-0092 M4 walk over what a binding holds.
    has_owned: bool,
    types: HashMap<&'a str, &'a TypeDecl>,
    /// Module contracts (RFC-0071), keyed by name — the source `contractOf`
    /// reflects. Comptime-only, so this is read by exactly one builtin.
    contracts: HashMap<&'a str, &'a ContractDecl>,
    /// Owned type map for `resolve`/codec (RFC-0018 JSON codec).
    type_map: HashMap<String, TypeDecl>,
    variants: std::collections::HashSet<&'a str>,
    /// Droppable `let` bindings (by `Stmt` node address) and their reclamation
    /// kind — the ownership analysis shared with the native backend.
    droppable: HashMap<usize, crate::own::DropKind>,
    /// The boxed streams (RFC-0075 M2c, re-hosted by RFC-0090 M3): what
    /// `boxStream` moved out of the program and `unboxStream` moves back in, keyed by
    /// the address it handed back. The compiled backends `malloc` one header and
    /// answer its address; here the address is a serial number, which is the
    /// same statement — an address either names a boxed stream or it does not,
    /// and `unboxStream` and `pullAt` trap when it does not.
    boxes: RefCell<HashMap<i64, StreamVal>>,
    /// The next address `boxStream` hands out. Never reused, so a stale address
    /// is a trap rather than a different stream.
    next_box: std::cell::Cell<i64>,
    /// The logging threshold ordinal (RFC-0008); calls below it are skipped.
    log_level: usize,
    /// Where log records are written (RFC-0008).
    log_sink: LogSink,
    /// The open log file, when `log_sink` is [`LogSink::File`].
    log_file: RefCell<Option<std::fs::File>>,
    /// Protocol methods (RFC-0002 §5): method name -> protocol name.
    protocol_methods: HashMap<String, String>,
    /// Enum variant name -> its enum's name, for dispatching on enum receivers.
    variant_enum: HashMap<String, String>,
    /// Current `region { .. }` nesting depth. The native runtime runs regions
    /// on a fixed 64-slot arena stack and traps past it; the interpreter
    /// enforces the same bound so the two stay observably identical.
    region_depth: std::cell::Cell<usize>,
    /// Vyrn calls currently in flight ([`CALL_DEPTH_LIMIT`]).
    call_depth: std::cell::Cell<u32>,
    /// Persistent module-state frame (RFC-0013): every function-call scope stack
    /// bottoms out on this. Populated once (in declaration order) before
    /// `main`; variable reads/writes fall back to it when the local scope misses.
    /// Slot-typed so reassignments coerce (and auto-validate) exactly like locals.
    globals: RefCell<Frame>,
    /// The program's command-line arguments (RFC-0014 `args()`), argv[1..].
    args: Vec<String>,
    /// Per-call counter for the fixed monotonic clock (RFC-0043): under
    /// `VYRN_FIXED_TIME`, `monotonic()` returns `1e9 + n * 1e6` on the nth call,
    /// mirroring the C shim exactly so successive readings are byte-identical
    /// across the three backends.
    mono_counter: std::cell::Cell<i64>,
    /// The stream a request handed to the host (RFC-0074 M3a `serveStream`).
    /// Parked here between the call that opened it and the host's pulls, because
    /// a streaming answer outlives the `handle` that produced it — which is the
    /// one thing RFC-0075's linearity otherwise forbids, and why the host owes it
    /// a `close`. At most one per request: a second `serveStream` traps.
    live: RefCell<Option<StreamVal>>,
    /// Set only while running a `gen fn` as a generation target (RFC-0021). When
    /// present: `readFile`/`listDir`/`moduleInterface` route through the loader's
    /// resolver (path-scoped + recorded as cache inputs), and every statement
    /// spends a unit of the step budget. Absent for ordinary `run`/`test`/`serve`.
    gen: Option<GenCtx<'a>>,
}

/// The generation sandbox (RFC-0021): the capability-mediated I/O + guardrails a
/// `gen fn` runs under when invoked as an import target. Owned by the [`Interp`]
/// for the duration of one generation.
pub(crate) struct GenCtx<'a> {
    /// The loader's resolver — the single mediated I/O channel.
    resolver: &'a dyn crate::loader::ModuleResolver,
    /// Load options (std root + manifest aliases), so `moduleInterface` can link
    /// the reflected module's imports and follow its type closure (RFC-0031).
    opts: &'a crate::loader::LoadOptions,
    /// The importing module's directory — the base for resolving the generator's
    /// relative path arguments (`readFile`/`listDir`/`moduleInterface`).
    importer_dir: String,
    /// Resolved path prefixes the generator may read under — its constant path
    /// arguments. A mediated read outside all of them is a trap.
    allowed: Vec<String>,
    /// The import-map step for those arguments that name a manifest dependency
    /// (RFC-0107 M2): `(spelling, resolved module key)`.
    aliased: Vec<(String, String)>,
    /// Every input read, in order: `(resolved path, bytes)`. Folded into the
    /// content-addressed cache key so a changed input invalidates the cache.
    reads: RefCell<Vec<GenRead>>,
    /// Remaining step budget; each statement spends one. Zero ⇒ the generator is
    /// killed with the canonical "exceeded its step budget" trap.
    fuel: std::cell::Cell<u64>,
}

/// A scope binding: the current value plus the declared type, when one exists
/// (a `let` annotation or a function parameter). The type is what a later
/// assignment must coerce — and therefore auto-validate — back into, mirroring
/// the native backend's typed stores.
#[derive(Clone)]
struct Slot {
    v: Val,
    ty: Option<Type>,
}

impl Slot {
    fn untyped(v: Val) -> Slot {
        Slot { v, ty: None }
    }
}

thread_local! {
    /// Line-start offsets per buffer, for `lineAt`/`colAt`.
    /// `array pointer -> (the array, its line-start offsets)`. The array is held
    /// so the address stays valid and unique for as long as the entry lives.
    #[allow(clippy::type_complexity)]
    static LINE_STARTS: std::cell::RefCell<
        std::collections::HashMap<usize, (std::rc::Rc<Vec<Val>>, std::rc::Rc<Vec<usize>>)>,
    > = std::cell::RefCell::new(std::collections::HashMap::new());
}

impl<'a> Interp<'a> {
    /// Initialize module state (RFC-0013) once, in declaration order, before
    /// `main` (or, under `vyrn test`, before the first test). Each initializer
    /// runs in a fresh empty local scope; a read of an earlier global falls back
    /// to the persistent frame populated as we go. The declared/annotated type is
    /// remembered so later assignments coerce.
    fn init_globals(&self, program: &Program) -> Result<(), Ctrl> {
        for g in &program.globals {
            let mut scope: Vec<Frame> = vec![Frame::default()];
            let mut v = self.expr(&g.init, &mut scope)?;
            if let Some(t) = &g.ty {
                v = self.coerce(v, t)?;
            }
            // Infer the type when there is no annotation, exactly as `Stmt::Let`
            // does for a local: an unannotated `let mut g = T { xs: [] }` is a
            // record whose field types are the only hook a later `g.xs.push(v)`
            // has to validate against, and without this the interpreter accepted
            // a value both compiled backends trapped on (RFC-0082 M3).
            let ty = match &g.ty {
                Some(t) => Some(t.clone()),
                None => self.type_of(&g.init, &scope),
            };
            self.globals
                .borrow_mut()
                .insert(g.name.clone(), Slot { v, ty });
        }
        Ok(())
    }

    // ---- generation sandbox (RFC-0021) ----------------------------------

    /// Resolve a mediated path argument against the importer's directory, then
    /// enforce that it stays under one of the generator's declared input roots
    /// (its constant path args). Returns the resolved key or a scoping trap.
    fn gen_scoped_path(&self, arg: &str) -> Result<String, Ctrl> {
        let g = self.gen.as_ref().expect("gen context");
        gen_scoped_path(&g.importer_dir, &g.allowed, &g.aliased, arg).map_err(Ctrl::Err)
    }

    /// Mediated `readFile` (RFC-0021): read through the resolver, record the
    /// bytes for the cache key, return a Vyrn `Result<String, String>`.
    fn gen_read_file(&self, path: &str) -> Result<Val, Ctrl> {
        let resolved = self.gen_scoped_path(path)?;
        let g = self.gen.as_ref().unwrap();
        match g.resolver.read(&resolved) {
            Ok(content) => {
                g.reads
                    .borrow_mut()
                    .push((resolved, Some(content.clone().into_bytes())));
                if content.as_bytes().contains(&0) {
                    return Ok(Val::Result(
                        false,
                        Box::new(Val::Str(std::rc::Rc::new(crate::trap::io_at(
                            "nulerr", &path,
                        )))),
                    ));
                }
                Ok(Val::Result(
                    true,
                    Box::new(Val::Str(std::rc::Rc::new(content))),
                ))
            }
            Err(why) => {
                // A read that FAILED is recorded too: the generator branched on
                // "not there", so the entry must miss once the file appears.
                let remote = crate::loader::is_remote(&resolved);
                g.reads.borrow_mut().push((resolved, None));
                if remote {
                    // A PINNED dependency that cannot be produced is not a
                    // condition to branch on — it is "locked but not cached" or
                    // "the upstream changed under an immutable URL", each with a
                    // remedy the resolver already spells, and each a broken build.
                    // So it aborts the generation with that refusal verbatim (the
                    // one a module import of the same key prints) rather than
                    // becoming an `Err` value under the canonical `readFile`
                    // wording, which would hide the remedy. The wasm generation
                    // engine refuses the same read the same way, because its
                    // status alphabet carries no message and an answer that
                    // differs by engine is two answers.
                    return Err(Ctrl::Err(why));
                }
                Ok(Val::Result(
                    false,
                    Box::new(Val::Str(std::rc::Rc::new(crate::trap::io_at(
                        "readerr", &path,
                    )))),
                ))
            }
        }
    }

    /// Mediated `listDir` (RFC-0021): list through the resolver, record the
    /// (sorted) listing for the cache key.
    fn gen_list_dir(&self, path: &str) -> Result<Val, Ctrl> {
        let resolved = self.gen_scoped_path(path)?;
        let g = self.gen.as_ref().unwrap();
        match g.resolver.list(&resolved) {
            Ok(mut names) => {
                names.sort();
                // Record the listing as a synthetic input so a directory whose
                // contents change invalidates the cache.
                g.reads
                    .borrow_mut()
                    .push((format!("{resolved}/"), Some(names.join("\n").into_bytes())));
                Ok(Val::Result(
                    true,
                    Box::new(Val::Array(std::rc::Rc::new(
                        names
                            .into_iter()
                            .map(|n| Val::Str(std::rc::Rc::new(n)))
                            .collect(),
                    ))),
                ))
            }
            Err(_) => {
                // A listing that FAILED is an input as much as one that worked:
                // the directory being absent is what the generator saw, and a
                // directory that appears must invalidate the entry.
                g.reads.borrow_mut().push((format!("{resolved}/"), None));
                Ok(Val::Result(
                    false,
                    Box::new(Val::Str(std::rc::Rc::new(crate::trap::io_at(
                        "listerr", &path,
                    )))),
                ))
            }
        }
    }

    /// `moduleInterface(path)` (RFC-0021): parse the referenced module through
    /// the resolver (recording its bytes) and build the `ModuleInterface` record
    /// literal for its EXPORTED surface. Generation-only — a runtime call traps.
    fn gen_module_interface(&self, path: &str) -> Result<Expr, Ctrl> {
        if self.gen.is_none() {
            return Err(Ctrl::Err(
                "`moduleInterface` is only available during generation".to_string(),
            ));
        }
        let g = self.gen.as_ref().unwrap();
        let mut reads = Vec::new();
        let r = gen_module_interface_lit(
            g.resolver,
            g.opts,
            &g.importer_dir,
            &g.allowed,
            &g.aliased,
            &mut reads,
            path,
        );
        // Recorded whether or not the reflection succeeded: the root module is
        // read before the link can fail, and a read that happened is a cache
        // input either way.
        g.reads.borrow_mut().extend(reads);
        r.map_err(Ctrl::Err)
    }

    fn call(&self, name: &str, args: &[Val]) -> Result<Val, Ctrl> {
        Ok(self.call_capturing(name, args)?.0)
    }

    /// The value of an RFC-0043 host-boundary extern (`hostNowMillis` /
    /// `hostMonotonicNanos` / `hostRandomSeed`), or `None` for any other extern.
    /// Honors `VYRN_FIXED_TIME` / `VYRN_FIXED_SEED` exactly like the native/wasi
    /// C shims (the parity contract); absent the env vars, reads the real host.
    fn host_boundary_value(&self, name: &str) -> Option<Val> {
        match name {
            "hostNowMillis" => {
                if let Some(ms) = fixed_env_i64("VYRN_FIXED_TIME") {
                    return Some(Val::Int(ms));
                }
                Some(Val::Int(host_epoch_millis()))
            }
            "hostMonotonicNanos" => {
                if fixed_env_i64("VYRN_FIXED_TIME").is_some() {
                    // Mirror the C shim: 1e9 + n*1e6 on the nth call.
                    let n = self.mono_counter.get();
                    self.mono_counter.set(n + 1);
                    return Some(Val::Int(1_000_000_000 + n * 1_000_000));
                }
                Some(Val::Int(host_epoch_nanos()))
            }
            "hostRandomSeed" => {
                if let Some(seed) = fixed_env_i64("VYRN_FIXED_SEED") {
                    return Some(Val::Int(seed));
                }
                // No injected seed: derive one from the wall clock (the CSPRNG
                // guarantee is a native/wasm property; the interpreter is the
                // reference for the FIXED path, which is all parity observes).
                Some(Val::Int(host_epoch_nanos()))
            }
            _ => None,
        }
    }

    /// Materialize a lambda literal into a closure value (RFC-0023). Captures are
    /// the CURRENT values of every visible local binding — a by-value snapshot,
    /// which is semantically exact because captures are read-only. Fixing them
    /// here (at the outer call site, where the argument is evaluated) is the
    /// capture-timing lock: a binding reassigned between now and the lambda's
    /// invocation is not observed, identically in every backend. Module state is
    /// NOT snapshotted — a global read inside the body resolves live, as in any
    /// function.
    fn make_closure(
        &self,
        params: &[String],
        body: &LambdaBody,
        scope: &[Frame],
        param_tys: Vec<Type>,
        ret: Type,
    ) -> Val {
        // Flatten outer→inner so an inner binding shadows an outer one, matching
        // lexical resolution at the definition site.
        let mut env: HashMap<String, Val> = HashMap::new();
        for frame in scope.iter() {
            for (k, slot) in frame {
                env.insert(k.clone(), slot.v.clone());
            }
        }
        let captures: Vec<(String, Val)> = env.into_iter().collect();
        Val::Fn(Box::new(FnVal::Lambda {
            params: params.to_vec(),
            body: body.clone(),
            captures,
            param_tys,
            ret,
        }))
    }

    /// Look up `name` in the local scope — or module state (RFC-0037) — and
    /// return a clone if it is a function value: the dispatch step for a call
    /// through a `fn`-typed parameter or any stored fn-typed binding.
    fn lookup_fnval(&self, scope: &[Frame], name: &str) -> Option<FnVal> {
        for frame in scope.iter().rev() {
            if let Some(slot) = frame.get(name) {
                return match &slot.v {
                    Val::Fn(fv) => Some((**fv).clone()),
                    _ => None,
                };
            }
        }
        // Module state of function type (RFC-0037) — read live, like any global.
        if let Some(slot) = self.globals.borrow().get(name) {
            if let Val::Fn(fv) = &slot.v {
                return Some((**fv).clone());
            }
        }
        None
    }

    /// Evaluate a `fn`-typed argument (RFC-0023) into a function value, given the
    /// parameter's expected `fn(param_tys) -> ret` type. A lambda literal captures
    /// its environment here; a bare name is a pass-through of an existing function
    /// value or a reference to a named top-level function.
    fn eval_fn_arg(&self, arg: &Expr, scope: &mut Vec<Frame>, fnty: &Type) -> Result<Val, Ctrl> {
        let (ptys, ret) = match fnty {
            Type::Fn(ps, r) => (ps.clone(), (**r).clone()),
            _ => (Vec::new(), Type::Unit),
        };
        match arg {
            Expr::Lambda { params, body, .. } => {
                Ok(self.make_closure(params, body, scope, ptys, ret))
            }
            Expr::Var { name, .. } => {
                if let Some(fv) = self.lookup_fnval(scope, name) {
                    return Ok(Val::Fn(Box::new(fv)));
                }
                if self.funcs.contains_key(name.as_str()) {
                    return Ok(Val::Fn(Box::new(FnVal::Named(name.clone()))));
                }
                Err(format!("`{name}` is not a function value").into())
            }
            other => self.expr(other, scope),
        }
    }

    /// Invoke a function value (RFC-0023): a named function is called directly; a
    /// lambda binds its captured snapshot plus its arguments and runs its body.
    fn call_fnval(&self, fv: &FnVal, args: &[Val]) -> Result<Val, Ctrl> {
        match fv {
            // Forcing a thunk is calling what it wraps — the tag is about where
            // the value is READ, not about how it is invoked.
            FnVal::Thunk(inner) => self.call_fnval(inner, args),
            FnVal::Named(name) => self.call(name, args),
            FnVal::Lambda {
                params,
                body,
                captures,
                param_tys,
                ret,
            } => {
                let mut scope: Vec<Frame> = vec![Frame::default()];
                // The captured environment is the outer (read-only) frame.
                for (k, v) in captures {
                    scope[0].insert(k.clone(), Slot::untyped(v.clone()));
                }
                // Then the lambda's own parameters shadow captures, coerced to the
                // signature's parameter types (sized-int wrapping / validation).
                scope.push(Frame::default());
                for (i, p) in params.iter().enumerate() {
                    let v = args.get(i).cloned().unwrap_or(Val::Unit);
                    let v = match param_tys.get(i) {
                        Some(t) => self.coerce(v, t)?,
                        None => v,
                    };
                    scope.last_mut().unwrap().insert(
                        p.clone(),
                        Slot {
                            v,
                            ty: param_tys.get(i).cloned(),
                        },
                    );
                }
                let out = match body {
                    LambdaBody::Expr(e) => self.expr(e, &mut scope)?,
                    LambdaBody::Block(b) => match self.block(b, &mut scope) {
                        Ok(Flow::Return(v)) => v,
                        Ok(Flow::Normal) => Val::Unit,
                        // `break`/`continue` outside a loop are a checker error,
                        // so they never legitimately reach a body top level.
                        Ok(Flow::Break | Flow::Continue) => {
                            return Err("internal: `break`/`continue` escaped a body".into())
                        }
                        Err(Ctrl::Return(v)) => v,
                        Err(e) => return Err(e),
                    },
                };
                self.coerce(out, ret)
            }
        }
    }

    /// Like [`call`], but also returns the final values of the parameters (so the
    /// caller can copy `modify` parameters back — call-by-value-result).
    fn call_capturing(&self, name: &str, args: &[Val]) -> Result<(Val, Vec<Val>), Ctrl> {
        // An `extern` (RFC-0012) is the host's frame, not Vyrn's, and no backend
        // gives it one to count; an unknown name errors before any frame exists.
        // Both are excluded so the three engines count exactly the same calls.
        let counted = self.funcs.get(name).is_some_and(|f| !f.is_extern);
        if counted {
            let d = self.call_depth.get() + 1;
            if d > CALL_DEPTH_LIMIT {
                return Err(Ctrl::Err(crate::trap::call_depth()));
            }
            self.call_depth.set(d);
        }
        let r = self.call_capturing_inner(name, args);
        // Balanced on every path out, including a trap the caller catches: a
        // `test` run calls many bodies in one process, and a depth left behind
        // would refuse the next one.
        if counted {
            self.call_depth.set(self.call_depth.get() - 1);
        }
        r
    }

    fn call_capturing_inner(&self, name: &str, args: &[Val]) -> Result<(Val, Vec<Val>), Ctrl> {
        let f = self
            .funcs
            .get(name)
            .ok_or_else(|| Ctrl::Err(format!("call to unknown function `{name}`")))?;
        // An `extern` (RFC-0012) is host-provided; the interpreter has no host to
        // call, so a *call* traps with the canonical wording (byte-identical to
        // the native backend's inline trap). Declaring one is fine — only calling
        // it here is the effect the interpreter cannot honor.
        if f.is_extern {
            // RFC-0043 host-boundary externs (time/random) have real semantics
            // here too — honoring the same injected env the native/wasi shims do,
            // so a fixed-clock/fixed-seed program is byte-identical to them.
            if let Some(v) = self.host_boundary_value(name) {
                return Ok((v, Vec::new()));
            }
            return Err(Ctrl::Err(extern_unavailable(name)));
        }
        let mut scope: Vec<Frame> = vec![Frame::default()];
        for (p, v) in f.params.iter().zip(args) {
            // Coerce each argument to its parameter type (sized-int wrapping,
            // and automatic validation into predicated types).
            let coerced = self.coerce(v.clone(), &p.ty)?;
            scope[0].insert(
                p.name.clone(),
                Slot {
                    v: coerced,
                    ty: Some(p.ty.clone()),
                },
            );
        }
        // A `?` inside the body surfaces as Ctrl::Return; catch it as the result.
        let ret = match self.block(&f.body, &mut scope) {
            Ok(Flow::Return(v)) => v,
            Ok(Flow::Normal) => Val::Unit,
            // `break`/`continue` outside a loop are a checker error.
            Ok(Flow::Break | Flow::Continue) => {
                return Err("internal: `break`/`continue` escaped a body".into())
            }
            Err(Ctrl::Return(v)) => v,
            Err(e) => return Err(e),
        };
        // Coerce the return value to the declared return type.
        let ret = self.coerce(ret, &f.ret)?;
        let finals = f
            .params
            .iter()
            .map(|p| {
                scope[0]
                    .get(&p.name)
                    .map(|s| s.v.clone())
                    .unwrap_or(Val::Unit)
            })
            .collect();
        Ok((ret, finals))
    }

    /// Construct a validated-type value: evaluate the refinement predicate on
    /// `v` and fail if it does not hold. The runtime representation of a
    /// validated value is just its base value (zero overhead).
    fn construct(&self, decl: &TypeDecl, v: Val) -> Result<Val, Ctrl> {
        if !self.validates(decl, &v)? {
            return Err(crate::trap::validation(&decl.name, false).into());
        }
        Ok(v)
    }

    fn block(&self, block: &Block, scope: &mut Vec<Frame>) -> Result<Flow, Ctrl> {
        scope.push(Frame::default());
        // Values reclaimed when this frame exits — normally, via `return`,
        // `break` or `continue`, or via a propagating `?` — mirroring the native
        // backend's block-exit drops. Only a reference
        // release is observable here (the slab slot is recycled and stale
        // aliases must trap); string/array buffers are host-reclaimed.
        //
        // Each entry is the binding's NAME, the value if it is already frozen,
        // and — when the type declared `impl Owned` (RFC-0086 M1) — the
        // `release` to call. The value is read out of the SLOT at exit, which is
        // what both compiling backends load out of the alloca, so a `mut`
        // binding releases what it holds last in all three engines (Phase 8b).
        // A binding is frozen early only when a later `let` in this same block
        // rebinds the name: the slot stops being this binding's there, and
        // reading it at exit would release one value twice and the other never.
        //
        // The fourth field is the `Stmt::Let`'s node address — `own`'s own key
        // for the binding, and what RFC-0101 M4's shadow trace reports so that
        // one gate can assert this engine's order against the other two.
        let mut drops: Vec<(&str, Option<Val>, Option<String>, usize)> = Vec::new();
        let blk = block as *const Block as usize;
        for stmt in &block.stmts {
            if let Stmt::Let { name, .. } = stmt {
                for d in drops.iter_mut().filter(|d| d.0 == name && d.1.is_none()) {
                    d.1 = scope
                        .last()
                        .unwrap()
                        .get(name.as_str())
                        .map(|s| s.v.clone());
                }
            }
            let flow = self.stmt(stmt, scope);
            match flow {
                // Any early exit — `return`, `break`, or `continue` — runs this
                // frame's drops on the way out, exactly as a normal frame exit
                // does (RFC-0060: break/continue drop what a normal iteration
                // end would). The signal then propagates to the enclosing loop.
                Ok(flow @ (Flow::Return(_) | Flow::Break | Flow::Continue)) => {
                    // The signal carries no node, so the site is the one the
                    // statement that RAISED it left behind — RFC-0101 M4's
                    // second phase, and the only thing the trace asks of this
                    // engine that the compiled ones do not need.
                    self.run_drops(None, &drops, scope)?;
                    scope.pop();
                    return Ok(flow);
                }
                Ok(Flow::Normal) => {
                    if let Stmt::Let { name, .. } = stmt {
                        let release = match self.droppable.get(&(stmt as *const Stmt as usize)) {
                            // A user type's `release` is ordinary Vyrn and may
                            // print, so this engine has to run it too — it is the
                            // only auto-reclamation that is observable from inside
                            // the language.
                            Some(crate::own::DropKind::Release(f, _)) => Some(Some(f.clone())),
                            // RFC-0092 M4: the binding declares no release, and
                            // what it HOLDS may. `Some(None)` asks `run_drops`
                            // for the walk. A program with no `impl Owned` in it
                            // has nothing the walk could find, so it is not made.
                            Some(crate::own::DropKind::Deep(_)) if self.has_owned => Some(None),
                            _ => None,
                        };
                        if let Some(f) = release {
                            drops.push((name.as_str(), None, f, stmt as *const Stmt as usize));
                        }
                    }
                }
                // A `?` leaves the function through the error channel
                // (`Ctrl::Return`), and a propagating `?` is a function exit like
                // any other: both compiled backends emit the whole early-exit
                // walk before the `ret` they propagate through
                // (`Gen::emit_all_drops`, `Fn_::emit_releases_above`). This arm
                // used to skip it, and RFC-0101 M4 wrote the program that made
                // the skip visible: a declared `release` is ordinary Vyrn and can
                // print, so the three engines printed different output for
                // `examples/releaseacrosstry.vyrn` and the corpus had never
                // reached it.
                //
                // A genuine error (`Ctrl::Err`) is not an exit — it traps, and
                // neither backend reclaims anything on the way out of a trap.
                Err(e) => {
                    if matches!(e, Ctrl::Return(_)) {
                        self.run_drops(None, &drops, scope)?;
                    }
                    scope.pop();
                    return Err(e);
                }
            }
        }
        let r = self.run_drops(Some(blk), &drops, scope);
        scope.pop();
        r?;
        Ok(Flow::Normal)
    }

    /// Execute a frame's pending block-exit drops: release each captured
    /// reference (bumping its slot's generation, exactly like the emitted
    /// `release` in the native backend), and call each declared `release`.
    ///
    /// Newest binding first, which is the order both compiling backends emit
    /// (`emit_all_drops` and `emit_releases_above` both walk their frame in
    /// reverse). It never mattered for a cell; a declared `release` can print, so
    /// now it does.
    ///
    /// An unfrozen entry reads its value out of the frame that is about to be
    /// popped — the slot's last value, which is what the compiling backends
    /// load. A name with no slot left is skipped rather than released as `Unit`.
    fn run_drops(
        &self,
        at: Option<usize>,
        drops: &[(&str, Option<Val>, Option<String>, usize)],
        scope: &[Frame],
    ) -> Result<(), Ctrl> {
        // `Some(block)` is the fall-through exit and is its own walk. `None` is
        // a frame being LEFT by a signal, and its steps join the walk that
        // signal opened — the place is taken BEFORE any release runs, because a
        // release is ordinary Vyrn and the callee's own block exits land in the
        // log between this frame and the next one out.
        let slot = at.is_none().then(crate::own::trace::joining);
        // RFC-0101 M4's shadow: the sequence this engine walks, made readable so
        // one gate can assert it against the other two and against the placement
        // `vyrn_lower` computes. Built only while the trace is on, and a drop
        // that fails mid-walk records nothing — the program is already leaving.
        let mut walked = Vec::new();
        for (name, frozen, release, binding) in drops.iter().rev() {
            let v = match frozen {
                Some(v) => v.clone(),
                None => match scope.last().and_then(|f| f.get(*name)) {
                    Some(s) => s.v.clone(),
                    None => continue,
                },
            };
            if crate::own::trace::on() {
                walked.push(*binding);
            }
            match release {
                Some(f) => {
                    self.call(f, std::slice::from_ref(&v))?;
                }
                // The binding declared no release of its own, so what it HOLDS
                // may declare one — RFC-0092 M4.
                None => self.release_nested(&v)?,
            }
        }
        match (at, slot) {
            (Some(blk), _) => crate::own::trace::note(crate::own::trace::Exit::Block, blk, walked),
            (None, Some(slot)) => crate::own::trace::joined(slot, walked),
            (None, None) => {}
        }
        Ok(())
    }

    /// Release the temporary a CONSTRUCT owns, once it is done with it —
    /// RFC-0101 M4's second phase, step 0.
    ///
    /// `own`'s `droppable` map has four kinds of row. Three of them are keyed by
    /// a construct rather than by a binding: a `match`'s scrutinee (keyed by the
    /// match expression), an `if let`'s (Phase 10a) and a `for`-in's iterable
    /// (RFC-0092 M5), each a value nothing named and nothing else can reclaim.
    /// Both compiled backends put each of those on a release frame of its own,
    /// so it runs at the construct's fall-through and on every early exit out of
    /// an arm. **This engine acted on one of the four**, and RFC-0101 §1.4
    /// recorded that as a documented difference — which is right for a buffer
    /// the host reclaims and wrong for a declared `release`, which is ordinary
    /// Vyrn and can print. `examples/releaseacrossexit.vyrn` is the program that
    /// made it visible: three lines the two compiled backends printed and this
    /// one did not, on a shape anybody could write, and the corpus had never
    /// reached it. It is the same defect the `?` path was one phase earlier.
    ///
    /// A row exists only where nothing took the value — an arm that hands its
    /// payload out marks the row and the binding the payload flowed into is the
    /// one owner there is — so the handover needs no case here: it is the
    /// ABSENCE of a row.
    /// `unwound` says whether the construct is being LEFT by a signal rather
    /// than finishing: a `return` out of an arm reclaims this temporary as one
    /// step of the function's whole walk, and both compiled backends emit it
    /// exactly there, because the frame is one of the frames `emit_all_drops`
    /// crosses. Falling through is its own exit and its own step.
    fn release_temp<T>(
        &self,
        key: usize,
        sv: &Val,
        unwound: bool,
        r: Result<T, Ctrl>,
    ) -> Result<T, Ctrl> {
        // A trap reclaims nothing in either compiled backend, so it reclaims
        // nothing here. Every other way out — falling through, `break`,
        // `return`, a propagating `?` — pays, which is the rule `Interp::block`
        // already applies to a `let`.
        if matches!(r, Err(Ctrl::Err(_))) {
            return r;
        }
        let slot = unwound.then(crate::own::trace::joining);
        let mut walked = Vec::new();
        match self.droppable.get(&key) {
            Some(crate::own::DropKind::Release(f, _)) => {
                walked.push(key);
                self.call(f, std::slice::from_ref(sv))?;
            }
            // RFC-0092 M4: the temporary declares no release and what it HOLDS
            // may. A program with no `impl Owned` has nothing the walk could
            // find, so it is not made — the same gate `Interp::block` uses.
            Some(crate::own::DropKind::Deep(_)) if self.has_owned => {
                walked.push(key);
                self.release_nested(sv)?
            }
            _ => {}
        }
        match slot {
            Some(slot) => crate::own::trace::joined(slot, walked),
            None => crate::own::trace::note(crate::own::trace::Exit::Scrutinee, key, walked),
        }
        r
    }

    /// Call every declared `release` (RFC-0086 M1) that a value reaching the end
    /// of its scope holds — RFC-0092 M4.
    ///
    /// The two compiling backends walk a release by TYPE. Here the value carries
    /// what the walk needs, because `coerce` stamps a record with the name it
    /// crossed its boundary as, and that name is the key a declared row is read
    /// by. A declared row STOPS the walk at the place that declared it, exactly
    /// as `deep_release` and `rel_at` stop: the declaration says what it
    /// reclaims, and reaching past it would reclaim the same storage twice.
    ///
    /// Order is the other two engines' order and is load-bearing, because a user
    /// `release` prints: an array's elements go in index order, and a record's
    /// fields go in DECLARED order, which is not the order a `HashMap` yields.
    /// A scalar and a `String` declare nothing and are most of what a container
    /// holds, so the walk stops on them without asking the table at all.
    fn release_nested(&self, v: &Val) -> Result<(), Ctrl> {
        match v {
            Val::Array(xs) => {
                for x in xs.iter() {
                    self.release_nested(x)?;
                }
            }
            // A map's keys are Strings and declare nothing; its values are the
            // half that can.
            Val::Map(kv) => {
                for (_, x) in kv.iter() {
                    self.release_nested(x)?;
                }
            }
            Val::Option(Some(x)) => self.release_nested(x)?,
            Val::Result(_, x) => self.release_nested(x)?,
            Val::Enum(n, ps) => {
                if let Some(f) = self
                    .variant_enum
                    .get(n)
                    .and_then(|k| crate::types::owned_impl_by_key(self.impls, k))
                {
                    self.call(&f, std::slice::from_ref(v))?;
                    return Ok(());
                }
                for p in ps {
                    self.release_nested(p)?;
                }
            }
            // An unnamed record has no declaration to read a field order out of.
            // Its fields are left alone rather than released in an order the
            // other two engines would not use.
            Val::Record(fs, Some(n)) => {
                if let Some(f) = crate::types::owned_impl_by_key(self.impls, n) {
                    self.call(&f, std::slice::from_ref(v))?;
                    return Ok(());
                }
                let ty = Type::Named(n.to_string());
                for f in crate::types::record_fields(&ty, &self.type_map).unwrap_or_default() {
                    if let Some(x) = fs.get(&f.name) {
                        self.release_nested(x)?;
                    }
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// The move-out half of a place desugar (RFC-0082): TAKE the container out
    /// of its home rather than copying it, leaving `Val::Unit` behind until the
    /// write-back at the end of the same statement group restores it.
    ///
    /// `t.xs[k] = v` desugars to `let mut t.xs[] = t.xs` / `t.xs[][k] = v` /
    /// `t.xs = t.xs[]`. Reading the field the ordinary way CLONES the
    /// `Rc<Vec<Val>>` to refcount 2, so the `Rc::make_mut` in `Stmt::IndexSet`
    /// deep-copies the whole vector on every write: measured 23.9 s at
    /// N = 32,000 against 0.05 s for the same loop on a plain array variable —
    /// quadratic. Both compiled backends were always fine, because there the
    /// same three statements copy a `{ptr,len,cap}` header into an alloca and
    /// write through the *same* buffer, so this fix is interpreter-only.
    ///
    /// Sound only because the desugar hoists every operand that could read a
    /// place ahead of the move (`place_receiver` and its callers): before that,
    /// `t.xs[t.xs.length - 1] = 99` read the field while it was out and only
    /// got the right answer because the copy was still there. Once hoisted, the
    /// only thing that can happen between take and write-back is a trap.
    ///
    /// **Locals only, and that restriction is load-bearing.** M1 recorded that
    /// its equivalent hole was unobservable because every trap aborts, and that
    /// a recoverable trap would have to revisit it. Vyrn already has one:
    /// `vyrn test` catches a trapping test and runs the next, so a hole left in
    /// MODULE state outlives it, and a later test reads `Val::Unit` where an
    /// array should be — `at of non-Array/Int64`, a value no program can
    /// otherwise produce. A local cannot outlive that boundary: its frame is
    /// popped on the way out and never read again, and a `modify` parameter's
    /// write-back does not run on the error path either (checked: the caller's
    /// record is intact). So a global keeps the copy — and stays quadratic,
    /// like `store.xs.push(v)` already is.
    ///
    /// Returns `Ok(None)` for any shape it does not recognise, having mutated
    /// nothing, so the general path produces the usual value or the usual error.
    fn take_place(
        &self,
        name: &str,
        value: &Expr,
        scope: &mut Vec<Frame>,
    ) -> Result<Option<Val>, Ctrl> {
        if !is_place_temp(name) {
            return Ok(None);
        }
        // The two place shapes: a record field (`r.a`) and an array element
        // (`rows[i]`). Both bases are plain variables by construction — the
        // desugar is recursive, so an outer level has already moved its own
        // container into a temp.
        let (parent, taken): (&String, Box<dyn FnOnce(&mut Slot) -> Option<Val>>) = match value {
            Expr::Field { expr, field, .. } => {
                let Expr::Var { name: parent, .. } = expr.as_ref() else {
                    return Ok(None);
                };
                (
                    parent,
                    Box::new(move |s: &mut Slot| match &mut s.v {
                        Val::Record(map, _) => map
                            .get_mut(field)
                            .map(|slot| std::mem::replace(slot, Val::Unit)),
                        _ => None,
                    }),
                )
            }
            Expr::Call { name: f, args, .. } if f == crate::project::AT && args.len() == 2 => {
                let Expr::Var { name: parent, .. } = &args[0] else {
                    return Ok(None);
                };
                // The index is itself a hoisted temp, a literal or a variable,
                // so evaluating it here cannot reach the place being taken.
                let Val::Int(idx) = self.expr(&args[1], scope)? else {
                    return Ok(None);
                };
                (
                    parent,
                    Box::new(move |s: &mut Slot| match &mut s.v {
                        // Out of bounds falls through so `at`'s own wording traps.
                        Val::Array(items) if idx >= 0 && (idx as usize) < items.len() => {
                            let items = std::rc::Rc::make_mut(items);
                            Some(std::mem::replace(&mut items[idx as usize], Val::Unit))
                        }
                        _ => None,
                    }),
                )
            }
            _ => return Ok(None),
        };
        for frame in scope.iter_mut().rev() {
            if let Some(s) = frame.get_mut(parent) {
                return Ok(taken(s));
            }
        }
        Ok(None) // a global: not in any frame, so leave it to the copying path
    }

    /// The append half of an in-place `push` through a place (RFC-0082 M2),
    /// shared by the two receiver forms that need one: a record field
    /// (`t.xs.push(v)`, `Stmt::SetField`) and an array element
    /// (`rows[i].push(v)`, `Stmt::IndexSet`). `xs.push(v)` on a plain variable
    /// does not go through here — it owns its slot outright and appends into it
    /// directly, with no snapshot and nothing to drop.
    ///
    /// **This is not `take_place`'s take, and deliberately not.** That exists
    /// because the index-store desugar had already split the statement in three,
    /// so the container HAD to live in a temp and the move-out has to leave a
    /// `Val::Unit` hole behind it for the rest of the group. An append is ONE
    /// statement (`place = @push(place, v)`), so the array never has to leave its
    /// home and a snapshot does that job instead: the caller clones the place's
    /// `Rc` *before* this runs, this evaluates the item, and only then does
    /// `drop_place_ref` release the place's own reference — which is what puts
    /// the refcount back to 1 and makes the append O(1). Cloning first is what
    /// keeps the general path's evaluation order, and that is not academic:
    /// `t.xs.push(f(t))` with `f` taking `t: modify T` reaches the same
    /// container mid-statement, and its write is discarded by all three engines
    /// either way. Taking early would make that program trap on `push of
    /// non-Array Unit`; appending without the snapshot would make it print `2`
    /// where every engine prints `1`.
    ///
    /// **Locals only**, and the callers enforce it by looking in `scope` and
    /// nowhere else. The rule is `take_place`'s, reused rather than re-derived,
    /// and weaker here on purpose: the only escape between dropping the place's
    /// reference and storing the grown array back is an out-of-memory `reserve`,
    /// and a local's frame is the exact scope in which `vyrn test`'s recovery
    /// cannot observe a hole. A global keeps the copy and stays quadratic.
    ///
    /// `elem_ty` is the type of the ITEM, not of the container. Appending one
    /// element only requires that element to be validated, so the coercion runs
    /// on the item alone; the general path coerces the whole grown array and
    /// re-proves every element already proven, once per push.
    fn append_snapshot(
        &self,
        mut arr: std::rc::Rc<Vec<Val>>,
        item: &Expr,
        elem_ty: Option<&Type>,
        scope: &mut Vec<Frame>,
        drop_place_ref: impl FnOnce(&mut Vec<Frame>),
    ) -> Result<Val, Ctrl> {
        let item = self.expr(item, scope)?;
        // Validate before anything is disturbed: a trap here must leave the
        // container exactly as it was, which is what the backends do (they check
        // the pushed element before the store).
        let item = match elem_ty {
            Some(t) => self.coerce(item, t)?,
            None => item,
        };
        // What the place holds now is discarded either way: unchanged it is the
        // snapshot, and changed it is a mid-statement write the general path
        // also overwrites.
        drop_place_ref(scope);
        let elems = std::rc::Rc::make_mut(&mut arr);
        reserve_vec(elems, 1)?;
        elems.push(item);
        Ok(Val::Array(arr))
    }

    fn stmt(&self, stmt: &Stmt, scope: &mut Vec<Frame>) -> Result<Flow, Ctrl> {
        // Generation step budget (RFC-0021): a runaway generator fails loudly
        // instead of hanging the build. Only active inside a generation run.
        if let Some(g) = &self.gen {
            let fuel = g.fuel.get();
            if fuel == 0 {
                return Err(Ctrl::Err("generator exceeded its step budget".into()));
            }
            g.fuel.set(fuel - 1);
        }
        match stmt {
            Stmt::Let {
                name, value, ty, ..
            } => {
                let mut v = match self.take_place(name, value, scope)? {
                    Some(taken) => taken,
                    None => self.expr(value, scope)?,
                };
                // An annotation coerces the initializer (sized-int wrapping,
                // automatic validation) and is remembered so reassignments run
                // through the same coercion.
                if let Some(t) = ty {
                    v = self.coerce(v, t)?;
                }
                // Remember the binding's type so a later `toJson(x)` can encode
                // record fields in declaration order (RFC-0018). An explicit
                // annotation wins; otherwise infer the initializer's type. This
                // fills a previously-`None` slot only — it never overrides an
                // annotation, so reassignment coercion is unaffected for the
                // annotated case, and the inferred type is idempotent-safe
                // (records were already validated at construction).
                let slot_ty = match ty {
                    Some(t) => Some(t.clone()),
                    None => self.type_of(value, scope),
                };
                scope
                    .last_mut()
                    .unwrap()
                    .insert(name.clone(), Slot { v, ty: slot_ty });
                Ok(Flow::Normal)
            }
            Stmt::Assign { name, value, .. } => {
                // `xs.push(v)` on a local Array: append IN PLACE.
                //
                // `push` clones the whole array to return a grown copy, and the
                // statement form desugars to `xs = @push(xs, v)`, so building an
                // array is quadratic: 3,000 pushes measured 251 ms against
                // 19,270 ms for 30,000. std/vyx builds its output as
                // `Array<UInt8>` buffers, which is what makes compiling one .vyx
                // page cost seconds.
                //
                // When the binding is DECLARED (`let mut out: Array<UInt8> = []`)
                // the general path also re-coerces the whole array on every push,
                // re-validating every element already proven valid. Appending one
                // element only requires that element to be checked, so the item is
                // coerced to the element type and the rest left alone — the same
                // guarantee without the rescan.
                //
                // The slot is inspected BEFORE the item is evaluated, so a shape
                // this cannot handle falls through to the general path having
                // evaluated nothing. Safe for a local: no callee can reach it, so
                // evaluating the item cannot change what was just inspected.
                if let Expr::Call {
                    name: fname, args, ..
                } = value
                {
                    if fname == "@push"
                        && args.len() == 2
                        && matches!(&args[0], Expr::Var { name: n, .. } if n == name)
                    {
                        let shape = scope.iter().rev().find_map(|f| f.get(name)).and_then(|s| {
                            if !matches!(s.v, Val::Array(_)) {
                                return None;
                            }
                            match &s.ty {
                                None => Some(None),
                                Some(t) => match crate::types::resolve(t, &self.type_map) {
                                    Type::Array(i) => Some(Some(*i)),
                                    // ArrayN/SmallArray carry a capacity the
                                    // general path enforces; leave those alone.
                                    _ => None,
                                },
                            }
                        });
                        if let Some(elem_ty) = shape {
                            let item = self.expr(&args[1], scope)?;
                            let item = match &elem_ty {
                                Some(t) => self.coerce(item, t)?,
                                None => item,
                            };
                            for frame in scope.iter_mut().rev() {
                                if let Some(slot) = frame.get_mut(name) {
                                    if let Val::Array(elems) = &mut slot.v {
                                        let elems = std::rc::Rc::make_mut(elems);
                                        reserve_vec(elems, 1)?;
                                        elems.push(item);
                                        return Ok(Flow::Normal);
                                    }
                                    break;
                                }
                            }
                        }
                    }
                }
                // `s = s + a + b + …` on an untyped local String: append IN PLACE.
                //
                // String `+` allocates a fresh String and copies both operands,
                // so accumulating in a loop is quadratic in the result — 40,000
                // appends measured 8.9 s against 1.1 s for 20,000. Every
                // generator builds its output this way (`out = out + …` appears
                // 56 times in std/vyx and 98 in std/ui).
                //
                // The chain matters: `out + a + ", "` parses as
                // `Add(Add(Var(out), a), ", ")`, so the target sits at the far
                // end of the left spine, not directly under the top `+`. Walking
                // the spine is what makes this fire on real code rather than only
                // on the single-append shape.
                //
                // Three guards keep it unobservable. The binding must be a LOCAL,
                // so nothing evaluated on the right can reach it and no alias
                // exists. It must have no declared type, so the coercion — and
                // the automatic validation riding on it — that the general path
                // performs is not being skipped. And the spine must bottom out in
                // a plain `Var` naming the assignment target, whose old value is
                // dead the instant this statement completes. Operands are
                // evaluated left to right, exactly as the general path would.
                if let Expr::Binary { op: BinOp::Add, .. } = value {
                    let mut spine: Vec<&Expr> = Vec::new();
                    let mut cur = value;
                    while let Expr::Binary {
                        op: BinOp::Add,
                        lhs,
                        rhs,
                        ..
                    } = cur
                    {
                        spine.push(rhs);
                        cur = lhs;
                    }
                    let rooted = matches!(cur, Expr::Var { name: n, .. } if n == name);
                    let appendable = rooted
                        && scope
                            .iter()
                            .rev()
                            .find_map(|f| f.get(name))
                            .is_some_and(|s| s.ty.is_none() && matches!(s.v, Val::Str(_)));
                    if appendable {
                        spine.reverse();
                        let mut parts: Vec<String> = Vec::with_capacity(spine.len());
                        for e in spine {
                            match self.expr(e, scope)? {
                                // The copy is fallible for the same reason the
                                // append below is: in `s = s + s` the operand IS
                                // the accumulator, so this is the larger of the
                                // two allocations for half the loop.
                                Val::Str(t) => {
                                    let mut c = String::new();
                                    reserve_str(&mut c, t.len())?;
                                    c.push_str(&t);
                                    parts.push(c);
                                }
                                // Not a String after all — rebuild through the
                                // general path rather than guess.
                                _ => {
                                    parts.clear();
                                    break;
                                }
                            }
                        }
                        if !parts.is_empty() {
                            for frame in scope.iter_mut().rev() {
                                if let Some(slot) = frame.get_mut(name) {
                                    if let Val::Str(head) = &mut slot.v {
                                        // Copy-on-write: grows in place while the
                                        // accumulator is unshared, which is the
                                        // whole point of `Rc<String>` over `Rc<str>`.
                                        let head = std::rc::Rc::make_mut(head);
                                        for t in &parts {
                                            reserve_str(head, t.len())?;
                                            head.push_str(t);
                                        }
                                        return Ok(Flow::Normal);
                                    }
                                }
                            }
                        }
                    }
                }
                let v = self.expr(value, scope)?;
                // Reassignment flows through the binding's declared type — the
                // same coercion (and automatic validation) as the original let.
                let declared = scope
                    .iter()
                    .rev()
                    .find_map(|f| f.get(name).and_then(|s| s.ty.clone()))
                    .or_else(|| self.globals.borrow().get(name).and_then(|s| s.ty.clone()));
                let v = match &declared {
                    Some(t) => self.coerce(v, t)?,
                    None => v,
                };
                for frame in scope.iter_mut().rev() {
                    if let Some(slot) = frame.get_mut(name) {
                        slot.v = v;
                        return Ok(Flow::Normal);
                    }
                }
                // Fall back to module state (RFC-0013): a `mut` global write.
                if let Some(slot) = self.globals.borrow_mut().get_mut(name) {
                    slot.v = v;
                    return Ok(Flow::Normal);
                }
                Err(format!("assignment to unbound variable `{name}`").into())
            }
            Stmt::SetField {
                name, field, value, ..
            } => {
                // `t.xs.push(v)` on a LOCAL record: append IN PLACE via
                // `append_snapshot`, which carries the mechanism and its rules —
                // the same fix as `xs.push(v)` above, one level down.
                //
                // The statement desugars to `t.xs = @push(t.xs, v)`, so the
                // general path evaluates `t.xs` into a second `Rc` while the
                // field still holds the first, and `push`'s `Rc::make_mut` then
                // copies the whole vector: measured 135 / 310 / 1,705 / 10,704 ms
                // at N = 4,000 → 32,000 against a flat 45 / 44 / 49 / 49 for the
                // same loop on a plain local. Quadratic, on the most common
                // container operation there is. Both compiled backends were
                // always flat (~44 native, ~50 wasm at every N), so this is
                // interpreter-only like the take below it.
                //
                // The slot is inspected BEFORE the item is evaluated, so a shape
                // this cannot handle falls through having evaluated nothing.
                //
                // The field's declared type is what the value is coerced — and
                // therefore VALIDATED — into, on both paths. This statement had
                // no coercion at all until RFC-0082 M3: `t.xs.push(n)` on an
                // `xs: Array<Age>` accepted a runtime 5 under the interpreter
                // while both compiled backends trapped, because they coerce at
                // this store (`emit_validation` inside codegen's own `coerce`)
                // and nothing here did. `None` when the record binding's type is
                // unknown — there is then nothing to check against.
                let fty = self.field_ty(name, field, scope);
                let mut pushed = None;
                if let Expr::Call {
                    name: fname, args, ..
                } = value
                {
                    if fname == "@push"
                        && args.len() == 2
                        && matches!(&args[0], Expr::Field { expr, field: f, .. }
                            if f == field
                                && matches!(expr.as_ref(), Expr::Var { name: n, .. } if n == name))
                    {
                        // Appending one element only requires THAT element to be
                        // validated, so the fast path coerces the item and leaves
                        // the rest alone — the general path's whole-array coerce
                        // would re-prove every element on every push. A fixed or
                        // small array carries a capacity only the general path
                        // enforces, so those fall through, exactly as the local
                        // `xs.push(v)` path above bails on them.
                        let elem_ty = match &fty {
                            None => Some(None),
                            Some(t) => match crate::types::resolve(t, &self.type_map) {
                                Type::Array(i) => Some(Some(*i)),
                                _ => None,
                            },
                        };
                        let snap =
                            elem_ty.and_then(|e| {
                                scope.iter().rev().find_map(|fr| fr.get(name)).and_then(
                                    |s| match &s.v {
                                        Val::Record(map, _) => match map.get(field) {
                                            Some(Val::Array(elems)) => Some((elems.clone(), e)),
                                            _ => None,
                                        },
                                        _ => None,
                                    },
                                )
                            });
                        if let Some((arr, elem_ty)) = snap {
                            pushed = Some(self.append_snapshot(
                                arr,
                                &args[1],
                                elem_ty.as_ref(),
                                scope,
                                |scope| {
                                    for fr in scope.iter_mut().rev() {
                                        let Some(slot) = fr.get_mut(name) else {
                                            continue;
                                        };
                                        if let Val::Record(map, _) = &mut slot.v {
                                            if let Some(cur) = map.get_mut(field) {
                                                *cur = Val::Unit;
                                            }
                                        }
                                        break;
                                    }
                                },
                            )?);
                        }
                    }
                }
                // The write-back is the ordinary one: the fast path only decided
                // what value the field gets (and already coerced its element).
                let v = match pushed {
                    Some(v) => v,
                    None => {
                        let v = self.expr(value, scope)?;
                        // A plain variable ALREADY of the field's type holds
                        // values that passed their own boundary, so re-proving
                        // them is pure cost — and it is the write-back every
                        // place desugar ends with (`t.xs[i] = v` becomes
                        // `.. t.xs = t.xs[]`), so it lands once per store:
                        // 8,000 writes into an `Array<Age>` field measured
                        // 13,467 ms re-validating against 76 ms not. This is
                        // the compiled backends' own rule, not a shortcut of
                        // the interpreter's — `validation_required` returns
                        // `None` when `from == to`, so they emit nothing here
                        // either. Deliberately variables only: `@push(t.xs, v)`
                        // is ALSO statically `Array<Age>` and its element has
                        // been validated by nothing at this point (the
                        // backends validate it inside `push`; the fast path
                        // above does, for a local).
                        let known = matches!(value, Expr::Var { name: n, .. }
                            if scope.iter().rev().find_map(|f| f.get(n).map(|s| s.ty.clone()))
                                .unwrap_or_else(|| self.globals.borrow().get(n)
                                    .and_then(|s| s.ty.clone()))
                                .as_ref() == fty.as_ref());
                        match &fty {
                            Some(t) if !known => self.coerce(v, t)?,
                            _ => v,
                        }
                    }
                };
                for frame in scope.iter_mut().rev() {
                    if let Some(Slot {
                        v: Val::Record(map, _),
                        ..
                    }) = frame.get_mut(name)
                    {
                        map.insert(field.clone(), v);
                        return Ok(Flow::Normal);
                    }
                }
                if let Some(Slot {
                    v: Val::Record(map, _),
                    ..
                }) = self.globals.borrow_mut().get_mut(name)
                {
                    map.insert(field.clone(), v);
                    return Ok(Flow::Normal);
                }
                Err(format!("field assignment to unbound record `{name}`").into())
            }
            // `name[index] = value` — in-place element store (RFC-0011). The
            // value coerces into the declared element type (sized-int wrapping,
            // automatic validation), then is written through the shared buffer;
            // an out-of-bounds index traps with the read path's wording.
            Stmt::IndexSet {
                name,
                index,
                value,
                line,
            } => {
                // RFC-0091 M3: a user container declares where its element is,
                // and `place atSet` is the writing half. Asked before the index
                // is evaluated, because the projection's prologue runs first.
                if let Some(stmts) = self.project_store(name, index, value, scope, *line)? {
                    return self.block(&Block { stmts }, scope);
                }
                let iv = self.expr(index, scope)?;
                // `m[k] = v` on a Map (RFC-0028) — insert or update in place.
                // An existing key keeps its slot (order preserved); a new key is
                // appended. The value coerces into `V` (auto-validation included).
                if let Val::Str(k) = &iv {
                    let val_of = |s: &Slot| match &s.ty {
                        Some(Type::Map(_, v)) => Some((**v).clone()),
                        _ => None,
                    };
                    let is_map = scope
                        .iter()
                        .rev()
                        .find_map(|f| f.get(name).map(|s| matches!(s.v, Val::Map(_))))
                        .or_else(|| {
                            self.globals
                                .borrow()
                                .get(name)
                                .map(|s| matches!(s.v, Val::Map(_)))
                        })
                        .unwrap_or(false);
                    if is_map {
                        let k = k.clone();
                        let val_ty = scope
                            .iter()
                            .rev()
                            .find_map(|f| f.get(name).and_then(val_of))
                            .or_else(|| self.globals.borrow().get(name).and_then(val_of));
                        let mut v = self.expr(value, scope)?;
                        if let Some(t) = &val_ty {
                            v = self.coerce(v, t)?;
                        }
                        for frame in scope.iter_mut().rev() {
                            if let Some(Slot {
                                v: Val::Map(pairs), ..
                            }) = frame.get_mut(name)
                            {
                                pairs.insert((*k).clone(), v);
                                return Ok(Flow::Normal);
                            }
                        }
                        if let Some(Slot {
                            v: Val::Map(pairs), ..
                        }) = self.globals.borrow_mut().get_mut(name)
                        {
                            pairs.insert((*k).clone(), v);
                            return Ok(Flow::Normal);
                        }
                        return Err(format!("index-assignment to unbound map `{name}`").into());
                    }
                }
                let idx = match iv {
                    Val::Int(n) => n,
                    other => {
                        return Err(format!("array index must be an Int64, found {other:?}").into())
                    }
                };
                // Coerce into the element type of the array binding's declared
                // type (validated element types validate here, exactly like a
                // `push` argument or an annotated `let`). Resolved before the
                // value runs because the append below needs it too; a binding's
                // declared type cannot change under evaluation.
                let elem_of = |s: &Slot| match &s.ty {
                    Some(Type::Array(t))
                    | Some(Type::ArrayN(t, _))
                    | Some(Type::SmallArray(t, _)) => Some((**t).clone()),
                    _ => None,
                };
                let elem_ty = scope
                    .iter()
                    .rev()
                    .find_map(|f| f.get(name).and_then(elem_of))
                    .or_else(|| self.globals.borrow().get(name).and_then(elem_of));
                // `rows[i].push(v)` on a LOCAL array of arrays: append IN PLACE.
                // The third and last receiver form, and the SAME snapshot as
                // `t.xs.push(v)` rather than a third copy of it — because the
                // shape is the same shape. The parser emits one `Stmt::IndexSet`
                // for this statement (`rows[i] = @push(rows[i], v)`), exactly as
                // it emits one `Stmt::SetField` for the field form; nothing is
                // split into a temp, so nothing has to be taken. It was the last
                // quadratic left: 211 / 401 / 1,744 / 9,420 ms at
                // N = 4,000 → 32,000 against a flat 49 / 47 / 58 / 55 for the
                // same appends on a plain local, both compiled backends flat
                // throughout. `at` clones the row's `Rc` to refcount 2 and
                // `push`'s `Rc::make_mut` copies the whole row per append.
                //
                // The receiver's index is the parser's CLONE of this statement's
                // own, so the general path reads it a second time and
                // `rows[f()].push(g())` calls `f` twice — on all three engines,
                // checked. Not this milestone's to change (finding 4 hoisted the
                // equivalent double read out of `rows[f()][j] = v`, and the same
                // hoist here would move IR in both backends), so the fast path
                // only fires when re-reading the index is unobservable: a
                // variable or a literal, which is what a loop writes. Anything
                // else keeps the copy and stays quadratic, like a global does.
                let mut pushed = None;
                if let (
                    Expr::Call {
                        name: fname, args, ..
                    },
                    true,
                ) = (value, matches!(index, Expr::Var { .. } | Expr::Int(_)))
                {
                    if fname == "@push" && args.len() == 2 {
                        if let Expr::Call {
                            name: at,
                            args: iargs,
                            ..
                        } = &args[0]
                        {
                            if at == "@at"
                                && iargs.len() == 2
                                && matches!(&iargs[0], Expr::Var { name: n, .. } if n == name)
                                && matches!(&iargs[1], Expr::Var { .. } | Expr::Int(_))
                                // Re-read rather than assume it is the same
                                // expression: a `Var` lookup is free and this is
                                // what says the row grown is the row stored back.
                                && matches!(self.expr(&iargs[1], scope)?, Val::Int(n) if n == idx)
                            {
                                // The item's type is one level in from the outer
                                // array's element type. A fixed or small row
                                // carries a capacity only the general path
                                // enforces, so those fall through, exactly as
                                // the two sibling fast paths bail on them.
                                let item_ty = match &elem_ty {
                                    None => Some(None),
                                    Some(t) => match crate::types::resolve(t, &self.type_map) {
                                        Type::Array(i) => Some(Some(*i)),
                                        _ => None,
                                    },
                                };
                                // Inspected BEFORE the item is evaluated, so a
                                // shape this cannot handle — an out-of-bounds
                                // index, a global — falls through to the general
                                // path having evaluated nothing, and traps with
                                // `at`'s own wording.
                                let snap =
                                    item_ty.and_then(|e| {
                                        scope.iter().rev().find_map(|fr| fr.get(name)).and_then(
                                            |s| match &s.v {
                                                Val::Array(rows) if idx >= 0 => {
                                                    match rows.get(idx as usize) {
                                                        Some(Val::Array(elems)) => {
                                                            Some((elems.clone(), e))
                                                        }
                                                        _ => None,
                                                    }
                                                }
                                                _ => None,
                                            },
                                        )
                                    });
                                if let Some((arr, item_ty)) = snap {
                                    pushed = Some(self.append_snapshot(
                                        arr,
                                        &args[1],
                                        item_ty.as_ref(),
                                        scope,
                                        |scope| {
                                            for fr in scope.iter_mut().rev() {
                                                let Some(slot) = fr.get_mut(name) else {
                                                    continue;
                                                };
                                                if let Val::Array(rows) = &mut slot.v {
                                                    std::rc::Rc::make_mut(rows)[idx as usize] =
                                                        Val::Unit;
                                                }
                                                break;
                                            }
                                        },
                                    )?);
                                }
                            }
                        }
                    }
                }
                // The write-back is the ordinary one: the fast path only decided
                // what value the element gets (and already coerced its item).
                let v = match pushed {
                    Some(v) => v,
                    None => {
                        let v = self.expr(value, scope)?;
                        match &elem_ty {
                            Some(t) => self.coerce(v, t)?,
                            None => v,
                        }
                    }
                };
                for frame in scope.iter_mut().rev() {
                    if let Some(Slot {
                        v: Val::Array(items),
                        ..
                    }) = frame.get_mut(name)
                    {
                        if idx < 0 || idx as usize >= items.len() {
                            return Err(crate::trap::array_index(idx).into());
                        }
                        std::rc::Rc::make_mut(items)[idx as usize] = v;
                        return Ok(Flow::Normal);
                    }
                }
                if let Some(Slot {
                    v: Val::Array(items),
                    ..
                }) = self.globals.borrow_mut().get_mut(name)
                {
                    if idx < 0 || idx as usize >= items.len() {
                        return Err(crate::trap::array_index(idx).into());
                    }
                    std::rc::Rc::make_mut(items)[idx as usize] = v;
                    return Ok(Flow::Normal);
                }
                Err(format!("index-assignment to unbound array `{name}`").into())
            }
            Stmt::Return { value, .. } => {
                let v = match value {
                    Some(e) => self.expr(e, scope)?,
                    None => Val::Unit,
                };
                // After the value, because the value may hold a `?` whose own
                // exit is the one the frames below are then paying for.
                leaving(crate::own::trace::Exit::Return, stmt);
                Ok(Flow::Return(v))
            }
            Stmt::IfLet {
                pattern,
                scrutinee,
                then_block,
                else_block,
                ..
            } => {
                // Evaluate the scrutinee ONCE (no double-eval), test the pattern,
                // and run the matching arm with the binders in scope (RFC-0060).
                let sv = self.expr(scrutinee, scope)?;
                let flow = match Self::match_pattern(pattern, &sv) {
                    Some(binds) => {
                        scope.push(Frame::default());
                        for (n, v) in binds {
                            scope.last_mut().unwrap().insert(n, Slot::untyped(v));
                        }
                        let flow = self.block(then_block, scope);
                        scope.pop();
                        flow
                    }
                    None => match else_block {
                        Some(eb) => self.block(eb, scope),
                        None => Ok(Flow::Normal),
                    },
                };
                let unwound = !matches!(flow, Ok(Flow::Normal));
                self.release_temp(stmt as *const Stmt as usize, &sv, unwound, flow)
            }
            Stmt::If {
                cond,
                then_block,
                else_block,
                ..
            } => {
                if self.as_bool(self.expr(cond, scope)?)? {
                    self.block(then_block, scope)
                } else if let Some(eb) = else_block {
                    self.block(eb, scope)
                } else {
                    Ok(Flow::Normal)
                }
            }
            Stmt::Break { .. } => {
                leaving(crate::own::trace::Exit::Break, stmt);
                Ok(Flow::Break)
            }
            Stmt::Continue { .. } => {
                leaving(crate::own::trace::Exit::Continue, stmt);
                Ok(Flow::Continue)
            }
            Stmt::While { cond, body, .. } => {
                while self.as_bool(self.expr(cond, scope)?)? {
                    match self.block(body, scope)? {
                        Flow::Return(v) => return Ok(Flow::Return(v)),
                        Flow::Break => break,
                        // `continue` re-tests the condition; `Normal` falls
                        // through to the same place.
                        Flow::Continue | Flow::Normal => {}
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::ForIn {
                var,
                iter,
                body,
                line,
                ..
            } => {
                // RFC-0091 M3: a user container declares how it is iterated, and
                // the loop is written in its own terms — `size` for how many, the
                // `place nth` projection for where each element is. Asked before
                // the iterable is evaluated, because naming a receiver twice is
                // what the read path already refuses to do.
                if let Some((size_fn, nth)) = self
                    .index_receiver_key(iter, scope)
                    .and_then(|k| crate::types::iterate_impl_by_key(self.impls, &k))
                {
                    let blk = crate::project::iterate_loop(&size_fn, nth, var, iter, body, *line)
                        .map_err(Ctrl::Err)?;
                    return self.block(&blk, scope);
                }
                // RFC-0075 M2b: a stream is pulled, not walked. The producer runs
                // once per iteration and the loop OWNS the stream, so leaving by
                // any route — falling off the end, `break`, `return` — releases it
                // on the way out, which is the same guarantee M1 gave a buffer.
                let iv = self.expr(iter, scope)?;
                let items = match iv.clone() {
                    Val::Stream(s) => return self.for_stream(*s, var, body, scope),
                    Val::Array(items) => items,
                    // Iterating a String yields each byte as an Int.
                    Val::Str(s) => s
                        .as_bytes()
                        .iter()
                        .map(|b| Val::Int(*b as i64))
                        .collect::<Vec<_>>()
                        .into(),
                    other => return Err(format!("`for` expected an array, found {other:?}").into()),
                };
                // Every way out of the loop, in one value, so the iterable's own
                // release below runs on all of them — which is what both
                // compiled backends buy by putting it on a release frame BELOW
                // the loop's boundary, where `break` cannot reach it and the
                // whole-function walk can.
                let mut out = Ok(Flow::Normal);
                for item in items.iter() {
                    // Fresh frame per iteration holding the loop variable; the
                    // body's own inner frame nests inside it.
                    scope.push(Frame::default());
                    scope
                        .last_mut()
                        .unwrap()
                        .insert(var.clone(), Slot::untyped(item.clone()));
                    let flow = self.block(body, scope);
                    scope.pop();
                    match flow {
                        Ok(Flow::Return(v)) => {
                            out = Ok(Flow::Return(v));
                            break;
                        }
                        Ok(Flow::Break) => break,
                        Ok(Flow::Continue | Flow::Normal) => {}
                        Err(e) => {
                            out = Err(e);
                            break;
                        }
                    }
                }
                // A `break` was CAUGHT here, so the statement is falling through
                // and the snapshot's release is its own exit; a `return` or a
                // propagating `?` is still leaving.
                let unwound = !matches!(out, Ok(Flow::Normal));
                self.release_temp(stmt as *const Stmt as usize, &iv, unwound, out)
            }
            Stmt::Drop { name, .. } => {
                // A reference is released — its slot's generation bumps, so any
                // later (illegally aliased) use traps, matching the native
                // backend. Strings and arrays are reclaimed by the host, which is
                // not observable, so dropping them has no runtime effect here.
                let v = scope
                    .iter()
                    .rev()
                    .find_map(|f| f.get(name))
                    .map(|s| s.v.clone())
                    .or_else(|| self.globals.borrow().get(name).map(|s| s.v.clone()));
                // A DECLARED release is ordinary Vyrn and may print, so it is
                // the one reclamation this engine cannot leave to the host —
                // exactly the reason `run_drops` runs it on the automatic path.
                // Both compiling backends already lowered `drop x` through the
                // same table (`release_kind` / `rel_for`); this arm was the
                // engine that did not, and it went unseen while the checker
                // refused `drop` on every type that could declare one.
                if let Some(v) = v {
                    match self
                        .val_type_key(&v)
                        .and_then(|k| crate::types::owned_impl_by_key(self.impls, &k))
                    {
                        Some(f) => {
                            self.call(&f, std::slice::from_ref(&v))?;
                        }
                        // RFC-0092 M4: `drop pool` releases what the pool holds,
                        // which is what makes the obligation the pool now carries
                        // a discharge rather than a demand.
                        None => self.release_nested(&v)?,
                    }
                }
                Ok(Flow::Normal)
            }
            Stmt::Expr(e) => {
                self.expr(e, scope)?;
                Ok(Flow::Normal)
            }
            // A `region` is semantically transparent to the reference
            // interpreter — it runs its body in a fresh scope and the host
            // reclaims memory. Deterministic freeing is observable only in the
            // native backend; the two agree on output and exit code.
            Stmt::Region { body, .. } => {
                // Match the native arena runtime's fixed region stack: entering
                // one past [`REGION_MAX`] traps there, so trap here with the same
                // message (interp == native, incl. traps).
                if self.region_depth.get() >= REGION_MAX as usize {
                    return Err(crate::trap::region_depth().into());
                }
                self.region_depth.set(self.region_depth.get() + 1);
                let r = self.block(body, scope);
                self.region_depth.set(self.region_depth.get() - 1);
                r
            }
        }
    }

    /// `a[i]` where `a` is a container of the user's own (RFC-0091 M2): inline
    /// the `place at` projection and read the place it yields.
    ///
    /// `Ok(None)` means "not a projection" — a builtin container, or a receiver
    /// whose type is not known here — and the caller keeps the builtin path. A
    /// builtin container deliberately does NOT come through here: the
    /// interpreter has no lowering to re-express, so `at` stays its own
    /// element-place primitive and only the two compiling backends carry the
    /// `@slot` spelling the dogfood proof introduced.
    /// The type key of an index receiver: its static type where one is known,
    /// else the name a record value carries. Both, because a `let` of a record
    /// literal may have no annotation to read and a record is otherwise
    /// anonymous.
    fn index_receiver_key(&self, recv: &Expr, scope: &mut Vec<Frame>) -> Option<String> {
        if let Some(ty) = self.type_of(recv, scope) {
            if let Some(k) = crate::types::type_key(&ty) {
                return Some(k);
            }
        }
        // Reading a plain binding has no side effect, so peeking is free. Any
        // other receiver shape would have to be evaluated, and evaluating it
        // twice is worse than not dispatching.
        let Expr::Var { name, .. } = recv else {
            return None;
        };
        let v = scope.iter().rev().find_map(|f| f.get(name))?;
        self.val_type_key(&v.v)
    }

    /// `a[i] = v` where `a` is a container of the user's own (RFC-0091 M3):
    /// inline the `place atSet` projection and hand back the statements that
    /// write to the place it yields.
    ///
    /// `Ok(None)` means "not a projection" — a builtin container, or a receiver
    /// whose type is not known here — and the caller keeps the builtin path.
    fn project_store(
        &self,
        name: &str,
        index: &Expr,
        value: &Expr,
        scope: &mut Vec<Frame>,
        line: usize,
    ) -> Result<Option<Vec<Stmt>>, Ctrl> {
        let recv = Expr::Var {
            name: name.to_string(),
            line,
        };
        let Some(key) = self.index_receiver_key(&recv, scope) else {
            return Ok(None);
        };
        let Some(f) = crate::project::lookup_by_key(self.impls, &key, "atSet") else {
            return Ok(None);
        };
        let p = crate::project::inline(f, &recv, std::slice::from_ref(index), line)
            .map_err(Ctrl::Err)?;
        let Some(store) = crate::project::store_stmts(&p.place, value, line) else {
            return Err(Ctrl::Err(format!(
                "line {line}: `{name}[..] = v` goes through a `place atSet` that yields \
                 something with no address — a call result or a temporary. A projection \
                 yields a place: a binding, a field of one, or an element of one"
            )));
        };
        let mut out = p.prologue;
        out.extend(store);
        Ok(Some(out))
    }

    fn project_read(
        &self,
        args: &[Expr],
        scope: &mut Vec<Frame>,
        line: usize,
    ) -> Result<Option<Val>, Ctrl> {
        let Some(key) = self.index_receiver_key(&args[0], scope) else {
            return Ok(None);
        };
        let Some(f) = crate::project::lookup_by_key(self.impls, &key, "at") else {
            return Ok(None);
        };
        let p = crate::project::inline(f, &args[0], &args[1..], line).map_err(Ctrl::Err)?;
        scope.push(Frame::default());
        let out = (|| -> Result<Val, Ctrl> {
            for s in &p.prologue {
                self.stmt(s, scope)?;
            }
            self.expr(&p.place, scope)
        })();
        scope.pop();
        out.map(Some)
    }

    fn expr(&self, expr: &Expr, scope: &mut Vec<Frame>) -> Result<Val, Ctrl> {
        match expr {
            // RFC-0093: a take reads the place. A `Val` is `Rc`-shared, so the
            // take clones the handle and the record keeps a word nothing reads
            // again — movecheck has already proved that. Nothing is released, so
            // nothing is observable, which is why parity expects byte-identical
            // output.
            Expr::Consume { place, .. } => self.expr(place, scope),
            Expr::Int(n) => Ok(Val::Int(*n)),
            // A byte literal (RFC-0057) is an integer value at runtime — the
            // checker has already given it its `UInt8`/coerced type; the raw
            // value flows through as a plain `Int` exactly like `Expr::Int`.
            Expr::Byte(b) => Ok(Val::Int(*b as i64)),
            Expr::Float(x) => Ok(Val::Float(*x)),
            Expr::Bool(b) => Ok(Val::Bool(*b)),
            Expr::Str(s) => Ok(Val::Str(std::rc::Rc::new(s.clone()))),
            // A lambda literal: a `fn`-typed call argument (RFC-0023) or a
            // storage-position source (RFC-0037). Captures snapshot HERE (the
            // evaluation site — the capture-timing lock); the parameter/return
            // types are left blank and adopted from the declared `fn(..) -> R`
            // slot type by `coerce` at the storage boundary (every storage path
            // — `let`, assign, push-rebind, field/element store — coerces).
            Expr::Lambda { params, body, .. } => {
                Ok(self.make_closure(params, body, scope, Vec::new(), Type::Unit))
            }
            Expr::Var { name, .. } => {
                // `None` is the empty-Option constructor, not a variable.
                if name == "None" {
                    return Ok(Val::Option(None));
                }
                // A nullary enum variant, e.g. `Empty`.
                if self.variants.contains(name.as_str()) {
                    return Ok(Val::Enum(name.clone(), Vec::new()));
                }
                for frame in scope.iter().rev() {
                    if let Some(slot) = frame.get(name) {
                        return Ok(slot.v.clone());
                    }
                }
                // Fall back to module state (RFC-0013).
                if let Some(slot) = self.globals.borrow().get(name) {
                    return Ok(slot.v.clone());
                }
                // A bare function name in a value position (RFC-0037): a stored
                // function value with an empty capture set.
                if self.funcs.contains_key(name.as_str()) {
                    return Ok(Val::Fn(Box::new(FnVal::Named(name.clone()))));
                }
                Err(format!("unbound variable `{name}`").into())
            }
            Expr::Unary { op, expr, .. } => {
                let v = self.expr(expr, scope)?;
                match (op, v) {
                    // wrapping: -i64::MIN has no representation; two's complement
                    // keeps it MIN, exactly as native `sub i64 0, %n` does.
                    (UnOp::Neg, Val::Int(n)) => Ok(Val::Int(n.wrapping_neg())),
                    (UnOp::Neg, Val::IntN { v, bits, signed }) => Ok(Val::IntN {
                        v: wrap_intn(v.wrapping_neg(), bits, signed),
                        bits,
                        signed,
                    }),
                    (UnOp::Neg, Val::Float(x)) => Ok(Val::Float(-x)),
                    (UnOp::Neg, Val::Float32(x)) => Ok(Val::Float32(-x)),
                    // `-v` flips each lane's sign bit (RFC-0083 M2). Rust's unary
                    // `-` on `f32` is IEEE `negate`, which is the bit flip and not
                    // `0.0 - x` — the difference shows at `-0.0` and is why this
                    // exists rather than leaving `F32x4.splat(0.0) - v` as the
                    // spelling.
                    (UnOp::Neg, Val::F32x4(v)) => Ok(Val::F32x4(v.map(|x| -x))),
                    (UnOp::Neg, Val::F64x2(v)) => Ok(Val::F64x2(v.map(|x| -x))),
                    // The integer negation WRAPS, exactly as the scalar one above
                    // does and as `i32x4.sub` from zero does: `-Int32.min` is
                    // `Int32.min`. Bare `-` would panic in a debug build.
                    (UnOp::Neg, Val::I32x4(v)) => Ok(Val::I32x4(v.map(|x| x.wrapping_neg()))),
                    (UnOp::Not, Val::Bool(b)) => Ok(Val::Bool(!b)),
                    // `~m` complements a mask lane-wise (RFC-0083 M2) — `~` and not
                    // `!` because `!` is the Bool operator and a mask is four
                    // answers, the same separation `&`/`&&` keeps in `binop`.
                    (UnOp::BitNot, Val::Mask32x4(m)) => Ok(Val::Mask32x4(m.map(|b| !b))),
                    (UnOp::BitNot, Val::Mask64x2(m)) => Ok(Val::Mask64x2(m.map(|b| !b))),
                    // An integer vector complements its lanes directly — `v128.not`
                    // has no lane width, so this is `xor` against all-ones either
                    // way, and the mask reaches it through the same instruction.
                    (UnOp::BitNot, Val::I32x4(v)) => Ok(Val::I32x4(v.map(|x| !x))),
                    // `~n` complements within the operand's width (RFC-0045):
                    // the literal `Int` at 64 bits, a sized integer at its own
                    // width (re-wrapped so an unsigned complement stays in range).
                    (UnOp::BitNot, Val::Int(n)) => Ok(Val::Int(!n)),
                    (UnOp::BitNot, Val::IntN { v, bits, signed }) => Ok(Val::IntN {
                        v: wrap_intn(!v, bits, signed),
                        bits,
                        signed,
                    }),
                    _ => Err("type error in unary op (should have been caught)".into()),
                }
            }
            Expr::Binary { op, lhs, rhs, .. } => {
                if let BinOp::And = op {
                    return Ok(Val::Bool(
                        self.as_bool(self.expr(lhs, scope)?)?
                            && self.as_bool(self.expr(rhs, scope)?)?,
                    ));
                }
                if let BinOp::Or = op {
                    return Ok(Val::Bool(
                        self.as_bool(self.expr(lhs, scope)?)?
                            || self.as_bool(self.expr(rhs, scope)?)?,
                    ));
                }
                let l = self.expr(lhs, scope)?;
                let r = self.expr(rhs, scope)?;
                self.binop(*op, l, r)
            }
            // `xs[i]` on a local Array or String, read WITHOUT copying `xs`.
            //
            // `xs[i]` parses as `@at(xs, i)`, and evaluating the `xs` argument
            // clones the whole collection so the index can throw it away: an
            // array read cost ~24 us against ~0.7 us for a loop doing no array
            // work, and the gap scaled with the collection's LENGTH rather than
            // the number of reads. std/vyx scans its input byte by byte this way,
            // which is most of what made compiling a .vyx page cost seconds.
            //
            // The guard matches only when it will succeed — a local holding an
            // Array or a String — so there is no fallback to get wrong. An
            // earlier version fell back to `self.call("@at", ..)`, which is not
            // where that builtin is dispatched; every Map and record receiver
            // took the fallback and died with "unknown function `@at`". Parity
            // caught it, which is what parity is for.
            //
            // Restricted to a local receiver so nothing evaluated for the index
            // can reach it — the same reason the in-place append above is safe.
            Expr::Call { name, args, .. }
                if name == crate::project::AT
                    && args.len() == 2
                    && matches!(&args[0], Expr::Var { .. })
                    && {
                        let Expr::Var { name: v, .. } = &args[0] else {
                            unreachable!()
                        };
                        scope
                            .iter()
                            .rev()
                            .find_map(|f| f.get(v))
                            .is_some_and(|s| matches!(s.v, Val::Array(_) | Val::Str(_)))
                    } =>
            {
                let Expr::Var { name: v, .. } = &args[0] else {
                    unreachable!()
                };
                let idx = self.expr(&args[1], scope)?;
                let i = match &idx {
                    Val::Int(i) => *i,
                    Val::IntN { v, .. } => *v,
                    other => {
                        return Err(format!("index must be an integer, found {other:?}").into())
                    }
                };
                let slot = scope
                    .iter()
                    .rev()
                    .find_map(|f| f.get(v))
                    .expect("guarded above");
                match &slot.v {
                    Val::Array(elems) => elems
                        .get(i as usize)
                        .cloned()
                        .ok_or_else(|| Ctrl::from(crate::trap::array_index(i))),
                    Val::Str(st) => st
                        .as_bytes()
                        .get(i as usize)
                        .map(|b| Val::IntN {
                            v: *b as i64,
                            bits: 8,
                            signed: false,
                        })
                        .ok_or_else(|| Ctrl::from(crate::trap::string_index(i))),
                    _ => unreachable!("guarded above"),
                }
            }
            Expr::Call { name, args, line } => {
                // `a[i]` on a container of the user's own (RFC-0091 M2): inline
                // the `place at` projection here and read the place it yields.
                // A builtin container falls through to `@at` below, which reads
                // the element directly — the interpreter has no lowering to
                // delete, so it keeps one spelling.
                if name == crate::project::AT && args.len() == 2 {
                    if let Some(v) = self.project_read(args, scope, *line)? {
                        return Ok(v);
                    }
                }
                // Calling a `fn`-typed parameter (RFC-0023): `f(x)` where `f` is a
                // local bound to a function value. Resolved before the builtins so
                // a parameter always shadows a same-named builtin, and evaluated by
                // invoking the closure directly (a monomorphized direct call in the
                // native/wasm backends).
                if let Some(fv) = self.lookup_fnval(scope, name) {
                    let mut vals = Vec::with_capacity(args.len());
                    for a in args {
                        vals.push(self.expr(a, scope)?);
                    }
                    return self.call_fnval(&fv, &vals);
                }
                // Test builtins (RFC-0015): `assert` / `assertEq`. A failing
                // assertion traps the current test with a canonical message; the
                // `vyrn test` runner catches it and marks the test FAILED.
                if name == "assert" {
                    match self.expr(&args[0], scope)? {
                        Val::Bool(true) => return Ok(Val::Unit),
                        Val::Bool(false) => {
                            return Err(format!("assertion failed at line {line}").into())
                        }
                        other => return Err(format!("assert of non-Bool {other:?}").into()),
                    }
                }
                // `blackBox(v)` (RFC-0055): identity in the interpreter (which does
                // not optimize, so there is nothing to defeat). The optimizer-opacity
                // guarantee is a native/wasm codegen property; here the value simply
                // flows straight through.
                if name == "blackBox" {
                    return self.expr(&args[0], scope);
                }
                if name == "assertEq" {
                    let a = self.expr(&args[0], scope)?;
                    let b = self.expr(&args[1], scope)?;
                    // Reuse `==` semantics exactly (parity-identical by
                    // construction), then render each side with the canonical
                    // `toString` formatting on mismatch.
                    let equal = matches!(
                        self.binop(BinOp::Eq, a.clone(), b.clone())?,
                        Val::Bool(true)
                    );
                    if equal {
                        return Ok(Val::Unit);
                    }
                    return Err(format!(
                        "assertion failed at line {line}: {} != {}",
                        scalar_to_string(&a),
                        scalar_to_string(&b)
                    )
                    .into());
                }
                // `schemaOf(TypeName)` reflects a type at compile time — its
                // argument is a type name, not a value — so build and evaluate its
                // `Schema` literal before the normal argument evaluation.
                if name == "schemaOf" {
                    if let Some(Expr::Var { name: tn, .. }) = args.first() {
                        if let Some(decl) = self.types.get(tn.as_str()) {
                            let sl = crate::types::schema_struct_lit(decl);
                            return self.expr(&sl, scope);
                        }
                    }
                    return Err("`schemaOf` needs a declared type name".into());
                }
                // `jsonSchema(TypeName)` renders the declared type as a JSON Schema
                // string at compile time — computed from the declaration, so both
                // backends produce identical bytes.
                // `contractOf(Name)` reflects a module contract at compile time
                // (RFC-0071) — its argument is a contract name, not a value, so
                // build and evaluate its `ContractInfo` literal exactly the way
                // `schemaOf` builds a `Schema`.
                if name == "contractOf" {
                    if let Some(Expr::Var { name: cn, .. }) = args.first() {
                        if let Some(decl) = self.contracts.get(cn.as_str()) {
                            let cl = crate::schema_reflect::contract_info_lit(decl);
                            return self.expr(&cl, scope);
                        }
                    }
                    return Err("`contractOf` needs a declared contract name".into());
                }
                if name == "jsonSchema" {
                    if let Some(Expr::Var { name: tn, .. }) = args.first() {
                        if self.types.contains_key(tn.as_str()) {
                            // `json_schema_string` wants an owned `TypeDecl` map; the
                            // interpreter keeps borrows, so materialize one here (only
                            // on this rare compile-time-reflection call).
                            let owned: std::collections::HashMap<String, crate::ast::TypeDecl> =
                                self.types
                                    .iter()
                                    .map(|(k, v)| (k.to_string(), (*v).clone()))
                                    .collect();
                            let js = crate::types::json_schema_string(&owned[tn.as_str()], &owned);
                            return Ok(Val::Str(std::rc::Rc::new(js)));
                        }
                    }
                    return Err("`jsonSchema` needs a declared type name".into());
                }
                // `toJson(x)` (RFC-0018) — encode a codable value to canonical
                // JSON. The argument's static type drives record field order and
                // the None-field omission, so infer it alongside the value.
                if name == "toJson" {
                    let ty = self
                        .type_of(&args[0], scope)
                        .ok_or("`toJson` could not determine the argument's type")?;
                    let e = crate::jsonenc::encode_expr(args[0].clone(), &ty, *line);
                    return self.expr(&e, scope);
                }
                // `fromJson(TypeName, s)` (RFC-0018) — type-directed decode into
                // `Validation<T>`. Never traps; every problem is an accumulated
                // `Issue`. The first argument is a type name (not a value).
                if name == "fromJson" {
                    let tn = match args.first() {
                        Some(Expr::Var { name: tn, .. })
                            if self.types.contains_key(tn.as_str()) =>
                        {
                            tn.clone()
                        }
                        _ => return Err("`fromJson` needs a declared type name".into()),
                    };
                    let target = Type::Named(tn);
                    let e = crate::jsondec::decode_expr(&target, args[1].clone(), *line);
                    let top = crate::jsondec::top_name(&target);
                    if !self.funcs.contains_key(top.as_str()) {
                        return Err(format!(
                            "`fromJson` needs the Vyrn runtime: no decoder for `{target}`                              (is a std library root reachable?)"
                        )
                        .into());
                    }
                    return self.expr(&e, scope);
                }
                // `a.pop()` (RFC-0011) — remove and return the last element as
                // `Option<T>` (`None` on empty), mutating the receiver in place.
                // Handled before the generic argument evaluation because it needs
                // to write the shrunk array back through the binding.
                if name == "@pop" {
                    if let Some(Expr::Var { name: recv, .. }) = args.first() {
                        for frame in scope.iter_mut().rev() {
                            if let Some(Slot {
                                v: Val::Array(items),
                                ..
                            }) = frame.get_mut(recv)
                            {
                                let popped = std::rc::Rc::make_mut(items).pop();
                                return Ok(Val::Option(popped.map(Box::new)));
                            }
                        }
                        if let Some(Slot {
                            v: Val::Array(items),
                            ..
                        }) = self.globals.borrow_mut().get_mut(recv)
                        {
                            let popped = std::rc::Rc::make_mut(items).pop();
                            return Ok(Val::Option(popped.map(Box::new)));
                        }
                    }
                    return Err("`pop` needs a mutable array binding".into());
                }
                // `a.swapRemove(i)` (RFC-0011) — move the last element into slot
                // `i`, shrink by one, return the old element `i`. Traps on an
                // out-of-bounds index with the read path's wording.
                if name == "@swapRemove" {
                    let Some(Expr::Var { name: recv, .. }) = args.first() else {
                        return Err("`swapRemove` needs a mutable array binding".into());
                    };
                    let recv = recv.clone();
                    let idx = match self.expr(&args[1], scope)? {
                        Val::Int(n) => n,
                        other => {
                            return Err(
                                format!("array index must be an Int64, found {other:?}").into()
                            )
                        }
                    };
                    for frame in scope.iter_mut().rev() {
                        if let Some(Slot {
                            v: Val::Array(items),
                            ..
                        }) = frame.get_mut(&recv)
                        {
                            if idx < 0 || idx as usize >= items.len() {
                                return Err(crate::trap::array_index(idx).into());
                            }
                            return Ok(std::rc::Rc::make_mut(items).swap_remove(idx as usize));
                        }
                    }
                    if let Some(Slot {
                        v: Val::Array(items),
                        ..
                    }) = self.globals.borrow_mut().get_mut(&recv)
                    {
                        if idx < 0 || idx as usize >= items.len() {
                            return Err(crate::trap::array_index(idx).into());
                        }
                        return Ok(std::rc::Rc::make_mut(items).swap_remove(idx as usize));
                    }
                    return Err("`swapRemove` needs a mutable array binding".into());
                }
                // `F32x4.store(xs, i, v)` (RFC-0083 M2) — write four consecutive
                // elements, bounds-checked ONCE. Handled here rather than below
                // because it mutates the receiver, and the receiver is copy-on-
                // write: storing into the evaluated VALUE would store into a copy.
                if name == "@f32x4Store" || name == "@i32x4Store" || name == "@f64x2Store" {
                    let Some(Expr::Var { name: recv, .. }) = args.first() else {
                        return Err("`.store` needs a mutable array binding".into());
                    };
                    let recv = recv.clone();
                    let idx = match self.expr(&args[1], scope)? {
                        Val::Int(n) => n,
                        other => {
                            return Err(
                                format!("array index must be an Int64, found {other:?}").into()
                            )
                        }
                    };
                    // The lane VALUES the array will hold, boxed by width: the
                    // element type is the lane type, so a store is four ordinary
                    // element writes and the array stays an ordinary array.
                    let lanes: Vec<Val> = match self.expr(&args[2], scope)? {
                        Val::F32x4(v) => v.iter().map(|l| Val::Float32(*l)).collect(),
                        Val::I32x4(v) => v
                            .iter()
                            .map(|l| Val::IntN {
                                v: i64::from(*l),
                                bits: 32,
                                signed: true,
                            })
                            .collect(),
                        Val::F64x2(v) => v.iter().map(|l| Val::Float(*l)).collect(),
                        other => return Err(format!(".store: {other:?}").into()),
                    };
                    // The span is the lane count, which the value already knows —
                    // two `Float64`s write two elements where four `Float32`s
                    // write four.
                    let span = lanes.len() as i64;
                    let put = |items: &mut std::rc::Rc<Vec<Val>>| -> Result<Val, Ctrl> {
                        if idx < 0 || idx > items.len() as i64 - span {
                            return Err(vec_oob(idx, span).into());
                        }
                        let items = std::rc::Rc::make_mut(items);
                        for (k, l) in lanes.iter().enumerate() {
                            items[idx as usize + k] = l.clone();
                        }
                        Ok(Val::Unit)
                    };
                    for frame in scope.iter_mut().rev() {
                        if let Some(Slot {
                            v: Val::Array(items),
                            ..
                        }) = frame.get_mut(&recv)
                        {
                            return put(items);
                        }
                    }
                    if let Some(Slot {
                        v: Val::Array(items),
                        ..
                    }) = self.globals.borrow_mut().get_mut(&recv)
                    {
                        return put(items);
                    }
                    return Err("`.store` needs a mutable array binding".into());
                }
                // `m.remove(k)` (RFC-0028) — remove the entry for `k`, shifting
                // later entries down (order-preserving), returning whether it was
                // present. Mutates the receiver in place, so it is handled here.
                if name == "@remove" {
                    let Some(Expr::Var { name: recv, .. }) = args.first() else {
                        return Err("`remove` needs a mutable map binding".into());
                    };
                    let recv = recv.clone();
                    let key = match self.expr(&args[1], scope)? {
                        Val::Str(s) => s,
                        other => {
                            return Err(
                                format!("a map key must be a String, found {other:?}").into()
                            )
                        }
                    };
                    for frame in scope.iter_mut().rev() {
                        if let Some(Slot {
                            v: Val::Map(pairs), ..
                        }) = frame.get_mut(&recv)
                        {
                            return Ok(Val::Bool(pairs.remove(key.as_str())));
                        }
                    }
                    if let Some(Slot {
                        v: Val::Map(pairs), ..
                    }) = self.globals.borrow_mut().get_mut(&recv)
                    {
                        return Ok(Val::Bool(pairs.remove(key.as_str())));
                    }
                    return Err("`remove` needs a mutable map binding".into());
                }
                // A callee with `fn`-typed parameters (RFC-0023): materialize each
                // such argument into a function value (a lambda snapshots its
                // captures HERE, at the outer call; a bare name becomes a named or
                // pass-through function value). Every other argument evaluates
                // normally. The callee's declared parameter types drive which is
                // which.
                let fn_param_tys: Option<Vec<Type>> = self
                    .funcs
                    .get(name.as_str())
                    .map(|f| f.params.iter().map(|p| p.ty.clone()).collect());
                let mut vals = Vec::with_capacity(args.len());
                for (i, a) in args.iter().enumerate() {
                    match fn_param_tys.as_ref().and_then(|ts| ts.get(i)) {
                        Some(fnty @ Type::Fn(..)) => vals.push(self.eval_fn_arg(a, scope, fnty)?),
                        _ => vals.push(self.expr(a, scope)?),
                    }
                }
                // Numeric conversion `Int32(x)`, `Float64(x)`, ...
                if let Some(target) = crate::types::numeric_conv_target(name) {
                    if vals.len() == 1 {
                        return Ok(convert_val(vals.remove(0), &target));
                    }
                }
                // A user function shadows the gen-only surface builtins
                // (`render`/`rawAt`/`raw`/`lex`) — they are common words and not
                // reserved, so a same-named user function wins (RFC-0054).
                let shadowed = matches!(name.as_str(), "render" | "rawAt" | "raw" | "lex")
                    && self.funcs.contains_key(name.as_str());
                // RFC-0078 M4c: a builtin whose implementation IS a Vyrn function
                // is a call to it, and nothing else. The loader injected the module
                // and renamed its declarations to reserved `$` spellings no source
                // can write; the builtin's own name is reserved too, so there is no
                // shadowing question to answer — and the interpreter holds no second
                // definition to drift from.
                if let Some(rt) = crate::loader::routed_builtin(name) {
                    if !self.funcs.contains_key(rt) {
                        return Err(format!(
                            "`{name}` is implemented in Vyrn and its module is not in the link \
                             — a std root is needed to call it"
                        )
                        .into());
                    }
                    return self.call(rt, &vals);
                }
                // RFC-0094 M3: the three renderers take a union of the scalars
                // the language renders. A value outside that union renders
                // through its own `impl Show`, and what the arms below then see
                // is the String it handed back — so `print`, `@str` and `value`
                // each keep exactly one lowering.
                if matches!(name.as_str(), "print" | "@str" | "value") && vals.len() == 1 {
                    if let Some(m) = self.show_dispatch(&vals[0]) {
                        vals[0] = self.call(&m, &vals[0..1])?;
                    }
                }
                match name.as_str() {
                    // RFC-0079: `panic(msg)` is a trap whose text the caller
                    // wrote. Same channel as every `@.trap.*` — the CLI prefixes
                    // `error: ` and the newline — so nothing here frames it but
                    // the site, which census U5 appends: `msg (file:line)`.
                    // `panic` without a site is the single-file `analyze` path,
                    // which never runs the loader that stamps one.
                    "panic" | "@panicAt" => {
                        let msg = match &vals[0] {
                            Val::Str(s) => (**s).clone(),
                            other => format!("{other:?}"),
                        };
                        Err(Ctrl::Err(match vals.get(1) {
                            Some(Val::Str(at)) => format!("{msg} ({at})"),
                            _ => msg,
                        }))
                    }
                    "print" => {
                        match &vals[0] {
                            Val::Int(n) => vyrn_out!("{n}"),
                            // A sized int prints its logical value; unsigned
                            // formats the bits as `u64` (native uses %lu).
                            Val::IntN {
                                v, signed: true, ..
                            } => vyrn_out!("{v}"),
                            Val::IntN {
                                v, signed: false, ..
                            } => vyrn_out!("{}", *v as u64),
                            // Fixed 6-decimal precision matches native `printf("%f")`
                            // exactly (Rust's shortest-repr Display would not). A
                            // Float32 promotes to f64 for printing, as C varargs do.
                            Val::Float(x) => vyrn_out!("{x:.6}"),
                            Val::Float32(x) => vyrn_out!("{:.6}", *x as f64),
                            Val::Bool(b) => vyrn_out!("{b}"),
                            Val::Str(s) => vyrn_out!("{s}"),
                            other => vyrn_out!("{other:?}"),
                        }
                        Ok(Val::Unit)
                    }
                    // Vector construction, splat and lane read (RFC-0083 M1). Each
                    // lane goes through `convert_val` rather than being read
                    // straight off the `Val`, because a float LITERAL evaluates to
                    // `Val::Float` (f64) here while the checker already typed it
                    // `Float32` — the same rounding the backends' `fptrunc` does.
                    "F32x4" => {
                        let mut lanes = [0f32; 4];
                        for (i, v) in vals.into_iter().enumerate() {
                            lanes[i] = match convert_val(v, &Type::Float32) {
                                Val::Float32(f) => f,
                                other => return Err(format!("F32x4 lane: {other:?}").into()),
                            };
                        }
                        Ok(Val::F32x4(lanes))
                    }
                    "@f32x4Splat" => match convert_val(vals.remove(0), &Type::Float32) {
                        Val::Float32(f) => Ok(Val::F32x4([f; 4])),
                        other => Err(format!("F32x4.splat: {other:?}").into()),
                    },
                    // The integer width (RFC-0083 M3), same two shapes. An integer
                    // literal is a `Val::Int` (i64) here for the reason a float
                    // literal is a `Val::Float`, so it goes through `convert_val`
                    // to be truncated exactly as the backends' `trunc i64 to i32`
                    // truncates it.
                    "I32x4" => {
                        let mut lanes = [0i32; 4];
                        for (i, v) in vals.into_iter().enumerate() {
                            lanes[i] = i32_lane(v)?;
                        }
                        Ok(Val::I32x4(lanes))
                    }
                    "@i32x4Splat" => Ok(Val::I32x4([i32_lane(vals.remove(0))?; 4])),
                    // The wide float width (RFC-0083 M4). `Float64` is the
                    // literal's own type, so `convert_val` has nothing to round
                    // here — unlike both narrower widths, where a literal arrives
                    // wider than the lane.
                    "F64x2" => {
                        let mut lanes = [0f64; 2];
                        for (i, v) in vals.into_iter().enumerate() {
                            lanes[i] = match convert_val(v, &Type::Float) {
                                Val::Float(f) => f,
                                other => return Err(format!("F64x2 lane: {other:?}").into()),
                            };
                        }
                        Ok(Val::F64x2(lanes))
                    }
                    "@f64x2Splat" => match convert_val(vals.remove(0), &Type::Float) {
                        Val::Float(f) => Ok(Val::F64x2([f; 2])),
                        other => Err(format!("F64x2.splat: {other:?}").into()),
                    },
                    // The index was proven constant and in range by the checker,
                    // so there is nothing to bounds-check and no trap to reach.
                    "@lane" => match (&vals[0], &vals[1]) {
                        (Val::F32x4(v), Val::Int(k)) => Ok(Val::Float32(v[*k as usize])),
                        (Val::I32x4(v), Val::Int(k)) => Ok(Val::IntN {
                            v: i64::from(v[*k as usize]),
                            bits: 32,
                            signed: true,
                        }),
                        (Val::Mask32x4(m), Val::Int(k)) => Ok(Val::Bool(m[*k as usize])),
                        (Val::F64x2(v), Val::Int(k)) => Ok(Val::Float(v[*k as usize])),
                        (Val::Mask64x2(m), Val::Int(k)) => Ok(Val::Bool(m[*k as usize])),
                        (a, b) => Err(format!("lane: {a:?}[{b:?}]").into()),
                    },
                    // `v.replaceLane(k, x)` — `lane`'s inverse, and the same
                    // checker-proven constant index, so again no bounds check.
                    // The lane goes through `convert_val` for the reason the
                    // constructor's do: a float LITERAL is a `Val::Float` here.
                    "@replaceLane" => {
                        let Val::Int(k) = vals[1] else {
                            return Err(format!("replaceLane index: {:?}", vals[1]).into());
                        };
                        let recv = vals[0].clone();
                        let x = vals.remove(2);
                        match recv {
                            Val::F32x4(mut v) => {
                                v[k as usize] = match convert_val(x, &Type::Float32) {
                                    Val::Float32(f) => f,
                                    o => return Err(format!("replaceLane value: {o:?}").into()),
                                };
                                Ok(Val::F32x4(v))
                            }
                            Val::I32x4(mut v) => {
                                v[k as usize] = i32_lane(x)?;
                                Ok(Val::I32x4(v))
                            }
                            Val::F64x2(mut v) => {
                                v[k as usize] = match convert_val(x, &Type::Float) {
                                    Val::Float(f) => f,
                                    o => return Err(format!("replaceLane value: {o:?}").into()),
                                };
                                Ok(Val::F64x2(v))
                            }
                            other => Err(format!("replaceLane: {other:?}").into()),
                        }
                    }
                    // Mask reductions (RFC-0083 M2). This is the reference answer
                    // the two backends' single instructions have to match, and it
                    // is a plain fold over the four lanes because a `Mask32x4`'s
                    // only inhabitants are comparison results — there is no
                    // "partly set" lane here whose meaning the engines could read
                    // differently.
                    "@anyTrue" | "@allTrue" => {
                        // Either mask, over its own lanes — the fold does not care
                        // how many there are, which is the shape wasm's two
                        // all-true opcodes have too.
                        let m: &[bool] = match &vals[0] {
                            Val::Mask32x4(m) => m,
                            Val::Mask64x2(m) => m,
                            other => return Err(format!("mask reduce: {other:?}").into()),
                        };
                        Ok(Val::Bool(if name == "@anyTrue" {
                            m.iter().any(|b| *b)
                        } else {
                            m.iter().all(|b| *b)
                        }))
                    }
                    // `F32x4.load(xs, i)` (RFC-0083 M2) — four consecutive
                    // elements as one value, bounds-checked ONCE. The check is
                    // signed and against `i + 4`, not unsigned against `i`, because
                    // `i + 4` on an unsigned index can wrap past the length and let
                    // a load through; `len - 4` cannot, since `len >= 0`.
                    "@f32x4Load" => {
                        let (Val::Array(xs), Val::Int(i)) = (&vals[0], &vals[1]) else {
                            return Err(format!("F32x4.load: {:?}", vals[0]).into());
                        };
                        let (i, len) = (*i, xs.len() as i64);
                        if i < 0 || i > len - 4 {
                            return Err(vec_oob(i, 4).into());
                        }
                        let mut lanes = [0f32; 4];
                        for (k, l) in lanes.iter_mut().enumerate() {
                            *l = match &xs[(i + k as i64) as usize] {
                                Val::Float32(f) => *f,
                                other => return Err(format!("F32x4.load lane: {other:?}").into()),
                            };
                        }
                        Ok(Val::F32x4(lanes))
                    }
                    // The integer load (RFC-0083 M3), the same one check for four
                    // elements. Written separately rather than merged with the
                    // float one because the ELEMENT type is what differs and the
                    // census scan reads these arm heads.
                    "@i32x4Load" => {
                        let (Val::Array(xs), Val::Int(i)) = (&vals[0], &vals[1]) else {
                            return Err(format!("I32x4.load: {:?}", vals[0]).into());
                        };
                        let (i, len) = (*i, xs.len() as i64);
                        if i < 0 || i > len - 4 {
                            return Err(vec_oob(i, 4).into());
                        }
                        let mut lanes = [0i32; 4];
                        for (k, l) in lanes.iter_mut().enumerate() {
                            *l = i32_lane(xs[(i + k as i64) as usize].clone())?;
                        }
                        Ok(Val::I32x4(lanes))
                    }
                    // The wide load (RFC-0083 M4): TWO elements behind the one
                    // check, and the check is `len - 2` rather than `len - 4` for
                    // that reason. An 8-byte stride on the backends' side, which
                    // is the one place the wide width is not the narrow one with
                    // different opcodes.
                    "@f64x2Load" => {
                        let (Val::Array(xs), Val::Int(i)) = (&vals[0], &vals[1]) else {
                            return Err(format!("F64x2.load: {:?}", vals[0]).into());
                        };
                        let (i, len) = (*i, xs.len() as i64);
                        if i < 0 || i > len - 2 {
                            return Err(vec_oob(i, 2).into());
                        }
                        let mut lanes = [0f64; 2];
                        for (k, l) in lanes.iter_mut().enumerate() {
                            *l = match &xs[(i + k as i64) as usize] {
                                Val::Float(f) => *f,
                                other => return Err(format!("F64x2.load lane: {other:?}").into()),
                            };
                        }
                        Ok(Val::F64x2(lanes))
                    }
                    // (`@i32x4Min`/`Max`/`Abs` were here, lowering to
                    // `i32x4.min_s`/`max_s`/`abs`, and were deleted for `select`'s
                    // reason with `select`'s number. Natively LLVM compiles the
                    // Vyrn `if a < b` into the same `pminsd` — 5.98 µs against
                    // 5.98 µs per 65536 lanes — and on wasm the builtin wins 1.05x
                    // once the Vyrn version is written without helper calls. An
                    // integer `min` is ONE comparison; the float one below has a
                    // NaN rule and a signed zero to reproduce, which is why that
                    // one is 3.7x and this one was not worth a row. See RFC-0083's
                    // M3 note.)
                    //
                    // `min`/`max` are IEEE-754-2019 `minimum`/`maximum`, which is
                    // wasm's `f32x4.min` rule: NaN in either operand propagates,
                    // and `-0.0` orders below `+0.0`. Written out rather than
                    // reaching for `f32::min`, which is `minNum` and returns the
                    // NON-NaN operand — a difference the six-decimal formatter DOES
                    // show, unlike a NaN payload difference.
                    //
                    // `nearest` is `round_ties_even` and deliberately NOT
                    // `f32::round`, which is roundTiesAwayFromZero: they differ on
                    // exactly the halves (`2.5` -> 2 against 3), which is the same
                    // kind of silent split `f32::min` would have been. wasm's
                    // `f32x4.nearest` and LLVM's `llvm.roundeven` are both
                    // roundTiesToEven, measured before the choice was made rather
                    // than after.
                    "@f32x4Min" | "@f32x4Max" | "@f32x4Sqrt" | "@f32x4Ceil" | "@f32x4Floor"
                    | "@f32x4Trunc" | "@f32x4Nearest" => {
                        let a = match &vals[0] {
                            Val::F32x4(v) => *v,
                            other => return Err(format!("F32x4 op: {other:?}").into()),
                        };
                        let b = match vals.get(1) {
                            Some(Val::F32x4(v)) => *v,
                            _ => a,
                        };
                        let mut out = [0f32; 4];
                        for k in 0..4 {
                            out[k] = match name.as_str() {
                                "@f32x4Min" => fminimum(a[k], b[k]),
                                "@f32x4Max" => fmaximum(a[k], b[k]),
                                // (`@f32x4Abs` was here, clearing the sign bit.
                                // Deleted in M4: a bit operation with no rule to
                                // reproduce measured 1.00x native and 1.07x wasm
                                // once the Vyrn version was written without a
                                // helper call, which is `select`'s bar.)
                                "@f32x4Ceil" => a[k].ceil(),
                                "@f32x4Floor" => a[k].floor(),
                                "@f32x4Trunc" => a[k].trunc(),
                                "@f32x4Nearest" => a[k].round_ties_even(),
                                // Spelled out rather than left as the `_` arm
                                // because the census scan (`primitives.rs`) reads
                                // these literals, and the outer head above is too
                                // long for it to parse across the wrap.
                                "@f32x4Sqrt" => a[k].sqrt(),
                                other => return Err(format!("vector op: {other}").into()),
                            };
                        }
                        Ok(Val::F32x4(out))
                    }
                    // The wide width's three (RFC-0083 M4), and only three: the
                    // same NaN rule and the same signed zero at 64 bits, plus the
                    // square root that is not writable in Vyrn at any width. The
                    // four roundings are NOT here — `f64x2.ceil` and the rest all
                    // exist, and they were left out because the four `F32x4`
                    // rounding rows are this RFC's weakest block and four more
                    // would be symmetry rather than evidence.
                    "@f64x2Min" | "@f64x2Max" | "@f64x2Sqrt" => {
                        let a = match &vals[0] {
                            Val::F64x2(v) => *v,
                            other => return Err(format!("F64x2 op: {other:?}").into()),
                        };
                        let b = match vals.get(1) {
                            Some(Val::F64x2(v)) => *v,
                            _ => a,
                        };
                        let mut out = [0f64; 2];
                        for k in 0..2 {
                            out[k] = match name.as_str() {
                                "@f64x2Min" => fminimum64(a[k], b[k]),
                                "@f64x2Max" => fmaximum64(a[k], b[k]),
                                // Spelled out for the census scan's sake, exactly
                                // as the narrow width's `sqrt` is.
                                "@f64x2Sqrt" => a[k].sqrt(),
                                other => return Err(format!("vector op: {other}").into()),
                            };
                        }
                        Ok(Val::F64x2(out))
                    }
                    // (`@f32x4Select` was here, lowering to `v128.bitselect` and
                    // `select <4 x i1>`. It is not a builtin: written in Vyrn on
                    // `m.lane(k)` and `if` it measured 1.1x native and 1.06x wasm —
                    // both optimizers turn the four branches back into a blend — so
                    // by RFC-0078's own bar it had no reason to be a primitive.
                    // `examples/simdbench.vyrn`'s `selectV` is the measurement and
                    // the replacement at once.)
                    // A logger handle is its name string (RFC-0008).
                    "logger" => Ok(vals.remove(0)),
                    // Log methods write `[LEVEL] name: msg` to stderr (kept off
                    // stdout, so program output and logs are separable — the
                    // "where does it print" concern behind RFC-0008).
                    "trace" | "debug" | "info" | "warn" | "error" => {
                        // Drop calls below the configured threshold (RFC-0008).
                        if log_level_ordinal(name).unwrap_or(0) >= self.log_level {
                            let lname = match &vals[0] {
                                Val::Str(s) => (**s).clone(),
                                other => format!("{other:?}"),
                            };
                            let msg = match &vals[1] {
                                Val::Str(s) => (**s).clone(),
                                other => format!("{other:?}"),
                            };
                            let line = format!("[{}] {lname}: {msg}", name.to_uppercase());
                            match &self.log_sink {
                                LogSink::Stderr => vyrn_err!("{line}"),
                                LogSink::Stdout => vyrn_out!("{line}"),
                                LogSink::File(_) => {
                                    if let Some(f) = self.log_file.borrow_mut().as_mut() {
                                        let _ = writeln!(f, "{line}");
                                    }
                                }
                            }
                        }
                        Ok(Val::Unit)
                    }
                    // `@concat` — internal spelling produced by interpolation
                    // (the surface form is `a + b`, handled in `binop`).
                    "@concat" => match (&vals[0], &vals[1]) {
                        (Val::Str(a), Val::Str(b)) => concat_str(a, b),
                        _ => Err("@concat of non-Strings".into()),
                    },
                    // RFC-0054 code quotes. `@codeText`/`@codeSplice` are the
                    // internal desugar of `vyrn"…"`; `render`/`rawAt`/`raw`/`lex`
                    // are the surface builtins. Gen-only is enforced by the CHECKER
                    // (`in_gen`), which is what keeps them out of every backend (a
                    // `gen fn` body is never emitted); the operations themselves are
                    // pure, so the interpreter runs them anywhere a checked program
                    // reaches — in particular so a `gen fn` emission helper stays
                    // unit-testable at runtime (RFC-0021: a `gen fn` is callable for
                    // testing).
                    "@codeText" => match &vals[0] {
                        Val::Str(s) => Ok(Val::Code(vec![CodePiece::Text((**s).clone())])),
                        _ => Err("@codeText of non-String".into()),
                    },
                    "@codeSplice" => {
                        let ctx = match &vals[1] {
                            Val::Int(n) => *n,
                            _ => return Err("@codeSplice context flag must be Int".into()),
                        };
                        Ok(Val::Code(code_splice(&vals[0], ctx)?))
                    }
                    "raw" if !shadowed => match &vals[0] {
                        Val::Str(s) => Ok(Val::Code(vec![CodePiece::Text((**s).clone())])),
                        _ => Err("`raw` of non-String".into()),
                    },
                    "rawAt" if !shadowed => {
                        let text = match &vals[0] {
                            Val::Str(s) => (**s).clone(),
                            _ => return Err("`rawAt` text must be a String".into()),
                        };
                        let path = match &vals[1] {
                            Val::Str(s) => (**s).clone(),
                            _ => return Err("`rawAt` path must be a String".into()),
                        };
                        let line = match &vals[2] {
                            Val::Int(n) => *n,
                            _ => return Err("`rawAt` line must be an Int64".into()),
                        };
                        let col = match &vals[3] {
                            Val::Int(n) => *n,
                            _ => return Err("`rawAt` col must be an Int64".into()),
                        };
                        Ok(Val::Code(vec![CodePiece::Origin {
                            path,
                            line,
                            col,
                            text,
                        }]))
                    }
                    "render" if !shadowed => match &vals[0] {
                        Val::Code(pieces) => Ok(Val::Str(std::rc::Rc::new(render_code(pieces)))),
                        _ => Err("`render` of non-Code".into()),
                    },
                    "lex" if !shadowed => match &vals[0] {
                        Val::Str(s) => Ok(Val::Array(std::rc::Rc::new(lex_tokens(s)))),
                        _ => Err("`lex` of non-String".into()),
                    },
                    // (`contains`, `startsWith`, `endsWith` and — since RFC-0079 M3
                    // — `slice` are `std/strpred`. The predicates were three Rust
                    // one-liners, but three DEFINITIONS, and the direct wasm backend
                    // owed a fourth. `slice` was the expensive one: `is_char_boundary`
                    // here, an open-coded continuation-byte pair in the emitted IR,
                    // and a third in the direct backend's runtime, all four of them
                    // agreeing only because a test said so. It returns its failure
                    // now, so the whole range check is `sliceV`'s.)
                    // `bytes` decodes the UTF-8 bytes as UInt8 (RFC-0014 M2) —
                    // the irreducible VIEW every Vyrn string routine is built on.
                    "bytes" => match &vals[0] {
                        Val::Str(s) => Ok(Val::Array(
                            s.bytes()
                                .map(|b| Val::IntN {
                                    v: b as i64,
                                    bits: 8,
                                    signed: false,
                                })
                                .collect::<Vec<_>>()
                                .into(),
                        )),
                        _ => Err("bytes of non-String".into()),
                    },
                    // (`chars` is `std/text`'s `charsV` — RFC-0078 M4c. Rust's
                    // `str::chars` here against a two-pass decoder in 82 lines of
                    // emitted IR; one Vyrn `decodeUtf8` replaces both.)
                    // Input I/O (RFC-0014). Error payloads are canonical Vyrn
                    // wording (never Rust `io::Error` text) — kept byte-identical
                    // to the codegen's format strings so all three backends agree.
                    "args" => Ok(Val::Array(
                        self.args
                            .iter()
                            .map(|s| Val::Str(std::rc::Rc::new(s.clone())))
                            .collect::<Vec<_>>()
                            .into(),
                    )),
                    "readLine" => {
                        // One raw line: bytes up to and including `\n`, or empty
                        // at EOF.
                        let mut buf = host_read_line();
                        if buf.is_empty() {
                            return Ok(Val::Option(None)); // EOF
                        }
                        // Strip a trailing `\n`, then a trailing `\r` (so Windows
                        // and POSIX pipes read identically).
                        if buf.last() == Some(&b'\n') {
                            buf.pop();
                            if buf.last() == Some(&b'\r') {
                                buf.pop();
                            }
                        }
                        // A NUL byte cannot live in a NUL-terminated Vyrn String,
                        // so a line containing one is not representable → None
                        // (the parity-safe rule; documented in RFC-0014).
                        if buf.contains(&0) {
                            return Ok(Val::Option(None));
                        }
                        match String::from_utf8(buf) {
                            Ok(s) => Ok(Val::Option(Some(Box::new(Val::Str(std::rc::Rc::new(s)))))),
                            // Not valid UTF-8: not representable as a String → None
                            // (native rejects the same way via the UTF-8 DFA).
                            Err(_) => Ok(Val::Option(None)),
                        }
                    }
                    "readFile" => {
                        let path = match &vals[0] {
                            Val::Str(s) => s.clone(),
                            other => return Err(format!("readFile of non-String {other:?}").into()),
                        };
                        // In a generation run (RFC-0021), route through the
                        // resolver, path-scoped + recorded for the cache key.
                        if self.gen.is_some() {
                            return self.gen_read_file(&path);
                        }
                        match std::fs::read(path.as_str()) {
                            Ok(bytes) => {
                                // NUL first: a NUL byte IS valid UTF-8, but cannot
                                // survive in a NUL-terminated String, so it is
                                // rejected with its own canonical wording before
                                // the UTF-8 check (matches the native ordering).
                                if bytes.contains(&0) {
                                    return Ok(Val::Result(
                                        false,
                                        Box::new(Val::Str(std::rc::Rc::new(crate::trap::io_at(
                                            "nulerr", &path,
                                        )))),
                                    ));
                                }
                                match String::from_utf8(bytes) {
                                    Ok(s) => Ok(Val::Result(
                                        true,
                                        Box::new(Val::Str(std::rc::Rc::new(s))),
                                    )),
                                    Err(_) => Ok(Val::Result(
                                        false,
                                        Box::new(Val::Str(std::rc::Rc::new(crate::trap::io_at(
                                            "utf8err", &path,
                                        )))),
                                    )),
                                }
                            }
                            Err(_) => Ok(Val::Result(
                                false,
                                Box::new(Val::Str(std::rc::Rc::new(crate::trap::io_at(
                                    "readerr", &path,
                                )))),
                            )),
                        }
                    }
                    // `listDir(path) -> Result<Array<String>, String>` (RFC-0021).
                    // Entry names are sorted for cross-platform determinism.
                    "listDir" => {
                        let path = match &vals[0] {
                            Val::Str(s) => s.clone(),
                            other => return Err(format!("listDir of non-String {other:?}").into()),
                        };
                        if self.gen.is_some() {
                            return self.gen_list_dir(&path);
                        }
                        match std::fs::read_dir(path.as_str()) {
                            Ok(entries) => {
                                let mut names: Vec<String> = entries
                                    .filter_map(|e| e.ok())
                                    .map(|e| e.file_name().to_string_lossy().into_owned())
                                    .collect();
                                names.sort();
                                Ok(Val::Result(
                                    true,
                                    Box::new(Val::Array(std::rc::Rc::new(
                                        names
                                            .into_iter()
                                            .map(|n| Val::Str(std::rc::Rc::new(n)))
                                            .collect(),
                                    ))),
                                ))
                            }
                            Err(_) => Ok(Val::Result(
                                false,
                                Box::new(Val::Str(std::rc::Rc::new(crate::trap::io_at(
                                    "listerr", &path,
                                )))),
                            )),
                        }
                    }
                    // `moduleInterface(path) -> ModuleInterface` (RFC-0021) — the
                    // reflection primitive. Generation-only: at runtime it traps.
                    "moduleInterface" => {
                        let path = match &vals[0] {
                            Val::Str(s) => s.clone(),
                            other => {
                                return Err(
                                    format!("moduleInterface of non-String {other:?}").into()
                                )
                            }
                        };
                        let lit = self.gen_module_interface(&path)?;
                        return self.expr(&lit, scope);
                    }
                    "writeFile" => {
                        let path = match &vals[0] {
                            Val::Str(s) => s.clone(),
                            other => {
                                return Err(format!("writeFile of non-String {other:?}").into())
                            }
                        };
                        let contents = match &vals[1] {
                            Val::Str(s) => s.clone(),
                            other => {
                                return Err(format!("writeFile of non-String {other:?}").into())
                            }
                        };
                        match std::fs::write(path.as_str(), contents.as_bytes()) {
                            Ok(()) => Ok(Val::Result(true, Box::new(Val::Bool(true)))),
                            Err(_) => Ok(Val::Result(
                                false,
                                Box::new(Val::Str(std::rc::Rc::new(crate::trap::io_at(
                                    "writeerr", &path,
                                )))),
                            )),
                        }
                    }
                    // RFC-0044: atomic overwrite. On success `Ok(true)`; on failure
                    // the canonical `@.io.*` wording (reusing `cannot write` for the
                    // common not-found/permission case, a distinct message for a
                    // cross-device rename). Wording is byte-identical to the native
                    // shim + wasm shims so a storage program is a parity citizen.
                    "renameFile" => {
                        let from = match &vals[0] {
                            Val::Str(s) => s.clone(),
                            other => {
                                return Err(format!("renameFile of non-String {other:?}").into())
                            }
                        };
                        let to = match &vals[1] {
                            Val::Str(s) => s.clone(),
                            other => {
                                return Err(format!("renameFile of non-String {other:?}").into())
                            }
                        };
                        match std::fs::rename(from.as_str(), to.as_str()) {
                            Ok(()) => Ok(Val::Result(true, Box::new(Val::Bool(true)))),
                            Err(e) => {
                                let msg = if is_cross_device(&e) {
                                    crate::trap::io_at("xdeverr", &to)
                                } else {
                                    crate::trap::io_at("writeerr", &to)
                                };
                                Ok(Val::Result(
                                    false,
                                    Box::new(Val::Str(std::rc::Rc::new(msg))),
                                ))
                            }
                        }
                    }
                    // RFC-0044: flush a file to stable storage (open + sync_all).
                    "fsyncFile" => {
                        let path = match &vals[0] {
                            Val::Str(s) => s.clone(),
                            other => {
                                return Err(format!("fsyncFile of non-String {other:?}").into())
                            }
                        };
                        // Open read+write (not read-only): flushing a file's
                        // buffers needs write access on Windows (FlushFileBuffers),
                        // and `write(true)` without `truncate` leaves the contents
                        // intact. A missing file is an error (`cannot write`).
                        let synced = std::fs::OpenOptions::new()
                            .write(true)
                            .open(path.as_str())
                            .and_then(|f| f.sync_all());
                        match synced {
                            Ok(()) => Ok(Val::Result(true, Box::new(Val::Bool(true)))),
                            Err(_) => Ok(Val::Result(
                                false,
                                Box::new(Val::Str(std::rc::Rc::new(crate::trap::io_at(
                                    "writeerr", &path,
                                )))),
                            )),
                        }
                    }
                    // RFC-0014 M2 (bytes): binary read + the byte<->String bridge.
                    "readFileBytes" => {
                        let path = match &vals[0] {
                            Val::Str(s) => s.clone(),
                            other => {
                                return Err(format!("readFileBytes of non-String {other:?}").into())
                            }
                        };
                        match std::fs::read(path.as_str()) {
                            Ok(bytes) => Ok(Val::Result(
                                true,
                                Box::new(Val::Array(
                                    bytes
                                        .into_iter()
                                        .map(|b| Val::IntN {
                                            v: b as i64,
                                            bits: 8,
                                            signed: false,
                                        })
                                        .collect::<Vec<_>>()
                                        .into(),
                                )),
                            )),
                            Err(_) => Ok(Val::Result(
                                false,
                                Box::new(Val::Str(std::rc::Rc::new(crate::trap::io_at(
                                    "readerr", &path,
                                )))),
                            )),
                        }
                    }
                    "stringFromBytes" => match &vals[0] {
                        Val::Array(elems) => {
                            let mut bytes = Vec::with_capacity(elems.len());
                            for e in elems.iter() {
                                match e {
                                    Val::IntN { v, .. } => bytes.push(*v as u8),
                                    Val::Int(v) => bytes.push(*v as u8),
                                    other => {
                                        return Err(format!(
                                            "stringFromBytes element is not a byte: {other:?}"
                                        )
                                        .into())
                                    }
                                }
                            }
                            // Same NUL-then-UTF-8 ordering as `readFile`.
                            if bytes.contains(&0) {
                                return Ok(Val::Result(
                                    false,
                                    Box::new(Val::Str(std::rc::Rc::new(
                                        crate::trap::io("bnul").to_string(),
                                    ))),
                                ));
                            }
                            match String::from_utf8(bytes) {
                                Ok(s) => {
                                    Ok(Val::Result(true, Box::new(Val::Str(std::rc::Rc::new(s)))))
                                }
                                Err(_) => Ok(Val::Result(
                                    false,
                                    Box::new(Val::Str(std::rc::Rc::new(
                                        crate::trap::io("butf8").to_string(),
                                    ))),
                                )),
                            }
                        }
                        other => Err(format!("stringFromBytes of non-Array {other:?}").into()),
                    },
                    // The IEEE-754 bit views (RFC-0078 M4a): a reinterpretation,
                    // not a conversion. `UInt64` is carried as an `IntN` whose
                    // `v` holds the raw pattern, so both directions are one
                    // `to_bits`/`from_bits` and nothing rounds.
                    "floatBits" => match &vals[0] {
                        Val::Float(f) => Ok(Val::IntN {
                            v: f.to_bits() as i64,
                            bits: 64,
                            signed: false,
                        }),
                        other => Err(format!("floatBits of non-Float64 {other:?}").into()),
                    },
                    "floatFromBits" => match &vals[0] {
                        Val::IntN { v, .. } => Ok(Val::Float(f64::from_bits(*v as u64))),
                        Val::Int(v) => Ok(Val::Float(f64::from_bits(*v as u64))),
                        other => Err(format!("floatFromBits of non-UInt64 {other:?}").into()),
                    },
                    // (The six text encodings are `std/codecs` — RFC-0078 M4c.
                    // They were 159 lines of Rust here and are now a routed call,
                    // handled above.)
                    // `@str` (from `x.toString()` and interpolation) must render
                    // exactly as `print` does: signed IntN by value, unsigned as
                    // `u64`, Float to 6 decimals.
                    "@str" => match &vals[0] {
                        Val::Int(_)
                        | Val::IntN { .. }
                        | Val::Float(_)
                        | Val::Float32(_)
                        | Val::Bool(_)
                        | Val::Str(_) => Ok(Val::Str(std::rc::Rc::new(scalar_to_string(&vals[0])))),
                        other => Err(format!("str of unsupported value {other:?}").into()),
                    },
                    "parse" => match &vals[0] {
                        Val::Str(s) => Ok(Val::Option(parse_int(s).map(|n| Box::new(Val::Int(n))))),
                        other => Err(format!("parse of non-String {other:?}").into()),
                    },
                    // `lineAt(bytes, off)` / `colAt(bytes, off)`, 1-based.
                    //
                    // Memoized on the buffer's contents: a scanner asks once per
                    // node over the same buffer, and counting newlines from byte
                    // 0 each time is quadratic. Hashing the buffer per call is
                    // linear and ~1 us for a source file, against the ~0.7 ms a
                    // rescan cost.
                    "lineAt" | "colAt" => match (&vals[0], &vals[1]) {
                        (Val::Array(elems), off) => {
                            let off = match off {
                                Val::Int(i) => *i,
                                Val::IntN { v, .. } => *v,
                                other => {
                                    return Err(format!(
                                        "offset must be an integer, found {other:?}"
                                    )
                                    .into())
                                }
                            }
                            .max(0) as usize;
                            let byte_of = |v: &Val| -> u8 {
                                match v {
                                    Val::Int(i) => *i as u8,
                                    Val::IntN { v, .. } => *v as u8,
                                    _ => 0,
                                }
                            };
                            // Keyed by the array's IDENTITY, not its contents.
                            // Hashing 3,000 elements per call cost more than the
                            // scan this replaces; the pointer is O(1). The cache
                            // holds the `Rc`, so the allocation stays alive and
                            // its address cannot be recycled under a live entry.
                            let key = std::rc::Rc::as_ptr(elems) as usize;
                            let starts = LINE_STARTS.with(|c| {
                                if let Some((_, hit)) = c.borrow().get(&key) {
                                    return hit.clone();
                                }
                                let mut v: Vec<usize> = vec![0];
                                for (i, e) in elems.iter().enumerate() {
                                    if byte_of(e) == 10u8 {
                                        v.push(i + 1);
                                    }
                                }
                                let rc = std::rc::Rc::new(v);
                                let mut m = c.borrow_mut();
                                // ponytail: bounded crudely; a scanner touches a
                                // handful of buffers, not thousands.
                                if m.len() > 64 {
                                    m.clear();
                                }
                                m.insert(key, (elems.clone(), rc.clone()));
                                rc
                            });
                            // The last line start at or before `off`.
                            let idx = starts.partition_point(|&s| s <= off).saturating_sub(1);
                            Ok(Val::Int(if name == "lineAt" {
                                idx as i64 + 1
                            } else {
                                (off.min(elems.len()) - starts[idx]) as i64 + 1
                            }))
                        }
                        (other, _) => {
                            Err(format!("{name} takes an Array<UInt8>, found {other:?}").into())
                        }
                    },
                    "@push" => match &vals[0] {
                        Val::Array(elems) => {
                            let mut next = elems.clone();
                            let v = std::rc::Rc::make_mut(&mut next);
                            reserve_vec(v, 1)?;
                            v.push(vals[1].clone());
                            Ok(Val::Array(next))
                        }
                        other => Err(format!("push of non-Array {other:?}").into()),
                    },
                    // Spelled out rather than named through `project::AT`
                    // because `primitives::the_census_is_the_code` reads these
                    // arms as literals.
                    "@at" => match (&vals[0], &vals[1]) {
                        (Val::Array(elems), Val::Int(i)) => elems
                            .get(*i as usize)
                            .cloned()
                            .ok_or_else(|| crate::trap::array_index(i).into()),
                        // `s[i]` on a String is the byte at index `i` as a
                        // `UInt8` (bounds-checked) — same value shape as an
                        // element of `bytes(s)` (RFC-0022).
                        (Val::Str(s), Val::Int(i)) => s
                            .as_bytes()
                            .get(*i as usize)
                            .map(|b| Val::IntN {
                                v: *b as i64,
                                bits: 8,
                                signed: false,
                            })
                            .ok_or_else(|| crate::trap::string_index(i).into()),
                        // `m[k]` on a Map (RFC-0028) → `Option<V>`.
                        (Val::Map(pairs), Val::Str(k)) => Ok(Val::Option(
                            pairs.get(k.as_str()).map(|v| Box::new(v.clone())),
                        )),
                        _ => Err("at of non-Array/Int64".into()),
                    },
                    // `m.has(k)` (RFC-0028) — membership test.
                    "@has" => match (&vals[0], &vals[1]) {
                        (Val::Map(pairs), Val::Str(k)) => Ok(Val::Bool(pairs.contains(k.as_str()))),
                        _ => Err("`has` needs a Map and a String key".into()),
                    },
                    // `m.keys()` (RFC-0028) — a fresh snapshot Array<String> in
                    // insertion order (safe to mutate the map while iterating it).
                    "@keys" => match &vals[0] {
                        Val::Map(pairs) => Ok(Val::Array(
                            pairs
                                .iter()
                                .map(|(k, _)| Val::Str(std::rc::Rc::new(k.clone())))
                                .collect::<Vec<_>>()
                                .into(),
                        )),
                        other => Err(format!("`keys` needs a Map, found {other:?}").into()),
                    },
                    // RFC-0075. The two producers and the release. `close` is
                    // variant-aware (M2b): a buffer stream has nothing the host
                    // does not reclaim anyway, but a stepped one owns a cursor
                    // cell, and that cell is a slot in a slab of 65536 — the one
                    // resource an interpreter CAN exhaust, so releasing it is
                    // observable here exactly as the `free` is natively.
                    "fromArray" => match vals.remove(0) {
                        Val::Array(a) => Ok(Val::Stream(Box::new(StreamVal::Buf(a, 0)))),
                        other => Err(format!("fromArray of non-Array {other:?}").into()),
                    },
                    "fromStep" => {
                        let slot = Self::stream_int(&vals[0])?;
                        let gen = Self::stream_int(&vals[1])?;
                        let step = match vals.remove(2) {
                            Val::Fn(f) => f,
                            other => return Err(format!("fromStep of non-fn {other:?}").into()),
                        };
                        Ok(Val::Stream(Box::new(StreamVal::Step {
                            slot,
                            gen,
                            step,
                            done: false,
                        })))
                    }
                    // RFC-0075 M2c, re-hosted by RFC-0090 M3. The two halves of
                    // one move: a stream leaves the program into a box and comes
                    // back out of it. `movecheck` reads the first as a disposal
                    // and the second as an acquisition, so the pair cannot lose
                    // a stream between them without failing to compile.
                    "boxStream" => match vals.remove(0) {
                        Val::Stream(s) => {
                            let a = self.next_box.get();
                            self.next_box.set(a + 1);
                            self.boxes.borrow_mut().insert(a, *s);
                            Ok(Val::Int(a))
                        }
                        other => Err(format!("boxStream of non-Stream {other:?}").into()),
                    },
                    "unboxStream" => {
                        let a = Self::stream_int(&vals[0])?;
                        match self.boxes.borrow_mut().remove(&a) {
                            Some(s) => Ok(Val::Stream(Box::new(s))),
                            None => Err(Self::no_boxed_stream()),
                        }
                    }
                    // `pullAt(a)` — one element from the stream in that box. The
                    // box stays; only its contents advance.
                    "pullAt" => {
                        let a = Self::stream_int(&vals[0])?;
                        // Taken out for the duration of the step, which may run
                        // arbitrary Vyrn code — including, for a chain, another
                        // `pullAt` on a different box.
                        let Some(mut src) = self.boxes.borrow_mut().remove(&a) else {
                            return Err(Self::no_boxed_stream());
                        };
                        let got = self.stream_next(&mut src);
                        self.boxes.borrow_mut().insert(a, src);
                        Ok(match got? {
                            Some(v) => Val::Option(Some(Box::new(v))),
                            None => Val::Option(None),
                        })
                    }
                    "close" => match &vals[0] {
                        Val::Stream(s) => {
                            self.release_stream(s)?;
                            Ok(Val::Unit)
                        }
                        other => Err(format!("close of non-Stream {other:?}").into()),
                    },
                    // RFC-0074 M3a. The handoff: the stream stops being this
                    // call's obligation and becomes the host's, which will pull
                    // it, write each frame, and `close` it the first time a write
                    // fails. Two in one request would leave the first with nobody
                    // to release it, so it traps rather than silently dropping.
                    "serveStream" => match vals.remove(0) {
                        Val::Stream(s) => {
                            if self.live.borrow().is_some() {
                                return Err(
                                    "serveStream: this request already opened a stream".into()
                                );
                            }
                            *self.live.borrow_mut() = Some(*s);
                            Ok(Val::Unit)
                        }
                        other => Err(format!("serveStream of non-Stream {other:?}").into()),
                    },
                    // value(x) -> Value: box a scalar into the interpolation enum.
                    "value" => {
                        let v = vals.remove(0);
                        let variant = match &v {
                            Val::Int(_) => "IntVal",
                            Val::Bool(_) => "BoolVal",
                            Val::Str(_) => "StrVal",
                            other => return Err(format!("value of {other:?}").into()),
                        };
                        Ok(Val::Enum(variant.to_string(), vec![v]))
                    }
                    // `@list` (tagged-template desugaring): fixed and growable
                    // arrays share a runtime representation here — the identity.
                    "@list" => match &vals[0] {
                        Val::Array(_) => Ok(vals.remove(0)),
                        other => Err(format!("@list of non-Array {other:?}").into()),
                    },
                    // `xs.toArray()` (RFC-0056) — a `SmallArray<T, N>` and an
                    // `Array<T>` share the `Val::Array` representation here, so
                    // the copy-out is the identity (the native/wasm backends copy
                    // the inline/spilled buffer into a fresh heap triple).
                    "@toArray" => match &vals[0] {
                        Val::Array(_) => Ok(vals.remove(0)),
                        other => Err(format!("@toArray of non-Array {other:?}").into()),
                    },
                    // `x.copy()` (RFC-0089 M1b) — a value that shares no heap
                    // with the receiver. Copy-on-write already gives this engine
                    // value semantics, so an identity clone would pass every
                    // test; it is written out anyway, because the two compiled
                    // backends allocate here and the three engines must describe
                    // one operation.
                    // RFC-0091 M1: the type answers first. `impl Copy for T`
                    // is what duplicating a `T` means; everything else derives.
                    "@copy" => {
                        match self
                            .val_type_key(&vals[0])
                            .and_then(|k| crate::types::copy_impl_by_key(self.impls, &k))
                        {
                            Some(m) => self.call(&m, &vals),
                            None => Ok(deep_copy(&vals[0])),
                        }
                    }
                    // `@join` (`t.join()`) awaits a task; eager tasks are in hand.
                    "@join" => Ok(vals.remove(0)),
                    "Some" => Ok(Val::Option(Some(Box::new(vals.remove(0))))),
                    "Ok" => Ok(Val::Result(true, Box::new(vals.remove(0)))),
                    "Err" => Ok(Val::Result(false, Box::new(vals.remove(0)))),
                    _ => {
                        // Protocol-method dispatch (RFC-0002 §5): resolve by the
                        // receiver's runtime type to the impl, and then take the
                        // SAME path an ordinary call takes.
                        //
                        // It used to call the impl and return from here, which
                        // was right while every receiver was `read`. A `modify
                        // self` receiver is call-by-value-result like any other
                        // `modify` parameter, so returning early skipped the
                        // copy-back and `people.insert(x)` left `people` empty
                        // in the interpreter alone.
                        let target = if let Some(proto) =
                            self.protocol_methods.get(name.as_str()).cloned()
                        {
                            let key = self.val_type_key(&vals[0]).ok_or_else(|| {
                                Ctrl::Err(format!("cannot dispatch `{name}` on {:?}", vals[0]))
                            })?;
                            crate::types::impl_method_name(&proto, &key, name)
                        } else {
                            // Enum variant with payload(s), e.g. `Circle(5)`, `Rect(w, h)`.
                            if self.variants.contains(name.as_str()) {
                                return Ok(Val::Enum(name.clone(), vals));
                            }
                            if let Some(decl) = self.types.get(name.as_str()) {
                                return self.construct(decl, vals.remove(0));
                            }
                            name.clone()
                        };
                        // `modify` parameters copy back into the caller's variable
                        // after the call (call-by-value-result).
                        let modifies: Vec<usize> = self
                            .funcs
                            .get(target.as_str())
                            .map(|f| {
                                f.params
                                    .iter()
                                    .enumerate()
                                    .filter(|(_, p)| p.capability == Capability::Modify)
                                    .map(|(i, _)| i)
                                    .collect()
                            })
                            .unwrap_or_default();
                        if modifies.is_empty() {
                            return self.call(&target, &vals);
                        }
                        let (ret, finals) = self.call_capturing(&target, &vals)?;
                        for i in modifies {
                            if let Expr::Var { name: vn, .. } = &args[i] {
                                let mut wrote = false;
                                for frame in scope.iter_mut().rev() {
                                    if let Some(slot) = frame.get_mut(vn) {
                                        slot.v = finals[i].clone();
                                        wrote = true;
                                        break;
                                    }
                                }
                                if !wrote {
                                    if let Some(slot) = self.globals.borrow_mut().get_mut(vn) {
                                        slot.v = finals[i].clone();
                                    }
                                }
                            }
                        }
                        Ok(ret)
                    }
                }
            }
            Expr::Match {
                scrutinee, arms, ..
            } => {
                let sv = self.expr(scrutinee, scope)?;
                let r = self.eval_match(sv.clone(), arms, scope);
                self.release_temp(expr as *const Expr as usize, &sv, r.is_err(), r)
            }
            // `if` as an expression (RFC-0030): evaluate the condition, then ONLY
            // the taken branch (laziness identical to statement-`if`/match). The
            // checker guarantees `else_branch` is present.
            Expr::IfExpr {
                cond,
                then_branch,
                else_branch,
                ..
            } => {
                if self.as_bool(self.expr(cond, scope)?)? {
                    self.expr(then_branch, scope)
                } else if let Some(eb) = else_branch {
                    self.expr(eb, scope)
                } else {
                    Err("internal: `if` expression without `else` reached the \
                         interpreter (checker should have rejected it)"
                        .into())
                }
            }
            Expr::Try { expr: operand, .. } => {
                let v = self.expr(operand, scope)?;
                // A propagating `?` is a function exit, and the frames it
                // unwinds are paying for THIS node — RFC-0101 M4 step 0 is why
                // they are paid at all, and this names what they are paid for.
                // At each raise rather than once above, because the `Fallible`
                // path below calls user code first and a `return` inside that
                // callee would otherwise be the last site left behind.
                let prop = |v| {
                    leaving(crate::own::trace::Exit::Try, expr);
                    Err(Ctrl::Return(v))
                };
                match v {
                    Val::Option(Some(inner)) => Ok(*inner),
                    Val::Option(None) => prop(Val::Option(None)),
                    Val::Result(true, inner) => Ok(*inner),
                    Val::Result(false, e) => prop(Val::Result(false, e)),
                    // Anything else goes through `Fallible` (RFC-0080 M3). The
                    // checker has already confirmed the impl exists and that the
                    // enclosing function returns the same type, so the failing
                    // path returns `v` itself — the whole sum, whichever variant
                    // it is, which is the claim this milestone exists to execute.
                    other => {
                        let key = self.val_type_key(&other).ok_or_else(|| {
                            Ctrl::Err(format!("`?` on a value with no impl target {other:?}"))
                        })?;
                        let ask = |m: &str| {
                            crate::types::impl_method_name(crate::types::FALLIBLE, &key, m)
                        };
                        if !matches!(
                            self.call(&ask("isSuccess"), &[other.clone()])?,
                            Val::Bool(true)
                        ) {
                            return prop(other);
                        }
                        self.call(&ask("success"), &[other])
                    }
                }
            }
            Expr::StructLit { name, fields, .. } => {
                let mut map = HashMap::new();
                for (fname, value) in fields {
                    let v = self.expr(value, scope)?;
                    map.insert(fname.clone(), v);
                }
                // Each field value flows through its declared field type —
                // sized-int wrapping and automatic validation for predicated
                // field types (`age: Age` from a raw Int64 runs Age's check).
                // Generic field types (Params) pass through coerce untouched.
                if let Some(Type::Record(rfields)) = self.types.get(name.as_str()).map(|d| &d.base)
                {
                    for f in rfields {
                        if let Some(v) = map.remove(&f.name) {
                            map.insert(f.name.clone(), self.coerce(v, &f.ty)?);
                        }
                    }
                }
                // Enforce a cross-field `where` invariant, if the record declares
                // one (e.g. `{ start, end } where start < end`). The predicate
                // runs under the runtime evaluator with every field bound, so
                // Float/sized-int fields compare with exact runtime semantics.
                if let Some(decl) = self.types.get(name.as_str()) {
                    if let Some(pred) = &decl.predicate {
                        let mut env = vec![map
                            .iter()
                            .map(|(k, v)| (k.clone(), Slot::untyped(v.clone())))
                            .collect::<Frame>()];
                        match self.expr(pred, &mut env)? {
                            Val::Bool(true) => {}
                            Val::Bool(false) => {
                                return Err(crate::trap::validation(name, true).into())
                            }
                            other => {
                                return Err(format!(
                                    "cross-field predicate for `{name}` did not evaluate \
                                     to Bool (got {other:?})"
                                )
                                .into())
                            }
                        }
                    }
                }
                // A literal names its own type, and the checker types it as
                // exactly that name — so `User { .. }` is born stamped `User`
                // (RFC-0084 M1). This is a DEFAULT, not the rule: `coerce`
                // overwrites it at every typed boundary, which is what makes a
                // literal passed into a differently-named parameter of the same
                // shape dispatch as the parameter's type. Without it, an
                // unannotated `let u = User { .. }` — the one binding `coerce`
                // never sees — would carry no name while native dispatches on
                // the inferred one, and the engines would disagree.
                Ok(Val::Record(map, Some(std::rc::Rc::from(name.as_str()))))
            }
            // `xs.length` on a local Array or Map, read WITHOUT copying `xs` —
            // same reason as the index peephole above. A scan loop mentions it in
            // its condition on every iteration, so cloning the collection just to
            // ask how long it is costs as much as the scan itself. The guard
            // matches only when it will succeed, so nothing needs a fallback.
            Expr::Field { expr, field, .. }
                if field == "length" && matches!(&**expr, Expr::Var { .. }) && {
                    let Expr::Var { name: v, .. } = &**expr else {
                        unreachable!()
                    };
                    scope
                        .iter()
                        .rev()
                        .find_map(|f| f.get(v))
                        .is_some_and(|s| matches!(s.v, Val::Array(_) | Val::Map(_)))
                } =>
            {
                let Expr::Var { name: v, .. } = &**expr else {
                    unreachable!()
                };
                let slot = scope
                    .iter()
                    .rev()
                    .find_map(|f| f.get(v))
                    .expect("guarded above");
                Ok(match &slot.v {
                    Val::Array(items) => Val::Int(items.len() as i64),
                    Val::Map(pairs) => Val::Int(pairs.len() as i64),
                    _ => unreachable!("guarded above"),
                })
            }
            Expr::Field { expr, field, .. } => {
                let v = self.expr(expr, scope)?;
                match v {
                    // `arr.length` is the element count.
                    Val::Array(items) if field == "length" => Ok(Val::Int(items.len() as i64)),
                    // `map.length` is the entry count (RFC-0028).
                    Val::Map(pairs) if field == "length" => Ok(Val::Int(pairs.len() as i64)),
                    // `str.byteLength` is the O(1) byte length (RFC-0058; matches
                    // `strlen`). `.length` on a String is rejected by the checker.
                    Val::Str(s) if field == "byteLength" => Ok(Val::Int(s.len() as i64)),
                    Val::Record(map, _) => {
                        match map.get(field) {
                            // RFC-0085 M4a: reading a `lazy T` field FORCES it.
                            // Nothing is cached, so a second read runs it again
                            // (see the RFC's "M4a — as landed").
                            Some(Val::Fn(fv)) => match &**fv {
                                FnVal::Thunk(inner) => self.call_fnval(inner, &[]),
                                _ => Ok(Val::Fn(fv.clone())),
                            },
                            Some(v) => Ok(v.clone()),
                            None => Err(Ctrl::Err(format!("no field `{field}`"))),
                        }
                    }
                    other => Err(format!("field access on non-record {other:?}").into()),
                }
            }
            Expr::TryConstruct { name, args, .. } => {
                let v = self.expr(&args[0], scope)?;
                let decl = self
                    .types
                    .get(name.as_str())
                    .ok_or_else(|| Ctrl::Err(format!("unknown type `{name}`")))?;
                // Valid ⇒ Some(value); refinement fails ⇒ None (never aborts).
                if self.validates(decl, &v)? {
                    Ok(Val::Option(Some(Box::new(v))))
                } else {
                    Ok(Val::Option(None))
                }
            }
            Expr::ArrayLit { elems, .. } => {
                let mut vals = Vec::with_capacity(elems.len());
                for e in elems {
                    vals.push(self.expr(e, scope)?);
                }
                Ok(Val::Array(std::rc::Rc::new(vals)))
            }
            // A map literal (RFC-0028): evaluate entries in written order as
            // insertions — a repeated key updates in place (keeps its slot).
            Expr::MapLit { entries, .. } => {
                let mut pairs = MapVal::default();
                for (ke, ve) in entries {
                    let k = match self.expr(ke, scope)? {
                        Val::Str(s) => s,
                        other => {
                            return Err(
                                format!("a map key must be a String, found {other:?}").into()
                            )
                        }
                    };
                    let v = self.expr(ve, scope)?;
                    pairs.insert((*k).clone(), v);
                }
                Ok(Val::Map(pairs))
            }
            // A deterministic fork-join task: the callee is isolated (pure), so
            // running it eagerly here yields the same result any scheduler would.
            Expr::Spawn { name, args, .. } => {
                let mut vals = Vec::with_capacity(args.len());
                for a in args {
                    vals.push(self.expr(a, scope)?);
                }
                self.call(name, &vals)
            }
        }
    }

    /// Whether `v` satisfies `decl`'s refinement predicate (always true if none).
    ///
    /// The predicate is evaluated by the *runtime* evaluator with `value` bound
    /// — not by consteval — so every value kind the interpreter has (Float,
    /// sized ints, strings, indexing, `=~`) validates with exactly its runtime
    /// semantics, and a predicate that traps (division by zero) traps the same
    /// way an ordinary expression does.
    fn validates(&self, decl: &TypeDecl, v: &Val) -> Result<bool, Ctrl> {
        let pred = match &decl.predicate {
            None => return Ok(true),
            Some(p) => p,
        };
        let mut scope = vec![Frame::from_iter([(
            "value".to_string(),
            Slot::untyped(v.clone()),
        )])];
        match self.expr(pred, &mut scope)? {
            Val::Bool(b) => Ok(b),
            other => Err(format!(
                "refinement for `{}` did not evaluate to Bool (got {other:?})",
                decl.name
            )
            .into()),
        }
    }

    /// Evaluate a `match` over an Option or Result, binding the payload.
    fn eval_match(&self, sv: Val, arms: &[MatchArm], scope: &mut Vec<Frame>) -> Result<Val, Ctrl> {
        for arm in arms {
            let Some(bindings) = Self::match_pattern(&arm.pattern, &sv) else {
                continue;
            };
            scope.push(Frame::default());
            for (name, val) in bindings {
                scope.last_mut().unwrap().insert(name, Slot::untyped(val));
            }
            let result = self.expr(&arm.body, scope);
            scope.pop();
            return result;
        }
        Err("non-exhaustive match (should have been caught)".into())
    }

    /// Test a single refutable pattern against a value, returning its payload
    /// bindings on a match or `None` otherwise (RFC-0060). Shared by `match`,
    /// `if let`, and the `while let` desugar so all three bind identically.
    fn match_pattern(pattern: &Pattern, sv: &Val) -> Option<Vec<(String, Val)>> {
        match (pattern, sv) {
            (Pattern::Some(b), Val::Option(Some(v))) => Some(vec![(b.clone(), (**v).clone())]),
            (Pattern::None, Val::Option(None)) => Some(vec![]),
            (Pattern::Ok(b), Val::Result(true, v)) => Some(vec![(b.clone(), (**v).clone())]),
            (Pattern::Err(b), Val::Result(false, v)) => Some(vec![(b.clone(), (**v).clone())]),
            // The `??` desugar's type-agnostic pair (RFC-0079): tag first, sum
            // second, which is the whole point of having them.
            (Pattern::Success(b), Val::Option(Some(v)) | Val::Result(true, v)) => {
                Some(vec![(b.clone(), (**v).clone())])
            }
            (Pattern::Failure(b), Val::Result(false, v)) => Some(vec![(b.clone(), (**v).clone())]),
            (Pattern::Failure(_), Val::Option(None)) => Some(vec![]),
            (Pattern::Variant(n, binds), Val::Enum(vn, payload)) if n == vn => {
                Some(binds.iter().cloned().zip(payload.iter().cloned()).collect())
            }
            _ => None,
        }
    }

    fn binop(&self, op: BinOp, l: Val, r: Val) -> Result<Val, Ctrl> {
        use BinOp::*;
        // Lane-wise arithmetic (RFC-0083). Four independent `f32` operations in
        // written lane order; the checker admits only these ten operators.
        if let (Val::F32x4(a), Val::F32x4(b)) = (&l, &r) {
            // Comparison (M2) yields a mask, not a `Bool`. Rust's `<` on `f32` is
            // already IEEE's ORDERED comparison — false whenever either side is
            // NaN — and `!=` is the unordered one, true whenever either is. That
            // is `fcmp olt`/`fcmp une`, which is the pair RFC-0081 had to correct
            // at scalar width, so it is written down rather than assumed.
            if matches!(op, Lt | LtEq | Gt | GtEq | Eq | NotEq) {
                let mut m = [false; 4];
                for i in 0..4 {
                    m[i] = match op {
                        Lt => a[i] < b[i],
                        LtEq => a[i] <= b[i],
                        Gt => a[i] > b[i],
                        GtEq => a[i] >= b[i],
                        Eq => a[i] == b[i],
                        _ => a[i] != b[i],
                    };
                }
                return Ok(Val::Mask32x4(m));
            }
            let mut out = [0f32; 4];
            for i in 0..4 {
                out[i] = match op {
                    Add => a[i] + b[i],
                    Sub => a[i] - b[i],
                    Mul => a[i] * b[i],
                    Div => a[i] / b[i],
                    _ => return Err("type error in vector binop (should have been caught)".into()),
                };
            }
            return Ok(Val::F32x4(out));
        }
        // The wide float width (RFC-0083 M4): the same ten operators over two
        // `f64` lanes, with `/` kept — `f64x2.div` exists, which is what makes
        // this the float table rather than the integer one. The comparison
        // answers a `Mask64x2`, and that is the only line here that is not the
        // narrow width with a wider lane.
        if let (Val::F64x2(a), Val::F64x2(b)) = (&l, &r) {
            if matches!(op, Lt | LtEq | Gt | GtEq | Eq | NotEq) {
                let mut m = [false; 2];
                for i in 0..2 {
                    m[i] = match op {
                        Lt => a[i] < b[i],
                        LtEq => a[i] <= b[i],
                        Gt => a[i] > b[i],
                        GtEq => a[i] >= b[i],
                        Eq => a[i] == b[i],
                        _ => a[i] != b[i],
                    };
                }
                return Ok(Val::Mask64x2(m));
            }
            let mut out = [0f64; 2];
            for i in 0..2 {
                out[i] = match op {
                    Add => a[i] + b[i],
                    Sub => a[i] - b[i],
                    Mul => a[i] * b[i],
                    Div => a[i] / b[i],
                    _ => return Err("type error in vector binop (should have been caught)".into()),
                };
            }
            return Ok(Val::F64x2(out));
        }
        // Lane-wise integer arithmetic (RFC-0083 M3). Wrapping, which is the
        // language's overflow rule at every other width and also what `i32x4.add`
        // and `add <4 x i32>` do — a SATURATING add where the scalar wraps would
        // be a divergence, so the `wrapping_*` spelling is written out rather
        // than left to a release build's `+`. There is no `Div` arm: the checker
        // refuses `/` on this type, because no instruction exists to lower it to.
        if let (Val::I32x4(a), Val::I32x4(b)) = (&l, &r) {
            // SIGNED comparison, from the lane type. `i32x4.lt_u` is the operation
            // a `U32x4` would name, and the difference is visible exactly at
            // `Int32.min`, which reads as the largest value unsigned.
            if matches!(op, Lt | LtEq | Gt | GtEq | Eq | NotEq) {
                let mut m = [false; 4];
                for i in 0..4 {
                    m[i] = match op {
                        Lt => a[i] < b[i],
                        LtEq => a[i] <= b[i],
                        Gt => a[i] > b[i],
                        GtEq => a[i] >= b[i],
                        Eq => a[i] == b[i],
                        _ => a[i] != b[i],
                    };
                }
                return Ok(Val::Mask32x4(m));
            }
            let mut out = [0i32; 4];
            for i in 0..4 {
                out[i] = match op {
                    Add => a[i].wrapping_add(b[i]),
                    Sub => a[i].wrapping_sub(b[i]),
                    Mul => a[i].wrapping_mul(b[i]),
                    BitAnd => a[i] & b[i],
                    BitOr => a[i] | b[i],
                    BitXor => a[i] ^ b[i],
                    _ => return Err("type error in vector binop (should have been caught)".into()),
                };
            }
            return Ok(Val::I32x4(out));
        }
        // Combining masks (RFC-0083 M2). `&`/`|`/`^` and not `&&`/`||`, which are
        // the SHORT-CIRCUITING Bool operators — there is nothing to short-circuit
        // in four lanes computed at once, and the bitwise family makes no such
        // promise. Both backends emit `v128.and` / `and <4 x i32>`; this is the
        // reference answer they have to match.
        if let (Val::Mask32x4(a), Val::Mask32x4(b)) = (&l, &r) {
            let mut m = [false; 4];
            for i in 0..4 {
                m[i] = match op {
                    BitAnd => a[i] && b[i],
                    BitOr => a[i] || b[i],
                    BitXor => a[i] != b[i],
                    _ => return Err("type error in mask binop (should have been caught)".into()),
                };
            }
            return Ok(Val::Mask32x4(m));
        }
        // The two-lane mask, the same three combinators.
        if let (Val::Mask64x2(a), Val::Mask64x2(b)) = (&l, &r) {
            let mut m = [false; 2];
            for i in 0..2 {
                m[i] = match op {
                    BitAnd => a[i] && b[i],
                    BitOr => a[i] || b[i],
                    BitXor => a[i] != b[i],
                    _ => return Err("type error in mask binop (should have been caught)".into()),
                };
            }
            return Ok(Val::Mask64x2(m));
        }
        // Float32 (possibly with a plain-Float literal sibling): round both to f32
        // and compute at single precision, matching native `float` instructions.
        if matches!(l, Val::Float32(_)) || matches!(r, Val::Float32(_)) {
            let to_f32 = |v: &Val| -> Result<f32, Ctrl> {
                match v {
                    Val::Float32(f) => Ok(*f),
                    Val::Float(f) => Ok(*f as f32),
                    _ => Err("type error in Float32 binop".into()),
                }
            };
            let (a, b) = (to_f32(&l)?, to_f32(&r)?);
            return Ok(match op {
                Add => Val::Float32(a + b),
                Sub => Val::Float32(a - b),
                Mul => Val::Float32(a * b),
                Div => Val::Float32(a / b),
                Lt => Val::Bool(a < b),
                LtEq => Val::Bool(a <= b),
                Gt => Val::Bool(a > b),
                GtEq => Val::Bool(a >= b),
                Eq => Val::Bool(a == b),
                NotEq => Val::Bool(a != b),
                Rem | And | Or | Match | BitAnd | BitOr | BitXor | Shl | Shr => {
                    return Err("type error in float binop (should have been caught)".into())
                }
            });
        }
        // Sized integers (possibly with a plain-Int literal sibling): compute in
        // i64, then wrap arithmetic back to the operand width (matching native iN).
        if matches!(l, Val::IntN { .. }) || matches!(r, Val::IntN { .. }) {
            let (bits, signed) = match (&l, &r) {
                (Val::IntN { bits, signed, .. }, _) | (_, Val::IntN { bits, signed, .. }) => {
                    (*bits, *signed)
                }
                _ => unreachable!(),
            };
            // Wrap BOTH operands to the sized type first: a plain-`Int` literal
            // sibling (`x < 300` on a UInt8) must be truncated exactly as the
            // native backend's iN registers truncate it — comparing or dividing
            // by the raw i64 would give a different answer.
            let x = match l {
                Val::IntN { v, .. } => wrap_intn(v, bits, signed),
                Val::Int(n) => wrap_intn(n, bits, signed),
                _ => return Err("type error in sized-int binop".into()),
            };
            let y = match r {
                Val::IntN { v, .. } => wrap_intn(v, bits, signed),
                Val::Int(n) => wrap_intn(n, bits, signed),
                _ => return Err("type error in sized-int binop".into()),
            };
            let mk = |v: i64| Val::IntN {
                v: wrap_intn(v, bits, signed),
                bits,
                signed,
            };
            // The sized type's minimum, for the signed-overflow division trap
            // (MIN / -1 has no representable result; native sdiv traps on it).
            // Arithmetic shift sign-extends, so this is exact for bits = 8..64.
            let min_n: i64 = if signed { i64::MIN >> (64 - bits) } else { 0 };
            // Add/Sub/Mul are identical for signed/unsigned (two's complement);
            // Div/Rem and comparison differ — unsigned uses `u64` semantics.
            return Ok(match op {
                Add => mk(x.wrapping_add(y)),
                Sub => mk(x.wrapping_sub(y)),
                Mul => mk(x.wrapping_mul(y)),
                Div => {
                    if y == 0 {
                        return Err(crate::trap::DIV_ZERO.into());
                    }
                    if signed && x == min_n && y == -1 {
                        return Err(crate::trap::DIV_OVERFLOW.into());
                    }
                    mk(if signed {
                        x.wrapping_div(y)
                    } else {
                        (x as u64).wrapping_div(y as u64) as i64
                    })
                }
                Rem => {
                    if y == 0 {
                        return Err(crate::trap::REM_ZERO.into());
                    }
                    // `MIN % -1 == 0` (RFC-0060): NO trap, unlike `MIN / -1`.
                    // `wrapping_rem` yields 0 there; raw `%` would panic.
                    mk(if signed {
                        x.wrapping_rem(y)
                    } else {
                        (x as u64).wrapping_rem(y as u64) as i64
                    })
                }
                Lt => Val::Bool(if signed {
                    x < y
                } else {
                    (x as u64) < (y as u64)
                }),
                LtEq => Val::Bool(if signed {
                    x <= y
                } else {
                    (x as u64) <= (y as u64)
                }),
                Gt => Val::Bool(if signed {
                    x > y
                } else {
                    (x as u64) > (y as u64)
                }),
                GtEq => Val::Bool(if signed {
                    x >= y
                } else {
                    (x as u64) >= (y as u64)
                }),
                Eq => Val::Bool(x == y),
                NotEq => Val::Bool(x != y),
                // Bitwise (RFC-0045): and/or/xor on the wrapped operands;
                // shifts trap when the amount is out of range (`>= bits`, or
                // negative on a signed amount — both caught by the unsigned
                // `>= bits` test since a negative reads as a huge unsigned).
                BitAnd => mk(x & y),
                BitOr => mk(x | y),
                BitXor => mk(x ^ y),
                Shl => {
                    if y < 0 || y >= i64::from(bits) {
                        return Err(crate::trap::SHIFT_RANGE.into());
                    }
                    mk(x.wrapping_shl(y as u32))
                }
                Shr => {
                    if y < 0 || y >= i64::from(bits) {
                        return Err(crate::trap::SHIFT_RANGE.into());
                    }
                    // Signed `>>` is arithmetic (sign-extends); unsigned is
                    // logical (zero-fills). `x`/`y` are already width-wrapped:
                    // for an unsigned operand `x` is zero-extended into the i64,
                    // so `(x as u64) >> y` is the logical shift.
                    mk(if signed {
                        x >> y
                    } else {
                        ((x as u64) >> y) as i64
                    })
                }
                And | Or | Match => return Err("`&&`/`||` need Bool operands".into()),
            });
        }
        match (l, r) {
            (Val::Int(a), Val::Int(b)) => Ok(match op {
                // Wrapping two's complement — the language's defined overflow
                // semantics, matching native (and independent of the build
                // profile; bare `+` would panic in debug and wrap in release).
                Add => Val::Int(a.wrapping_add(b)),
                Sub => Val::Int(a.wrapping_sub(b)),
                Mul => Val::Int(a.wrapping_mul(b)),
                Div => {
                    if b == 0 {
                        return Err(crate::trap::DIV_ZERO.into());
                    }
                    if a == i64::MIN && b == -1 {
                        return Err(crate::trap::DIV_OVERFLOW.into());
                    }
                    Val::Int(a / b)
                }
                Rem => {
                    if b == 0 {
                        return Err(crate::trap::REM_ZERO.into());
                    }
                    // `MIN % -1 == 0` (RFC-0060): NO trap, unlike `MIN / -1`.
                    // `wrapping_rem` yields 0 there; raw `%` would panic on overflow.
                    Val::Int(a.wrapping_rem(b))
                }
                Lt => Val::Bool(a < b),
                LtEq => Val::Bool(a <= b),
                Gt => Val::Bool(a > b),
                GtEq => Val::Bool(a >= b),
                Eq => Val::Bool(a == b),
                NotEq => Val::Bool(a != b),
                // Bitwise on the literal `Int` (64-bit, signed): `>>` is
                // arithmetic. An amount outside 0..64 traps (RFC-0045).
                BitAnd => Val::Int(a & b),
                BitOr => Val::Int(a | b),
                BitXor => Val::Int(a ^ b),
                Shl => {
                    if b < 0 || b >= 64 {
                        return Err(crate::trap::SHIFT_RANGE.into());
                    }
                    Val::Int(a.wrapping_shl(b as u32))
                }
                Shr => {
                    if b < 0 || b >= 64 {
                        return Err(crate::trap::SHIFT_RANGE.into());
                    }
                    Val::Int(a >> b)
                }
                And | Or | Match => unreachable!("handled above"),
            }),
            (Val::Float(a), Val::Float(b)) => Ok(match op {
                Add => Val::Float(a + b),
                Sub => Val::Float(a - b),
                Mul => Val::Float(a * b),
                Div => Val::Float(a / b), // IEEE: /0.0 is inf/NaN, not a trap
                Lt => Val::Bool(a < b),
                LtEq => Val::Bool(a <= b),
                Gt => Val::Bool(a > b),
                GtEq => Val::Bool(a >= b),
                Eq => Val::Bool(a == b),
                NotEq => Val::Bool(a != b),
                Rem | And | Or | Match | BitAnd | BitOr | BitXor | Shl | Shr => {
                    return Err("type error in float binop (should have been caught)".into())
                }
            }),
            (Val::Bool(a), Val::Bool(b)) => match op {
                Eq => Ok(Val::Bool(a == b)),
                NotEq => Ok(Val::Bool(a != b)),
                _ => Err("type error in bool binop (should have been caught)".into()),
            },
            // `Code + Code` concatenates fragments, origins carried (RFC-0054).
            (Val::Code(mut a), Val::Code(b)) => match op {
                Add => {
                    a.extend(b);
                    Ok(Val::Code(a))
                }
                _ => Err("type error in Code binop (should have been caught)".into()),
            },
            (Val::Str(a), Val::Str(b)) => match op {
                // `a + b` concatenates (replacing `concat`) — a fresh String.
                Add => concat_str(a.as_str(), b.as_str()),
                Eq => Ok(Val::Bool(a == b)),
                NotEq => Ok(Val::Bool(a != b)),
                // Ordering is byte-wise lexicographic (UTF-8 byte order — Rust's
                // `str` `Ord` is exactly memcmp, so this matches the native shim).
                Lt => Ok(Val::Bool(a.as_bytes() < b.as_bytes())),
                LtEq => Ok(Val::Bool(a.as_bytes() <= b.as_bytes())),
                Gt => Ok(Val::Bool(a.as_bytes() > b.as_bytes())),
                GtEq => Ok(Val::Bool(a.as_bytes() >= b.as_bytes())),
                // `s =~ "pat"`: compile the (literal) pattern and full-match.
                Match => match crate::regex::compile(&b) {
                    Ok(dfa) => Ok(Val::Bool(dfa.matches(&a))),
                    Err(e) => Err(format!("invalid regex `{b}`: {e}").into()),
                },
                _ => Err("type error in string binop (should have been caught)".into()),
            },
            _ => Err("type error in binop (should have been caught)".into()),
        }
    }

    fn as_bool(&self, v: Val) -> Result<bool, Ctrl> {
        match v {
            Val::Bool(b) => Ok(b),
            other => Err(format!("expected Bool, found {other:?}").into()),
        }
    }

    /// The protocol-dispatch key for a runtime value (RFC-0002 §5): the scalar
    /// name for a scalar, the enum's name for an enum value, or the declared
    /// name a record was stamped with at its last typed boundary (RFC-0084 M1).
    ///
    /// Read this together with `ok_target` in the checker: it returns `None` for
    /// exactly the targets that one refuses.
    /// The `impl Show for T` this value renders through (RFC-0094 M3), or
    /// `None` where the language renders it itself.
    ///
    /// Dispatched on the runtime value's stamp, which is the route
    /// [`Interp::val_type_key`] exists for. The scalar guard is first, and it is
    /// what keeps an `impl Show for Int64` from redefining the digits of `7`.
    fn show_dispatch(&self, v: &Val) -> Option<String> {
        if matches!(
            v,
            Val::Int(_)
                | Val::IntN { .. }
                | Val::Float(_)
                | Val::Float32(_)
                | Val::Bool(_)
                | Val::Str(_)
        ) {
            return None;
        }
        crate::types::show_impl_by_key(self.impls, &self.val_type_key(v)?)
    }

    fn val_type_key(&self, v: &Val) -> Option<String> {
        match v {
            Val::Int(_) => Some("Int64".to_string()),
            Val::Bool(_) => Some("Bool".to_string()),
            Val::Str(_) => Some("String".to_string()),
            Val::Enum(variant, _) => self.variant_enum.get(variant).cloned(),
            // A generic impl keys on the constructor alone (RFC-0080 M1), which
            // is all a runtime value can offer: `Some(1)` and `Some("a")` are
            // the same `Val::Option`. The impl body is generic over the payload,
            // so the type arguments the key drops are ones it never needed.
            Val::Option(_) => Some("Option".to_string()),
            Val::Result(..) => Some("Result".to_string()),
            // A record answers from the name `coerce` stamped on it. An unstamped
            // record has not crossed a typed boundary, so there is no static type
            // to answer WITH — not an error about the record, just no key.
            Val::Record(_, name) => name.as_ref().map(|n| n.to_string()),
            _ => None,
        }
    }

    /// Convert a value to `ty` at a typed boundary (let/param/return/field/
    /// element/assign). A plain integer flowing into a sized-integer slot wraps
    /// to that width, matching the native backend's `iN` truncation; a float in
    /// a `Float32` slot rounds to single precision.
    ///
    /// This is also where **automatic validation** happens: a value entering a
    /// predicated named type runs its `where` predicate and traps with the
    /// canonical `validation failed for \`T\`` when it does not hold. The walk
    /// is exhaustive — record fields, Option/Result payloads, and array
    /// elements are coerced (and therefore validated) recursively.
    fn coerce(&self, v: Val, ty: &Type) -> Result<Val, Ctrl> {
        // A container whose element type can neither change a value nor reject
        // one is rebuilt for nothing, and every typed boundary rebuilds it
        // again: `rows[i][j] = v` on an `Array<Array<Int64>>` desugars to a
        // move-out and a move-back, so the field/element write-backs re-walked
        // the whole grid per store — 65,304 ms for a 400x400 fill against 99 ms
        // with this short-circuit (RFC-0082 M2, finding 3). Values are handed
        // back untouched, so this is not an optimization the semantics can see.
        if self.coercion_is_noop(ty, &v, 0) {
            return Ok(v);
        }
        match (ty, v) {
            (Type::IntN { bits, signed }, Val::Int(n)) => Ok(Val::IntN {
                v: wrap_intn(n, *bits, *signed),
                bits: *bits,
                signed: *signed,
            }),
            (Type::IntN { bits, signed }, Val::IntN { v, .. }) => Ok(Val::IntN {
                v: wrap_intn(v, *bits, *signed),
                bits: *bits,
                signed: *signed,
            }),
            // A float literal in a `Float32` slot rounds to single precision; an
            // already-f32 value stays put.
            (Type::Float32, Val::Float(f)) => Ok(Val::Float32(f as f32)),
            (Type::Named(n), v) => {
                let Some(decl) = self.types.get(n.as_str()) else {
                    return Ok(v);
                };
                // Coerce toward the base first (a record base coerces fields;
                // a scalar base wraps), then run the predicate on the result.
                let v = self.coerce(v, &decl.base)?;
                if let Some(pred) = &decl.predicate {
                    // A record base has a cross-field predicate (field names in
                    // scope); a scalar base binds `value`.
                    let holds = if matches!(decl.base, Type::Record(_)) {
                        match &v {
                            Val::Record(map, _) => {
                                let mut env = vec![map
                                    .iter()
                                    .map(|(k, v)| (k.clone(), Slot::untyped(v.clone())))
                                    .collect::<Frame>()];
                                match self.expr(pred, &mut env)? {
                                    Val::Bool(b) => b,
                                    other => {
                                        return Err(format!(
                                            "cross-field predicate for `{n}` did not \
                                             evaluate to Bool (got {other:?})"
                                        )
                                        .into())
                                    }
                                }
                            }
                            _ => true, // not a record value — nothing to check
                        }
                    } else {
                        self.validates(decl, &v)?
                    };
                    if !holds {
                        let msg = crate::trap::validation_of(decl);
                        return Err(msg.into());
                    }
                }
                // The typed boundary is where a record learns its name
                // (RFC-0084 M1). `n` is the STATIC type of the slot the value is
                // entering, which is the same thing `type_key` hands the two
                // compiled backends — so the interpreter's dispatch key is
                // derived from the static type, not read off the shape.
                if let Val::Record(map, _) = v {
                    return Ok(Val::Record(map, Some(std::rc::Rc::from(n.as_str()))));
                }
                Ok(v)
            }
            // The structural arm keeps whatever name the value already carried:
            // a bare `{ x: Int64 }` has no name to give, so nothing about the
            // value's identity changed by passing through it. The NAMED arm above
            // is where a name is assigned (RFC-0084 M1).
            (Type::Record(fields), Val::Record(mut map, name)) => {
                for f in fields {
                    if let Some(fv) = map.remove(&f.name) {
                        map.insert(f.name.clone(), self.coerce(fv, &f.ty)?);
                    }
                }
                Ok(Val::Record(map, name))
            }
            (Type::Option(inner), Val::Option(Some(p))) => {
                Ok(Val::Option(Some(Box::new(self.coerce(*p, inner)?))))
            }
            (Type::Result(tok, terr), Val::Result(is_ok, p)) => {
                let inner = if is_ok { tok } else { terr };
                Ok(Val::Result(is_ok, Box::new(self.coerce(*p, inner)?)))
            }
            (Type::Array(inner), Val::Array(items))
            | (Type::ArrayN(inner, _), Val::Array(items))
            | (Type::SmallArray(inner, _), Val::Array(items)) => {
                let mut out = Vec::with_capacity(items.len());
                for it in items.iter() {
                    out.push(self.coerce(it.clone(), inner)?);
                }
                Ok(Val::Array(std::rc::Rc::new(out)))
            }
            // A `Map<String, V>` coerces (and thus validates) every value into
            // `V` — the boundary re-validation for a predicated value type.
            (Type::Map(_, val), Val::Map(pairs)) => {
                let mut out = MapVal::default();
                for (k, v) in pairs.pairs {
                    out.insert(k, self.coerce(v, val)?);
                }
                Ok(Val::Map(out))
            }
            // A function value flowing into a stored fn-typed slot (RFC-0037):
            // adopt the slot's signature. A lambda evaluated bare (in a storage
            // position) snapshots its captures with blank types; the declared
            // `fn(P..) -> R` supplies the parameter coercions and the return
            // type its invocations must honor — exactly what a `fn`-typed
            // parameter position supplies in v1. A named source needs nothing
            // (its own signature coerces at the call boundary).
            // A function value flowing into a `lazy T` field (RFC-0085 M4a):
            // adopt the `fn() -> T` signature exactly as above, then TAG it, so
            // the read that forces it can tell it apart from an ordinary stored
            // fn-typed field (`std/ui`'s `Query { run: fn() -> T }` is one, and
            // reading THAT hands back the closure). Idempotent — a value already
            // tagged crosses this boundary again on every copy.
            (Type::Lazy(inner), Val::Fn(fv)) => {
                let inner = Type::Fn(Vec::new(), inner.clone());
                let bare = match *fv {
                    FnVal::Thunk(t) => Val::Fn(t),
                    other => Val::Fn(Box::new(other)),
                };
                match self.coerce(bare, &inner)? {
                    Val::Fn(fv) => Ok(Val::Fn(Box::new(FnVal::Thunk(fv)))),
                    other => Ok(other),
                }
            }
            (Type::Fn(ptys, ret), Val::Fn(fv)) => Ok(Val::Fn(Box::new(match *fv {
                FnVal::Lambda {
                    params,
                    body,
                    captures,
                    ..
                } => FnVal::Lambda {
                    params,
                    body,
                    captures,
                    param_tys: ptys.clone(),
                    ret: (**ret).clone(),
                },
                named => named,
            }))),
            (_, v) => Ok(v),
        }
    }

    /// The declared type of `name.field`, resolved the same way the checker's
    /// `Stmt::SetField` resolves it (the record binding's type, then that
    /// record's field). `None` when the binding has no remembered type — a
    /// record value is otherwise type-erased, so there is no other hook.
    fn field_ty(&self, name: &str, field: &str, scope: &[Frame]) -> Option<Type> {
        let ty = scope
            .iter()
            .rev()
            .find_map(|f| f.get(name).map(|s| s.ty.clone()))
            .unwrap_or_else(|| self.globals.borrow().get(name).and_then(|s| s.ty.clone()))?;
        crate::types::record_fields(&ty, &self.type_map)?
            .into_iter()
            .find(|f| f.name == field)
            .map(|f| f.ty)
    }

    /// Whether coercing INTO `ty` is the identity: no width to wrap to, no
    /// predicate to run, and nothing nested that has either. Conservative —
    /// anything unrecognized (a type transformer, a generic application, a
    /// `Param`) answers `false` and takes the ordinary walk, which is only
    /// slower, never wrong.
    ///
    /// `depth` guards a self-referential named type (`type Tree = { kids:
    /// Array<Tree> }`): the value walk in `coerce` terminates because values are
    /// finite, but a type walk does not. `crate::types::resolve` bounds itself
    /// the same way.
    /// [`Self::coercion_is_identity`] with the value in hand — the same question
    /// asked of a coercion that has one thing left to do.
    ///
    /// A named record type is never *type*-identity after RFC-0084 M1, because
    /// coercing into it stamps the name. But stamping a name a value already
    /// carries changes nothing, and that is the steady state: an `Array<Cell>`
    /// written one element at a time re-coerces the whole row per store (the
    /// `rows[i][j] = v` desugar's write-back), so answering this from the type
    /// alone put RFC-0082 M2's quadratic straight back — measured 76 -> 881 ms
    /// on 16,000 stores, and 3,539 at four times the row length, which is the
    /// same scaling with the row length that fix removed.
    ///
    /// So the array walk is skipped when every element is ALREADY stamped. That
    /// is O(n) reads and no allocation, against O(n) `HashMap` clones — and it
    /// short-circuits on the first element that is not, so a coercion that
    /// really has work to do pays one check.
    fn coercion_is_noop(&self, ty: &Type, v: &Val, depth: usize) -> bool {
        if self.coercion_is_identity(ty, depth) {
            return true;
        }
        if depth > 16 {
            return false;
        }
        let d = depth + 1;
        match (ty, v) {
            (Type::Named(_), Val::Record(_, Some(stamped))) => {
                self.stamp_only(ty, d).is_some_and(|n| **stamped == *n)
            }
            // A sized int ALREADY at this width and signedness, holding a value
            // that wrapping would not move, is nothing to do. The type is not the
            // identity (`IntN` wraps, which is why `coercion_is_identity` says
            // no), but this VALUE is already where wrapping would leave it, and
            // the guard proves that rather than assuming it.
            //
            // The cost this removes is not per int, it is per `Array<UInt8>`
            // BOUNDARY: without the arm every element answers `false`, so coerce
            // rebuilds the whole array — on each call, for every `std/jsonread` /
            // `std/scan` / `std/strings` function whose parameter is a byte array
            // (RFC-0014's `bytes(s)`). Measured on a 43 kB document through
            // `parseJson` under a `gen fn`: see RFC-0107's M2 section.
            (
                Type::IntN { bits, signed },
                Val::IntN {
                    v,
                    bits: b,
                    signed: s,
                },
            ) => bits == b && signed == s && wrap_intn(*v, *bits, *signed) == *v,
            (
                Type::Array(inner) | Type::ArrayN(inner, _) | Type::SmallArray(inner, _),
                Val::Array(items),
            ) => match self.stamp_only(inner, d) {
                // The element type is decided ONCE and the scan is then a name
                // compare per element. Asking the general question per element
                // costs two hashed `types` lookups each, which on a 1,600-element
                // row is most of the store.
                Some(n) => items
                    .iter()
                    .all(|it| matches!(it, Val::Record(_, Some(m)) if **m == *n)),
                None => items.iter().all(|it| self.coercion_is_noop(inner, it, d)),
            },
            (Type::Option(inner), Val::Option(Some(p))) => self.coercion_is_noop(inner, p, d),
            (Type::Result(ok, err), Val::Result(is_ok, p)) => {
                self.coercion_is_noop(if *is_ok { ok } else { err }, p, d)
            }
            (Type::Map(_, val), Val::Map(pairs)) => {
                pairs.iter().all(|(_, x)| self.coercion_is_noop(val, x, d))
            }
            (Type::Record(fields), Val::Record(map, _)) => fields.iter().all(|f| {
                map.get(&f.name)
                    .is_none_or(|x| self.coercion_is_noop(&f.ty, x, d))
            }),
            _ => false,
        }
    }

    /// The declared name of a record type that coercion has nothing to do to
    /// except stamp it (RFC-0084 M1) — no predicate to run, no field that could
    /// change. `None` for anything else, including a record type that really
    /// does have work to do.
    fn stamp_only(&self, ty: &Type, depth: usize) -> Option<&'a str> {
        let Type::Named(n) = ty else { return None };
        let decl = self.types.get(n.as_str())?;
        let ok = matches!(decl.base, Type::Record(_))
            && decl.predicate.is_none()
            && self.coercion_is_identity(&decl.base, depth);
        ok.then_some(decl.name.as_str())
    }

    fn coercion_is_identity(&self, ty: &Type, depth: usize) -> bool {
        if depth > 16 {
            return false;
        }
        let d = depth + 1;
        match ty {
            // `F32x4`/`I32x4`/`Mask32x4` are here for the reason the scalars are:
            // their lanes are already `f32`/`i32`/`bool`, so there is nothing to
            // round and nothing to validate (RFC-0083 — they are values, and a
            // value type carries no predicate).
            Type::Int
            | Type::Float
            | Type::Bool
            | Type::Str
            | Type::Unit
            | Type::F32x4
            | Type::I32x4
            | Type::F64x2
            | Type::Mask32x4
            | Type::Mask64x2 => true,
            Type::Named(n) => match self.types.get(n.as_str()) {
                // An unknown name coerces to itself in the walk below.
                None => true,
                // A named RECORD type is never the identity, whatever its fields
                // do: coercing into it stamps the name a protocol call dispatches
                // on (RFC-0084 M1). This is the narrowest place to say it — the
                // predicate already had the decl in hand, so it costs nothing
                // where it still answers `true`, and the cost where it now
                // answers `false` falls only on types that mention a named
                // record. The field walk underneath is unaffected: the base is
                // still a bare `Type::Record`, which short-circuits as before.
                Some(decl) => {
                    decl.predicate.is_none()
                        && !matches!(decl.base, Type::Record(_))
                        && self.coercion_is_identity(&decl.base, d)
                }
            },
            Type::Option(i) | Type::Array(i) | Type::ArrayN(i, _) | Type::SmallArray(i, _) => {
                self.coercion_is_identity(i, d)
            }
            Type::Result(ok, err) => {
                self.coercion_is_identity(ok, d) && self.coercion_is_identity(err, d)
            }
            Type::Map(_, val) => self.coercion_is_identity(val, d),
            Type::Record(fields) => fields.iter().all(|f| self.coercion_is_identity(&f.ty, d)),
            // `IntN` wraps, `Float32` rounds, `Fn` adopts a signature.
            _ => false,
        }
    }

    // ---- JSON codec (RFC-0018) ------------------------------------------
    // The reference implementation of `toJson`/`fromJson`. The native backend
    // (per-type generated IR + C runtime) must produce byte-identical output,
    // including every `Issue`'s key/path/message; the wording lives in
    // `crate::codec` so both sides read from one source.

    /// Best-effort static type of an expression, used by `toJson` to encode
    /// record fields in **declaration order** (a `Val::Record` is an unordered
    /// map). Covers the forms a codable value flows through: bindings/params,
    /// record literals, field access, `Some(..)`, indexing, numeric
    /// conversions, and user-function results.
    fn type_of(&self, e: &Expr, scope: &[Frame]) -> Option<Type> {
        match e {
            Expr::Var { name, .. } => {
                for frame in scope.iter().rev() {
                    if let Some(s) = frame.get(name) {
                        return s.ty.clone();
                    }
                }
                self.globals.borrow().get(name).and_then(|s| s.ty.clone())
            }
            Expr::StructLit { name, fields, .. } => {
                if !name.is_empty() {
                    return Some(Type::Named(name.clone()));
                }
                let mut fs = Vec::new();
                for (k, ve) in fields {
                    fs.push(Field {
                        name: k.clone(),
                        ty: self.type_of(ve, scope)?,
                    });
                }
                Some(Type::Record(fs))
            }
            Expr::Field { expr, field, .. } => {
                let pt = self.type_of(expr, scope)?;
                let fields = crate::types::record_fields(&pt, &self.type_map)?;
                // A read of a `lazy T` field is a `T` — it has already been
                // forced by the time anything asks what it is (RFC-0085 M4a).
                fields
                    .into_iter()
                    .find(|f| &f.name == field)
                    .map(|f| crate::types::forced(&f.ty))
            }
            Expr::Call { name, args, .. } => {
                if name == "Some" {
                    return Some(Type::Option(Box::new(self.type_of(args.first()?, scope)?)));
                }
                if name == crate::project::AT && args.len() == 2 {
                    let at = self.type_of(&args[0], scope)?;
                    return match crate::types::resolve(&at, &self.type_map) {
                        Type::Array(i) | Type::ArrayN(i, _) | Type::SmallArray(i, _) => Some(*i),
                        _ => None,
                    };
                }
                if let Some(t) = crate::types::numeric_conv_target(name) {
                    return Some(t);
                }
                // A `Result` constructor: the taken arm's payload type is what
                // encode needs; the other parameter is a placeholder (RFC-0024,
                // mirroring codegen's `Ok`/`Err` typing).
                if name == "Ok" {
                    return Some(Type::Result(
                        Box::new(self.type_of(args.first()?, scope)?),
                        Box::new(Type::Unit),
                    ));
                }
                if name == "Err" {
                    return Some(Type::Result(
                        Box::new(Type::Unit),
                        Box::new(self.type_of(args.first()?, scope)?),
                    ));
                }
                // An enum variant constructor (`Circle(2)`) resolves to its enum.
                if let Some(en) = self.enum_of_variant(name) {
                    return Some(Type::Named(en));
                }
                self.funcs.get(name.as_str()).map(|f| f.ret.clone())
            }
            Expr::TryConstruct { name, .. } => {
                Some(Type::Option(Box::new(Type::Named(name.clone()))))
            }
            _ => None,
        }
    }

    /// The name of the enum declaration that has a variant named `variant`, if
    /// any — used by `toJson` to recover an enum constructor's static type.
    fn enum_of_variant(&self, variant: &str) -> Option<String> {
        for (name, decl) in self.type_map.iter() {
            if let Type::Enum(vs) = &decl.base {
                if vs.iter().any(|v| v.name == variant) {
                    return Some(name.clone());
                }
            }
        }
        None
    }

    /// `unboxStream`/`pullAt` on an address that names no boxed stream (RFC-0090 M3).
    /// The compiled backends print this and exit 1; the wording is theirs.
    fn stream_int(v: &Val) -> Result<i64, Ctrl> {
        match v {
            Val::Int(n) => Ok(*n),
            other => Err(format!("a stream word that is {other:?}").into()),
        }
    }

    fn no_boxed_stream() -> Ctrl {
        Ctrl::Err(crate::trap::NO_STREAM.into())
    }

    /// One element from a stream, advancing it (RFC-0075 M2b). `None` ends the
    /// loop; a stepped stream that has ended is never stepped again.
    fn stream_next(&self, s: &mut StreamVal) -> Result<Option<Val>, Ctrl> {
        match s {
            StreamVal::Buf(items, i) => {
                let v = items.get(*i).cloned();
                if v.is_some() {
                    *i += 1;
                }
                Ok(v)
            }
            StreamVal::Step {
                slot,
                gen,
                step,
                done,
            } => {
                if *done {
                    return Ok(None);
                }
                let cursor = [Val::Int(*slot), Val::Int(*gen), Val::Bool(false)];
                match self.call_fnval(step, &cursor)? {
                    Val::Option(Some(v)) => Ok(Some(*v)),
                    Val::Option(None) => {
                        *done = true;
                        Ok(None)
                    }
                    other => Err(format!("a stream step answered {other:?}").into()),
                }
            }
        }
    }

    /// A stream's release path (RFC-0075, re-hosted by RFC-0090 M3): a buffer
    /// has nothing the host does not reclaim anyway, a producer owns a cursor
    /// slot in a slab that now lives in `std/stream`. One function so
    /// `for … in`, `close` and the host's disconnect (RFC-0074 M3a) run the same
    /// one rather than three that agree.
    ///
    /// A producer releases itself. The slab is Vyrn and a release is type-erased
    /// here, so the step is asked to do it: it is called once with `closing`
    /// true, gives its slot back, and answers `None`. A wrapper's step closes
    /// its own source in the same call, which is why the walk M2c wrote as a
    /// loop over a chain is now ordinary Vyrn recursion — and why `movecheck`
    /// checks it.
    fn release_stream(&self, s: &StreamVal) -> Result<(), Ctrl> {
        let StreamVal::Step {
            slot, gen, step, ..
        } = s
        else {
            return Ok(());
        };
        let closing = [Val::Int(*slot), Val::Int(*gen), Val::Bool(true)];
        self.call_fnval(step, &closing)?;
        Ok(())
    }

    /// `for x in <stream>` — the pull loop. The stream is a local here, so the
    /// release below runs on every way out, including the `?` on a trapping body.
    fn for_stream(
        &self,
        mut s: StreamVal,
        var: &str,
        body: &Block,
        scope: &mut Vec<Frame>,
    ) -> Result<Flow, Ctrl> {
        let release = |s: &StreamVal| -> Result<(), Ctrl> { self.release_stream(s) };
        loop {
            let item = match self.stream_next(&mut s) {
                Ok(Some(v)) => v,
                Ok(None) => break,
                Err(e) => {
                    release(&s)?;
                    return Err(e);
                }
            };
            scope.push(Frame::default());
            scope
                .last_mut()
                .unwrap()
                .insert(var.to_string(), Slot::untyped(item));
            let flow = self.block(body, scope);
            scope.pop();
            match flow {
                Ok(Flow::Return(v)) => {
                    release(&s)?;
                    return Ok(Flow::Return(v));
                }
                Ok(Flow::Break) => break,
                Ok(Flow::Continue | Flow::Normal) => {}
                Err(e) => {
                    release(&s)?;
                    return Err(e);
                }
            }
        }
        release(&s)?;
        Ok(Flow::Normal)
    }
}

#[cfg(test)]
mod tests {
    use super::{
        code_splice, lex_tokens, render_code, reserve_str, reserve_vec, CodePiece, Ctrl, Val,
    };
    use crate::run;

    /// Run a single-source program that uses `toJson`, with `std/json` reachable.
    ///
    /// Since RFC-0078 M2b, `toJson`'s serializer IS that module's `emit`: the
    /// builtin is a type-directed walk plus a call into Vyrn, and the module enters
    /// the link by injection. `crate::run` takes a bare source with no resolver, so
    /// it has no runtime library to inject — these tests go through the loader, with
    /// the real `std/json` text so nothing here can drift from what ships.
    /// RFC-0078 M4c widened it: `std/codecs`, `std/text` and `std/strpred` are
    /// injected the same way for the thirteen builtins they implement, so every
    /// runtime module is mapped here rather than one per helper.
    fn run_json(src: &str) -> Result<i64, String> {
        let files = crate::loader::MapResolver(
            [
                (
                    "std/json.vyrn".to_string(),
                    include_str!("../../../std/json.vyrn").to_string(),
                ),
                (
                    "std/codecs.vyrn".to_string(),
                    include_str!("../../../std/codecs.vyrn").to_string(),
                ),
                (
                    "std/text.vyrn".to_string(),
                    include_str!("../../../std/text.vyrn").to_string(),
                ),
                (
                    "std/strpred.vyrn".to_string(),
                    include_str!("../../../std/strpred.vyrn").to_string(),
                ),
                // RFC-0078 M3: `fromJson`'s untyped half, and the two modules it
                // stands on. `std/jsondec` imports `std/jsonread` and `std/num`, and
                // the loader resolves those like any other import, so all three have
                // to be here even though only one is injected by name.
                (
                    "std/jsondec.vyrn".to_string(),
                    include_str!("../../../std/jsondec.vyrn").to_string(),
                ),
                (
                    "std/jsonread.vyrn".to_string(),
                    include_str!("../../../std/jsonread.vyrn").to_string(),
                ),
                (
                    "std/num.vyrn".to_string(),
                    include_str!("../../../std/num.vyrn").to_string(),
                ),
                // RFC-0078 M4b(2) follow-on: `jsonread`'s duplicate-key set
                // hashes keys, so `std/hash` is part of the reader's closure.
                (
                    "std/hash.vyrn".to_string(),
                    include_str!("../../../std/hash.vyrn").to_string(),
                ),
            ]
            .into_iter()
            .collect(),
        );
        let opts = crate::loader::LoadOptions {
            std_root: Some("std".into()),
            ..Default::default()
        };
        let program = crate::load(src, "main.vyrn", &opts, &files).map_err(|ds| {
            ds.iter().map(|d| d.render()).collect::<Vec<_>>().join(
                "
",
            )
        })?;
        crate::interp::run(&program)
    }

    #[test]
    fn arithmetic_and_return() {
        assert_eq!(run("fn main() -> Int64 { return 2 + 3 * 4; }").unwrap(), 14);
    }

    // ---- payload enums / Result round-trip (RFC-0024) -------------------

    /// `fromJson(T, toJson(x)) == Valid(x)` over the new domain: a payload enum
    /// (single/tuple/nullary) and a `Result`, both nested through a record, an
    /// array, and an `Option`. Returns 0 only when every arm round-trips.
    #[test]
    fn payload_codec_round_trip_law() {
        let src = "type Shape = | Circle(Int64) | Rect(Int64, Int64) | Nothing \
                   type Box = { s: Shape, r: Result<Int64, String>, \
                                tags: Array<Shape>, opt: Option<Shape> } \
                   fn cmp(enc: String, b: Box) -> Int64 { \
                       if toJson(b) == enc { return 0 } \
                       return 1 \
                   } \
                   fn same(a: Box) -> Int64 { \
                       let enc = toJson(a) \
                       return match fromJson(Box, enc) { \
                           Valid(b) => cmp(enc, b), \
                           Invalid(is) => 2, \
                       } \
                   } \
                   fn main() -> Int64 { \
                       let ok = Box { s: Rect(3, 4), r: Ok(9), \
                                      tags: [Circle(1), Nothing], opt: Some(Rect(2, 2)) } \
                       let err = Box { s: Nothing, r: Err(\"boom\"), tags: [], opt: None } \
                       return same(ok) + same(err) \
                   }";
        assert_eq!(run_json(src).unwrap(), 0);
    }

    // ---- testing (RFC-0015) ---------------------------------------------

    #[test]
    fn run_tests_reports_pass_fail_and_trap_messages() {
        let src = "test \"passes\" { assert(1 + 1 == 2) }\n\
                   test \"fails assert\" { assert(1 == 2) }\n\
                   test \"fails eq\" { assertEq(3 + 4, 8) }\n";
        let program = crate::check(src).unwrap();
        let mut results: Vec<(String, Result<(), String>)> = Vec::new();
        let (passed, failed) = super::run_tests(&program, None, |name, r| {
            results.push((name.to_string(), r.clone()));
        })
        .unwrap();
        assert_eq!((passed, failed), (1, 2));
        assert_eq!(results[0].0, "passes");
        assert!(results[0].1.is_ok());
        assert_eq!(
            results[1].1.as_ref().unwrap_err(),
            "assertion failed at line 2"
        );
        assert_eq!(
            results[2].1.as_ref().unwrap_err(),
            "assertion failed at line 3: 7 != 8"
        );
    }

    #[test]
    fn run_tests_name_filter() {
        let src = "test \"alpha\" { assert(true) }\n\
                   test \"beta\" { assert(true) }\n";
        let program = crate::check(src).unwrap();
        let mut names = Vec::new();
        let (passed, failed) = super::run_tests(&program, Some("alph"), |name, _| {
            names.push(name.to_string())
        })
        .unwrap();
        assert_eq!((passed, failed), (1, 0));
        assert_eq!(names, vec!["alpha".to_string()]);
    }

    // ---- input I/O (RFC-0014) ---------------------------------------------
    // `readLine` streams real stdin, so it is exercised by the parity harness's
    // `.stdin` fixtures (examples/input.vyrn) rather than unit-mocked here;
    // these cover the file and byte builtins, whose errors must be the
    // CANONICAL wording (never `io::Error` text).

    /// A unique temp path (forward slashes, so it can embed in Vyrn source).
    fn temp_path(tag: &str) -> String {
        let p = std::env::temp_dir().join(format!("vyrn-io-test-{tag}-{}.txt", std::process::id()));
        p.to_string_lossy().replace('\\', "/")
    }

    #[test]
    fn write_then_read_file_roundtrip() {
        let path = temp_path("roundtrip");
        let src = format!(
            "fn main() -> Int64 {{ \
                 let w = writeFile(\"{path}\", \"alpha\\nbeta\") \
                 let ok = match w {{ Ok(b) => b, Err(e) => false }} \
                 if ok == false {{ return 1 }} \
                 let r = readFile(\"{path}\") \
                 return match r {{ \
                     Ok(s) => s.byteLength, \
                     Err(e) => 2, \
                 }} }}"
        );
        // "alpha\nbeta" is 10 bytes.
        assert_eq!(run(&src).unwrap(), 10);
        let _ = std::fs::remove_file(path.as_str());
    }

    #[test]
    fn read_file_missing_yields_canonical_err() {
        let src = "fn main() -> Int64 { \
                       let r = readFile(\"vyrn-io-test-definitely-missing.txt\") \
                       let msg = match r { Ok(s) => s, Err(e) => e } \
                       if msg == \"cannot read `vyrn-io-test-definitely-missing.txt`\" { \
                           return 1 } \
                       return 0 }";
        assert_eq!(run(src).unwrap(), 1);
    }

    // ---- crash-safe persistence (RFC-0044) --------------------------------

    /// `renameFile` atomically overwrites an existing target and consumes the
    /// source — after it, the target holds the new content and no source (or
    /// `.tmp`) remains.
    #[test]
    fn rfc0044_rename_file_over_existing_replaces() {
        let dst = temp_path("rn-dst");
        let src_path = temp_path("rn-src");
        std::fs::write(&dst, "OLDOLD").unwrap();
        std::fs::write(&src_path, "NEW").unwrap();
        let src = format!(
            "fn main() -> Int64 {{ \
                 return match renameFile(\"{src_path}\", \"{dst}\") {{ \
                     Ok(b) => 1, Err(e) => 0 }} }}"
        );
        assert_eq!(run(&src).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(&dst).unwrap(), "NEW");
        assert!(!std::path::Path::new(&src_path).exists());
        let _ = std::fs::remove_file(&dst);
    }

    /// The atomic-write algorithm (`writeAtomic` = `std/storage`): write `<path>.tmp`
    /// then rename it over `path`. A successful write replaces the target and
    /// leaves NO `.tmp` sibling. Uses the same body the std module ships.
    #[test]
    fn rfc0044_write_atomic_replaces_and_leaves_no_tmp() {
        let path = temp_path("wa-ok");
        std::fs::write(path.as_str(), "OLD").unwrap();
        let src = format!(
            "fn writeAtomic(path: String, content: String) -> Result<Bool, String> {{ \
                 let tmp = \"\\{{path}}.tmp\" \
                 return match writeFile(tmp, content) {{ \
                     Ok(d) => renameFile(tmp, path), Err(w) => Err(w) }} }} \
             fn main() -> Int64 {{ \
                 return match writeAtomic(\"{path}\", \"BRANDNEW\") {{ \
                     Ok(b) => 1, Err(e) => 0 }} }}"
        );
        assert_eq!(run(&src).unwrap(), 1);
        assert_eq!(std::fs::read_to_string(path.as_str()).unwrap(), "BRANDNEW");
        assert!(!std::path::Path::new(&format!("{path}.tmp")).exists());
        let _ = std::fs::remove_file(path.as_str());
    }

    /// THE atomicity proof: when the temp write FAILS, `writeAtomic` never touches
    /// `path`, so the original target is byte-for-byte unchanged (the tear a bare
    /// `writeFile` would cause is gone). The temp write is forced to fail by making
    /// `<path>.tmp` a directory — `writeFile` cannot open it as a file.
    #[test]
    fn rfc0044_write_atomic_failed_temp_leaves_target_unchanged() {
        let path = temp_path("wa-tear");
        let tmp = format!("{path}.tmp");
        let _ = std::fs::remove_dir_all(&tmp);
        std::fs::write(path.as_str(), "ORIGINAL").unwrap();
        std::fs::create_dir(&tmp).unwrap(); // writeFile("<path>.tmp") now fails
        let src = format!(
            "fn writeAtomic(path: String, content: String) -> Result<Bool, String> {{ \
                 let tmp = \"\\{{path}}.tmp\" \
                 return match writeFile(tmp, content) {{ \
                     Ok(d) => renameFile(tmp, path), Err(w) => Err(w) }} }} \
             fn main() -> Int64 {{ \
                 return match writeAtomic(\"{path}\", \"CLOBBERED\") {{ \
                     Ok(b) => 1, Err(e) => 0 }} }}"
        );
        // The write reports failure...
        assert_eq!(run(&src).unwrap(), 0);
        // ...and — the point — the original target is untouched.
        assert_eq!(std::fs::read_to_string(path.as_str()).unwrap(), "ORIGINAL");
        let _ = std::fs::remove_dir_all(&tmp);
        let _ = std::fs::remove_file(path.as_str());
    }

    /// `load(TypeName, path)` distinguishes the three honest outcomes: a missing
    /// file is `Missing`, a garbage file is `Corrupt`, a good file is `Loaded`.
    /// Encoded as 1 / 2 / 100+value so one run proves all three.
    #[test]
    fn rfc0044_load_three_outcomes() {
        let good = temp_path("ld-good");
        let bad = temp_path("ld-bad");
        std::fs::write(&good, "{\"n\": 41}").unwrap();
        std::fs::write(&bad, "{ not json ]").unwrap();
        let missing = temp_path("ld-missing");
        let _ = std::fs::remove_file(&missing);
        let outcome = |p: &str| {
            format!(
                "match load(Rec, \"{p}\") {{ \
                     Missing => 1, Corrupt(iss) => 2, Loaded(r) => 100 + r.n }}"
            )
        };
        let src = format!(
            "type Rec = {{ n: Int64 }} \
             fn main() -> Int64 {{ \
                 let m = {} \
                 let c = {} \
                 let g = {} \
                 return m * 1000000 + c * 1000 + g }}",
            outcome(&missing),
            outcome(&bad),
            outcome(&good),
        );
        // Missing=1, Corrupt=2, Loaded(41)=141.
        assert_eq!(run_json(&src).unwrap(), 1_002_141);
        let _ = std::fs::remove_file(&good);
        let _ = std::fs::remove_file(&bad);
    }

    /// `loadOr(TypeName, path, default)` returns the default for a missing OR a
    /// corrupt file, and the decoded value for a good one.
    #[test]
    fn rfc0044_load_or_defaults() {
        let good = temp_path("lo-good");
        let bad = temp_path("lo-bad");
        let missing = temp_path("lo-missing");
        std::fs::write(&good, "{\"n\": 7}").unwrap();
        std::fs::write(&bad, "nonsense").unwrap();
        let _ = std::fs::remove_file(&missing);
        let src = format!(
            "type Rec = {{ n: Int64 }} \
             fn main() -> Int64 {{ \
                 let d = Rec {{ n: 9 }} \
                 let g = loadOr(Rec, \"{good}\", d).n \
                 let m = loadOr(Rec, \"{missing}\", d).n \
                 let c = loadOr(Rec, \"{bad}\", d).n \
                 return g * 100 + m * 10 + c }}"
        );
        // good=7, missing=9(default), corrupt=9(default) -> 799.
        assert_eq!(run_json(&src).unwrap(), 799);
        let _ = std::fs::remove_file(&good);
        let _ = std::fs::remove_file(&bad);
    }

    /// `fsyncFile` succeeds on an existing file and errors (canonically) on a
    /// missing one — the durability step never silently no-ops a bad path.
    #[test]
    fn rfc0044_fsync_file_ok_and_missing() {
        let path = temp_path("fs-ok");
        std::fs::write(path.as_str(), "durable").unwrap();
        let src = format!(
            "fn main() -> Int64 {{ \
                 let a = match fsyncFile(\"{path}\") {{ Ok(b) => 1, Err(e) => 0 }} \
                 let b = match fsyncFile(\"{path}-nope\") {{ Ok(x) => 0, Err(e) => 1 }} \
                 return a * 10 + b }}"
        );
        assert_eq!(run(&src).unwrap(), 11);
        let _ = std::fs::remove_file(path.as_str());
    }

    #[test]
    fn read_file_rejects_invalid_utf8_and_nul_canonically() {
        let bad = temp_path("badutf8");
        let nul = temp_path("nul");
        std::fs::write(&bad, [0x63u8, 0xE9, 0x21]).unwrap();
        std::fs::write(&nul, [0x61u8, 0x00, 0x62]).unwrap();
        let src = format!(
            "fn msgOf(r: Result<String, String>) -> String {{ \
                 return match r {{ Ok(s) => \"ok\", Err(e) => e.copy() }} }} \
             fn main() -> Int64 {{ \
                 let a = msgOf(readFile(\"{bad}\")) \
                 let b = msgOf(readFile(\"{nul}\")) \
                 if a != \"`{bad}` is not valid UTF-8\" {{ return 1 }} \
                 if b != \"`{nul}` contains a NUL byte\" {{ return 2 }} \
                 return 0 }}"
        );
        assert_eq!(run(&src).unwrap(), 0);
        let _ = std::fs::remove_file(&bad);
        let _ = std::fs::remove_file(&nul);
    }

    #[test]
    fn string_bytes_roundtrip_is_pinned() {
        // RFC-0014 M2's pinned law: stringFromBytes(s.bytes()) == Ok(s).
        let src = "fn main() -> Int64 { \
                       let s = \"héllo ☕ wörld\" \
                       let back = match stringFromBytes(bytes(s)) { \
                           Ok(t) => t, \
                           Err(e) => e, \
                       } \
                       if back == s { return 1 } \
                       return 0 }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn string_from_bytes_rejects_invalid_utf8() {
        // 0xFF is never valid UTF-8. Build it via an Array<UInt8> literal.
        let src = "fn main() -> Int64 { \
                       let b: Array<UInt8> = [104, 255] \
                       let msg = match stringFromBytes(b) { Ok(s) => s, Err(e) => e } \
                       if msg == \"bytes are not valid UTF-8\" { return 1 } \
                       return 0 }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn string_from_bytes_rejects_nul() {
        let src = "fn main() -> Int64 { \
                       let b: Array<UInt8> = [104, 0, 105] \
                       let msg = match stringFromBytes(b) { Ok(s) => s, Err(e) => e } \
                       if msg == \"bytes contain a NUL byte\" { return 1 } \
                       return 0 }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn read_file_bytes_reads_binary() {
        let path = temp_path("binary");
        std::fs::write(path.as_str(), [0u8, 1, 2, 0xFF, 0]).unwrap();
        let src = format!(
            "fn main() -> Int64 {{ \
                 return match readFileBytes(\"{path}\") {{ \
                     Ok(b) => b.length, \
                     Err(e) => -1, \
                 }} }}"
        );
        // Binary read: NUL and invalid-UTF-8 bytes are fine, all 5 come back.
        assert_eq!(run(&src).unwrap(), 5);
        let _ = std::fs::remove_file(path.as_str());
    }

    #[test]
    fn args_default_to_empty() {
        // `run` (no args) must present an empty argv[1..] — the parity harness
        // runs every example argument-less on all three backends.
        assert_eq!(
            run("fn main() -> Int64 { return args().length }").unwrap(),
            0
        );
    }

    #[test]
    fn functions_and_recursion() {
        let src = "
            fn fib(n: Int64) -> Int64 {
                if n < 2 { return n; }
                return fib(n - 1) + fib(n - 2);
            }
            fn main() -> Int64 { return fib(10); }
        ";
        assert_eq!(run(src).unwrap(), 55);
    }

    #[test]
    fn option_and_match() {
        let src = "
            fn sd(a: Int64, b: Int64) -> Option<Int64> {
                if b == 0 { return None; }
                return Some(a / b);
            }
            fn uw(o: Option<Int64>, f: Int64) -> Int64 {
                return match o { Some(x) => x, None => f };
            }
            fn main() -> Int64 { return uw(sd(10, 2), 0) + uw(sd(1, 0), 100); }
        ";
        assert_eq!(run(src).unwrap(), 105); // 5 + 100
    }

    #[test]
    fn result_and_question_mark() {
        // `?` propagates Err out of `chain`, so chain(0) returns Err(-1) and the
        // final match yields the fallback.
        let src = "
            fn checked(n: Int64) -> Result<Int64, Int64> {
                if n == 0 { return Err(0 - 1); }
                return Ok(n);
            }
            fn chain(n: Int64) -> Result<Int64, Int64> {
                let x = checked(n)?;      // early-returns Err when n == 0
                return Ok(x + 1);
            }
            fn main() -> Int64 {
                let a = match chain(5) { Ok(v) => v, Err(e) => e };   // 6
                let b = match chain(0) { Ok(v) => v, Err(e) => e };   // -1
                return a + b;             // 5
            }
        ";
        assert_eq!(run(src).unwrap(), 5);
    }

    #[test]
    fn str_and_parse_roundtrip() {
        let src = "fn main() -> Int64 { \
                       let s = (0 - 123).toString(); \
                       return match parse(s) { Some(n) => n, None => 0 }; }";
        assert_eq!(run(src).unwrap(), -123);
    }

    #[test]
    fn parse_rejects_non_integers() {
        let cases = [
            ("\"12x\"", -1),
            ("\"\"", -1),
            ("\"-\"", -1),
            ("\" 5\"", -1),
            ("\"42\"", 42),
        ];
        for (lit, want) in cases {
            let src = format!(
                "fn main() -> Int64 {{ return match parse({lit}) {{ Some(n) => n, None => 0 - 1 }}; }}"
            );
            assert_eq!(run(&src).unwrap(), want, "parse({lit})");
        }
    }

    #[test]
    fn result_holds_non_int_payloads() {
        // Ok carries an Array, Err carries a String — neither rides in the word.
        let src = "
            fn lookup(k: Int64) -> Result<Array<Int64>, String> {
                if k == 0 { return Err(\"nope\"); }
                let mut a: Array<Int64> = []; a.push(k * 10); return Ok(a);
            }
            fn main() -> Int64 {
                let a = match lookup(5) { Ok(r) => r[0], Err(e) => 0 - e.byteLength };
                let b = match lookup(0) { Ok(r) => r[0], Err(e) => 0 - e.byteLength };
                return a + b;  // 50 + (-4)
            }
        ";
        assert_eq!(run(src).unwrap(), 46);
    }

    #[test]
    fn fixed_array_literal_and_index() {
        let src = "fn main() -> Int64 { let a: Array<Int64, 4> = [10, 20, 30, 40]; \
                   let mut s = 0; let mut i = 0; \
                   while i < a.length { s = s + a[i]; i = i + 1; } return s; }";
        assert_eq!(run(src).unwrap(), 100);
    }

    #[test]
    fn fixed_array_out_of_bounds_errors() {
        let src = "fn main() -> Int64 { let a: Array<Int64, 2> = [1, 2]; return a[4]; }";
        assert!(run(src).unwrap_err().contains("out of bounds"));
    }

    #[test]
    fn growable_array_push_and_read() {
        let src = "fn main() -> Int64 { \
                       let mut a: Array<Int64> = []; \
                       let mut i = 0; \
                       while i < 6 { a.push(i * i); i = i + 1; } \
                       let mut s = 0; let mut j = 0; \
                       while j < a.length { s = s + a[j]; j = j + 1; } \
                       return s; }"; // 0+1+4+9+16+25 = 55
        assert_eq!(run(src).unwrap(), 55);
    }

    #[test]
    fn array_index_out_of_bounds_errors() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   a.push(1); return a[3]; }";
        assert!(run(src).unwrap_err().contains("out of bounds"));
    }

    #[test]
    fn for_over_fixed_array() {
        let src = "fn main() -> Int64 { let a: Array<Int64, 5> = [0, 1, 4, 9, 16]; \
                   let mut s = 0; for x in a { s = s + x; } return s; }";
        assert_eq!(run(src).unwrap(), 30);
    }

    #[test]
    fn for_over_growable_array() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   let mut i = 0; while i < 6 { a.push(i * i); i = i + 1; } \
                   let mut s = 0; for x in a { s = s + x; } return s; }"; // 0+1+4+9+16+25
        assert_eq!(run(src).unwrap(), 55);
    }

    #[test]
    fn for_over_empty_array_runs_zero_times() {
        let src = "fn main() -> Int64 { let a: Array<Int64> = []; \
                   let mut s = 7; for x in a { s = s + x; } return s; }";
        assert_eq!(run(src).unwrap(), 7);
    }

    #[test]
    fn for_loop_variable_is_scoped_to_body() {
        // `x` must not leak past the loop — referencing it after is unbound.
        let src = "fn main() -> Int64 { let a: Array<Int64, 2> = [1, 2]; \
                   for x in a { let y = x; } return x; }";
        assert!(run(src).is_err());
    }

    #[test]
    fn for_body_early_return() {
        // Returning from inside the loop stops iteration immediately.
        let src = "fn firstOver(a: Array<Int64, 4>, t: Int64) -> Int64 { \
                   for x in a { if x > t { return x; } } return 0 - 1; } \
                   fn main() -> Int64 { let a: Array<Int64, 4> = [3, 8, 1, 9]; \
                   return firstOver(a, 5); }"; // first element > 5 is 8
        assert_eq!(run(src).unwrap(), 8);
    }

    #[test]
    fn for_over_non_array_is_rejected() {
        let src = "fn main() -> Int64 { let n = 3; for x in n { } return 0; }";
        assert!(run(src).unwrap_err().contains("Array"));
    }

    #[test]
    fn method_index_and_length_surface() {
        // `[]` is a literal, `.length` is a field, and `.push` / `[i]` desugar
        // to the internal `@push` / `@at`. There is no other spelling.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   a.push(10); a.push(20); a.push(30); \
                   return a.length + a[0] + a[2]; }"; // 3 + 10 + 30
        assert_eq!(run(src).unwrap(), 43);
    }

    #[test]
    fn method_push_writes_back() {
        // `a.push(x);` as a statement mutates `a` in place (write-back).
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; \
                   let mut i = 0; while i < 5 { a.push(i); i = i + 1; } \
                   let mut s = 0; for x in a { s = s + x; } return s; }"; // 0+1+2+3+4
        assert_eq!(run(src).unwrap(), 10);
    }

    #[test]
    fn drop_then_use_is_a_compile_error() {
        // `drop` consumes: using the value afterward must be rejected.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = []; a.push(1); \
                   drop a; return a.length; }";
        assert!(run(src).is_err());
    }

    #[test]
    fn drop_of_non_heap_is_rejected() {
        let src = "fn main() -> Int64 { let n = 5; drop n; return 0; }";
        assert!(run(src).unwrap_err().contains("heap"));
    }

    #[test]
    fn string_interpolation_renders_scalars() {
        // `\{ }` holes render Int/Bool/String; literal braces are untouched. The
        // program returns the interpolated string's length so we can assert it.
        let src = "fn main() -> Int64 { let n = 42; let ok = true; \
                   let s = \"n=\\{n} ok=\\{ok} {lit}\"; return s.byteLength; }";
        // "n=42 ok=true {lit}" -> 18 characters
        assert_eq!(run(src).unwrap(), 18);
    }

    #[test]
    fn interpolation_evaluates_hole_expressions() {
        let src = "fn main() -> Int64 { let a = 3; let b = 4; \
                   let s = \"\\{a * b}\"; return s.byteLength; }"; // "12" -> len 2
        assert_eq!(run(src).unwrap(), 2);
    }

    #[test]
    fn str_renders_bool_and_string() {
        let src = "fn main() -> Int64 { let s = false.toString(); return s.byteLength; }"; // "false" -> 5
        assert_eq!(run(src).unwrap(), 5);
    }

    #[test]
    fn str_renders_sized_int() {
        // A signed Int32 renders by value; an unsigned UInt8 renders its magnitude.
        let s = "fn main() -> Int64 { let a: Int32 = 42; let b: UInt8 = 200; \
                 let s = \"\\{a}/\\{b + b}\"; return s.byteLength; }"; // "42/144" -> 6
        assert_eq!(run(s).unwrap(), 6);
    }

    #[test]
    fn str_renders_uint64_above_i64_max() {
        // The full 64-bit magnitude renders (not a signed reinterpretation).
        let s = "fn main() -> Int64 { let n: UInt64 = 10000000000000000000; \
                 let s = n.toString(); return s.byteLength; }"; // 20 digits
        assert_eq!(run(s).unwrap(), 20);
    }

    #[test]
    fn str_renders_float_to_six_decimals() {
        let s = "fn main() -> Int64 { let s = (3.14159).toString(); return s.byteLength; }"; // "3.141590" -> 8
        assert_eq!(run(s).unwrap(), 8);
    }

    #[test]
    fn float_arithmetic_and_comparison() {
        // 1.5 * 2.5 = 3.75 > 3.0 → 1
        let src = "fn main() -> Int64 { let a = 1.5; let b = 2.5; \
                   if a * b > 3.0 { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn float_through_function_and_negation() {
        let src = "fn half(x: Float64) -> Float64 { return x / 2.0; } \
                   fn main() -> Int64 { let h = half(5.0); \
                   if h == 2.5 { if -h < 0.0 { return 7; } } return 0; }";
        assert_eq!(run(src).unwrap(), 7);
    }

    #[test]
    fn float_to_int_truncates_toward_zero() {
        let src = "fn main() -> Int64 { let f = 3.9; return Int64(f); }";
        assert_eq!(run(src).unwrap(), 3);
    }

    #[test]
    fn int_to_float_and_back() {
        let src = "fn main() -> Int64 { let f = Float64(7); let g = f + 0.5; return Int64(g); }"; // 7.5 -> 7
        assert_eq!(run(src).unwrap(), 7);
    }

    #[test]
    fn float32_rounds_to_single_precision() {
        // 2^24 + 1 is exact in f64 but rounds to 2^24 in f32, so `Int(..)` differs.
        let f32 = "fn main() -> Int64 { let x: Float32 = 16777217.0; return Int64(x); }";
        assert_eq!(run(f32).unwrap(), 16777216);
        let f64 = "fn main() -> Int64 { let x: Float64 = 16777217.0; return Int64(x); }";
        assert_eq!(run(f64).unwrap(), 16777217);
    }

    #[test]
    fn float32_arithmetic_stays_single_precision() {
        // Adding 1.0 to 1e8 is below the f32 ULP → lost; f64 keeps it.
        let src = "fn addf(a: Float32, b: Float32) -> Float32 { return a + b; } \
                   fn main() -> Int64 { let g: Float32 = 100000000.0; return Int64(addf(g, 1.0)); }";
        assert_eq!(run(src).unwrap(), 100000000);
    }

    #[test]
    fn float32_widens_to_float64_exactly() {
        // 0.5 is exact in both; Float32 -> Float64 -> Int round-trips its value.
        let src = "fn main() -> Int64 { let x: Float32 = 2.5; let d = Float64(x); \
                   if d == 2.5 { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn float32_literal_adapts_to_sibling() {
        // A plain float literal takes the Float32 sibling's precision.
        let src = "fn main() -> Int64 { let h: Float32 = 1.5; let r = h + 2.5; return Int64(r); }";
        assert_eq!(run(src).unwrap(), 4);
    }

    #[test]
    fn int_to_int32_wraps_and_back() {
        // 5_000_000_000 wraps into i32 to 705032704; Int(..) sext's it back.
        let src = "fn main() -> Int64 { let big = 5000000000; return Int64(Int32(big)); }";
        assert_eq!(run(src).unwrap(), 705032704);
    }

    #[test]
    fn int8_conversion_wraps() {
        let src = "fn main() -> Int64 { return Int64(Int8(300)); }"; // 300 & 0xFF as i8 = 44
        assert_eq!(run(src).unwrap(), 44);
    }

    #[test]
    fn rejects_conversion_of_non_number() {
        let src = "fn main() -> Int64 { let x = Int64(\"hi\"); return 0; }";
        assert!(run(src).unwrap_err().contains("converts a number"));
    }

    #[test]
    fn int64_is_an_alias_for_int() {
        let src = "fn f(n: Int64) -> Int64 { return n + 1; } \
                   fn main() -> Int64 { let x: Int64 = 41; return f(x); }";
        assert_eq!(run(src).unwrap(), 42);
    }

    #[test]
    fn rejects_int_float_mixing() {
        let src = "fn main() -> Int64 { let a = 1 + 2.0; return 0; }";
        assert!(run(src).unwrap_err().contains("matching numeric"));
    }

    #[test]
    fn rejects_float_assigned_to_int() {
        let src = "fn main() -> Int64 { let x: Int64 = 1.5; return x; }";
        assert!(run(src).is_err());
    }

    #[test]
    fn int32_overflow_wraps() {
        // 2e9 + 2e9 = 4e9 wraps at 32 bits to -294967296.
        let src = "fn main() -> Int64 { let a: Int32 = 2000000000; let b: Int32 = 2000000000; \
                   let c = a + b; if c < 0 { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn int8_wraps_at_eight_bits() {
        // 100 + 100 = 200 wraps at 8 bits (signed) to -56.
        let src = "fn wrap(a: Int8, b: Int8) -> Int8 { return a + b; } \
                   fn main() -> Int64 { let x: Int8 = 100; let r = wrap(x, x); \
                   if r < 0 { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn uint8_wraps_into_magnitude_range() {
        // 200 + 200 = 400 wraps at 8 bits (unsigned) to 144 — stays non-negative.
        let src = "fn main() -> Int64 { let x: UInt8 = 200; let r = x + x; return Int64(r); }";
        assert_eq!(run(src).unwrap(), 144);
    }

    #[test]
    fn uint8_subtraction_wraps_below_zero() {
        // 200 - 250 = -50 wraps to 206 in unsigned 8-bit space.
        let src = "fn main() -> Int64 { let x: UInt8 = 200; let r = x - 250; return Int64(r); }";
        assert_eq!(run(src).unwrap(), 206);
    }

    #[test]
    fn uint_uses_unsigned_division() {
        // A UInt64 above i64::MAX divides unsigned (signed sdiv would give a
        // different, negative-influenced quotient).
        let src = "fn main() -> Int64 { let n: UInt64 = 10000000000000000000; \
                   let q = n / 3; if q == 3333333333333333333 { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn uint_comparison_is_unsigned() {
        // As unsigned, 10e18 (>i64::MAX, stored as a negative i64) is GREATER
        // than 5 — a signed comparison would wrongly rank it below.
        let src = "fn main() -> Int64 { let big: UInt64 = 10000000000000000000; \
                   let small: UInt64 = 5; if big > small { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn uint32_holds_value_above_int32_max() {
        // 4_000_000_000 overflows Int32 but fits UInt32.
        let src = "fn main() -> Int64 { return Int64(UInt32(Int64(4000000000))); }";
        assert_eq!(run(src).unwrap(), 4000000000);
    }

    #[test]
    fn sized_int_no_overflow_is_normal() {
        let src = "fn main() -> Int64 { let a: Int32 = 5; let b = a * 3; \
                   if b == 15 { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn rejects_mixing_different_int_widths() {
        let src =
            "fn main() -> Int64 { let a: Int32 = 1; let b: Int8 = 2; let c = a + b; return 0; }";
        assert!(run(src).unwrap_err().contains("matching numeric"));
    }

    #[test]
    fn tagged_template_passes_parts_and_boxed_values() {
        // A `sql` tag receives literal parts + boxed values; the structure comes
        // only from parts (here we return $N per hole and check the length).
        let src = "fn sql(parts: Array<String>, values: Array<Value>) -> Int64 { \
                       return parts.length + values.length; } \
                   fn main() -> Int64 { let a = 1; let b = 2; \
                       return sql\"x\\{a}y\\{b}z\"; }"; // parts=3, values=2 -> 5
        assert_eq!(run(src).unwrap(), 5);
    }

    #[test]
    fn tagged_template_values_are_matchable_and_typed() {
        // The boxed values decode back to their original scalars via `match`.
        let src = "fn sql(parts: Array<String>, values: Array<Value>) -> Int64 { \
                       return match values[0] { IntVal(n) => n, BoolVal(b) => 0, StrVal(s) => s.byteLength }; } \
                   fn main() -> Int64 { let x = 41; return sql\"n=\\{x}\"; }";
        assert_eq!(run(src).unwrap(), 41);
    }

    // ---- RFC-0054 code quotes ------------------------------------------------

    #[test]
    fn splice_string_in_expression_position_is_an_inert_literal() {
        // An injection attempt: a String value carrying Vyrn syntax becomes an
        // escaped string LITERAL, never code (splice-rule safety by construction).
        let evil = "ev\"; dropTables(); \"";
        let pieces = code_splice(&Val::Str(std::rc::Rc::new(evil.to_string())), 0).unwrap();
        assert_eq!(pieces.len(), 1);
        match &pieces[0] {
            CodePiece::Text(t) => {
                // Starts/ends with a quote and re-lexes to a single string token
                // whose value is the original text — pure data.
                assert!(t.starts_with('"') && t.ends_with('"'));
                let toks = crate::lexer::lex(t).unwrap();
                assert!(matches!(&toks[0].tok, crate::lexer::Tok::Str(s) if s == evil));
                assert_eq!(toks.len(), 2); // string + EOF: one token, no injection
            }
            other => panic!("expected Text, got {other:?}"),
        }
    }

    #[test]
    fn splice_numbers_and_bools_are_literals() {
        assert_eq!(
            code_splice(&Val::Int(42), 0).unwrap(),
            vec![CodePiece::Text("42".to_string())]
        );
        assert_eq!(
            code_splice(&Val::Bool(true), 0).unwrap(),
            vec![CodePiece::Text("true".to_string())]
        );
    }

    #[test]
    fn splice_rejects_non_finite_floats() {
        // `NaN`/`inf`/`-inf` render as words that do not lex as Vyrn number
        // literals, so the splice fails with a named error here rather than
        // emitting a module that cannot parse downstream.
        for v in [f64::NAN, f64::INFINITY, f64::NEG_INFINITY] {
            match code_splice(&Val::Float(v), 0) {
                Err(Ctrl::Err(m)) => {
                    assert!(m.contains("non-finite"), "message: {m}");
                }
                other => panic!("expected a non-finite rejection, got {other:?}"),
            }
            assert!(code_splice(&Val::Float32(v as f32), 0).is_err());
        }
        // Finite floats splice as plain decimal, shortest round-trip: never the
        // scientific notation Debug emits past 1e16 / below 1e-4 (the lexer
        // accepts no exponent), and always with a fraction so it lexes as a
        // float literal rather than an integer.
        assert_eq!(
            code_splice(&Val::Float(1.5), 0).unwrap(),
            vec![CodePiece::Text("1.5".to_string())]
        );
        assert_eq!(
            code_splice(&Val::Float(1e20), 0).unwrap(),
            vec![CodePiece::Text("100000000000000000000.0".to_string())]
        );
        assert_eq!(
            code_splice(&Val::Float(1e-7), 0).unwrap(),
            vec![CodePiece::Text("0.0000001".to_string())]
        );
        assert_eq!(
            code_splice(&Val::Float32(2.0f32), 0).unwrap(),
            vec![CodePiece::Text("2.0".to_string())]
        );
    }

    #[test]
    fn splice_bad_identifier_names_the_problem() {
        // `a b` (a space) in identifier position is a comptime error.
        let err = code_splice(&Val::Str(std::rc::Rc::new("a b".to_string())), 2).unwrap_err();
        let msg = match err {
            Ctrl::Err(s) => s,
            _ => panic!("expected Err"),
        };
        assert!(msg.contains("\"a b\""), "message: {msg}");
        assert!(msg.contains("identifier"), "message: {msg}");
    }

    #[test]
    fn splice_keyword_is_rejected_in_identifier_position() {
        // A keyword is not a valid standalone identifier.
        assert!(code_splice(&Val::Str(std::rc::Rc::new("fn".to_string())), 2).is_err());
        // A valid identifier is accepted verbatim.
        assert_eq!(
            code_splice(&Val::Str(std::rc::Rc::new("greet".to_string())), 2).unwrap(),
            vec![CodePiece::Text("greet".to_string())]
        );
    }

    #[test]
    fn splice_fragment_allows_leading_digit_but_not_symbols() {
        // A fragment (ctx 1) merges with adjacent word chars, so a leading digit
        // is fine; a space or symbol is not.
        assert_eq!(
            code_splice(&Val::Str(std::rc::Rc::new("123".to_string())), 1).unwrap(),
            vec![CodePiece::Text("123".to_string())]
        );
        assert!(code_splice(&Val::Str(std::rc::Rc::new("a-b".to_string())), 1).is_err());
    }

    #[test]
    fn code_values_splice_verbatim_in_every_context() {
        let c = Val::Code(vec![CodePiece::Text("foo()".to_string())]);
        for ctx in [0, 1, 2] {
            assert_eq!(
                code_splice(&c, ctx).unwrap(),
                vec![CodePiece::Text("foo()".to_string())]
            );
        }
    }

    #[test]
    fn render_inserts_origin_directives_around_rawat_regions() {
        let pieces = vec![
            CodePiece::Text("push(x)\n".to_string()),
            CodePiece::Origin {
                path: "comp/Item.vyx".to_string(),
                line: 14,
                col: 9,
                text: "item.title".to_string(),
            },
        ];
        let out = render_code(&pieces);
        assert!(out.contains("//@origin comp/Item.vyx:14:9\n"));
        assert!(out.contains("item.title"));
        assert!(out.contains("//@origin end\n"));
        // The directive precedes the origin text (RFC-0033: governs the next line).
        let d = out.find("//@origin comp").unwrap();
        let t = out.find("item.title").unwrap();
        assert!(d < t);
    }

    #[test]
    fn lex_tokens_agrees_with_the_lexer_on_the_audit_reproducers() {
        // A comment containing `props` yields no `props` token (comment is trivia).
        let toks = lex_tokens("// props here\nlet x = 1");
        assert!(
            !toks.iter().any(|t| matches!(t, Val::Record(r, _)
                if matches!(r.get("text"), Some(Val::Str(s)) if s.as_str() == "props"))),
            "a comment's words must not leak as tokens"
        );
        // `</script>` inside a string is one string token, not markup.
        let toks = lex_tokens("\"</script>\"");
        assert_eq!(toks.len(), 1);
        match &toks[0] {
            Val::Record(r, _) => {
                assert_eq!(
                    r.get("kind"),
                    Some(&Val::Str(std::rc::Rc::new("string".to_string())))
                );
                assert_eq!(
                    r.get("text"),
                    Some(&Val::Str(std::rc::Rc::new("</script>".to_string())))
                );
            }
            other => panic!("expected a record token, got {other:?}"),
        }
        // Literal `{ a }` text lexes as braces + ident, never interpolation.
        let toks = lex_tokens("{ a }");
        let kinds: Vec<String> = toks
            .iter()
            .filter_map(|t| match t {
                Val::Record(r, _) => match r.get("text") {
                    Some(Val::Str(s)) => Some((**s).clone()),
                    _ => None,
                },
                _ => None,
            })
            .collect();
        assert_eq!(kinds, vec!["{", "a", "}"]);
    }

    #[test]
    fn lex_never_traps_on_unlexable_bytes() {
        // A stray backslash is unlexable; `lex` returns an `error` token, not a trap.
        let toks = lex_tokens("let x = \\");
        assert!(toks
            .iter()
            .any(|t| matches!(t, Val::Record(r, _) if r.get("kind") == Some(&Val::Str(std::rc::Rc::new("error".to_string()))))));
    }

    #[test]
    fn schema_of_extracts_where_bounds() {
        // `schemaOf(Port)` reads the `where` predicate at compile time.
        let src = "type Port = Int64 where value >= 1 && value <= 65535; \
                   fn optOr(o: Option<Int64>, d: Int64) -> Int64 { \
                       return match o { Some(n) => n, None => d }; } \
                   fn main() -> Int64 { let s = schemaOf(Port); \
                       return optOr(s.min, 0) + optOr(s.max, 0); }"; // 1 + 65535
        assert_eq!(run(src).unwrap(), 65536);
    }

    /// The enriched `Schema`: name, base spelling (incl. sized ints), `///`
    /// doc, `multipleOf`, string length bounds, and the regex pattern.
    #[test]
    fn schema_of_enriched_fields() {
        let src = "/// A lowercase handle.\n\
                   type Username = String where value.byteLength >= 3 && value.byteLength <= 16 && value =~ \"[a-z]+\"\n\
                   type Even = Int64 where value % 2 == 0\n\
                   type Byte = UInt8\n\
                   fn optOr(o: Option<Int64>, d: Int64) -> Int64 {\n\
                       return match o { Some(n) => n, None => d }\n\
                   }\n\
                   fn main() -> Int64 {\n\
                       let u = schemaOf(Username)\n\
                       let e = schemaOf(Even)\n\
                       let b = schemaOf(Byte)\n\
                       let mut n = 0\n\
                       if u.name == \"Username\" { n = n + 1 }\n\
                       if u.base == \"String\" { n = n + 1 }\n\
                       if optOr(u.minLength, 0) == 3 { n = n + 1 }\n\
                       if optOr(u.maxLength, 0) == 16 { n = n + 1 }\n\
                       if match u.pattern { Some(p) => p == \"[a-z]+\", None => false } { n = n + 1 }\n\
                       if match u.doc { Some(d) => true, None => false } { n = n + 1 }\n\
                       if optOr(e.multipleOf, 0) == 2 { n = n + 1 }\n\
                       if b.base == \"UInt8\" { n = n + 1 }\n\
                       if match b.doc { Some(d) => false, None => true } { n = n + 1 }\n\
                       return n\n\
                   }";
        assert_eq!(run(src).unwrap(), 9);
    }

    #[test]
    fn schema_of_unbounded_type_has_no_bounds() {
        let src = "type Id = Int64; \
                   fn none(o: Option<Int64>) -> Int64 { return match o { Some(n) => 1, None => 0 }; } \
                   fn main() -> Int64 { let s = schemaOf(Id); return none(s.min) + none(s.max); }";
        assert_eq!(run(src).unwrap(), 0); // both None
    }

    #[test]
    fn schema_of_rejects_a_non_type() {
        let src = "fn main() -> Int64 { let x = 5; let s = schemaOf(x); return 0; }";
        assert!(run(src).unwrap_err().contains("not a type"));
    }

    #[test]
    fn string_length_field() {
        let src = "fn main() -> Int64 { let s = \"hello\"; return s.byteLength; }";
        assert_eq!(run(src).unwrap(), 5);
    }

    #[test]
    fn string_ordering_is_bytewise_lexicographic() {
        // RFC-0022: `< <= > >=` on Strings, byte order (not collation). Each
        // returns 1 when the ordering holds. Covers prefixes, empties, equality,
        // and a multibyte case where byte order puts "é" (0xC3..) after "z" (0x7A).
        let cases: &[(&str, i64)] = &[
            ("\"ab\" < \"b\"", 1),   // 'a' < 'b'
            ("\"a\" < \"ab\"", 1),   // shorter prefix sorts first
            ("\"ab\" < \"ab\"", 0),  // equal: strictly-less is false
            ("\"ab\" <= \"ab\"", 1), // equal: <= holds
            ("\"b\" > \"ab\"", 1),
            ("\"\" < \"a\"", 1), // empty precedes anything
            ("\"\" <= \"\"", 1),
            ("\"z\" < \"\u{e9}\"", 1), // 0x7A < 0xC3 (leading UTF-8 byte)
            ("\"\u{e9}\" > \"z\"", 1),
        ];
        for (expr, want) in cases {
            let src = format!("fn main() -> Int64 {{ if {expr} {{ return 1 }} return 0 }}");
            assert_eq!(run(&src).unwrap(), *want, "for `{expr}`");
        }
    }

    #[test]
    fn string_indexing_and_char_literal() {
        // `s[1]` is the byte 'e' (101) as a `UInt8` (RFC-0022) — `Int64(..)`
        // widens it for an Int64 return; a char literal adapts to the byte.
        let src = "fn main() -> Int64 { let s = \"hello\"; return Int64(s[1]); }";
        assert_eq!(run(src).unwrap(), 101);
        let cmp =
            "fn main() -> Int64 { let s = \"hello\"; if s[0] == 'h' { return 1; } return 0; }";
        assert_eq!(run(cmp).unwrap(), 1);
    }

    #[test]
    fn string_index_out_of_bounds_traps() {
        let src = "fn main() -> Int64 { let s = \"hi\"; return Int64(s[5]); }";
        assert!(run(src).unwrap_err().contains("out of bounds"));
    }

    #[test]
    fn unicode_bytes_vs_code_points() {
        // "café": 5 UTF-8 bytes but 4 code points; `é` is U+00E9 = 233.
        let bytes = "fn main() -> Int64 { return bytes(\"caf\\u{e9}\").length; }";
        assert_eq!(run(bytes).unwrap(), 5);
        // `chars` is `std/text`'s declaration since RFC-0094 M2, so it needs the
        // loader path AND an import. `bytes` and `byteLength` are the views that
        // stayed, so `run` still serves them.
        let imp = "import { chars } from \"std/text\" ";
        let chars = format!("{imp}fn main() -> Int64 {{ return chars(\"caf\\u{{e9}}\").length; }}");
        assert_eq!(run_json(&chars).unwrap(), 4);
        let cp = format!("{imp}fn main() -> Int64 {{ return chars(\"caf\\u{{e9}}\")[3]; }}");
        assert_eq!(run_json(&cp).unwrap(), 233);
    }

    #[test]
    fn code_point_iteration_and_emoji() {
        // A 4-byte emoji is a single code point.
        let len = "fn main() -> Int64 { return \"\\u{1F600}\".byteLength; }"; // 4 bytes
        assert_eq!(run(len).unwrap(), 4);
        let imp = "import { chars } from \"std/text\" ";
        let one = format!("{imp}fn main() -> Int64 {{ return chars(\"\\u{{1F600}}\").length; }}");
        assert_eq!(run_json(&one).unwrap(), 1); // 1 char
        let val = format!("{imp}fn main() -> Int64 {{ return chars(\"\\u{{1F600}}\")[0]; }}");
        assert_eq!(run_json(&val).unwrap(), 128512);
    }

    #[test]
    fn byte_literal_is_its_byte_value() {
        // A byte literal (RFC-0057) evaluates to its byte, as an integer value.
        assert_eq!(run("fn main() -> Int64 { return 'a' }").unwrap(), 97);
        assert_eq!(run("fn main() -> Int64 { return '{' }").unwrap(), 123);
        assert_eq!(run("fn main() -> Int64 { return '\\n' }").unwrap(), 10);
        assert_eq!(run("fn main() -> Int64 { return '\\xff' }").unwrap(), 255);
        // It coerces against a byte from `bytes(..)` (both `UInt8`).
        assert_eq!(
            run("fn main() -> Int64 { if bytes(\"{\")[0] == '{' { return 1 } return 0 }").unwrap(),
            1
        );
    }

    /// The six codecs, end to end (checker + loader + interpreter). RFC-0078 M4c
    /// routed them into `std/codecs` and RFC-0094 M2 made them ordinary imports,
    /// so what is worth asserting is unchanged and the import line is the only
    /// difference: a round trip, and the three refusals the deleted Rust helper
    /// tests covered.
    #[test]
    fn the_codecs_answer_through_std_codecs() {
        let src = "import { base64Decode, base64Encode, hexDecode, hexEncode, urlDecode, urlEncode } from \"std/codecs\"                    fn main() -> Int64 {                    let d = base64Decode(base64Encode(\"hey\"))                    if match d { Some(s) => s, None => \"\" } != \"hey\" { return 1 }                    if hexEncode(\"Hi\") != \"4869\" { return 2 }                    if urlEncode(\"a b&c\") != \"a%20b%26c\" { return 3 }                    if match hexDecode(\"zz\") { Some(s) => 1, None => 0 } != 0 { return 4 }                    if match base64Decode(\"bad\") { Some(s) => 1, None => 0 } != 0 { return 5 }                    if match urlDecode(\"%ZZ\") { Some(s) => 1, None => 0 } != 0 { return 6 }                    return 0 }";
        assert_eq!(run_json(src).unwrap(), 0);
    }

    /// The seam M2b named, as RFC-0094 M2 leaves it: a bare source with no
    /// resolver has no `std/codecs` in the link, so the name does not resolve at
    /// all and the diagnostic says where it lives rather than that it is missing.
    #[test]
    fn a_moved_builtin_without_a_std_root_names_its_module() {
        let e = run("fn main() -> Int64 { return hexEncode(\"hi\").byteLength }").unwrap_err();
        assert!(e.contains("`hexEncode` is `std/codecs`'s"), "{e}");
    }

    #[test]
    fn string_iteration_sums_bytes() {
        // 'a'(97) + 'b'(98) + 'c'(99) = 294.
        let src = "fn main() -> Int64 { let s = \"abc\"; let mut t = 0; \
                   for c in s { t = t + c; } return t; }";
        assert_eq!(run(src).unwrap(), 294);
    }

    #[test]
    fn string_predicate_methods() {
        // `std/strpred` exports since RFC-0094 M2, so this goes through the loader
        // and carries an import — `run` has no resolver and no module to link.
        let imp = "import { contains, endsWith, startsWith } from \"std/strpred\" ";
        let c = format!(
            "{imp}fn main() -> Int64 {{ if contains(\"hello\", \"ell\") {{ return 1 }} return 0 }}"
        );
        assert_eq!(run_json(&c).unwrap(), 1);
        let s = format!("{imp}fn main() -> Int64 {{ if startsWith(\"hello\", \"he\") {{ return 1 }} return 0 }}");
        assert_eq!(run_json(&s).unwrap(), 1);
        let e = format!(
            "{imp}fn main() -> Int64 {{ if endsWith(\"hello\", \"lo\") {{ return 1 }} return 0 }}"
        );
        assert_eq!(run_json(&e).unwrap(), 1);
        // `endsWith` guards against a suffix longer than the string.
        let g = format!(
            "{imp}fn main() -> Int64 {{ if endsWith(\"hi\", \"ahoy\") {{ return 1 }} return 0 }}"
        );
        assert_eq!(run_json(&g).unwrap(), 0);
    }

    #[test]
    fn indexing_in_refinement_predicate() {
        let ok = "type G = String where value.byteLength >= 1 && value[0] == 'H'; \
                  fn mk(s: String) -> G { return G(s); } \
                  fn main() -> Int64 { let g = mk(\"Hi\"); return g.byteLength; }";
        assert_eq!(run(ok).unwrap(), 2);
        // A provably-wrong constant is rejected at compile time (via consteval).
        let bad = "type G = String where value.byteLength >= 1 && value[0] == 'H'; \
                   fn main() -> Int64 { let g = G(\"bye\"); return 0; }";
        assert!(run(bad).unwrap_err().contains("does not satisfy `G`"));
    }

    #[test]
    fn validated_string_accepts_valid_value() {
        let src = "type Name = String where value.byteLength >= 3; \
                   fn mk(s: String) -> Name { return Name(s); } \
                   fn main() -> Int64 { let n = mk(\"bob\"); return n.byteLength; }";
        assert_eq!(run(src).unwrap(), 3);
    }

    #[test]
    fn validated_string_traps_on_too_short() {
        // Runtime construction of an invalid string aborts (matches native exit 1).
        let src = "type Name = String where value.byteLength >= 3; \
                   fn mk(s: String) -> Name { return Name(s); } \
                   fn main() -> Int64 { let n = mk(\"x\"); return 0; }";
        assert!(run(src)
            .unwrap_err()
            .contains("validation failed for `Name`"));
    }

    #[test]
    fn proven_interpolation_runs_correctly() {
        // RFC-0020 M1: a statically-proven interpolation flows into TransKey and
        // runs identically (the interp validation is a no-op on a proven value).
        let src = "type TransKey = String where value =~ \"nav\\\\.(home|about)\\\\.label\"\n\
                   type Section = String where value =~ \"home|about\"\n\
                   fn t(key: TransKey) -> Int64 { return key.byteLength }\n\
                   fn main() -> Int64 { let s: Section = \"home\"  return t(\"nav.\\{s}.label\") }";
        // "nav.home.label" is 14 bytes.
        assert_eq!(run(src).unwrap(), 14);
    }

    #[test]
    fn nonfinite_hole_interpolation_traps_at_runtime() {
        // A plain-String hole is not finite, so no static proof — an invalid
        // value produced at runtime traps through the canonical message (the
        // interp counterpart of the codegen runtime-validation test).
        let src = "type TransKey = String where value =~ \"nav\\\\.(home|about)\\\\.label\"\n\
                   fn build(x: String) -> Int64 { let k: TransKey = \"nav.\\{x}.label\"  return 0 }\n\
                   fn main() -> Int64 { return build(\"BAD\") }";
        assert!(run(src)
            .unwrap_err()
            .contains("validation failed for `TransKey`"));
    }

    #[test]
    fn cross_field_record_valid_and_invalid() {
        let ok = "type R = { a: Int64, b: Int64 } where a < b; \
                  fn mk(x: Int64, y: Int64) -> R { return R { a: x, b: y }; } \
                  fn main() -> Int64 { let r = mk(1, 2); return r.b; }";
        assert_eq!(run(ok).unwrap(), 2);
        let bad = "type R = { a: Int64, b: Int64 } where a < b; \
                   fn mk(x: Int64, y: Int64) -> R { return R { a: x, b: y }; } \
                   fn main() -> Int64 { let r = mk(5, 1); return 0; }";
        assert!(run(bad).unwrap_err().contains("violates its `where`"));
    }

    #[test]
    fn validation_trap_message_is_canonical() {
        let src = "type Age = Int64 where value >= 18; \
                   fn mk(n: Int64) -> Age { return Age(n); } \
                   fn main() -> Int64 { let a = mk(5); return 0; }";
        assert_eq!(run(src).unwrap_err(), "validation failed for `Age`");
    }

    #[test]
    fn auto_validation_traps_dynamic_violations_at_each_boundary() {
        // Argument boundary.
        let arg = "type Age = Int64 where value >= 18 \
                   fn g(a: Age) -> Int64 { return a } \
                   fn main() -> Int64 { let mut x = 30 x = x - 25 return g(x) }";
        assert_eq!(run(arg).unwrap_err(), "validation failed for `Age`");
        // Assignment boundary (the binding's declared type is remembered).
        let assign = "type Age = Int64 where value >= 18 \
                      fn main() -> Int64 { let mut a: Age = 20 a = a - 15 return a }";
        assert_eq!(run(assign).unwrap_err(), "validation failed for `Age`");
        // Return boundary (a raw match join validates on the way out).
        let ret = "type Age = Int64 where value >= 18 \
                   fn pick(o: Option<Int64>) -> Age { \
                       return match o { Some(x) => x, None => 18 } } \
                   fn main() -> Int64 { return pick(Some(5)) }";
        assert_eq!(run(ret).unwrap_err(), "validation failed for `Age`");
        // Record-field boundary.
        let field = "type Age = Int64 where value >= 18 \
                     type User = { age: Age } \
                     fn mk(n: Int64) -> User { return User { age: n } } \
                     fn main() -> Int64 { let u = mk(5) return 0 }";
        assert_eq!(run(field).unwrap_err(), "validation failed for `Age`");
        // Cross-field record coercion (structural value into a predicated type).
        let xf = "type Range = { start: Int64, end: Int64 } where start < end \
                  type Plain = { start: Int64, end: Int64 } \
                  fn span(r: Range) -> Int64 { return r.end - r.start } \
                  fn mk(a: Int64, b: Int64) -> Plain { return Plain { start: a, end: b } } \
                  fn main() -> Int64 { return span(mk(9, 3)) }";
        assert_eq!(
            run(xf).unwrap_err(),
            "validation failed: `Range` violates its `where` clause"
        );
    }

    /// The boundary that was NOT validated: a field STORE (RFC-0082 M3).
    ///
    /// `Stmt::SetField` never coerced, so every spelling that writes through a
    /// record field let a runtime value into a validated element type while both
    /// compiled backends trapped — the one hole in "a Vyrn program cannot even
    /// spell a value that failed its own predicate". A literal is folded by
    /// `consteval` at compile time on all three engines, which is why only a
    /// runtime value reaches it and why nothing said for so long.
    #[test]
    fn a_field_store_validates_like_every_other_boundary() {
        let head = "type Age = Int64 where value >= 18 \
                    type T = { xs: Array<Age> } \
                    fn rt(n: Int64) -> Int64 { return n - 1 } ";
        // `t.xs.push(v)` — the in-place append fast path.
        let push = format!(
            "{head} fn main() -> Int64 {{ let mut t = T {{ xs: [] }} \
             t.xs.push(rt(6)) return t.xs[0] }}"
        );
        assert_eq!(run(&push).unwrap_err(), "validation failed for `Age`");
        // The same, through a `modify` parameter rather than the local.
        let param = format!(
            "{head} fn add(t: modify T) {{ t.xs.push(rt(6)) }} \
             fn main() -> Int64 {{ let mut t = T {{ xs: [] }} add(t) return t.xs[0] }}"
        );
        assert_eq!(run(&param).unwrap_err(), "validation failed for `Age`");
        // And through module state, whose slot type is inferred from the
        // initializer for exactly this reason.
        let global = format!(
            "{head} let mut g = T {{ xs: [] }} \
             fn main() -> Int64 {{ g.xs.push(rt(6)) return g.xs[0] }}"
        );
        assert_eq!(run(&global).unwrap_err(), "validation failed for `Age`");
        // Valid values still flow through all three.
        let ok = format!(
            "{head} let mut g = T {{ xs: [] }} \
             fn add(t: modify T) {{ t.xs.push(rt(21)) }} \
             fn main() -> Int64 {{ let mut t = T {{ xs: [] }} add(t) g.xs.push(rt(31)) \
             return t.xs[0] + g.xs[0] }}"
        );
        assert_eq!(run(&ok).unwrap(), 50);
    }

    #[test]
    fn inline_field_refinements_validate_like_named_types() {
        // Zod/ArkType-style inline `where` on fields: valid values flow through…
        let ok = "type User = { name: String where value.byteLength >= 3, \
                                age: Int64 where value >= 18 } \
                  fn mk(n: Int64) -> User { return User { name: \"ada\", age: n } } \
                  fn main() -> Int64 { let u = mk(33) return u.age }";
        assert_eq!(run(ok).unwrap(), 33);
        // …a dynamic violation traps with the synthetic field-type name…
        let bad = "type User = { age: Int64 where value >= 18 } \
                   fn mk(n: Int64) -> User { return User { age: n } } \
                   fn main() -> Int64 { let u = mk(5) return 0 }";
        assert_eq!(run(bad).unwrap_err(), "validation failed for `User.age`");
        // …and a provably-bad constant is rejected at compile time.
        let constant = "type User = { age: Int64 where value >= 18 } \
                        fn main() -> Int64 { let u = User { age: 5 } return 0 }";
        assert!(run(constant)
            .unwrap_err()
            .contains("does not satisfy `User.age`"));
    }

    #[test]
    fn auto_validation_passes_valid_dynamic_values() {
        let src = "type Age = Int64 where value >= 18 \
                   fn g(a: Age) -> Int64 { return a } \
                   fn main() -> Int64 { \
                       let a: Age = 25 \
                       let mut m: Age = 21 \
                       m = m + 1 \
                       let xs: Array<Age, 2> = [19, 20] \
                       return g(a) + m + xs[1] }";
        assert_eq!(run(src).unwrap(), 25 + 22 + 20);
    }

    #[test]
    fn float_refined_type_constructs_and_rejects_at_runtime() {
        // Refinements over a Float base run under the runtime evaluator (this
        // used to fail for even VALID values — ConstVal had no Float).
        let ok = "type Ratio = Float64 where value > 0.0 && value <= 1.0; \
                  fn mk(x: Float64) -> Ratio { return Ratio(x); } \
                  fn main() -> Int64 { let r = mk(0.5); return 0; }";
        assert_eq!(run(ok).unwrap(), 0);
        let bad = "type Ratio = Float64 where value > 0.0 && value <= 1.0; \
                   fn mk(x: Float64) -> Ratio { return Ratio(x); } \
                   fn main() -> Int64 { let r = mk(2.5); return 0; }";
        assert!(run(bad)
            .unwrap_err()
            .contains("validation failed for `Ratio`"));
    }

    #[test]
    fn sized_int_refined_type_constructs_at_runtime() {
        let src = "type Small = Int32 where value < 100; \
                   fn mk(x: Int32) -> Small { return Small(x); } \
                   fn main() -> Int64 { let s = mk(Int32(5)); return 0; }";
        assert_eq!(run(src).unwrap(), 0);
    }

    #[test]
    fn cross_field_predicate_over_float_fields() {
        let ok = "type R = { a: Float64, b: Float64 } where a < b; \
                  fn mk(x: Float64, y: Float64) -> R { return R { a: x, b: y }; } \
                  fn main() -> Int64 { let r = mk(1.0, 2.0); return 0; }";
        assert_eq!(run(ok).unwrap(), 0);
        let bad = "type R = { a: Float64, b: Float64 } where a < b; \
                   fn mk(x: Float64, y: Float64) -> R { return R { a: x, b: y }; } \
                   fn main() -> Int64 { let r = mk(2.0, 1.0); return 0; }";
        assert!(run(bad).unwrap_err().contains("violates its `where`"));
    }

    #[test]
    fn int_arithmetic_wraps_like_native() {
        // i64::MAX + 1 wraps to i64::MIN in BOTH backends (and independent of
        // the cargo profile — bare `+` would panic in a debug build).
        let src = "fn main() -> Int64 { \
                       let m = 9223372036854775807 \
                       let w = m + 1 \
                       if w < 0 { return 1 } return 0 }";
        assert_eq!(run(src).unwrap(), 1);
        // -i64::MIN also wraps (back to MIN).
        let neg = "fn main() -> Int64 { \
                       let m = -9223372036854775808 \
                       let w = 0 - m \
                       if w < 0 { return 1 } return 0 }";
        assert_eq!(run(neg).unwrap(), 1);
    }

    #[test]
    fn division_traps_have_stable_messages() {
        let z = "fn main() -> Int64 { let mut d = 0; return 1 / d; }";
        assert_eq!(run(z).unwrap_err(), "division by zero");
        let rz = "fn main() -> Int64 { let mut d = 0; return 1 % d; }";
        assert_eq!(run(rz).unwrap_err(), "remainder by zero");
        // i64::MIN / -1 is unrepresentable: a clean trap, not a panic/SEH crash.
        let ovf = "fn main() -> Int64 { \
                       let m = -9223372036854775808 \
                       let mut d = 0 - 1 \
                       return m / d }";
        assert_eq!(run(ovf).unwrap_err(), "integer overflow in division");
    }

    #[test]
    fn remainder_min_neg_one_is_zero_not_a_trap() {
        // `MIN % -1 == 0` — NO trap (RFC-0060), unlike `MIN / -1`.
        let src = "fn main() -> Int64 { \
                       let m = -9223372036854775808 \
                       let mut d = 0 - 1 \
                       return m % d }";
        assert_eq!(run(src).unwrap(), 0);
    }

    #[test]
    fn remainder_sign_of_dividend_and_the_division_law() {
        // Truncated remainder takes the sign of the dividend (C/Rust/LLVM srem).
        let cases: &[(i64, i64, i64)] = &[
            (7, 3, 1),
            (-7, 3, -1),
            (7, -3, 1),
            (-7, -3, -1),
            (0, 5, 0),
            (9223372036854775807, 2, 1),
        ];
        for (a, b, want) in cases {
            let src = format!("fn main() -> Int64 {{ let a = {a} let b = {b} return a % b }}");
            assert_eq!(run(&src).unwrap(), *want, "{a} % {b}");
            // The law: `a == (a / b) * b + a % b` for every non-zero b.
            let law = format!(
                "fn main() -> Int64 {{ let a = {a} let b = {b} \
                 if (a / b) * b + a % b == a {{ return 1 }} return 0 }}"
            );
            assert_eq!(run(&law).unwrap(), 1, "law for {a} % {b}");
        }
    }

    #[test]
    fn remainder_on_sized_ints_wraps_and_upholds_the_law() {
        // UInt8 / Int8: remainder computed at width, sign of dividend for signed.
        let u = "fn main() -> Int64 { let a: UInt8 = 200 let b: UInt8 = 7 \
                 let r = a % b return Int64(r) }";
        assert_eq!(run(u).unwrap(), 200 % 7);
        // Int8 MIN % -1 == 0, no trap. Build MIN (-128) and -1 by wrapping at width.
        let s = "fn main() -> Int64 { \
                 let hi: Int8 = 127 let min = hi + 1 \
                 let zero: Int8 = 0 let d = zero - 1 \
                 let r = min % d return Int64(r) }";
        assert_eq!(run(s).unwrap(), 0);
    }

    #[test]
    fn break_exits_the_innermost_loop() {
        // Sum 0..10 but stop at 5: 0+1+2+3+4 = 10.
        let src = "fn main() -> Int64 { \
                   let mut s = 0 let mut i = 0 \
                   while i < 10 { if i == 5 { break } s = s + i i = i + 1 } \
                   return s }";
        assert_eq!(run(src).unwrap(), 10);
    }

    #[test]
    fn continue_skips_to_the_next_iteration() {
        // Sum only the even numbers in 0..6: 0+2+4 = 6.
        let src = "fn main() -> Int64 { \
                   let mut s = 0 \
                   for i in [0, 1, 2, 3, 4, 5] { if i % 2 == 1 { continue } s = s + i } \
                   return s }";
        assert_eq!(run(src).unwrap(), 6);
    }

    #[test]
    fn break_exits_only_the_inner_of_nested_loops() {
        // Inner breaks immediately; outer runs 3 times adding 1 each → 3.
        let src = "fn main() -> Int64 { \
                   let mut n = 0 \
                   for a in [0, 1, 2] { \
                       for b in [0, 1, 2] { break n = n + 100 } \
                       n = n + 1 } \
                   return n }";
        assert_eq!(run(src).unwrap(), 3);
    }

    #[test]
    fn if_let_binds_on_match_and_runs_else_otherwise() {
        // Some binds `v`; None runs the else branch.
        let hit = "fn f(b: Bool) -> Option<Int64> { if b { return Some(7) } return None } \
                   fn main() -> Int64 { if let Some(v) = f(true) { return v } return 0 - 1 }";
        assert_eq!(run(hit).unwrap(), 7);
        let miss = "fn f(b: Bool) -> Option<Int64> { if b { return Some(7) } return None } \
                    fn main() -> Int64 { if let Some(v) = f(false) { return v } return 0 - 1 }";
        assert_eq!(run(miss).unwrap(), -1);
    }

    #[test]
    fn if_let_over_result_and_user_enum() {
        let ok = "fn f() -> Result<Int64, String> { return Ok(4) } \
                  fn main() -> Int64 { if let Ok(n) = f() { return n } return 0 }";
        assert_eq!(run(ok).unwrap(), 4);
        let enm = "type Shape = | Circle(Int64) | Rect(Int64, Int64) | Empty \
                   fn main() -> Int64 { let s = Rect(3, 4) \
                       if let Rect(w, h) = s { return w * h } return 0 }";
        assert_eq!(run(enm).unwrap(), 12);
    }

    #[test]
    fn while_let_drains_without_double_evaluating_the_scrutinee() {
        // The scrutinee `next()` decrements a global each call; if `while let`
        // double-evaluated it, the count would be wrong. It must tick once per
        // iteration (RFC-0060): 3 → prints 2,1,0 → 3 iterations, global ends at 0.
        let src = "let mut n: Int64 = 3 \
                   fn next() -> Option<Int64> { if n == 0 { return None } n = n - 1 return Some(n) } \
                   fn main() -> Int64 { let mut count = 0 \
                       while let Some(v) = next() { print(v) count = count + 1 } \
                       return count }";
        assert_eq!(run(src).unwrap(), 3);
    }

    #[test]
    fn break_inside_if_let_exits_the_loop() {
        // `break` inside an `if let` body targets the enclosing loop (RFC-0060).
        let src = "fn main() -> Int64 { let mut s = 0 \
                   for x in [1, 2, 3, 4] { \
                       if let Some(v) = Some(x) { if v == 3 { break } s = s + v } } \
                   return s }";
        assert_eq!(run(src).unwrap(), 3);
    }

    #[test]
    fn continue_under_a_region_still_frees_the_region() {
        // A region inside the loop body, exited early by `continue` every other
        // iteration — the interpreter decrements its region depth on that path
        // (so 100 iterations never exceed the 64-region cap). Just must not trap.
        let src = "fn main() -> Int64 { \
                   let mut n = 0 let mut i = 0 \
                   while i < 100 { \
                       i = i + 1 \
                       region { if i % 2 == 0 { continue } n = n + 1 } } \
                   return n }";
        assert_eq!(run(src).unwrap(), 50);
    }

    #[test]
    fn wrapped_predicate_arithmetic_matches_native() {
        // `value + 1 != 0` at i64::MAX: wraps to MIN (≠ 0) — the predicate
        // holds in both backends (checked arithmetic used to refuse to prove
        // it and the interpreter then errored out).
        let src = "type T = Int64 where value + 1 != 0; \
                   fn mk(x: Int64) -> T { return T(x); } \
                   fn main() -> Int64 { let t = mk(9223372036854775807); return 0; }";
        assert_eq!(run(src).unwrap(), 0);
    }

    #[test]
    fn regex_match_operator() {
        let src = "fn main() -> Int64 { if \"abc\" =~ \"[a-z]+\" { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
        let no = "fn main() -> Int64 { if \"ab9\" =~ \"[a-z]+\" { return 1; } return 0; }";
        assert_eq!(run(no).unwrap(), 0);
    }

    #[test]
    fn validated_string_via_regex_traps() {
        let src = "type Code = String where value =~ \"[A-Z][A-Z][A-Z]\"; \
                   fn mk(s: String) -> Code { return Code(s); } \
                   fn main() -> Int64 { let c = mk(\"ab\"); return 0; }";
        assert!(run(src)
            .unwrap_err()
            .contains("validation failed for `Code`"));
    }

    #[test]
    fn validation_accumulates_all_issues() {
        // Both checks fail → Invalid carries both issues (i18n keys included).
        let src = "type P = { n: Int64 }; \
                   fn v(a: Int64, b: Int64) -> Validation<P> { \
                       let mut issues: Array<Issue> = []; \
                       if a < 0 { issues.push(Issue { key: \"a.min\", path: \"a\", message: \"m\" }); } \
                       if b < 0 { issues.push(Issue { key: \"b.min\", path: \"b\", message: \"m\" }); } \
                       if issues.length > 0 { return Invalid(issues); } \
                       return Valid(P { n: a + b }); } \
                   fn iss(x: Validation<P>) -> Array<Issue> { \
                       return match x { Valid(p) => [], Invalid(is) => is.copy() }; } \
                   fn main() -> Int64 { return iss(v(0 - 1, 0 - 1)).length; }";
        assert_eq!(run(src).unwrap(), 2);
    }

    #[test]
    fn validation_valid_case_carries_the_value() {
        let src = "type P = { n: Int64 }; \
                   fn v(a: Int64) -> Validation<P> { \
                       if a < 0 { return Invalid([]); } return Valid(P { n: a }); } \
                   fn valueOr(x: Validation<P>) -> Int64 { \
                       return match x { Valid(p) => p.n, Invalid(is) => 0 - 1 }; } \
                   fn main() -> Int64 { return valueOr(v(41)); }";
        assert_eq!(run(src).unwrap(), 41);
    }

    #[test]
    fn multiline_string_includes_the_newline() {
        // A raw newline inside "..." is part of the string (RFC-0007).
        let src = "fn main() -> Int64 { let s = \"ab\ncd\"; return s.byteLength; }"; // 'a','b','\n','c','d' = 5
        assert_eq!(run(src).unwrap(), 5);
    }

    #[test]
    fn template_value_exposes_parts_and_values() {
        // `template"..."` yields a first-class Template { parts, values }.
        let src = "fn main() -> Int64 { let n = 7; let t = template\"a\\{n}b\"; \
                   return t.parts.length + t.values.length; }"; // 2 parts + 1 value = 3
        assert_eq!(run(src).unwrap(), 3);
    }

    #[test]
    fn tagged_template_needs_an_interpolation() {
        // A tag on a hole-less string is rejected (use a plain string instead).
        let src = "fn sql(p: Array<String>, v: Array<Value>) -> Int64 { return 0; } \
                   fn main() -> Int64 { return sql\"no holes here\"; }";
        assert!(run(src).unwrap_err().contains("interpolation"));
    }

    #[test]
    fn value_boxes_string_and_int_distinctly() {
        let src = "fn main() -> Int64 { \
                   let a = match value(7) { IntVal(n) => n, BoolVal(b) => 0, StrVal(s) => 0 - 1 }; \
                   let b = match value(\"hey\") { IntVal(n) => 0, BoolVal(x) => 0, StrVal(s) => s.byteLength }; \
                   return a + b; }"; // 7 + 3
        assert_eq!(run(src).unwrap(), 10);
    }

    /// RFC-0094 M3, and RFC-0007 §v2 with it: a hole may carry any type that
    /// says how it renders, and it reaches the tag as the `StrVal` it rendered
    /// to — so a hole is still data and still cannot become the tag's structure.
    #[test]
    fn value_boxes_a_declared_type_as_the_string_it_renders_to() {
        let src = "protocol Show { fn show(self) -> String }\n\
                   type P = { x: Int64 }\n\
                   impl Show for P { fn show(self) -> String { return \"pt\" } }\n\
                   fn main() -> Int64 { let p = P { x: 1 }\n \
                   return match value(p) { IntVal(n) => 0, BoolVal(b) => 0, \
                   StrVal(s) => s.byteLength } }";
        assert_eq!(run(src).unwrap(), 2);
    }

    /// The scalar guard, at runtime. `impl Show for Int64` is callable by name
    /// and does NOT redefine the digits: `7.toString()` is `7`, not what the
    /// impl says. The impl's own body is `self.toString()`, so a dispatch that
    /// took a scalar would not return at all.
    #[test]
    fn a_scalar_never_renders_through_a_declaration() {
        let src = "protocol Show { fn show(self) -> String }\n\
                   impl Show for Int64 { fn show(self) -> String { \
                   return \"n\" + self.toString() } }\n\
                   fn main() -> Int64 { let n = 7\n \
                   return n.toString().byteLength + n.show().byteLength }";
        assert_eq!(run(src).unwrap(), 3); // "7" is 1, "n7" is 2
    }

    #[test]
    fn logger_and_levels_typecheck_and_run() {
        // A logger with each level, using interpolation in the message. Logs go
        // to stderr; the program returns normally.
        let src = "fn main() -> Int64 { let log = logger(\"t\"); let n = 2; \
                   log.trace(\"a\"); log.debug(\"b\"); log.info(\"n=\\{n}\"); \
                   log.warn(\"c\"); log.error(\"d\"); return n; }";
        assert_eq!(run(src).unwrap(), 2);
    }

    #[test]
    fn log_level_requires_a_logger() {
        // Calling a level on a non-Logger is rejected.
        let src = "fn main() -> Int64 { info(\"notalogger\", \"x\"); return 0; }";
        assert!(run(src).is_err());
    }

    #[test]
    fn logging_is_forbidden_in_spawned_tasks() {
        // A spawned function must be pure; logging is observable I/O.
        let src =
            "fn work(n: Int64) -> Int64 { let l = logger(\"w\"); l.info(\"hi\"); return n; } \
                   fn main() -> Int64 { let t = spawn work(1); return t.join(); }";
        assert!(run(src).is_err());
    }

    #[test]
    fn logging_config_block_parses_and_runs() {
        let src = "logging { level: warn } \
                   fn main() -> Int64 { let log = logger(\"a\"); \
                   log.info(\"filtered\"); log.error(\"shown\"); return 0; }";
        assert_eq!(run(src).unwrap(), 0);
    }

    #[test]
    fn invalid_log_level_is_rejected() {
        let src = "logging { level: loud } fn main() -> Int64 { return 0; }";
        assert!(run(src).unwrap_err().contains("log level"));
    }

    #[test]
    fn duplicate_logging_block_is_rejected() {
        let src = "logging { level: info } logging { level: warn } \
                   fn main() -> Int64 { return 0; }";
        assert!(run(src).unwrap_err().contains("duplicate"));
    }

    #[test]
    fn logging_sink_and_level_parse_together() {
        let src = "logging { level: warn, sink: stdout } \
                   fn main() -> Int64 { let l = logger(\"a\"); l.warn(\"x\"); return 0; }";
        assert_eq!(run(src).unwrap(), 0);
    }

    #[test]
    fn unknown_sink_is_rejected() {
        let src = "logging { sink: syslog } fn main() -> Int64 { return 0; }";
        assert!(run(src).unwrap_err().contains("sink"));
    }

    #[test]
    fn file_sink_needs_a_string_path() {
        let src = "logging { sink: file(main) } fn main() -> Int64 { return 0; }";
        assert!(run(src).is_err());
    }

    #[test]
    fn spawn_and_join_fork_join() {
        let src = "
            fn sq(n: Int64) -> Int64 { return n * n; }
            fn main() -> Int64 {
                let a = spawn sq(6);
                let b = spawn sq(8);
                return a.join() + b.join();   // 36 + 64
            }
        ";
        assert_eq!(run(src).unwrap(), 100);
    }

    #[test]
    fn modify_parameter_writes_back_to_caller() {
        let src = "
            type C = { x: Int64 };
            fn bump(c: modify C) { c.x = c.x + 1; }
            fn main() -> Int64 {
                let mut c = C { x: 40 };
                bump(c); bump(c);   // caller's c is mutated each time
                return c.x;          // 42
            }
        ";
        assert_eq!(run(src).unwrap(), 42);
    }

    #[test]
    fn record_field_access_and_subtyping() {
        let src = "
            type Named = { name: Int64 };
            type Pt = { name: Int64, x: Int64, y: Int64 };
            fn nm(w: Named) -> Int64 { return w.name; }
            fn main() -> Int64 {
                let p = Pt { name: 3, x: 10, y: 20 };
                return nm(p) + p.x + p.y;   // 3 + 10 + 20
            }
        ";
        assert_eq!(run(src).unwrap(), 33);
    }

    #[test]
    fn enum_construct_and_match() {
        let src = "
            type Shape = | Circle(Int64) | Square(Int64) | Nil;
            fn area(s: Shape) -> Int64 {
                return match s { Circle(r) => 3 * r * r, Square(w) => w * w, Nil => 0 };
            }
            fn main() -> Int64 { return area(Circle(2)) + area(Square(5)) + area(Nil); }
        ";
        assert_eq!(run(src).unwrap(), 37); // 12 + 25 + 0
    }

    #[test]
    fn dynamic_string_concat_and_len() {
        let src = "fn g(n: String) -> String { return \"Hi, \" + n + \"!\"; } \
                   fn main() -> Int64 { return g(\"Vyrn\").byteLength; }";
        assert_eq!(run(src).unwrap(), 9); // "Hi, Vyrn!" = 9 bytes
    }

    #[test]
    fn to_string_method_renders() {
        // `x.toString()` renders scalars, then `+` concatenates: "42/true" = 7.
        let src = "fn main() -> Int64 { let s = (42).toString() + \"/\" + true.toString(); \
                   return s.byteLength; }";
        assert_eq!(run(src).unwrap(), 7);
    }

    #[test]
    fn contextual_array_literal_is_growable() {
        // A literal in an `Array<T>` position is a growable heap array you can
        // `push` onto — its element count is observable via `.length`.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; \
                   a.push(4); return a.length + a[3]; }"; // 4 + 4
        assert_eq!(run(src).unwrap(), 8);
    }

    #[test]
    fn task_join_method_awaits_result() {
        let src = "fn sq(n: Int64) -> Int64 { return n * n } \
                   fn main() -> Int64 { let t = spawn sq(9); return t.join() }";
        assert_eq!(run(src).unwrap(), 81);
    }

    #[test]
    fn string_eq() {
        let src = "fn main() -> Int64 { \
                   let s = \"hello\"; \
                   if s == \"hello\" { return 1; } return 0; }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn while_loop_and_mut() {
        let src = "
            fn main() -> Int64 {
                let mut i = 0;
                let mut sum = 0;
                while i < 5 {
                    sum = sum + i;
                    i = i + 1;
                }
                return sum;
            }
        ";
        assert_eq!(run(src).unwrap(), 10); // 0+1+2+3+4
    }

    /// Calling an `extern fn` (RFC-0012) traps: the interpreter has no host to
    /// provide it. Declaring one is fine — only the call is the unavailable
    /// effect. Wording is byte-identical to the native trap stub's.
    #[test]
    fn extern_call_traps_with_canonical_wording() {
        let src = "extern fn jsNow() -> Float64\n\
                   fn main() -> Int64 {\n\
                       let t = jsNow()\n\
                       return 0\n\
                   }";
        assert_eq!(
            run(src).unwrap_err(),
            "extern `jsNow` is not available on this target"
        );
        // Declaring without calling is harmless.
        let src = "extern fn jsNow() -> Float64\nfn main() -> Int64 { return 7 }";
        assert_eq!(run(src).unwrap(), 7);
    }

    /// An `export extern fn` (RFC-0012 M2) is a normal function: calling it from
    /// Vyrn runs its body — no trap anywhere. Only body-less imports trap
    /// off-wasm, so an export-extern-using program stays three-way-parity-capable.
    #[test]
    fn export_extern_is_a_normal_call() {
        let src = "export extern fn vyrnAdd(a: Int64, b: Int64) -> Int64 { return a + b }\n\
                   fn main() -> Int64 { return vyrnAdd(40, 2) }";
        assert_eq!(run(src).unwrap(), 42);
    }

    /// The native arena runtime has a fixed 64-slot region stack and traps on
    /// a 65th nested region; the interpreter enforces the identical bound with
    /// the identical message — depth accumulates dynamically across calls.
    #[test]
    fn region_nesting_is_bounded_at_64() {
        let src = |n: i64| {
            format!(
                "fn deep(n: Int64) -> Int64 {{
                     if n == 0 {{ return 0; }}
                     region {{
                         return deep(n - 1);
                     }}
                 }}
                 fn main() -> Int64 {{ return deep({n}); }}"
            )
        };
        // 64 nested regions fill the stack exactly — fine.
        assert_eq!(run(&src(64)).unwrap(), 0);
        // The 65th traps, wording shared with the native runtime.
        assert_eq!(run(&src(65)).unwrap_err(), "region nesting exceeds 64");
    }

    // ---- in-place array mutation (RFC-0011) -----------------------------

    #[test]
    fn index_store_mutates_in_place() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [10, 20, 30]; \
                   a[1] = 25; return a[0] + a[1] + a[2]; }";
        assert_eq!(run(src).unwrap(), 65);
    }

    #[test]
    fn index_store_out_of_bounds_traps() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; a[5] = 9; return 0; }";
        assert_eq!(run(src).unwrap_err(), "array index 5 out of bounds");
    }

    #[test]
    fn pop_returns_last_and_shrinks() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 7]; \
                   let p = match a.pop() { Some(x) => x, None => -1 }; \
                   return p * 100 + a.length; }";
        assert_eq!(run(src).unwrap(), 702); // popped 7, length now 2
    }

    #[test]
    fn pop_on_empty_is_none() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [5]; \
                   let p1 = a.pop(); let p2 = a.pop(); \
                   return match p2 { Some(x) => x, None => -1 }; }";
        assert_eq!(run(src).unwrap(), -1);
    }

    #[test]
    fn swapremove_moves_last_into_slot() {
        // [10, 20, 30, 40]; swapRemove(1) returns 20, moves 40 into slot 1.
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [10, 20, 30, 40]; \
                   let g = a.swapRemove(1); \
                   return g * 1000 + a[0] * 100 + a[1] + a.length; }";
        // g=20 -> 20000; a=[10,40,30]; 10*100=1000; a[1]=40; length=3 -> 21043
        assert_eq!(run(src).unwrap(), 21043);
    }

    #[test]
    fn swapremove_out_of_bounds_traps() {
        let src = "fn main() -> Int64 { let mut a: Array<Int64> = [1, 2, 3]; \
                   let g = a.swapRemove(9); return g; }";
        assert_eq!(run(src).unwrap_err(), "array index 9 out of bounds");
    }

    #[test]
    fn index_store_validated_element_traps_at_runtime() {
        let src = "type Age = Int64 where value >= 18 \
                   fn main() -> Int64 { let mut a: Array<Age> = [Age(20)]; \
                   let mut n = 5; a[0] = n; return 0; }";
        assert_eq!(run(src).unwrap_err(), "validation failed for `Age`");
    }

    // ---- module state (RFC-0013) ---------------------------------------

    #[test]
    fn global_mutation_persists_across_calls() {
        // Each `bump` sees the previous call's write to the shared global.
        let src = "let mut hits = 0 \
                   fn bump() -> Int64 { hits = hits + 1 return hits } \
                   fn main() -> Int64 { let a = bump() let b = bump() let c = bump() \
                                        return a + b + c }";
        assert_eq!(run(src).unwrap(), 6); // 1 + 2 + 3
    }

    #[test]
    fn globals_initialize_in_declaration_order() {
        // `b`'s initializer reads the earlier global `a`.
        let src = "let a = 10 \
                   let b = a + 5 \
                   fn main() -> Int64 { return b }";
        assert_eq!(run(src).unwrap(), 15);
    }

    #[test]
    fn validated_global_traps_at_runtime_on_bad_store() {
        // A non-constant store into a validated global validates at runtime.
        let src = "type Age = Int64 where value >= 18 \
                   let mut a: Age = Age(20) \
                   fn setAge(n: Int64) -> Int64 { a = n return 0 } \
                   fn main() -> Int64 { return setAge(5) }";
        assert_eq!(run(src).unwrap_err(), "validation failed for `Age`");
    }

    #[test]
    fn local_shadows_global_in_interp() {
        // A local `hits` shadows the global; the global stays untouched.
        let src = "let mut hits = 100 \
                   fn f() -> Int64 { let hits = 1 return hits } \
                   fn main() -> Int64 { let a = f() return a + hits }";
        assert_eq!(run(src).unwrap(), 101); // local 1 + global 100
    }

    #[test]
    fn string_global_reads_back() {
        let src = "let banner = \"vyrn\" \
                   fn f() -> Int64 { return banner.byteLength } \
                   fn main() -> Int64 { return f() }";
        assert_eq!(run(src).unwrap(), 4);
    }

    // ---- RFC-0011 addendum: `a[i].field = v` write-through --------------

    #[test]
    fn index_field_write_through_is_visible() {
        // A field write through the array must stick (load-modify-store), and the
        // RHS reads the pre-write element.
        let src = "type P = { x: Int64, y: Int64 } \
                   fn main() -> Int64 { \
                       let mut a: Array<P> = [] \
                       a.push(P { x: 1, y: 2 }) \
                       a.push(P { x: 3, y: 4 }) \
                       a[1].x = 20 \
                       a[0].y = a[0].y + 9 \
                       return a[0].y + a[1].x }"; // 11 + 20 = 31
        assert_eq!(run(src).unwrap(), 31);
    }

    #[test]
    fn index_field_write_through_traps_on_oob_load() {
        // The bounds check on the element LOAD fires with the canonical wording.
        let src = "type P = { x: Int64 } \
                   fn main() -> Int64 { \
                       let mut a: Array<P> = [P { x: 1 }] \
                       a[5].x = 9 \
                       return 0 }";
        assert_eq!(run(src).unwrap_err(), "array index 5 out of bounds");
    }

    // ---- JSON codec (RFC-0018) ------------------------------------------
    // `run` returns an `Int64`, and match arms are single expressions, so these
    // programs fold each assertion into an integer via a tiny `eq` helper.
    const EQ: &str = "fn eq(a: String, b: String) -> Int64 { if a == b { return 1; } return 0; } ";

    #[test]
    fn tojson_canonical_record_order_and_escaping() {
        // Declaration order, no whitespace, minimal escaping.
        let src = "type P = { name: String, age: Int64, ok: Bool } \
                   fn main() -> Int64 { \
                       let p = P { name: \"a\\\"b\", age: 30, ok: true } \
                       if toJson(p) == \"{\\\"name\\\":\\\"a\\\\\\\"b\\\",\\\"age\\\":30,\\\"ok\\\":true}\" { return 1; } \
                       return 0; }";
        assert_eq!(run_json(src).unwrap(), 1);
    }

    #[test]
    fn tojson_omits_none_field_and_bare_option_is_null() {
        let src = "type P = { name: String, nick: Option<String> } \
                   fn main() -> Int64 { \
                       let p = P { name: \"x\", nick: None } \
                       if toJson(p) == \"{\\\"name\\\":\\\"x\\\"}\" { return 1; } \
                       return 0; }";
        assert_eq!(run_json(src).unwrap(), 1);
    }

    #[test]
    fn roundtrip_valid_record() {
        let src = "type Age = Int64 where value >= 0 && value <= 130 \
                   type User = { name: String, age: Age, nick: Option<String> } \
                   fn main() -> Int64 { \
                       let u = User { name: \"Ada\", age: 36, nick: Some(\"A\") } \
                       let s = toJson(u) \
                       return match fromJson(User, s) { \
                           Valid(u2) => u2.age + u2.name.byteLength, \
                           Invalid(iss) => 0 - iss.length, \
                       }; }";
        // age 36 + name length 3 = 39.
        assert_eq!(run_json(src).unwrap(), 39);
    }

    #[test]
    fn exact_large_integer_roundtrips() {
        // Beyond f64's 53-bit exact range — must survive as an exact i64.
        let src = "type W = { n: Int64 } \
                   fn main() -> Int64 { \
                       return match fromJson(W, \"{\\\"n\\\":9007199254740993}\") { \
                           Valid(w) => w.n - 9007199254740992, \
                           Invalid(iss) => 0 - iss.length, \
                       }; }";
        assert_eq!(run_json(src).unwrap(), 1);
    }

    #[test]
    fn decode_unknown_fields_ignored_and_null_option_is_none() {
        let src = "type U = { name: String, nick: Option<String> } \
                   fn main() -> Int64 { \
                       return match fromJson(U, \"{\\\"name\\\":\\\"x\\\",\\\"nick\\\":null,\\\"extra\\\":7}\") { \
                           Valid(u) => match u.nick { Some(s) => 2, None => 1, }, \
                           Invalid(iss) => 0 - iss.length, \
                       }; }";
        assert_eq!(run_json(src).unwrap(), 1);
    }

    #[test]
    fn decode_missing_field_issue_bytes() {
        let src = "type U = { name: String, age: Int64 } \
                   fn main() -> Int64 { \
                       return match fromJson(U, \"{\\\"name\\\":\\\"x\\\"}\") { \
                           Valid(u) => 0, \
                           Invalid(iss) => eq(iss[0].key, \"json.missing\") + eq(iss[0].path, \"age\") \
                               + eq(iss[0].message, \"missing required field `age`\"), \
                       }; }";
        assert_eq!(run_json(&format!("{EQ}{src}")).unwrap(), 3);
    }

    #[test]
    fn decode_type_mismatch_issue_bytes() {
        let src = "type U = { age: Int64 } \
                   fn main() -> Int64 { \
                       return match fromJson(U, \"{\\\"age\\\":\\\"nope\\\"}\") { \
                           Valid(u) => 0, \
                           Invalid(iss) => eq(iss[0].key, \"json.type\") + eq(iss[0].path, \"age\") \
                               + eq(iss[0].message, \"expected integer, found string\"), \
                       }; }";
        assert_eq!(run_json(&format!("{EQ}{src}")).unwrap(), 3);
    }

    #[test]
    fn decode_validation_issue_accumulates_all() {
        // Two failing `where` clauses -> two `validate` issues, both reported.
        let src = "type Age = Int64 where value >= 0 && value <= 130 \
                   type Name = String where value.byteLength >= 1 \
                   type U = { name: Name, age: Age } \
                   fn main() -> Int64 { \
                       return match fromJson(U, \"{\\\"name\\\":\\\"\\\",\\\"age\\\":999}\") { \
                           Valid(u) => 0, \
                           Invalid(iss) => iss.length, \
                       }; }";
        assert_eq!(run_json(src).unwrap(), 2);
    }

    #[test]
    fn decode_validation_issue_bytes() {
        let src = "type Age = Int64 where value >= 0 && value <= 130 \
                   type U = { age: Age } \
                   fn main() -> Int64 { \
                       return match fromJson(U, \"{\\\"age\\\":999}\") { \
                           Valid(u) => 0, \
                           Invalid(iss) => eq(iss[0].key, \"validate\") + eq(iss[0].path, \"age\") \
                               + eq(iss[0].message, \"validation failed for `Age`\"), \
                       }; }";
        assert_eq!(run_json(&format!("{EQ}{src}")).unwrap(), 3);
    }

    #[test]
    fn decode_parse_error_is_single_issue() {
        let src = "type U = { a: Int64 } \
                   fn main() -> Int64 { \
                       return match fromJson(U, \"{ bad\") { \
                           Valid(u) => 0, \
                           Invalid(iss) => iss.length + eq(iss[0].key, \"json.parse\") + eq(iss[0].path, \"\"), \
                       }; }";
        // one parse issue + key match + path match = 3.
        assert_eq!(run_json(&format!("{EQ}{src}")).unwrap(), 3);
    }

    #[test]
    fn decode_enum_payloadless_roundtrip() {
        let src = "type Color = | Red | Green | Blue \
                   type P = { c: Color } \
                   fn main() -> Int64 { \
                       let p = P { c: Green } \
                       let s = toJson(p) \
                       if s == \"{\\\"c\\\":\\\"Green\\\"}\" { \
                           return match fromJson(P, s) { Valid(q) => 1, Invalid(iss) => 0, }; \
                       } \
                       return 5; }";
        assert_eq!(run_json(src).unwrap(), 1);
    }

    // ---- function values (RFC-0023) -------------------------------------

    const TWICE: &str = "fn twice(xs: Array<Int64>, f: fn(Int64) -> Int64) -> Array<Int64> {\n\
         let mut out: Array<Int64> = []\n\
         for x in xs { out.push(f(x)) }\n\
         return out }\n\
         fn sum(xs: Array<Int64>) -> Int64 {\n\
             let mut s = 0  for x in xs { s = s + x }  return s }\n";

    #[test]
    fn lambda_argument_runs() {
        let src =
            format!("{TWICE}fn main() -> Int64 {{ return sum(twice([1, 2, 3], |x| x * 2)) }}");
        assert_eq!(run(&src).unwrap(), 12);
    }

    #[test]
    fn lambda_captures_by_read() {
        let src = format!(
            "{TWICE}fn main() -> Int64 {{ let off = 10  return sum(twice([1, 2, 3], |x| x + off)) }}"
        );
        assert_eq!(run(&src).unwrap(), 36);
    }

    #[test]
    fn named_function_as_value() {
        let src = format!(
            "{TWICE}fn dbl(n: Int64) -> Int64 {{ return n * 2 }}\n\
             fn main() -> Int64 {{ return sum(twice([1, 2, 3], dbl)) }}"
        );
        assert_eq!(run(&src).unwrap(), 12);
    }

    #[test]
    fn passthrough_and_empty_array() {
        let src = format!(
            "{TWICE}fn outer(xs: Array<Int64>, g: fn(Int64) -> Int64) -> Array<Int64> {{ return twice(xs, g) }}\n\
             fn main() -> Int64 {{ let e: Array<Int64> = []  let z = sum(outer(e, |x| x + 1))\n\
             let bump = 5  return z + sum(outer([1, 2], |x| x + bump)) }}"
        );
        // empty → 0; outer([1,2], +5) → [6,7] → 13.
        assert_eq!(run(&src).unwrap(), 13);
    }

    #[test]
    fn generic_map_runs() {
        let src = "fn map<T, U>(xs: Array<T>, f: fn(T) -> U) -> Array<U> {\n\
             let mut out: Array<U> = []  for x in xs { out.push(f(x)) }  return out }\n\
             fn main() -> Int64 {\n\
                 let ys: Array<Int64> = [1, 2, 3]\n\
                 let zs = map(ys, |x| x * x)\n\
                 let mut s = 0  for z in zs { s = s + z }  return s }";
        assert_eq!(run(src).unwrap(), 14);
    }

    // ---- stored function values (RFC-0037) -------------------------------

    #[test]
    fn stored_lambda_in_let_runs() {
        let src = "fn main() -> Int64 { let g: fn(Int64) -> Int64 = |x| x * 2  return g(21) }";
        assert_eq!(run(src).unwrap(), 42);
    }

    #[test]
    fn stored_capture_survives_scope_exit() {
        // The capture is a by-value snapshot at the lambda's evaluation site —
        // it lives inside the value, so it survives the maker's return.
        let src = "fn makeAdder(n: Int64) -> fn(Int64) -> Int64 { return |x| x + n }\n\
             fn main() -> Int64 { let add5 = makeAdder(5)  let add7 = makeAdder(7)\n\
             return add5(10) + add7(10) }";
        assert_eq!(run(src).unwrap(), 32);
    }

    #[test]
    fn stored_capture_is_a_snapshot_not_a_reference() {
        // Reassigning the captured binding after the literal is evaluated is
        // never observed (RFC-0023 capture timing, verbatim in storage).
        let src = "fn main() -> Int64 { let mut n = 1\n\
             let f: fn() -> Int64 = || n\n\
             n = 5\n\
             return f() }";
        assert_eq!(run(src).unwrap(), 1);
    }

    #[test]
    fn stored_values_in_arrays_records_options() {
        let src = "type Ops = { plus: fn(Int64) -> Int64, minus: fn(Int64) -> Int64 }\n\
             fn main() -> Int64 {\n\
             let mut xs: Array<fn(Int64) -> Int64> = []\n\
             xs.push(|x| x * 2)\n\
             xs.push(|x| x + 100)\n\
             let mut s = 0\n\
             for f in xs { s = s + f(10) }\n\
             let ops = Ops { plus: |x| x + 1, minus: |x| x - 1 }\n\
             let p = ops.plus\n\
             let m = ops.minus\n\
             let o: Option<fn(Int64) -> Int64> = Some(|x| x * x)\n\
             let q = match o { Some(f) => f(3), None => 0 }\n\
             return s + p(5) + m(5) + q }";
        // s = 20 + 110 = 130; p(5)=6; m(5)=4; q=9 → 149.
        assert_eq!(run(src).unwrap(), 149);
    }

    #[test]
    fn stored_fn_module_state_and_middleware_chain() {
        // Module state holds closures (read live at call time); a middleware
        // chain matches the RFC's surface: first Some(..) wins.
        let src = "type Middleware = fn(Int64) -> Option<Int64>\n\
             let mut chain: Array<Middleware> = []\n\
             fn add(threshold: Int64) { chain.push(|x| if x > threshold { Some(x * 10) } else { None }) }\n\
             fn runAll(x: Int64) -> Int64 {\n\
                 let mut hit = 0 - 1\n\
                 for m in chain {\n\
                     if hit < 0 { hit = match m(x) { Some(r) => r, None => hit } }\n\
                 }\n\
                 return hit }\n\
             fn main() -> Int64 { add(100)  add(10)  add(0)\n\
             return runAll(50) }";
        // 50 > 100? no. 50 > 10 → Some(500).
        assert_eq!(run(src).unwrap(), 500);
    }

    #[test]
    fn stored_named_fn_and_composition() {
        let src = "fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
             fn main() -> Int64 { let g = dbl  let h = g\n\
             let mut cur: fn(Int64) -> Int64 = h\n\
             return cur(4) }";
        assert_eq!(run(src).unwrap(), 8);
    }

    #[test]
    fn stored_value_flows_into_v1_fn_parameter() {
        // A stored value handed to a v1 `fn`-typed parameter dispatches inside
        // the (interp-dynamic / codegen-specialized) instance.
        let src = format!(
            "{TWICE}fn main() -> Int64 {{ let bump = 3\n\
             let g: fn(Int64) -> Int64 = |x| x + bump\n\
             return sum(twice([1, 2, 3], g)) }}"
        );
        assert_eq!(run(&src).unwrap(), 15);
    }

    #[test]
    fn stored_closure_reads_module_state_live() {
        // Module state is NOT captured — a read inside the body resolves live.
        let src = "let mut base: Int64 = 1\n\
             fn main() -> Int64 { let f: fn() -> Int64 = || base\n\
             base = 41\n\
             return f() + 1 }";
        assert_eq!(run(src).unwrap(), 42);
    }

    #[test]
    fn generic_function_stores_fn_values_per_instantiation() {
        // A stored fn type mentioning `T` monomorphizes with the body: each
        // instantiation gets its own signature (and, in codegen, its own enum).
        let src = "fn relay<T>(x: T) -> T {\n\
             let f: fn(T) -> T = |v| v\n\
             return f(x) }\n\
             fn main() -> Int64 {\n\
             let n = relay(41)\n\
             let s = relay(\"ok\")\n\
             if s == \"ok\" { return n + 1 }\n\
             return 0 }";
        assert_eq!(run(src).unwrap(), 42);
    }

    #[test]
    fn module_state_of_fn_type_with_init_order() {
        // A directly fn-typed module-state binding (RFC-0029 init order):
        // the initializer lambda is replaced at runtime; reads are live.
        let src = "let mut cur: fn(Int64) -> Int64 = |x| x + 1\n\
             fn dbl(n: Int64) -> Int64 { return n * 2 }\n\
             fn main() -> Int64 {\n\
             let before = cur(10)\n\
             cur = dbl\n\
             return before + cur(10) }";
        assert_eq!(run(src).unwrap(), 31);
    }

    #[test]
    fn stored_value_into_generic_v1_fn_parameter() {
        // A stored value handed to a GENERIC higher-order function: the
        // outbound type parameter solves from the stored signature's return.
        let src = "fn map<T, U>(xs: Array<T>, f: fn(T) -> U) -> Array<U> {\n\
             let mut out: Array<U> = []  for x in xs { out.push(f(x)) }  return out }\n\
             fn main() -> Int64 {\n\
             let xs: Array<Int64> = [1, 2]\n\
             let g: fn(Int64) -> Int64 = |x| x * 3\n\
             let ys = map(xs, g)\n\
             return ys[0] + ys[1] }";
        assert_eq!(run(src).unwrap(), 9);
    }

    #[test]
    fn trap_inside_stored_closure_has_canonical_wording() {
        let src = "fn main() -> Int64 { let f: fn(Int64) -> Int64 = |x| 10 / x\n\
             return f(0) }";
        let err = run(src).unwrap_err();
        assert!(err.contains("division by zero"), "{err}");
    }

    #[test]
    fn stored_lambda_coerces_arguments_to_signature_types() {
        // The declared slot type supplies the parameter coercions: a UInt8
        // parameter wraps exactly as a named callee's would.
        let src = "fn main() -> Int64 { let f: fn(UInt8) -> Int64 = |b| Int64(b + 200)\n\
             return f(100) }";
        // 100 + 200 wraps at the UInt8 parameter's width: 300 & 0xFF = 44.
        assert_eq!(run(src).unwrap(), 44);
    }

    // ---- `if` as an expression (RFC-0030) --------------------------------

    #[test]
    fn if_expression_yields_the_taken_branch() {
        let src = "fn main() -> Int64 {\n\
             let x = if 2 > 1 { 10 } else { 20 }\n\
             return x }";
        assert_eq!(run(src).unwrap(), 10);
    }

    #[test]
    fn if_expression_chain_selects_the_matching_arm() {
        let src = "fn tier(s: Int64) -> Int64 {\n\
             return if s >= 90 { 3 } else if s >= 50 { 2 } else { 1 } }\n\
             fn main() -> Int64 { return tier(95) + tier(60) * 10 + tier(10) * 100 }";
        // 3 + 2*10 + 1*100 = 123
        assert_eq!(run(src).unwrap(), 123);
    }

    #[test]
    fn only_the_taken_branch_evaluates() {
        // `boom()` traps; it sits in the untaken branch and must never run, so the
        // program returns cleanly. If both branches evaluated, this would trap.
        let src = "fn boom() -> Int64 { let a: Array<Int64> = [1]  return a[99] }\n\
             fn main() -> Int64 {\n\
             let x = if true { 7 } else { boom() }\n\
             return x }";
        assert_eq!(run(src).unwrap(), 7);
    }

    #[test]
    fn if_expression_nests_and_composes() {
        let src = "fn main() -> Int64 {\n\
             let n = if false { 1 } else { if true { 2 } else { 3 } }\n\
             let xs: Array<Int64> = [if n == 2 { 100 } else { 0 }, 5]\n\
             return xs[0] + xs[1] }";
        assert_eq!(run(src).unwrap(), 105);
    }
    /// Allocation failure is a Vyrn trap in the wording the other two engines
    /// print, not Rust's own abort (RFC-0081).
    ///
    /// The end-to-end measurement is `s = s + s` forty times: `error: out of
    /// memory` on exit 1, where it used to print `memory allocation of
    /// 68719476736 bytes failed` on exit 127. That run cannot BE the test.
    /// Whether a 64 GiB request is refused or lazily promised is the host's
    /// decision — the same reason the parity suite never saw this — and a test
    /// that got far enough to be refused would have committed several GiB first,
    /// which on a CI runner is an OOM kill rather than a trap.
    ///
    /// `try_reserve` past `isize::MAX` is refused by the allocation layer itself
    /// without the allocator being asked, so it is the same answer on every
    /// machine, and it is the same `Err` a refusal produces.
    ///
    /// This fails in both directions: with `reserve` in place of `try_reserve`
    /// these calls PANIC with `capacity overflow` rather than returning, which is
    /// a failed test — and is exactly the abort the trap replaced.
    #[test]
    fn an_allocation_that_cannot_be_served_is_a_trap_not_an_abort() {
        let mut s = String::from("x");
        match reserve_str(&mut s, usize::MAX) {
            Err(Ctrl::Err(m)) => assert_eq!(m, "out of memory"),
            other => panic!("expected a trap, got {other:?}"),
        }
        let mut v: Vec<Val> = Vec::new();
        match reserve_vec(&mut v, usize::MAX) {
            Err(Ctrl::Err(m)) => assert_eq!(m, "out of memory"),
            other => panic!("expected a trap, got {other:?}"),
        }
        // The CLI renders a trap as `error: {msg}` on stderr and exits 1, so
        // these four words ARE the line the direct backend's `malloc` traps
        // with, which is in turn the native shim's. Asserting against the shim's
        // constant is not possible in the direction the crates depend.
    }

    #[test]
    fn a_stepped_stream_runs_its_producer_only_when_asked() {
        // RFC-0075 M2b, in the engine with no IR to inspect. `tick` never answers
        // `None`, so this program does not terminate under the representation M1
        // shipped — it terminates here, and the count is the reason: eight `next`
        // calls out of a feed with no end.
        //
        // The cursor is module state rather than a slot, because these tests run
        // one file with no `std`: the slab a real producer's cursor comes from is
        // `std/stream`'s since RFC-0090 M3, and what the ENGINE owes is to hand
        // the two words back to the step and to call it once with `closing`.
        let src = "let mut steps = 0 \
                   let mut cur = 0 \
                   fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> { \
                   if cl { return None } \
                   let n = cur cur = n + 1 \
                   steps = steps + 1 return Some(n) } \
                   fn main() -> Int64 { \
                     let mut seen = 0 \
                     for v in fromStep(0, 1, tick) { seen = seen + v if v == 7 { break } } \
                     return steps }";
        assert_eq!(crate::run(src), Ok(8));
    }

    /// RFC-0075 M2c's `map`, spelled the way `std/stream` spells it: no
    /// `for … in` at all, a step that reads its source with `pullAt`, and a
    /// wrapper that owns the box the source moved into. Its closing call takes
    /// the source back out and closes it, which is the whole of what M2c used to
    /// do inside the runtime's walk.
    const LMAP: &str = "fn lmap<T, U>(s: Stream<T>, f: fn(T) -> U) -> Stream<U> { \
                        let a = boxStream(s) \
                        let g: fn(T) -> U = f \
                        let step: fn(Int64, Int64, Bool) -> Option<U> = |sl, gn, cl| { \
                        if cl { let src: Stream<T> = unboxStream(a) close(src) return None } \
                        let x: Option<T> = pullAt(a) \
                        if let Some(v) = x { return Some(g(v)) } return None } \
                        return fromStep(0, 1, step) } ";

    #[test]
    fn a_wrapper_asks_its_source_once_per_element_it_is_asked_for() {
        // The milestone, in the engine with no IR to inspect: `tick` has no end,
        // so this program does not terminate if `lmap` drains. It terminates, and
        // the count says nothing was read ahead — four elements out, four asks
        // in.
        let src = format!(
            "let mut steps = 0 \
             let mut cur = 0 \
             fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> {{ \
             if cl {{ return None }} \
             let n = cur cur = n + 1 \
             steps = steps + 1 return Some(n) }} \
             fn double(n: Int64) -> Int64 {{ return n * 2 }} \
             {LMAP} \
             fn main() -> Int64 {{ \
               let mut seen = 0 \
               for v in lmap(fromStep(0, 1, tick), double) {{ seen = seen + v \
                 if v == 6 {{ break }} }} \
               return steps }}"
        );
        assert_eq!(crate::run(&src), Ok(4));
    }

    #[test]
    fn releasing_a_chain_closes_one_stream_per_link() {
        // Three wrappers over one producer is four streams and four releases, and
        // the walk M2c ran inside the runtime is now the wrappers closing their
        // own sources. Each `unboxStream` empties its box, so a release that ran
        // twice would trap on the second — and one that stopped early would leave
        // a box behind, which the count below catches: 10 000 cycles, four closes
        // each, and the producer's own closing call is what `closed` counts.
        let src = format!(
            "let mut cur = 0 \
             let mut closed = 0 \
             fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> {{ \
             if cl {{ closed = closed + 1 return None }} \
             let n = cur cur = n + 1 return Some(n) }} \
             fn double(n: Int64) -> Int64 {{ return n * 2 }} \
             {LMAP} \
             fn main() -> Int64 {{ let mut i = 0 \
               while i < 10000 {{ \
                 let s = lmap(lmap(lmap(fromStep(0, 1, tick), double), double), double) \
                 close(s) i = i + 1 }} \
               return closed }}"
        );
        assert_eq!(crate::run(&src), Ok(10000));
    }

    #[test]
    fn pull_at_an_address_with_no_stream_traps() {
        // `pullAt` is a builtin and an address is an ordinary `Int64`, so nothing
        // stops a program from calling it on a number. The wording is the one the
        // compiled backends print.
        let src = "fn main() -> Int64 { let x: Option<Int64> = pullAt(24) return 0 }";
        match crate::run(src) {
            Err(e) => assert!(e.contains("no stream in this box"), "unexpected trap: {e}"),
            other => panic!("expected a trap, got {other:?}"),
        }
        // And an address that HELD a stream is empty once it is taken out, which
        // is what makes a second release a trap rather than a second owner.
        let src = "fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> { return None } \
                   fn main() -> Int64 { let a = boxStream(fromStep(0, 1, tick)) \
                     let s: Stream<Int64> = unboxStream(a) close(s) \
                     let t: Stream<Int64> = unboxStream(a) close(t) return 0 }";
        match crate::run(src) {
            Err(e) => assert!(e.contains("no stream in this box"), "unexpected trap: {e}"),
            other => panic!("expected a trap, got {other:?}"),
        }
    }

    #[test]
    fn every_released_stream_asks_its_step_to_close_exactly_once() {
        // RFC-0075's "10 000 open-then-abandon cycles" row, as the engine can see
        // it: the release is the step's closing call, so counting those counts
        // releases. A `close` that did not run would leave the count short and a
        // double release would run it over.
        let src = "let mut closed = 0 \
                   fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> { \
                   if cl { closed = closed + 1 return None } return Some(sl) } \
                   fn main() -> Int64 { let mut i = 0 \
                     while i < 100000 { let s = fromStep(i, 1, tick) close(s) i = i + 1 } \
                     return closed }";
        assert_eq!(crate::run(src), Ok(100000));
    }

    #[test]
    fn next_step_that_serves_a_new_stream_releases_the_old_one() {
        // RFC-0074 M3a regression: a pull takes the parked stream OUT of
        // `live`, so a producer step that calls `serveStream` mid-pull parks a
        // SECOND stream there without tripping the "already opened" trap.
        // Storing the pulled stream back unconditionally used to drop that
        // second stream unreleased; the newest producer now wins and the
        // displaced one goes through the ordinary release.
        let src = "fn tick(c: Int64, g: Int64, cl: Bool) -> Option<String> { \
                   if cl { return None } \
                   if c == 0 { let xs: Array<String> = [\"second\"] serveStream(fromArray(xs)) return Some(\"first\") } \
                   return None } \
                   fn open() -> Unit { serveStream(fromStep(0, 0, tick)) } \
                   fn main() -> Int64 { return 0 }";
        let program = crate::check(src).unwrap();
        let interp = super::new_interp(&program, &[]).unwrap();
        interp.init_globals(&program).unwrap();
        // Park the served stream exactly as a `handle` that called
        // `serveStream` does (RFC-0074 M3a), then drive the host's pulls.
        interp.call("open", &[]).unwrap();
        // First pull: the step serves a second stream before answering.
        match super::serve_call(&interp, super::ServeCall::Next) {
            Ok(super::ServeAnswer::Frame(Some(f))) => assert_eq!(f, "first"),
            _ => panic!("expected the first frame"),
        }
        // The newcomer must still be live — the pulled stream was released,
        // not stored back over it — so the NEXT frame comes from it.
        match super::serve_call(&interp, super::ServeCall::Next) {
            Ok(super::ServeAnswer::Frame(Some(f))) => assert_eq!(f, "second"),
            _ => panic!("expected the second stream's frame"),
        }
        // And Close releases the survivor cleanly.
        match super::serve_call(&interp, super::ServeCall::Close) {
            Ok(super::ServeAnswer::Released) => {}
            _ => panic!("expected Released"),
        }
    }
}
