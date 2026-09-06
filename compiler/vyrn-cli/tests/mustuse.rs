//! RFC-0075's disposal obligation, asked of the whole compiler — RFC-0125 §3
//! M3, the obligation slice.
//!
//! These are `movecheck.rs`'s own unit tests for the must-use walk, moved with
//! the rule. They asked `vyrn_frontend::check`, which no longer states it: the
//! obligation is a rule about a TYPE, so it is the typed judgment's
//! (`vyrn_lower::typed::obligation`) and reaches a reader through the one list
//! a file's refusals come out in. A crate below the lowering cannot state it,
//! and this crate links the lowering, so the programs are asked of `vyrn
//! check` here — which is also what puts them back in the corpus a licence is
//! read from, through the lift `testsweep` does over `tests/*.rs`.
//!
//! The census rows are `r30` and `r31` in `tests/refusals.rs`; these are the
//! shapes around them.

mod common;
use common::vyrn;

/// The producer every stream case below acquires from, and a consumer that
/// discharges one — a call, so it fits in an expression position.
const FEED: &str = "fn feed() -> Stream<Int64> { let xs: Array<Int64> = [1, 2] return fromArray(xs) } fn drain(s: Stream<Int64>) -> Int64 { let mut t = 0 for v in s { t = t + v } return t } ";

/// A combinator, spelled locally rather than imported: nothing in the compiler
/// knows about std/stream, and the point is that nothing has to.
const TWICE: &str = "fn twice(s: Stream<Int64>) -> Stream<Int64> { let mut out: Array<Int64> = [] for x in s { out.push(x * 2) } return fromArray(out) } ";

/// `vyrn check` over one program, as the unit tests' `run` was: `Ok` where the
/// compiler accepts it, and its whole standard error where it does not.
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

/// The three ways an obligation is discharged, all accepted.
#[test]
fn the_three_discharges_are_accepted() {
    assert!(stream("for p in feed() { print(p) } return 0").is_ok());
    assert!(stream("let s = feed() close(s) return 0").is_ok());
    assert!(run(&format!(
        "{FEED} fn fwd() -> Stream<Int64> {{ let s = feed() return s }}          fn main() -> Int64 {{ close(fwd()) return 0 }}"
    ))
    .is_ok());
}

/// The linear walk reads unreachable code the way the move check's own block walk does:
/// a mention after a diverging statement is not a second disposal.
#[test]
fn a_mention_in_unreachable_code_is_not_a_second_disposal() {
    assert!(stream("let s = feed() close(s) return 0 close(s)").is_ok());
    assert!(stream("let s = feed() close(s) panic(\"gone\") close(s)").is_ok());
    // A REACHABLE second disposal is still refused.
    let e = stream("let s = feed() close(s) let n = 0 close(s) return 0").unwrap_err();
    assert!(e.contains("disposed more than once"), "{e}");
}

#[test]
fn an_abandoned_stream_does_not_build() {
    // The milestone's whole claim: the `#6193` shape is a compile error.
    let e = stream("let events = feed() return 0").unwrap_err();
    assert!(
        e.contains("`events` is a `Stream<Int64>` and is never disposed"),
        "{e}"
    );
}

#[test]
fn a_stepped_producer_carries_the_same_obligation() {
    // RFC-0075 M2b's producer is a second builtin, and this pass keys on the
    // NAME — so an abandoned `fromStep` result had to be added here or the
    // one stream the language cannot materialise would be the one it lets
    // leak. Its endlessness is the checker's business, not this pass's: the
    // obligation is the same obligation.
    //
    // The step is spelled the way `std/stream.vyrn` spells one — the slot, the
    // generation and the closing flag — because the whole compiler types this
    // program now and the unit test's two-argument sketch never did.
    const STEP: &str = "fn tick(slot: Int64, gen: Int64, closing: Bool) -> Option<Int64> { \
                        if closing { return None } return Some(slot + gen) } ";
    let e = run(&format!(
        "{STEP} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) return 0 }}"
    ))
    .unwrap_err();
    assert!(e.contains("`s` is a `Stream` and is never disposed"), "{e}");
    assert!(run(&format!(
        "{STEP} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) close(s) return 0 }}"
    ))
    .is_ok());
}

