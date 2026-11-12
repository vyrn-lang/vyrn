//! The disposal obligation, asked of `vyrn check`. The kernel states it as two
//! rows (`Rule::NeverDisposed`, `Rule::DisposedTwice`); `testsweep` lifts
//! these programs into the corpus. The census rows are `r30` and `r31` in
//! `tests/refusals.rs`; these are the shapes around them.

mod common;
use common::vyrn;

/// A producer, and a consumer that discharges a stream in expression position.
const FEED: &str = "fn feed() -> Stream<Int64> { let xs: Array<Int64> = [1, 2] return fromArray(xs) } fn drain(s: consume Stream<Int64>) -> Int64 { let mut t = 0 for v in s { t = t + v } return t } ";

/// A combinator spelled locally: nothing in the compiler knows about std/stream, and
/// nothing has to.
const TWICE: &str = "fn twice(s: consume Stream<Int64>) -> Stream<Int64> { let mut out: Array<Int64> = [] for x in s { out.push(x * 2) } return fromArray(out) } ";

/// `vyrn check` over one program: `Ok`, or its whole standard error.
fn run(src: &str) -> Result<(), String> {
    let dir = common::scratch("mustuse");
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(src, &mut h);
    let path = dir.join(format!("p{:016x}.vyrn", std::hash::Hasher::finish(&h)));
    std::fs::write(&path, src).expect("write the program");
    verdict(vyrn().arg("check").arg(&path))
}

/// [`run`] over `src` as `std/stream` itself, the one module that may call the box
/// builtins: a copy of `std/` whose `stream.vyrn` is `src`.
fn run_as_stream(src: &str) -> Result<(), String> {
    let std_dir = common::scratch("mustuse-std").join("std");
    std::fs::create_dir_all(&std_dir).expect("create the std copy");
    let repo_std = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    for e in std::fs::read_dir(repo_std).expect("read std") {
        let e = e.expect("a std entry").path();
        let name = e.file_name().expect("a std file name");
        if e.is_file() {
            std::fs::copy(&e, std_dir.join(name)).expect("copy a std file");
        }
    }
    let path = std_dir.join("stream.vyrn");
    std::fs::write(&path, src).expect("write the program");
    verdict(vyrn().arg("check").arg(&path).env("VYRN_STD", &std_dir))
}

/// `Ok` for an accepted check, or its whole standard error.
fn verdict(c: &mut std::process::Command) -> Result<(), String> {
    let out = c.output().expect("vyrn check");
    match out.status.success() {
        true => Ok(()),
        false => Err(String::from_utf8_lossy(&out.stderr)
            .replace(
                "

", "
",
            )
            .to_string()),
    }
}

fn stream(body: &str) -> Result<(), String> {
    run(&format!("{FEED} fn main() -> Int64 {{ {body} }}"))
}

#[test]
fn the_three_discharges_are_accepted() {
    assert!(stream("for p in feed() { print(p) } return 0").is_ok());
    assert!(stream("let s = feed() close(s) return 0").is_ok());
    assert!(run(&format!(
        "{FEED} fn fwd() -> Stream<Int64> {{ let s = feed() return s }}          fn main() -> Int64 {{ close(fwd()) return 0 }}"
    ))
    .is_ok());
}

/// The linear walk reads unreachable code as the move check's block walk does.
#[test]
fn a_mention_in_unreachable_code_is_not_a_second_disposal() {
    assert!(stream("let s = feed() close(s) return 0 close(s)").is_ok());
    assert!(stream("let s = feed() close(s) panic(\"gone\") close(s)").is_ok());
    // A reachable second disposal is refused.
    let e = stream("let s = feed() close(s) let n = 0 close(s) return 0").unwrap_err();
    assert!(e.contains("disposed more than once"), "{e}");
}

