//! The harness the corpus tests share: where the examples are, how a backend process
//! runs (cwd, stdin and argv fixtures, fixed clock and seed), how output is normalized,
//! and which examples do not take part. One home keeps every test agreeing on what
//! "the same run" means.

#![allow(dead_code)] // each test binary uses a subset.

use std::path::{Path, PathBuf};
use std::process::{Command, Stdio};

/// Examples expected to diverge, with the reason. Shrink it; never grow it silently.
pub const KNOWN_DIVERGENT: &[(&str, &str)] = &[];

/// Examples that are intentional compile errors: excluded from run-time parity and
/// asserted to fail `vyrn check` by [`expected_check_failures_do_fail`]. The third
/// field is a substring the diagnostic must contain.
pub const EXPECTED_CHECK_FAILURE: &[(&str, &str, &str)] = &[
    (
        "copyfromowned.vyrn",
        "`copyFrom` overwrites by bytes, so an owning element that is          overwritten would never be released",
        "overwrites the receiver's elements by bytes",
    ),
    (
        "appendowned.vyrn",
        "`append` is a byte copy, so an element type that owns heap          is refused — copying one by bytes gives two arrays one buffer",
        "copies its source's elements by bytes",
    ),
    (
        "clearowned.vyrn",
        "`clear` forgets its elements, so an element type          that owns heap is refused — forgetting one leaks it",
        "forgets its elements without releasing them",
    ),
    (
        "floatkey.vyrn",
        "float keys are refused by name — NaN != NaN breaks the          reflexivity a key needs",
        "holds a float",
    ),
    (
        "heapkey.vyrn",
        "a Map key is String, Int64, or a heapless Hashable user          type; an Array key is none of these",
        "or a heapless `Hashable` type",
    ),
    (
        "nohashkey.vyrn",
        "a heapless user record keys a map only once it declares          `impl Hashable` — the refusal names the missing obligation",
        "once it declares the obligation",
    ),
    (
        "payloadkey.vyrn",
        "a payload-bearing enum key waits for a packer something          real demands; fieldless enums and scalar records key today",
        "waits for real demand",
    ),
    (
        "heapfieldkey.vyrn",
        "a key is heapless all the way down — a String field          disqualifies the record around it",
        "heapless all the way down",
    ),
    (
        "blockvalarm.vyrn",
        "a match used as a value keeps single-expression arms; a          block arm exists only in statement position",
        "a block arm needs statement position",
    ),
    (
        "dropparam.vyrn",
        "`drop` on a bare type parameter would launder the record rule through          generic trampolines, and no instance check runs on a generic body",
        "is a type parameter",
    ),
    // One row per load-bearing assumption of the release-algorithm proofs: if
    // one compiles, a theorem's hypothesis is false.
    (
        "a1_afterjoin.vyrn",
        "A1's reach: a read AFTER the join of a conditional move (Theorem 4's          first case). The branch-disjoint read is accepted; this is not",
        "already consumed by",
    ),
    (
        "a2_capture.vyrn",
        "A2's capture clause: a closure over a `read` parameter (Lemma 3's          bracketing dies if this compiles)",
        "may not be captured by a closure",
    ),
    (
        "a2_capture_escape.vyrn",
        "A2, the escaping form: the capturing closure is returned",
        "may not be captured by a closure",
    ),
    (
        "a6_reassign.vyrn",
        "A6: parameters are not reassignable — borrows are path-invariant          because nothing can overwrite one",
        "cannot assign to",
    ),
    (
        "excl_alias.vyrn",
        "A7: a `modify` borrow is exclusive — one variable as `modify` and          `read` in one call is refused",
        "borrow is exclusive",
    ),
    (
        "validate_compile.vyrn",
        "compile-time rejection of a provably-invalid constant",
        "does not satisfy",
    ),
    (
        "polyrecursion.vyrn",
        "polymorphic recursion — a generic that calls itself with a bigger type has no \
         monomorphization fixed point (audit A5.2). `check` used to say `ok` and `build` \
         then ran forever printing nothing",
        "past the instantiation limit",
    ),
    (
        "protocol_overlap.vyrn",
        "two impls of one protocol for one type constructor",
        "collides with `impl<T> Show for Option<T>` (line",
    ),
    (
        "assoctype_unbound.vyrn",
        "an impl that omits an associated type the protocol declares, and one that \
         binds a name it does not",
        "does not bind the associated type `Output`",
    ),
    (
        "protocol_conformance.vyrn",
        "an impl whose methods do not have the signatures the protocol declared — a \
         wrong return type and a wrong parameter type",
        "it declares `fn area(self) -> Int64`, this provides `fn area(self) -> Bool`",
    ),
    (
        "protocol_incomplete.vyrn",
        "an impl missing a method its protocol declares — `vyrn check` \
         used to pass this and the mangled `Shape__Sq__name` surfaced at run time",
        "does not provide `fn name(self) -> String`",
    ),
    (
        "protocol_extra.vyrn",
        "an impl providing methods its protocol does not declare — a \
         typo beside the real method, and a plainly extra one; both used to compile \
         into a mangled symbol nothing could ever dispatch to",
        "provides `fn aera(self) -> Int64`, which protocol `Shape` does not declare",
    ),
    (
        "protocol_scalar.vyrn",
        "a protocol implemented for a validated scalar",
        "erases to `Int64` at run time",
    ),
    (
        "stream_abandoned.vyrn",
        "a `Stream` acquired and abandoned — the `trpc#6193` shape",
        "`events` is a `Stream<Int64>` and is never disposed",
    ),
    (
        "stream_combinator_abandoned.vyrn",
        "an abandoned std/stream combinator result — the obligation \
         must not launder through `map`",
        "`mapped` is a `Stream` and is never disposed",
    ),
    (
        "streammove_after.vyrn",
        "an array read after `fromArray` took it — the frame used to \
         release a buffer the stream had already freed, and the native binary \
         corrupted its heap",
        "`xs` was moved here into `fromArray(..)`",
    ),
    (
        "mapkeyborrowed.vyrn",
        "a borrowed array element handed to `m[k] = v` — the map takes the key, so the \
         array and the map both released one buffer and the native binary exited 127; \
         the map LITERAL refused the same borrow all along, which made it one fact with \
         two verdicts. mapkeyowned.vyrn is the shape that runs",
        "`ks[i]` may not be stored into `m`",
    ),
    (
        "consume_borrowed.vyrn",
        "a `read` parameter handed to a `consume` parameter — the frame gave away \
         what it does not own, and the native binary exited 0xC0000374",
        "`ys` may not be passed to a `consume` parameter via `take(..)`",
    ),
    (
        "region_consume.vyrn",
        "a value a `region` allocated, handed to a `consume` parameter — \
         the escape guard watched named stores, so `kept.push(s)` inside the region was \
         refused and `keep(s)`, which stores one frame down, was not; the native binary \
         printed freed memory that changed from run to run",
        "cannot hand a heap value to argument 1 of `keep`, which is `consume`, inside a \
         `region`",
    ),
    (
        "task_abandoned.vyrn",
        "a `Task` acquired and abandoned, one joined twice, and one discharged on a \
         single branch — the three refusals a `Stream` gets, over the \
         type that leaks an operating-system handle rather than bytes",
        "`t` is a `Task` and is never disposed",
    ),
    (
        "mustuse_abandoned.vyrn",
        "a USER type's must-use obligation, abandoned — the same three \
         rejections `Stream` gets, reached through `impl MustUse for Txn` and naming \
         the user's type",
        "`a` is a `Txn` and is never disposed",
    ),
    (
        "gentablefail.vyrn",
        "a GENERATOR's own error, at the line and column of the input file it read \
         — the rule is `lib/gen_table.vyrn`'s, in Vyrn, and the compiler \
         knows nothing about tables",
        "data/dupe.tbl:4:1: column `id` is declared twice",
    ),
    (
        "loopalias.vyrn",
        "a join arm that hands out a name bound outside an enclosing loop \
         — the arm stands the outer name down \
         and the `let` owns the result, so the back edge freed one buffer once per \
         turn and the native binary exited 134 under `VYRN_LEAK_CHECK=1`",
        "may not be handed out of an arm inside a loop",
    ),
];