#[test]
fn a_wrapper_carries_the_obligation_and_swallows_its_source() {
    // A lazy wrapper's source is DISCHARGED by `boxStream`, which is an
    // ordinary mention of the binding and therefore an ordinary move; the
    // stream the wrapper hands back is a new obligation. Both halves matter,
    // and RFC-0090 M3 added a third: the source comes back out of the box
    // with `unboxStream`, which ACQUIRES one — so a wrapper's own release
    // path is checked here rather than trusted to a walk inside the runtime.
    let base = "fn tick(sl: Int64, gn: Int64, cl: Bool) -> Option<Int64> { \
                if cl { return None } return Some(sl) } ";
    let src = format!(
        "{base} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) \
         let a = boxStream(s) return 0 }}"
    );
    // The box is not a disposal: whatever holds the address owes the stream.
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
    // And the source may not be closed as well as boxed — that is the double
    // release the wrapper's own close would then complete.
    let src = format!(
        "{base} fn main() -> Int64 {{ let s = fromStep(0, 1, tick) \
         let a = boxStream(s) close(s) return 0 }}"
    );
    let e = run(&src).unwrap_err();
    assert!(e.contains("is disposed more than once"), "{e}");
}

#[test]
fn a_stream_must_be_disposed_on_every_path() {
    // Disposing on one branch only is the tRPC pathology in miniature: the
    // cleanup exists, and there is a path that skips it.
    let e = stream("let s = feed() if true { close(s) } return 0").unwrap_err();
    assert!(e.contains("never disposed"), "{e}");
    let e = stream("let s = feed() if true { return 1 } close(s) return 0").unwrap_err();
    assert!(e.contains("never disposed"), "{e}");
    // Both branches, or a branch that leaves, are fine.
    assert!(stream("let s = feed() if true { close(s) } else { close(s) } return 0").is_ok());
    assert!(stream("let s = feed() if true { close(s) return 1 } close(s) return 0").is_ok());
}

/// RFC-0095 M3. "Every path" now reaches into an ARM.
///
/// The `if` STATEMENT was refused from RFC-0075 M1, because the walk walks
/// its two blocks. A `match` is an expression, so the walk read the whole
/// statement at once with `mentions` — "some path names it" — and treated
/// that as a disposal on all of them. One `||` was the difference between
/// the two spellings of one program.
#[test]
fn an_arm_is_a_path_like_a_branch_is() {
    let pick = "let o: Option<Int64> = Some(1) ";
    // Disposed in one arm and not the other: refused, both spellings.
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
    // The same shape returned rather than bound — the `return` reads its
    // expression the same way.
    let e = stream(&format!(
        "{pick} let s = feed() return match o {{ Some(k) => drain(s), None => 0 }}"
    ))
    .unwrap_err();
    assert!(e.contains("never disposed"), "{e}");
    // EVERY arm disposes: accepted. `examples/branchtypes.vyrn` is this
    // shape, and it must keep compiling.
    assert!(stream(&format!(
        "{pick} let s = feed() let n = match o {{ Some(k) => drain(s) + k, \
             None => drain(s) }} return n"
    ))
    .is_ok());
    assert!(stream(&format!(
        "{pick} let s = feed() let n = if true {{ drain(s) }} else {{ drain(s) }} return n"
    ))
    .is_ok());
    // The scrutinee runs whatever arm is taken, so a disposal there is one
    // on every path.
    assert!(stream(
        "let s = feed() let n = match Some(drain(s)) { Some(k) => k, None => 0 } \
                return n"
    )
    .is_ok());
}

/// The limit RFC-0095 M3 recorded and did not close: a branch ACQUIRES in
/// each arm, and the binding it acquires into inherited nothing.
///
/// The RFC wrote the shape as `let t2 = match c { A => t, B => u }` over two
/// live bindings, and that spelling is already refused — at `t`, which one
/// arm hands on and the other does not, which is M3's own rule. The shape
/// that reaches the hole acquires in the arm instead, so no earlier binding
/// is there to answer, and the program was accepted with a stream nobody
/// answered for.
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
    // Disposing it is the fix, and it is accepted.
    assert!(stream(&format!(
        "{pick} let t = match o {{ Some(k) => feed(), None => feed() }} close(t) return 0"
    ))
    .is_ok());
    // The if-expression spelling of the same program.
    let e = stream("let t = if true { feed() } else { feed() } return 0").unwrap_err();
    assert!(e.contains("is never disposed"), "{e}");
    // An arm that acquires nothing leaves the binding alone.
    assert!(stream("let n = if true { 1 } else { 2 } return n").is_ok());
}

#[test]
fn breaking_out_of_the_declaring_block_abandons_it() {
    // `break` means two different things depending on which side of the
    // declaring block the loop it leaves is on.
    let e = stream("for i in [0, 1] { let s = feed() break } return 0").unwrap_err();
    assert!(e.contains("never disposed"), "{e}");
    // Here the loop is BELOW the declaration, so control comes back owning it.
    assert!(stream("let s = feed() for i in [0, 1] { break } close(s) return 0").is_ok());
}

#[test]
fn a_stream_may_not_be_disposed_twice() {
    // The direction the leak check does not cover, and the worse bug of the
    // two: `close` frees the buffer, so a second one is a double free.
    let e = stream("let s = feed() close(s) close(s) return 0").unwrap_err();
    assert!(e.contains("disposed more than once"), "{e}");
    let e = stream("let s = feed() for p in s { print(p) } close(s) return 0").unwrap_err();
    assert!(e.contains("disposed more than once"), "{e}");
}

