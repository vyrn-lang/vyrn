//! `std/syntax`: the parser a grammar writes, run as a program and held to the
//! formatter. `examples/syntaxjson.vyrn` is the fixture every other gate runs.

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
use common::*;

fn grammar_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../vyrn-syntax")
}

fn written(grammar: &Path) -> String {
    let text = std::fs::read_to_string(grammar).expect("the grammar reads");
    let g = vyrn_syntax::load(&text).expect("the grammar loads");
    vyrn_syntax::parser(&g).expect("the grammar lowers")
}

/// Runs `main` in a new directory that holds the two grammars this crate owns,
/// under the free audit.
fn run_beside_the_grammars(stem: &str, main: &str) -> (Option<i32>, String) {
    let dir = std::env::temp_dir().join(format!("vyrn-stdsyntax-{stem}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the directory");
    for name in ["syntax.syntax", "vyrn.syntax"] {
        std::fs::copy(grammar_dir().join(name), dir.join(name)).expect("copy the grammar");
    }
    std::fs::write(dir.join("main.vyrn"), main).expect("write the program");
    let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .env("VYRN_LEAK_CHECK", "1")
        .arg("run")
        .arg("main.vyrn")
        .current_dir(&dir)
        .output()
        .expect("vyrn run");
    let _ = std::fs::remove_dir_all(&dir);
    let text = String::from_utf8_lossy(&out.stdout).to_string() + &norm(&out.stderr);
    (out.status.code(), text)
}

const SELF: &str = r#"import { syntax, SyntaxError } from "std/syntax"
import { parseFile, Row } from syntax("./syntax.syntax")

fn say(text: String) -> String {
    return match parseFile(text) {
        Ok(file) => "accepted",
        Err(errors) => "refused at line " + errors[0].line.toString(),
    }
}

fn main() -> Int64 {
    print(say(readFile("syntax.syntax") ?? ""))
    print(say(readFile("vyrn.syntax") ?? ""))
    print(say("expect ^E\na = ( 'x'\n"))
    print(say("expect ^E\na = ^\n"))
    return 0
}
"#;

#[test]
fn the_format_grammar_reads_itself_and_vyrn_and_refuses_two_errors() {
    let got = run_beside_the_grammars("self", SELF);
    let want = "accepted\naccepted\nrefused at line 2\nrefused at line 2\n";
    assert_eq!(got, (Some(0), want.to_string()));
}

#[test]
fn a_written_parser_is_what_the_formatter_writes() {
    let grammars = [
        grammar_dir().join("syntax.syntax"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/syntaxjson.syntax"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/syntaxcalc.syntax"),
    ];
    for grammar in grammars {
        let text = written(&grammar);
        let formatted = vyrn_frontend::fmt::fmt(&text).expect("the written parser parses");
        assert!(text == formatted, "{}", grammar.display());
    }
}
