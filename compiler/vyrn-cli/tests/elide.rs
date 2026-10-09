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
    rows_of(&file, body)
}

/// [`verdicts`] of the program rooted at `file`.
fn rows_of(file: &std::path::Path, body: &str) -> Vec<String> {
    let out = vyrn().arg("emit-lowered").arg(file).output().unwrap();
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
    run_oracle(&dir, &file, body)
}

/// [`oracle`] of the program rooted at `file`, its log in `dir`.
fn run_oracle(dir: &std::path::Path, file: &std::path::Path, body: &str) -> (String, Vec<String>) {
    let log = dir.join("counts.tsv");
    let out = vyrn()
        .arg("run")
        .arg(file)
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
    assert_eq!(rows, ["2 0 array-index proved 3"]);
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
fn a_condition_stored_in_a_bool_proves_its_index() {
    let src = "fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    if i < 0 || i >= xs.length {
                       return 0
    }
    return xs[i]
}
";
    assert_eq!(verdicts(src, "w"), ["proved array-index"]);
    let (err, rows) = oracle(src, "    print(w(xs, 2).toString())", "w");
    assert_eq!(
        (err.as_str(), rows),
        ("", vec!["5 0 array-index proved 1".to_string()])
    );
}

#[test]
fn a_sum_of_two_bounded_operands_is_exact() {
    let src = "fn w(xs: Array<Int64>, i: Int64, k: Int64) -> Int64 {
    if i >= 0 {
        if i < xs.length {
            if k >= 0 {
                if k < 4 {
                    let j = i + k
                    if j < xs.length {
                        return xs[j]
                    }
                }
            }
        }
    }
    return 0
}
";
    assert_eq!(verdicts(src, "w"), ["proved array-index"]);
    let (err, rows) = oracle(src, "    print(w(xs, 1, 1).toString())", "w");
    assert_eq!(
        (err.as_str(), rows),
        ("", vec!["8 0 array-index proved 1".to_string()])
    );
}

#[test]
fn a_join_restates_a_counter_through_each_branchs_temporaries() {
    let src = "fn w(xs: Array<Int64>) -> Int64 {
    let mut s: Int64 = 0
    let mut i: Int64 = 0
    while i < xs.length {
        let x = xs[i]
        s = s + x
        if x < 0 {
            i = i + 1
        } else {
            let k = (x & 3) + 1
            if i + k > xs.length {
                return s
            }
            i = i + k
        }
    }
    return s
}
";
    assert_eq!(verdicts(src, "w"), ["proved array-index"]);
    let (err, rows) = oracle(src, "    print(w(xs).toString())", "w");
    assert_eq!(
        (err.as_str(), rows),
        ("", vec!["5 0 array-index proved 1".to_string()])
    );
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

/// The shift amount `k & (width - 1)` is dropped to `k` where nothing else reads it: wasm's
/// `shl` and `shr` mask by the width. A second reader keeps the mask.
#[test]
fn a_shift_by_width_minus_one_masked_emits_no_and() {
    let dir = scratch("shiftmask");
    let wide = "fn f(x: Int64, k: Int64) -> Int64 {
    return (x << (k & 63)) + 1234567
}
                fn main() -> Int64 {
    return f(3, 70)
}
";
    let narrow = "fn f(x: Int32, k: Int32) -> Int32 {
    return (x >> (k & 31)) + 1234567
}
                  fn main() -> Int64 {
    return Int64(f(1024, 33))
}
";
    let shared = "fn f(x: Int64, k: Int64) -> Int64 {
    let m = k & 63
                      return (x << m) + m + 1234567
}
                  fn main() -> Int64 {
    return f(3, 70)
}
";
    let ands = |name: &str, src: &str| {
        let body = wat_func_containing(&dir, name, src, "1234567");
        body.lines().filter(|l| l.trim().ends_with(".and")).count()
    };
    assert_eq!(ands("wide", wide), 0);
    assert_eq!(ands("narrow", narrow), 0);
    assert_eq!(ands("shared", shared), 1);
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

#[test]
fn a_halving_loop_bounds_its_counter() {
    let src = "fn w(xs: Array<Int64>, b: UInt8, t: Int32) -> Int64 {
    let mut u = b
                   let mut k: Int64 = 0
    while u > 1 {
        u = u >> 1
        k = k + 1
    }
                   let mut v = t >> 2
    let mut shift: Int32 = 0
    while v > 1 {
                       v = v >> 1
        shift = shift + 1
    }
    let size = (t >> shift) & 3
                   if xs.length > 7 {
        return xs[k] + Int64(size)
    }
    return 0
}
";
    assert_eq!(
        verdicts(src, "w"),
        [
            "proved shift-range",
            "proved shift-range",
            "proved shift-range",
            "proved shift-range",
            "proved array-index",
        ]
    );
}

#[test]
fn a_where_rule_of_equal_lengths_proves_every_column() {
    let src = "type C = {
    a: Array<Int64>,
    b: Array<Int64>,
    c: Array<Int64>,
} where a.length == b.length && c.length == b.length

fn sum(c: modify C) -> Int64 {
    let mut i: Int64 = 0
    while i < c.a.length {
        c.c[i] = c.a[i] + c.b[i]
        i = i + 1
    }
    return 0
}
";
    assert_eq!(
        verdicts(src, "sum"),
        [
            "proved array-index",
            "proved array-index",
            "proved array-index"
        ]
    );
}