/// Examples whose behavior only a browser page provides: the `extern` `vyrn` import
/// namespace (`web/externdemo.html` runs it). Only the harnesses that drive an outside
/// tool skip these, because wasm2c and the `wasmtime` CLI do not know the namespace.
/// `tests/fixtures.rs` runs them: its embedded host answers with the canonical
/// `extern` trap. The build is pinned by its row in `tests/pins/wasm-sha256.tsv`.
pub const WASM_ONLY: &[(&str, &str)] = &[(
    "externdemo.vyrn",
    "calls `extern` fns; only the browser provides the `vyrn` namespace",
)];

/// Project entries under `examples/*/` that their artifact's floor refuses,
/// with text the refusal must contain. `tests/floor.rs` asserts each; the corpus
/// harnesses that walk project entries skip them.
pub const EXPECTED_PROJECT_CHECK_FAILURE: &[(&str, &str, &str)] = &[
    (
        "leak/client/boot.vyrn",
        "the browser artifact reaches a file reader three          hops away, and the chain is the diagnostic",
        "`readFile` needs `fs`; target `browser` has no filesystem",
    ),
    (
        "listing/client/boot.vyrn",
        "a browser artifact that lists a directory degrades          to the canonical `Err` on a page, so the floor's `fs` row carries `listDir`",
        "`listDir` needs `fs`; target `browser` has no filesystem",
    ),
];