#[test]
fn a_stepped_producer_carries_the_same_obligation() {
    // The pass keys on the producer's name, so `fromStep` must be listed, or the
    // one stream the language cannot materialise leaks. The step takes the slot,
    // generation and closing flag, as `std/stream.vyrn` spells one, because the
    // program is type-checked.
    const STEP: &str = "fn tick(slot: Int64, gen: Int64, closing: Bool) -> Option<Int64> { \
                        if closing { return None } return Some(slot + gen) } ";
    let e = run(&format!(
        "{STEP} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) return 0 }}"
    ))
    .unwrap_err();
    assert!(
        e.contains("`s` is a `Stream<Int64>` and is never disposed"),
        "{e}"
    );
    assert!(run(&format!(
        "{STEP} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) close(s) return 0 }}"
    ))
    .is_ok());
}

#[test]
fn a_wrapper_carries_the_obligation_and_swallows_its_source() {
    // `boxStream` discharges its source by an ordinary move. `unboxStream`
    // acquires one, so a wrapper's release path is checked here rather
    // than trusted to the runtime. Each program stands in for `std/stream`,
    // the one module that may call them.
    let base = "fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> { \
                if cl { return None } return Some(sl) } ";
    let src = format!(
        "{base} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) \
         let a = boxStream(s) return 0 }}"
    );
    // Whatever holds the box owes the stream.
    assert!(
        run_as_stream(&src).is_ok(),
        "the wrapper owes it, not `main`"
    );
    let src = format!(
        "{base} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) \
         let a = boxStream(s) let t: Stream<Int64> = unboxStream(a) return 0 }}"
    );
    let e = run_as_stream(&src).unwrap_err();
    assert!(
        e.contains("`t` is a `Stream<Int64>` and is never disposed"),
        "{e}"
    );
    let src = format!(
        "{base} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) \
         let a = boxStream(s) let t: Stream<Int64> = unboxStream(a) close(t) return 0 }}"
    );
    assert!(run_as_stream(&src).is_ok());
    // Closed as well as boxed is a double release.
    let src = format!(
        "{base} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) \
         let a = boxStream(s) close(s) return 0 }}"
    );
    let e = run_as_stream(&src).unwrap_err();
    assert!(e.contains("is disposed more than once"), "{e}");
}

#[test]
fn a_stream_must_be_disposed_on_every_path() {
    // The tRPC pathology in miniature: the cleanup exists and a path skips it.
    let e = stream("let s = feed() if true { close(s) } return 0").unwrap_err();
    assert!(e.contains("never disposed"), "{e}");
    let e = stream("let s = feed() if true { return 1 } close(s) return 0").unwrap_err();
    assert!(e.contains("never disposed"), "{e}");
    assert!(stream("let s = feed() if true { close(s) } else { close(s) } return 0").is_ok());
    assert!(stream("let s = feed() if true { close(s) return 1 } close(s) return 0").is_ok());
}

/// "Every path" reaches into a `match` arm and an `if` expression: a mention
/// on some path is not a disposal on all of them.
#[test]
fn an_arm_is_a_path_like_a_branch_is() {
    let pick = "let o: Option<Int64> = Some(1) ";
    let e = stream(&format!(
        "{pick} let s = feed() let n = match o {{ Some(k) => drain(s) + k, None => 0 }} \
         return n"
    ))
    .unwrap_err();
    assert!(
        e.contains("`s` is a `Stream<Int64>` and is never disposed"),
        "{e}"
    );
    let e = stream(&format!(
        "{pick} let s = feed() let n = if true {{ drain(s) }} else {{ 0 }} return n"
    ))
    .unwrap_err();
    assert!(e.contains("never disposed"), "{e}");
    let e = stream(&format!(
        "{pick} let s = feed() return match o {{ Some(k) => drain(s), None => 0 }}"
    ))
    .unwrap_err();
    assert!(e.contains("never disposed"), "{e}");
    // Every arm disposes: `examples/branchtypes.vyrn` is this shape.
    assert!(stream(&format!(
        "{pick} let s = feed() let n = match o {{ Some(k) => drain(s) + k, \
             None => drain(s) }} return n"
    ))
    .is_ok());
    assert!(stream(&format!(
        "{pick} let s = feed() let n = if true {{ drain(s) }} else {{ drain(s) }} return n"
    ))
    .is_ok());
    // The scrutinee runs on every path.
    assert!(stream(
        "let s = feed() let n = match Some(drain(s)) { Some(k) => k, None => 0 } \
                return n"
    )
    .is_ok());
}

