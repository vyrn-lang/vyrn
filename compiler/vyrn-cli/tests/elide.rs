//! Check elision (`vyrn_lower::elide`): which check rows a body proves, and the
//! programs that witness each rule's side condition. A witness is a check the
//! pass must keep, and a run that traps there under the oracle
//! (`VYRN_CHECKS=<file>`), which also fails the run if the check was proved.

mod common;
use common::*;

/// `src` as a program with `main` from `calls`, in a fresh directory.
fn program(src: &str, calls: &str) -> (Scratch, std::path::PathBuf) {
    let dir = scratch("elide");
    let file = dir.join("p.vyrn");
    let main = format!(
        "fn main() -> Int64 {{\n    let mut xs: Array<Int64> = []\n    xs.push(10)\n    \
         xs.push(20)\n    xs.push(30)\n{calls}\n    return 0\n}}\n"
    );
    std::fs::write(&file, format!("{src}\n{main}")).unwrap();
    (dir, file)
}

/// The check rows of `body` as `vyrn emit-lowered` prints them: `proved` or
/// `check`, then the rule.
fn verdicts(src: &str, body: &str) -> Vec<String> {
    let (_dir, file) = program(src, "");
    let out = vyrn().arg("emit-lowered").arg(&file).output().unwrap();
    assert!(out.status.success(), "{}", norm(&out.stderr));
    let mut rows = Vec::new();
    let mut inside = false;
    for l in norm(&out.stdout).lines() {
        if let Some(head) = l.strip_prefix("fn ") {
            inside = head.split('(').next() == Some(body);
        } else if inside {
            let w: Vec<&str> = l.split_whitespace().collect();
            if matches!(w.first(), Some(&"proved" | &"check")) {
                rows.push(format!("{} {}", w[0], w[1]));
            }
        }
    }
    rows
}

/// Runs the program under the oracle and returns its stderr and the oracle's
/// rows for `body`, each as `line ordinal rule verdict count`.
fn oracle(src: &str, calls: &str, body: &str) -> (String, Vec<String>) {
    let (dir, file) = program(src, calls);
    let log = dir.join("counts.tsv");
    let out = vyrn()
        .arg("run")
        .arg(&file)
        .env("VYRN_CHECKS", &log)
        .output()
        .unwrap();
    let rows = std::fs::read_to_string(&log)
        .unwrap_or_default()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f[1] == body).then(|| f[2..].join(" "))
        })
        .collect();
    (norm(&out.stderr), rows)
}

#[test]
fn the_oracle_counts_each_run_of_a_check_row() {
    let src = "fn get(xs: Array<Int64>, i: Int64) -> Int64 {\n    return xs[i]\n}\n";
    let calls = "    print((get(xs, 0) + get(xs, 1) + get(xs, 2)).toString())";
    let (err, rows) = oracle(src, calls, "get");
    assert_eq!(err, "");
    assert_eq!(rows, ["2 0 array-index kept 3"]);
}

#[test]
fn a_for_loop_proves_its_element_read() {
    let src = "fn sum(xs: Array<Int64>) -> Int64 {\n    let mut s: Int64 = 0\n    \
               for x in xs {\n        s = s + x\n    }\n    return s\n}\n";
    assert_eq!(verdicts(src, "sum"), ["proved array-index"]);
}

#[test]
fn a_counted_while_proves_its_index() {
    let src = "fn sum(xs: Array<Int64>) -> Int64 {\n    let mut s: Int64 = 0\n    \
               let mut i: Int64 = 0\n    while i < xs.length {\n        s = s + xs[i]\n        \
               i = i + 1\n    }\n    return s\n}\n";
    assert_eq!(verdicts(src, "sum"), ["proved array-index"]);
}

#[test]
fn a_length_guard_proves_the_last_element() {
    let src = "fn last(xs: Array<Int64>) -> Int64 {\n    if xs.length > 0 {\n        \
               return xs[xs.length - 1]\n    }\n    return 0\n}\n";
    assert_eq!(verdicts(src, "last"), ["proved array-index"]);
}

#[test]
fn literal_operands_and_masks_prove_their_checks() {
    let src = "fn ops(x: Int64, k: Int64) -> Int64 {\n    let t = [1, 2, 3]\n    \
               return x / 2 + x % 7 + (x << 3) + (x << (k & 63)) + t[2]\n}\n";
    assert_eq!(
        verdicts(src, "ops"),
        [
            "proved int-div-zero",
            "proved int-div-overflow",
            "proved int-rem-zero",
            "proved shift-range",
            "proved shift-range",
            "proved array-index",
        ]
    );
}