/// Returns the one function in `src`'s module whose body contains `marker`, as WAT
/// (`vyrn emit-wat`). The module has no name section, so a function is found by
/// content. `wasmprinter` opens each `(func` at two spaces and closes it at the same
/// column, which makes the slice exact. A marker in two functions or none fails.
pub fn wat_func_containing(dir: &Path, name: &str, src: &str, marker: &str) -> String {
    let file = dir.join(format!("{name}.vyrn"));
    std::fs::write(&file, src).unwrap();
    let out = vyrn()
        .arg("emit-wat")
        .arg(&file)
        .output()
        .expect("vyrn emit-wat");
    assert!(out.status.success(), "{}", norm(&out.stderr));
    let wat = norm(&out.stdout);
    let bodies: Vec<&str> = wat
        .split("\n  (func ")
        .skip(1)
        .map(|f| &f[..f.find("\n  )").expect("unterminated function")])
        .filter(|f| f.contains(marker))
        .collect();
    assert_eq!(
        bodies.len(),
        1,
        "expected exactly one function containing `{marker}`, found {}",
        bodies.len()
    );
    bodies[0].to_string()
}

/// Returns the whole module as WAT.
pub fn wat_of(dir: &Path, name: &str, src: &str) -> String {
    let file = dir.join(format!("{name}.vyrn"));
    std::fs::write(&file, src).unwrap();
    let out = vyrn()
        .arg("emit-wat")
        .arg(&file)
        .output()
        .expect("vyrn emit-wat");
    assert!(out.status.success(), "{}", norm(&out.stderr));
    norm(&out.stdout)
}

pub fn examples_dir() -> PathBuf {
    // vyrn-cli/ -> compiler/ -> repo root -> examples/
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../../examples")
        .canonicalize()
        .unwrap()
}

pub fn vyrn() -> Command {
    Command::new(env!("CARGO_BIN_EXE_vyrn"))
}