/// A group of stores that grows every column once, from the rule's equal
/// lengths, keeps the rule.
#[test]
fn a_group_that_grows_every_column_once_proves_its_rule() {
    let src = "type C = { a: Array<Int64>, b: Array<Int64>, c: Array<Int64> } where a.length == b.length && c.length == b.length
fn w(c: modify C, x: Int64) {
    c.a.push(x)
    c.b.push(x)
    c.c.push(x)
}
";
    assert_eq!(verdicts(src, "w"), ["proved where"]);
}

/// Inside a group each column has its own length: `a`'s length does not
/// bound an index into `b`, which has not grown yet.
#[test]
fn a_group_proves_no_index_by_another_columns_length() {
    let src = "type C = { a: Array<Int64>, b: Array<Int64> } where a.length == b.length
fn w(c: modify C) -> Int64 {
    c.a.push(1)
    c.b.push(if 0 < c.a.length { c.b[0] } else { 0 })
    return 0
}
";
    assert_eq!(verdicts(src, "w"), ["check array-index", "proved where"]);
    let calls = "    let mut c = C { a: [], b: [] }\n    print(w(c).toString())";
    let (err, _) = oracle(src, calls, "w");
    assert!(
        err.contains("array index 0 out of bounds")
            && !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// Each witness: a function `w` whose checks must all stay, and the call in
/// `main` that makes one trap with the wording given.
const WITNESSES: &[(&str, &str, &str)] = &[
    // A field's length bounds only that field.
    (
        "type P = { a: Array<Int64>, b: Array<Int64> }
fn w(p: P) -> Int64 {
    let mut s: Int64 = 0
    let mut i: Int64 = 0
    while i < p.a.length {
        s = s + p.b[i]
        i = i + 1
    }
    return s
}
",
        "    print(w(P { a: xs, b: [1] }).toString())",
        "array index 1 out of bounds",
    ),
    // A rule that equates `a` and `b` says nothing of `c`.
    (
        "type P = { a: Array<Int64>, b: Array<Int64>, c: Array<Int64> } where a.length == b.length
fn w(p: P) -> Int64 {
    let mut s: Int64 = 0
    let mut i: Int64 = 0
    while i < p.a.length {
        s = s + p.c[i]
        i = i + 1
    }
    return s
}
",
        "    print(w(P { a: xs, b: [1, 2, 3], c: [1] }).toString())",
        "array index 1 out of bounds",
    ),
    // An equality under `||` is no rule.
    (
        "type P = { a: Array<Int64>, b: Array<Int64> } where a.length == b.length || b.length == 1
fn w(p: P) -> Int64 {
    let mut s: Int64 = 0
    let mut i: Int64 = 0
    while i < p.a.length {
        s = s + p.b[i]
        i = i + 1
    }
    return s
}
",
        "    print(w(P { a: xs, b: [1] }).toString())",
        "array index 1 out of bounds",
    ),
    // A store into the field forgets its length.
    (
        "type P = { a: Array<Int64>, b: Array<Int64> }
fn w(p: consume P, i: Int64) -> Int64 {
    let mut q = p
    if i >= 0 {
        if i < q.a.length {
            q.a = [9]
            return q.a[i]
        }
    }
    return 0
}
",
        "    print(w(P { a: xs, b: [] }, 2).toString())",
        "array index 2 out of bounds",
    ),
    // A store into the field in a loop forgets its length at the head.
    (
        "type P = { a: Array<Int64>, b: Array<Int64> }
fn w(p: consume P) -> Int64 {
    let mut q = p
    let n = q.a.length
    let mut s: Int64 = 0
    let mut i: Int64 = 0
    while i < n {
        s = s + q.a[i]
        q.a = [5]
        i = i + 1
    }
    return s
}
",
        "    print(w(P { a: xs, b: [] }).toString())",
        "array index 1 out of bounds",
    ),
    // A builtin shrinks the field in place.
    (
        "type P = { a: Array<Int64>, b: Array<Int64> }
fn w(p: consume P, i: Int64) -> Int64 {
    let mut q = p
    if i >= 0 {
        if i < q.a.length {
            q.a.clear()
            return q.a[i]
        }
    }
    return 0
}
",
        "    print(w(P { a: xs, b: [] }, 0).toString())",
        "array index 0 out of bounds",
    ),
    // A call that writes the record forgets the length a store gave a field.
    (
        "type P = { a: Array<Int64>, b: Array<Int64> }
fn clearA(p: modify P) {
    p.a = []
}
fn w(p: consume P) -> Int64 {
    let mut q = p
    q.a = [1, 2, 3]
    clearA(q)
    return q.a[2]
}
",
        "    print(w(P { a: xs, b: [] }).toString())",
        "array index 2 out of bounds",
    ),
    // A group that grows one column by an array's length and the other by
    // one.
    (
        "type P = { a: Array<Int64>, b: Array<Int64> } where a.length == b.length
fn w(p: modify P, q: Array<Int64>) -> Int64 {
    p.a.append(q)
    p.b.push(3)
    return 0
}
",
        "    let mut p = P { a: [], b: [] }\n    print(w(p, [1, 2]).toString())",
        "violates its `where` clause",
    ),
    // A rule with a conjunct beside the lengths is not proved by them.
    (
        "type P = { a: Array<Int64>, b: Array<Int64> } where a.length == b.length && a.length < 2
fn w(p: modify P) -> Int64 {
    p.a.push(1)
    p.b.push(2)
    return 0
}
",
        "    let mut p = P { a: [1], b: [1] }\n    print(w(p).toString())",
        "violates its `where` clause",
    ),
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
    // A Bool that one branch overwrites keeps only what both branches give.
    (
        "fn w(xs: Array<Int64>, i: Int64, c: Bool) -> Int64 {
    let mut ok = i >= 0 && i < xs.length
    if c {
        ok = true
    }
    if ok {
        return xs[i]
    }
    return 0
}
",
        "    print(w(xs, 5, true).toString())",
        "array index 5 out of bounds",
    ),
    // A Bool's facts end where a name they mention is written.
    (
        "fn w(xs: Array<Int64>, k: Int64) -> Int64 {
    let mut i = k
    let ok = i >= 0 && i < xs.length
    i = i + 1
    if ok {
        return xs[i]
    }
    return 0
}
",
        "    print(w(xs, 2).toString())",
        "array index 3 out of bounds",
    ),
    (
        "fn w(xs: modify Array<Int64>, i: Int64) -> Int64 {
    let ok = i >= 0 && i < xs.length
    xs.clear()
    if ok {
        return xs[i]
    }
    return 0
}
",
        "    print(w(xs, 0).toString())",
        "array index 0 out of bounds",
    ),
    // A Bool a loop writes keeps nothing at the loop's exit.
    (
        "fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    let mut ok = i >= 0 && i < xs.length
    let mut k: Int64 = 0
    while k < 2 {
        if k == 1 {
            ok = true
        }
        k = k + 1
    }
    if ok {
        return xs[i]
    }
    return 0
}
",
        "    print(w(xs, 9).toString())",
        "array index 9 out of bounds",
    ),
    // A branch-local step that may be negative keeps no lower bound.
    (
        "fn w(xs: Array<Int64>) -> Int64 {
    let mut s: Int64 = 0
    let mut i: Int64 = 0
    while i < xs.length {
        let x = xs[i]
        s = s + x
        if x > 15 {
            i = i + 1
        } else {
            let k = (x & 3) - 3
            if i + k > xs.length {
                return s
            }
            i = i + k
        }
    }
    return s
}
",
        "    print(w(xs).toString())",
        "array index -1 out of bounds",
    ),
    // A branch-local alias of a length a later pop shrinks.
    (
        "fn w(xs: modify Array<Int64>, c: Bool) -> Int64 {
    if xs.length < 3 {
        return 0
    }
    let mut j: Int64 = 0
    if c {
        let t = xs.length - 1
        j = t
    } else {
        let t = xs.length - 1
        let p = xs.pop() ?? 0
        j = t
    }
    return xs[j]
}
",
        "    print(w(xs, false).toString())",
        "array index 2 out of bounds",
    ),
    // Branch-local aliases of different values.
    (
        "fn w(xs: Array<Int64>, c: Bool) -> Int64 {
    let mut j: Int64 = 0
    if c {
        let t = xs.length - 1
        j = t
    } else {
        let t = xs.length
        j = t
    }
    return xs[j]
}
",
        "    print(w(xs, false).toString())",
        "array index 3 out of bounds",
    ),
    // A sum is exact only when it provably fits (research witness h21t).
    (
        "fn w(xs: Array<Int64>, n: Int64) -> Int64 {\n    if n >= 0 {\n        let i = n + n\n        \
         if i < xs.length {\n            return xs[i]\n        }\n    }\n    return 0\n}\n",
        "    print(w(xs, 4611686018427387904).toString())",
        "out of bounds",
    ),
    // Operands each below 2^62 sum past `Int64`: neither is in half the range.
    (
        "fn w(xs: Array<Int64>, i: Int64, k: Int64) -> Int64 {
    if i >= 0 {
        if k >= 0 {
            if i <= 4611686018427387904 {
                if k <= 4611686018427387904 {
                    let j = i + k
                    if j < xs.length {
                        return xs[j]
                    }
                }
            }
        }
    }
    return 0
}
",
        "    print(w(xs, 4611686018427387904, 4611686018427387904).toString())",
        "array index -9223372036854775808 out of bounds",
    ),
    (
        "fn w(xs: Array<Int64>, i: Int64, k: Int64) -> Int64 {
    if i >= 0 {
        if i <= 4611686018427387904 {
            if k <= 0 {
                if k >= 0 - 4611686018427387904 {
                    let j = i - k
                    if j < xs.length {
                        return xs[j]
                    }
                }
            }
        }
    }
    return 0
}
",
        "    print(w(xs, 4611686018427387904, 0 - 4611686018427387904).toString())",
        "array index -9223372036854775808 out of bounds",
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

