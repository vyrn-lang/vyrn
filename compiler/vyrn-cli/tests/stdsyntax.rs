//! `std/syntax`: the parser a grammar writes, run as a program and held to the
//! formatter. `examples/syntaxjson.vyrn` is the fixture every other gate runs.

use std::path::{Path, PathBuf};
use std::process::Command;

mod common;
use common::*;

fn grammar_dir() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../vyrn-syntax")
}

fn format_grammar() -> PathBuf {
    Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std/syntax.syntax")
}

fn written(grammar: &Path) -> String {
    let text = std::fs::read_to_string(grammar).expect("the grammar reads");
    let g = vyrn_syntax::load(&text).expect("the grammar loads");
    vyrn_syntax::parser(&g).expect("the grammar lowers").0
}

/// Runs `main` in a new directory that holds the two grammars this crate owns,
/// under the free audit.
fn run_beside_the_grammars(stem: &str, main: &str) -> (Option<i32>, String) {
    let dir = std::env::temp_dir().join(format!("vyrn-stdsyntax-{stem}-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(&dir).expect("create the directory");
    for grammar in [format_grammar(), grammar_dir().join("vyrn.syntax")] {
        let name = grammar.file_name().expect("a file name");
        std::fs::copy(&grammar, dir.join(name)).expect("copy the grammar");
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
    print(say("syntax S // c\nexpect ^E\n"))
    return 0
}
"#;

#[test]
fn the_format_grammar_reads_itself_and_vyrn_and_refuses_two_errors() {
    let got = run_beside_the_grammars("self", SELF);
    let want = "accepted\naccepted\nrefused at line 2\nrefused at line 2\naccepted\n";
    assert_eq!(got, (Some(0), want.to_string()));
}

#[test]
fn a_written_parser_is_what_the_formatter_writes() {
    let grammars = [
        format_grammar(),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/syntaxjson.syntax"),
        Path::new(env!("CARGO_MANIFEST_DIR")).join("../../examples/syntaxcalc.syntax"),
    ];
    for grammar in grammars {
        let text = written(&grammar);
        let formatted = vyrn_frontend::fmt::fmt(&text).expect("the written parser parses");
        assert!(text == formatted, "{}", grammar.display());
    }
}

/// The `report<Start>` a written parser exports refuses a broken line at its line
/// and column, in the sentence the catalogue beside the grammar holds, and a row
/// the catalogue does not hold as the row and its holes.
#[test]
fn a_written_parser_reports_a_broken_line_in_its_sentence() {
    let dir = std::env::temp_dir().join(format!("vyrn-stdsyntax-report-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("toy.errors")).expect("create the directory");
    let files = [
        (
            "toy.syntax",
            "syntax Toy\nexpect ^Expected(found=.)\ntrivia = ws*\nFile = items:Item*\nItem = 'pay' amount:Num\nNum = text:number\nnumber = [0-9]+ (',' ^Comma(at=.))?\n",
        ),
        ("toy.errors/en.json", "{ \"expected\": \"{$found} cannot stand here\" }"),
        (
            "toy.vyrn",
            "import { syntaxReport } from \"std/syntax\"\nimport * as toy from syntaxReport(\"./toy.syntax\")\n",
        ),
        ("broken.toy", "pay 1\npay x\n"),
        ("comma.toy", "pay 1,2\n"),
    ];
    for (name, text) in files {
        std::fs::write(dir.join(name), text).expect("write the file");
    }
    let root = (dir.to_string_lossy().replace('\\', "/")) + "/toy.vyrn";
    let std_root = Path::new(env!("CARGO_MANIFEST_DIR")).join("../../std");
    let opts = vyrn_frontend::loader::LoadOptions {
        std_root: Some(std_root.to_string_lossy().replace('\\', "/")),
        ..Default::default()
    };
    let engine = vyrn_genwasm::engine();
    let report = |input: &str| {
        let arg = [vyrn_frontend::consteval::ConstVal::Str(input.to_string())];
        let resolver = vyrn_frontend::loader::DiskResolver;
        let run = vyrn_frontend::loader::run_gen_fn;
        let w = run(
            &root,
            &root,
            "reportFile",
            &arg,
            false,
            &opts,
            &resolver,
            Some(&*engine),
        );
        let w = w.expect("the report runs");
        w.reports.iter().map(|r| r.line.clone()).collect::<Vec<_>>()
    };
    let got = (report("./broken.toy"), report("./comma.toy"));
    let _ = std::fs::remove_dir_all(&dir);
    let want = (
        vec!["error ./broken.toy:2:5 x cannot stand here".to_string()],
        vec!["error ./comma.toy:1:7 Comma(at=2)".to_string()],
    );
    assert_eq!(got, want);
}

/// A catalogue that is no JSON object of sentences is refused at its own file.
#[test]
fn a_catalogue_member_that_is_no_sentence_is_refused_at_the_catalogue() {
    let dir = std::env::temp_dir().join(format!("vyrn-stdsyntax-catalogue-{}", std::process::id()));
    let _ = std::fs::remove_dir_all(&dir);
    std::fs::create_dir_all(dir.join("toy.errors")).expect("create the directory");
    let files = [
        ("toy.syntax", "syntax Toy\nexpect ^Expected\nFile = 'pay'\n"),
        ("toy.errors/en.json", "{ \"expected\": 3 }"),
        (
            "main.vyrn",
            "import { syntaxReport } from \"std/syntax\"\nimport * as toy from syntaxReport(\"./toy.syntax\")\n\nfn main() -> Int64 {\n    return 0\n}\n",
        ),
    ];
    for (name, text) in files {
        std::fs::write(dir.join(name), text).expect("write the file");
    }
    let out = Command::new(env!("CARGO_BIN_EXE_vyrn"))
        .args(["check", "main.vyrn"])
        .current_dir(&dir)
        .output()
        .expect("vyrn check");
    let _ = std::fs::remove_dir_all(&dir);
    let said = norm(&out.stderr);
    let want = "toy.errors/en.json:1:1: the catalogue's `expected` is no sentence: a sentence is a JSON string";
    assert!(said.contains(want), "{said}");
}
