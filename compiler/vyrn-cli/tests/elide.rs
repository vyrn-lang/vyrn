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

/// A parameter's `where` clause is the body's premise and a check at each call
/// row: proved where the caller's facts show it, kept and trapping where they
/// do not. The body is exported, so no inferred entry fact proves it.
#[test]
fn a_parameter_clause_moves_the_check_to_the_call() {
    let src =
        "export fn at(b: Array<Int64>, i: Int64 where value >= 0 && value < b.length) -> Int64 {
    return b[i]
}
fn sum(b: Array<Int64>) -> Int64 {
    let mut s: Int64 = 0
    let mut i: Int64 = 0
    while i < b.length {
        s = s + at(b, i)
        i = i + 1
    }
    return s
}
fn past(b: Array<Int64>) -> Int64 {
    return at(b, b.length)
}
";
    assert_eq!(verdicts(src, "at"), ["proved array-index"]);
    assert_eq!(verdicts(src, "sum"), ["proved where-arg"]);
    let calls = "    print(sum(xs).toString())\n    print(past(xs).toString())";
    let (err, rows) = oracle(src, calls, "past");
    assert_eq!(
        (err.as_str(), rows),
        (
            "error: validation failed for parameter `i` of `at`\n",
            vec!["14 0 where-arg kept 1".to_string()]
        )
    );
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
fn two_arrays_a_loop_grows_in_step_keep_equal_lengths() {
    let src = "fn w(n: Int64) -> Int64 {
    let mut a: Array<Int64> = []
    let mut b: Array<Int64> = []
    let mut i: Int64 = 0
    while i < n {
        a.push(i)
        b.push(i * 2)
        i = i + 1
    }
    let mut s: Int64 = 0
    let mut k: Int64 = 0
    while k < a.length {
        s = s + b[k]
        k = k + 1
    }
    return s
}
";
    assert_eq!(verdicts(src, "w"), ["proved array-index"]);
    let (err, rows) = oracle(src, "    print(w(3).toString())", "w");
    assert_eq!(
        (err.as_str(), rows),
        ("", vec!["13 0 array-index proved 3".to_string()])
    );
}

