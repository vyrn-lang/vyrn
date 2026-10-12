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

/// A function whose every `Some` return wraps an index below a parameter's
/// length states that of the payload, and the caller's `if let` binder takes
/// it. A path that wraps the length itself, or a `None` name a later store
/// fills with it, keeps the caller's check.
#[test]
fn a_some_payload_proves_its_callers_index() {
    let find = "fn find(xs: Array<Int64>, v: Int64) -> Option<Int64> {
    let mut k = 0
    while k < xs.length {
        if xs[k] == v {
            return Some(k)
        }
        k = k + 1
    }
    return None
}
";
    let w = "fn w(xs: Array<Int64>, v: Int64) -> Int64 {
    if let Some(i) = find(xs, v) {
        return xs[i]
    }
    return 0
}
";
    let proof = format!("{find}{w}");
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    let (err, _) = oracle(&proof, "    print(w(xs, 20).toString())", "w");
    assert_eq!(err, "");
    let witnesses = [
        find.replace(
            "    let mut k = 0\n",
            "    if v < 0 {\n        return Some(xs.length)\n    }\n    let mut k = 0\n",
        ),
        "fn find(xs: Array<Int64>, v: Int64) -> Option<Int64> {
    let mut r: Option<Int64> = None
    if v < 0 {
        r = Some(xs.length)
    }
    return r
}
"
        .to_string(),
    ];
    let mut failures = Vec::new();
    for f in &witnesses {
        let src = format!("{f}{w}");
        let rows = verdicts(&src, "w");
        if rows != ["check array-index"] {
            failures.push(format!("{src}\nrows: {rows:?}"));
        }
        let (err, _) = oracle(&src, "    print(w(xs, 0 - 1).toString())", "w");
        if !err.contains("array index 3 out of bounds")
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

/// A function that hands back the array it takes by `consume`, exactly as
/// long, gives its result the argument's length in the caller. A callee that
/// pops on one path, pushes on one path, or returns its other parameter
/// keeps the caller's check.
#[test]
fn a_result_as_long_as_its_consume_argument_proves_its_callers_index() {
    let keep = |body: &str| {
        format!(
            "fn keep(p: consume Array<Int64>, q: consume Array<Int64>, k: Int64) -> Array<Int64> {{
{body}
}}
fn w(x: Int64) -> Int64 {{
    let u = [1, 2, 3, 4, 5]
    let t = keep([1, 2, 3, 4, 5], [1, 2, 3, 4], x)
    return u[t.length - 1] + t[4]
}}
"
        )
    };
    let path = |row: &str| {
        format!(
            "    let mut a = p
    if k > 3 {{
        {row}
    }}
    return a"
        )
    };
    let proof = keep(&path("a[0] = k"));
    assert_eq!(
        verdicts(&proof, "w"),
        ["proved array-index", "proved array-index"],
        "{proof}"
    );
    let (err, _) = oracle(&proof, "    print(w(5).toString())", "w");
    assert_eq!(err, "");
    let witnesses = [
        (path("a.pop()"), "array index 4 out of bounds"),
        (path("a.push(0)"), "array index 5 out of bounds"),
        ("    return q".to_string(), "array index 4 out of bounds"),
    ];
    let mut failures = Vec::new();
    for (body, trap) in witnesses {
        let src = keep(&body);
        let rows = verdicts(&src, "w");
        if !rows.contains(&"check array-index".to_string()) {
            failures.push(format!(
                "{src}
rows: {rows:?}"
            ));
        }
        let (err, _) = oracle(&src, "    print(w(5).toString())", "w");
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
    return findSkipping(s, \"ab\", 0, skipTable(\"ab\", 1000)) ?? 0 - 1
}
fn short(s: String) -> Int64 {
    let t: Array<UInt32> = [UInt32(1)]
    return findSkipping(s, \"ab\", 0, t) ?? 0 - 1
}
fn below(s: String, from: Int64) -> Int64 {
    return findSkipping(s, \"ab\", from, skipTable(\"ab\", 1000)) ?? 0 - 1
}
fn shortened(s: String) -> Int64 {
    let mut t = skipTable(\"ab\", 1000)
    shorten(t)
    return findSkipping(s, \"ab\", 0, t) ?? 0 - 1
}
";
    let print = |call: &str| format!("    print({call}.toString())");
    let (err, rows) = oracle(src, &print("found(\"xxaxab\")"), "findSkipping");
    let verdicts: Vec<&str> = rows.iter().map(|r| r.split(' ').nth(3).unwrap()).collect();
    assert_eq!((err.as_str(), verdicts), ("", vec!["proved"; 5]));
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

/// A Bool result states the facts every `true` return proves, over the
/// parameters and their records' fields; the caller assumes them where the
/// call is true. A `true` return that does not prove the index, a use where
/// the call is false, and a write or a `modify` call between the call and
/// the use each keep the check, which traps.
#[test]
fn a_true_result_proves_its_callers_index() {
    let live = "type H = { slot: Int64 }
type S = { gens: Array<Int64> }
fn live(s: S, h: H) -> Bool {
    if h.slot < 0 || h.slot >= s.gens.length {
        return false
    }
    return s.gens[h.slot] != 0
}
fn shrink(s: modify S) {
    s.gens.clear()
}
";
    let proof = format!(
        "{live}fn w(s: modify S, h: H) -> Int64 {{
    if live(s, h) {{
        return s.gens[h.slot]
    }}
    return 0
}}
"
    );
    let calls = |slot: i64| {
        format!(
            "    let mut s = S {{ gens: xs }}
    print(w(s, H {{ slot: {slot} }}).toString())"
        )
    };
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    let (err, _) = oracle(&proof, &calls(2), "w");
    assert_eq!(err, "");
    let used = |between: &str, test: &str| {
        format!(
            "{live}fn w(s: modify S, h: H) -> Int64 {{
    let ok = live(s, h)
{between}    if {test} {{
        return s.gens[h.slot]
    }}
    return 0
}}
"
        )
    };
    let witnesses = [
        (
            format!(
                "type H = {{ slot: Int64 }}
type S = {{ gens: Array<Int64> }}
fn live(s: S, h: H) -> Bool {{
    if h.slot == 7 {{
        return true
    }}
    return h.slot >= 0 && h.slot < s.gens.length
}}
fn w(s: modify S, h: H) -> Int64 {{
    if live(s, h) {{
        return s.gens[h.slot]
    }}
    return 0
}}
"
            ),
            7,
        ),
        (used("", "!ok"), 5),
        (
            used(
                "    s.gens.pop()
",
                "ok",
            ),
            2,
        ),
        (
            used(
                "    shrink(s)
",
                "ok",
            ),
            0,
        ),
    ];
    let mut failures = Vec::new();
    for (src, slot) in &witnesses {
        let rows = verdicts(src, "w");
        if rows != ["check array-index"] {
            failures.push(format!(
                "{src}
rows: {rows:?}"
            ));
        }
        let (err, _) = oracle(src, &calls(*slot), "w");
        if !err.contains(&format!("array index {slot} out of bounds"))
            || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED)
        {
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

/// A check on a global's array field reads the facts of a borrow of the
/// global until a row may write it. A call that writes the global, a store
/// to it, a call through a function value, a generic instance that writes it,
/// and a loop that writes it after the read each keep the check, which traps.
#[test]
fn a_borrow_of_a_global_proves_its_index_until_a_write() {
    let lib = "type Box = { xs: Array<Int64> }
let mut g: Box = Box { xs: [1, 2, 3] }
fn inRange(b: Box, i: Int64) -> Bool {
    return i >= 0 && i < b.xs.length
}
fn shrink(b: modify Box) {
    b.xs.clear()
}
fn clearG() {
    shrink(g)
}
fn clearT<T>(x: T) {
    shrink(g)
}
";
    let w = |param: &str, body: &str| {
        format!(
            "{lib}fn w(i: Int64{param}) -> Int64 {{
    let mut s = 0
    if inRange(g, i) {{
{body}
    }}
    return s
}}
"
        )
    };
    let read = |between: &str| {
        format!(
            "        {between}
        s = g.xs[i]"
        )
    };
    let proof = w("", "        s = g.xs[i]");
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    let (err, _) = oracle(&proof, "    print(w(2).toString())", "w");
    assert_eq!(err, "");
    let looped = "        let mut k = 0
        while k < 2 {
            s = s + g.xs[i]
            clearG()
            k = k + 1
        }";
    let witnesses = [
        (w("", &read("clearG()")), "w(2)"),
        (w("", &read("g = Box { xs: [] }")), "w(2)"),
        (w(", f: fn()", &read("f()")), "w(2, clearG)"),
        (w("", &read("clearT(i)")), "w(2)"),
        (w("", looped), "w(2)"),
    ];
    let mut failures = Vec::new();
    for (src, call) in &witnesses {
        let rows = verdicts(src, "w");
        if rows != ["check array-index"] {
            failures.push(format!("{src}rows: {rows:?}"));
        }
        let (err, _) = oracle(src, &format!("    print({call}.toString())"), "w");
        if !err.contains("array index 2 out of bounds")
            || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED)
        {
            failures.push(format!("{src}ran: {err}"));
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

/// A loop header on a global is not hoisted when a call in the loop writes the
/// global, so the index check stays. `emit-lowered` prints the emitter's body;
/// a body built again without the effect judgment hoists the header and
/// proves the check.
#[test]
fn a_header_a_called_function_writes_keeps_its_check() {
    let src = "type Box = { xs: Array<Int64> }
let mut g: Box = Box { xs: [1, 2, 3] }
fn shrink(b: modify Box) {
    b.xs.clear()
}
fn clearG() {
    shrink(g)
}
fn w() -> Int64 {
    let mut s = 0
    let mut k = 0
    while k < g.xs.length {
        clearG()
        s = s + g.xs[k]
        k = k + 1
    }
    return s
}
";
    assert_eq!(verdicts(src, "w"), ["check array-index"]);
}

/// A returned `a && b` states what both operands' truth states, through a
/// name `b` binds: the byte read is gone at the join, and its bounds carry
/// the fact. Where `b` adds the byte instead, the true return proves only
/// `a`'s half, and the check stays and traps.
#[test]
fn an_and_result_keeps_its_second_operands_facts() {
    let src = |test: &str| {
        format!(
            "fn fits(xs: Array<Int64>, i: Int64, pad: Array<UInt8>) -> Bool {{
    if pad.length < 1 {{
        return false
    }}
    return i >= 0 && {test}
}}
export fn w(xs: Array<Int64>, i: Int64, pad: Array<UInt8>) -> Int64 {{
    if fits(xs, i, pad) {{
        return xs[i]
    }}
    return 0
}}
"
        )
    };
    let calls = |i: i64, pad: i64| {
        format!(
            "    let mut pad: Array<UInt8> = []
    pad.push({pad})
    print(w(xs, {i}, pad).toString())"
        )
    };
    let proof = src("i < xs.length - Int64(pad[0])");
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    assert_eq!(oracle(&proof, &calls(1, 1), "w").0, "");
    let witness = src("i < xs.length + Int64(pad[0])");
    assert_eq!(verdicts(&witness, "w"), ["check array-index"]);
    let (err, _) = oracle(&witness, &calls(4, 5), "w");
    assert!(err.contains("array index 4 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// An integer field a check divides by is at least one in every record value
/// when every literal proves it. A literal that stores 0 on one path drops the
/// fact, and the division stays a check and traps.
#[test]
fn a_record_field_divisor_at_least_one_proves_the_division() {
    let src = |zero: &str| {
        format!(
            "type R = {{ n: Int64 }}
fn make(b: UInt8) -> R {{
{zero}    return R {{ n: Int64(b) + 1 }}
}}
export fn w(r: R, x: Int64) -> Int64 {{
    return x / r.n
}}
"
        )
    };
    let calls = |b: i64| format!("    print(w(make(UInt8({b})), 10).toString())");
    let proof = src("");
    let proved = ["proved int-div-zero", "proved int-div-overflow"];
    assert_eq!(verdicts(&proof, "w"), proved);
    assert_eq!(oracle(&proof, &calls(3), "w").0, "");
    let witness = src("    if b == UInt8(7) {
        return R { n: 0 }
    }
");
    let kept = ["check int-div-zero", "check int-div-overflow"];
    assert_eq!(verdicts(&witness, "w"), kept);
    let (err, _) = oracle(&witness, &calls(7), "w");
    assert!(err.contains("division by zero"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// A counter that steps down to one while it is not one keeps `r - 1 >= 0`,
/// which its index `r - 1` needs. A counter that starts at zero drops the
/// candidate at the loop's entry, and the read stays a check and traps.
#[test]
fn a_counter_down_to_one_proves_its_index_below_it() {
    let src = |start: &str| {
        format!(
            "export fn w(xs: Array<Int64>) -> Int64 {{
    if xs.length < 1 {{
        return 0
    }}
    let mut s = 0
    let mut r = {start}
    while r != 1 {{
        s = s + xs[r - 1]
        r = r - 1
    }}
    return s
}}
"
        )
    };
    let calls = "    print(w(xs).toString())";
    let proof = src("xs.length");
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    assert_eq!(oracle(&proof, calls, "w").0, "");
    let witness = src("0");
    assert_eq!(verdicts(&witness, "w"), ["check array-index"]);
    let (err, _) = oracle(&witness, calls, "w");
    assert!(err.contains("array index -1 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// A private function of an injected runtime module (`std/text`, spelled
/// `text$`) starts with what every caller proves of its arguments, as a root
/// module's does. With the caller's bound weakened by one, the read stays a
/// check and traps.
#[test]
fn a_runtime_modules_private_function_takes_its_callers_facts() {
    let run = |high: &str, i: i64| {
        let dir = scratch("elide-rt-entry");
        let std_dir = dir.join("std");
        std::fs::create_dir_all(&std_dir).unwrap();
        let repo_std = std::path::Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
        for e in std::fs::read_dir(repo_std).unwrap() {
            let e = e.unwrap().path();
            std::fs::copy(&e, std_dir.join(e.file_name().unwrap())).unwrap();
        }
        let text = std_dir.join("text.vyrn");
        let mut src = std::fs::read_to_string(&text).unwrap();
        src.push_str(&format!(
            "
fn pickW(xs: Array<Int64>, i: Int64) -> Int64 {{
    return xs[i]
}}

export fn pickAt(xs: Array<Int64>, i: Int64) -> Int64 {{
    if i < 0 || {high} {{
        return 0
    }}
    return pickW(xs, i)
}}
"
        ));
        std::fs::write(&text, src).unwrap();
        let file = dir.join("p.vyrn");
        std::fs::write(
            &file,
            format!(
                "import {{ pickAt }} from \"std/text\"
fn main() -> Int64 {{
    let xs: Array<Int64> = [10, 20, 30]
    print(pickAt(xs, {i}).toString())
    return 0
}}
"
            ),
        )
        .unwrap();
        let log = dir.join("counts.tsv");
        let out = vyrn()
            .arg("run")
            .arg(&file)
            .env("VYRN_STD", &std_dir)
            .env("VYRN_CHECKS", &log)
            .output()
            .unwrap();
        let rows: Vec<String> = std::fs::read_to_string(&log)
            .unwrap_or_default()
            .lines()
            .filter_map(|l| {
                let f: Vec<&str> = l.split('\t').collect();
                (f[1] == "text$pickW").then(|| f[4..6].join(" "))
            })
            .collect();
        (norm(&out.stderr), rows)
    };
    let (err, rows) = run("i >= xs.length", 2);
    assert_eq!(err, "");
    assert_eq!(rows, ["array-index proved"]);
    let (err, rows) = run("i > xs.length", 3);
    assert_eq!(rows, ["array-index kept"]);
    assert!(err.contains("array index 3 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// A `bytes(s, a, b)` range is proved where the path bounds `0 <= a <= b <=
/// s.byteLength`. With the upper bound weakened by one, the range stays a
/// check and traps.
#[test]
fn a_bounded_range_proves_its_bytes_slice() {
    let src = |high: &str| {
        format!(
            "fn w(s: String, a: Int64, b: Int64) -> Int64 {{
    if a < 0 || {high} || a > b {{
        return 0
    }}
    return bytes(s, a, b).length
}}
"
        )
    };
    let proof = src("b > s.byteLength");
    assert_eq!(verdicts(&proof, "w"), ["proved string-index"]);
    assert_eq!(
        oracle(&proof, "    print(w(\"ABCD\", 1, 4).toString())", "w").0,
        ""
    );
    let witness = src("b > s.byteLength + 1");
    assert_eq!(verdicts(&witness, "w"), ["check string-index"]);
    let (err, _) = oracle(&witness, "    print(w(\"ABCD\", 1, 5).toString())", "w");
    assert!(err.contains("string index 5 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// A check's goal takes a chain of three premises: `m < r`, `r < n` and
/// `n <= xs.length` bound `xs[m]`. With one link weakened to `r <= n + 1`
/// the chain does not hold, and the read stays a check and traps.
#[test]
fn a_chain_of_three_premises_proves_an_index() {
    let src = |link: &str| {
        format!(
            "export fn w(xs: Array<Int64>, r: Int64, n: Int64) -> Int64 {{
    if n > xs.length || {link} || r < 0 {{
        return 0
    }}
    let mut s = 0
    let mut m = 0
    while m < r {{
        s = s + xs[m]
        m = m + 1
    }}
    return s
}}
"
        )
    };
    let calls = |r: i64, n: i64| format!("    print(w(xs, {r}, {n}).toString())");
    let proof = src("r >= n");
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    assert_eq!(oracle(&proof, &calls(2, 3), "w").0, "");
    let witness = src("r > n + 1");
    assert_eq!(verdicts(&witness, "w"), ["check array-index"]);
    let (err, _) = oracle(&witness, &calls(4, 3), "w");
    assert!(err.contains("array index 3 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// A Bool a branch stores takes that branch's facts: the `&&` chain's last
/// operand bounds nothing, and `i + 1 < s.byteLength` still reaches the body.
#[test]
fn a_bool_a_branch_stores_keeps_the_branch_facts() {
    let src = "fn w(s: String, i: Int64, c: Bool) -> Int64 {
    if i >= 0 && i < s.byteLength && i + 1 < s.byteLength && c {
        return Int64(s[i + 1])
    }
    return 0
}
";
    assert_eq!(verdicts(src, "w"), ["proved string-index"]);
    let calls = "    print(w(\"a/b\", 1, true).toString())";
    assert_eq!(oracle(src, calls, "w").0, "");
}

/// The core binds `@borrow` once per read in a loop. A fact of the borrow of
/// `s` proves `s[i]` and does not reach the borrow of `t`.
#[test]
fn a_borrow_fact_reaches_no_other_borrow() {
    let src = "fn w(s: String, t: String) -> Int64 {
    let mut n: Int64 = 0
    let mut i: Int64 = 0
    while i < s.byteLength {
        if s[i] == 47 && i + 1 < s.byteLength && t[i + 1] == 42 {
            n = n + 1
        }
        i = i + 1
    }
    return n
}
";
    assert_eq!(
        verdicts(src, "w"),
        ["proved string-index", "check string-index"]
    );
    let (err, _) = oracle(src, "    print(w(\"/x\", \"a\").toString())", "w");
    assert!(
        err.contains("string index 1 out of bounds")
            && !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
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
    // A Bool a branch stores: `<=` leaves the index one past the String.
    (
        "fn w(s: String, i: Int64, c: Bool) -> Int64 {
    if i >= 0 && i < s.byteLength && i + 1 <= s.byteLength && c {
        return Int64(s[i + 1])
    }
    return 0
}
",
        "    print(w(\"a/\", 1, true).toString())",
        "string index 2 out of bounds",
    ),
    // ... and `||` joins a branch where the bound fails.
    (
        "fn w(s: String, i: Int64, c: Bool) -> Int64 {
    if i >= 0 && i < s.byteLength && (i + 1 < s.byteLength || i == 1) && c {
        return Int64(s[i + 1])
    }
    return 0
}
",
        "    print(w(\"a/\", 1, true).toString())",
        "string index 2 out of bounds",
    ),
    // A fact of one binding reaches no other binding of its spelling.
    (
        "fn w(xs: Array<Int64>, ys: Array<Int64>, i: Int64) -> Int64 {
    let a = xs
    if i >= 0 && i < a.length {
        let a = ys
        return a[i]
    }
    return 0
}
",
        "    let mut ys: Array<Int64> = []\n    ys.push(1)\n    print(w(xs, ys, 2).toString())",
        "array index 2 out of bounds",
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

/// The record type, the one writer that keeps its element fact, and the
/// reader the fact proves: `t.data[t.idx[k]]`.
const ELEMENT_HEAD: &str = r#"type T = { idx: Array<Int64>, data: Array<Int64> }
fn add(t: modify T, x: Int64) {
    t.data.push(x)
    t.idx.push(t.data.length - 1)
}
fn w(t: T, k: Int64) -> Int64 {
    return t.data[t.idx[k]]
}
"#;

/// A `main` that builds the record only through `add`.
const GOOD_MAIN: &str = r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    add(t, 6)
    print(w(t, 1).toString())
    return 0
}
"#;

/// A `main` that copies the record field by field.
const COPY_MAIN: &str = r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    add(t, 6)
    let v = T { idx: t.idx.copy(), data: t.data.copy() }
    print((w(t, 1) + w(v, 1)).toString())
    return 0
}
"#;

/// Programs whose `main` breaks the element fact of [`ELEMENT_HEAD`], each
/// with the trap its `w` raises.
const ELEMENT_WITNESSES: &[(&str, &str)] = &[
    // A store into an element that no fact bounds.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    t.idx[0] = 99
    print(w(t, 0).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // A push of an element that no fact bounds.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    t.idx.push(99)
    print(w(t, 1).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // The bounding field shrinks.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    add(t, 6)
    t.data.pop()
    print(w(t, 1).toString())
    return 0
}
"#,
        "array index 1 out of bounds",
    ),
    // A record decoded by `fromJson` holds any element.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    let v = match fromJson<T>("{\"idx\": [99], \"data\": [1]}") {
        Valid(d) => w(d, 0),
        Invalid(_) => 0,
    }
    print((w(t, 0) + v).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // A callee stores a bad element into the record it takes `modify`.
    (
        r#"fn bad(t: modify T) {
    t.idx[0] = 99
}
fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    bad(t)
    print(w(t, 0).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // A callee pushes a bad element onto the record it takes `modify`.
    (
        r#"fn bad(t: modify T) {
    t.idx.push(99)
}
fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    bad(t)
    print(w(t, 1).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // A generic function pushes onto a copy of the field, which then replaces it.
    (
        r#"fn put<E>(xs: modify Array<E>, v: consume E) {
    xs.push(v)
}
fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    let mut a = t.idx.copy()
    put(a, 99)
    t.idx = a
    print(w(t, 1).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // A lambda pushes a bad element onto a record it builds.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    let f: fn(Int64) -> Int64 = (k) -> {
        let mut u = T { idx: [], data: [1] }
        u.idx.push(k)
        return w(u, 0)
    }
    print((w(t, 0) + f(99)).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // The field is replaced whole.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    t.idx = [99]
    print(w(t, 0).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // An `append` adds elements no walk checks.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    t.idx.append([99])
    print(w(t, 1).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // A record literal holds a bad element.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    let u = T { idx: [99], data: [1] }
    print((w(t, 0) + w(u, 0)).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // A literal holds an array its `let` made empty and a row then grew.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    let mut a: Array<Int64> = []
    a.push(99)
    let u = T { idx: a, data: [1] }
    print((w(t, 0) + w(u, 0)).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // A `copyFrom` replaces the elements.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    t.idx.copyFrom([99])
    print(w(t, 0).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // Module state takes a bad element.
    (
        r#"let mut g = T { idx: [], data: [] }
fn bump() {
    g.idx.push(99)
}
fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    bump()
    print((w(t, 0) + w(g, 0)).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // A record in an array takes a bad element.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    let mut ts: Array<T> = [t.copy()]
    ts[0].idx[0] = 99
    print((w(t, 0) + w(ts[0], 0)).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // The field takes another record's elements.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    let mut u = T { idx: [], data: [] }
    add(u, 1)
    add(u, 2)
    add(u, 3)
    t.idx = u.idx.copy()
    print(w(t, 2).toString())
    return 0
}
"#,
        "array index 2 out of bounds",
    ),
    // A pushed element is below another record's bounding field.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    let mut u = T { idx: [], data: [] }
    add(u, 1)
    add(u, 2)
    t.idx.push(u.data.length - 1)
    print(w(t, 1).toString())
    return 0
}
"#,
        "array index 1 out of bounds",
    ),
    // A store through a user projection.
    (
        r#"impl Index for T {
    fn tryAt(read self, i: Int64) -> read Option<Int64> {
        if i < 0 || i >= self.idx.length {
            return None
        }
        return Some(self.idx[i])
    }
    fn at(read self, i: Int64) -> read Int64 {
        return self.idx[i]
    }
    fn atSet(modify self, i: Int64) -> modify Int64 {
        return self.idx[i]
    }
}
fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    t[0] = 99
    print(w(t, 0).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // A literal copies the elements from one record and the bound from another.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    add(t, 6)
    let mut u = T { idx: [], data: [] }
    add(u, 7)
    let v = T { idx: t.idx.copy(), data: u.data.copy() }
    print((w(t, 1) + w(v, 1)).toString())
    return 0
}
"#,
        "array index 1 out of bounds",
    ),
    // A literal copies the bound before the record grows.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    let d = t.data.copy()
    add(t, 6)
    let v = T { idx: t.idx.copy(), data: d }
    print((w(t, 1) + w(v, 1)).toString())
    return 0
}
"#,
        "array index 1 out of bounds",
    ),
    // A taken field is stored into the record that replaced its own.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    add(t, 6)
    let a = consume t.idx
    t = T { idx: [], data: [] }
    t.idx = a
    print(w(t, 1).toString())
    return 0
}
"#,
        "array index 1 out of bounds",
    ),
    // A copy of the field is reassigned before a literal holds it.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    let mut a = t.idx.copy()
    a = [99]
    let v = T { idx: a, data: t.data.copy() }
    print((w(t, 0) + w(v, 0)).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // A copy of the field is written by a callee before a literal holds it.
    (
        r#"fn put(xs: modify Array<Int64>, v: Int64) {
    xs.push(v)
}
fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    let mut a = t.idx.copy()
    put(a, 99)
    let v = T { idx: a, data: t.data.copy() }
    print((w(t, 0) + w(v, 1)).toString())
    return 0
}
"#,
        "array index 99 out of bounds",
    ),
    // The record is replaced after its field was copied.
    (
        r#"fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    add(t, 6)
    add(t, 7)
    let a = t.idx.copy()
    t = T { idx: [], data: [] }
    add(t, 8)
    let v = T { idx: a, data: t.data.copy() }
    print((w(t, 0) + w(v, 2)).toString())
    return 0
}
"#,
        "array index 2 out of bounds",
    ),
    // A callee replaces the record after its field was copied.
    (
        r#"fn renew(t: modify T) {
    t = T { idx: [], data: [] }
    add(t, 8)
}
fn main() -> Int64 {
    let mut t = T { idx: [], data: [] }
    add(t, 5)
    add(t, 6)
    add(t, 7)
    let a = t.idx.copy()
    renew(t)
    let v = T { idx: a, data: t.data.copy() }
    print((w(t, 0) + w(v, 2)).toString())
    return 0
}
"#,
        "array index 2 out of bounds",
    ),
];

#[test]
fn an_element_fact_proves_the_index_it_bounds() {
    for main in [GOOD_MAIN, COPY_MAIN] {
        let (_dir, file) = modules(&format!("{ELEMENT_HEAD}{main}"), &[]);
        assert_eq!(
            rows_of(&file, "w"),
            ["check array-index", "proved array-index"]
        );
    }
}

#[test]
fn every_element_witness_keeps_its_index_and_traps_there() {
    witnesses(ELEMENT_HEAD, ELEMENT_WITNESSES, &["check", "check"]);
}

/// Runs each `(main, trap)` of `mains` after `head`, and fails unless `w`'s
/// check rows have the verdicts `rows` and the run traps with `trap`, never
/// at a proved row.
fn witnesses(head: &str, mains: &[(&str, &str)], rows: &[&str]) {
    let mut failures = Vec::new();
    for (main, trap) in mains {
        let root = format!("{head}{main}");
        let (dir, file) = modules(&root, &[]);
        let got = rows_of(&file, "w");
        let verdicts: Vec<&str> = got.iter().map(|v| v.split(' ').next().unwrap()).collect();
        if verdicts != rows {
            failures.push(format!("{root}\nrows: {got:?}"));
        }
        let (err, _) = run_oracle(&dir, &file, "w");
        if !err.contains(trap) || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED) {
            failures.push(format!("{root}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// [`ELEMENT_HEAD`] with a reader that loops over every element. The core
/// reads `t.idx` and `t.data` through borrows it binds above the loop.
const LOOP_HEAD: &str = r#"type T = { idx: Array<Int64>, data: Array<Int64> }
fn add(t: modify T, x: Int64) {
    t.data.push(x)
    t.idx.push(t.data.length - 1)
}
fn w(t: T, k: Int64) -> Int64 {
    let mut s = k
    let mut j = 0
    while j < t.idx.length {
        s = s + t.data[t.idx[j]]
        j = j + 1
    }
    return s
}
"#;

#[test]
fn a_loop_reads_a_fields_element_facts_through_its_borrow() {
    for main in [GOOD_MAIN, COPY_MAIN] {
        let (_dir, file) = modules(&format!("{LOOP_HEAD}{main}"), &[]);
        assert_eq!(
            rows_of(&file, "w"),
            ["proved array-index", "proved array-index"]
        );
    }
}

/// The loop meets a bad element before the one each witness's `w` call
/// names, so the trap's index differs.
#[test]
fn every_element_witness_keeps_its_index_in_a_loop() {
    let mains: Vec<(&str, &str)> = (ELEMENT_WITNESSES.iter())
        .map(|(main, _)| (*main, "out of bounds"))
        .collect();
    witnesses(LOOP_HEAD, &mains, &["proved", "check"]);
}

/// A graph in compressed rows: the targets of vertex `v` are
/// `tgt[off[v]..off[v + 1]]`, so an offset may equal `tgt.length`.
const ROWS_HEAD: &str = r#"type G = { off: Array<Int64>, tgt: Array<Int64> }
fn vertex(g: modify G, a: Int64, b: Int64) {
    g.tgt.push(a)
    g.tgt.push(b)
    g.off.push(g.tgt.length)
}
fn w(g: G, v: Int64) -> Int64 {
    let mut s = 0
    let mut e = g.off[v]
    while e < g.off[v + 1] {
        s = s + g.tgt[e]
        e = e + 1
    }
    return s
}
"#;

#[test]
fn an_element_may_end_a_range_of_another_field() {
    let main = r#"fn main() -> Int64 {
    let mut g = G { off: [0], tgt: [] }
    vertex(g, 1, 2)
    vertex(g, 3, 4)
    print(w(g, 1).toString())
    return 0
}
"#;
    let (_dir, file) = modules(&format!("{ROWS_HEAD}{main}"), &[]);
    assert_eq!(
        rows_of(&file, "w"),
        [
            "check array-index",
            "check array-index",
            "proved array-index"
        ]
    );
}

/// Programs that break the offsets of [`ROWS_HEAD`], each with its trap.
const ROWS_WITNESSES: &[(&str, &str)] = &[
    // An offset past the targets.
    (
        r#"fn main() -> Int64 {
    let mut g = G { off: [0], tgt: [] }
    vertex(g, 1, 2)
    g.off.push(g.tgt.length + 1)
    print(w(g, 1).toString())
    return 0
}
"#,
        "array index 2 out of bounds",
    ),
    // A literal whose first offset is past the targets.
    (
        r#"fn main() -> Int64 {
    let mut g = G { off: [1], tgt: [] }
    g.off.push(g.tgt.length + 2)
    print(w(g, 0).toString())
    return 0
}
"#,
        "array index 1 out of bounds",
    ),
];

#[test]
fn every_offset_witness_keeps_its_index_and_traps_there() {
    witnesses(ROWS_HEAD, ROWS_WITNESSES, &["check", "check", "check"]);
}

/// A forest whose roots point at themselves; `w` follows one parent.
const FOREST_HEAD: &str = r#"type F = { parent: Array<Int64> }
fn w(f: F, x: Int64) -> Int64 {
    if x < 0 || x >= f.parent.length {
        return 0
    }
    return f.parent[f.parent[x]]
}
"#;

#[test]
fn a_push_of_the_counter_keeps_its_fields_element_fact() {
    let main = r#"fn main() -> Int64 {
    let mut f = F { parent: [] }
    let mut i = 0
    while i < 4 {
        f.parent.push(i)
        i = i + 1
    }
    print(w(f, 3).toString())
    return 0
}
"#;
    let (_dir, file) = modules(&format!("{FOREST_HEAD}{main}"), &[]);
    assert_eq!(
        rows_of(&file, "w"),
        ["proved array-index", "proved array-index"]
    );
}

/// Programs that push past the bound of [`FOREST_HEAD`], each with its trap.
const FOREST_WITNESSES: &[(&str, &str)] = &[
    // A push one past the length it leaves.
    (
        r#"fn main() -> Int64 {
    let mut f = F { parent: [] }
    let mut i = 0
    while i < 4 {
        f.parent.push(i + 1)
        i = i + 1
    }
    print(w(f, 3).toString())
    return 0
}
"#,
        "array index 4 out of bounds",
    ),
    // A push on some turns only: the counter outruns the length.
    (
        r#"fn main() -> Int64 {
    let mut f = F { parent: [] }
    let mut i = 0
    while i < 4 {
        if i % 2 == 0 {
            f.parent.push(i)
        }
        i = i + 1
    }
    print(w(f, 1).toString())
    return 0
}
"#,
        "array index 2 out of bounds",
    ),
];

#[test]
fn every_forest_witness_keeps_its_index_and_traps_there() {
    witnesses(FOREST_HEAD, FOREST_WITNESSES, &["proved", "check"]);
}

/// A writer that tests its arguments with `||` before an element store, and
/// the reader the store's element fact proves.
const PUT_HEAD: &str = r#"type S = { dense: Array<Int64>, sparse: Array<Int64> }
fn put(s: modify S, i: Int64, x: Int64) {
    if i < 0 || i >= s.dense.length || x < 0 || x >= s.sparse.length {
        return
    }
    s.dense[i] = x
}
fn w(s: S, i: Int64) -> Int64 {
    if i < 0 || i >= s.dense.length {
        return 0
    }
    return s.sparse[s.dense[i]]
}
"#;

#[test]
fn a_store_after_an_or_test_keeps_its_element_fact() {
    let main = r#"fn main() -> Int64 {
    let mut s = S { dense: [0, 0], sparse: [5, 6, 7] }
    put(s, 1, 2)
    print(w(s, 1).toString())
    return 0
}
"#;
    let (_dir, file) = modules(&format!("{PUT_HEAD}{main}"), &[]);
    assert_eq!(
        rows_of(&file, "w"),
        ["proved array-index", "proved array-index"]
    );
}

#[test]
fn a_store_the_test_does_not_cover_keeps_its_check() {
    let head = PUT_HEAD.replace(" || x >= s.sparse.length", "");
    let main = r#"fn main() -> Int64 {
    let mut s = S { dense: [0, 0], sparse: [5, 6, 7] }
    put(s, 1, 3)
    print(w(s, 1).toString())
    return 0
}
"#;
    witnesses(
        &head,
        &[(main, "array index 3 out of bounds")],
        &["proved", "check"],
    );
}

/// A table of row offsets and the reader its facts prove: `w` steps a state
/// `s` through `d.next[s + byte]`. Each entry is a row offset at least 256
/// before the table's end, or -256, and the table is at least 256 long.
const REACH_HEAD: &str = r#"type D = { next: Array<Int64> }
fn pad(d: modify D) {
    d.next.push(0 - 256)
}
fn make() -> D {
    let mut d = D { next: [] }
    while d.next.length < 512 {
        d.next.push(0 - 256)
    }
    d.next[3] = 256
    return d
}
fn w(d: modify D, bytes: Array<UInt8>) -> Int64 {
    let mut s = 0
    let mut i = 0
    while i < bytes.length {
        let t = d.next[s + Int64(bytes[i])]
        if t < 0 {
            pad(d)
            return s
        }
        s = t
        i = i + 1
    }
    return s
}
"#;

/// Programs that break a fact the read of [`REACH_HEAD`] needs, each a
/// `main` with the trap its `w` raises.
const REACH_WITNESSES: &[(&str, &str)] = &[
    // A store of an entry with no reach past it.
    (
        r#"fn main() -> Int64 {
    let mut d = make()
    d.next[0] = 600
    print(w(d, [0, 5]).toString())
    return 0
}
"#,
        "array index 605 out of bounds",
    ),
    // A push of an entry with no reach past it.
    (
        r#"fn main() -> Int64 {
    let mut d = D { next: [] }
    d.next.push(600)
    while d.next.length < 512 {
        d.next.push(0 - 256)
    }
    print(w(d, [0, 5]).toString())
    return 0
}
"#,
        "array index 605 out of bounds",
    ),
    // The table shrinks below an entry's reach.
    (
        r#"fn main() -> Int64 {
    let mut d = D { next: [] }
    while d.next.length < 600 {
        d.next.push(0 - 256)
    }
    d.next[3] = 344
    d.next.pop()
    print(w(d, [3, 255]).toString())
    return 0
}
"#,
        "array index 599 out of bounds",
    ),
    // A table shorter than the first state's reach.
    (
        r#"fn main() -> Int64 {
    let mut d = D { next: [] }
    while d.next.length < 100 {
        d.next.push(0 - 256)
    }
    print(w(d, [200]).toString())
    return 0
}
"#,
        "array index 200 out of bounds",
    ),
    // A record decoded by `fromJson` holds any entry.
    (
        r#"fn main() -> Int64 {
    let mut d = make()
    match fromJson<D>("{\"next\": [600]}") {
        Valid(x) => {
            d = x
        }
        Invalid(_) => {}
    }
    print(w(d, [0, 5]).toString())
    return 0
}
"#,
        "array index 605 out of bounds",
    ),
];