/// Each arm acquires, so no earlier binding answers for the stream: the binding the
/// branch initializes inherits the obligation.
#[test]
fn a_branch_acquires_into_the_binding() {
    let pick = "let o: Option<Int64> = Some(1) ";
    let e = stream(&format!(
        "{pick} let t = match o {{ Some(k) => feed(), None => feed() }} return 0"
    ))
    .unwrap_err();
    assert!(
        e.contains("`t` is a `Stream<Int64>` and is never disposed"),
        "{e}"
    );
    assert!(stream(&format!(
        "{pick} let t = match o {{ Some(k) => feed(), None => feed() }} close(t) return 0"
    ))
    .is_ok());
    let e = stream("let t = if true { feed() } else { feed() } return 0").unwrap_err();
    assert!(e.contains("is never disposed"), "{e}");
    assert!(stream("let n = if true { 1 } else { 2 } return n").is_ok());
}

#[test]
fn breaking_out_of_the_declaring_block_abandons_it() {
    // `break` differs by which side of the declaring block its loop is on.
    let e = stream("for i in [0, 1] { let s = feed() break } return 0").unwrap_err();
    assert!(e.contains("never disposed"), "{e}");
    // The loop is below the declaration, so control comes back owning it.
    assert!(stream("let s = feed() for i in [0, 1] { break } close(s) return 0").is_ok());
}

#[test]
fn aliasing_moves_the_obligation_rather_than_dropping_it() {
    let e = stream("let s = feed() let t = s return 0").unwrap_err();
    assert!(e.contains("`t` is a `Stream<Int64>`"), "{e}");
    assert!(stream("let s = feed() let t = s close(t) return 0").is_ok());
}

#[test]
fn a_consume_parameter_carries_the_obligation_into_the_callee() {
    // Otherwise `fn sink(s: consume Stream<Int64>) {}` is a hole: the caller
    // discharges by moving, and nobody else has to do anything.
    let e = run(&format!(
        "{FEED} fn sink(s: consume Stream<Int64>) -> Int64 {{ return 0 }} \
         fn main() -> Int64 {{ return sink(feed()) }}"
    ))
    .unwrap_err();
    assert!(
        e.contains("`s` is a `Stream<Int64>` and is never disposed"),
        "{e}"
    );
}

#[test]
fn a_combinator_neither_swallows_the_obligation_nor_launders_it() {
    // A `consume` parameter carries the obligation in and a `Stream` return
    // hands one back; the two rules compose, with no rule about combinators.
    let e = run(&format!(
        "{FEED}{TWICE} fn main() -> Int64 {{ let m = twice(feed()) return 0 }}"
    ))
    .unwrap_err();
    assert!(
        e.contains("`m` is a `Stream<Int64>` and is never disposed"),
        "{e}"
    );

    let e = run(&format!(
        "{FEED} fn sink(s: consume Stream<Int64>) -> Stream<Int64> {{ return feed() }} \
         fn main() -> Int64 {{ close(sink(feed())) return 0 }}"
    ))
    .unwrap_err();
    assert!(
        e.contains("`s` is a `Stream<Int64>` and is never disposed"),
        "{e}"
    );

    let e = run(&format!(
        "{FEED}{TWICE} fn main() -> Int64 {{ let m = twice(feed()) \
         for v in m {{ print(v) }} close(m) return 0 }}"
    ))
    .unwrap_err();
    assert!(e.contains("disposed more than once"), "{e}");

    // Includes an intermediate that never gets a name.
    assert!(run(&format!(
        "{FEED}{TWICE} fn main() -> Int64 {{ for v in twice(twice(feed())) \
         {{ print(v) }} return 0 }}"
    ))
    .is_ok());
}