/// A loop bound by a literal that pushes once a turn leaves at least the
/// bound's elements; a turn that skips or undoes its push keeps the check.
#[test]
fn a_loop_bound_by_a_literal_counts_its_pushes() {
    let proof = "fn w(n: Int64) -> Int64 {
    let mut t: Array<Int64> = [0]
    let mut k = 1
    while k < 256 {
        t.push(t[k - 1] + n)
        k = k + 1
    }
    let mut j = 0
    while j < n {
        t[Int64(UInt8(j))] = j
        j = j + 1
    }
    return t[255]
}
";
    assert_eq!(verdicts(proof, "w"), ["proved array-index"; 3]);
    let (err, _) = oracle(proof, "    print(w(3).toString())", "w");
    assert_eq!(err, "");
    let witnesses = [
        "fn w(n: Int64) -> Int64 {
    let mut t: Array<Int64> = []
    let mut k = 0
    while k < 256 {
        if k < n {
            t.push(k)
        }
        k = k + 1
    }
    return t[255]
}
",
        "fn empty(t: modify Array<Int64>) {
    t.clear()
}
fn w(n: Int64) -> Int64 {
    let mut t: Array<Int64> = []
    let mut k = 0
    while k <= 255 {
        t.push(k)
        if k == n {
            empty(t)
        }
        k = k + 1
    }
    return t[255]
}
",
        "fn w(n: Int64) -> Int64 {
    let mut t: Array<Int64> = []
    let mut k = 0
    while k < 256 {
        t.push(k)
        k = k + 1
        if k == n {
            k = k + 1
        }
    }
    return t[255]
}
",
    ];
    let mut failures = Vec::new();
    for src in witnesses {
        let rows = verdicts(src, "w");
        if rows != ["check array-index"] {
            failures.push(format!("{src}\nrows: {rows:?}"));
        }
        let (err, _) = oracle(src, "    print(w(10).toString())", "w");
        if !err.contains("array index 255 out of bounds")
            || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED)
        {
            failures.push(format!("{src}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A function that returns a table of a literal size states that length
/// of its result; a path that returns a shorter one, or a `modify` call that
/// shortens it, keeps the caller's check.
#[test]
fn a_result_of_a_literal_size_proves_its_callers_index() {
    let table = "fn table(n: Int64) -> Array<Int64> {
    let mut t: Array<Int64> = []
    let mut k = 0
    while k < 8 {
        t.push(k + n)
        k = k + 1
    }
    return t
}
";
    let proof = format!(
        "{table}fn w(x: Int64) -> Int64 {{
    let t = table(x)
    return t[x & 7]
}}
"
    );
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    let (err, _) = oracle(&proof, "    print(w(15).toString())", "w");
    assert_eq!(err, "");
    let witnesses = [
        "fn table(n: Int64) -> Array<Int64> {
    if n > 10 {
        return [1, 2, 3]
    }
    let mut t: Array<Int64> = []
    let mut k = 0
    while k < 8 {
        t.push(k)
        k = k + 1
    }
    return t
}
fn w(x: Int64) -> Int64 {
    let t = table(x)
    return t[x & 7]
}
"
        .to_string(),
        format!(
            "{table}fn empty(t: modify Array<Int64>) {{
    t.clear()
}}
fn w(x: Int64) -> Int64 {{
    let mut t = table(x)
    empty(t)
    return t[x & 7]
}}
"
        ),
    ];
    let mut failures = Vec::new();
    for src in &witnesses {
        let rows = verdicts(src, "w");
        if rows != ["check array-index"] {
            failures.push(format!("{src}\nrows: {rows:?}"));
        }
        let (err, _) = oracle(src, "    print(w(15).toString())", "w");
        if !err.contains("array index 7 out of bounds")
            || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED)
        {
            failures.push(format!("{src}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A function whose loop, bound by an integer parameter, pushes to or
/// stores into every index of the array it returns states that its result is
/// at least that long; a path that makes one entry fewer, a store the loop
/// guards by the length, or a `modify` call that shortens the result keeps
/// the caller's check.
#[test]
fn a_result_as_long_as_a_parameter_proves_its_callers_index() {
    let filled = "fn filled(n: Int64) -> Array<Int64> {
    let mut t: Array<Int64> = []
    let mut i = 0
    while i < n {
        t.push(i)
        i = i + 1
    }
    return t
}
";
    let sum = "fn w(x: Int64) -> Int64 {
    let t = make(x)
    let mut s = 0
    let mut i = 0
    while i < x {
        s = s + t[i]
        i = i + 1
    }
    return s
}
";
    let stored = "fn make(n: Int64) -> Array<Int64> {
    let mut t = filled(n)
    let mut i = 0
    while i < n {
        t[i] = i * 2
        i = i + 1
    }
    return t
}
";
    for proof in [
        format!("{}{sum}", filled.replace("filled", "make")),
        format!("{filled}{stored}{sum}"),
    ] {
        assert_eq!(verdicts(&proof, "w"), ["proved array-index"], "{proof}");
        let (err, _) = oracle(&proof, "    print(w(5).toString())", "w");
        assert_eq!(err, "");
    }
    let witnesses = [
        format!(
            "fn make(n: Int64) -> Array<Int64> {{
    let mut t: Array<Int64> = []
    let mut i = 0
    if n > 3 {{
        i = 1
    }}
    while i < n {{
        t.push(i)
        i = i + 1
    }}
    return t
}}
{sum}"
        ),
        format!(
            "{filled}fn make(n: Int64) -> Array<Int64> {{
    let mut t = filled(n - 1)
    let mut i = 0
    while i < n {{
        if i < t.length {{
            t[i] = i
        }}
        i = i + 1
    }}
    return t
}}
{sum}"
        ),
        format!(
            "{filled}fn empty(t: modify Array<Int64>) {{
    t.pop()
}}
fn w(x: Int64) -> Int64 {{
    let mut t = filled(x)
    empty(t)
    let mut s = 0
    let mut i = 0
    while i < x {{
        s = s + t[i]
        i = i + 1
    }}
    return s
}}
"
        ),
    ];
    let mut failures = Vec::new();
    for src in &witnesses {
        let rows = verdicts(src, "w");
        if rows != ["check array-index"] {
            failures.push(format!("{src}\nrows: {rows:?}"));
        }
        let (err, _) = oracle(src, "    print(w(5).toString())", "w");
        if !err.contains("array index 4 out of bounds")
            || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED)
        {
            failures.push(format!("{src}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// The bytes of a String literal are as long as the literal, and a function
/// that returns them states that length of its result. The three-operand
/// `bytes` states nothing, and a path that returns a shorter literal keeps
/// the caller's check.
#[test]
fn a_bytes_result_keeps_its_strings_length() {
    let proof = "fn table() -> Array<UInt8> {
    return bytes(\"ABCDEFGH\")
}
fn w(x: Int64) -> Int64 {
    let t = table()
    let u = bytes(\"ABCDEFGH\")
    let mut n = Int64(t[x & 7])
    let mut i = 0
    while i < 8 {
        n = n + Int64(u[i])
        i = i + 1
    }
    return n
}
";
    assert_eq!(verdicts(proof, "w"), ["proved array-index"; 2]);
    let (err, _) = oracle(proof, "    print(w(15).toString())", "w");
    assert_eq!(err, "");
    // The slice's own range check comes first.
    let witnesses = [
        (
            "fn w(x: Int64) -> Int64 {
    let t = bytes(\"ABCDEFGH\", 1, 8)
    let mut n = x
    let mut i = 0
    while i < 8 {
        n = n + Int64(t[i])
        i = i + 1
    }
    return n
}
",
            &["check string-index", "check array-index"][..],
        ),
        (
            "fn table(x: Int64) -> Array<UInt8> {
    if x > 10 {
        return bytes(\"ABC\")
    }
    return bytes(\"ABCDEFGH\")
}
fn w(x: Int64) -> Int64 {
    let t = table(x)
    return Int64(t[x & 7])
}
",
            &["check array-index"][..],
        ),
    ];
    let mut failures = Vec::new();
    for (src, want) in witnesses {
        let rows = verdicts(src, "w");
        if rows != want {
            failures.push(format!("{src}\nrows: {rows:?}"));
        }
        let (err, _) = oracle(src, "    print(w(15).toString())", "w");
        if !err.contains("array index 7 out of bounds")
            || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED)
        {
            failures.push(format!("{src}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A function whose every return returns one integer literal states its
/// result equal to it; a path that returns another literal, or a name,
/// keeps the caller's check.
#[test]
fn a_constant_result_proves_its_callers_index() {
    let read = "fn w(x: Int64) -> Int64 {
    let t: Array<Int64> = [1, 2, 3, 4, 5, 6, 7, 8]
    let mut keep = t.length
    if keep > eight(x) - x {
        keep = eight(x)
    }
    return t[keep - 1]
}
";
    let proof = format!("fn eight(x: Int64) -> Int64 {{\n    return 8\n}}\n{read}");
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    let (err, _) = oracle(&proof, "    print(w(15).toString())", "w");
    assert_eq!(err, "");
    let witnesses = [
        format!(
            "fn eight(x: Int64) -> Int64 {{
    if x > 10 {{
        return 9
    }}
    return 8
}}
{read}"
        ),
        format!(
            "fn eight(x: Int64) -> Int64 {{
    let mut r = 8
    if x > 10 {{
        r = 9
    }}
    return r
}}
{read}"
        ),
    ];
    let mut failures = Vec::new();
    for src in &witnesses {
        let rows = verdicts(src, "w");
        if rows != ["check array-index"] {
            failures.push(format!("{src}\nrows: {rows:?}"));
        }
        let (err, _) = oracle(src, "    print(w(15).toString())", "w");
        if !err.contains("array index 8 out of bounds")
            || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED)
        {
            failures.push(format!("{src}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A call to a generic function reads the summary of the instance its type
/// arguments name, which every return of that instance proves.
#[test]
fn a_generic_instances_result_proves_its_callers_index() {
    let read = "fn w(x: Int64) -> Int64 {
    let t: Array<Int64> = [1, 2, 3]
    return t[two(x, x)]
}
";
    let proof = format!("fn two<T>(v: T, x: Int64) -> Int64 {{\n    return 2\n}}\n{read}");
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    let witness = format!(
        "fn two<T>(v: T, x: Int64) -> Int64 {{
    if x > 10 {{
        return 3
    }}
    return 2
}}
{read}"
    );
    assert_eq!(verdicts(&witness, "w"), ["check array-index"]);
    let (err, _) = oracle(&witness, "    print(w(15).toString())", "w");
    assert!(
        err.contains("array index 3 out of bounds")
            && !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

#[test]
fn a_loop_condition_under_and_proves_its_index() {
    let src = "fn w(xs: Array<Int64>) -> Int64 {
    let mut s: Int64 = 0
    let mut j: Int64 = 0
    while j < xs.length && xs[j] > 0 {
        s = s + xs[j]
        j = j + 1
    }
    return s
}
";
    assert_eq!(
        verdicts(src, "w"),
        ["proved array-index", "proved array-index"]
    );
    let (err, rows) = oracle(src, "    print(w(xs).toString())", "w");
    assert_eq!(
        (err.as_str(), rows),
        (
            "",
            vec![
                "4 0 array-index proved 3".to_string(),
                "5 0 array-index proved 3".to_string()
            ]
        )
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

/// `i <= n - nl` bounds `i` by `LEN_MAX` through the two lengths' axioms, so
/// `i + 1` and `i + j` are exact and `i >= 0` survives the loop's head.
#[test]
fn a_bound_by_a_difference_of_lengths_keeps_the_counter_exact() {
    let src = "export fn w(s: String, t: String, from: Int64 where value >= 0) -> Int64 {
    let n = s.byteLength
    let nl = t.byteLength
    let mut i = from
    while i <= n - nl {
        let mut j = 0
        while j < nl && s[i + j] == t[j] {
            j = j + 1
        }
        if j == nl {
            return i
        }
        i = i + 1
    }
    return 0 - 1
}
";
    assert_eq!(
        verdicts(src, "w"),
        ["proved string-index", "proved string-index"]
    );
    let (err, rows) = oracle(src, "    print(w(\"abcab\", \"ab\", 1).toString())", "w");
    assert_eq!(
        (err.as_str(), rows),
        (
            "",
            vec![
                "7 0 string-index proved 4".to_string(),
                "7 1 string-index proved 4".to_string()
            ]
        )
    );
}

/// `std/strpred`'s `findSkipping` states `from >= 0` and a table of at least
/// 256 entries, so its reads are proved: an entry is a `UInt32`, so the step
/// `i + entry` is exact and keeps `i >= 0`. A call the caller's facts do not
/// show keeps its clause check and traps there: a short table, a negative
/// start, and a table a callee shortened through `modify`.
#[test]
fn a_skip_table_clause_proves_find_skippings_reads() {
    let src = "import { findSkipping, skipTable } from \"std/strpred\"
fn shorten(t: modify Array<UInt32>) {
    t.clear()
}
fn found(s: String) -> Int64 {
    return findSkipping(s, \"ab\", 0, skipTable(\"ab\", 1000))
}
fn short(s: String) -> Int64 {
    let t: Array<UInt32> = [UInt32(1)]
    return findSkipping(s, \"ab\", 0, t)
}
fn below(s: String, from: Int64) -> Int64 {
    return findSkipping(s, \"ab\", from, skipTable(\"ab\", 1000))
}
fn shortened(s: String) -> Int64 {
    let mut t = skipTable(\"ab\", 1000)
    shorten(t)
    return findSkipping(s, \"ab\", 0, t)
}
";
    let print = |call: &str| format!("    print({call}.toString())");
    let (err, rows) = oracle(src, &print("found(\"xxaxab\")"), "findSkipping");
    let verdicts: Vec<&str> = rows.iter().map(|r| r.split(' ').nth(3).unwrap()).collect();
    assert_eq!((err.as_str(), verdicts), ("", vec!["proved"; 4]));
    for (call, param, row) in [
        ("short(\"ab\")", "skip", "10 1 where-arg kept 1"),
        ("below(\"ab\", 0 - 1)", "from", "13 0 where-arg kept 1"),
        ("shortened(\"ab\")", "skip", "18 1 where-arg kept 1"),
    ] {
        let body = call.split('(').next().unwrap();
        let (err, rows) = oracle(src, &print(call), body);
        let says = format!("validation failed for parameter `{param}` of `findSkipping`");
        assert_eq!(err, format!("error: {says}\n"), "{call}");
        assert!(rows.iter().any(|r| r == row), "{call}: {rows:?}");
    }
}

/// `std/regex`'s `nextMatch` states `from >= 0`, so a search from a negative
/// start traps at the call, not at the first read of the haystack.
#[test]
fn a_negative_match_start_traps_at_the_call() {
    let src = "import { compile, find } from \"std/regex\"
fn at(from: Int64) -> Int64 {
    let re = match compile(\"ab\") {
        Ok(r) => r,
        Err(w) => panic(\"compile: \\{w}\"),
    }
    return match find(re, \"xxab\", from) { Some(m) => m.at, None => 0 - 1 }
}
";
    let (err, _) = oracle(src, "    print(at(0).toString())", "at");
    assert_eq!(err, "");
    let (err, rows) = oracle(src, "    print(at(0 - 1).toString())", "find");
    assert_eq!(
        err,
        "error: validation failed for parameter `from` of `nextMatch`\n"
    );
    assert!(
        rows.iter().any(|r| r.ends_with("where-arg kept 1")),
        "{rows:?}"
    );
}

#[test]
fn a_disequality_proves_its_divisor() {
    let src = "fn w(a: Int64, b: Int64) -> Int64 {
    if b == 0 {
        return 0
    }
    return a / b
}
";
    assert_eq!(
        verdicts(src, "w"),
        ["proved int-div-zero", "check int-div-overflow"]
    );
    let (err, rows) = oracle(src, "    print(w(7, -2).toString())", "w");
    assert_eq!(
        (err.as_str(), rows),
        (
            "",
            vec![
                "5 0 int-div-zero proved 1".to_string(),
                "5 1 int-div-overflow kept 1".to_string()
            ]
        )
    );
}

#[test]
fn a_passed_divisor_check_proves_the_next() {
    let src = "fn w(a: Int64, b: Int64) -> Int64 {\n    return a / b + a % b\n}\n";
    assert_eq!(
        verdicts(src, "w"),
        [
            "check int-div-zero",
            "check int-div-overflow",
            "proved int-rem-zero"
        ]
    );
}

#[test]
fn a_read_parameters_field_is_one_value() {
    let src = "type H = { slot: Int64, gen: Int64 }
fn w(h: H, xs: Array<Int64>) -> Int64 {
    if h.slot < 0 || h.slot >= xs.length {
        return 0
    }
    return xs[h.slot]
}
";
    assert_eq!(verdicts(src, "w"), ["proved array-index"]);
    let calls = "    print(w(H { slot: 2, gen: 0 }, xs).toString())";
    let (err, rows) = oracle(src, calls, "w");
    assert_eq!(
        (err.as_str(), rows),
        ("", vec!["6 0 array-index proved 1".to_string()])
    );
}

#[test]
fn a_join_of_constants_keeps_the_values_between_out() {
    let src = "fn w(x: Int64) -> Int64 {
    let t = [10, 20, 30]
    let mut k: Int64 = 0
    if x > 5 {
        k = 2
    } else if x > 2 {
        k = 3
    } else if x > 0 {
        k = 4
    }
    if k == 0 {
        return 0
    }
    return t[k - 2]
}
";
    assert_eq!(verdicts(src, "w"), ["proved array-index"]);
    let (err, rows) = oracle(src, "    print(w(1).toString())", "w");
    assert_eq!(
        (err.as_str(), rows),
        ("", vec!["14 0 array-index proved 1".to_string()])
    );
}

#[test]
fn a_copy_of_a_same_typed_name_needs_no_range_proof() {
    let src = "fn w(xs: Array<Int64>) -> Int64 {
    let mut s: Int64 = 0
    let mut i: Int64 = 0
    while i < xs.length {
        let x = xs[i]
        s = s + x
        let k = (x & 3) + 1
        i = i + k
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

/// A right shift or a quotient of a value in `[0, h]` lies below `h >> a` or
/// `h / d`: by a literal, and by an amount the state bounds below by `a`.
#[test]
fn a_right_shift_of_a_non_negative_value_is_bounded() {
    let src = "fn w(b: UInt8, x: Int64, k: Int64) -> Int64 {
    let db = \"0123456789abcdef\"
    let mut s = Int64(db[Int64(b >> 4)]) + Int64(db[Int64(b) / 16])
    if x >= 0 && x < 256 && k >= 4 {
        s = s + Int64(db[x >> k])
    }
    return s
}
";
    assert_eq!(
        verdicts(src, "w"),
        [
            "proved shift-range",
            "proved string-index",
            "proved int-div-zero",
            "proved int-div-overflow",
            "proved string-index",
            "check shift-range",
            "proved string-index",
        ]
    );
}

/// Each keeps the index of `w`'s shift and traps there: an arithmetic shift
/// of a negative value, and a bound one past the String by a literal and by
/// a ranged amount.
#[test]
fn every_shift_witness_keeps_its_index_and_traps_there() {
    let witnesses = [
        (
            "fn w(x: Int64) -> Int64 {
    let t = [1, 2, 3, 4]
    if x < 64 {
        return t[x >> 4]
    }
    return 0
}
",
            "    print(w(-1).toString())",
            "check array-index",
            "array index -1 out of bounds",
        ),
        (
            "fn w(b: UInt8) -> Int64 {
    let db = \"0123456789abcde\"
    return Int64(db[Int64(b >> 4)])
}
",
            "    print(w(255).toString())",
            "check string-index",
            "string index 15 out of bounds",
        ),
        (
            "fn w(b: UInt8, k: UInt8) -> Int64 {
    let db = \"0123456789abcde\"
    if k >= 4 {
        return Int64(db[Int64(b >> k)])
    }
    return 0
}
",
            "    print(w(255, 4).toString())",
            "check string-index",
            "string index 15 out of bounds",
        ),
    ];
    let mut failures = Vec::new();
    for (src, calls, row, trap) in witnesses {
        let rows = verdicts(src, "w");
        if !rows.iter().any(|r| r == row) {
            failures.push(format!("{src}\nrows: {rows:?}"));
        }
        let (err, _) = oracle(src, calls, "w");
        if !err.contains(trap) || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED) {
            failures.push(format!("{src}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
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
fn make(xs: Array<Int64>) -> P {
    return P { a: xs.copy(), b: [1] }
}
",
        "    print(w(make(xs)).toString())",
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
fn make(xs: Array<Int64>) -> P {
    return P { a: xs.copy(), b: [1, 2, 3], c: [1] }
}
",
        "    print(w(make(xs)).toString())",
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
fn make(xs: Array<Int64>) -> P {
    return P { a: xs.copy(), b: [1] }
}
",
        "    print(w(make(xs)).toString())",
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
    // The right operand of `&&` shrinks the array the left one bounded.
    (
        "fn w(xs: modify Array<Int64>) -> Int64 {
    let mut s: Int64 = 0
    let mut j: Int64 = 0
    while j < xs.length && (xs.pop() ?? 0) > 0 {
        s = s + xs[j]
        j = j + 1
    }
    return s
}
",
        "    print(w(xs).toString())",
        "array index 1 out of bounds",
    ),
    (
        "fn shrink(xs: modify Array<Int64>) -> Bool {
    xs.clear()
    return true
}
fn w(xs: modify Array<Int64>) -> Int64 {
    let mut s: Int64 = 0
    let mut j: Int64 = 0
    while j < xs.length && shrink(xs) {
        s = s + xs[j]
        j = j + 1
    }
    return s
}
",
        "    print(w(xs).toString())",
        "array index 0 out of bounds",
    ),
    // `||`: its truth bounds nothing, and its right operand may shrink the array.
    (
        "fn w(xs: Array<Int64>) -> Int64 {
    let mut s: Int64 = 0
    let mut j: Int64 = 0
    while j < xs.length || j < 5 {
        s = s + xs[j]
        j = j + 1
    }
    return s
}
",
        "    print(w(xs).toString())",
        "array index 3 out of bounds",
    ),
    (
        "fn shrink(xs: modify Array<Int64>) -> Bool {
    xs.clear()
    return false
}
fn w(xs: modify Array<Int64>, j: Int64) -> Int64 {
    if j < 0 || j >= xs.length || shrink(xs) {
        return 0
    }
    return xs[j]
}
",
        "    print(w(xs, 0).toString())",
        "array index 0 out of bounds",
    ),
    // A negated `&&` is true where the bound may fail.
    (
        "fn w(xs: Array<Int64>, j: Int64, c: Bool) -> Int64 {
    if !(j >= 0 && j < xs.length && c) {
        return xs[j]
    }
    return 0
}
",
        "    print(w(xs, 5, true).toString())",
        "array index 5 out of bounds",
    ),
    // A Bool temp stored again between its condition and its use.
    (
        "fn w(xs: Array<Int64>, j: Int64, c: Bool) -> Int64 {
    let mut ok = j >= 0 && j < xs.length && c
    ok = c
    if ok {
        return xs[j]
    }
    return 0
}
",
        "    print(w(xs, 7, true).toString())",
        "array index 7 out of bounds",
    ),
    // A loop condition under `&&` that lets the counter reach the length.
    (
        "fn w(xs: Array<Int64>) -> Int64 {
    let mut s: Int64 = 0
    let mut j: Int64 = 0
    while j <= xs.length && j < 9 {
        s = s + xs[j]
        j = j + 1
    }
    return s
}
",
        "    print(w(xs).toString())",
        "array index 3 out of bounds",
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
    // A field of a `modify` parameter is a fresh value at each read.
    (
        "type H = { slot: Int64, gen: Int64 }
fn bump(h: modify H) {
    h.slot = h.slot + 5
}
fn w(xs: Array<Int64>, h: modify H) -> Int64 {
    if h.slot < 0 || h.slot >= xs.length {
        return 0
    }
    bump(h)
    return xs[h.slot]
}
",
        "    let mut h = H { slot: 1, gen: 0 }\n    print(w(xs, h).toString())",
        "array index 6 out of bounds",
    ),
    // A field of a record the body stores to is a fresh value at each read.
    (
        "type H = { slot: Int64, gen: Int64 }
fn w(xs: Array<Int64>, h: H) -> Int64 {
    let mut r = h
    if r.slot < 0 || r.slot >= xs.length {
        return 0
    }
    r.slot = r.slot + 5
    return xs[r.slot]
}
",
        "    print(w(xs, H { slot: 1, gen: 0 }).toString())",
        "array index 6 out of bounds",
    ),
    // A branch that stores a computed value leaves no gap among the values.
    (
        "fn w(xs: Array<Int64>, x: Int64) -> Int64 {
    let t = [10, 20, 30]
    let mut k: Int64 = 0
    if x > 5 {
        k = 2
    } else if x > 0 {
        k = x - 1
    }
    if k == 0 {
        return 0
    }
    return t[k - 2]
}
",
        "    print(w(xs, 2).toString())",
        "array index -1 out of bounds",
    ),
    // A narrowing is a conversion, never a copy: the checker refuses a store
    // between integer types, so `j` takes `Int32(..)`'s wrapped value.
    (
        "fn w(xs: Array<Int64>, x: Int64) -> Int64 {
    if x >= 0 && x < 3 {
        let mut j: Int32 = 0
        j = Int32(x + 2147483648)
        return xs[Int64(j)]
    }
    return 0
}
",
        "    print(w(xs, 1).toString())",
        "array index -2147483647 out of bounds",
    ),
    // A store to the divisor drops its disequality.
    (
        "fn w(a: Int64, b: Int64) -> Int64 {\n    let mut c = b\n    if c == 0 {\n        return 0\n    }\n    \
         c = a\n    return a / c\n}\n",
        "    print(w(0, 3).toString())",
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
    // A loop that grows one array on some turns only: the lengths part.
    (
        "fn w(n: Int64) -> Int64 {
    let mut a: Array<Int64> = []
    let mut b: Array<Int64> = []
    let mut i: Int64 = 0
    while i < n {
        a.push(i)
        if i > 0 {
            b.push(i)
        }
        i = i + 1
    }
    let mut s: Int64 = 0
    let mut k: Int64 = 0
    while k < a.length {
        s = s + b[k]
        k = k + 1
    }
    return s
}
",
        "    print(w(3).toString())",
        "array index 2 out of bounds",
    ),
    // Two arrays grown in step from unequal lengths stay unequal.
    (
        "fn w(n: Int64) -> Int64 {
    let mut a: Array<Int64> = [7]
    let mut b: Array<Int64> = []
    let mut i: Int64 = 0
    while i < n {
        a.push(i)
        b.push(i)
        i = i + 1
    }
    let mut s: Int64 = 0
    let mut k: Int64 = 0
    while k < a.length {
        s = s + b[k]
        k = k + 1
    }
    return s
}
",
        "    print(w(3).toString())",
        "array index 3 out of bounds",
    ),
    // One array grows twice a turn.
    (
        "fn w(n: Int64) -> Int64 {
    let mut a: Array<Int64> = []
    let mut b: Array<Int64> = []
    let mut i: Int64 = 0
    while i < n {
        a.push(i)
        a.push(i)
        b.push(i)
        i = i + 1
    }
    let mut s: Int64 = 0
    let mut k: Int64 = 0
    while k < a.length {
        s = s + b[k]
        k = k + 1
    }
    return s
}
",
        "    print(w(2).toString())",
        "array index 2 out of bounds",
    ),
    // A step no fact bounds leaves `i + d` inexact: it wraps below zero.
    (
        "export fn w(s: String, t: String, d: Int64) -> Int64 {
    let n = s.byteLength
    let nl = t.byteLength
    let mut c: Int64 = 0
    let mut i: Int64 = 1
    while i <= n - nl {
        c = c + Int64(s[i])
        i = i + d
    }
    return c
}
",
        "    print(w(\"abc\", \"x\", 9223372036854775807).toString())",
        "string index -9223372036854775808 out of bounds",
    ),
    // The body raises the exit's bound before the read: `i + k - 1` reaches `n`.
    (
        "export fn w(s: String) -> Int64 {
    let n = s.byteLength
    let mut c: Int64 = 0
    let mut k: Int64 = 1
    let mut i: Int64 = 0
    while i <= n - k {
        k = k + 1
        c = c + Int64(s[i + k - 1])
        i = i + 1
    }
    return c
}
",
        "    print(w(\"abc\").toString())",
        "string index 3 out of bounds",
    ),
    // Nothing states `nl <= n`: a needle longer than the text reads past it.
    (
        "export fn w(s: String, t: String) -> Int64 {
    let nl = t.byteLength
    let mut c: Int64 = 0
    let mut j: Int64 = 0
    while j < nl {
        c = c + Int64(s[j])
        j = j + 1
    }
    return c
}
",
        "    print(w(\"ab\", \"xyz\").toString())",
        "string index 2 out of bounds",
    ),
    // A start below zero: no clause states `from >= 0`.
    (
        "export fn w(s: String, t: String, from: Int64) -> Int64 {
    let n = s.byteLength
    let nl = t.byteLength
    let mut c: Int64 = 0
    let mut i = from
    while i <= n - nl {
        c = c + Int64(s[i])
        i = i + 1
    }
    return c
}
",
        "    print(w(\"abc\", \"x\", -1).toString())",
        "string index -1 out of bounds",
    ),
    // A start past `n`: `i + nl` wraps, so `i + nl <= n` holds at `i64` max.
    (
        "export fn w(s: String, t: String, from: Int64 where value >= 0) -> Int64 {
    let n = s.byteLength
    let nl = t.byteLength
    let mut c: Int64 = 0
    let mut i = from
    while i + nl <= n {
        c = c + Int64(s[i])
        i = i + 1
    }
    return c
}
",
        "    print(w(\"abc\", \"x\", 9223372036854775807).toString())",
        "string index 9223372036854775807 out of bounds",
    ),
    // A skip table with a negative entry steps below zero. A zero entry
    // loops forever and reads in range, so it witnesses nothing here.
    (
        "export fn w(s: String, skip: Array<Int64>) -> Int64 {
    let n = s.byteLength
    let mut c: Int64 = 0
    let mut i: Int64 = 0
    while i < n {
        let b = Int64(s[i])
        c = c + b
        i = i + skip[b - 97]
    }
    return c
}
",
        "    print(w(\"ab\", [1, -2]).toString())",
        "string index -1 out of bounds",
    ),
    // The loop writes its skip table: the entry it read last turn changes.
    (
        "fn lower(skip: modify Array<Int64>) {
    skip[0] = -5
}
export fn w(s: String, skip: modify Array<Int64>) -> Int64 {
    let n = s.byteLength
    let mut c: Int64 = 0
    let mut i: Int64 = 0
    while i < n {
        c = c + Int64(s[i])
        i = i + skip[0]
        lower(skip)
    }
    return c
}
",
        "    let mut t: Array<Int64> = [1]\n    print(w(\"abc\", t).toString())",
        "string index -4 out of bounds",
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
    // A `swapRemove` after the call shrinks the array the result indexes. Its
    // own index is a parameter the callers do not bound: `lastIndex` proves
    // `ys` is not empty.
    (
        r#"fn lastIndex(a: Array<Int64>) -> Int64 {
    if a.length == 0 {
        panic("empty")
    }
    return a.length - 1
}
fn w(xs: Array<Int64>, k: Int64) -> Int64 {
    let mut ys = xs.copy()
    let j = lastIndex(ys)
    let gone = ys.swapRemove(k)
    return ys[j]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(xs, 0).toString())
    print(w(xs, 9).toString())
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