/// Returns a scratch directory unique to this process and call, removed when the test
/// passes. Concurrent runs must not share artifacts, or one reports a divergence the
/// other produced.
pub fn scratch(tag: &str) -> Scratch {
    // Keeps two calls with one tag in one run apart.
    static NTH: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
    let nth = NTH.fetch_add(1, std::sync::atomic::Ordering::Relaxed);
    let path = std::env::temp_dir().join(format!("vyrn-{tag}-{}-{nth}", std::process::id()));
    // Pids are reused: a stale `.exe` here would pass for this run's build.
    let _ = std::fs::remove_dir_all(&path);
    std::fs::create_dir_all(&path).expect("create scratch dir");
    Scratch(path)
}

/// The directory [`scratch`] hands out; derefs to a `Path`.
pub struct Scratch(PathBuf);

impl std::ops::Deref for Scratch {
    type Target = Path;
    fn deref(&self) -> &Path {
        &self.0
    }
}

impl AsRef<Path> for Scratch {
    fn as_ref(&self) -> &Path {
        &self.0
    }
}

impl Drop for Scratch {
    fn drop(&mut self) {
        // A failing test keeps its artifacts as evidence.
        if !std::thread::panicking() {
            let _ = std::fs::remove_dir_all(&self.0);
        }
    }
}

/// Describes where two engines' output for one stream first differs: the line, the
/// two lines and their context. `None` only when the bytes are equal. A difference
/// the line view cannot show (a trailing newline, a CR) is reported by byte offset.
pub fn first_diff(stream: &str, a_name: &str, a: &str, b_name: &str, b: &str) -> Option<String> {
    if a == b {
        return None;
    }
    let (al, bl): (Vec<&str>, Vec<&str>) = (a.lines().collect(), b.lines().collect());
    let counts = format!("({a_name} {} lines, {b_name} {} lines)", al.len(), bl.len());
    let n = (0..al.len().max(bl.len())).find(|&i| al.get(i) != bl.get(i));
    let Some(n) = n else {
        // The lines are equal and the bytes are not: a trailing newline, or a CR
        // `lines()` stripped.
        let at = a.bytes().zip(b.bytes()).position(|(x, y)| x != y);
        let at = at.unwrap_or(a.len().min(b.len()));
        // The byte itself, not a slice from `at`: an offset inside a multi-byte
        // character is not a `str` boundary, and this has to print either way.
        let byte = |s: &str| match s.as_bytes().get(at) {
            Some(x) => format!("{x:#04x}"),
            None => "<end of output>".to_string(),
        };
        return Some(format!(
            "  {stream}: the {} lines are equal, the bytes are not — first differs at byte {at} {counts}\n    \
             {a_name}: {}\n    {b_name}: {}\n",
            al.len(),
            byte(a),
            byte(b),
        ));
    };
    let missing = "<no such line>";
    let mut out = format!("  {stream}: first differs at line {} {counts}\n", n + 1);
    // Lines before `n` are equal in both, so they print once.
    for i in n.saturating_sub(2)..n {
        out.push_str(&format!("     same {:>5} | {}\n", i + 1, al[i]));
    }
    // After the divergence each engine prints its own next lines.
    for (who, lines) in [(a_name, &al), (b_name, &bl)] {
        for i in n..(n + 3).min(lines.len().max(n + 1)) {
            out.push_str(&format!(
                "  {who:>7} {:>5} | {}\n",
                i + 1,
                lines.get(i).unwrap_or(&missing)
            ));
        }
    }
    Some(out)
}

pub fn norm(bytes: &[u8]) -> String {
    String::from_utf8_lossy(bytes).replace("\r\n", "\n")
}

/// Returns a run's stderr without compile-time warnings. `vyrn run`
/// compiles in the same process, so its warnings share the program's stream; the
/// native and wasm columns run a built artifact and never warn. A compile error never
/// reaches a comparison, and a runtime trap is program output.
pub fn runtime_err(bytes: &[u8]) -> String {
    let mut out = String::new();
    let mut in_warning = false;
    for line in norm(bytes).split_inclusive('\n') {
        if line.contains(": warning: ") {
            in_warning = true;
            continue;
        }
        // A warning's `  note:` continuation belongs to it.
        if in_warning && line.starts_with("  note: ") {
            continue;
        }
        in_warning = false;
        out.push_str(line);
    }
    out
}