/// `boxStream` and `serveStream` hand a stream away for good, and each has one corpus
/// caller. The type carries the fact: every mention of a stream binding is a disposal,
/// so a second one is refused whatever the callee. Each program stands in for
/// `std/stream`, the one module that may call `boxStream`.
#[test]
fn a_stream_is_handed_away_once_however_it_is_handed_away() {
    // Each producer feeds the stream type its consumer declares: `serveStream`
    // takes a `Stream<String>` of encoded frames.
    const TEXT: &str = "fn lines() -> Stream<String> { let xs: Array<String> = [\"a\" + \"b\"] \
                        return fromArray(xs) } ";
    for (call, prelude, ty, feed, bind) in [
        ("boxStream(s)", FEED, "Int64", "feed()", "let a = "),
        ("serveStream(s)", TEXT, "String", "lines()", ""),
    ] {
        let e = run_as_stream(&format!(
            "{prelude} fn go(s: consume Stream<{ty}>) -> Int64 {{ {bind}{call} {bind}{call} \
             return 0 }} fn main() -> Int64 {{ return go({feed}) }}"
        ))
        .unwrap_err();
        assert!(e.contains("disposed more than once"), "{call}: {e}");
    }
    // On a local, where the binding is the frame's own.
    let e = run_as_stream(&format!(
        "{FEED} fn main() -> Int64 {{ let s = feed() let a = boxStream(s) \
         let b = boxStream(s) return 0 }}"
    ))
    .unwrap_err();
    assert!(e.contains("disposed more than once"), "{e}");
    // The box owes the release, which `close` after `unboxStream` discharges.
    assert!(run_as_stream(&format!(
        "{FEED} fn main() -> Int64 {{ let s = feed() let a = boxStream(s) \
         let back: Stream<Int64> = unboxStream(a) close(back) return 0 }}"
    ))
    .is_ok());
}

/// A user must-use type with a release that prints, so a double release or a
/// missed one shows in the output as well as under the free audit.
const TXN: &str = "type Txn = { label: String, ops: Array<String> } impl MustUse for Txn {} \
                   impl Owned for Txn { fn release(consume self) { print(\"end \" + self.label) \
                   let ops = consume self.ops drop ops let label = consume self.label drop label } } \
                   fn begin(label: String) -> Txn { let ops: Array<String> = [] \
                   return Txn { label: label.copy(), ops: ops } } \
                   fn finish(t: consume Txn) { drop t } \
                   fn pool() -> Array<Txn> { let mut xs: Array<Txn> = [] xs.push(begin(\"a\")) return xs } ";