/// Witnesses of the halving lemma: the literal halving shift is proved, the
/// index stays and traps.
const HALVING: &[(&str, &str, &str)] = &[
    // A `UInt8` halves at most seven times, not six.
    (
        "fn w(xs: Array<Int64>, b: UInt8) -> Int64 {
    let mut u = b
    let mut k: Int64 = 0
             while u > 1 {
        u = u >> 1
        k = k + 1
    }
    if xs.length > 6 {
                 return xs[k]
    }
    return 0
}
",
        "    print(w([0, 0, 0, 0, 0, 0, 0], 255).toString())",
        "array index 7 out of bounds",
    ),
    // A second write of the halved name.
    (
        "fn w(xs: Array<Int64>, b: UInt8) -> Int64 {
    let mut u = b
    let mut k: Int64 = 0
             let mut again = true
    while u > 1 {
        u = u >> 1
        k = k + 1
                 if again {
            u = 255
            again = false
        }
    }
             if xs.length > 7 {
        return xs[k]
    }
    return 0
}
",
        "    print(w([0, 0, 0, 0, 0, 0, 0, 0], 255).toString())",
        "array index 8 out of bounds",
    ),
    // A `continue` skips the halving.
    (
        "fn w(xs: Array<Int64>, b: UInt8) -> Int64 {
    let mut u = b
    let mut k: Int64 = 0
             while u > 1 {
        k = k + 1
        if k == 1 {
            continue
        }
                 u = u >> 1
    }
    if xs.length > 7 {
        return xs[k]
    }
    return 0
}
",
        "    print(w([0, 0, 0, 0, 0, 0, 0, 0], 255).toString())",
        "array index 8 out of bounds",
    ),
];