/// The clock and seed every backend gets, so a time or random example stays
/// byte-identical: `now()` returns these epoch milliseconds (2023-11-14T22:13:20Z)
/// and `randomSeed()` this seed.
pub const FIXED_TIME: &str = "1700000000000";
pub const FIXED_SEED: &str = "424242";

/// Runs `cmd` in `dir` with stdin from `stdin_fixture` when it exists, else closed so a
/// `readLine()` example cannot hang, and with the fixed clock and seed. The
/// wasm run forwards those to the guest with `--env`.
pub fn run_io(mut cmd: Command, dir: &Path, stdin_fixture: &Path) -> std::process::Output {
    cmd.current_dir(dir);
    cmd.env("VYRN_FIXED_TIME", FIXED_TIME);
    cmd.env("VYRN_FIXED_SEED", FIXED_SEED);
    if stdin_fixture.exists() {
        cmd.stdin(std::fs::File::open(stdin_fixture).expect("open stdin fixture"));
    } else {
        cmd.stdin(Stdio::null());
    }
    cmd.output().expect("run backend")
}

/// Reads an example's program arguments: one token per line, so a token may
/// hold spaces. Every route gets the same argv; no fixture means none.
pub fn read_args(args_fixture: &Path) -> Vec<String> {
    if !args_fixture.exists() {
        return Vec::new();
    }
    let text = std::fs::read_to_string(args_fixture).expect("read args fixture");
    text.lines().map(|l| l.to_string()).collect()
}

/// Returns a `wasmtime` to run a module under, the only tool the wasm column needs.
/// The resolver chooses it: `$VYRN_WASMTIME`, then the pin in the root
/// `vyrn.json`, then the `tools/` walk.
pub fn wasmtime() -> Option<PathBuf> {
    let root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../..");
    let found = vyrn_codegen::toolchain::find_wasmtime_from(&root);
    require_tools("wasmtime", "VYRN_WASMTIME", found)
}

/// Turns a missing tool from a skip into a failure when `VYRN_REQUIRE_TOOLS` is set.
/// `vyrn-codegen/tests/` shares the one statement.
pub use vyrn_codegen::toolchain::require_tools;

/// Whether `VYRN_PIN=write` asks every pin to rewrite its file instead of
/// asserting it. Panics on any other value, so a typo cannot pass as a check.
pub fn pin_write() -> bool {
    let mode = std::env::var("VYRN_PIN").unwrap_or_default();
    assert!(
        matches!(mode.as_str(), "" | "write"),
        "VYRN_PIN must be unset or `write`, not {mode:?}"
    );
    mode == "write"
}

/// Holds a census table equal to `tests/pins/<name>.tsv`: `header`, then one
/// tab-separated line per row. Under `VYRN_PIN=write` it writes the file.
///
/// A pin is data a gate writes, so a rebase never merges one by hand:
/// `.gitattributes` keeps either side, this names the stale line, and the
/// write mode fixes it.
pub fn pin(name: &str, header: &str, rows: impl IntoIterator<Item = String>) {
    let mut got = format!("{header}\n");
    for row in rows {
        got.push_str(&row);
        got.push('\n');
    }
    let file = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("tests/pins")
        .join(format!("{name}.tsv"));
    if pin_write() {
        std::fs::write(&file, &got).expect("write the pin");
        return;
    }
    let want = std::fs::read_to_string(&file).unwrap_or_default();
    if let Some(diff) = first_diff(name, "pinned", &want, "counted", &got) {
        panic!("tests/pins/{name}.tsv has moved; rewrite it with VYRN_PIN=write\n{diff}");
    }
}

/// A pin row: `label`, then each count, tab-separated.
pub fn pin_row(label: &str, counts: &[usize]) -> String {
    let cells: Vec<String> = counts.iter().map(|n| n.to_string()).collect();
    format!("{label}\t{}", cells.join("\t"))
}