/// The obligation follows ownership: a `read` or `modify` parameter borrows a
/// must-use value and owes nothing, the caller keeps it, and a temporary fills
/// only a position that takes it. Each refused program names its first
/// sentence; each accepted one runs under the free audit and prints its
/// releases once.
#[test]
fn a_must_use_obligation_follows_ownership() {
    let cases: [(&str, String, Result<&str, &str>); 15] = [
        (
            "i",
            format!(
                "{TXN} fn count(xs: Array<Txn>) -> Int64 {{ let n = xs.length drop xs return n }} \
             fn main() -> Int64 {{ let xs = pool() print(count(xs)) drop xs return 0 }}"
            ),
            Err("`xs` (line 1) is released although the body does not own it"),
        ),
        (
            "d",
            format!(
                "{TXN} fn count(xs: Array<Txn>) -> Int64 {{ return xs.length }} \
             fn main() -> Int64 {{ let xs = pool() print(count(xs)) drop xs return 0 }}"
            ),
            Ok("1\nend a"),
        ),
        (
            "e",
            format!(
                "{TXN} fn count(xs: Array<Txn>) -> Int64 {{ return xs.length }} \
             fn main() -> Int64 {{ let xs = pool() print(count(xs)) return 0 }}"
            ),
            Err("`xs` is an `Array<Txn>` and is never disposed"),
        ),
        (
            "h",
            "fn count(xs: Array<String>) -> Int64 { let n = xs.length drop xs return n } \
             fn main() -> Int64 { let xs: Array<String> = [\"a\".copy()] print(count(xs)) \
             return 0 }"
                .to_string(),
            Err("`xs` (line 1) is released although the body does not own it"),
        ),
        (
            "t2",
            format!(
                "{TXN} fn look(t: Txn) -> Int64 {{ finish(t) return 0 }} \
             fn main() -> Int64 {{ let t = begin(\"a\") print(look(t)) drop t return 0 }}"
            ),
            Err(
                "`t` may not be passed to a `consume` parameter via `finish(..)`: it is a \
                 `read` parameter",
            ),
        ),
        (
            "t5",
            format!(
                "{TXN} fn add(t: modify Txn) {{ t.ops.push(\"x\".copy()) drop t }} \
             fn main() -> Int64 {{ let mut t = begin(\"a\") add(t) drop t return 0 }}"
            ),
            Err("`t` (line 1) is released although the body does not own it"),
        ),
        (
            "t6",
            format!(
                "{TXN} fn look(t: Txn) -> Int64 {{ drop t return 0 }} \
             fn main() -> Int64 {{ let t = begin(\"a\") print(look(t)) drop t return 0 }}"
            ),
            Err("`t` (line 1) is released although the body does not own it"),
        ),
        (
            "t7",
            format!(
                "{TXN} protocol Sized {{ fn size(self) -> Int64 }} \
             impl Sized for Txn {{ fn size(self) -> Int64 {{ return self.ops.length }} }} \
             fn main() -> Int64 {{ let t = begin(\"a\") print(t.size()) drop t return 0 }}"
            ),
            Ok("0\nend a"),
        ),
        (
            "t10",
            format!("{TXN} fn main() -> Int64 {{ print(begin(\"b\").ops.length) return 0 }}"),
            Err("`begin(..)` is a `Txn` and is never disposed"),
        ),
        (
            "s2",
            "fn drain(s: Stream<Int64>) -> Int64 { let mut t = 0 for v in s { t = t + v } \
             return t } fn main() -> Int64 { let xs: Array<Int64> = [1, 2] \
             let s = fromArray(xs) let n = drain(s) close(s) return n }"
                .to_string(),
            Err("`s` may not be consumed: it is a `read` parameter"),
        ),
        (
            "s5",
            format!(
                "{FEED} fn peek(s: Stream<Int64>) -> Int64 {{ return 7 }} \
             fn main() -> Int64 {{ let s = feed() print(peek(s)) close(s) return 0 }}"
            ),
            Ok("7"),
        ),
        (
            "s7",
            "fn drain(s: Stream<Int64>) -> Int64 { let mut t = 0 for v in s { t = t + v } \
             return t } fn main() -> Int64 { let xs: Array<Int64> = [1, 2] \
             return drain(fromArray(xs)) }"
                .to_string(),
            Err("`s` may not be consumed: it is a `read` parameter"),
        ),
        (
            "s8",
            format!(
                "{FEED} fn main() -> Int64 {{ let s = feed() let n = drain(s) close(s) \
             return n }}"
            ),
            Err("`s` is a `Stream<Int64>` and is disposed more than once"),
        ),
        (
            "s9",
            format!(
                "{FEED} fn peek(s: Stream<Int64>) -> Int64 {{ return 0 }} \
             fn main() -> Int64 {{ return peek(feed()) }}"
            ),
            Err("`feed(..)` is a `Stream<Int64>` and is never disposed"),
        ),
        (
            "c9",
            format!(
                "{TXN} fn add(xs: modify Array<Txn>) {{ xs.push(begin(\"b\")) }} \
             fn main() -> Int64 {{ let mut xs = pool() add(xs) print(xs.length) drop xs \
             return 0 }}"
            ),
            Ok("2\nend a\nend b"),
        ),
    ];
    judged("mustuse-owner", cases);
}