/// Programs where `w` proves its one index from what it knows of a record's
/// fields, and the run passes the oracle. In the pair programs every value of
/// `P` the program builds keeps `a` and `b` equally long, so `b[i]` under
/// `i < a.length` is proved.
const RECORD_PROOFS: &[&str] = &[
    // A table field that every constructor fills to a literal size.
    r#"type T = { tbl: Array<Int64>, n: Int64 }
fn fill() -> Array<Int64> {
    let mut t: Array<Int64> = []
    let mut k = 0
    while k < 8 {
        t.push(k)
        k = k + 1
    }
    return t
}
fn make(n: Int64) -> T {
    return T { tbl: fill(), n: n }
}
fn w(t: T) -> Int64 {
    return t.tbl[t.n & 7]
}
fn main() -> Int64 {
    print(w(make(15)).toString())
    return 0
}
"#,
    // Both fields grow in step through a `modify` parameter.
    r#"type P = { a: Array<Int64>, b: Array<Int64> }
fn add(p: modify P, x: Int64) {
    p.a.push(x)
    p.b.push(x * 2)
}
fn w(p: P, i: Int64) -> Int64 {
    if i < 0 || i >= p.a.length {
        return 0
    }
    return p.b[i]
}
fn main() -> Int64 {
    let mut p = P { a: [], b: [] }
    add(p, 1)
    add(p, 2)
    print(w(p, 1).toString())
    return 0
}
"#,
    // A literal from two arrays a loop grew in step.
    r#"type P = { a: Array<Int64>, b: Array<Int64> }
fn make(n: Int64) -> P {
    let mut a: Array<Int64> = []
    let mut b: Array<Int64> = []
    let mut i: Int64 = 0
    while i < n {
        a.push(i)
        b.push(i)
        i = i + 1
    }
    return P { a: a, b: b }
}
fn w(p: P, i: Int64) -> Int64 {
    if i < 0 || i >= p.a.length {
        return 0
    }
    return p.b[i]
}
fn main() -> Int64 {
    print(w(make(3), 2).toString())
    return 0
}
"#,
    // A row that resizes a field no pair names keeps the pairs.
    r#"type P = { a: Array<Int64>, b: Array<Int64>, c: Array<Int64> }
fn add(p: modify P, x: Int64) {
    p.a.push(x)
    p.b.push(x)
}
fn log(p: modify P, x: Int64) {
    p.c.push(x)
    p.c.push(x)
}
fn w(p: P, i: Int64) -> Int64 {
    if i < 0 || i >= p.a.length {
        return 0
    }
    return p.b[i]
}
fn main() -> Int64 {
    let mut p = P { a: [], b: [], c: [] }
    add(p, 1)
    log(p, 2)
    print(w(p, 0).toString())
    return 0
}
"#,
    // A literal's integer field is one value across its reads.
    r#"type C = { xs: Array<Int64>, i: Int64 }
fn w(xs: Array<Int64>) -> Int64 {
    let c = C { xs: xs.copy(), i: 1 }
    if c.i >= c.xs.length {
        return 0
    }
    return c.xs[c.i]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [1, 2, 3]
    print(w(xs).toString())
    return 0
}
"#,
    // So is a `modify` parameter's, until the body stores into it.
    r#"type C = { xs: Array<Int64>, i: Int64 }
fn w(c: modify C) -> Int64 {
    if c.i < 0 || c.i >= c.xs.length {
        return 0
    }
    let v = c.xs[c.i]
    c.i = c.i + 1
    return v
}
fn main() -> Int64 {
    let mut c = C { xs: [1, 2, 3], i: 2 }
    print(w(c).toString())
    return 0
}
"#,
    // A cursor that stays between zero and `n`, and `n` at most the length.
    r#"type R = { src: Array<Int64>, n: Int64, pos: Int64 }
fn make(xs: Array<Int64>) -> R {
    let n = xs.length
    return R { src: xs.copy(), n: n, pos: 0 }
}
fn step(r: modify R) {
    if r.pos >= r.n {
        return
    }
    r.pos = r.pos + 1
}
fn w(r: R) -> Int64 {
    if r.pos >= r.n {
        return 0
    }
    return r.src[r.pos]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let mut r = make(xs)
    step(r)
    print(w(r).toString())
    return 0
}
"#,
    // A cursor at most the length reads the byte before it.
    r#"type R = { src: Array<Int64>, pos: Int64 }
fn step(r: modify R) {
    if r.pos >= r.src.length {
        return
    }
    r.pos = r.pos + 1
}
fn w(r: R) -> Int64 {
    if r.pos == 0 {
        return 0
    }
    return r.src[r.pos - 1]
}
fn main() -> Int64 {
    let mut r = R { src: [10, 20, 30], pos: 0 }
    step(r)
    step(r)
    print(w(r).toString())
    return 0
}
"#,
];

