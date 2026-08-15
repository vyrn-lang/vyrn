//! No trap wording is spelled outside the table — RFC-0101 M5.
//!
//! Parity compares stderr, so every sentence a running Vyrn program can die with
//! is a byte-for-byte contract between three engines. RFC-0101 §1.3 measured
//! what held that contract together before the table existed: **20 distinct
//! wordings across about 55 literal sites, and not one of them in a place all
//! three engines could read.** `array index {i} out of bounds` had seven
//! independent `format!`s inside `interp.rs` alone; `out of memory` had six
//! sites across three runtimes, the C shim included; `call depth exceeds N` had
//! a fourth copy in `vyrn-play`. What kept them equal was fourteen comments
//! saying one engine mirrors another — the clearest of them
//! (`interp.rs:4854`) being "kept byte-identical to the codegen's format strings
//! so all three backends agree", which is a rule, written as a wish, in a
//! comment, in the file that could not import the constant.
//!
//! `vyrn_frontend::trap` is that place, and this file is what makes it the only
//! one. The needles are read **out of the table**, not listed here, so a wording
//! added to `trap.rs` tomorrow is gated by this test the day it lands. A list
//! here would be a twenty-first copy.
//!
//! WHAT IS SCANNED: every `.rs` file under a `src/` directory of the compiler
//! workspace, `trap.rs` itself excepted. That includes the two excluded crates
//! (`vyrn-lsp`, `vyrn-genwasm`) and `vyrn-play`, because a wording drifting in
//! one of those is exactly as invisible as one drifting in a backend.
//!
//! WHAT IS NOT, and why each exemption is not a hole:
//!
//! - **Comments.** A comment quoting a wording is documentation of the contract,
//!   and forbidding it would delete the sentences that explain the table.
//! - **`#[cfg(test)]` modules.** A test asserting `run(src).unwrap_err() ==
//!   "array index 5 out of bounds"` is the INDEPENDENT check on the table; if it
//!   read the wording out of `trap` it would assert that a value equals itself,
//!   which is the mistake RFC-0101 M4's own retirement notes name. So the
//!   literals stay in the tests deliberately, and the module is skipped by brace
//!   depth rather than by "everything after the first `#[cfg(test)]`" — two
//!   files in this workspace have production code after one.
//!
//! The count this test asserts is **zero**, which is the gate RFC-0101 M5 states,
//! and the shape is RFC-0094 M2's reserved-name gate: a fact about the code,
//! checked against the code.

use std::path::{Path, PathBuf};

use vyrn_frontend::trap;

fn workspace() -> PathBuf {
    let mut d = PathBuf::from(env!("CARGO_MANIFEST_DIR"));
    d.pop();
    d
}

/// One wording as this file looks for it: whole, or as the two halves a
/// runtime value sits between.
///
/// **Both halves, or it is not this wording.** The prefix alone is not
/// evidence — `array index ` opens the checker's `array index must be an
/// Int64`, which is a compile-time diagnostic about a program and not a trap a
/// program dies with. A re-spelling writes the whole sentence, so the whole
/// sentence is what is looked for.
type Needle = (String, Option<String>);

/// Every wording the table holds, in the form a source literal would spell it.
///
/// Built from `trap` rather than written down: the fixed wordings whole, the
/// split ones as their two halves (a backend that concatenates writes exactly
/// those), and the two constant-filled ones as the prefix before the number,
/// because the number is what the constant supplies.
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
        // Both validation wordings, up to the type name.
        whole(trap::validation("@", false).split('@').next().unwrap()),
        whole(trap::validation("@", true).split('@').next().unwrap()),
    ];
    // Each I/O message, split around its `%s` where it has one.
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

/// Whether `line` spells this wording. Both halves, in order.
fn spelled(line: &str, (head, tail): &Needle) -> bool {
    match line.find(head.as_str()) {
        None => false,
        Some(i) => match tail {
            None => true,
            Some(t) => line[i + head.len()..].contains(t.as_str()),
        },
    }
}

/// Every `.rs` file under a `src/` directory of the workspace.
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
/// A test module ends at the next line that is a bare `}` in column zero, which
/// is the one thing that survives this workspace's files. **Brace counting does
/// not**: `vyrn-codegen/src/lib.rs` holds LLVM function bodies as string
/// constants, and their braces are as real to a counter as Rust's. A test module
/// is a top-level item, so its closing brace is the only unindented one between
/// its start and its end.
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

/// The gate. Zero, and the failure names every site so a reviewer sees the
/// whole list rather than the first one.
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
        "the table should hold at least the 20 wordings RFC-0101 §1.3 counted, \
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
         A running engine must ASK the table, never re-spell it (RFC-0101 §1.3):\n  {}",
        found.len(),
        found.join("\n  ")
    );
}

/// The other half of the same rule: the table is reachable from all three
/// engines, which is the whole reason it is in `vyrn-frontend` and not in
/// `vyrn-lower` (RFC-0101 §6.4).
///
/// `vyrn-codegen` re-exports the I/O half under its old names, so a backend that
/// asks `io_message` gets the table's answer and not a copy of it.
#[test]
fn the_two_backends_and_the_interpreter_read_the_same_table() {
    assert_eq!(vyrn_codegen::io_message("readerr"), trap::io("readerr"));
    assert_eq!(
        vyrn_codegen::IO_MESSAGES.len(),
        trap::IO.len(),
        "one list, not two"
    );
    // The framing an engine adds, and what the C shim gets handed.
    assert!(vyrn_codegen::toolchain::runtime_shim()
        .contains(&format!("{:?}", trap::line(trap::OUT_OF_MEMORY))));
}