#[test]
fn aliasing_moves_the_obligation_rather_than_dropping_it() {
    let e = stream("let s = feed() let t = s return 0").unwrap_err();
    assert!(e.contains("`t` is a `Stream<Int64>`"), "{e}");
    assert!(stream("let s = feed() let t = s close(t) return 0").is_ok());
}

#[test]
fn a_stream_parameter_carries_the_obligation_into_the_callee() {
    // Without this, `fn sink(s: Stream<Int64>) {}` is a one-line hole through
    // the whole analysis: the caller discharges by moving, and nobody else has
    // to do anything.
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
    // The hole that only opens once combinators exist, in both directions.
    // M1's two rules already close it — a `Stream` parameter carries the
    // obligation in, a `Stream` return hands one back — so this pins that
    // they compose rather than adding a rule about combinators.

    // The result is owed exactly as `fromArray`'s is.
    let e = run(&format!(
        "{FEED}{TWICE} fn main() -> Int64 {{ let m = twice(feed()) return 0 }}"
    ))
    .unwrap_err();
    assert!(
        e.contains("`m` is a `Stream<Int64>` and is never disposed"),
        "{e}"
    );

    // A combinator that drops its argument on the floor does not build.
    let e = run(&format!(
        "{FEED} fn sink(s: Stream<Int64>) -> Stream<Int64> {{ return feed() }} \
         fn main() -> Int64 {{ close(sink(feed())) return 0 }}"
    ))
    .unwrap_err();
    assert!(
        e.contains("`s` is a `Stream<Int64>` and is never disposed"),
        "{e}"
    );

    // Consumed and then closed is still the double free.
    let e = run(&format!(
        "{FEED}{TWICE} fn main() -> Int64 {{ let m = twice(feed()) \
         for v in m {{ print(v) }} close(m) return 0 }}"
    ))
    .unwrap_err();
    assert!(e.contains("disposed more than once"), "{e}");

    // A discharged chain is accepted, including the intermediate that never
    // gets a name.
    assert!(run(&format!(
        "{FEED}{TWICE} fn main() -> Int64 {{ for v in twice(twice(feed())) \
         {{ print(v) }} return 0 }}"
    ))
    .is_ok());
}

/// `boxStream` and `serveStream` — the two the census counted as carrying
/// their ownership fact **nowhere at all** (Q1). They carry it on the TYPE,
/// and this is where that is written down.
///
/// Each takes a `Stream<T>` and hands it away for good. A second call on one
/// binding is a double free, and each has exactly one corpus caller — which
/// the census read as "the only reason no heap has been corrupted". The
/// reason is stronger than that: `Stream<T>` is linear, every mention of a
/// stream binding is a disposal in the must-use walk, and a second mention
/// is refused whatever the name is. The signatures now say `consume` as
/// well, so the fact is legible; the refusal was always there.
#[test]
fn a_stream_is_handed_away_once_however_it_is_handed_away() {
    // `serveStream` takes a `Stream<String>` of encoded frames, which the unit
    // test never had to satisfy: it parsed the program itself and no type
    // check ran. The whole compiler runs one, so each producer feeds the
    // stream its consumer declares.
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
    // And on a local, where the binding is the frame's own.
    let e = run(&format!(
        "{FEED} fn main() -> Int64 {{ let s = feed() let a = boxStream(s) \
         let b = boxStream(s) return 0 }}"
    ))
    .unwrap_err();
    assert!(e.contains("disposed more than once"), "{e}");
    // One is fine — the box owes the release, which `close` after
    // `unboxStream` discharges.
    assert!(run(&format!(
        "{FEED} fn main() -> Int64 {{ let s = feed() let a = boxStream(s) \
         let back: Stream<Int64> = unboxStream(a) close(back) return 0 }}"
    ))
    .is_ok());
}

#[test]
fn a_generic_producer_is_quoted_as_plain_stream() {
    // `Stream<U>` at a call site names a type parameter the program never
    // wrote. This pass has no types, so it under-specifies instead — the
    // same `Stream` it has always used for `fromArray`.
    //
    // `consume` on the parameter, because `fromArray` TAKES the array
    // (RFC-0092 M5): the stream's close frees the buffer, so a `read`
    // parameter's buffer may not go into one. The rule refuses it and names
    // `consume` on the menu; this test was written before it did.
    let e = run(
        "fn mk<T>(xs: consume Array<T>) -> Stream<T> { return fromArray(xs) } \
                 fn main() -> Int64 { let s = mk([1, 2]) return 0 }",
    )
    .unwrap_err();
    assert!(e.contains("`s` is a `Stream` and is never disposed"), "{e}");
}
