//! The surface census: one row per `ast::Type` constructor, its cost
//! in the files that decide, pinned in `tests/pins/surface.tsv`.
//!
//! A line whose trimmed text starts with `//` is not code, and a `#[cfg(test)]`
//! item is skipped whole. `Type::<Name>` counts where the next character does
//! not continue an identifier, so `Type::Int` does not count `Type::IntN`.

mod common;

use std::path::{Path, PathBuf};

/// The files whose columns the census carries, in the RFC's order. `shared` is
/// the lowering both emitters call.
const COLUMNS: &[(&str, &str)] = &[
    ("checker", "vyrn-frontend/src/checker.rs"),
    ("shared", "vyrn-codegen/src/lib.rs"),
    ("wasm", "vyrn-codegen/src/direct.rs"),
    ("types", "vyrn-frontend/src/types.rs"),
    ("prelude", "vyrn-frontend/src/prelude.rs"),
    ("editor", "vyrn-frontend/src/symbols.rs"),
];

fn repo_root() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .canonicalize()
        .expect("repo root")
}

fn compiler_file(rel: &str) -> String {
    let p = repo_root().join("compiler").join(rel);
    std::fs::read_to_string(&p).unwrap_or_else(|e| panic!("{}: {e}", p.display()))
}

/// The file without comment lines and `#[cfg(test)]` items: a doc naming a
/// constructor is not a case, and a test fixture is not a pass.
fn code_only(src: &str) -> String {
    let lines: Vec<&str> = src.lines().collect();
    let mut out: Vec<&str> = Vec::new();
    let mut i = 0usize;
    while i < lines.len() {
        let t = lines[i].trim_start();
        if t.starts_with("#[cfg(test)]") {
            // Skip to the line closing the first brace at or after the attribute.
            let mut depth = 0i32;
            let mut open = false;
            let mut j = i;
            while j < lines.len() {
                for ch in lines[j].chars() {
                    if ch == '{' {
                        depth += 1;
                        open = true;
                    } else if ch == '}' {
                        depth -= 1;
                    }
                }
                if open && depth <= 0 {
                    break;
                }
                j += 1;
            }
            i = j + 1;
            continue;
        }
        if !t.starts_with("//") {
            out.push(lines[i]);
        }
        i += 1;
    }
    out.join("\n")
}

/// How many times `code` names `Type::<name>`. The next character must not
/// continue the identifier, or `Type::Int` would count `Type::IntN`.
fn mentions(code: &str, name: &str) -> usize {
    let needle = format!("Type::{name}");
    let bytes = code.as_bytes();
    let mut n = 0usize;
    let mut from = 0usize;
    while let Some(at) = code[from..].find(&needle) {
        let end = from + at + needle.len();
        let ok = match bytes.get(end) {
            None => true,
            Some(c) => !(c.is_ascii_alphanumeric() || *c == b'_'),
        };
        if ok {
            n += 1;
        }
        from = end;
    }
    n
}

/// `ast::Type`'s variants in declaration order: the four-space-indented
/// capitalised identifiers of the enum body.
fn constructors() -> Vec<String> {
    let src = compiler_file("vyrn-frontend/src/ast.rs");
    let start = src
        .find("pub enum Type {")
        .expect("`pub enum Type` in ast.rs");
    let body = &src[start..];
    let end = body.find("\n}\n").expect("the end of `pub enum Type`");
    let mut out = Vec::new();
    for line in body[..end].lines() {
        let Some(rest) = line.strip_prefix("    ") else {
            continue;
        };
        if rest.starts_with(' ') || rest.starts_with("//") {
            continue;
        }
        let name: String = rest
            .chars()
            .take_while(|c| c.is_ascii_alphanumeric())
            .collect();
        if !name.is_empty() && name.starts_with(|c: char| c.is_ascii_uppercase()) {
            out.push(name);
        }
    }
    assert!(out.len() > 20, "only {} variants found", out.len());
    out
}

/// The cost table, pinned in `tests/pins/surface.tsv`.
#[test]
fn the_surface_census_matches_its_pin() {
    let code: Vec<String> = COLUMNS
        .iter()
        .map(|(_, f)| code_only(&compiler_file(f)))
        .collect();
    let mut totals = vec![0usize; COLUMNS.len() + 1];
    let mut rows: Vec<String> = Vec::new();
    for name in constructors() {
        let mut counts: Vec<usize> = code.iter().map(|c| mentions(c, &name)).collect();
        counts.push(counts.iter().sum());
        for (t, n) in totals.iter_mut().zip(&counts) {
            *t += n;
        }
        rows.push(common::pin_row(&format!("Type::{name}"), &counts));
    }
    rows.push(common::pin_row("all", &totals));
    let header: Vec<&str> = COLUMNS.iter().map(|(c, _)| *c).collect();
    common::pin(
        "surface",
        &format!("constructor\t{}\tall", header.join("\t")),
        rows,
    );
}