/// Checks each program; a refused one names its first sentence, and an
/// accepted one runs under the free audit and prints `Ok`'s text.
fn judged<const N: usize>(scratch: &str, cases: [(&str, String, Result<&str, &str>); N]) {
    let dir = common::scratch(scratch);
    let mut bad = Vec::new();
    for (name, src, want) in cases {
        let file = dir.join(format!("{name}.vyrn"));
        std::fs::write(&file, &src).expect("write the program");
        let checked = verdict(vyrn().arg("check").arg(&file));
        match (want, checked) {
            (Err(says), Err(e)) if e.lines().next().is_some_and(|l| l.ends_with(says)) => {}
            (Ok(prints), Ok(())) => {
                let out = vyrn()
                    .env("VYRN_LEAK_CHECK", "1")
                    .arg("run")
                    .arg(&file)
                    .output()
                    .expect("vyrn run");
                let stdout = String::from_utf8_lossy(&out.stdout).replace("\r\n", "\n");
                if out.status.code() != Some(0) || stdout.trim_end() != prints {
                    bad.push(format!(
                        "{name}: run exited {:?} and printed {stdout:?}",
                        out.status.code()
                    ));
                }
            }
            (_, got) => bad.push(format!("{name}: check said {got:?}")),
        }
    }
    assert!(bad.is_empty(), "{}", bad.join("\n"));
}

/// `drop` is a must-use value's disposal whatever the value holds: a record
/// with a String and no `impl Owned`, or one of scalars, which frees nothing.
/// Without it the value's last owner could not discharge it.
#[test]
fn a_must_use_value_is_dropped_whatever_it_holds() {
    let dir = common::scratch("mustuse-drop");
    let cases = [
        (
            "text",
            "type Txn = { label: String } impl MustUse for Txn {} \
             fn main() -> Int64 { let t = Txn { label: \"a\".copy() } print(t.label) drop t \
             return 0 }",
            "a",
        ),
        (
            "scalar",
            "type Tok = { id: Int64 } impl MustUse for Tok {} \
             fn finish(t: consume Tok) -> Int64 { let n = t.id drop t return n } \
             fn main() -> Int64 { let t = Tok { id: 3 } print(finish(t)) return 0 }",
            "3",
        ),
    ];
    for (name, src, prints) in cases {
        let file = dir.join(format!("{name}.vyrn"));
        std::fs::write(&file, src).expect("write the program");
        let out = vyrn()
            .env("VYRN_LEAK_CHECK", "1")
            .arg("run")
            .arg(&file)
            .output()
            .expect("vyrn run");
        let stdout = String::from_utf8_lossy(&out.stdout);
        assert_eq!(
            (out.status.code(), stdout.trim()),
            (Some(0), prints),
            "{name}: {}",
            String::from_utf8_lossy(&out.stderr)
        );
    }
    let e = run("type Tok = { id: Int64 } impl MustUse for Tok {} \
                 fn main() -> Int64 { let t = Tok { id: 3 } drop t drop t return 0 }")
    .unwrap_err();
    assert!(
        e.contains("`t` is a `Tok` and is disposed more than once"),
        "{e}"
    );
}