#[test]
fn every_record_proof_proves_the_index() {
    let mut failures = Vec::new();
    for root in RECORD_PROOFS {
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

/// Programs where `w` indexes a record's field under a bound that a write or a
/// value it did not build breaks: `w` keeps its index and traps there. In the
/// pair programs `w` indexes `b` under `i < a.length` and a value of a record
/// with fields `a` and `b` does not keep them equal where `w` reads it.
const RECORD_WITNESSES: &[(&str, &str)] = &[
    // A `modify` call empties a table field every constructor fills.
    (
        r#"type T = { tbl: Array<Int64>, n: Int64 }
fn fill() -> Array<Int64> {
    let mut t: Array<Int64> = []
    let mut k = 0
    while k < 8 {
        t.push(k)
        k = k + 1
    }
    return t
}
fn make(n: Int64) -> T {
    return T { tbl: fill(), n: n }
}
fn cut(t: modify T) {
    t.tbl.clear()
}
fn w(t: T) -> Int64 {
    return t.tbl[t.n & 7]
}
fn main() -> Int64 {
    let mut t = make(3)
    cut(t)
    print(w(t).toString())
    return 0
}
"#,
        "array index 3 out of bounds",
    ),
    // One path of the constructor builds a short table.
    (
        r#"type T = { tbl: Array<Int64>, n: Int64 }
fn fill() -> Array<Int64> {
    let mut t: Array<Int64> = []
    let mut k = 0
    while k < 8 {
        t.push(k)
        k = k + 1
    }
    return t
}
fn make(n: Int64) -> T {
    if n > 10 {
        return T { tbl: [1, 2, 3], n: n }
    }
    return T { tbl: fill(), n: n }
}
fn w(t: T) -> Int64 {
    return t.tbl[t.n & 7]
}
fn main() -> Int64 {
    print(w(make(15)).toString())
    return 0
}
"#,
        "array index 7 out of bounds",
    ),
    // The table's callee returns a short one on one path.
    (
        r#"type T = { tbl: Array<Int64>, n: Int64 }
fn fill(n: Int64) -> Array<Int64> {
    if n > 10 {
        return [1]
    }
    let mut t: Array<Int64> = []
    let mut k = 0
    while k < 8 {
        t.push(k)
        k = k + 1
    }
    return t
}
fn make(n: Int64) -> T {
    return T { tbl: fill(n), n: n }
}
fn w(t: T) -> Int64 {
    return t.tbl[t.n & 7]
}
fn main() -> Int64 {
    print(w(make(13)).toString())
    return 0
}
"#,
        "array index 5 out of bounds",
    ),
    // A record decoded by `fromJson` holds any table.
    (
        r#"type T = { tbl: Array<Int64>, n: Int64 }
fn fill() -> Array<Int64> {
    let mut t: Array<Int64> = []
    let mut k = 0
    while k < 8 {
        t.push(k)
        k = k + 1
    }
    return t
}
fn make(n: Int64) -> T {
    return T { tbl: fill(), n: n }
}
fn w(t: T) -> Int64 {
    return t.tbl[t.n & 7]
}
fn main() -> Int64 {
    let v = match fromJson<T>("{\"tbl\": [1], \"n\": 3}") {
        Valid(d) => w(d),
        Invalid(_) => 0,
    }
    print((w(make(1)) + v).toString())
    return 0
}
"#,
        "array index 3 out of bounds",
    ),
    // A store into the field after the guard; the body restores the pair
    // before it returns.
    (
        r#"type P = { a: Array<Int64>, b: Array<Int64> }
fn add(p: modify P, x: Int64) {
    p.a.push(x)
    p.b.push(x)
}
fn w(p: modify P, i: Int64) -> Int64 {
    if i < 0 || i >= p.a.length {
        return 0
    }
    let saved = p.b.copy()
    p.b = []
    let r = p.b[i]
    p.b = saved
    return r
}
fn main() -> Int64 {
    let mut p = P { a: [], b: [] }
    add(p, 1)
    add(p, 2)
    print(w(p, 1).toString())
    return 0
}
"#,
        "array index 1 out of bounds",
    ),
    // A call that takes the record `modify` and changes both fields: the
    // pair holds after it, the guard's bound does not.
    (
        r#"type P = { a: Array<Int64>, b: Array<Int64> }
fn empty(p: modify P) {
    p.a.clear()
    p.b.clear()
}
fn w(p: modify P, i: Int64) -> Int64 {
    if i < 0 || i >= p.a.length {
        return 0
    }
    empty(p)
    return p.b[i]
}
fn main() -> Int64 {
    let mut p = P { a: [1, 2], b: [3, 4] }
    print(w(p, 1).toString())
    return 0
}
"#,
        "array index 1 out of bounds",
    ),
    // A callee grows one field of the record it takes `modify`.
    (
        r#"type P = { a: Array<Int64>, b: Array<Int64> }
fn grow(p: modify P) {
    p.a.push(9)
}
fn w(p: P, i: Int64) -> Int64 {
    if i < 0 || i >= p.a.length {
        return 0
    }
    return p.b[i]
}
fn main() -> Int64 {
    let mut p = P { a: [1], b: [2] }
    grow(p)
    print(w(p, 1).toString())
    return 0
}
"#,
        "array index 1 out of bounds",
    ),
    // A store displaces a record whose field a row shrank; its release
    // reads it.
    (
        r#"type P = { a: Array<Int64>, b: Array<Int64> }
impl Owned for P {
    fn release(consume self) {
        print(w(self, 1).toString())
        let a = consume self.a
        drop a
        let b = consume self.b
        drop b
    }
}
fn w(p: P, i: Int64) -> Int64 {
    if i < 0 || i >= p.a.length {
        return 0
    }
    return p.b[i]
}
fn main() -> Int64 {
    let mut p = P { a: [1, 2], b: [3, 4] }
    p.b.pop()
    p = P { a: [], b: [] }
    print(p.a.length.toString())
    return 0
}
"#,
        "array index 1 out of bounds",
    ),
    // The same record released where its scope ends.
    (
        r#"type P = { a: Array<Int64>, b: Array<Int64> }
impl Owned for P {
    fn release(consume self) {
        print(w(self, 1).toString())
        let a = consume self.a
        drop a
        let b = consume self.b
        drop b
    }
}
fn w(p: P, i: Int64) -> Int64 {
    if i < 0 || i >= p.a.length {
        return 0
    }
    return p.b[i]
}
fn main() -> Int64 {
    let mut p = P { a: [1, 2], b: [3, 4] }
    p.b.pop()
    print(p.a.length.toString())
    return 0
}
"#,
        "array index 1 out of bounds",
    ),
    // A store puts a record whose field a row replaced into an array.
    (
        r#"type P = { a: Array<Int64>, b: Array<Int64> }
fn w(p: P, i: Int64) -> Int64 {
    if i < 0 || i >= p.a.length {
        return 0
    }
    return p.b[i]
}
fn main() -> Int64 {
    let mut p = P { a: [1, 2], b: [3, 4] }
    p.b = [3]
    let mut ps: Array<P> = [P { a: [], b: [] }]
    ps[0] = p
    print(w(ps[0], 1).toString())
    return 0
}
"#,
        "array index 1 out of bounds",
    ),
    // One path of the only constructor breaks the pair.
    (
        r#"type P = { a: Array<Int64>, b: Array<Int64> }
fn make(xs: Array<Int64>, odd: Bool) -> P {
    if odd {
        return P { a: xs.copy(), b: [1] }
    }
    return P { a: xs.copy(), b: xs.copy() }
}
fn w(p: P, i: Int64) -> Int64 {
    if i < 0 || i >= p.a.length {
        return 0
    }
    return p.b[i]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(make(xs, true), 2).toString())
    return 0
}
"#,
        "array index 2 out of bounds",
    ),
    // A record the decoder builds from JSON, whose arrays have any length.
    (
        r#"type P = { a: Array<Int64>, b: Array<Int64> }
fn w(p: P, i: Int64) -> Int64 {
    if i < 0 || i >= p.a.length {
        return 0
    }
    return p.b[i]
}
fn main() -> Int64 {
    let r = match fromJson<P>("{\"a\": [1, 2, 3], \"b\": [1]}") {
        Valid(p) => w(p, 2),
        Invalid(_) => 0,
    }
    print(r.toString())
    return 0
}
"#,
        "array index 2 out of bounds",
    ),
    // A callee pushes onto the array it takes `modify`, which then
    // replaces a field.
    (
        r#"type P = { a: Array<Int64>, b: Array<Int64> }
fn grow(xs: modify Array<Int64>) {
    xs.push(9)
}
fn w(p: modify P, i: Int64) -> Int64 {
    let mut a = p.a.copy()
    grow(a)
    p.a = a
    if i < 0 || i >= p.a.length {
        return 0
    }
    return p.b[i]
}
fn main() -> Int64 {
    let mut p = P { a: [1, 2], b: [3, 4] }
    print(w(p, 2).toString())
    return 0
}
"#,
        "array index 2 out of bounds",
    ),
    // Module state grows one field; the walk tracks no global's fields.
    (
        r#"type P = { a: Array<Int64>, b: Array<Int64> }
let mut g = P { a: [], b: [] }
fn put(x: Int64) {
    g.a.push(x)
}
fn w(p: P, i: Int64) -> Int64 {
    if i < 0 || i >= p.a.length {
        return 0
    }
    return p.b[i]
}
fn main() -> Int64 {
    put(1)
    put(2)
    print(w(g, 1).toString())
    return 0
}
"#,
        "array index 1 out of bounds",
    ),
    // A module-state initializer builds the record.
    (
        r#"type P = { a: Array<Int64>, b: Array<Int64> }
let g = P { a: [1, 2], b: [3] }
fn make(xs: Array<Int64>) -> P {
    return P { a: xs.copy(), b: xs.copy() }
}
fn w(p: P, i: Int64) -> Int64 {
    if i < 0 || i >= p.a.length {
        return 0
    }
    return p.b[i]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [1, 2]
    print((w(make(xs), 1) + w(g, 1)).toString())
    return 0
}
"#,
        "array index 1 out of bounds",
    ),
    // A wider record type with the same two fields passes for `P`.
    (
        r#"type P = { a: Array<Int64>, b: Array<Int64> }
type Wide = { a: Array<Int64>, b: Array<Int64>, c: Int64 }
fn w(p: P, i: Int64) -> Int64 {
    if i < 0 || i >= p.a.length {
        return 0
    }
    return p.b[i]
}
fn main() -> Int64 {
    let p = P { a: [1], b: [2] }
    let wide = Wide { a: [1, 2, 3], b: [1], c: 0 }
    print((w(p, 0) + w(wide, 2)).toString())
    return 0
}
"#,
        "array index 2 out of bounds",
    ),
    // A store into an integer field after the guard.
    (
        r#"type C = { xs: Array<Int64>, i: Int64 }
fn w(c: modify C) -> Int64 {
    if c.i < 0 || c.i >= c.xs.length {
        return 0
    }
    c.i = c.i + 1
    return c.xs[c.i]
}
fn main() -> Int64 {
    let mut c = C { xs: [1, 2, 3], i: 2 }
    print(w(c).toString())
    return 0
}
"#,
        "array index 3 out of bounds",
    ),
    // The same store into a literal's field.
    (
        r#"type C = { xs: Array<Int64>, i: Int64 }
fn w(xs: Array<Int64>) -> Int64 {
    let mut c = C { xs: xs.copy(), i: 0 }
    if c.i >= c.xs.length {
        return 0
    }
    c.i = 5
    return c.xs[c.i]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [1, 2, 3]
    print(w(xs).toString())
    return 0
}
"#,
        "array index 5 out of bounds",
    ),
    // A callee writes the field of the record it takes `modify`.
    (
        r#"type C = { xs: Array<Int64>, i: Int64 }
fn bump(c: modify C) {
    c.i = c.i + 10
}
fn w(c: modify C) -> Int64 {
    if c.i < 0 || c.i >= c.xs.length {
        return 0
    }
    bump(c)
    return c.xs[c.i]
}
fn main() -> Int64 {
    let mut c = C { xs: [1, 2, 3], i: 0 }
    print(w(c).toString())
    return 0
}
"#,
        "array index 10 out of bounds",
    ), // A store in the constructor moves the cursor past the end.
    (
        r#"type R = { src: Array<Int64>, pos: Int64 }
fn make(xs: Array<Int64>) -> R {
    let mut r = R { src: xs.copy(), pos: 0 }
    r.pos = 7
    return r
}
fn w(r: R) -> Int64 {
    if r.pos == 0 {
        return 0
    }
    return r.src[r.pos - 1]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(make(xs)).toString())
    return 0
}
"#,
        "array index 6 out of bounds",
    ),
    // One path of the only constructor starts the cursor below zero.
    (
        r#"type R = { src: Array<Int64>, pos: Int64 }
fn make(xs: Array<Int64>, odd: Bool) -> R {
    if odd {
        return R { src: xs.copy(), pos: -1 }
    }
    return R { src: xs.copy(), pos: 0 }
}
fn w(r: R) -> Int64 {
    if r.pos >= r.src.length {
        return 0
    }
    return r.src[r.pos]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    print(w(make(xs, true)).toString())
    return 0
}
"#,
        "array index -1 out of bounds",
    ),
    // A `modify` call shortens `src` below `n`.
    (
        r#"type R = { src: Array<Int64>, n: Int64, pos: Int64 }
fn make(xs: Array<Int64>) -> R {
    let n = xs.length
    return R { src: xs.copy(), n: n, pos: 0 }
}
fn step(r: modify R) {
    if r.pos >= r.n {
        return
    }
    r.pos = r.pos + 1
}
fn chop(r: modify R) {
    let _ = r.src.pop()
}
fn w(r: R) -> Int64 {
    if r.pos >= r.n {
        return 0
    }
    return r.src[r.pos]
}
fn main() -> Int64 {
    let xs: Array<Int64> = [10, 20, 30]
    let mut r = make(xs)
    step(r)
    step(r)
    chop(r)
    print(w(r).toString())
    return 0
}
"#,
        "array index 2 out of bounds",
    ),
    // A record decoded by `fromJson` holds any cursor.
    (
        r#"type R = { src: Array<Int64>, pos: Int64 }
fn w(r: R) -> Int64 {
    if r.pos >= r.src.length {
        return 0
    }
    return r.src[r.pos]
}
fn main() -> Int64 {
    let r = R { src: [1, 2], pos: 0 }
    let v = match fromJson<R>("{\"src\": [1, 2, 3], \"pos\": -1}") {
        Valid(d) => w(d),
        Invalid(_) => 0,
    }
    print((w(r) + v).toString())
    return 0
}
"#,
        "array index -1 out of bounds",
    ),
    // Module state moves its cursor below zero; the walk tracks no global's fields.
    (
        r#"type R = { src: Array<Int64>, pos: Int64 }
let mut g = R { src: [1, 2, 3], pos: 0 }
fn back() {
    g.pos = g.pos - 1
}
fn w(r: R) -> Int64 {
    if r.pos >= r.src.length {
        return 0
    }
    return r.src[r.pos]
}
fn main() -> Int64 {
    let r = R { src: [1, 2], pos: 0 }
    back()
    print((w(r) + w(g)).toString())
    return 0
}
"#,
        "array index -1 out of bounds",
    ),
    // A callee moves the cursor of the record it takes `modify` below zero.
    (
        r#"type R = { src: Array<Int64>, pos: Int64 }
fn back(r: modify R) {
    r.pos = r.pos - 1
}
fn w(r: R) -> Int64 {
    if r.pos >= r.src.length {
        return 0
    }
    return r.src[r.pos]
}
fn main() -> Int64 {
    let mut r = R { src: [1, 2, 3], pos: 0 }
    back(r)
    print(w(r).toString())
    return 0
}
"#,
        "array index -1 out of bounds",
    ),
];

#[test]
fn every_record_witness_keeps_its_index_and_traps_there() {
    let mut failures = Vec::new();
    for (root, trap) in RECORD_WITNESSES {
        let (dir, file) = modules(root, &[]);
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