#[test]
fn a_push_in_a_loop_over_the_old_length_proves_its_index() {
    let src = "fn w(xs: modify Array<Int64>) -> Int64 {
    let n = xs.length
                   let mut s: Int64 = 0
    let mut i: Int64 = 0
    while i < n {
                       xs.push(i)
        s = s + xs[i]
        i = i + 1
    }
    return s
}
";
    assert_eq!(verdicts(src, "w"), ["proved array-index"]);
}

#[test]
fn builtin_rows_carry_the_length_across_a_resize() {
    let src = "fn w(xs: modify Array<Int64>, ys: Array<Int64>) -> Int64 {
                   let n = xs.length
    xs.push(1)
    let a = xs[n]
                   xs.append(ys)
    let b = xs[n]
    if n >= 1 {
                       let p = xs.pop() ?? 0
        return a + b + p + xs[n - 1]
    }
    return a + b
}
";
    assert_eq!(verdicts(src, "w"), ["proved array-index"; 3]);
}

/// A length reaches `LENGTH_LIMIT + 1`, which `Int32` wraps: the conversion is
/// not exact, and the index below the length stays checked.
const LENGTH_PAST_INT32: &str = "fn w(xs: Array<UInt8>) -> UInt8 {
    if xs.length > 0 {
            let k = Int32(xs.length)
        return xs[Int64(k) - 1]
    }
    return 0
}
";

#[test]
fn a_length_is_not_exact_in_int32() {
    assert_eq!(verdicts(LENGTH_PAST_INT32, "w"), ["check array-index"]);
}

/// The run behind [`a_length_is_not_exact_in_int32`]; it takes 3 GiB.
#[test]
#[ignore]
fn a_length_of_two_to_the_31_traps_below_itself() {
    let calls = "    let mut ys: Array<UInt8> = [1]
    ys.reserve(1073741823)
                     let mut i: Int64 = 0
    while i < 31 {
        ys.append(ys)
                         i = i + 1
    }
    print(w(ys).toString())";
    let (err, _) = oracle(LENGTH_PAST_INT32, calls, "w");
    assert!(
        err.contains("array index -2147483649 out of bounds"),
        "{err}"
    );
}