#[test]
fn every_halving_witness_keeps_its_index_and_traps_there() {
    let mut failures = Vec::new();
    for (src, calls, trap) in HALVING {
        let rows = verdicts(src, "w");
        if rows != ["proved shift-range", "check array-index"] {
            failures.push(format!(
                "{src}
rows: {rows:?}"
            ));
        }
        let (err, _) = oracle(src, calls, "w");
        if !err.contains(trap) || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED) {
            failures.push(format!(
                "{src}
ran: {err}"
            ));
        }
    }
    assert!(
        failures.is_empty(),
        "{}",
        failures.join(
            "

"
        )
    );
}

/// Witnesses of callee summaries (`vyrn_lower::elide::summaries`): each a
/// program whose `w` holds one index a wrong summary would prove, the files
/// beside `p.vyrn`, and the trap its `main` reaches there.
const CALLEE_WITNESSES: &[(&str, &[(&str, &str)], &str)] = &[
    // A private function shares its name with one in another module.
    (
        r#"import { viaB } from "./b"
fn pick(i: Int64) -> Int64 {
    return i + 1
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    if i >= 0 {
        if i < xs.length {
            return xs[pick(i)]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print((viaB(xs, 0) + w(xs, 2)).toString())
    return 0
}
"#,
        &[(
            "b.vyrn",
            r#"fn pick(i: Int64) -> Int64 {
    return i
}
export fn viaB(xs: Array<Int64>, i: Int64) -> Int64 {
    if i >= 0 && i < xs.length {
        return xs[pick(i)]
    }
    return 0
}
"#,
        )],
        "array index 3 out of bounds",
    ),
    // A function named as a value is called through the value.
    (
        r#"fn ok(i: Int64) -> Int64 {
    return i
}
fn bad(i: Int64) -> Int64 {
    return i + 1
}
fn w(xs: Array<Int64>, i: Int64, c: Bool) -> Int64 {
    let f = if c { ok } else { bad }
    if i >= 0 {
        if i < xs.length {
            return xs[f(i)]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print((ok(0) + w(xs, 2, false)).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // A `fn`-typed parameter is bound at the call.
    (
        r#"fn ok(i: Int64) -> Int64 {
    return i
}
fn bad(i: Int64) -> Int64 {
    return i + 1
}
fn apply(f: fn(Int64) -> Int64, i: Int64) -> Int64 {
    return f(i)
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    if i >= 0 {
        if i < xs.length {
            return xs[apply(bad, i)]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print((apply(ok, 0) + w(xs, 2)).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // Every arm of a `return match` is a return.
    (
        r#"type K = | Zero | More
fn pick(i: Int64, k: K) -> Int64 {
    return match k {
        Zero => i,
        More => i + 1,
    }
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    if i >= 0 {
        if i < xs.length {
            return xs[pick(i, More)]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print((pick(0, Zero) + w(xs, 2)).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // A record name rebound to a longer record.
    (
        r#"type P = { a: Array<Int64> }
fn last(p: P) -> Int64 {
    let mut q = P { a: p.a.copy() }
    if q.a.length == 0 {
        return 0
    }
    let n = q.a.length - 1
    q = P { a: [1, 2, 3, 4, 5, 6] }
    return q.a.length - 1
}
fn w(xs: Array<Int64>) -> Int64 {
    let j = last(P { a: xs.copy() })
    if j >= 0 {
        return xs[j]
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs).toString())
    return 0
}
"#,
        &[],
        "array index 5 out of bounds",
    ),
    // An import alias names another module's function.
    (
        r#"import { pick as choose } from "./b"
fn pick(i: Int64) -> Int64 {
    return i
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    if i >= 0 {
        if i < xs.length {
            return xs[choose(i)]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print((pick(0) + w(xs, 2)).toString())
    return 0
}
"#,
        &[(
            "b.vyrn",
            r#"export fn pick(i: Int64) -> Int64 {
    return i + 1
}
"#,
        )],
        "array index 3 out of bounds",
    ),
    // A namespace call names another module's function.
    (
        r#"import * as ns from "./b"
fn pick(i: Int64) -> Int64 {
    return i
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    if i >= 0 {
        if i < xs.length {
            return xs[ns.pick(i)]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print((pick(0) + w(xs, 2)).toString())
    return 0
}
"#,
        &[(
            "b.vyrn",
            r#"export fn pick(i: Int64) -> Int64 {
    return i + 1
}
"#,
        )],
        "array index 3 out of bounds",
    ),
    // A protocol method dispatches on the receiver's type.
    (
        r#"protocol Pick {
    fn pick(self, i: Int64) -> Int64
}
type A = { tag: Int64 }
type B = { tag: Int64 }
impl Pick for A {
    fn pick(self, i: Int64) -> Int64 {
        return i
    }
}
impl Pick for B {
    fn pick(self, i: Int64) -> Int64 {
        return i + 1
    }
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    let b = B { tag: 0 }
    if i >= 0 {
        if i < xs.length {
            return xs[b.pick(i)]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let a = A { tag: 0 }
    print((a.pick(0) + w(xs, 2)).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // A generic body dispatches on its type argument.
    (
        r#"protocol Off {
    fn off(self) -> Int64
}
type A = { tag: Int64 }
type B = { tag: Int64 }
impl Off for A {
    fn off(self) -> Int64 {
        return 0
    }
}
impl Off for B {
    fn off(self) -> Int64 {
        return 1
    }
}
fn pick<T: Off>(x: T, i: Int64) -> Int64 {
    let o = x.off()
    if o == 0 {
        return i
    }
    return i + 1
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    let b = B { tag: 0 }
    if i >= 0 {
        if i < xs.length {
            return xs[pick(b, i)]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let a = A { tag: 0 }
    print((pick(a, 0) + w(xs, 2)).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // A callee reads a mutable global another call wrote.
    (
        r#"let mut off: Int64 = 0
fn pick(i: Int64) -> Int64 {
    return i + off
}
fn bump() {
    off = 1
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    bump()
    if i >= 0 {
        if i < xs.length {
            return xs[pick(i)]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs, 2).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // A module-state initializer calls a function.
    (
        r#"import { start } from "./b"
let mut off: Int64 = start()
fn pick(i: Int64) -> Int64 {
    if off == 0 {
        return i
    }
    return i + off
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    if i >= 0 {
        if i < xs.length {
            return xs[pick(i)]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs, 2).toString())
    return 0
}
"#,
        &[(
            "b.vyrn",
            r#"export fn start() -> Int64 {
    return 1
}
"#,
        )],
        "array index 3 out of bounds",
    ),
    // The callee shrinks its `modify` array argument.
    (
        r#"fn pick(xs: modify Array<Int64>, i: Int64) -> Int64 {
    let p = xs.pop() ?? 0
    return i
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    let mut ys = xs.copy()
    if i >= 0 {
        if i < ys.length {
            let j = pick(ys, i)
            return ys[j]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs, 2).toString())
    return 0
}
"#,
        &[],
        "array index 2 out of bounds",
    ),
    // The callee empties a field of its `modify` record argument.
    (
        r#"type P = { a: Array<Int64> }
fn pick(p: modify P, i: Int64) -> Int64 {
    p.a = []
    return i
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    let mut p = P { a: xs.copy() }
    if i >= 0 {
        if i < p.a.length {
            let j = pick(p, i)
            return p.a[j]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs, 0).toString())
    return 0
}
"#,
        &[],
        "array index 0 out of bounds",
    ),
    // The result is the entry length of a `modify` argument the callee shrank.
    (
        r#"fn popLast(xs: modify Array<Int64>) -> Int64 {
    let n = xs.length
    if n == 0 {
        panic("empty")
    }
    let p = xs.pop() ?? 0
    return n - 1
}
fn w(xs: Array<Int64>) -> Int64 {
    let mut ys = xs.copy()
    let j = popLast(ys)
    return ys[j]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs).toString())
    return 0
}
"#,
        &[],
        "array index 2 out of bounds",
    ),
    // `k = rest(xs, k)`: the argument is the old `k`, the result the new.
    (
        r#"fn rest(a: Array<Int64>, i: Int64) -> Int64 {
    if i < 0 {
        panic("bad")
    }
    if i > a.length {
        panic("bad")
    }
    return a.length - i
}
fn w(xs: Array<Int64>, k0: Int64) -> Int64 {
    let mut k = k0
    if k >= 0 {
        if k <= xs.length {
            k = rest(xs, k)
            if k >= 1 {
                return xs[k]
            }
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs, 0).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // A `swapRemove` after the call shrinks the array the result indexes.
    (
        r#"fn lastIndex(a: Array<Int64>) -> Int64 {
    if a.length == 0 {
        panic("empty")
    }
    return a.length - 1
}
fn w(xs: Array<Int64>) -> Int64 {
    let mut ys = xs.copy()
    let j = lastIndex(ys)
    let gone = ys.swapRemove(0)
    return ys[j]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs).toString())
    return 0
}
"#,
        &[],
        "array index 2 out of bounds",
    ),
    // A result narrowed to `Int32` wraps.
    (
        r#"fn low(i: Int64) -> Int32 {
    return Int32(i)
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    let k = Int64(low(i))
    if k >= 0 {
        if k < xs.length {
            return xs[i]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs, 4294967297).toString())
    return 0
}
"#,
        &[],
        "array index 4294967297 out of bounds",
    ),
    // Mutually recursive bodies assume each other's facts.
    (
        r#"fn down(i: Int64, n: Int64) -> Int64 {
    if n <= 0 {
        return i + 1
    }
    return up(i, n - 1)
}
fn up(i: Int64, n: Int64) -> Int64 {
    if n <= 0 {
        return i
    }
    return down(i, n - 1)
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    if i >= 0 {
        if i < xs.length {
            return xs[up(i, 3)]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs, 2).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // An array result shorter than the argument.
    (
        r#"fn shorter(a: Array<Int64>) -> Array<Int64> {
    let mut b = a.copy()
    let p = b.pop() ?? 0
    return b
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    let ys = shorter(xs)
    if i >= 0 {
        if i < xs.length {
            return ys[i]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs, 2).toString())
    return 0
}
"#,
        &[],
        "array index 2 out of bounds",
    ),
    // A reader and its callee first visited in one round, the callee first:
    // the callee lowers before the reader states it reads it.
    (
        r#"fn bump(i: Int64) -> Int64 {
    return i + 1
}
fn pick(i: Int64) -> Int64 {
    return bump(i)
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    if i >= 0 {
        if i < xs.length {
            return xs[pick(i)] + xs[bump(i) - 1]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs, 2).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
];

/// `c{depth}` calls `c{depth - 1}` and so on down to `c0`, which returns
/// `last`, each declared before its callee: the summary of `c0` reaches `w`
/// only after `depth` bodies lower theirs (research witnesses `spec3_3` and
/// `spec3_4`, where a round cap kept the first value).
fn chain(depth: usize, last: &str) -> String {
    let mut src: String = (1..=depth)
        .rev()
        .map(|k| {
            format!(
                "fn c{k}(i: Int64) -> Int64 {{\n    return c{}(i)\n}}\n",
                k - 1
            )
        })
        .collect();
    src.push_str(&format!(
        "fn c0(i: Int64) -> Int64 {{\n    return {last}\n}}\n"
    ));
    src.push_str(&format!(
        "fn w(xs: Array<Int64>, i: Int64) -> Int64 {{\n    if i >= 0 {{\n        \
         if i < xs.length {{\n            return xs[c{depth}(i)]\n        }}\n    }}\n    \
         return 0\n}}\nfn main() -> Int64 {{\n    let xs: Array<Int64> = [10, 20, 30]\n    \
         print(w(xs, 2).toString())\n    return 0\n}}\n"
    ));
    src
}

/// Writes `root` as `p.vyrn` and each of `files` beside it, in a fresh
/// directory.
fn modules(root: &str, files: &[(&str, &str)]) -> (Scratch, std::path::PathBuf) {
    let dir = scratch("elide");
    for (name, src) in files {
        std::fs::write(dir.join(name), src).unwrap();
    }
    let file = dir.join("p.vyrn");
    std::fs::write(&file, root).unwrap();
    (dir, file)
}

#[test]
fn every_callee_witness_keeps_its_index_and_traps_there() {
    let deep = chain(70, "i + 1");
    let all = CALLEE_WITNESSES.iter().copied().chain([(
        deep.as_str(),
        &[][..],
        "array index 3 out of bounds",
    )]);
    let mut failures = Vec::new();
    for (root, files, trap) in all {
        let (dir, file) = modules(root, files);
        let rows = rows_of(&file, "w");
        if rows.is_empty() || rows.iter().any(|v| v.starts_with("proved")) {
            failures.push(format!("{root}\nrows: {rows:?}"));
        }
        let (err, _) = run_oracle(&dir, &file, "w");
        if !err.contains(trap) || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED) {
            failures.push(format!("{root}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Programs whose `w` indexes by what a callee returns: the callee's summary
/// proves the index, and the run passes the oracle.
const CALLEE_PROOFS: &[&str] = &[
    r#"fn pick(i: Int64) -> Int64 {
    return i
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    if i >= 0 {
        if i < xs.length {
            return xs[pick(i)]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs, 2).toString())
    return 0
}
"#,
    r#"fn lastIndex(a: Array<Int64>) -> Int64 {
    if a.length < 1 {
        panic("empty")
    }
    return a.length - 1
}
fn w(xs: Array<Int64>) -> Int64 {
    let j = lastIndex(xs)
    return xs[j]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs).toString())
    return 0
}
"#,
    r#"fn width(b: Array<Int64>, i: Int64) -> Int64 {
    if b[i] < 15 {
        return 1
    }
    if i + 2 > b.length {
        return 0
    }
    return 2
}
fn w(xs: Array<Int64>) -> Int64 {
    let n = xs.length
    let mut s: Int64 = 0
    let mut i: Int64 = 0
    while i < n {
        s = s + xs[i]
        let k = width(xs, i)
        if k == 0 {
            return s
        }
        i = i + k
    }
    return s
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs).toString())
    return 0
}
"#,
    r#"fn longer(a: Array<Int64>) -> Array<Int64> {
    let mut b: Array<Int64> = []
    b.append(a)
    b.push(0)
    return b
}
fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    let ys = longer(xs)
    if i >= 0 {
        if i < xs.length {
            return ys[i]
        }
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs, 2).toString())
    return 0
}
"#,
];

#[test]
fn a_callee_summary_proves_an_index() {
    let deep = chain(70, "i");
    let mut failures = Vec::new();
    for root in CALLEE_PROOFS.iter().copied().chain([deep.as_str()]) {
        let (dir, file) = modules(root, &[]);
        let rows = rows_of(&file, "w");
        if rows != ["proved array-index"] {
            failures.push(format!("{root}\nrows: {rows:?}"));
        }
        let (err, _) = run_oracle(&dir, &file, "w");
        if !err.is_empty() {
            failures.push(format!("{root}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Programs where a wrong `pre` of `w` would prove its check: each keeps it
/// and traps there. The check is in the root file unless a file is named
/// `b.vyrn` and holds `w`; a witness with no trap reaches no failing call
/// in a run, because only a host would make it.
const CALLER_WITNESSES: &[(&str, &[(&str, &str)], &str)] = &[
    // A caller passes an index it does not prove.
    (
        r#"fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    return xs[i]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let mut s = 0
    let mut i = 0
    while i < xs.length {
        s = s + w(xs, i)
        i = i + 1
    }
    print((s + w(xs, 3)).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // A lambda's call row passes an index it does not prove.
    (
        r#"fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    return xs[i]
}
fn apply(f: fn(Int64) -> Int64, k: Int64) -> Int64 {
    return f(k)
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let mut s = 0
    let mut i = 0
    while i < xs.length {
        s = s + w(xs, i)
        i = i + 1
    }
    print((s + apply(k -> w(xs, k), 3)).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // An export passes its own parameter on; the export's caller does not
    // prove it.
    (
        r#"fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    return xs[i]
}
export fn viaE(xs: Array<Int64>, i: Int64) -> Int64 {
    return w(xs, i)
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let mut s = 0
    let mut i = 0
    while i < xs.length {
        s = s + w(xs, i)
        i = i + 1
    }
    print((s + viaE(xs, 3)).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // `w` is an export only a proving caller reaches here. The host may call
    // it with anything, so it stays kept; no run makes that call.
    (
        r#"export fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    return xs[i]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs, 0).toString())
    return 0
}
"#,
        &[],
        "",
    ),
    // The function is passed as a `fn`-typed argument and called through it.
    (
        r#"fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    return xs[i]
}
fn apply(f: fn(Array<Int64>, Int64) -> Int64, xs: Array<Int64>, i: Int64) -> Int64 {
    return f(xs, i)
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let mut s = 0
    let mut i = 0
    while i < xs.length {
        s = s + w(xs, i)
        i = i + 1
    }
    print((s + apply(w, xs, 3)).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // The function is a value bound by `let` and called through it.
    (
        r#"fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    return xs[i]
}
fn other(xs: Array<Int64>, i: Int64) -> Int64 {
    return i
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let mut s = 0
    let mut i = 0
    while i < xs.length {
        s = s + w(xs, i)
        i = i + 1
    }
    let f = if s > 0 { w } else { other }
    print((s + f(xs, 3)).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // A `fn`-typed parameter leaves the argument list, so argument `k` is
    // not parameter `k`.
    (
        r#"fn id(x: Int64) -> Int64 {
    return x
}
fn w(f: fn(Int64) -> Int64, i: Int64, j: Int64, xs: Array<Int64>, zs: Array<Int64>) -> Int64 {
    return f(j) + xs[i] + zs.length
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let zs: Array<Int64> = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10]
    print(w(id, 5, 0, xs, zs).toString())
    return 0
}
"#,
        &[],
        "array index 5 out of bounds",
    ),
    // The recursive call steps the index past the end.
    (
        r#"fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    let v = xs[i]
    if v > 100 {
        return v
    }
    return w(xs, i + 1)
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    if xs.length > 0 {
        print(w(xs, 0).toString())
    }
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // Mutual recursion steps the index past the end.
    (
        r#"fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    let v = xs[i]
    if v > 100 {
        return v
    }
    return u(xs, i)
}
fn u(xs: Array<Int64>, i: Int64) -> Int64 {
    return w(xs, i + 1)
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    if xs.length > 0 {
        print(w(xs, 0).toString())
    }
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // A private caller passes its own unproved parameter on.
    (
        r#"fn w(xs: Array<Int64>, i: Int64) -> String {
    return xs[i].toString()
}
fn a(xs: Array<Int64>, i: Int64) {
    print(w(xs, i))
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let mut i = 0
    while i < xs.length {
        a(xs, i)
        i = i + 1
    }
    a(xs, 3)
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // A caller's own entry candidates would prove the callee's; the caller's
    // caller does not prove them.
    (
        r#"fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    return xs[i]
}
fn a(xs: Array<Int64>, i: Int64) -> Int64 {
    let v = xs[i - 1]
    return v + w(xs, i)
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let ys: Array<Int64> = [1, 2, 3, 4, 5, 6, 7, 8, 9, 10, 11, 12]
    let mut s = 0
    let mut i = 0
    while i < ys.length {
        s = s + ys[i] * 2 + i
        i = i + 1
    }
    let mut j = 0
    while j < xs.length {
        s = s + w(xs, j)
        j = j + 1
    }
    print((s + a(xs, 3)).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // The caller proves the fact, then a store breaks it before the call.
    (
        r#"fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    return xs[i]
}
fn g(xs: Array<Int64>, k: Int64) -> Int64 {
    if k >= 0 && k < xs.length {
        let mut j = k
        let s = w(xs, j)
        j = j + 2
        return s + w(xs, j)
    }
    return 0
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(g(xs, 1).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // The caller proves the fact, then a pop breaks it before the call.
    (
        r#"fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    return xs[i]
}
fn g(xs: modify Array<Int64>, k: Int64) -> Int64 {
    if k >= 0 && k < xs.length {
        let s = w(xs, k)
        let _ = xs.pop()
        return s + w(xs, k)
    }
    return 0
}
fn main() -> Int64 {
    let mut xs: Array<Int64> = [10, 20, 30]
    print(g(xs, 2).toString())
    return 0
}
"#,
        &[],
        "array index 2 out of bounds",
    ),
    // A caller whose summary every return refutes before the call row.
    (
        r#"fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    return xs[i]
}
fn g(xs: Array<Int64>, k: Int64, m: Int64) -> Int64 {
    if m > 0 {
        return m * m
    }
    return w(xs, k + 1)
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let mut s = 0
    let mut i = 0
    while i < xs.length {
        s = s + w(xs, i)
        i = i + 1
    }
    print((s + g(xs, 2, 0)).toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // A caller with no summary of its own proves the fact from a callee's
    // summary that the callee's walk lowers later.
    (
        r#"fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    return xs[i]
}
fn pick(i: Int64) -> Int64 {
    return i + 1
}
fn c(xs: Array<Int64>, i: Int64) {
    if i >= 0 && i < xs.length {
        print(w(xs, pick(i)).toString())
    }
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let mut s = 0
    let mut i = 0
    while i < xs.length {
        s = s + w(xs, i)
        i = i + 1
    }
    c(xs, 2)
    print(s.toString())
    return 0
}
"#,
        &[],
        "array index 3 out of bounds",
    ),
    // A generator body passes an unproved index; the trap is at load.
    (
        r#"import { g } from "./b"
fn main() -> Int64 {
    print(derive(g, 1).toString())
    return 0
}
"#,
        &[(
            "b.vyrn",
            r#"fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    return xs[i]
}
export gen fn g(t: TypeArg) -> String {
    let xs: Array<Int64> = [10, 20, 30]
    let mut s = 0
    let mut i = 0
    while i < xs.length {
        s = s + w(xs, i)
        i = i + 1
    }
    let k = t.roots.length + 2
    return "fn shown(v: Int64) -> Int64 {\n    return " + (s + w(xs, k)).toString() + "\n}\n"
}
"#,
        )],
        "array index 3 out of bounds",
    ),
];

#[test]
fn every_caller_witness_keeps_its_check_and_traps_there() {
    let mut failures = Vec::new();
    for (root, files, trap) in CALLER_WITNESSES.iter().copied() {
        let (dir, file) = modules(root, files);
        let site = match files.iter().any(|(_, src)| src.contains("fn w(")) {
            true => dir.join("b.vyrn"),
            false => file.clone(),
        };
        let rows = rows_of(&site, "w");
        if rows.is_empty() || rows.iter().any(|v| v.starts_with("proved")) {
            failures.push(format!("{root}\nrows: {rows:?}"));
        }
        let (err, _) = run_oracle(&dir, &file, "w");
        if !err.contains(trap) || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED) {
            failures.push(format!("{root}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Programs whose `w` every caller calls with what its checks need: the
/// facts every call row proves prove them, and the run passes the oracle.
const CALLER_PROOFS: &[&str] = &[
    r#"fn w(xs: Array<Int64>, i: Int64) -> Int64 {
    return xs[i]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let mut s = 0
    let mut i = 0
    while i < xs.length {
        s = s + w(xs, i)
        i = i + 1
    }
    print(s.toString())
    return 0
}
"#,
    r#"fn w(xs: Array<Int64>) -> Int64 {
    return xs[0]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let ys: Array<Int64> = [7]
    print((w(xs) + w(ys)).toString())
    return 0
}
"#,
    r#"fn w(a: Int64, b: Int64) -> Int64 {
    return a / b
}
fn main() -> Int64 {
    print((w(10, 2) + w(9, 3)).toString())
    return 0
}
"#,
];

#[test]
fn every_call_row_proving_a_fact_proves_the_check() {
    let mut failures = Vec::new();
    for root in CALLER_PROOFS {
        let (dir, file) = modules(root, &[]);
        let rows = rows_of(&file, "w");
        if rows.is_empty() || rows.iter().any(|v| !v.starts_with("proved")) {
            failures.push(format!("{root}\nrows: {rows:?}"));
        }
        let (err, _) = run_oracle(&dir, &file, "w");
        if !err.is_empty() {
            failures.push(format!("{root}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}
