//! Gates that no trap wording is spelled outside `vyrn_frontend::trap`.
//!
//! The needles are read out of the table, so a wording added to `trap.rs` is
//! gated the day it lands. Scanned: every `.rs` file under a `src/` directory of
//! the compiler workspace, the excluded crates included, `trap.rs` excepted.
//! Exempt: comments, which document the contract, and `#[cfg(test)]` modules,
//! whose literals are the independent check on the table.

use std::path::{Path, PathBuf};

use vyrn_frontend::trap;

fn workspace() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d
}

/// A wording, whole or as the two halves a runtime value sits between. A match
/// needs both halves: `array index ` alone also opens a checker diagnostic.
type Needle = (String, Option<String>);

/// Every wording the table holds, in the form a source literal would spell it.
fn needles() -> Vec<Needle> {
    let whole = |s: &str| -> Needle { (s.to_string(), None) };
    let split = |p: (&str, &str)| -> Needle { (p.0.to_string(), Some(p.1.to_string())) };
    let mut n: Vec<Needle> = vec![
        whole(trap::DIV_ZERO),
        whole(trap::REM_ZERO),
        whole(trap::DIV_OVERFLOW),
        whole(trap::SHIFT_RANGE),
        whole(trap::OUT_OF_MEMORY),
        whole(trap::NO_STREAM),
        whole(trap::BAD_FN_VALUE),
        whole(trap::SERVE_STREAM),
        split(trap::ARRAY_INDEX),
        split(trap::STRING_INDEX),
        // The prefix, without the number the constant fills in.
        whole(trap::call_depth().split(" exceeds").next().unwrap()),
        whole(trap::region_depth().split(" exceeds").next().unwrap()),
        // Up to the type name.
        whole(trap::validation("@", false).split('@').next().unwrap()),
        whole(trap::validation("@", true).split('@').next().unwrap()),
    ];
    for (name, _) in trap::IO {
        let m = trap::io(name);
        match m.split_once("%s") {
            Some((a, b)) => n.push((a.to_string(), Some(b.to_string()))),
            None => n.push(whole(m)),
        }
    }
    n.sort();
    n.dedup();
    n
}

fn spelled(line: &str, (head, tail): &Needle) -> bool {
    match line.find(head.as_str()) {
        None => false,
        Some(i) => match tail {
            None => true,
            Some(t) => line[i + head.len()..].contains(t.as_str()),
        },
    }
}

fn sources(dir: &Path, out: &mut Vec<PathBuf>) {
    let Ok(rd) = std::fs::read_dir(dir) else {
        return;
    };
    for e in rd.filter_map(|e| e.ok()) {
        let p = e.path();
        if p.is_dir() {
            if p.file_name().is_some_and(|n| n == "target") {
                continue;
            }
            sources(&p, out);
        } else if p.extension().is_some_and(|x| x == "rs")
            && p.components().any(|c| c.as_os_str() == "src")
        {
            out.push(p);
        }
    }
}

/// The lines of `src` that are running code: no comment, no `#[cfg(test)]`
/// module. Returns `(1-based line number, text)`.
///
/// A test module ends at the next bare `}` in column zero. Not brace counting:
/// string constants hold braces too. A test module is a top-level item, so its
/// closing brace is its only unindented one.
fn running_code(src: &str) -> Vec<(usize, &str)> {
    let mut out = Vec::new();
    let mut in_test = false;
    for (i, raw) in src.lines().enumerate() {
        let line = raw.trim_start();
        if in_test {
            if raw == "}" {
                in_test = false;
            }
            continue;
        }
        if line.starts_with("#[cfg(test)]") {
            in_test = true;
            continue;
        }
        if line.starts_with("//") {
            continue;
        }
        out.push((i + 1, raw));
    }
    out
}

#[test]
fn no_trap_wording_is_spelled_outside_the_table() {
    let root = workspace();
    let table = root.join("vyrn-frontend").join("src").join("trap.rs");
    let mut files = Vec::new();
    sources(&root, &mut files);
    files.sort();
    files.retain(|f| *f != table);
    assert!(
        files.len() > 40,
        "expected the compiler's sources, found {} files under {}",
        files.len(),
        root.display()
    );
    assert!(table.exists(), "the table is missing: {}", table.display());

    let needles = needles();
    assert!(
        needles.len() >= 20,
        "the table should hold at least 20 wordings, \
         built {} needles",
        needles.len()
    );

    let mut found: Vec<String> = Vec::new();
    for f in &files {
        let Ok(src) = std::fs::read_to_string(f) else {
            continue;
        };
        let rel = f.strip_prefix(&root).unwrap_or(f).display().to_string();
        for (n, line) in running_code(&src) {
            for needle in &needles {
                if spelled(line, needle) {
                    found.push(format!("{rel}:{n}: {:?} in {}", needle.0, line.trim()));
                }
            }
        }
    }
    assert!(
        found.is_empty(),
        "{} trap wording(s) spelled outside `vyrn_frontend::trap`. \
         A running engine must ASK the table, never re-spell it:\n  {}",
        found.len(),
        found.join("\n  ")
    );
}