/// Returns a census's sections of `file` as `(index, first line, last line)`,
/// one-based and inclusive, one per anchor in order.
///
/// An anchor names an item by its head: `fn name`, `struct Name`, `impl Parser`,
/// `crate::m!`. It matches the first line after the previous anchor's that starts
/// with it, with whitespace runs collapsed and visibility ignored, so a changed
/// signature leaves the table alone. A section runs from its anchor's doc comment
/// and attributes to the line before the next section's; an anchor that is itself
/// a comment starts at that comment. Panics on an anchor no later line matches and
/// on an empty section, so the sections tile the file.
pub fn census_spans<'a>(
    file: &str,
    lines: &[String],
    anchors: impl IntoIterator<Item = &'a str>,
) -> Vec<(usize, usize, usize)> {
    fn head(l: &str) -> String {
        let l = l.split_whitespace().collect::<Vec<_>>().join(" ");
        let rest = l.strip_prefix("pub ").or_else(|| {
            l.strip_prefix("pub(")
                .and_then(|r| r.split_once(") ").map(|(_, r)| r))
        });
        rest.map_or(l.clone(), str::to_string)
    }
    let ident = |c: char| c.is_alphanumeric() || c == '_';
    let heads: Vec<String> = lines.iter().map(|l| head(l)).collect();
    let mut starts = Vec::new();
    let mut from = 0;
    for at in anchors {
        let want = head(at);
        let hit = (from..lines.len())
            .find(|&i| {
                heads[i]
                    .strip_prefix(want.as_str())
                    .is_some_and(|rest| !(rest.starts_with(ident) && want.ends_with(ident)))
            })
            .unwrap_or_else(|| {
                panic!("the census anchor `{at}` names no line of {file} after the one before it")
            });
        let mut start = hit;
        if !want.starts_with("//") {
            while start > 0 {
                let t = lines[start - 1].trim_start();
                if !(t.starts_with("//") || t.starts_with("#[")) {
                    break;
                }
                start -= 1;
            }
        }
        starts.push((at, start));
        from = hit + 1;
    }
    let mut out = Vec::new();
    for i in 0..starts.len() {
        let first = if i == 0 { 0 } else { starts[i].1 };
        let last = starts.get(i + 1).map_or(lines.len(), |s| s.1);
        assert!(
            first < last,
            "census section `{}` of {file} is empty",
            starts[i].0
        );
        out.push((i, first + 1, last));
    }
    out
}

/// Runs `vyrn args` in `cwd` on one thread with the allocation counter armed and
/// returns its table as `(phase, allocations, bytes requested)`, in the table's order. Needs a
/// binary built with `--features allocs`. `cache` is the generator cache: a
/// warm cache is the only state a run may start from, so callers run twice.
pub fn alloc_phases(args: &[&str], cwd: &Path, cache: &Path) -> Vec<(String, u64, u64)> {
    let out = vyrn()
        .args(args)
        .current_dir(cwd)
        .env("VYRN_BUILD_PROFILE", "allocs")
        .env("VYRN_THREADS", "1")
        .env("VYRN_GEN_CACHE_DIR", cache)
        .output()
        .expect("run vyrn");
    let err = String::from_utf8_lossy(&out.stderr);
    assert!(out.status.success(), "vyrn {args:?} failed:\n{err}");
    let (_, table) = err
        .split_once("alloc phase")
        .unwrap_or_else(|| panic!("vyrn {args:?} printed no allocation table:\n{err}"));
    table
        .lines()
        .skip(1)
        .filter_map(|line| {
            // `reallocs` and `frees` sit between the count and the bytes. Phase names hold spaces.
            let cells: Vec<&str> = line.split_whitespace().collect();
            let n = cells.len().checked_sub(4).filter(|&n| n > 0)?;
            Some((
                cells[..n].join(" "),
                cells[n].parse().ok()?,
                cells[n + 3].parse().ok()?,
            ))
        })
        .collect()
}
