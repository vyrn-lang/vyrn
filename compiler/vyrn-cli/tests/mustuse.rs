//! The disposal obligation, asked of `vyrn check`. The rule is about a
//! type, so the typed judgment (`vyrn_lower::typed::obligation`) states it and a crate
//! below the lowering cannot; `testsweep` lifts these programs into the corpus. The
//! census rows are `r30` and `r31` in `tests/refusals.rs`; these are the shapes around
//! them.

mod common;
use common::vyrn;

/// A producer, and a consumer that discharges a stream in expression position.
const FEED: &str = "fn feed() -> Stream<Int64> { let xs: Array<Int64> = [1, 2] return fromArray(xs) } fn drain(s: Stream<Int64>) -> Int64 { let mut t = 0 for v in s { t = t + v } return t } ";

/// A combinator spelled locally: nothing in the compiler knows about std/stream, and
/// nothing has to.
const TWICE: &str = "fn twice(s: Stream<Int64>) -> Stream<Int64> { let mut out: Array<Int64> = [] for x in s { out.push(x * 2) } return fromArray(out) } ";

/// `vyrn check` over one program: `Ok`, or its whole standard error.
fn run(src: &str) -> Result<(), String> {
    let dir = common::scratch("mustuse");
    let mut h = std::collections::hash_map::DefaultHasher::new();
    std::hash::Hash::hash(src, &mut h);
    let path = dir.join(format!("p{:016x}.vyrn", std::hash::Hasher::finish(&h)));
    std::fs::write(&path, src).expect("write the program");
    let out = vyrn().arg("check").arg(&path).output().expect("vyrn check");
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
    // than trusted to the runtime.
    let base = "fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> { \
                if cl { return None } return Some(sl) } ";
    let src = format!(
        "{base} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) \
         let a = boxStream(s) return 0 }}"
    );
    // Whatever holds the box owes the stream.
    assert!(run(&src).is_ok(), "the wrapper owes it, not `main`");
    let src = format!(
        "{base} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) \
         let a = boxStream(s) let t: Stream<Int64> = unboxStream(a) return 0 }}"
    );
    let e = run(&src).unwrap_err();
    assert!(
        e.contains("`t` is a `Stream<Int64>` and is never disposed"),
        "{e}"
    );
    let src = format!(
        "{base} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) \
         let a = boxStream(s) let t: Stream<Int64> = unboxStream(a) close(t) return 0 }}"
    );
    assert!(run(&src).is_ok());
    // Closed as well as boxed is a double release.
    let src = format!(
        "{base} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) \
         let a = boxStream(s) close(s) return 0 }}"
    );
    let e = run(&src).unwrap_err();
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
fn a_stream_parameter_carries_the_obligation_into_the_callee() {
    // Otherwise `fn sink(s: Stream<Int64>) {}` is a hole: the caller discharges by
    // moving, and nobody else has to do anything.
    let e = run(&format!(
        "{FEED} fn sink(s: Stream<Int64>) -> Int64 {{ return 0 }} \
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
    // A `Stream` parameter carries the obligation in and a `Stream` return hands
    // one back; the two rules compose, with no rule about combinators.
    let e = run(&format!(
        "{FEED}{TWICE} fn main() -> Int64 {{ let m = twice(feed()) return 0 }}"
    ))
    .unwrap_err();
    assert!(
        e.contains("`m` is a `Stream<Int64>` and is never disposed"),
        "{e}"
    );

    let e = run(&format!(
        "{FEED} fn sink(s: Stream<Int64>) -> Stream<Int64> {{ return feed() }} \
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
/// so a second one is refused whatever the callee.
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
        let e = run(&format!(
            "{prelude} fn go(s: Stream<{ty}>) -> Int64 {{ {bind}{call} {bind}{call} \
             return 0 }} fn main() -> Int64 {{ return go({feed}) }}"
        ))
        .unwrap_err();
        assert!(e.contains("disposed more than once"), "{call}: {e}");
    }
    // On a local, where the binding is the frame's own.
    let e = run(&format!(
        "{FEED} fn main() -> Int64 {{ let s = feed() let a = boxStream(s) \
         let b = boxStream(s) return 0 }}"
    ))
    .unwrap_err();
    assert!(e.contains("disposed more than once"), "{e}");
    // The box owes the release, which `close` after `unboxStream` discharges.
    assert!(run(&format!(
        "{FEED} fn main() -> Int64 {{ let s = feed() let a = boxStream(s) \
         let back: Stream<Int64> = unboxStream(a) close(back) return 0 }}"
    ))
    .is_ok());
}