/// A store releases what it displaces, so a store over a held must-use value
/// is refused. A name bound to a value made from no parts holds none, and the
/// write-back of a rebuild under a `modify` root hands the old value back.
#[test]
fn a_store_over_a_held_must_use_value_is_refused() {
    let held = "`t` holds a must-use value, and a store over it would release it undisposed";
    let elem = "`xs[..]` holds a must-use value, and a store over it would release it undisposed";
    let cases: [(&str, String, Result<&str, &str>); 7] = [
        (
            "name",
            format!(
                "{TXN} fn main() -> Int64 {{ let mut t = begin(\"a\") t = begin(\"b\") drop t \
                 return 0 }}"
            ),
            Err(held),
        ),
        (
            "element",
            format!(
                "{TXN} fn main() -> Int64 {{ let mut xs = pool() xs[0] = begin(\"b\") drop xs \
                 return 0 }}"
            ),
            Err(elem),
        ),
        (
            "nullary",
            format!(
                "{TXN} fn main() -> Int64 {{ let mut xs: Array<Option<Txn>> = [] \
                 xs.push(Some(begin(\"a\"))) xs[0] = None drop xs return 0 }}"
            ),
            Err(elem),
        ),
        (
            "heapless",
            "type Tok = { id: Int64 } impl MustUse for Tok {} \
             fn main() -> Int64 { let mut t = Tok { id: 1 } t = Tok { id: 2 } drop t return 0 }"
                .to_string(),
            Err(held),
        ),
        (
            "moved",
            format!(
                "{TXN} fn main() -> Int64 {{ let mut t = begin(\"a\") drop t t = begin(\"b\") \
                 drop t return 0 }}"
            ),
            Ok("end a\nend b"),
        ),
        (
            "vacant",
            format!(
                "{TXN} fn main() -> Int64 {{ let mut o: Option<Txn> = None \
                 o = Some(begin(\"a\")) drop o return 0 }}"
            ),
            Ok("end a"),
        ),
        (
            "write-back",
            format!(
                "{TXN} type Pool = {{ xs: Array<Txn> }} \
                 fn add(p: modify Pool) {{ p.xs.push(begin(\"b\")) }} \
                 fn main() -> Int64 {{ let mut p = Pool {{ xs: pool() }} add(p) \
                 print(p.xs.length) let xs = consume p.xs drop xs drop p return 0 }}"
            ),
            Ok("2\nend a\nend b"),
        ),
    ];
    judged("mustuse-store", cases);
}

/// `clear`, `append` and `copyFrom` forget, copy or overwrite elements, so
/// each is refused at a must-use element that owns no heap.
#[test]
fn a_method_that_forgets_elements_is_refused_at_a_must_use_element() {
    for (m, call) in [
        ("clear", "xs.clear()"),
        ("append", "xs.append(ys)"),
        ("copyFrom", "xs.copyFrom(ys)"),
    ] {
        let e = run(&format!(
            "type Tok = {{ id: Int64 }} impl MustUse for Tok {{}} \
             fn main() -> Int64 {{ let mut xs: Array<Tok> = [] xs.push(Tok {{ id: 1 }}) \
             let mut ys: Array<Tok> = [] ys.push(Tok {{ id: 2 }}) {call} drop xs drop ys \
             return 0 }}"
        ))
        .unwrap_err();
        let says = format!(
            "`{m}` forgets or overwrites elements without releasing them, and `Tok` is \
             must-use: move the elements one at a time instead"
        );
        assert!(e.contains(&says), "{m}: {e}");
    }
}

/// A binder of a `match consume` arm owns its must-use payload: the arm
/// disposes of it, and a borrowing `match` leaves it with the scrutinee.
#[test]
fn a_match_consume_binder_owns_its_must_use_payload() {
    let head = "type Tok = { id: Int64 } impl MustUse for Tok {} \
                fn take(t: consume Tok) { drop t } \
                fn main() -> Int64 { let o: Option<Tok> = Some(Tok { id: 1 }) ";
    for (arm, refused) in [
        (
            "match consume o { Some(t) => { take(t) } None => {} }",
            false,
        ),
        (
            "match consume o { Some(t) => { drop t } None => {} }",
            false,
        ),
        (
            "match o { Some(t) => { print(t.id) } None => {} } drop o",
            false,
        ),
        (
            "match consume o { Some(t) => { print(t.id) } None => {} }",
            true,
        ),
    ] {
        let r = run(&format!("{head}{arm} return 0 }}"));
        match refused {
            true => {
                let e = r.unwrap_err();
                assert!(
                    e.contains("`t` is a `Tok` and is never disposed"),
                    "{arm}: {e}"
                );
            }
            false => assert_eq!(r, Ok(()), "{arm}"),
        }
    }
}

