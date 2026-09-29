//! The check oracle (`VYRN_CHECKS=<file>`): a run counts each check row it
//! reaches into the file, one line per row.

mod common;
use common::*;

/// Runs `src` under the oracle and returns the log's rows for `body`, each as
/// `line ordinal rule verdict count`.
fn oracle(src: &str, body: &str) -> Vec<String> {
    let dir = scratch("elide");
    let (file, log) = (dir.join("p.vyrn"), dir.join("counts.tsv"));
    std::fs::write(&file, src).unwrap();
    let out = vyrn()
        .arg("run")
        .arg(&file)
        .env("VYRN_CHECKS", &log)
        .output()
        .unwrap();
    assert!(out.status.success(), "{}", norm(&out.stderr));
    std::fs::read_to_string(&log)
        .unwrap()
        .lines()
        .filter_map(|l| {
            let f: Vec<&str> = l.split('\t').collect();
            (f[1] == body).then(|| f[2..].join(" "))
        })
        .collect()
}

#[test]
fn the_oracle_counts_each_run_of_a_check_row() {
    let src = "fn get(xs: Array<Int64>, i: Int64) -> Int64 {\n    return xs[i]\n}\n\n\
               fn main() -> Int64 {\n    let mut xs: Array<Int64> = []\n    xs.push(4)\n    \
               print((get(xs, 0) + get(xs, 0) + get(xs, 0)).toString())\n    return 0\n}\n";
    assert_eq!(oracle(src, "get"), ["2 0 array-index kept 3"]);
}