/// A `main` for [`REACH_HEAD`] with `w`'s state stepping past its reach on
/// each turn (`s = s + 300`), and the trap it raises.
const REACH_COUNTER: (&str, &str) = (
    r#"fn main() -> Int64 {
    let mut d = make()
    print(w(d, [3, 255]).toString())
    return 0
}
"#,
    "array index 555 out of bounds",
);

#[test]
fn a_row_offset_and_a_byte_prove_the_tables_read() {
    let root = format!(
        "{REACH_HEAD}fn main() -> Int64 {{\n    let mut d = make()\n    \
         print(w(d, [3, 7]).toString())\n    return 0\n}}\n"
    );
    let (_dir, file) = modules(&root, &[]);
    assert_eq!(
        rows_of(&file, "w"),
        ["proved array-index", "proved array-index"]
    );
}

#[test]
fn every_reach_witness_keeps_its_index_and_traps_there() {
    let counter = REACH_HEAD.replace(
        "        s = t
",
        "        s = s + 300
",
    );
    let (main, trap) = REACH_COUNTER;
    let roots = (REACH_WITNESSES.iter())
        .map(|(main, trap)| (format!("{REACH_HEAD}{main}"), *trap))
        .chain([(format!("{counter}{main}"), trap)]);
    let mut failures = Vec::new();
    for (root, trap) in roots {
        let (dir, file) = modules(&root, &[]);
        let rows = rows_of(&file, "w");
        if rows != ["proved array-index", "check array-index"] {
            failures.push(format!("{root}\nrows: {rows:?}"));
        }
        let (err, _) = run_oracle(&dir, &file, "w");
        if !err.contains(trap) || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED) {
            failures.push(format!("{root}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// Two fields one literal builds empty and a loop grows, read as equally
/// long by `w`; `{push}` is the loop's pushes.
const EQUAL_FIELDS: &str = r#"type E = { a: Array<Int64>, b: Array<Int64> }
fn w(e: E, i: Int64) -> Int64 {
    if i < e.a.length {
        return e.b[i]
    }
    return 0
}
fn make(n: Int64) -> E {
    let mut e = E { a: [], b: [] }
    let mut k = 0
    while k < n {
{push}        k = k + 1
    }
    return e
}
fn main() -> Int64 {
    print(w(make(3), {at}).toString())
    return 0
}
"#;

#[test]
fn two_fields_a_loop_grows_in_step_stay_equally_long() {
    let push = "        e.a.push(k)\n        e.b.push(k)\n";
    let root = EQUAL_FIELDS.replace("{push}", push).replace("{at}", "2");
    let (_dir, file) = modules(&root, &[]);
    assert_eq!(rows_of(&file, "w"), ["proved array-index"]);
}

#[test]
fn two_fields_grown_out_of_step_keep_the_index_and_trap_there() {
    let push = "        e.a.push(k)\n        e.a.push(k)\n        e.b.push(k)\n";
    let root = EQUAL_FIELDS.replace("{push}", push).replace("{at}", "4");
    let (dir, file) = modules(&root, &[]);
    assert_eq!(rows_of(&file, "w"), ["check array-index"]);
    let (err, _) = run_oracle(&dir, &file, "w");
    assert!(err.contains("array index 4 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// A loop that pushes once per turn while its counter is below a name's value
/// leaves the array at least that long. A push on some turns only, or a turn
/// that empties the array, keeps the read's check, which traps.
#[test]
fn a_loop_bound_by_a_name_counts_its_pushes() {
    let src = |push: &str| {
        format!(
            "fn w(n: Int64) -> Int64 {{
    let len = n + 1
    let mut t: Array<Int64> = []
    let mut i = 0
    while i < len {{
        {push}
        i = i + 1
    }}
    let mut s = 0
    let mut k = len
    while k > 0 {{
        s = s + t[k - 1]
        k = k - 1
    }}
    return s
}}
"
        )
    };
    let calls = "    print(w(3).toString())";
    let proof = src("t.push(i)");
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    assert_eq!(oracle(&proof, calls, "w").0, "");
    let mut failures = Vec::new();
    for push in [
        "if i != 2 {\n            t.push(i)\n        }",
        "t.push(i)\n        if i == 2 {\n            t.clear()\n        }",
    ] {
        let witness = src(push);
        let rows = verdicts(&witness, "w");
        if rows != ["check array-index"] {
            failures.push(format!("{witness}\nrows: {rows:?}"));
        }
        let (err, _) = oracle(&witness, calls, "w");
        if !err.contains("array index 3 out of bounds")
            || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED)
        {
            failures.push(format!("{witness}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A goal whose coefficients share a factor is asked divided by it, its
/// constant floored: `2 * k - 1 != 0` for `k >= 1` is `k - 1 >= 0`. The
/// divisor `2 * k - 2` is zero at `k == 1`, keeps its check and traps.
#[test]
fn a_goal_with_a_common_factor_proves_a_divisor() {
    let src = |d: &str| {
        format!(
            "fn w(t: Array<Int64>) -> Int64 {{
    let mut k = t.length
    let mut s = 0
    while k > 0 {{
        s = s + 1000 / ({d})
        k = k - 1
    }}
    return s
}}
"
        )
    };
    let calls = "    print(w(xs).toString())";
    let proof = src("2 * k - 1");
    assert_eq!(
        verdicts(&proof, "w"),
        ["proved int-div-zero", "proved int-div-overflow"]
    );
    assert_eq!(oracle(&proof, calls, "w").0, "");
    let witness = src("2 * k - 2");
    assert_eq!(verdicts(&witness, "w")[0], "check int-div-zero");
    let (err, _) = oracle(&witness, calls, "w");
    assert!(err.contains(vyrn_frontend::trap::DIV_ZERO), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// `h & m` lies in `0..=m` when the state proves `m >= 0`, so `t[h & m]` with
/// `m = t.length - 1` needs no check. A mask one too wide, or one that may be
/// negative (`-1` keeps every bit of `h`), keeps the check, which traps.
#[test]
fn a_mask_bounds_an_index_by_its_non_negative_operand() {
    let src = |m: &str, guard: &str| {
        format!(
            "fn w(t: Array<Int64>, h: Int64) -> Int64 {{
    let m = {m}
    if m < {guard} {{
        return 0
    }}
    return t[h & m]
}}
"
        )
    };
    let calls = "    let e: Array<Int64> = []\n    print(w(xs, 7).toString())\n    print(w(e, 7).toString())";
    let proof = src("t.length - 1", "0");
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    assert_eq!(oracle(&proof, calls, "w").0, "");
    let mut failures = Vec::new();
    for (m, guard, at) in [("t.length", "0", 3), ("t.length - 1", "-1", 7)] {
        let witness = src(m, guard);
        let rows = verdicts(&witness, "w");
        if rows != ["check array-index"] {
            failures.push(format!("{witness}\nrows: {rows:?}"));
        }
        let (err, _) = oracle(&witness, calls, "w");
        if !err.contains(&format!("array index {at} out of bounds"))
            || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED)
        {
            failures.push(format!("{witness}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// A record whose every builder makes an integer field below an array
/// field's length states it, so `s.slots[h & s.mask]` needs no check. One
/// builder that makes `mask` equal to the length drops the fact, and the
/// read keeps its check, which traps.
#[test]
fn an_integer_field_below_a_length_proves_a_masked_index() {
    let src = |mask: i64| {
        format!(
            "type Tab = {{ slots: Array<Int64>, mask: Int64 }}
fn make() -> Tab {{
    return Tab {{ slots: [0, 0, 0, 0], mask: {mask} }}
}}
fn w(s: Tab, h: Int64) -> Int64 {{
    return s.slots[h & s.mask]
}}
"
        )
    };
    let calls = "    print(w(make(), 4).toString())";
    let proof = src(3);
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    assert_eq!(oracle(&proof, calls, "w").0, "");
    let witness = src(4);
    assert_eq!(verdicts(&witness, "w"), ["check array-index"]);
    let (err, _) = oracle(&witness, calls, "w");
    assert!(err.contains("array index 4 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// A loop that passes a record whose fields a row set to a `modify` call
/// keeps the record's facts when every turn proves them, so the callee's
/// masked read needs no check. A turn that shortens `slots` after the call
/// drops them, and the read keeps its check, which traps on the next turn.
#[test]
fn a_record_written_in_a_loop_keeps_its_facts() {
    let src = |turn: &str| {
        format!(
            "type Tab = {{ slots: Array<Int64>, mask: Int64 }}
fn make() -> Tab {{
    return Tab {{ slots: [0, 0], mask: 1 }}
}}
fn mark(s: modify Tab, h: Int64) {{
    s.slots[h & s.mask] = 1
}}
fn grow(s: modify Tab, n: Int64) {{
    s.slots = [0, 0, 0, 0]
    s.mask = 3
    let mut i = 0
    while i < n {{
        mark(s, i)
        {turn}
        i = i + 1
    }}
}}
"
        )
    };
    let calls = "    let mut s = make()\n    grow(s, 6)\n    print(s.mask.toString())";
    let proof = src("s.slots[0] = i");
    assert_eq!(verdicts(&proof, "mark"), ["proved array-index"]);
    assert_eq!(oracle(&proof, calls, "mark").0, "");
    let witness = src("s.slots = [0]");
    assert_eq!(verdicts(&witness, "mark"), ["check array-index"]);
    let (err, _) = oracle(&witness, calls, "mark");
    assert!(err.contains("array index 1 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// A premise counts as many times as the rest of a goal's coefficients
/// share a factor: `off + j * 2 < t.length` from `off <= t.length - 8` and
/// twice `3 - j >= 0`. At `j == 4` the index passes the block's end, keeps
/// its check and traps.
#[test]
fn a_premise_taken_twice_proves_a_strided_index() {
    let src = |n: &str| {
        format!(
            "fn w(t: Array<Int64>) -> Int64 {{
    let mut s = 0
    let mut off = 0
    while off <= t.length - 8 {{
        let mut j = 0
        while j < {n} {{
            s = s + t[off + j * 2]
            j = j + 1
        }}
        off = off + 8
    }}
    return s
}}
"
        )
    };
    let calls =
        "    xs.push(40)\n    xs.push(50)\n    xs.push(60)\n    xs.push(70)\n    xs.push(80)\n    \
                 print(w(xs).toString())";
    let proof = src("4");
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    assert_eq!(oracle(&proof, calls, "w").0, "");
    let witness = src("5");
    assert_eq!(verdicts(&witness, "w"), ["check array-index"]);
    let (err, _) = oracle(&witness, calls, "w");
    assert!(err.contains("array index 8 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// A `modify` call keeps what its callee cannot shrink: `note` writes
/// another field of `log` or appends to `log.lines`, so `i < log.lines.length`
/// holds after the call. A `pop` in `note` shrinks the array: the read stays
/// a check and traps.
#[test]
fn a_modify_call_keeps_a_length_its_callee_cannot_shrink() {
    let src = |turn: &str| {
        format!(
            "type Log = {{ lines: Array<Int64>, seen: Int64 }}
type Cfg = {{ step: Int64 }}

fn note(cfg: Cfg, log: modify Log) {{
    {turn}
}}

fn w(cfg: Cfg, log: modify Log, i: Int64) -> Int64 {{
    if i < 0 || i >= log.lines.length {{
        return 0
    }}
    note(cfg, log)
    return log.lines[i]
}}
"
        )
    };
    let calls = "    let mut log = Log { lines: xs, seen: 0 }\n    \
                 print(w(Cfg { step: 1 }, log, 2).toString())";
    for turn in ["log.seen = log.seen + cfg.step", "log.lines.push(cfg.step)"] {
        let proof = src(turn);
        assert_eq!(verdicts(&proof, "w"), ["proved array-index"], "{turn}");
        assert_eq!(oracle(&proof, calls, "w").0, "", "{turn}");
    }
    let witness = src("log.lines.pop()");
    assert_eq!(verdicts(&witness, "w"), ["check array-index"]);
    let (err, _) = oracle(&witness, calls, "w");
    assert!(err.contains("array index 2 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// A length a `modify` call may grow keeps its lower bounds and loses its
/// upper ones: after `note` appends, `log.lines.length <= t.length` no longer
/// holds, so `t[log.lines.length - 1]` stays a check and traps.
#[test]
fn a_modify_call_that_grows_a_length_drops_its_upper_bound() {
    let src = "type Log = { lines: Array<Int64> }

fn note(log: modify Log) {
    log.lines.push(0)
}

fn w(log: modify Log, t: Array<Int64>) -> Int64 {
    if log.lines.length < 1 || log.lines.length > t.length {
        return 0
    }
    note(log)
    return t[log.lines.length - 1]
}
";
    let calls = "    let ys = xs.copy()\n    let mut log = Log { lines: xs }\n    \
                 print(w(log, ys).toString())";
    assert_eq!(verdicts(src, "w"), ["check array-index"]);
    let (err, _) = oracle(src, calls, "w");
    assert!(err.contains("array index 3 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// The length of an element an index name picks bounds a read of that
/// element: `j < g.rows[k].length` proves `g.rows[k][j]`, across a call that
/// takes another record. When `k` moves, or a store replaces the row, between
/// the bound and the read, the read stays a check and traps.
#[test]
fn an_element_length_bounds_a_read_of_the_same_element() {
    let src = |turn: &str| {
        format!(
            "type Grid = {{ rows: Array<Array<Int64>> }}
type Tally = {{ w: Int64 }}

fn sumRow(t: Tally, g: modify Grid, r: Int64) -> Int64 {{
    if r < 0 || r >= g.rows.length || t.w < 0 || t.w >= g.rows.length {{
        return 0
    }}
    let mut k = r
    let mut s = 0
    let mut j = 0
    while j < g.rows[k].length {{
        {turn}
        s = s + g.rows[k][j]
        j = j + 1
    }}
    return s
}}
"
        )
    };
    let calls = "    let mut g = Grid { rows: [[1, 2, 3], [4]] }\n    \
                 print(sumRow(Tally { w: 1 }, g, 0).toString())";
    let rows = |a: &str, b: &str, c: &str| [a, b, c].map(String::from).to_vec();
    let proof = src("s = s + t.w");
    let proved = rows(
        "proved array-index",
        "proved array-index",
        "proved array-index",
    );
    assert_eq!(verdicts(&proof, "sumRow"), proved);
    assert_eq!(oracle(&proof, calls, "sumRow").0, "");
    for turn in [
        "if j == 1 {
            k = t.w
        }",
        "if j == 1 {
            g.rows[k] = [t.w]
        }",
    ] {
        let witness = src(turn);
        let read = verdicts(&witness, "sumRow");
        assert_eq!(
            read.last().map(String::as_str),
            Some("check array-index"),
            "{turn}"
        );
        let (err, _) = oracle(&witness, calls, "sumRow");
        assert!(err.contains("array index 1 out of bounds"), "{turn}: {err}");
        assert!(
            !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
            "{err}"
        );
    }
}

/// A sentinel's facts hold where its result is at least zero: `peek`
/// returns `-1` past the end, so `peek(lx) == c` with `c >= 0` shows
/// `lx.pos < lx.src.length`. A `peek` that returns `0` at the end, or a
/// test the sentinel also passes (`!=`), leaves the read a check, and it
/// traps.
#[test]
fn a_sentinel_result_at_least_zero_proves_its_guard() {
    let src = |end: &str, test: &str| {
        format!(
            "type Lexer = {{ src: Array<Int64>, pos: Int64 }}
type Out = {{ toks: Array<Int64> }}

fn peek(lx: Lexer) -> Int64 {{
    if lx.pos < 0 || lx.pos > lx.src.length {{
        return 0 - 1
    }}
    if lx.pos == lx.src.length {{
        return {end}
    }}
    return lx.src[lx.pos]
}}

fn take(lx: Lexer, out: modify Out) {{
    if peek(lx) {test} {{
        out.toks.push(lx.src[lx.pos])
    }}
}}
"
        )
    };
    let calls = |pos: i64| {
        format!(
            "    let mut out = Out {{ toks: [] }}
    take(Lexer {{ src: xs, pos: {pos} }}, out)
                 print(out.toks.length.toString())"
        )
    };
    let run = |src: &str, pos: i64| {
        let (dir, file) = program(src, &calls(pos));
        (rows_of(&file, "take"), run_oracle(&dir, &file, "take").0)
    };
    let (rows, err) = run(&src("0 - 1", "== 10"), 3);
    assert_eq!(rows, ["proved array-index"]);
    assert_eq!(err, "");
    for (end, test) in [("0", "== 0"), ("0 - 1", "!= 41")] {
        let (rows, err) = run(&src(end, test), 3);
        assert_eq!(rows, ["check array-index"], "{end} {test}");
        assert!(
            err.contains("array index 3 out of bounds"),
            "{end} {test}: {err}"
        );
        assert!(
            !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
            "{err}"
        );
    }
}

/// Neither branch of `if small` writes `small`, so the join keeps what each
/// proves of the names it stores as a side of `small`: `t` is at least
/// `i + 1` long where `small` holds and empty where it does not. A read of
/// `t[i]` under `!small` keeps its check and traps.
#[test]
fn a_join_keeps_each_branch_facts_under_its_condition() {
    let src = |cond: &str| {
        format!(
            "fn table(n: Int64) -> Array<Int64> {{
    let mut t: Array<Int64> = []
    let mut k = 0
    while k < n {{
        t.push(k)
        k = k + 1
    }}
    return t
}}

fn w(i: Int64 where value >= 0) -> Int64 {{
    let small = i < 256
    let t: Array<Int64> = if small {{ table(i + 1) }} else {{ [] }}
    if {cond} {{
        return t[i]
    }}
    return 0
}}
"
        )
    };
    let calls = "    print(w(3).toString())
    print(w(300).toString())";
    let proof = src("small");
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    assert_eq!(oracle(&proof, calls, "w").0, "");
    let witness = src("!small");
    assert_eq!(verdicts(&witness, "w"), ["check array-index"]);
    let (err, _) = oracle(&witness, calls, "w");
    assert!(err.contains("array index 300 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// `h & 7` lies in `[0, 7]` though `h` is a `UInt64`, whose values above
/// `i64::MAX` a term reads as negatives. With the mask `15`, `h = 9` passes
/// the table's end, keeps its check and traps.
#[test]
fn a_mask_bounds_a_wide_operand() {
    let src = |mask: &str| {
        format!(
            "fn w(h: UInt64) -> Int64 {{
    let t = [1, 2, 3, 4, 5, 6, 7, 8]
    return t[Int64(h & {mask})]
}}
"
        )
    };
    let calls = "    print(w(9).toString())";
    let proof = src("7");
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    assert_eq!(oracle(&proof, calls, "w").0, "");
    let witness = src("15");
    assert_eq!(verdicts(&witness, "w"), ["check array-index"]);
    let (err, _) = oracle(&witness, calls, "w");
    assert!(err.contains("array index 9 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// `k` is `0` on one path and `-e` on the other, with `e` in
/// `[-100, 155]`: the join keeps `0 <= k <= 100`, so `t.length - k` is an
/// exact sum. Without a write of `k` on the path where `e >= 0`, `k` is 0
/// and `t[t.length - k]` passes the end, keeps its check and traps.
#[test]
fn a_join_bounds_a_term_one_path_defines_as_a_constant() {
    let src = |end: &str| {
        format!(
            "fn w(x: UInt8, t: Array<Int64>) -> Int64 {{
    let e = Int64(x) - 100
    let mut k = 0
    if e < 0 {{
        k = 0 - e
    }}
    let i = t.length - k{end}
    if i >= 0 {{
        return t[i]
    }}
    return 0
}}
"
        )
    };
    let calls = "    print(w(98, xs).toString())\n    print(w(200, xs).toString())";
    let proof = src(" - 1");
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    assert_eq!(oracle(&proof, calls, "w").0, "");
    let witness = src("");
    assert_eq!(verdicts(&witness, "w"), ["check array-index"]);
    let (err, _) = oracle(&witness, calls, "w");
    assert!(err.contains("array index 3 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// `(k + 1) % t.length` lies in `[0, t.length - 1]`: a ring buffer's index
/// needs no check. Modulo `t.length + 1` it reaches `t.length`, keeps its
/// check and traps.
#[test]
fn a_remainder_by_a_length_stays_below_it() {
    let src = |m: &str| {
        format!(
            "fn w(t: Array<Int64> where value.length >= 1, n: Int64) -> Int64 {{
    let mut s = 0
    let mut k = 0
    let mut i = 0
    while i < n {{
        s = s + t[k]
        k = (k + 1) % ({m})
        i = i + 1
    }}
    return s
}}
"
        )
    };
    let calls = "    print(w(xs, 4).toString())";
    let proof = src("t.length");
    assert_eq!(
        verdicts(&proof, "w"),
        ["proved array-index", "proved int-rem-zero"]
    );
    assert_eq!(oracle(&proof, calls, "w").0, "");
    let witness = src("t.length + 1");
    assert_eq!(
        verdicts(&witness, "w"),
        ["check array-index", "proved int-rem-zero"]
    );
    let (err, _) = oracle(&witness, calls, "w");
    assert!(err.contains("array index 3 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// An inner loop's ring index over an outer array: the outer loop binds
/// its own borrow of `t` each turn, and its head states `k < t.length`
/// through `t` itself. Modulo `t.length + 1` the read passes the end,
/// keeps its check and traps.
#[test]
fn a_loop_head_names_a_borrow_by_its_source() {
    let src = |m: &str| {
        format!(
            "fn w(t: Array<Int64> where value.length >= 1, n: Int64) -> Int64 {{
    let mut s = 0
    let mut k = 0
    let mut j = 0
    while j < n {{
        let mut i = 0
        while i < 2 {{
            s = s + t[k]
            k = (k + 1) % ({m})
            i = i + 1
        }}
        j = j + 1
    }}
    return s
}}
"
        )
    };
    let calls = "    print(w(xs, 2).toString())";
    let proof = src("t.length");
    assert_eq!(
        verdicts(&proof, "w"),
        ["proved array-index", "proved int-rem-zero"]
    );
    assert_eq!(oracle(&proof, calls, "w").0, "");
    let witness = src("t.length + 1");
    assert_eq!(
        verdicts(&witness, "w"),
        ["check array-index", "proved int-rem-zero"]
    );
    let (err, _) = oracle(&witness, calls, "w");
    assert!(err.contains("array index 3 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// The last index where `xs` holds 20: `at = i` keeps `at < i`, and the
/// exit's `i <= xs.length` gives `at < xs.length`. Stored after the step,
/// `at` can reach `xs.length`, keeps its check and traps.
#[test]
fn a_copy_of_the_counter_stays_below_its_bound() {
    let src = |turn: &str| {
        format!(
            "fn w(xs: Array<Int64>) -> Int64 {{
    let mut at = 0 - 1
    let mut i = 0
    while i < xs.length {{
{turn}
    }}
    if at < 0 {{
        return 0
    }}
    return xs[at]
}}
"
        )
    };
    let calls = "    print(w(xs).toString())";
    let proof = src("        if xs[i] == 30 {\n            at = i\n        }\n        i = i + 1");
    assert_eq!(
        verdicts(&proof, "w"),
        ["proved array-index", "proved array-index"]
    );
    assert_eq!(oracle(&proof, calls, "w").0, "");
    let witness =
        src("        i = i + 1\n        if xs[i - 1] == 30 {\n            at = i\n        }");
    assert_eq!(
        verdicts(&witness, "w"),
        ["proved array-index", "check array-index"]
    );
    let (err, _) = oracle(&witness, calls, "w");
    assert!(err.contains("array index 3 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// A pop of an array of at least one element shrinks it by exactly one:
/// `t[3 - (before - a.length)]` reads `t[2]`. Where `a` may be empty, the
/// pop may keep it, so the read may be `t[3]`: at `n = 0` it keeps its
/// check and traps.
#[test]
fn a_pop_of_a_full_array_shrinks_it_by_one() {
    let src = |clause: &str| {
        format!(
            "fn w(n: Int64{clause}) -> Int64 {{
    let mut a: Array<Int64> = []
    let mut i = 0
    while i < n {{
        a.push(i)
        i = i + 1
    }}
    let before = a.length
    a.pop()
    let t = [1, 2, 3]
    return t[3 - (before - a.length)]
}}
"
        )
    };
    let proof = src(" where value >= 1");
    assert_eq!(verdicts(&proof, "w"), ["proved array-index"]);
    assert_eq!(oracle(&proof, "    print(w(2).toString())", "w").0, "");
    let witness = src("");
    assert_eq!(verdicts(&witness, "w"), ["check array-index"]);
    let (err, _) = oracle(&witness, "    print(w(0).toString())", "w");
    assert!(err.contains("array index 3 out of bounds"), "{err}");
    assert!(
        !err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED),
        "{err}"
    );
}

/// The verdicts of the check rows of `body` on the source line that holds
/// `text`, in the program rooted at `file`.
fn verdicts_on(file: &std::path::Path, body: &str, text: &str) -> Vec<String> {
    let src = std::fs::read_to_string(file).unwrap();
    let line = 1 + src.lines().position(|l| l.contains(text)).unwrap();
    let out = vyrn().arg("emit-lowered").arg(file).output().unwrap();
    assert!(out.status.success(), "{}", norm(&out.stderr));
    let mut rows = Vec::new();
    let mut inside = false;
    for l in norm(&out.stdout).lines() {
        if let Some(head) = l.strip_prefix("fn ") {
            inside = head.split('(').next() == Some(body);
        } else if inside && l.contains(&format!("(line {line} #")) {
            let w: Vec<&str> = l.split_whitespace().collect();
            if matches!(w.first(), Some(&"proved" | &"check")) {
                rows.push(w[0].to_string());
            }
        }
    }
    rows
}

/// A body that builds `idx` with every element below `data.length`, runs
/// `pre`, then reads `data[idx[k]]` in a loop that runs `turn` after it. The
/// core reads `idx` and `data` through borrows it binds above the loop.
fn arrays(pre: &str, turn: &str) -> String {
    format!(
        r#"fn bump(xs: modify Array<Int64>) {{
    xs[0] = 99
}}
fn shorten(xs: modify Array<Int64>) {{
    xs.pop()
}}
fn w(n: Int64) -> Int64 {{
    if n < 1 {{
        return 0
    }}
    let mut data: Array<Int64> = []
    let mut idx: Array<Int64> = []
    let mut i = 0
    while i < n {{
        data.push(i * 3)
        idx.push(i)
        i = i + 1
    }}
{pre}
    let mut s = 0
    let mut k = 0
    while k < idx.length {{
        s = s + data[idx[k]]
{turn}
        k = k + 1
    }}
    return s
}}
fn main() -> Int64 {{
    print(w(4).toString())
    return 0
}}
"#
    )
}

const READ: &str = "s = s + data[idx[k]]";

#[test]
fn an_array_names_element_templates_prove_the_index_it_bounds() {
    let (_dir, file) = modules(&arrays("", ""), &[]);
    assert_eq!(verdicts_on(&file, "w", READ), ["proved", "proved"]);
}

/// Rows that break the template of `idx` in [`arrays`], before the loop or
/// inside it, each with its trap.
const ARRAY_WITNESSES: &[(&str, &str, &str)] = &[
    // A store of an element no template bounds.
    ("    idx[0] = n", "", "array index 4 out of bounds"),
    // A push of one.
    ("    idx.push(n)", "", "array index 4 out of bounds"),
    // The array is replaced whole.
    ("    idx = [n + 5]", "", "array index 9 out of bounds"),
    // The bounding array shrinks.
    ("    data.pop()", "", "array index 3 out of bounds"),
    ("    data.swapRemove(0)", "", "array index 3 out of bounds"),
    ("    data.clear()", "", "array index 0 out of bounds"),
    // An `append` or a `copyFrom` brings elements no template bounds.
    ("    idx.append([n])", "", "array index 4 out of bounds"),
    ("    idx.copyFrom([n])", "", "array index 4 out of bounds"),
    // One branch stores an element no template bounds.
    (
        "    if n < 3 {
        idx.push(0)
    } else {
        idx[0] = n
    }",
        "",
        "array index 4 out of bounds",
    ),
    // A call takes the array `modify`.
    ("    bump(idx)", "", "array index 99 out of bounds"),
    ("    shorten(data)", "", "array index 3 out of bounds"),
    // A store into the array while the loop reads it through a borrow.
    (
        "",
        "        idx[idx.length - 1] = n",
        "array index 4 out of bounds",
    ),
];

#[test]
fn every_array_witness_keeps_its_index_and_traps_there() {
    let mut failures = Vec::new();
    for (pre, turn, trap) in ARRAY_WITNESSES {
        let root = arrays(pre, turn);
        let (dir, file) = modules(&root, &[]);
        let rows = verdicts_on(&file, "w", READ);
        if rows != ["proved", "check"] {
            failures.push(format!("{root}\nrows: {rows:?}"));
        }
        let (err, _) = run_oracle(&dir, &file, "w");
        if !err.contains(trap) || err.contains(vyrn_frontend::trap::PROVED_CHECK_FAILED) {
            failures.push(format!("{root}\nran: {err}"));
        }
    }
    assert!(failures.is_empty(), "{}", failures.join("\n\n"));
}

/// No template names a global: a call may shrink one, and the walk sees no
/// row that writes it.
#[test]
fn an_index_into_a_global_array_keeps_its_check() {
    let root = arrays("    shrink()", "")
        .replace(
            "    let mut data: Array<Int64> = []
",
            "",
        )
        .replace(
            "fn bump",
            "let mut data: Array<Int64> = []
fn shrink() {
    data.pop()
}
fn bump",
        );
    let (dir, file) = modules(&root, &[]);
    assert_eq!(verdicts_on(&file, "w", READ), ["proved", "check"]);
    let (err, _) = run_oracle(&dir, &file, "w");
    assert!(err.contains("array index 3 out of bounds"), "{err}");
}
