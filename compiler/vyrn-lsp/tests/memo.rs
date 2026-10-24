//! The editor's judgment memo (`vyrn_frontend::movecheck::Judgments`) serves no
//! stale verdict: after every edit, one server's diagnostics and memory notes
//! equal those of a fresh server that analyzes the same files once.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use vyrn_frontend::loader::{DiskResolver, LoadOptions};
use vyrn_frontend::session::Session;

fn scratch(tag: &str) -> PathBuf {
    let dir = std::env::temp_dir().join(format!("vyrn-memo-{tag}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("scratch");
    dir
}

/// The root's diagnostics and memory notes, as the server analyzes it under
/// `session`.
fn analyze(root: &Path, session: &Arc<Session>) -> String {
    let path = root.to_string_lossy().replace('\\', "/");
    let text = std::fs::read_to_string(root).expect("read the root");
    let opts = LoadOptions {
        // The test binary sits where `manifest::std_root` finds no `std`.
        std_root: Some(concat!(env!("CARGO_MANIFEST_DIR"), "/../../std").replace('\\', "/")),
        session: Some(session.clone()),
        ..Default::default()
    };
    let a = vyrn_frontend::analyze_judged(
        &text,
        Some((&path, &opts, &DiskResolver)),
        Some(&*vyrn_genwasm::engine()),
        &vyrn_lower::JUDGE,
    );
    format!("{:#?}\n{:#?}\n{:#?}", a.diagnostics, a.remapped, a.memory)
}

/// Runs `f` on a thread with the server's stack and a session of its own.
fn server<T: Send + 'static>(f: impl FnOnce(&Arc<Session>) -> T + Send + 'static) -> T {
    std::thread::Builder::new()
        .stack_size(64 * 1024 * 1024)
        .spawn(move || f(&Session::new(true)))
        .expect("spawn")
        .join()
        .expect("the analysis panicked")
}

/// The refusal the effect witnesses turn on.
const STATE: &str = "is written here while `g` still reads out of it";

/// Writes each step's files into `dir`, then analyzes `main.vyrn` in one
/// server that saw every earlier step and in a fresh one. Returns the first
/// step whose two answers differ, with both, or a sentence if no step's fresh
/// answer holds `turns`, the text the witness turns on.
fn drift(dir: &Path, turns: &str, steps: &[&[(&str, &str)]]) -> Option<String> {
    let turns = turns.to_string();
    let dir = dir.to_path_buf();
    let steps: Vec<Vec<(String, String)>> = (steps.iter())
        .map(|s| {
            s.iter()
                .map(|(f, t)| (f.to_string(), t.to_string()))
                .collect()
        })
        .collect();
    server(move |session| {
        let root = dir.join("main.vyrn");
        let mut turned = false;
        for (i, step) in steps.iter().enumerate() {
            for (file, text) in step {
                std::fs::write(dir.join(file), text).expect("write");
            }
            let editor = analyze(&root, session);
            let r = root.clone();
            let fresh = server(move |fresh| analyze(&r, fresh));
            turned |= fresh.contains(&turns);
            if editor != fresh {
                return Some(format!(
                    "step {i}\n-- editor:\n{editor}\n-- fresh:\n{fresh}"
                ));
            }
        }
        (!turned).then(|| format!("no step holds `{turns}`, so the witness shows nothing"))
    })
}

const ROOT_STATE: &str = "import { apply } from \"./b\"\n\
    let mut g: Array<Int64> = [1, 2, 3]\n\
    fn fill() -> Int64 {\n  g.push(4)\n  return 0\n}\n\
    fn main() -> Int64 {\n  print(apply(fill, g))\n  return 0\n}\n";

const CALLS_F: &str = "export fn apply(f: fn() -> Int64, xs: Array<Int64>) -> Int64 {\n  \
    let n = f()\n  return xs[0] + n\n}\n";

const IGNORES_F: &str = "export fn apply(f: fn() -> Int64, xs: Array<Int64>) -> Int64 {\n  \
    return xs[0]\n}\n";

/// A root keystroke serves `apply` out of the memo; the root's call to it
/// still stores into `g`, which the argument reads.
#[test]
fn a_root_keystroke_keeps_a_served_callees_state_writes() {
    let dir = scratch("keystroke");
    let edited = format!("{ROOT_STATE}// k\n");
    let got = drift(
        &dir,
        STATE,
        &[
            &[("b.vyrn", CALLS_F), ("main.vyrn", ROOT_STATE)],
            &[("main.vyrn", &edited)],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.is_none(), "{}", got.unwrap_or_default());
}

/// An edit inside `apply`'s body, and no other, starts or stops its store into
/// the root's state.
#[test]
fn an_imported_body_edit_moves_the_roots_verdict() {
    let dir = scratch("callee");
    let got = drift(
        &dir,
        STATE,
        &[
            &[("b.vyrn", IGNORES_F), ("main.vyrn", ROOT_STATE)],
            &[("b.vyrn", CALLS_F)],
            &[("main.vyrn", &format!("{ROOT_STATE}// k\n"))],
            &[("b.vyrn", IGNORES_F)],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.is_none(), "{}", got.unwrap_or_default());
}

const A_STATE: &str = "import { apply } from \"./b\"\n\
    let mut g: Array<Int64> = [1, 2, 3]\n\
    fn fill() -> Int64 {\n  g.push(4)\n  return 0\n}\n\
    export fn run() -> Int64 {\n  return apply(fill, g)\n}\n";

const ROOT_RUNS_A: &str =
    "import { run } from \"./a\"\nfn main() -> Int64 {\n  print(run())\n  return 0\n}\n";

/// `a`'s verdict reads `apply`'s body in `b`, which neither `a`'s hash nor
/// the declaration fingerprint covers.
#[test]
fn an_imported_body_edit_moves_another_imported_verdict() {
    let dir = scratch("between");
    let got = drift(
        &dir,
        STATE,
        &[
            &[
                ("b.vyrn", IGNORES_F),
                ("a.vyrn", A_STATE),
                ("main.vyrn", ROOT_RUNS_A),
            ],
            &[("b.vyrn", CALLS_F)],
            &[("b.vyrn", IGNORES_F)],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.is_none(), "{}", got.unwrap_or_default());
}

const A_CALLS_BACK: &str = "let mut g: Array<Int64> = [1, 2, 3]\n\
    export fn bump() -> Int64 {\n  g.push(4)\n  return 0\n}\n\
    fn use(f: fn() -> Int64, xs: Array<Int64>) -> Int64 {\n  let n = f()\n  return xs[0] + n\n}\n\
    export fn run(f: fn() -> Int64) -> Int64 {\n  return use(f, g)\n}\n";

/// A root whose `poke` returns `body`, beside an `aux` over `aux`, a type no
/// body of `a` reads.
fn root_pokes(body: &str, aux: &str) -> String {
    format!(
        "import {{ bump, run }} from \"./a\"\nfn poke() -> Int64 {{\n  return {body}\n}}\n\
         fn aux(x: {aux}) -> {aux} {{\n  return x\n}}\n\
         fn main() -> Int64 {{\n  print(run(poke))\n  return 0\n}}\n"
    )
}

/// A root keystroke moves a served body's verdict: `a`'s `use` runs whatever
/// `fn() -> Int64` the program passes it, and the root's `poke` starts or
/// stops storing into `a`'s state.
#[test]
fn a_root_edit_moves_an_imported_verdict() {
    let dir = scratch("callback");
    let (pure, writes) = (root_pokes("0", "Int64"), root_pokes("bump()", "Int64"));
    let got = drift(
        &dir,
        STATE,
        &[
            &[("a.vyrn", A_CALLS_BACK), ("main.vyrn", &pure)],
            &[("main.vyrn", &writes)],
            &[("main.vyrn", &pure)],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.is_none(), "{}", got.unwrap_or_default());
}

/// A root signature edit leaves `a`'s bodies served, and one of them still
/// reads what the root's `poke` stores into.
#[test]
fn a_root_signature_edit_keeps_what_a_served_body_reads() {
    let dir = scratch("rootsig");
    let (pure, writes) = (root_pokes("0", "Int64"), root_pokes("bump()", "Int32"));
    let got = drift(
        &dir,
        STATE,
        &[
            &[("a.vyrn", A_CALLS_BACK), ("main.vyrn", &pure)],
            &[("main.vyrn", &writes)],
            &[("main.vyrn", &pure)],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.is_none(), "{}", got.unwrap_or_default());
}

const IGNORES: &str = "export fn ignore<T>(x: consume T) -> Int64 {\n  return 0\n}\n";

fn txn(must: bool) -> String {
    let imp = if must {
        "impl MustUse for Txn {}\n"
    } else {
        "\n"
    };
    format!(
        "import {{ ignore }} from \"./b\"\n\
         type Txn = {{ id: Int64 }}\n{imp}\
         fn main() -> Int64 {{\n  print(ignore(Txn {{ id: 1 }}))\n  return 0\n}}\n"
    )
}

/// An imported generic's instance at a root type reads the root's impls:
/// `ignore<Txn>` owes `Txn` its disposal only while the root declares `impl
/// MustUse for Txn`. The imported body does not move.
#[test]
fn a_root_impl_moves_an_imported_instances_verdict() {
    let dir = scratch("rootimpl");
    let got = drift(
        &dir,
        "is never disposed",
        &[
            &[("b.vyrn", IGNORES), ("main.vyrn", &txn(false))],
            &[("main.vyrn", &txn(true))],
            &[("main.vyrn", &txn(false))],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.is_none(), "{}", got.unwrap_or_default());
}

/// A line moved above the root's declarations keeps `ignore<Txn>`'s verdict
/// served and its diagnostic on the moved line. The root's validated type and
/// protocol are in the fingerprint by content.
#[test]
fn a_moved_root_line_keeps_an_imported_instances_verdict() {
    let dir = scratch("rootline");
    let root = |above: &str| {
        format!(
            "{above}{}type Port = Int64 where value >= 1\n\
             protocol Sized {{\n  fn size(self) -> Int64\n}}\n",
            txn(true)
        )
    };
    let (moved, twice) = (root("\n"), root("\n\n"));
    let got = drift(
        &dir,
        "is never disposed",
        &[
            &[("b.vyrn", IGNORES), ("main.vyrn", &root(""))],
            &[("main.vyrn", &moved)],
            &[("main.vyrn", &twice)],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.is_none(), "{}", got.unwrap_or_default());
}

/// A doc comment edited on a root protocol and its member keeps
/// `ignore<Txn>`'s verdict served: no fingerprint holds a doc.
#[test]
fn a_root_protocol_doc_edit_keeps_an_imported_instances_verdict() {
    let dir = scratch("rootdoc");
    let root = |doc: &str| {
        format!(
            "{}/// {doc}\nprotocol Sized {{\n  /// {doc}\n  fn size(self) -> Int64\n}}\n",
            txn(true)
        )
    };
    let (first, second) = (root("The size."), root("The size, in words."));
    let got = drift(
        &dir,
        "is never disposed",
        &[
            &[("b.vyrn", IGNORES), ("main.vyrn", &root("A size."))],
            &[("main.vyrn", &first)],
            &[("main.vyrn", &second)],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.is_none(), "{}", got.unwrap_or_default());
}

/// The fact refusal the fact witnesses turn on.
const UNSORTED: &str = "needs `xs` sorted by `Asc<Int64>`";

const A_SEARCHES: &str = "import { Asc, lowerBound } from \"std/order\"\n\
    export fn find(xs: Array<Int64>) -> Int64 {\n  return lowerBound<Asc<Int64>>(xs, 3)\n}\n";

const ROOT_FINDS: &str = "import { find } from \"./a\"\n\
    fn main() -> Int64 {\n  print(find([3, 1, 2]))\n  return 0\n}\n";

/// A root keystroke serves `find` out of the memo, with its fact refusal.
#[test]
fn a_root_keystroke_keeps_a_served_bodys_fact_refusal() {
    let dir = scratch("factkey");
    let got = drift(
        &dir,
        UNSORTED,
        &[
            &[("a.vyrn", A_SEARCHES), ("main.vyrn", ROOT_FINDS)],
            &[("main.vyrn", &format!("{ROOT_FINDS}// k\n"))],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.is_none(), "{}", got.unwrap_or_default());
}

fn b_needs(clause: &str) -> String {
    format!(
        "import {{ Asc }} from \"std/order\"\n\
         export fn need(xs: Array<Int64>{clause}) -> Int64 {{\n  return xs.length\n}}\n"
    )
}

const A_NEEDS: &str = "import { need } from \"./b\"\n\
    export fn find(xs: Array<Int64>) -> Int64 {\n  return need(xs)\n}\n";

/// `a`'s fact verdict reads the clause of `b`'s `need`, which `a`'s hash does
/// not cover and the declaration fingerprint does.
#[test]
fn an_imported_clause_edit_moves_another_imported_fact_verdict() {
    let dir = scratch("factclause");
    let (none, sorted) = (b_needs(""), b_needs(" where sorted<Asc<Int64>>(value)"));
    let got = drift(
        &dir,
        UNSORTED,
        &[
            &[
                ("b.vyrn", &none),
                ("a.vyrn", A_NEEDS),
                ("main.vyrn", ROOT_FINDS),
            ],
            &[("b.vyrn", &sorted)],
            &[("b.vyrn", &none)],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.is_none(), "{}", got.unwrap_or_default());
}

const A_SORTS_FOR_A_CALLBACK: &str = "import { Asc, sort, lowerBound } from \"std/order\"\n\
    let mut g: Array<Int64> = [1, 2, 3]\n\
    export fn bump() -> Int64 {\n  g.push(4)\n  return 0\n}\n\
    fn helper(xs: consume Array<Int64>) -> Array<Int64> {\n  return sort<Asc<Int64>>(xs)\n}\n\
    fn use(f: fn() -> Int64, xs: Array<Int64>) -> Int64 {\n  let n = f()\n  \
    let s = helper([3, 1])\n  return xs[0] + n + lowerBound<Asc<Int64>>(s, 2)\n}\n\
    export fn run(f: fn() -> Int64) -> Int64 {\n  return use(f, g)\n}\n";

/// A root edit rebuilds `use`, whose callback starts storing into `g`, and
/// serves `helper`: `use` reads the summary the memo kept of `helper`, so
/// `lowerBound` is accepted in the editor as in a fresh analysis.
#[test]
fn a_rebuilt_body_reads_a_served_helpers_summary() {
    let dir = scratch("summary");
    let (pure, writes) = (root_pokes("0", "Int64"), root_pokes("bump()", "Int64"));
    let got = drift(
        &dir,
        STATE,
        &[
            &[("a.vyrn", A_SORTS_FOR_A_CALLBACK), ("main.vyrn", &pure)],
            &[("main.vyrn", &writes)],
            &[("main.vyrn", &pure)],
        ],
    );
    let _ = std::fs::remove_dir_all(&dir);
    assert!(got.is_none(), "{}", got.unwrap_or_default());
}