/// A `for` that takes its container owns each must-use element: the body
/// disposes of it on every path, and a `break` or a `return` that leaves
/// elements no turn reached is refused in the loop's words.
#[test]
fn a_loop_that_takes_its_container_owns_each_must_use_element() {
    let left = "`t` does not reach every element of `xs`: a `break` or a `return` leaves the \
                rest undisposed";
    let cases: [(&str, String, Result<&str, &str>); 6] = [
        (
            "read",
            format!(
                "{TXN} fn main() -> Int64 {{ let xs = pool() let mut n = 0 \
                 for t in consume xs {{ n = n + t.ops.length }} return n }}"
            ),
            Err("`t` is a `Txn` and is never disposed"),
        ),
        (
            "one-branch",
            format!(
                "{TXN} fn main() -> Int64 {{ let xs = pool() \
                 for t in consume xs {{ if t.label == \"b\" {{ finish(t) }} }} return 0 }}"
            ),
            Err("`t` is a `Txn` and is never disposed"),
        ),
        (
            "break",
            format!(
                "{TXN} fn main() -> Int64 {{ let xs = pool() \
                 for t in consume xs {{ finish(t) break }} return 0 }}"
            ),
            Err(left),
        ),
        (
            "return",
            format!(
                "{TXN} fn first() -> Int64 {{ let xs = pool() \
                 for t in consume xs {{ finish(t) return 1 }} return 0 }} \
                 fn main() -> Int64 {{ return first() }}"
            ),
            Err(left),
        ),
        (
            "drop",
            format!(
                "{TXN} fn main() -> Int64 {{ let xs = pool() \
                 for t in consume xs {{ drop t }} return 0 }}"
            ),
            Ok("end a"),
        ),
        (
            "pop",
            format!(
                "{TXN} fn main() -> Int64 {{ let mut xs = pool() xs.push(begin(\"b\")) \
                 while xs.length > 0 {{ if let Some(t) = xs.pop() {{ finish(t) }} break }} \
                 drop xs return 0 }}"
            ),
            Ok("end b\nend a"),
        ),
    ];
    judged("mustuse-loop", cases);
}

/// A record field and an enum payload hold their part's obligation, as an
/// array's element does, so the holder is disposed of by name.
#[test]
fn a_record_or_an_enum_holding_a_must_use_value_is_must_use() {
    let cases: [(&str, String, Result<&str, &str>); 4] = [
        (
            "record",
            format!(
                "{TXN} type Pool = {{ n: Int64, xs: Array<Txn> }} \
                 fn main() -> Int64 {{ let p = Pool {{ n: 1, xs: pool() }} return p.n }}"
            ),
            Err("`p` is a `Pool` and is never disposed"),
        ),
        (
            "enum",
            format!(
                "{TXN} type Slot = enum {{ Empty, Full(Txn) }} \
                 fn main() -> Int64 {{ let s = Full(begin(\"a\")) return 0 }}"
            ),
            Err("`s` is a `Slot` and is never disposed"),
        ),
        (
            "drop",
            format!(
                "{TXN} type Pool = {{ n: Int64, xs: Array<Txn> }} \
                 fn main() -> Int64 {{ let p = Pool {{ n: 1, xs: pool() }} drop p return 0 }}"
            ),
            Ok("end a"),
        ),
        (
            "taken",
            format!(
                "{TXN} type Pool = {{ n: Int64, xs: Array<Txn> }} \
                 fn main() -> Int64 {{ let mut p = Pool {{ n: 1, xs: pool() }} \
                 let xs = consume p.xs drop xs drop p return 0 }}"
            ),
            Ok("end a"),
        ),
    ];
    judged("mustuse-holder", cases);
}

/// A `drop` of the variable of a loop that takes its container hands the
/// element on, as a `consume` argument does, so each turn owns its element.
#[test]
fn a_drop_in_a_loop_that_takes_its_container_releases_the_element_once() {
    let cases: [(&str, String, Result<&str, &str>); 1] = [(
        "drop",
        "fn main() -> Int64 { let mut xs: Array<String> = [] xs.push(\"a\".copy()) \
         xs.push(\"bc\".copy()) let mut n = 0 for s in consume xs { if s == \"a\" { drop s } \
         else { n = n + s.byteLength } } print(n) return 0 }"
            .to_string(),
        Ok("2"),
    )];
    judged("mustuse-drop-elem", cases);
}
