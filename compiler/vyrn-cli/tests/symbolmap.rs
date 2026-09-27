//! Generator symbol maps: an origin's `file:line:col` is where its
//! `name` is written. Each assertion reads the source at that position, so a map
//! that drifts by a line, a column or a rename fails, and one that only changes
//! shape does not. The corpus is `examples/bin` and `examples/fullstack` because
//! real generated code has a re-emitted type declared in another file, a `mut fn`
//! whose name sits further along, and a doc comment above each declaration.

use std::path::{Path, PathBuf};
use std::process::Command;
use std::sync::atomic::{AtomicUsize, Ordering};

fn repo_dir(rel: &str) -> PathBuf {
    let p = Path::new(env!("CARGO_MANIFEST_DIR"))
        .join("../..")
        .join(rel)
        .canonicalize()
        .unwrap();
    let s = p.to_string_lossy().replace('\\', "/");
    PathBuf::from(s.strip_prefix("//?/").unwrap_or(&s).to_string())
}

/// Every symbol map a root file's generators baked in, as compact JSON.
fn maps(project: &Path, root: &str) -> Vec<String> {
    let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .env("VYRN_NO_GEN_CACHE", "1")
        .env("VYRN_STD", repo_dir("std"))
        .current_dir(project)
        .args(["emit-gen", root, "--maps"])
        .output()
        .expect("run vyrn");
    assert!(
        out.status.success(),
        "emit-gen --maps failed for {root}: {}",
        String::from_utf8_lossy(&out.stderr)
    );
    String::from_utf8(out.stdout)
        .unwrap()
        .lines()
        .map(|l| l.trim().to_string())
        .filter(|l| !l.is_empty())
        .collect()
}

#[derive(Debug, PartialEq)]
struct Origin {
    symbol: String,
    file: String,
    line: usize,
    col: usize,
    name: String,
}

fn after<'a>(hay: &'a str, key: &str) -> &'a str {
    hay.split_once(key)
        .unwrap_or_else(|| panic!("no {key} in {hay}"))
        .1
}

/// Every origin in a compact symbol-map document. A scanner over the known shape,
/// not a JSON dependency: `std/json`'s writer is byte-stable.
fn origins(json: &str) -> Vec<Origin> {
    let mut out = Vec::new();
    for entry in json.split("{\"name\":\"").skip(1) {
        // `{"name":"<symbol>","origin":{"file":"..","line":N,"col":N,"name":".."}`
        let Some((symbol, rest)) = entry.split_once('"') else {
            continue;
        };
        if !rest.starts_with(",\"origin\":{") {
            continue; // the origin's own `{"name": ..}` is not a symbol entry
        }
        let file = after(rest, "\"file\":\"")
            .split('"')
            .next()
            .unwrap()
            .to_string();
        let line: usize = after(rest, "\"line\":")
            .split(',')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let col: usize = after(rest, "\"col\":")
            .split(',')
            .next()
            .unwrap()
            .parse()
            .unwrap();
        let name = after(after(rest, "\"col\":"), "\"name\":\"")
            .split('"')
            .next()
            .unwrap()
            .to_string();
        out.push(Origin {
            symbol: symbol.to_string(),
            file,
            line,
            col,
            name,
        });
    }
    out
}

/// Reads `project/file` at `line:col` and finds `name` there.
fn assert_points_at(project: &Path, o: &Origin) {
    let text = std::fs::read_to_string(project.join(&o.file))
        .unwrap_or_else(|e| panic!("origin names an unreadable file {}: {e}", o.file));
    let line = text
        .lines()
        .nth(o.line - 1)
        .unwrap_or_else(|| panic!("{}:{} is past the end of the file", o.file, o.line));
    let at: String = line
        .chars()
        .skip(o.col - 1)
        .take(o.name.chars().count())
        .collect();
    assert_eq!(
        at, o.name,
        "symbol `{}`: {}:{}:{} does not point at `{}` — the line reads `{line}`",
        o.symbol, o.file, o.line, o.col, o.name
    );
}

/// Every origin of every map a project's server and client roots produce.
fn all_origins(project: &Path) -> Vec<Origin> {
    let mut out = Vec::new();
    for root in ["server.vyrn", "client/boot.vyrn"] {
        for m in maps(project, root) {
            out.extend(origins(&m));
        }
    }
    assert!(!out.is_empty(), "no origins for {}", project.display());
    out
}