/// Each witness: a function `w` whose checks must all stay, and the call in
/// `main` that makes one trap with the wording given.
const WITNESSES: &[(&str, &str, &str)] = &[
    // The bound is one past the end.
    (
        "fn w(xs: Array<Int64>) -> Int64 {\n    let mut s: Int64 = 0\n    let mut i: Int64 = 0\n    \
         while i <= xs.length {\n        s = s + xs[i]\n        i = i + 1\n    }\n    return s\n}\n",
        "    print(w(xs).toString())",
        "array index 3 out of bounds",
    ),
    // The counter starts below zero: Houdini's entry filter.
    (
        "fn w(xs: Array<Int64>) -> Int64 {\n    let mut s: Int64 = 0\n    let mut i: Int64 = -1\n    \
         while i < xs.length {\n        s = s + xs[i]\n        i = i + 1\n    }\n    return s\n}\n",
        "    print(w(xs).toString())",
        "array index -1 out of bounds",
    ),
    (
        "fn w(xs: Array<Int64>) -> Int64 {\n    let mut s: Int64 = 0\n    let mut i: Int64 = 0\n    \
         while i < xs.length {\n        s = s + xs[i + 1]\n        i = i + 1\n    }\n    return s\n}\n",
        "    print(w(xs).toString())",
        "array index 3 out of bounds",
    ),
    // The counter falls: Houdini's end-of-turn filter.
    (
        "fn w(xs: Array<Int64>) -> Int64 {\n    let mut s: Int64 = 0\n    let mut i: Int64 = 0\n    \
         while i < xs.length {\n        s = s + xs[i]\n        i = i - 1\n    }\n    return s\n}\n",
        "    print(w(xs).toString())",
        "array index -1 out of bounds",
    ),
    // A builtin shrinks the array through `modify`.
    (
        "fn w(xs: modify Array<Int64>) -> Int64 {\n    let mut s: Int64 = 0\n    let mut i: Int64 = 0\n    \
         while i < xs.length {\n        let p = xs.pop() ?? 0\n        s = s + p + xs[i]\n        \
         i = i + 1\n    }\n    return s\n}\n",
        "    print(w(xs).toString())",
        "array index 1 out of bounds",
    ),
    // A call shrinks the array through `modify`.
    (
        "fn cut(xs: modify Array<Int64>) {\n    let p = xs.pop() ?? 0\n}\n\n\
         fn w(xs: modify Array<Int64>) -> Int64 {\n    let mut s: Int64 = 0\n    let mut i: Int64 = 0\n    \
         while i < xs.length {\n        cut(xs)\n        s = s + xs[i]\n        i = i + 1\n    }\n    \
         return s\n}\n",
        "    print(w(xs).toString())",
        "array index 1 out of bounds",
    ),
    // `push` grows by one, not two.
    (
        "fn w(xs: modify Array<Int64>) -> Int64 {
    let n = xs.length
    xs.push(1)
             return xs[n + 1]
}
",
        "    print(w(xs).toString())",
        "array index 4 out of bounds",
    ),
    // `pop` shrinks by one: the old last index is gone.
    (
        "fn w(xs: modify Array<Int64>) -> Int64 {
    let n = xs.length
    if n > 0 {
                 let p = xs.pop() ?? 0
        return p + xs[n - 1]
    }
    return 0
}
",
        "    print(w(xs).toString())",
        "array index 2 out of bounds",
    ),
    (
        "fn w(xs: modify Array<Int64>, k: Int64) -> Int64 {
    let n = xs.length
    if n > 0 {
                 let p = xs.swapRemove(k)
        return p + xs[n - 1]
    }
    return 0
}
",
        "    print(w(xs, 0).toString())",
        "array index 2 out of bounds",
    ),
    (
        "fn w(xs: modify Array<Int64>) -> Int64 {
    let n = xs.length
    if n > 0 {
                 xs.clear()
        return xs[0]
    }
    return 0
}
",
        "    print(w(xs).toString())",
        "array index 0 out of bounds",
    ),
    // The join keeps only what both branches prove.
    (
        "fn w(c: Bool) -> Int64 {\n    let t = [1, 2, 3]\n    let mut i: Int64 = 1\n    if c {\n        \
         i = 5\n    }\n    return t[i]\n}\n",
        "    print(w(true).toString())",
        "array index 5 out of bounds",
    ),
    // A sum is exact only when it provably fits (research witness h21t).
    (
        "fn w(xs: Array<Int64>, n: Int64) -> Int64 {\n    if n >= 0 {\n        let i = n + n\n        \
         if i < xs.length {\n            return xs[i]\n        }\n    }\n    return 0\n}\n",
        "    print(w(xs, 4611686018427387904).toString())",
        "out of bounds",
    ),
    // A conversion is exact only when its source provably fits (witness s3).
    (
        "fn w(xs: Array<Int64>, x: Int64) -> Int64 {\n    let k = Int32(x)\n    if k >= 0 {\n        \
         if Int64(k) < xs.length {\n            return xs[x]\n        }\n    }\n    return 0\n}\n",
        "    print(w(xs, 4294967297).toString())",
        "array index 4294967297 out of bounds",
    ),
    (
        "fn w(x: Int64, y: Int64) -> Int64 {\n    return x / y\n}\n",
        "    print(w(1, 0).toString())",
        "division by zero",
    ),
    // `-1` is a computed divisor: the quotient's check stays.
    (
        "fn w(x: Int64, y: Int64) -> Int64 {\n    return x / -y\n}\n",
        "    print(w(-9223372036854775807 - 1, 1).toString())",
        "overflow",
    ),
    (
        "fn w(x: Int64, k: Int64) -> Int64 {\n    return x << (k & 64)\n}\n",
        "    print(w(1, 64).toString())",
        "shift",
    ),
    (
        "fn w(xs: Array<Int64>, b: UInt8) -> Int64 {\n    return xs[Int64(b)]\n}\n",
        "    print(w(xs, 255).toString())",
        "array index 255 out of bounds",
    ),
];

#[test]
fn every_witness_keeps_its_checks_and_traps_there() {
    let mut failures = Vec::new();
    for (src, calls, trap) in WITNESSES {
        let rows = verdicts(src, "w");
        if rows.is_empty() || rows.iter().any(|v| v.starts_with("proved")) {
            failures.push(format!("{src}\nrows: {rows:?}"));
        }
        let (err, _) = oracle(src, calls, "w");
        if !err.contains(trap) || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED) {
            failures.push(format!("{src}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