#[test]
fn every_origin_in_the_bin_corpus_points_at_its_declaration() {
    let project = repo_dir("examples/bin");
    let origins = all_origins(&project);
    for o in &origins {
        assert_points_at(&project, o);
    }
    // Generated names differ from declared ones, so an origin's `name` carries a fact.
    assert!(origins.iter().any(|o| o.symbol != o.name), "{origins:#?}");
    // A re-emitted type has lost its file in the emitted source; only the map keeps it.
    assert!(
        origins.iter().any(|o| o.file.starts_with("shared/")),
        "no closure type mapped back across a module boundary: {origins:#?}"
    );
}

#[test]
fn every_origin_in_the_fullstack_corpus_points_at_its_declaration() {
    let project = repo_dir("examples/fullstack");
    for o in &all_origins(&project) {
        assert_points_at(&project, o);
    }
}

#[test]
fn the_server_and_client_maps_agree_on_where_a_procedure_lives() {
    let project = repo_dir("examples/bin");
    let of = |root: &str| -> Vec<Origin> {
        maps(&project, root)
            .iter()
            .flat_map(|m| origins(m))
            .collect()
    };
    let server = of("server.vyrn");
    let client = of("client/boot.vyrn");
    // Two generated symbols for one declaration must name the same place, so a
    // rename can cross the server-client boundary.
    let s = server
        .iter()
        .find(|o| o.symbol == "rpcHandlePastesCreate")
        .expect("server create");
    let c = client
        .iter()
        .find(|o| o.symbol == "pastesCreate")
        .expect("client create");
    assert_eq!(
        (&s.file, s.line, s.col, &s.name),
        (&c.file, c.line, c.col, &c.name)
    );
}

static COUNTER: AtomicUsize = AtomicUsize::new(0);

fn copy_tree(from: &Path, to: &Path) {
    std::fs::create_dir_all(to).unwrap();
    for e in std::fs::read_dir(from).unwrap() {
        let e = e.unwrap();
        let dst = to.join(e.file_name());
        if e.file_type().unwrap().is_dir() {
            copy_tree(&e.path(), &dst);
        } else {
            std::fs::copy(e.path(), dst).unwrap();
        }
    }
}

/// A scratch copy of `examples/bin`, so a test can edit it and regenerate.
fn scratch_bin(tag: &str) -> PathBuf {
    let n = COUNTER.fetch_add(1, Ordering::SeqCst);
    let dir = std::env::temp_dir().join(format!("vyrn_symbolmap_{tag}_{}_{n}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    copy_tree(&repo_dir("examples/bin"), &dir);
    dir
}

fn create_origin(project: &Path) -> Origin {
    maps(project, "client/boot.vyrn")
        .iter()
        .flat_map(|m| origins(m))
        .find(|o| o.name == "create" || o.name == "publish")
        .expect("the create procedure is mapped")
}

#[test]
fn renaming_a_procedure_moves_its_origin() {
    let dir = scratch_bin("rename");
    let before = create_origin(&dir);
    assert_eq!(before.name, "create");

    let api = dir.join("server/api/pastes.vyrn");
    let src = std::fs::read_to_string(&api).unwrap();
    assert!(src.contains("export mut fn create("));
    std::fs::write(
        &api,
        src.replace("export mut fn create(", "export mut fn publish("),
    )
    .unwrap();

    let after = create_origin(&dir);
    assert_eq!(after.name, "publish");
    assert_points_at(&dir, &after);
    assert_eq!((after.line, after.col), (before.line, before.col));
    let _ = std::fs::remove_dir_all(&dir);
}

#[test]
fn a_line_inserted_above_a_procedure_moves_its_line() {
    let dir = scratch_bin("shift");
    let before = create_origin(&dir);

    let api = dir.join("server/api/pastes.vyrn");
    let src = std::fs::read_to_string(&api).unwrap();
    std::fs::write(&api, format!("\n\n{src}")).unwrap();

    let after = create_origin(&dir);
    assert_eq!(
        after.line,
        before.line + 2,
        "the map did not follow the edit"
    );
    assert_eq!(after.col, before.col);
    assert_points_at(&dir, &after);
    let _ = std::fs::remove_dir_all(&dir);
}
